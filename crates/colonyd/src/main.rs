//! colonyd: the Colony Command daemon.
//!
//! Gathers events from this machine's Claude home and from a colony-probe in
//! each running WSL distro, folds them into one `Colony`, and serves it to the
//! map over a local WebSocket. It also hosts the terminals of sessions the
//! map starts. Connection details (port and a random token) are written to
//! `~/.colony/daemon.json` for the app to read.

mod api;
mod approvals;
#[cfg(windows)]
mod conpty;
mod pty;
#[cfg(windows)]
mod wsl;

use std::sync::Arc;
use std::time::Duration;

use colony_core::{Colony, Envelope, HostId};
use colony_source::{home_dir, now_ms, DirSource};
use serde_json::json;
use tokio::sync::{broadcast, mpsc, Notify, RwLock};

use crate::pty::PtyHost;

const DEFAULT_PORT: u16 = 7878;
const POLL: Duration = Duration::from_millis(200);
/// How much captured history to replay at startup.
pub const REPLAY_MS: u64 = 12 * 60 * 60 * 1000;

pub struct Shared {
    pub colony: RwLock<Colony>,
    /// Serialized delta messages for every connected map.
    pub deltas: broadcast::Sender<String>,
    pub token: String,
    pub pty: Arc<PtyHost>,
    /// Installed WSL distros, for the New session dialog.
    pub distros: RwLock<Vec<String>>,
    /// Wakes the WSL supervisor, e.g. right after starting a session in a
    /// distro that was stopped, so its probe attaches without waiting.
    pub wsl_wake: Notify,
    /// Into the reducer, for events the API produces itself.
    pub events: mpsc::Sender<Envelope>,
    /// Permission requests held for the map to answer.
    pub approvals: Arc<approvals::Approvals>,
}

pub fn log(msg: impl AsRef<str>) {
    eprintln!("[colonyd {}] {}", now_ms(), msg.as_ref());
}

/// Delta messages for agents that changed: an upsert, or a remove when the
/// agent is gone from the colony.
pub fn delta_messages(colony: &Colony, changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .map(|id| match colony.agents.get(id) {
            Some(a) => json!({ "type": "upsert", "agent": a }).to_string(),
            None => json!({ "type": "remove", "id": id }).to_string(),
        })
        .collect()
}

fn local_host() -> HostId {
    if cfg!(windows) {
        HostId::Windows
    } else {
        HostId::Wsl(std::env::var("WSL_DISTRO_NAME").unwrap_or_else(|_| "linux".into()))
    }
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("COLONY_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    let token = uuid::Uuid::new_v4().simple().to_string();
    let (deltas, _) = broadcast::channel(4096);
    let (ev_tx, mut ev_rx) = mpsc::channel::<Envelope>(8192);
    let shared = Arc::new(Shared {
        colony: RwLock::new(Colony::new()),
        deltas,
        token: token.clone(),
        pty: PtyHost::new(ev_tx.clone()),
        distros: RwLock::default(),
        wsl_wake: Notify::new(),
        events: ev_tx.clone(),
        approvals: Arc::default(),
    });

    // This machine's sessions.
    let local_tx = ev_tx.clone();
    std::thread::spawn(move || {
        let mut src = DirSource::for_current_user(local_host(), now_ms().saturating_sub(REPLAY_MS));
        loop {
            for e in src.poll() {
                if local_tx.blocking_send(e).is_err() {
                    return;
                }
            }
            std::thread::sleep(POLL);
        }
    });

    // Sessions inside running WSL distros.
    #[cfg(windows)]
    tokio::spawn(wsl::supervise(ev_tx.clone(), shared.clone()));
    drop(ev_tx);

    // The reducer: the only writer of colony state.
    let reducer = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            let changed = tokio::select! {
                Some(e) = ev_rx.recv() => {
                    let mut colony = reducer.colony.write().await;
                    let mut changed = colony.apply(&e);
                    // Drain whatever else is queued in one lock.
                    while let Ok(e) = ev_rx.try_recv() {
                        changed.extend(colony.apply(&e));
                    }
                    changed.sort();
                    changed.dedup();
                    reducer.approvals.release_answered(&colony);
                    delta_messages(&colony, &changed)
                }
                _ = tick.tick() => {
                    let mut colony = reducer.colony.write().await;
                    let changed = colony.tick(now_ms());
                    reducer.approvals.release_answered(&colony);
                    delta_messages(&colony, &changed)
                }
            };
            for msg in changed {
                // No connected maps is fine.
                let _ = reducer.deltas.send(msg);
            }
        }
    });

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            log(format!("cannot listen on 127.0.0.1:{port}: {e}. Is another colonyd running?"));
            std::process::exit(1);
        }
    };
    let info_path = home_dir().join(".colony").join("daemon.json");
    let info = json!({ "port": port, "token": token, "pid": std::process::id() });
    if let Err(e) = std::fs::create_dir_all(info_path.parent().unwrap())
        .and_then(|_| std::fs::write(&info_path, serde_json::to_vec_pretty(&info).unwrap()))
    {
        log(format!("could not write {}: {e}", info_path.display()));
    }
    log(format!("listening on http://127.0.0.1:{port} (details in {})", info_path.display()));
    axum::serve(listener, api::router(shared)).await.expect("server runs");
}
