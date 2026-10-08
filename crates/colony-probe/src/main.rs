//! colony-probe: runs inside a WSL distro, started by colonyd as
//! `wsl.exe -d <distro> -- ~/.colony/bin/colony-probe`, and writes this
//! distro's Claude Code events to stdout as JSON lines. Using the process's
//! stdio as the transport avoids WSL networking entirely.
//!
//! The first line is a hello; every following line is an `Envelope`. The probe
//! exits when stdout closes, so it never outlives the daemon.

use std::io::{self, Write};
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
    let stdout = io::stdout();
    let hello = serde_json::json!({
        "probe": "colony-probe",
        "version": env!("CARGO_PKG_VERSION"),
        "host": host,
    });
    if writeln!(stdout.lock(), "{hello}").is_err() {
        return;
    }
    loop {
        let events = src.poll();
        let mut out = stdout.lock();
        for e in &events {
            let line = serde_json::to_string(e).expect("envelopes serialize");
            if writeln!(out, "{line}").is_err() {
                return;
            }
        }
        // Flushing also detects a closed pipe while idle.
        if out.flush().is_err() {
            return;
        }
        drop(out);
        sleep(POLL);
    }
}
