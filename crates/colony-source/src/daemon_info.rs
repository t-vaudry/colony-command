//! What `daemon.json` says about a running colonyd, and how the app decides
//! whether that daemon is the one it ships with.
//!
//! The daemon outlives app updates, so after an install the app can find an
//! older (or dev-build) colonyd answering on the saved port. The map speaks
//! the bundled daemon's protocol, so a mismatch is replaced. Everything here
//! that can be pure is, so the decisions are testable without a daemon.

use std::path::Path;

use serde_json::{json, Value};

/// The contents of `daemon.json`. Daemons from before `version` and `exe`
/// existed leave them out; readers must accept that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonInfo {
    pub port: u16,
    pub token: String,
    pub version: Option<String>,
    pub pid: Option<u32>,
    pub exe: Option<String>,
}

impl DaemonInfo {
    /// What colonyd writes: its own version, pid and executable next to port and token.
    pub fn for_this_process(port: u16, token: &str) -> DaemonInfo {
        DaemonInfo {
            port,
            token: token.to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            pid: Some(std::process::id()),
            exe: std::env::current_exe().ok().map(|p| p.display().to_string()),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({ "port": self.port, "token": self.token });
        let o = v.as_object_mut().unwrap();
        if let Some(x) = &self.version {
            o.insert("version".into(), json!(x));
        }
        if let Some(x) = self.pid {
            o.insert("pid".into(), json!(x));
        }
        if let Some(x) = &self.exe {
            o.insert("exe".into(), json!(x));
        }
        v
    }

    /// `None` when port or token is missing; the newer fields are optional.
    pub fn from_json(v: &Value) -> Option<DaemonInfo> {
        Some(DaemonInfo {
            port: u16::try_from(v.get("port")?.as_u64()?).ok()?,
            token: v.get("token")?.as_str()?.to_string(),
            version: v.get("version").and_then(Value::as_str).map(str::to_string),
            pid: v.get("pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok()),
            exe: v.get("exe").and_then(Value::as_str).map(str::to_string),
        })
    }

    pub fn read(colony_home: &Path) -> Option<DaemonInfo> {
        DaemonInfo::from_json(&serde_json::from_slice(&std::fs::read(colony_home.join("daemon.json")).ok()?).ok()?)
    }
}

/// What to do about a daemon that answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    /// Older than the app, or from before daemons reported a version.
    ReplaceOlder,
    /// Newer than the app: its protocol is not the one this map speaks.
    ReplaceNewer,
}

/// Compares the running daemon's version with the app's own. A daemon that
/// reports no version predates this check, so it is replaced.
pub fn judge(app_version: &str, daemon_version: Option<&str>) -> Verdict {
    match daemon_version {
        None => Verdict::ReplaceOlder,
        Some(v) if v == app_version => Verdict::Keep,
        Some(v) => match (parse_version(v), parse_version(app_version)) {
            (Some(d), Some(a)) if d > a => Verdict::ReplaceNewer,
            _ => Verdict::ReplaceOlder,
        },
    }
}

fn parse_version(v: &str) -> Option<Vec<u64>> {
    let core = v.split(['-', '+']).next()?;
    core.split('.').map(|p| p.parse().ok()).collect()
}

/// What `ensure()` does with a daemon that answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Use it as is.
    Keep,
    /// Stop it and start the bundled one.
    Replace,
    /// Mismatched, but use it and let the map show the out-of-date banner.
    ConnectOnly,
    /// Mismatched, and the bundled colonyd.exe isn't the app's version either (or can't
    /// say), so replacing would only restart the same wrong file: stop nothing and
    /// tell the user the update didn't finish.
    Incomplete,
}

/// What the colonyd file next to the app says when asked `--version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bundled {
    Current,
    Other(String),
    /// It couldn't be run, timed out (an old exe without `--version` starts a daemon
    /// instead of answering) or printed something that isn't a version.
    Unknown,
}

/// Reads the output of `colonyd --version` against the app's own version.
pub fn judge_bundled(app_version: &str, output: Option<&str>) -> Bundled {
    match output.map(str::trim) {
        Some(v) if v == app_version => Bundled::Current,
        Some(v) if parse_version(v).is_some() => Bundled::Other(v.to_string()),
        _ => Bundled::Unknown,
    }
}

/// `decide`, plus the check of the bundled file before anything is stopped. `bundled`
/// is only called when a replacement would otherwise go ahead (it runs a program).
pub fn decide_update(verdict: Verdict, replaced_once: bool, gave_up: bool, bundled: impl FnOnce() -> Bundled) -> Action {
    match decide(verdict, replaced_once, gave_up) {
        Action::Replace if bundled() != Bundled::Current => Action::Incomplete,
        a => a,
    }
}

/// The banner for an update that left an old colonyd.exe on disk. `disk` is the version
/// of that file when known.
pub fn incomplete_message(app_version: &str, disk: Option<&str>) -> String {
    format!(
        "Colony's update didn't finish: colonyd.exe on disk is {}, the app is {app_version}. Run the Colony installer again.",
        disk.unwrap_or("an older version")
    )
}

/// At most one automatic replacement per app process, so two apps of different
/// versions can't keep replacing each other's daemon; after that (or after a daemon
/// couldn't be stopped) only the Restart button replaces again.
pub fn decide(verdict: Verdict, replaced_once: bool, gave_up: bool) -> Action {
    match verdict {
        Verdict::Keep => Action::Keep,
        _ if replaced_once || gave_up => Action::ConnectOnly,
        _ => Action::Replace,
    }
}

/// The image file name in `path` is colonyd's.
pub fn is_colonyd_image(path: &str) -> bool {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase();
    name == "colonyd.exe" || name == "colonyd"
}

/// Paths from the same file, tolerating case on Windows and `\\?\` prefixes.
pub fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| p.trim_start_matches(r"\\?\").replace('\\', "/").to_ascii_lowercase();
    norm(a) == norm(b)
}

/// The PID listening on `127.0.0.1:port` (or any local address) in `netstat -ano -p TCP` output.
pub fn listening_pid_netstat(output: &str, port: u16) -> Option<u32> {
    output.lines().find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        // Proto, Local Address, Foreign Address, State, PID
        if cols.len() < 5 || !cols[0].eq_ignore_ascii_case("TCP") || !cols[3].eq_ignore_ascii_case("LISTENING") {
            return None;
        }
        let (_, p) = cols[1].rsplit_once(':')?;
        if p.parse::<u16>().ok()? != port {
            return None;
        }
        cols[4].parse().ok()
    })
}

/// The PID in `ss -H -ltnp` output for a listener on `port`.
pub fn listening_pid_ss(output: &str, port: u16) -> Option<u32> {
    output.lines().find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let local = cols.get(3)?;
        if local.rsplit_once(':')?.1.parse::<u16>().ok()? != port {
            return None;
        }
        let rest = line.split("pid=").nth(1)?;
        rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    })
}

/// The process listening on `127.0.0.1:port`, by asking the OS tools.
pub fn listening_pid(port: u16) -> Option<u32> {
    use std::process::Command;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = Command::new("netstat").args(["-ano", "-p", "TCP"]).creation_flags(0x0800_0000).output().ok()?;
        listening_pid_netstat(&String::from_utf8_lossy(&out.stdout), port)
    }
    #[cfg(not(windows))]
    {
        let out = Command::new("ss").args(["-H", "-ltnp"]).output().ok()?;
        listening_pid_ss(&String::from_utf8_lossy(&out.stdout), port)
    }
}

/// Full path of a running process's executable.
pub fn image_path(pid: u32) -> Option<String> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
    #[cfg(not(windows))]
    {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok().map(|p| p.display().to_string())
    }
}

/// Ends a process the caller has already verified is a colonyd.
pub fn terminate(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = TerminateProcess(h, 1);
        CloseHandle(h);
        ok != 0
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill").arg(pid.to_string()).status().is_ok_and(|s| s.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_bundled_version() {
        assert_eq!(judge_bundled("0.1.7", Some("0.1.7\n")), Bundled::Current);
        assert_eq!(judge_bundled("0.1.7", Some("0.1.6")), Bundled::Other("0.1.6".into()));
        assert_eq!(judge_bundled("0.1.7", Some("")), Bundled::Unknown);
        assert_eq!(judge_bundled("0.1.7", Some("colonyd listening on 7878")), Bundled::Unknown);
        assert_eq!(judge_bundled("0.1.7", None), Bundled::Unknown);
    }

    #[test]
    fn checks_the_bundled_file_before_replacing() {
        use Action::*;
        let other = || Bundled::Other("0.1.6".into());
        for verdict in [Verdict::ReplaceOlder, Verdict::ReplaceNewer] {
            assert_eq!(decide_update(verdict, false, false, || Bundled::Current), Replace);
            assert_eq!(decide_update(verdict, false, false, other), Incomplete);
            assert_eq!(decide_update(verdict, false, false, || Bundled::Unknown), Incomplete);
            // Already replaced once, or given up: connect, and never look at the file.
            assert_eq!(decide_update(verdict, true, false, || panic!("not asked")), ConnectOnly);
            assert_eq!(decide_update(verdict, false, true, || panic!("not asked")), ConnectOnly);
        }
        // A matching daemon is kept whatever the file says.
        assert_eq!(decide_update(Verdict::Keep, false, false, || panic!("not asked")), Keep);
        assert_eq!(decide_update(Verdict::Keep, true, false, other), Keep);
    }

    #[test]
    fn says_what_is_on_disk() {
        assert_eq!(
            incomplete_message("0.1.7", Some("0.1.6")),
            "Colony's update didn't finish: colonyd.exe on disk is 0.1.6, the app is 0.1.7. Run the Colony installer again."
        );
        assert!(incomplete_message("0.1.7", None).contains("is an older version,"));
    }

    #[test]
    fn judges_versions() {
        assert_eq!(judge("0.1.6", Some("0.1.6")), Verdict::Keep);
        assert_eq!(judge("0.1.6", Some("0.1.5")), Verdict::ReplaceOlder);
        assert_eq!(judge("0.1.6", Some("0.1.7")), Verdict::ReplaceNewer);
        assert_eq!(judge("0.1.6", Some("0.2.0")), Verdict::ReplaceNewer);
        assert_eq!(judge("0.1.6", Some("0.1.10")), Verdict::ReplaceNewer);
        assert_eq!(judge("0.1.6", None), Verdict::ReplaceOlder);
        assert_eq!(judge("0.1.6", Some("garbage")), Verdict::ReplaceOlder);
    }

    #[test]
    fn replaces_at_most_once_automatically() {
        use Action::*;
        assert_eq!(decide(Verdict::Keep, false, false), Keep);
        assert_eq!(decide(Verdict::Keep, true, true), Keep);
        assert_eq!(decide(Verdict::ReplaceOlder, false, false), Replace);
        assert_eq!(decide(Verdict::ReplaceNewer, false, false), Replace);
        assert_eq!(decide(Verdict::ReplaceOlder, true, false), ConnectOnly);
        assert_eq!(decide(Verdict::ReplaceNewer, true, false), ConnectOnly);
        assert_eq!(decide(Verdict::ReplaceOlder, false, true), ConnectOnly);
    }

    #[test]
    fn json_round_trips_with_and_without_the_new_fields() {
        let full = DaemonInfo::for_this_process(7777, "tok");
        assert_eq!(DaemonInfo::from_json(&full.to_json()), Some(full.clone()));
        assert_eq!(full.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(full.pid, Some(std::process::id()));

        // Written by a daemon from before version and exe existed (pid only).
        let old = json!({ "port": 7777, "token": "tok", "pid": 42 });
        let parsed = DaemonInfo::from_json(&old).unwrap();
        assert_eq!((parsed.version, parsed.exe, parsed.pid), (None, None, Some(42)));
        // Oldest form, and the minimal form omits the absent fields.
        let oldest = DaemonInfo::from_json(&json!({ "port": 1, "token": "t" })).unwrap();
        assert_eq!(oldest.pid, None);
        assert!(oldest.to_json().get("version").is_none());
        assert!(DaemonInfo::from_json(&json!({ "port": 1 })).is_none());
    }

    const NETSTAT: &str = "\
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1180
  TCP    127.0.0.1:47200        0.0.0.0:0              LISTENING       21044
  TCP    127.0.0.1:47200        127.0.0.1:50612        ESTABLISHED     21044
  TCP    127.0.0.1:50612        127.0.0.1:47200        ESTABLISHED     9120
  TCP    [::]:4720              [::]:0                 LISTENING       777
";

    #[test]
    fn finds_the_listener_in_netstat_output() {
        assert_eq!(listening_pid_netstat(NETSTAT, 47200), Some(21044));
        assert_eq!(listening_pid_netstat(NETSTAT, 4720), Some(777));
        assert_eq!(listening_pid_netstat(NETSTAT, 50612), None);
        assert_eq!(listening_pid_netstat(NETSTAT, 9999), None);
    }

    #[test]
    fn finds_the_listener_in_ss_output() {
        let ss = "LISTEN 0 128 127.0.0.1:47200 0.0.0.0:* users:((\"colonyd\",pid=3141,fd=7))\n";
        assert_eq!(listening_pid_ss(ss, 47200), Some(3141));
        assert_eq!(listening_pid_ss(ss, 80), None);
    }

    #[test]
    fn only_colonyd_images_pass() {
        assert!(is_colonyd_image(r"C:\Users\x\code\colony-command\target\release\colonyd.exe"));
        assert!(is_colonyd_image("/usr/bin/colonyd"));
        assert!(is_colonyd_image(r"C:\Colony\COLONYD.EXE"));
        assert!(!is_colonyd_image(r"C:\Colony\colony-ptyd.exe"));
        assert!(!is_colonyd_image(r"C:\Colony\colonyd.exe.bak"));
        assert!(!is_colonyd_image(r"C:\Tools\node.exe"));
    }

    #[test]
    fn paths_compare_loosely() {
        assert!(same_path(r"C:\Colony\colonyd.exe", r"\\?\c:\colony\COLONYD.exe"));
        assert!(!same_path(r"C:\Colony\colonyd.exe", r"C:\Other\colonyd.exe"));
    }
}
