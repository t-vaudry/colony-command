//! Finds or starts colonyd for the map.
//!
//! The daemon outlives the window on purpose: sessions it hosts keep running
//! and history keeps being recorded while the map is closed. So the app only
//! starts one when none is answering, and never stops it on exit.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub port: u16,
    pub token: String,
}

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

pub fn colony_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("COLONY_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".colony")
}

fn read_info() -> Option<Info> {
    serde_json::from_slice(&std::fs::read(colony_home().join("daemon.json")).ok()?).ok()
}

/// The daemon is up and accepts this token.
fn answers(info: &Info) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], info.port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
    let req = format!("GET /api/agents?token={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n", info.token);
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 16];
    matches!(s.read(&mut head), Ok(n) if n >= 12 && &head[9..12] == b"200")
}

/// colonyd.exe ships next to this app's executable.
fn daemon_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = exe.with_file_name(if cfg!(windows) { "colonyd.exe" } else { "colonyd" });
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("colonyd not found at {}", path.display()))
    }
}

fn start_daemon() -> Result<(), String> {
    let exe = daemon_exe()?;
    std::fs::create_dir_all(colony_home()).map_err(|e| e.to_string())?;
    // The log is only ever appended to; keep it from growing forever.
    let log_path = colony_home().join("colonyd.log");
    if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > 8 * 1024 * 1024) {
        let _ = std::fs::rename(&log_path, log_path.with_extension("log.1"));
    }
    let spawn = |flags: u32| -> Result<(), String> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(colony_home().join("colonyd.log"))
            .map_err(|e| format!("can't open colonyd.log: {e}"))?;
        let mut cmd = Command::new(&exe);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(log));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = flags;
        cmd.spawn().map(|_| ()).map_err(|e| format!("couldn't start {}: {e}", exe.display()))
    };
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    // Break away from the app's job where allowed, so a terminal window the
    // app was launched from can't take the daemon down when it closes.
    spawn(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP))
}

/// Connection details for a running daemon, starting one if needed.
pub fn ensure() -> Result<Info, String> {
    // The map may ask several times while the daemon boots; start it once.
    static STARTING: Mutex<()> = Mutex::new(());
    let _guard = STARTING.lock().unwrap();
    if let Some(info) = read_info().filter(answers) {
        return Ok(info);
    }
    start_daemon()?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(info) = read_info().filter(answers) {
            return Ok(info);
        }
    }
    Err(format!(
        "colonyd didn't start within {}s; see {}",
        STARTUP_TIMEOUT.as_secs(),
        colony_home().join("colonyd.log").display()
    ))
}
