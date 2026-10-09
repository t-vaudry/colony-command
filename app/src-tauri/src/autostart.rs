//! Start at login, via the per-user Run key (no administrator needed).
//!
//! Off unless the user turns it on, from the tray or the Set up Colony window.
//! The entry launches the app with `--tray`, so it starts hidden in the tray.

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Colony Command";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
        Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output()
    }

    pub fn enabled() -> bool {
        reg(&["query", KEY, "/v", NAME]).is_ok_and(|o| o.status.success())
    }

    pub fn set(on: bool) -> Result<(), String> {
        let out = if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            reg(&["add", KEY, "/v", NAME, "/t", "REG_SZ", "/d", &format!("\"{}\" --tray", exe.display()), "/f"])
        } else if !enabled() {
            return Ok(());
        } else {
            reg(&["delete", KEY, "/v", NAME, "/f"])
        };
        let out = out.map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn enabled() -> bool {
        false
    }

    pub fn set(_on: bool) -> Result<(), String> {
        Err("start at login is only supported on Windows".into())
    }
}

pub use imp::{enabled, set};
