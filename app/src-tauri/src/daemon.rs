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

use std::sync::atomic::{AtomicBool, Ordering};

use colony_source::daemon_info::{self, Action, Bundled, DaemonInfo, Verdict};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub port: u16,
    pub token: String,
}

/// The daemon this app's map speaks: the one that ships with it.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set when a mismatched daemon was replaced; the map takes it once and shows it.
static REPLACED: Mutex<Option<String>> = Mutex::new(None);
/// An automatic replacement has been attempted; only the Restart button does another.
static REPLACED_ONCE: AtomicBool = AtomicBool::new(false);
/// A daemon (port, pid) that couldn't be stopped safely; not retried until the user asks.
static GAVE_UP: Mutex<Option<(u16, Option<u32>)>> = Mutex::new(None);
/// Set when the colonyd file on disk is not this app's version: the update didn't finish.
/// The map shows it as a banner without a Restart button, and nothing is retried.
static INCOMPLETE: Mutex<Option<String>> = Mutex::new(None);

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Same folder the hook and daemon use (`COLONY_HOME`, else `~/.colony`).
pub fn colony_home() -> PathBuf {
    colony_source::colony_home()
}

fn read_info() -> Option<DaemonInfo> {
    DaemonInfo::read(&colony_home())
}

/// The daemon is up and accepts this token.
fn answers(info: &DaemonInfo) -> bool {
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

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(300)).is_ok()
}

fn wait_closed(port: u16, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !port_open(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !port_open(port)
}

/// Asks a daemon that has `/api/shutdown` to exit; true once its port is closed.
fn shutdown_politely(d: &DaemonInfo) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], d.port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let req = format!("POST /api/shutdown?token={} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", d.token);
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 16];
    matches!(s.read(&mut head), Ok(n) if n >= 12 && &head[9..12] == b"200") && wait_closed(d.port, Duration::from_secs(5))
}

/// The process to end for this daemon, only if it is verifiably a colonyd: it must
/// be the listener on the port, run an image named colonyd, and (when daemon.json
/// says where the daemon lives) still be that executable. Never matched by name alone.
fn verified_pid(d: &DaemonInfo) -> Option<(u32, String)> {
    let pid = daemon_info::listening_pid(d.port)?;
    // A pid in daemon.json that isn't the listener is stale (or reused).
    if d.exe.is_some() && d.pid.is_some_and(|p| p != pid) {
        return None;
    }
    let image = daemon_info::image_path(pid)?;
    if !daemon_info::is_colonyd_image(&image) || d.exe.as_deref().is_some_and(|e| !daemon_info::same_path(e, &image)) {
        return None;
    }
    Some((pid, image))
}

/// Stops a daemon, politely if it can be asked; Ok has where it ran. Err says why it was left running.
fn stop(d: &DaemonInfo) -> Result<String, String> {
    if d.version.is_some() && shutdown_politely(d) {
        return Ok(d.exe.clone().unwrap_or_else(|| "an unknown location".into()));
    }
    let (pid, image) = verified_pid(d).ok_or_else(|| format!("can't tell which process runs colonyd on port {}", d.port))?;
    if !daemon_info::terminate(pid) || !wait_closed(d.port, Duration::from_secs(5)) {
        return Err(format!("couldn't stop {image} (pid {pid})"));
    }
    Ok(image)
}

/// The version of the colonyd file next to this app, read from the bytes of the file
/// (the marker colonyd embeds), never by running it: an old colonyd would start a daemon,
/// and a colony-ptyd that outlives it. No marker (an old file) or no file is `Unknown`.
fn bundled_version() -> Bundled {
    let Ok(exe) = daemon_exe() else { return Bundled::Unknown };
    let found = std::fs::read(&exe).ok().and_then(|bytes| daemon_info::version_from_bytes(&bytes));
    daemon_info::judge_bundled(APP_VERSION, found.as_deref())
}

/// The banner text when the update didn't finish, until a daemon of this version runs.
pub fn incomplete() -> Option<String> {
    INCOMPLETE.lock().unwrap().clone()
}

fn record_incomplete(disk: Option<&str>) {
    let msg = daemon_info::incomplete_message(APP_VERSION, disk);
    eprintln!("colony: {msg}");
    *INCOMPLETE.lock().unwrap() = Some(msg);
}

/// Connection details for a running daemon, starting one if needed. A daemon from
/// another version than this app is replaced by the bundled one, but only if that
/// file really is this app's version; otherwise the update is recorded as incomplete.
pub fn ensure() -> Result<Info, String> {
    // The map may ask several times while the daemon boots; start it once.
    static STARTING: Mutex<()> = Mutex::new(());
    let _guard = STARTING.lock().unwrap();
    if let Some(d) = read_info().filter(answers) {
        let verdict = daemon_info::judge(APP_VERSION, d.version.as_deref());
        let gave_up = *GAVE_UP.lock().unwrap() == Some((d.port, d.pid));
        let mut disk: Option<Bundled> = None;
        let action = daemon_info::decide_update(verdict, REPLACED_ONCE.load(Ordering::SeqCst), gave_up, || {
            // No bundled file at all (a dev layout) is handled below; don't call that an unfinished update.
            let b = if daemon_exe().is_ok() { bundled_version() } else { Bundled::Current };
            disk = Some(b.clone());
            b
        });
        match action {
            Action::Keep => {
                *INCOMPLETE.lock().unwrap() = None;
                return Ok(Info { port: d.port, token: d.token });
            }
            Action::Incomplete => {
                // Stop nothing: the file on disk would only start the same wrong daemon again.
                REPLACED_ONCE.store(true, Ordering::SeqCst);
                record_incomplete(match &disk {
                    Some(Bundled::Other(v)) => Some(v.as_str()),
                    _ => None,
                });
                return Ok(Info { port: d.port, token: d.token });
            }
            Action::ConnectOnly => {
                eprintln!("colony: colonyd {} doesn't match this app ({APP_VERSION}); leaving it for the Restart button", d.version.as_deref().unwrap_or("(no version)"));
                return Ok(Info { port: d.port, token: d.token });
            }
            Action::Replace => {}
        }
        // Don't stop a working daemon we can't replace (a dev or odd layout).
        if let Err(why) = daemon_exe() {
            eprintln!("colony: not replacing the running colonyd: {why}");
            return Ok(Info { port: d.port, token: d.token });
        }
        REPLACED_ONCE.store(true, Ordering::SeqCst);
        if verdict == Verdict::ReplaceNewer {
            eprintln!("colony: the running colonyd ({}) is newer than this app ({APP_VERSION}); replacing it with the bundled one", d.version.as_deref().unwrap_or("?"));
        }
        match stop(&d) {
            Ok(exe) => {
                let was = d.version.as_deref().map_or("an older version".to_string(), |v| format!("version {v}"));
                *REPLACED.lock().unwrap() =
                    Some(format!("Restarted Colony's background service (was {was} from {exe}). Your sessions kept running."));
            }
            Err(why) => {
                // The map shows the out-of-date banner; don't retry on every reconnect.
                eprintln!("colony: not replacing the running colonyd: {why}");
                *GAVE_UP.lock().unwrap() = Some((d.port, d.pid));
                return Ok(Info { port: d.port, token: d.token });
            }
        }
    }
    start_daemon()?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(d) = read_info().filter(answers) {
            // A stale file starts a stale daemon: say so, and don't try again by itself.
            if d.version.as_deref() != Some(APP_VERSION) {
                REPLACED_ONCE.store(true, Ordering::SeqCst);
                record_incomplete(d.version.as_deref());
            } else {
                *INCOMPLETE.lock().unwrap() = None;
            }
            return Ok(Info { port: d.port, token: d.token });
        }
    }
    Err(format!(
        "colonyd didn't start within {}s; see {}",
        STARTUP_TIMEOUT.as_secs(),
        colony_home().join("colonyd.log").display()
    ))
}

/// The notice about a replaced daemon, once.
pub fn take_notice() -> Option<String> {
    REPLACED.lock().unwrap().take()
}

/// The Restart button: try the replacement again, even for a daemon given up on.
pub fn restart() -> Result<Info, String> {
    *GAVE_UP.lock().unwrap() = None;
    REPLACED_ONCE.store(false, Ordering::SeqCst);
    *INCOMPLETE.lock().unwrap() = None;
    ensure()
}
