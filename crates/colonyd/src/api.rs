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
