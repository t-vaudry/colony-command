//! Is the process behind a session-registry file still running?
//!
//! A Claude Code process that is killed can't remove its registry file, so a
//! file's presence alone doesn't mean the session is open. Pids get reused, so
//! on Windows the process's creation time is also compared with the file's
//! `procStart` when it has one.

/// `proc_start`: the registry's `procStart`, if any (Windows: FILETIME of the
/// process's creation, in 100 ns units since 1601).
pub fn alive(pid: u32, proc_start: Option<u64>) -> bool {
    imp::alive(pid, proc_start)
}

/// The process and up to a few of its ancestors, nearest first. Used to
/// recognize a registered session as one Colony started (its `claude` may
/// sit a launcher or two below the process Colony created).
pub fn lineage(pid: u32) -> Vec<u32> {
    imp::chain(pid, LINEAGE_DEPTH, false)
}

/// The whole parent chain, for deciding what must never be touched.
pub fn ancestry(pid: u32) -> Vec<u32> {
    imp::chain(pid, MAX_DEPTH, false)
}

/// Bring the window that hosts a session's process to the front: the nearest
/// ancestor (the process itself first) that owns a visible top-level window,
/// such as Windows Terminal, a console host or the Claude desktop app. Windows
/// owned by a pid in `exclude` (Colony's own) are never touched. Returns what
/// was focused, or why nothing could be.
pub fn focus_window(pid: u32, exclude: &[u32]) -> Result<String, String> {
    imp::focus_window(pid, exclude)
}

/// Whether Colony must not end this process: itself, or anything it runs
/// inside of (taskkill /T would take Colony down with it).
pub fn protected(pid: u32, own_lineage: &[u32]) -> bool {
    pid == 0 || pid == 4 || own_lineage.contains(&pid)
}

/// Linux: the `COLONY_TERM_ID` in a process's environment, which Colony sets
/// on sessions it starts.
pub fn colony_term(pid: u32) -> Option<String> {
    imp::colony_term(pid)
}

/// This process runs as administrator. Colony shouldn't: everything it
/// starts, Claude sessions included, would inherit those rights.
pub fn elevated() -> bool {
    imp::elevated()
}

/// A warning to log at startup when running as administrator.
pub fn elevation_warning(what: &str) -> Option<String> {
    elevated().then(|| {
        format!(
            "WARNING: {what} is running as administrator, so the Claude sessions Colony starts will too.              Start Colony normally (not from an administrator prompt)."
        )
    })
}

const LINEAGE_DEPTH: usize = 4;
/// How far up to look for a session's window: claude, a shell, maybe a
/// launcher, then the terminal.
const WINDOW_DEPTH: usize = 8;
/// Cycle guard for walking the whole chain.
const MAX_DEPTH: usize = 64;

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FILETIME, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// `stop_at_shell`: end the chain before explorer.exe, which is the desktop, not a session's terminal.
    pub fn chain(pid: u32, depth: usize, stop_at_shell: bool) -> Vec<u32> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        };
        let mut parents = std::collections::HashMap::new();
        let mut names = std::collections::HashMap::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return vec![pid];
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            while ok {
                parents.insert(e.th32ProcessID, e.th32ParentProcessID);
                if stop_at_shell {
                    let n = e.szExeFile.iter().take_while(|c| **c != 0).copied().collect::<Vec<u16>>();
                    names.insert(e.th32ProcessID, String::from_utf16_lossy(&n).to_lowercase());
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
        }
        let mut chain = vec![pid];
        while chain.len() <= depth {
            match parents.get(chain.last().unwrap()) {
                Some(&p) if p != 0 && !chain.contains(&p) && names.get(&p).is_none_or(|n| n != "explorer.exe") => chain.push(p),
                _ => break,
            }
        }
        chain
    }

    pub fn colony_term(_pid: u32) -> Option<String> {
        None
    }

    struct Search<'a> {
        pids: &'a [u32],
        found: Vec<(u32, isize)>,
    }

    unsafe extern "system" fn visit(hwnd: windows_sys::Win32::Foundation::HWND, param: isize) -> i32 {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindow, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER};
        let search = &mut *(param as *mut Search);
        // Top-level app windows only: visible and not owned by another window.
        if IsWindowVisible(hwnd) != 0 && GetWindow(hwnd, GW_OWNER).is_null() {
            let mut owner = 0u32;
            GetWindowThreadProcessId(hwnd, &mut owner);
            if search.pids.contains(&owner) {
                search.found.push((owner, hwnd as isize));
            }
        }
        1
    }

    pub fn focus_window(pid: u32, exclude: &[u32]) -> Result<String, String> {
        use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        use windows_sys::Win32::UI::WindowsAndMessaging::{BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE};
        let pids: Vec<u32> = chain(pid, super::WINDOW_DEPTH, true).into_iter().filter(|p| !exclude.contains(p)).collect();
        let mut search = Search { pids: &pids, found: Vec::new() };
        unsafe { EnumWindows(Some(visit), &mut search as *mut _ as isize) };
        // Nearest ancestor first, so a tab's own terminal beats whatever launched that.
        let hit = pids.iter().find_map(|p| search.found.iter().find(|(o, _)| o == p));
        let Some(&(_, hwnd)) = hit else {
            return Err("Couldn't find a window for this session (it may run in a hidden or background process). Switch to its terminal yourself.".into());
        };
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            // Windows only lets the foreground window's thread change the foreground
            // window, so join its input queue for the moment it takes.
            let (me, fg) = (GetCurrentThreadId(), GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut()));
            let attached = fg != 0 && fg != me && AttachThreadInput(me, fg, 1) != 0;
            let raised = SetForegroundWindow(hwnd) != 0;
            BringWindowToTop(hwnd);
            if attached {
                AttachThreadInput(me, fg, 0);
            }
            if !raised {
                return Err("Windows wouldn't bring the terminal forward. Switch to it yourself.".into());
            }
        }
        Ok("Brought its terminal window to the front.".into())
    }

    pub fn elevated() -> bool {
        use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut len = 0u32;
            let ok = GetTokenInformation(token, TokenElevation, &mut e as *mut _ as *mut _, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len);
            CloseHandle(token);
            ok != 0 && e.TokenIsElevated != 0
        }
    }

    pub fn alive(pid: u32, proc_start: Option<u64>) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                // Someone else's process we may not query: it exists.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let running = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE as u32;
            let same_process = match proc_start {
                None => true,
                Some(expected) => {
                    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
                    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
                    GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user) == 0
                        || ((created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64) == expected
                }
            };
            CloseHandle(h);
            running && same_process
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn alive(pid: u32, _proc_start: Option<u64>) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    pub fn elevated() -> bool {
        false
    }

    pub fn focus_window(_pid: u32, _exclude: &[u32]) -> Result<String, String> {
        Err("Focusing a terminal is only available for Windows sessions.".into())
    }

    pub fn chain(pid: u32, depth: usize, _stop_at_shell: bool) -> Vec<u32> {
        let mut chain = vec![pid];
        while chain.len() <= depth {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", chain.last().unwrap())).unwrap_or_default();
            // "pid (comm) state ppid ...": comm may contain spaces, so split after ')'.
            let ppid = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok());
            match ppid {
                Some(p) if p > 1 && !chain.contains(&p) => chain.push(p),
                _ => break,
            }
        }
        chain
    }

    pub fn colony_term(pid: u32) -> Option<String> {
        // Check the process and its parents: Colony starts `bash -lc 'exec claude'`.
        chain(pid, super::LINEAGE_DEPTH, false).into_iter().find_map(|p| {
            let env = std::fs::read(format!("/proc/{p}/environ")).ok()?;
            env.split(|b| *b == 0)
                .find_map(|kv| kv.strip_prefix(b"COLONY_TERM_ID="))
                .map(|v| String::from_utf8_lossy(v).into_owned())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineage_starts_with_the_process_and_finds_a_parent() {
        let chain = lineage(std::process::id());
        assert_eq!(chain[0], std::process::id());
        assert!(chain.len() >= 2, "test runner has a parent: {chain:?}");
    }

    #[test]
    fn this_process_is_alive_and_a_bogus_one_is_not() {
        assert!(alive(std::process::id(), None));
        assert!(!alive(u32::MAX - 7, None));
    }

    #[test]
    fn never_ends_itself_or_what_it_runs_inside() {
        let own = ancestry(std::process::id());
        assert!(protected(std::process::id(), &own));
        if own.len() > 1 {
            assert!(protected(own[1], &own));
        }
        assert!(protected(0, &own) && protected(4, &own));
        assert!(!protected(u32::MAX - 7, &own));
    }

    #[test]
    fn no_window_for_a_process_that_does_not_exist() {
        assert!(focus_window(u32::MAX - 7, &[]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn a_reused_pid_does_not_count() {
        // Our own pid, but a creation time that can't be ours.
        assert!(!alive(std::process::id(), Some(1)));
    }
}
