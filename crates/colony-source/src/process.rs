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
    imp::lineage(pid)
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

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FILETIME, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub fn lineage(pid: u32) -> Vec<u32> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
        };
        let mut parents = std::collections::HashMap::new();
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
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
        }
        let mut chain = vec![pid];
        while chain.len() <= super::LINEAGE_DEPTH {
            match parents.get(chain.last().unwrap()) {
                Some(&p) if p != 0 && !chain.contains(&p) => chain.push(p),
                _ => break,
            }
        }
        chain
    }

    pub fn colony_term(_pid: u32) -> Option<String> {
        None
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

    pub fn lineage(pid: u32) -> Vec<u32> {
        let mut chain = vec![pid];
        while chain.len() <= super::LINEAGE_DEPTH {
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
        lineage(pid).into_iter().find_map(|p| {
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

    #[cfg(windows)]
    #[test]
    fn a_reused_pid_does_not_count() {
        // Our own pid, but a creation time that can't be ours.
        assert!(!alive(std::process::id(), Some(1)));
    }
}
