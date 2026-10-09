//! colonyd: the Colony Command daemon.
//!
//! Gathers events from this machine's Claude home and from a colony-probe in
//! each running WSL distro, folds them into one `Colony`, and serves it to the
//! map over a local WebSocket. It also hosts the terminals of sessions the
//! map starts. Connection details (port and a random token) are written to
//! `~/.colony/daemon.json` for the app to read.

mod api;
mod approvals;
mod diffstat;
mod eventlog;
mod latency_store;
mod needs;
mod pause;
mod policy;
mod pty;
mod resume;
mod timelapse;
mod worktree;
#[cfg(windows)]
mod wsl;

use std::sync::Arc;
use std::time::Duration;

use colony_core::{Colony, Envelope, HostId};
use colony_source::{now_ms, DirSource};
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

/// A registered session whose process (or a parent of it) is one Colony
/// started belongs to that Colony terminal: note it on the record.
fn tag_colony_session(e: &mut Envelope, owned: &pty::Owned) {
    let colony_core::DomainEvent::SessionSeen { record } = &mut e.event else { return };
    if record.colony_term().is_some() {
        return;
    }
    let owned = owned.lock().unwrap();
    if owned.is_empty() {
        return;
    }
    let term = colony_source::process::lineage(record.pid).into_iter().find_map(|p| owned.get(&p).cloned());
    if let Some(term) = term {
        record.extra.insert("colonyTerm".into(), term.into());
    }
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
    if let Some(w) = colony_source::process::elevation_warning("colonyd") {
        log(w);
    }
    let port: u16 = std::env::var("COLONY_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    let token = uuid::Uuid::new_v4().simple().to_string();
    let (deltas, _) = broadcast::channel(4096);
    let (ev_tx, mut ev_rx) = mpsc::channel::<Envelope>(8192);
    let shared = Arc::new(Shared {
        colony: RwLock::new({
            let mut c = Colony::with_dismissed(api::load_dismissed());
            c.latency = latency_store::load(&latency_store::path(), now_ms());
            c
        }),
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
    let owned = shared.pty.owned.clone();
    std::thread::spawn(move || {
        let mut src = DirSource::for_current_user(local_host(), now_ms().saturating_sub(REPLAY_MS));
        loop {
            for mut e in src.poll() {
                tag_colony_session(&mut e, &owned);
                if local_tx.blocking_send(e).is_err() {
                    return;
                }
            }
            std::thread::sleep(POLL);
        }
    });

    // Sessions inside running WSL distros.
    // COLONY_INGEST=1 is a test daemon fed by tools/synth: leave real distros alone.
    #[cfg(windows)]
    if std::env::var("COLONY_INGEST").map_or(true, |v| v != "1") {
        tokio::spawn(wsl::supervise(ev_tx.clone(), shared.clone()));
    }
    drop(ev_tx);

    // The reducer: the only writer of colony state.
    let reducer = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut events = eventlog::EventLog::open(&eventlog::path(), now_ms());
        // Response times are saved shortly after they change, not on every one.
        let mut latency_dirty = false;
        let mut latency_saved = std::time::Instant::now();
        loop {
            let changed = tokio::select! {
                Some(e) = ev_rx.recv() => {
                    let mut colony = reducer.colony.write().await;
                    events.record(&e);
                    let mut changed = colony.apply(&e);
                    // Drain whatever else is queued in one lock.
                    while let Ok(e) = ev_rx.try_recv() {
                        events.record(&e);
                        changed.extend(colony.apply(&e));
                    }
                    events.flush();
                    changed.sort();
                    changed.dedup();
                    reducer.approvals.release_answered(&colony);
                    delta_messages(&colony, &changed)
                }
                _ = tick.tick() => {
                    let mut colony = reducer.colony.write().await;
                    let changed = colony.tick(now_ms());
                    reducer.approvals.release_answered(&colony);
                    let mut msgs = delta_messages(&colony, &changed);
                    // Spend moves with every message; maps hear about it once a second.
                    let spend = colony.take_spend_changes();
                    if !spend.is_empty() {
                        msgs.push(json!({ "type": "spend", "projects": spend.into_iter().collect::<std::collections::BTreeMap<_, _>>() }).to_string());
                    }
                    let latency = colony.take_latency_changes();
                    if !latency.is_empty() {
                        msgs.push(json!({ "type": "latency", "projects": latency.into_iter().collect::<std::collections::BTreeMap<_, _>>() }).to_string());
                        latency_dirty = true;
                    }
                    if latency_dirty && latency_saved.elapsed() >= Duration::from_secs(10) {
                        latency_dirty = false;
                        latency_saved = std::time::Instant::now();
                        let ledger = colony.latency.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = latency_store::save(&latency_store::path(), &ledger) {
                                log(format!("could not save response times: {e}"));
                            }
                        });
                    }
                    msgs
                }
            };
            for msg in changed {
                // No connected maps is fine.
                let _ = reducer.deltas.send(msg);
            }
        }
    });

    // Paused sessions come back on the map, and pauses waiting for a turn to end happen.
    pause::restore(&shared).await;
    let pauser = shared.clone();
    tokio::spawn(async move {
        loop {
            pause::sweep(&pauser).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    // Files and lines changed, for work ready to review.
    tokio::spawn(diffstat::run(shared.clone()));

    // Worktrees of sessions that ended without being dismissed.
    let sweeper = shared.clone();
    tokio::spawn(async move {
        // Let the first replay of history settle before judging anything ended.
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            api::sweep_worktrees(&sweeper).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    // Restart sessions that were cut off, a few times with growing pauses.
    tokio::spawn(resume::supervise(shared.clone()));

    // Keep bots' branches current as origin/main moves.
    let syncer = shared.clone();
    tokio::spawn(async move {
        let mut state = api::SyncState::default();
        tokio::time::sleep(Duration::from_secs(90)).await;
        loop {
            api::sync_worktrees(&syncer, &mut state).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            log(format!("cannot listen on 127.0.0.1:{port}: {e}. Is another colonyd running?"));
            std::process::exit(1);
        }
    };
    let info_path = colony_source::colony_home().join("daemon.json");
    let info = json!({ "port": port, "token": token, "pid": std::process::id() });
    if let Err(e) = std::fs::create_dir_all(info_path.parent().unwrap())
        .and_then(|_| std::fs::write(&info_path, serde_json::to_vec_pretty(&info).unwrap()))
    {
        log(format!("could not write {}: {e}", info_path.display()));
    }
    log(format!("listening on http://127.0.0.1:{port} (details in {})", info_path.display()));
    let saver = shared.clone();
    axum::serve(listener, api::router(shared))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server runs");
    // A clean stop keeps the response times from the last few seconds too.
    let ledger = saver.colony.read().await.latency.clone();
    if let Err(e) = latency_store::save(&latency_store::path(), &ledger) {
        log(format!("could not save response times: {e}"));
    }
}
