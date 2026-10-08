//! Local API for the map. Bound to 127.0.0.1 only, and every route needs the
//! token from `~/.colony/daemon.json` (query `?token=` or a Bearer header),
//! because this API can type into sessions.
//!
//! - `GET  /ws`         snapshot, then upsert/remove deltas as JSON text frames;
//!                      the map sends commands back on the same socket
//! - `GET  /api/agents` the current snapshot
//! - `POST /api/ack`    `{"id": "..."}`: mark finished work reviewed, or clear a
//!                      crash (for scripts; the map uses the socket)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use colony_core::{DomainEvent, Envelope, HostId};
use colony_source::now_ms;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::pty::SpawnRequest;
use crate::{delta_messages, log, Shared};

type Params = Query<HashMap<String, String>>;

/// Pause between pasting a message and pressing Enter, so the TUI has taken
/// the paste before the submit arrives.
const PASTE_SETTLE: Duration = Duration::from_millis(80);

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/ws", get(ws))
        .route("/api/agents", get(agents))
        .route("/api/ack", post(ack))
        .with_state(shared)
}

fn authorized(shared: &Shared, params: &HashMap<String, String>, headers: &HeaderMap) -> bool {
    let from_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let given = params.get("token").map(String::as_str).or(from_header);
    // Only local pages may connect from a browser context.
    let origin_ok = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).is_none_or(|o| {
        o == "tauri://localhost"
            || o.starts_with("http://tauri.localhost")
            || o.starts_with("http://localhost:")
            || o.starts_with("http://127.0.0.1:")
    });
    origin_ok && given == Some(shared.token.as_str())
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "missing or wrong token; see ~/.colony/daemon.json").into_response()
}

async fn snapshot(shared: &Shared) -> String {
    let hosts = shared.pty.hosts(&shared.distros.read().await);
    let colony = shared.colony.read().await;
    let agents: Vec<_> = colony.agents.values().collect();
    json!({ "type": "snapshot", "now": now_ms(), "agents": agents, "hosts": hosts }).to_string()
}

async fn agents(State(shared): State<Arc<Shared>>, Query(params): Params, headers: HeaderMap) -> Response {
    if !authorized(&shared, &params, &headers) {
        return unauthorized();
    }
    ([(header::CONTENT_TYPE, "application/json")], snapshot(&shared).await).into_response()
}

#[derive(Deserialize)]
struct AckBody {
    id: String,
}

async fn ack(
    State(shared): State<Arc<Shared>>,
    Query(params): Params,
    headers: HeaderMap,
    Json(body): Json<AckBody>,
) -> Response {
    if !authorized(&shared, &params, &headers) {
        return unauthorized();
    }
    Json(json!({ "ok": acknowledge(&shared, &body.id).await })).into_response()
}

async fn acknowledge(shared: &Shared, id: &str) -> bool {
    let msgs = {
        let mut colony = shared.colony.write().await;
        let changed = colony.acknowledge(id, now_ms());
        delta_messages(&colony, &changed)
    };
    let found = !msgs.is_empty();
    for m in msgs {
        let _ = shared.deltas.send(m);
    }
    found
}

/// Commands the map sends over its WebSocket. Using the socket rather than
/// HTTP keeps the browser's cross-origin rules out of the way.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Ack { id: String },
    /// Start a session (or resume one) in a terminal Colony owns.
    Spawn(SpawnRequest),
    /// Stream this terminal to the map: scrollback first, then live output.
    Attach { term: String },
    Detach,
    /// Raw keystrokes from the terminal pane.
    Input { term: String, data: String },
    /// A message from the reply box: pasted as one block, then submitted.
    Send { term: String, text: String },
    /// Esc: stop what the agent is doing, keep the session.
    Interrupt { term: String },
    Resize { term: String, cols: u16, rows: u16 },
    /// End the session's process.
    Kill { term: String },
    /// End a session Colony did not start (e.g. one the Claude desktop app
    /// keeps running in the background), so it can be resumed here.
    Terminate { id: String },
}

/// What one map connection is looking at.
#[derive(Default)]
struct Conn {
    attached: Option<String>,
}

fn error(message: impl std::fmt::Display) -> Option<String> {
    Some(json!({ "type": "error", "message": message.to_string() }).to_string())
}

fn term_data(term: &str, bytes: &[u8], reset: bool) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    json!({ "type": "term_data", "term": term, "data": data, "reset": reset }).to_string()
}

/// Run one command; returns a message to send back to this map, if any.
async fn handle_command(shared: &Shared, conn: &mut Conn, text: &str) -> Option<String> {
    let cmd = match serde_json::from_str::<Command>(text) {
        Ok(c) => c,
        Err(e) => return error(format!("unknown command ({e})")),
    };
    let pty = &shared.pty;
    let result = match cmd {
        Command::Ack { id } => {
            if !acknowledge(shared, &id).await {
                log(format!("ack for {id}: nothing to acknowledge"));
            }
            Ok(None)
        }
        Command::Spawn(req) => {
            let wsl = matches!(req.host, colony_core::HostId::Wsl(_));
            match pty.spawn(req) {
                Ok(s) => {
                    if wsl {
                        shared.wsl_wake.notify_one();
                    }
                    conn.attached = Some(s.term_id.clone());
                    Ok(Some(json!({ "type": "spawned", "term": s.term_id, "session_id": s.session_id }).to_string()))
                }
                Err(e) => Err(e),
            }
        }
        Command::Attach { term } => {
            let sb = pty.scrollback(&term);
            conn.attached = Some(term.clone());
            match sb {
                Some(bytes) => Ok(Some(term_data(&term, &bytes, true))),
                None => Err("that terminal has closed".into()),
            }
        }
        Command::Detach => {
            conn.attached = None;
            Ok(None)
        }
        Command::Input { term, data } => pty.input(&term, data.as_bytes()).map(|_| None),
        Command::Send { term, text } => match pty.paste(&term, &text) {
            Ok(()) => {
                tokio::time::sleep(PASTE_SETTLE).await;
                pty.input(&term, b"\r").map(|_| None)
            }
            Err(e) => Err(e),
        },
        Command::Interrupt { term } => pty.input(&term, b"\x1b").map(|_| None),
        Command::Resize { term, cols, rows } => pty.resize(&term, cols, rows).map(|_| None),
        Command::Kill { term } => pty.kill(&term).map(|_| None),
        Command::Terminate { id } => terminate(shared, &id).await.map(|_| None),
    };
    match result {
        Ok(reply) => reply,
        Err(e) => error(e),
    }
}

async fn ws(
    State(shared): State<Arc<Shared>>,
    Query(params): Params,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !authorized(&shared, &params, &headers) {
        return unauthorized();
    }
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("no origin").to_string();
    log(format!("map connected ({origin})"));
    upgrade.on_upgrade(move |socket| stream(shared, socket))
}

async fn stream(shared: Arc<Shared>, socket: WebSocket) {
    // Subscribe before taking the snapshot so no delta falls in the gap.
    let mut rx = shared.deltas.subscribe();
    let mut term_rx = shared.pty.output.subscribe();
    let mut conn = Conn::default();
    let (mut tx, mut incoming) = socket.split();
    if tx.send(Message::Text(snapshot(&shared).await.into())).await.is_err() {
        return;
    }
    loop {
        let out: Option<String> = tokio::select! {
            msg = rx.recv() => match msg {
                Ok(m) => Some(m),
                // Fell behind: resync with a fresh snapshot.
                Err(RecvError::Lagged(n)) => {
                    log(format!("map client lagged by {n} messages; resending snapshot"));
                    Some(snapshot(&shared).await)
                }
                Err(RecvError::Closed) => return,
            },
            chunk = term_rx.recv() => match chunk {
                Ok(c) if conn.attached.as_deref() == Some(c.term_id.as_str()) => Some(term_data(&c.term_id, &c.bytes, false)),
                Ok(_) => None,
                // Lost some output: redraw the attached terminal from scrollback.
                Err(RecvError::Lagged(_)) => conn
                    .attached
                    .as_ref()
                    .and_then(|t| shared.pty.scrollback(t).map(|b| term_data(t, &b, true))),
                Err(RecvError::Closed) => return,
            },
            frame = incoming.next() => match frame {
                Some(Ok(Message::Text(text))) => handle_command(&shared, &mut conn, text.as_str()).await,
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => None,
            },
        };
        if let Some(m) = out {
            if tx.send(Message::Text(m.into())).await.is_err() {
                return;
            }
        }
    }
}

/// End the copies of a session that Colony didn't start (for example one the
/// Claude desktop app keeps running in the background), so it can be resumed
/// here without two copies writing to one conversation. Each pid is re-checked
/// against its registry file first, so a reused pid is never touched. A
/// Colony terminal for the session is left alone; End session handles that.
async fn terminate(shared: &Shared, id: &str) -> Result<(), String> {
    let (session_id, pids, host, terminal) = {
        let colony = shared.colony.read().await;
        let a = colony.agents.get(id).ok_or("no such session")?;
        let mut pids = a.pids.clone();
        if pids.is_empty() {
            pids.extend(a.pid);
        }
        (a.session_id.clone(), pids, a.host.clone(), a.terminal.clone())
    };
    if pids.is_empty() {
        return Err("Colony doesn't know of another running copy of this session".into());
    }
    for pid in &pids {
        end_process(&session_id, *pid, &host).await?;
        log(format!("ended session {session_id} (pid {pid} on {host}) at the user's request"));
        let _ = shared
            .events
            .send(Envelope { ts: now_ms(), host: host.clone(), session_id: session_id.clone(), cwd: None, event: DomainEvent::SessionGone { pid: *pid } })
            .await;
    }
    if terminal.is_none() {
        // A forced exit skips Claude Code's SessionEnd hook; record the end here.
        let _ = shared
            .events
            .send(Envelope { ts: now_ms(), host, session_id, cwd: None, event: DomainEvent::SessionEnded })
            .await;
    }
    Ok(())
}

async fn end_process(session_id: &str, pid: u32, host: &HostId) -> Result<(), String> {
    match host {
        HostId::Windows => {
            let record = std::fs::read_to_string(colony_source::home_dir().join(".claude").join("sessions").join(format!("{pid}.json")))
                .ok()
                .and_then(|t| colony_core::SessionRecord::parse(&t).ok())
                .filter(|r| r.session_id == session_id);
            let Some(record) = record else { return Ok(()) };
            if !colony_source::process::alive(pid, record.proc_start()) {
                return Ok(());
            }
            // /T: also its tool and MCP child processes. /F: console programs
            // don't respond to a polite close request.
            let status = quiet("taskkill.exe").args(["/PID", &pid.to_string(), "/T", "/F"]).output().await.map_err(|e| e.to_string())?;
            if !status.status.success() {
                return Err(format!("taskkill failed: {}", String::from_utf8_lossy(&status.stderr).trim()));
            }
        }
        HostId::Wsl(distro) => {
            // SIGTERM lets Claude Code exit cleanly and run its SessionEnd hook.
            let status = quiet("wsl.exe").args(["-d", distro, "-e", "kill", "-TERM", &pid.to_string()]).output().await.map_err(|e| e.to_string())?;
            if !status.status.success() {
                return Err(format!("kill failed: {}", String::from_utf8_lossy(&status.stderr).trim()));
            }
        }
    }
    Ok(())
}

/// A command that won't flash a console window.
fn quiet(program: &str) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}
