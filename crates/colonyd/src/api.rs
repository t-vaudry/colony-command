//! Local API for the map. Bound to 127.0.0.1 only, and every route needs the
//! token from `~/.colony/daemon.json` (query `?token=` or a Bearer header),
//! because this API can type into sessions.
//!
//! - `GET  /ws`         snapshot, then upsert/remove deltas as JSON text frames;
//!                      the map sends commands back on the same socket
//! - `GET  /api/agents` the current snapshot
//! - `POST /api/ack`    `{"id": "..."}`: mark finished work reviewed, or clear a
//!                      crash (for scripts; the map uses the socket)
//! - `POST /api/permission` a `PermissionRequest` hook payload; held until the
//!                      map answers, then the hook's decision JSON (or no body)

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

use crate::approvals::{Choice, Decision, Hold, MAX_WAIT};
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
        .route("/api/permission", post(permission))
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
    json!({ "type": "snapshot", "now": now_ms(), "agents": agents, "hosts": hosts, "leftovers": crate::worktree::leftovers() }).to_string()
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
    /// Switch a Colony-started session's model (types `/model <name>`).
    SetModel { id: String, model: String },
    /// Answer a held permission request.
    Permission {
        request_id: String,
        choice: Choice,
        #[serde(default)]
        message: Option<String>,
        /// For AskUserQuestion: chosen answer per question text.
        #[serde(default)]
        answers: Option<serde_json::Value>,
    },
    /// End a session Colony did not start (e.g. one the Claude desktop app
    /// keeps running in the background), so it can be resumed here.
    Terminate { id: String },
    /// Done with a session: end every copy of it and clear it off the map.
    Dismiss { id: String },
    /// Show a leftover worktree's folder in the file manager.
    OpenWorktree { session_id: String },
    /// Delete a leftover worktree and its branch, uncommitted work and all.
    DiscardWorktree { session_id: String },
    /// Stop listing a leftover; its folder and branch stay as they are.
    ForgetWorktree { session_id: String },
    /// Open the sign-in or install a stuck bot is waiting on, in a terminal for the user to finish.
    FixNeed {
        id: String,
        #[serde(default = "crate::pty::default_cols")]
        cols: u16,
        #[serde(default = "crate::pty::default_rows")]
        rows: u16,
    },
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
async fn handle_command(shared: &Arc<Shared>, conn: &mut Conn, text: &str) -> Option<String> {
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
            let chosen = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
            let host = req.host.clone();
            match spawn_session(shared, req).await {
                Ok(s) => {
                    if let Some(name) = chosen {
                        let _ = shared
                            .events
                            .send(Envelope { ts: now_ms(), host, session_id: s.session_id.clone(), cwd: None, event: DomainEvent::Renamed { name } })
                            .await;
                    }
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
        Command::FixNeed { id, cols, rows } => match crate::needs::start(shared, &id, cols, rows).await {
            Ok(term) => {
                conn.attached = Some(term.clone());
                Ok(Some(json!({ "type": "spawned", "term": term, "session_id": id }).to_string()))
            }
            Err(e) => Err(e),
        },
        Command::Terminate { id } => terminate(shared, &id).await.map(|_| None),
        Command::Dismiss { id } => dismiss(shared, &id).await.map(|_| None),
        Command::OpenWorktree { session_id } => leftover_command(shared, &session_id, Leftover::Open).await.map(|_| None),
        Command::DiscardWorktree { session_id } => leftover_command(shared, &session_id, Leftover::Discard).await.map(|_| None),
        Command::ForgetWorktree { session_id } => leftover_command(shared, &session_id, Leftover::Forget).await.map(|_| None),
        Command::SetModel { id, model } => set_model(shared, &id, &model).await.map(|_| None),
        Command::Permission { request_id, choice, message, answers } => shared.approvals.decide(&request_id, choice, message, answers).map(|_| None),
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
    // Count this map as able to answer permission requests while it's open.
    shared.approvals.maps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    struct Watching(Arc<crate::approvals::Approvals>);
    impl Drop for Watching {
        fn drop(&mut self) {
            self.0.maps.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _watching = Watching(shared.approvals.clone());
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

/// Remove the worktrees of sessions that have ended without being dismissed
/// (the terminal closed, Claude exited, a crash). A worktree with uncommitted
/// changes is never removed, and a branch is only deleted once merged, so this
/// is safe to retry; a session resumed later gets its worktree back.
pub async fn sweep_worktrees(shared: &Shared) {
    for w in crate::worktree::all() {
        // Already waiting for the user to decide.
        if w.kept.is_some() {
            continue;
        }
        let ended = {
            let colony = shared.colony.read().await;
            let mut mains = colony.agents.values().filter(|a| a.session_id == w.session_id && a.kind == colony_core::AgentKind::Main).peekable();
            if mains.peek().is_none() {
                // Left the map's history: ended long ago.
                now_ms().saturating_sub(w.created_at) > crate::REPLAY_MS
            } else {
                mains.all(|a| a.terminal.is_none() && matches!(a.state, colony_core::AgentState::Ended | colony_core::AgentState::Crashed))
            }
        };
        if ended {
            clean_up_worktree(shared, &w).await;
        }
    }
}

/// Remove a finished bot's worktree. Whatever can't go (uncommitted changes,
/// unmerged commits) stays, recorded as a leftover the map lists for the user.
async fn clean_up_worktree(shared: &Shared, w: &crate::worktree::Worktree) {
    use crate::worktree::{forget, kept_reason, mark_kept, remove};
    match remove(w).await {
        Ok(false) => forget(&w.session_id),
        Ok(true) => mark_kept(&w.session_id, format!("Unmerged commits on {}", w.branch), true),
        Err(e) => {
            log(format!("kept worktree {} of session {}: {e}", w.path, w.session_id));
            mark_kept(&w.session_id, kept_reason(&e), false);
        }
    }
    broadcast_leftovers(shared);
}

fn leftovers_message() -> String {
    json!({ "type": "leftovers", "items": crate::worktree::leftovers() }).to_string()
}

fn broadcast_leftovers(shared: &Shared) {
    let _ = shared.deltas.send(leftovers_message());
}

enum Leftover {
    Open,
    Discard,
    Forget,
}

/// A command about one leftover worktree.
async fn leftover_command(shared: &Shared, session_id: &str, what: Leftover) -> Result<(), String> {
    let w = crate::worktree::find(session_id).filter(|w| w.kept.is_some()).ok_or("that leftover is already gone")?;
    match what {
        Leftover::Open => return crate::worktree::open_folder(&w),
        Leftover::Discard => crate::worktree::discard(&w).await?,
        Leftover::Forget => {}
    }
    crate::worktree::forget(session_id);
    broadcast_leftovers(shared);
    Ok(())
}

/// Start a session, first giving it its own git worktree when asked. If the
/// terminal can't start, the worktree made for it is removed again.
async fn spawn_session(shared: &Shared, mut req: SpawnRequest) -> Result<crate::pty::Spawned, String> {
    let mut made = None;
    // Resuming a bot whose worktree was cleaned up while it was ended.
    if let Some(w) = req.resume.as_deref().and_then(crate::worktree::find) {
        crate::worktree::restore(&w).await?;
    }
    if req.isolate && req.resume.is_none() {
        let sid = req.session_id.get_or_insert_with(|| uuid::Uuid::new_v4().to_string()).clone();
        let label = req.name.clone().unwrap_or_default();
        let created = crate::worktree::create(&req.host, &req.dir, &sid, &label).await?;
        req.dir = created.dir;
        made = Some(created.record);
    }
    // A restart on another model reuses this request; it must not make a second worktree.
    req.isolate = false;
    match shared.pty.spawn(req).await {
        Ok(s) => {
            if let Some(w) = made {
                crate::worktree::remember(w);
            }
            Ok(s)
        }
        Err(e) => {
            if let Some(w) = made {
                let _ = crate::worktree::remove(&w).await;
            }
            Err(e)
        }
    }
}

/// End the copies of a session that Colony didn't start (for example one the
/// Claude desktop app keeps running in the background), so it can be resumed
/// here without two copies writing to one conversation. Each pid is re-checked
/// against its registry file first, so a reused pid is never touched. A
/// Colony terminal for the session is left alone; End session handles that.
async fn terminate(shared: &Shared, id: &str) -> Result<(), String> {
    let c = copies(shared, id).await?;
    if c.others.is_empty() {
        return Err("Colony doesn't know of another running copy of this session".into());
    }
    end_others(shared, &c).await?;
    if c.terminal.is_none() {
        // A forced exit skips Claude Code's SessionEnd hook; record the end here.
        let _ = shared
            .events
            .send(Envelope { ts: now_ms(), host: c.host, session_id: c.session_id, cwd: None, event: DomainEvent::SessionEnded })
            .await;
    }
    Ok(())
}

/// The user is done with a session: end every copy of it (Colony's terminal
/// and any outside one) and take it off the map, so its context isn't picked
/// up again by accident. The conversation stays on disk and can be resumed.
async fn dismiss(shared: &Shared, id: &str) -> Result<(), String> {
    let c = copies(shared, id).await?;
    if c.main_id.as_deref() != Some(id) {
        return Err("only a session's main bot can be dismissed".into());
    }
    end_others(shared, &c).await?;
    if let Some(term) = &c.terminal {
        // Already closed is fine: there's nothing left to end.
        if let Err(e) = shared.pty.kill(term) {
            log(format!("dismiss {id}: terminal {term}: {e}"));
        }
        // Windows won't remove a folder a process is still in.
        shared.pty.closed(term, Duration::from_secs(10)).await;
    }
    if let Some(w) = crate::worktree::find(&c.session_id) {
        // Uncommitted work stays where it is, and shows up in the map's leftovers.
        clean_up_worktree(shared, &w).await;
    }
    let msgs = {
        let mut colony = shared.colony.write().await;
        let changed = colony.dismiss(id, now_ms());
        save_dismissed(&colony);
        delta_messages(&colony, &changed)
    };
    log(format!("dismissed session {} at the user's request", c.session_id));
    for m in msgs {
        let _ = shared.deltas.send(m);
    }
    Ok(())
}

/// Where a session is running.
struct Copies {
    /// Set when `id` named a main agent.
    main_id: Option<String>,
    session_id: String,
    host: HostId,
    terminal: Option<String>,
    /// Processes outside Colony's own terminal.
    others: Vec<u32>,
}

async fn copies(shared: &Shared, id: &str) -> Result<Copies, String> {
    let colony = shared.colony.read().await;
    let a = colony.agents.get(id).ok_or("no such session")?;
    let mut others = a.other_pids();
    if others.is_empty() && a.terminal.is_none() {
        others.extend(a.pid);
    }
    Ok(Copies {
        main_id: (a.kind == colony_core::AgentKind::Main).then(|| a.id.clone()),
        session_id: a.session_id.clone(),
        host: a.host.clone(),
        terminal: a.terminal.clone(),
        others,
    })
}

/// End the copies of a session that Colony didn't start. Each pid is
/// re-checked against its registry file first, so a reused pid is never
/// touched.
async fn end_others(shared: &Shared, c: &Copies) -> Result<(), String> {
    let owned = shared.pty.owned.lock().unwrap().clone();
    for pid in &c.others {
        // Never a process Colony started, whatever the registry says.
        if colony_source::process::lineage(*pid).iter().any(|p| owned.contains_key(p)) {
            log(format!("not ending pid {pid}: it runs in a Colony terminal"));
            continue;
        }
        end_process(&c.session_id, *pid, &c.host).await?;
        log(format!("ended session {} (pid {pid} on {}) at the user's request", c.session_id, c.host));
        let _ = shared
            .events
            .send(Envelope { ts: now_ms(), host: c.host.clone(), session_id: c.session_id.clone(), cwd: None, event: DomainEvent::SessionGone { pid: *pid } })
            .await;
    }
    Ok(())
}

fn dismissed_path() -> std::path::PathBuf {
    colony_source::colony_home().join("dismissed.json")
}

/// Dismissals are kept on disk so a colonyd restart, which replays recent
/// history, doesn't put dismissed sessions back on the map.
fn save_dismissed(colony: &colony_core::Colony) {
    let body = serde_json::to_vec_pretty(&colony.dismissed).expect("serializes");
    if let Err(e) = std::fs::write(dismissed_path(), body) {
        log(format!("could not save dismissed sessions: {e}"));
    }
}

pub fn load_dismissed() -> std::collections::BTreeMap<String, colony_core::state::Dismissed> {
    std::fs::read(dismissed_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
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

/// The approval hook's call: hold the request until the map answers it, the
/// session moves on, or `MAX_WAIT` passes. No body means no decision, and
/// Claude Code's own prompt takes over.
async fn permission(State(shared): State<Arc<Shared>>, Query(params): Params, headers: HeaderMap, body: String) -> Response {
    if !authorized(&shared, &params, &headers) {
        return unauthorized();
    }
    let no_decision = || StatusCode::NO_CONTENT.into_response();
    let Ok(p) = colony_core::HookPayload::parse(&body) else { return no_decision() };
    if p.hook_event_name != "PermissionRequest" || !shared.approvals.anyone_watching() {
        return no_decision();
    }
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let agent_id = match &p.agent_id {
        Some(a) => colony_core::state::sub_id(&p.session_id, a),
        None => p.session_id.clone(),
    };
    let suggestions = p.extra.get("permission_suggestions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let rx = shared.approvals.hold(&request_id, agent_id, suggestions, p.tool_input.clone());
    // Requests over HTTP come from hooks on this Windows machine.
    let host = HostId::Windows;
    let tool = p.tool_name.clone().unwrap_or_else(|| "tool".into());
    log(format!("holding permission request {request_id} from {}: {tool}", p.session_id));
    let _ = shared
        .events
        .send(Envelope {
            ts: now_ms(),
            host: host.clone(),
            session_id: p.session_id.clone(),
            cwd: p.cwd.clone(),
            event: DomainEvent::PermissionAsked { request_id: request_id.clone(), agent_id: p.agent_id.clone(), tool, target: p.tool_target(), input: p.tool_input.as_ref().map(colony_core::state::trim_strings) },
        })
        .await;
    // However this call ends (answered, timed out, or the hook was killed),
    // take the request off the map.
    let events = shared.events.clone();
    let (sid, rid) = (p.session_id.clone(), request_id.clone());
    let _hold = Hold {
        approvals: shared.approvals.clone(),
        request_id: request_id.clone(),
        on_drop: Some(Box::new(move || {
            let _ = events.try_send(Envelope { ts: now_ms(), host, session_id: sid, cwd: None, event: DomainEvent::PermissionSettled { request_id: rid } });
        })),
    };
    let decision = tokio::select! {
        d = rx => d.unwrap_or(Decision::Pass),
        _ = tokio::time::sleep(MAX_WAIT) => Decision::Pass,
    };
    log(format!("permission request {request_id}: {decision:?}"));
    match decision.hook_output() {
        Some(out) => ([(header::CONTENT_TYPE, "application/json")], out).into_response(),
        None => no_decision(),
    }
}

/// Switch a session Colony started to another model by restarting it on that
/// model: `--resume <id> --model <name>`. The conversation carries over and,
/// unlike typing `/model`, the user's default model for new sessions stays as
/// it was. Only between turns, so no work is cut off.
async fn set_model(shared: &Shared, id: &str, model: &str) -> Result<(), String> {
    let model = crate::pty::checked_model(model)?.to_string();
    restart_session(shared, id, Some(model), None).await
}

/// Restart a session Colony started with `--resume`, optionally on another
/// model and with a first message (`prompt`). The restarted Claude gets a
/// fresh environment, which is how a program installed meanwhile is found.
pub async fn restart_session(shared: &Shared, id: &str, model: Option<String>, prompt: Option<String>) -> Result<(), String> {
    let (term, session_id, host, dir, busy, has_conversation) = {
        let colony = shared.colony.read().await;
        let a = colony.agents.get(id).ok_or("no such session")?;
        let term = a.terminal.clone().ok_or("Colony can only restart sessions it started")?;
        let dir = a.project_dir.clone().or_else(|| a.cwd.clone()).ok_or("Colony doesn't know this session's folder")?;
        // Nothing to resume until it has been given something to do.
        let has_conversation = a.last_prompt.is_some() || a.objective.is_some() || a.tool_calls > 0;
        (term, a.session_id.clone(), a.host.clone(), dir, a.state == colony_core::AgentState::Working, has_conversation)
    };
    if busy {
        return Err("Claude is in the middle of a task; try again when this turn ends.".into());
    }
    let mut req = shared.pty.request_for(&term).unwrap_or(SpawnRequest {
        host,
        dir,
        prompt: None,
        resume: None,
        name: None,
        permission_mode: None,
        chrome: Some(false),
        session_id: None,
        model: None,
        isolate: false,
        cols: 120,
        rows: 32,
    });
    if has_conversation {
        req.resume = Some(session_id.clone());
        req.session_id = None;
    } else {
        req.resume = None;
        req.session_id = Some(session_id.clone());
    }
    req.prompt = prompt;
    req.name = None;
    if model.is_some() {
        req.model = model.clone();
    }
    shared.pty.kill(&term)?;
    if !shared.pty.closed(&term, Duration::from_secs(10)).await {
        return Err("the session didn't stop; try again".into());
    }
    let spawned = shared.pty.spawn(req).await?;
    let host = shared.colony.read().await.agents.get(id).map(|a| a.host.clone()).unwrap_or(HostId::Windows);
    if let Some(model) = &model {
        let _ = shared.events.send(Envelope { ts: now_ms(), host, session_id, cwd: None, event: DomainEvent::ModelSet { model: model.clone() } }).await;
    }
    log(format!("restarted session{} in terminal {}", model.map(|m| format!(" on {m}")).unwrap_or_default(), spawned.term_id));
    Ok(())
}
