//! Pause and resume for sessions Colony started.
//!
//! You can't freeze a model mid-answer, so a pause waits for a safe point
//! between turns and then stops the session's process. The conversation is
//! already on disk, so resuming is `claude --resume <id>` in a new Colony
//! terminal (the same machinery as switching models). Sessions Colony didn't
//! start are never paused: their process isn't ours to stop.
//!
//! Paused sessions are remembered in `~/.colony/paused.json`, so they are still
//! on the map, still resumable, after colonyd restarts.

use std::collections::BTreeMap;
use std::time::Duration;

use colony_core::state::Agent;
use colony_core::{AgentKind, AgentState, DomainEvent, Envelope, HostId};
use colony_source::{colony_home, now_ms};
use serde::{Deserialize, Serialize};

use crate::pty::SpawnRequest;
use crate::{log, Shared};

/// Paused sessions are forgotten after this long.
const KEEP_MS: u64 = 14 * 24 * 60 * 60 * 1000;

/// What's needed to bring a paused session back, and to show it before then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Paused {
    pub host: HostId,
    pub dir: String,
    pub name: String,
    pub at: u64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub chrome: Option<bool>,
}

type Records = BTreeMap<String, Paused>;

fn path() -> std::path::PathBuf {
    colony_home().join("paused.json")
}

fn load() -> Records {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn store(records: &Records) {
    let body = serde_json::to_vec_pretty(records).expect("serializes");
    let tmp = path().with_extension("json.tmp");
    if let Err(e) = std::fs::create_dir_all(colony_home()).and_then(|_| std::fs::write(&tmp, body)).and_then(|_| std::fs::rename(&tmp, path())) {
        log(format!("could not save paused sessions: {e}"));
    }
}

/// Stop remembering a session as paused (resumed, dismissed, or running again).
pub fn forget(session_id: &str) {
    let mut records = load();
    if records.remove(session_id).is_some() {
        store(&records);
    }
}

/// Whether a session can be stopped now without cutting anything off: between
/// turns, nothing waiting on a permission, no background run or subagent out.
pub fn at_safe_point(colony: &colony_core::Colony, a: &Agent) -> bool {
    a.terminal.is_some()
        && matches!(a.state, AgentState::Idle | AgentState::AwaitingReply | AgentState::ReadyToReview)
        && a.permission.is_none()
        && a.current_tool.is_none()
        && a.background_tasks == 0
        && a.children.iter().all(|c| colony.agents.get(c).is_none_or(|k| k.state == AgentState::Ended))
}

fn envelope(a: &Agent, event: DomainEvent) -> Envelope {
    Envelope { ts: now_ms(), host: a.host.clone(), session_id: a.session_id.clone(), cwd: None, event }
}

/// The pause button: stop now if the session is between turns, otherwise at
/// the end of the current turn.
pub async fn request(shared: &Shared, id: &str) -> Result<(), String> {
    let a = {
        let colony = shared.colony.read().await;
        let a = colony.agents.get(id).ok_or("no such session")?.clone();
        if a.kind != AgentKind::Main {
            return Err("only a session's main bot can be paused".into());
        }
        if a.paused_at.is_some() {
            return Err("already paused".into());
        }
        if a.terminal.is_none() {
            return Err("Colony can only pause sessions it started: it stops the session's process between turns and resumes it later, and a session running in another terminal or app isn't Colony's to stop. Resume it in Colony to get Pause.".into());
        }
        a
    };
    let _ = shared.events.send(envelope(&a, DomainEvent::PauseRequested)).await;
    // The reducer applies the request in a moment; if this is already a safe
    // point, pause straight away rather than wait for the next sweep.
    let safe = {
        let colony = shared.colony.read().await;
        colony.agents.get(id).is_some_and(|a| at_safe_point(&colony, a))
    };
    if safe {
        pause_now(shared, id).await?;
    }
    Ok(())
}

pub async fn cancel(shared: &Shared, id: &str) -> Result<(), String> {
    let a = shared.colony.read().await.agents.get(id).cloned().ok_or("no such session")?;
    let _ = shared.events.send(envelope(&a, DomainEvent::PauseCancelled)).await;
    Ok(())
}

/// Stop the session's process and keep its bot, idle, on the map.
async fn pause_now(shared: &Shared, id: &str) -> Result<(), String> {
    let (a, req) = {
        let colony = shared.colony.read().await;
        let a = colony.agents.get(id).ok_or("no such session")?.clone();
        let term = a.terminal.clone().ok_or("that session isn't running in Colony's terminal")?;
        (a, shared.pty.request_for(&term))
    };
    let term = a.terminal.clone().expect("checked above");
    let dir = a.project_dir.clone().or_else(|| a.cwd.clone()).ok_or("Colony doesn't know this session's folder")?;
    let at = now_ms();
    let mut records = load();
    records.insert(
        a.session_id.clone(),
        Paused {
            host: a.host.clone(),
            dir,
            name: a.name.clone(),
            at,
            model: req.as_ref().and_then(|r| r.model.clone()).or_else(|| a.model.clone()),
            permission_mode: req.as_ref().and_then(|r| r.permission_mode.clone()),
            chrome: req.as_ref().and_then(|r| r.chrome),
        },
    );
    store(&records);
    // Tell the reducer first, so the process exiting reads as a pause.
    let mut e = envelope(&a, DomainEvent::Paused);
    e.ts = at;
    let _ = shared.events.send(e).await;
    if let Err(err) = shared.pty.kill(&term) {
        // Already gone: the pause stands, there is just nothing to stop.
        log(format!("pause {id}: terminal {term}: {err}"));
    } else if !shared.pty.closed(&term, Duration::from_secs(10)).await {
        return Err("the session didn't stop; it stays paused on the map, but check its terminal".into());
    }
    log(format!("paused session {} ({})", a.session_id, a.name));
    Ok(())
}

/// Start a paused session again with `--resume`.
pub async fn resume(shared: &Shared, id: &str) -> Result<(), String> {
    let a = shared.colony.read().await.agents.get(id).cloned().ok_or("no such session")?;
    if a.paused_at.is_none() {
        return Err("that session isn't paused".into());
    }
    let record = load().remove(&a.session_id);
    let dir = record
        .as_ref()
        .map(|r| r.dir.clone())
        .or_else(|| a.project_dir.clone())
        .or_else(|| a.cwd.clone())
        .ok_or("Colony doesn't know this session's folder")?;
    let req = SpawnRequest {
        host: record.as_ref().map(|r| r.host.clone()).unwrap_or_else(|| a.host.clone()),
        dir,
        prompt: None,
        resume: Some(a.session_id.clone()),
        name: None,
        permission_mode: record.as_ref().and_then(|r| r.permission_mode.clone()),
        chrome: record.as_ref().and_then(|r| r.chrome).or(Some(false)),
        session_id: None,
        model: record.as_ref().and_then(|r| r.model.clone()),
        isolate: false,
        cols: crate::pty::default_cols(),
        rows: crate::pty::default_rows(),
        auto: false,
    };
    let spawned = crate::api::spawn_session(shared, req).await?;
    forget(&a.session_id);
    log(format!("resumed paused session {} in terminal {}", a.session_id, spawned.term_id));
    Ok(())
}

/// Pause sessions that were asked to wait for a turn to end, once they get
/// to a safe point.
pub async fn sweep(shared: &Shared) {
    let due: Vec<String> = {
        let colony = shared.colony.read().await;
        colony.agents.values().filter(|a| a.pause_pending && at_safe_point(&colony, a)).map(|a| a.id.clone()).collect()
    };
    for id in due {
        if let Err(e) = pause_now(shared, &id).await {
            log(format!("pause {id}: {e}"));
        }
    }
}

/// After a colonyd restart, put paused sessions back on the map. Their events
/// carry the time of the pause, so replayed history from before it can't undo it.
pub async fn restore(shared: &Shared) {
    let mut records = load();
    let now = now_ms();
    let before = records.len();
    records.retain(|_, r| now.saturating_sub(r.at) < KEEP_MS);
    if records.len() != before {
        store(&records);
    }
    for (session_id, r) in records {
        let env = |event| Envelope { ts: r.at, host: r.host.clone(), session_id: session_id.clone(), cwd: Some(r.dir.clone()), event };
        let _ = shared.events.send(env(DomainEvent::Renamed { name: r.name.clone() })).await;
        let _ = shared.events.send(env(DomainEvent::Paused)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_core::Colony;

    fn agent_in(colony: &mut Colony, events: Vec<DomainEvent>) -> Agent {
        let mut t = 1_000;
        for event in events {
            t += 1_000;
            colony.apply(&Envelope { ts: t, host: HostId::Windows, session_id: "s".into(), cwd: Some("C:/x".into()), event });
        }
        colony.agents["s"].clone()
    }

    fn attached() -> DomainEvent {
        DomainEvent::TerminalAttached { term_id: "t".into(), dir: "C:/x".into(), pid: None }
    }

    #[test]
    fn only_between_turns_with_nothing_outstanding() {
        let mut colony = Colony::new();
        let a = agent_in(&mut colony, vec![attached(), DomainEvent::PromptSubmitted { preview: "go".into(), synthetic: false, task_ended: false }]);
        assert!(!at_safe_point(&colony, &a), "mid-turn");
        let a = agent_in(&mut colony, vec![DomainEvent::TurnEnded { last_message: Some("All done.".into()) }]);
        assert!(at_safe_point(&colony, &a), "between turns");
        let a = agent_in(&mut colony, vec![DomainEvent::TurnEnded { last_message: Some("Which one?".into()) }]);
        assert_eq!(a.state, AgentState::AwaitingReply);
        assert!(at_safe_point(&colony, &a), "waiting for your reply is between turns");
    }

    #[test]
    fn not_with_a_permission_request_open_or_a_background_run_out() {
        let mut colony = Colony::new();
        agent_in(&mut colony, vec![attached(), DomainEvent::TurnEnded { last_message: Some("ok".into()) }]);
        colony.agents.get_mut("s").unwrap().permission = Some(colony_core::state::PermissionAsk { request_id: "q".into(), tool: "Bash".into(), target: None, input: None, asked_at: 1 });
        assert!(!at_safe_point(&colony, &colony.agents["s"].clone()));
        colony.agents.get_mut("s").unwrap().permission = None;
        colony.agents.get_mut("s").unwrap().background_tasks = 1;
        assert!(!at_safe_point(&colony, &colony.agents["s"].clone()));
    }

    #[test]
    fn not_for_a_session_colony_does_not_own_or_one_with_a_subagent_running() {
        let mut colony = Colony::new();
        let a = agent_in(&mut colony, vec![DomainEvent::TurnEnded { last_message: Some("ok".into()) }]);
        assert!(!at_safe_point(&colony, &a), "no Colony terminal");
        agent_in(&mut colony, vec![attached(), DomainEvent::TurnEnded { last_message: Some("ok".into()) }]);
        assert!(at_safe_point(&colony, &colony.agents["s"].clone()));
        agent_in(&mut colony, vec![DomainEvent::SubagentStarted { agent_id: "a1".into(), agent_type: None }]);
        assert!(!at_safe_point(&colony, &colony.agents["s"].clone()), "a subagent is still out");
    }
}
