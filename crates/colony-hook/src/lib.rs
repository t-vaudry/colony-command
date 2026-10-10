//! The Claude Code hook that feeds colonyd.
//!
//! Claude Code runs it for every hook event with the payload on stdin. It:
//!
//! 1. records the payload as `<colony home>/capture/<nanotime>-<pid>.json`,
//!    which is what colonyd reads. If colonyd isn't reachable the file goes to
//!    `<colony home>/spool/` instead, and colony-source moves spooled files into
//!    `capture/` when a daemon starts, so nothing is lost while it is away;
//! 2. keeps the first payload of each event, per Claude Code version, in
//!    `<colony home>/fixtures/<version>/<Event>.json` (unscrubbed, local only),
//!    so schema changes between releases can be spotted and tested against;
//! 3. for `PermissionRequest`, hands the request to colonyd and prints its
//!    decision, if the map is open (see colonyd's `/api/permission`).
//!
//! Nothing here may block or break a session: every failure is swallowed and
//! the hook exits 0 with no output, which Claude Code treats as "no opinion".

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// How long the hook waits for stdin. Claude Code writes the payload right after starting the hook,
/// but on a busy machine (several sessions starting, antivirus scanning) the write can land a few
/// hundred ms late; at 40 ms the payload was dropped silently, and with it the session's first
/// events and permission prompts (found with real Claude Code, scripts/acceptance.mjs real-windows).
/// This only costs time when stdin never closes, which Claude Code doesn't do.
pub const STDIN_BUDGET: Duration = Duration::from_millis(1000);
/// How long it waits for colonyd to accept a connection (loopback, so a live
/// daemon answers at once and a dead port refuses at once).
const CONNECT_BUDGET: Duration = Duration::from_millis(15);
/// Matches colonyd's own cap on holding a permission request.
const GATE_BUDGET: Duration = Duration::from_secs(595);

pub struct Ctx {
    pub colony_home: PathBuf,
    pub claude_home: PathBuf,
    pub version_hint: VersionHint,
}

/// Where the Claude Code version can be learned; payloads don't carry it.
#[derive(Default, Clone)]
pub struct VersionHint {
    /// `COLONY_CLAUDE_VERSION`
    pub explicit: Option<String>,
    /// `CLAUDE_PID`: looks up the session registry entry, which has `version`.
    pub claude_pid: Option<String>,
    /// `CLAUDE_CODE_EXECPATH`: may contain a version-named folder.
    pub exec_path: Option<String>,
}

impl Ctx {
    pub fn from_env() -> Ctx {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ctx {
            colony_home: colony_source::colony_home(),
            claude_home: colony_source::home_dir().join(".claude"),
            version_hint: VersionHint {
                explicit: var("COLONY_CLAUDE_VERSION"),
                claude_pid: var("CLAUDE_PID"),
                exec_path: var("CLAUDE_CODE_EXECPATH"),
            },
        }
    }
}

/// Handles one hook call. Returns what to print on stdout, if anything.
pub fn run(input: &str, ctx: &Ctx) -> Option<String> {
    let event = serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|v| v.get("hook_event_name").and_then(Value::as_str).map(str::to_string));
    let daemon = Daemon::find(&ctx.colony_home);
    // Record first: a request that is held for minutes must still be on the
    // map's timeline, and a failure later must not lose it.
    let dir = if daemon.is_some() { "capture" } else { "spool" };
    let _ = write_unique(&ctx.colony_home.join(dir), input.as_bytes());
    // Nobody may be reading the spool (the app was uninstalled, or just isn't running), so it
    // can't be allowed to grow forever. Sampled by pid to keep the hook fast.
    if dir == "spool" && std::process::id() % 8 == 0 {
        prune_spool(&ctx.colony_home.join("spool"), unix_nanos(), SPOOL_MAX_FILES, SPOOL_MAX_AGE);
    }
    if let Some(event) = &event {
        let _ = keep_fixture(ctx, event, input);
    }
    // The tray's kill switch: the request is still recorded above, just not held.
    if event.as_deref() == Some("PermissionRequest") && !colony_source::gating::paused(&ctx.colony_home) {
        return daemon?.permission(input);
    }
    None
}

/// The spool keeps at most this many payloads, and none older than `SPOOL_MAX_AGE`.
const SPOOL_MAX_FILES: usize = 2000;
const SPOOL_MAX_AGE: Duration = Duration::from_secs(3 * 24 * 3600);

fn unix_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// Deletes spooled payloads older than `max_age`, then the oldest beyond `max_files`.
/// Names are `<nanotime>-<pid>.json`, so age comes from the name. Best effort.
pub fn prune_spool(dir: &Path, now_nanos: u128, max_files: usize, max_age: Duration) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut files: Vec<(u128, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stamp = name.strip_suffix(".json")?.split('-').next()?.parse::<u128>().ok()?;
            Some((stamp, e.path()))
        })
        .collect();
    files.sort();
    let cutoff = now_nanos.saturating_sub(max_age.as_nanos());
    let fresh = files.partition_point(|(t, _)| *t < cutoff);
    let excess = (files.len() - fresh).saturating_sub(max_files);
    for (_, p) in files.iter().take(fresh + excess) {
        let _ = fs::remove_file(p);
    }
}

/// Writes `<nanotime>-<pid>.json` atomically (a temp name colony-source
/// ignores, then a rename), so a reader never sees half a payload.
fn write_unique(dir: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = format!("{nanos}-{}", std::process::id());
    let tmp = dir.join(format!("{name}.tmp"));
    let dest = dir.join(format!("{name}.json"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, &dest)?;
    Ok(dest)
}

fn keep_fixture(ctx: &Ctx, event: &str, input: &str) -> std::io::Result<()> {
    if event.is_empty() || !event.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(());
    }
    let dir = ctx.colony_home.join("fixtures").join(claude_version(ctx));
    let file = dir.join(format!("{event}.json"));
    if file.exists() {
        return Ok(());
    }
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{event}.{}.tmp", std::process::id()));
    fs::write(&tmp, input)?;
    fs::rename(&tmp, &file)
}

/// The running Claude Code's version, or `unknown`.
pub fn claude_version(ctx: &Ctx) -> String {
    let hint = &ctx.version_hint;
    let found = hint
        .explicit
        .clone()
        .or_else(|| {
            let pid = hint.claude_pid.as_ref().filter(|p| p.chars().all(|c| c.is_ascii_digit()))?;
            let text = fs::read_to_string(ctx.claude_home.join("sessions").join(format!("{pid}.json"))).ok()?;
            serde_json::from_str::<Value>(&text).ok()?.get("version")?.as_str().map(str::to_string)
        })
        .or_else(|| hint.exec_path.as_ref()?.split(['/', '\\']).find(|s| looks_like_version(s)).map(str::to_string));
    match found {
        Some(v) if looks_like_version(&v) => v,
        _ => "unknown".into(),
    }
}

fn looks_like_version(s: &str) -> bool {
    let mut parts = s.split('.');
    let numeric = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    numeric(parts.next()) && numeric(parts.next()) && numeric(parts.next()) && parts.next().is_none()
}

/// A colonyd that answered on its port.
pub struct Daemon {
    addr: SocketAddr,
    token: String,
}

impl Daemon {
    /// Reads `daemon.json` and checks that something is listening. A stale file
    /// left by a dead daemon fails the connect, which sends the payload to the
    /// spool.
    pub fn find(colony_home: &Path) -> Option<Daemon> {
        let info: Value = serde_json::from_str(&fs::read_to_string(colony_home.join("daemon.json")).ok()?).ok()?;
        let port = u16::try_from(info.get("port")?.as_u64()?).ok()?;
        let token = info.get("token")?.as_str()?.to_string();
        if token.is_empty() || !token.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        TcpStream::connect_timeout(&addr, CONNECT_BUDGET).ok()?;
        Some(Daemon { addr, token })
    }

    /// POSTs a permission request and waits for the map's answer. Anything but
    /// a 200 with a body is "no decision".
    fn permission(&self, body: &str) -> Option<String> {
        let mut s = TcpStream::connect_timeout(&self.addr, CONNECT_BUDGET).ok()?;
        s.set_read_timeout(Some(GATE_BUDGET)).ok()?;
        s.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
        let req = format!(
            "POST /api/permission?token={} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.token,
            body.len()
        );
        s.write_all(req.as_bytes()).ok()?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).ok()?;
        let text = String::from_utf8(raw).ok()?;
        let (head, rest) = text.split_once("\r\n\r\n")?;
        if !head.lines().next()?.contains(" 200") {
            return None;
        }
        let out = rest.trim();
        // Claude Code reads stdout as a decision: only a JSON object may reach
        // it (not an error page or a half-sent body from a dying daemon).
        serde_json::from_str::<Value>(out).ok().filter(Value::is_object).map(|_| out.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_request_steps_aside_while_gating_is_paused() {
        use std::net::TcpListener;
        let home = std::env::temp_dir().join(format!("colony-hook-gating-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        // A fake colonyd that answers every request with a decision.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let mut buf = [0u8; 4096];
                let _ = conn.read(&mut buf);
                let body = r#"{"decision":"allow"}"#;
                let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        fs::write(home.join("daemon.json"), format!(r#"{{"port":{port},"token":"abc123"}}"#)).unwrap();
        let ctx = Ctx { colony_home: home.clone(), claude_home: home.join(".claude"), version_hint: VersionHint::default() };
        let input = r#"{"hook_event_name":"PermissionRequest","session_id":"s","tool_name":"Bash"}"#;

        assert!(run(input, &ctx).is_some(), "without the marker the request goes to colonyd");
        fs::write(colony_source::gating::marker_path(&home), b"").unwrap();
        assert_eq!(run(input, &ctx), None, "with the marker the hook steps aside");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn spool_is_capped_and_aged_out() {
        let dir = std::env::temp_dir().join(format!("colony-hook-spool-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let day = Duration::from_secs(24 * 3600).as_nanos();
        let now = 100 * day;
        // Two old payloads, five fresh ones, and files that aren't payloads.
        for (i, t) in [now - 5 * day, now - 4 * day, now - 100, now - 90, now - 80, now - 70, now - 60].iter().enumerate() {
            fs::write(dir.join(format!("{t}-{i}.json")), "{}").unwrap();
        }
        fs::write(dir.join("notes.txt"), "keep").unwrap();
        fs::write(dir.join("1-2.tmp"), "keep").unwrap();
        prune_spool(&dir, now, 3, Duration::from_secs(3 * 24 * 3600));
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left.len(), 5, "{left:?}");
        assert!(left.contains(&"notes.txt".to_string()) && left.contains(&"1-2.tmp".to_string()));
        assert!(left.iter().any(|n| n.starts_with(&format!("{}-", now - 60))), "newest payload kept");
        assert!(!left.iter().any(|n| n.starts_with(&format!("{}-", now - 90))), "oldest beyond the cap removed");
        let _ = fs::remove_dir_all(&dir);
    }
}
