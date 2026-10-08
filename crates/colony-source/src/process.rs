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

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED, FILETIME, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
