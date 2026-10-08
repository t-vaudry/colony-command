//! Local API for the map. Bound to 127.0.0.1 only, and every route needs the
//! token from `~/.colony/daemon.json` (query `?token=` or a Bearer header),
//! because later versions of this API can type into sessions.
//!
//! - `GET  /ws`        snapshot, then upsert/remove deltas as JSON text frames;
//!                     the map sends commands back on the same socket
//! - `GET  /api/agents` the current snapshot
//! - `POST /api/ack`   `{"id": "..."}`: mark finished work reviewed, or clear a
//!                     crash (for scripts; the map uses the socket)

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use colony_source::now_ms;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::{delta_messages, log, Shared};

type Params = Query<HashMap<String, String>>;

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
    let colony = shared.colony.read().await;
    let agents: Vec<_> = colony.agents.values().collect();
    json!({ "type": "snapshot", "now": now_ms(), "agents": agents }).to_string()
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
}

async fn handle_command(shared: &Shared, text: &str) {
    match serde_json::from_str::<Command>(text) {
        Ok(Command::Ack { id }) => {
            if !acknowledge(shared, &id).await {
                log(format!("ack for {id}: nothing to acknowledge"));
            }
        }
        Err(e) => log(format!("map sent an unknown command ({e}): {text}")),
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
    let (mut tx, mut incoming) = socket.split();
    if tx.send(Message::Text(snapshot(&shared).await.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(m) => if tx.send(Message::Text(m.into())).await.is_err() { return },
                // Fell behind: resync with a fresh snapshot.
                Err(RecvError::Lagged(n)) => {
                    log(format!("map client lagged by {n} messages; resending snapshot"));
                    if tx.send(Message::Text(snapshot(&shared).await.into())).await.is_err() { return }
                }
                Err(RecvError::Closed) => return,
            },
            frame = incoming.next() => match frame {
                Some(Ok(Message::Text(text))) => handle_command(&shared, text.as_str()).await,
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
        }
    }
}
