//! A minimal Windows pseudo console (ConPTY): start a process attached to a
//! terminal Colony owns, read its output, write its input, resize, kill.
//!
//! Written against `windows-sys` directly rather than a PTY crate, because
//! those pull in build scripts that Smart App Control blocks on this machine.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::FromRawHandle;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

pub struct Pty {
    hpc: HPCON,
    process: HANDLE,
    closed: AtomicBool,
}

// The handles are owned by this struct and the Win32 calls on them are
// thread-safe; access is shared through an Arc.
unsafe impl Send for Pty {}
unsafe impl Sync for Pty {}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn coord(cols: u16, rows: u16) -> COORD {
    COORD { X: cols.min(i16::MAX as u16) as i16, Y: rows.min(i16::MAX as u16) as i16 }
}

impl Pty {
    /// Start `program args…` in a new pseudo console. Returns the console, a
    /// reader for its output, and a writer for its input.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        env: &[(String, String)],
        cols: u16,
        rows: u16,
    ) -> io::Result<(Pty, File, File)> {
        unsafe {
            let (mut in_read, mut in_write): (HANDLE, HANDLE) = (null_mut(), null_mut());
            let (mut out_read, mut out_write): (HANDLE, HANDLE) = (null_mut(), null_mut());
            if CreatePipe(&mut in_read, &mut in_write, null(), 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            if CreatePipe(&mut out_read, &mut out_write, null(), 0) == 0 {
                let e = io::Error::last_os_error();
                CloseHandle(in_read);
                CloseHandle(in_write);
                return Err(e);
            }
            let mut hpc: HPCON = 0;
            let hr = CreatePseudoConsole(coord(cols, rows), in_read, out_write, 0, &mut hpc);
            // The pseudo console keeps its own copies of its ends of the pipes.
            CloseHandle(in_read);
            CloseHandle(out_write);
            let reader = File::from_raw_handle(out_read as _);
            let writer = File::from_raw_handle(in_write as _);
            if hr < 0 {
                return Err(io::Error::other(format!("CreatePseudoConsole failed (0x{:08x})", hr as u32)));
            }
            // From here on, dropping `pty` closes the console on any early return.
            let mut pty = Pty { hpc, process: null_mut(), closed: AtomicBool::new(false) };

            let mut size = 0usize;
            // Sizing call: fails by design and reports the size it needs.
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
            let mut attrs = vec![0u8; size];
            let list = attrs.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
            if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
                return Err(io::Error::last_os_error());
            }
            let attached = UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const c_void,
                std::mem::size_of::<HPCON>(),
                null_mut(),
                null(),
            );
            if attached == 0 {
                let e = io::Error::last_os_error();
                DeleteProcThreadAttributeList(list);
                return Err(e);
            }

            let mut si: STARTUPINFOEXW = std::mem::zeroed();
            si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            // Otherwise the child can inherit the daemon's own redirected
            // stdio instead of talking to the pseudo console.
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
            si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
            si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
            si.lpAttributeList = list;

            let mut cmdline = wide(&command_line(program, args));
            let env_block = environment_block(env);
            let cwd_w = cwd.map(wide);
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let ok = CreateProcessW(
                null(),
                cmdline.as_mut_ptr(),
                null(),
                null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                env_block.as_ptr() as *const c_void,
                cwd_w.as_ref().map_or(null(), |w| w.as_ptr()),
                &si.StartupInfo,
                &mut pi,
            );
            DeleteProcThreadAttributeList(list);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            CloseHandle(pi.hThread);
            pty.process = pi.hProcess;
            Ok((pty, reader, writer))
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let hr = unsafe { ResizePseudoConsole(self.hpc, coord(cols, rows)) };
        if hr < 0 {
            Err(io::Error::other(format!("ResizePseudoConsole failed (0x{:08x})", hr as u32)))
        } else {
            Ok(())
        }
    }

    pub fn kill(&self) -> io::Result<()> {
        if unsafe { TerminateProcess(self.process, 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Block until the process exits; returns its exit code.
    pub fn wait(&self) -> io::Result<u32> {
        unsafe {
            WaitForSingleObject(self.process, INFINITE);
            let mut code = 0u32;
            if GetExitCodeProcess(self.process, &mut code) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(code)
        }
    }

    /// Close the console. The output reader then reaches end of file, so call
    /// this after the process exits, from a thread other than the reader's.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            unsafe { ClosePseudoConsole(self.hpc) };
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.close();
        if !self.process.is_null() {
            unsafe { CloseHandle(self.process) };
        }
    }
}

/// `NAME=value\0…\0\0` in UTF-16, sorted by name as Windows expects.
fn environment_block(env: &[(String, String)]) -> Vec<u16> {
    let mut vars: Vec<&(String, String)> = env.iter().collect();
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// Join a program and its arguments into one command line, quoting the way
/// the Microsoft C runtime (and wsl.exe) splits it back apart.
pub fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program).chain(args.iter().map(String::as_str)).map(quote).collect::<Vec<_>>().join(" ")
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_round_trips_msvc_rules() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("two words"), "\"two words\"");
        assert_eq!(quote(r#"exec claude "$@""#), r#""exec claude \"$@\"""#);
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(
            command_line("wsl.exe", &["-d".into(), "Ubuntu".into(), "fix the bug".into()]),
            r#"wsl.exe -d Ubuntu "fix the bug""#
        );
    }
}
