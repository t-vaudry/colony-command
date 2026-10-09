//! colony-probe: runs inside a WSL distro, started by colonyd as
//! `wsl.exe -d <distro> -- ~/.colony/bin/colony-probe`, and writes this
//! distro's Claude Code events to stdout as JSON lines. Using the process's
//! stdio as the transport avoids WSL networking entirely.
//!
//! The first line is a hello; every following line is an `Envelope`, or a
//! permission request relayed from the distro's approval hook (see `hook`).
//! colonyd's answers to those arrive on stdin. The probe exits when stdout or
//! stdin closes, so it never outlives the daemon.

#[cfg(unix)]
mod hook;

use std::io;
use std::thread::sleep;
use std::time::Duration;

use colony_core::HostId;
use colony_source::{now_ms, DirSource};

const POLL: Duration = Duration::from_millis(200);
/// Matches colonyd's own replay window so a reconnect rebuilds the same view.
const REPLAY_MS: u64 = 12 * 60 * 60 * 1000;

fn main() {
    let distro = std::env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "linux".into());
    let host = HostId::Wsl(distro);
    let mut src = DirSource::for_current_user(host.clone(), now_ms().saturating_sub(REPLAY_MS));
    let bridge = hook::Bridge::new(Box::new(io::stdout()));
    let hello = serde_json::json!({
        "probe": "colony-probe",
        "version": env!("CARGO_PKG_VERSION"),
        "host": host,
    });
    if bridge.send_line(&hello.to_string()).is_err() {
        return;
    }
    start_approvals(&bridge);
    loop {
        let events = src.poll();
        for e in &events {
            let line = serde_json::to_string(e).expect("envelopes serialize");
            if bridge.send_line(&line).is_err() {
                return finish();
            }
        }
        sleep(POLL);
    }
}

/// Approvals are best-effort: if the socket can't be opened, events still flow
/// and the hook finds no socket and steps aside.
fn start_approvals(bridge: &std::sync::Arc<hook::Bridge>) {
    let Some(path) = hook::socket_path() else { return };
    if let Err(e) = hook::serve(&path, bridge.clone()) {
        eprintln!("approvals unavailable ({}): {e}", path.display());
        return;
    }
    let bridge = bridge.clone();
    std::thread::spawn(move || {
        use io::BufRead;
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            bridge.handle_reply(&line);
        }
        // colonyd closed our stdin: it's gone.
        bridge.abandon_all();
        finish();
        std::process::exit(0);
    });
}

fn finish() {
    if let Some(p) = hook::socket_path() {
        let _ = std::fs::remove_file(p);
    }
}

/// The probe only runs inside Linux; this keeps the workspace building on Windows.
#[cfg(not(unix))]
mod hook {
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    pub fn socket_path() -> Option<PathBuf> {
        None
    }
    pub struct Bridge(Mutex<io::Stdout>);
    impl Bridge {
        pub fn new(_: Box<dyn Write + Send>) -> Arc<Bridge> {
            Arc::new(Bridge(Mutex::new(io::stdout())))
        }
        pub fn send_line(&self, line: &str) -> io::Result<()> {
            let mut o = self.0.lock().unwrap();
            writeln!(o, "{line}")?;
            o.flush()
        }
        pub fn handle_reply(&self, _: &str) {}
        pub fn abandon_all(&self) {}
    }
    pub fn serve(_: &Path, _: Arc<Bridge>) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}
