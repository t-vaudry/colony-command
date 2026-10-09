//! Picking up sessions that were cut off.
//!
//! Colony remembers how it started each bot (`sessions.json`). If the bot's
//! process dies without the session ending (the terminal host was killed, the
//! machine rebooted), the map keeps it as crashed, its worktree is left alone,
//! and this supervisor restarts it with `--resume` in the same folder, a
//! bounded number of times with growing pauses, so unfinished work goes on
//! without anyone at the computer. A session that ends normally, is ended from
//! Colony, or is dismissed is forgotten here and never restarted.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use colony_core::AgentState;
use colony_source::now_ms;
use serde::{Deserialize, Serialize};

use crate::log;
use crate::pty::SpawnRequest;

/// Restarts tried before leaving the session crashed for a person to look at.
pub const MAX_ATTEMPTS: u32 = 3;
/// Pause before the first restart; each further one waits four times longer.
const FIRST_PAUSE_MS: u64 = 30_000;
/// A restarted session that has run this long counts as healthy again.
const HEALTHY_AFTER_MS: u64 = 10 * 60 * 1000;
/// What the restarted bot is told, since `--resume` alone just waits at the prompt.
pub const RESUME_PROMPT: &str = "Colony restarted this session because its terminal was interrupted (not by you or the user). \
Continue the task from where you left off; if you were waiting for the user's answer, say so again briefly.";
const EVERY: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub request: SpawnRequest,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_attempt_at: u64,
}

pub type Records = BTreeMap<String, Record>;

/// What one pass over the records should do.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Sessions to restart now.
    pub resume: Vec<String>,
    /// Sessions that are over (or gone): drop the record.
    pub forget: Vec<String>,
    /// Sessions running fine again: start counting attempts afresh.
    pub reset: Vec<String>,
}

fn pause_ms(attempts: u32) -> u64 {
    FIRST_PAUSE_MS << (2 * attempts.min(8))
}

/// `agents`: each main agent's state and whether it has a live terminal.
pub fn plan(records: &Records, agents: &HashMap<String, (AgentState, bool)>, now: u64) -> Plan {
    let mut p = Plan::default();
    for (sid, r) in records {
        match agents.get(sid) {
            // Dismissed, expired, or ended on purpose.
            None | Some((AgentState::Ended, _)) => p.forget.push(sid.clone()),
            Some((AgentState::Crashed, false)) => {
                if r.attempts < MAX_ATTEMPTS && now.saturating_sub(r.last_attempt_at) >= pause_ms(r.attempts) {
                    p.resume.push(sid.clone());
                }
            }
            Some((_, true)) => {
                if r.attempts > 0 && now.saturating_sub(r.last_attempt_at) >= HEALTHY_AFTER_MS {
                    p.reset.push(sid.clone());
                }
            }
            _ => {}
        }
    }
    p
}

// --- on disk -----------------------------------------------------------------

static STORE: Mutex<()> = Mutex::new(());

fn path() -> std::path::PathBuf {
    colony_source::colony_home().join("sessions.json")
}

fn load() -> Records {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(all: &Records) {
    if let Err(e) = std::fs::write(path(), serde_json::to_vec_pretty(all).expect("serializes")) {
        log(format!("could not save sessions.json: {e}"));
    }
}

fn update(f: impl FnOnce(&mut Records)) {
    let _g = STORE.lock().unwrap();
    let mut all = load();
    f(&mut all);
    save(&all);
}

/// Note how a session was started (a fresh start, or a restart of the same session).
pub fn remember(session_id: &str, req: &SpawnRequest) {
    update(|all| note(all, session_id, req));
}

fn note(all: &mut Records, session_id: &str, req: &SpawnRequest) {
    let mut request = req.clone();
    // What it was first asked to do is not repeated on a restart.
    request.prompt = None;
    request.resume = None;
    request.session_id = None;
    request.isolate = false;
    let kept = all.get(session_id).map(|r| (r.attempts, r.last_attempt_at)).unwrap_or_default();
    all.insert(session_id.to_string(), Record { request, attempts: kept.0, last_attempt_at: kept.1 });
}

fn mark_attempt(session_id: &str, now: u64) {
    update(|all| {
        if let Some(r) = all.get_mut(session_id) {
            r.attempts += 1;
            r.last_attempt_at = now;
        }
    });
}

// --- supervisor ----------------------------------------------------------------

pub async fn supervise(shared: std::sync::Arc<crate::Shared>) {
    // Let the first replay of history settle before judging anything crashed.
    tokio::time::sleep(Duration::from_secs(60)).await;
    loop {
        let records = {
            let _g = STORE.lock().unwrap();
            load()
        };
        if !records.is_empty() {
            let agents: HashMap<String, (AgentState, bool)> = {
                let colony = shared.colony.read().await;
                colony
                    .agents
                    .values()
                    .filter(|a| a.kind == colony_core::AgentKind::Main)
                    .map(|a| (a.session_id.clone(), (a.state, a.terminal.is_some())))
                    .collect()
            };
            let p = plan(&records, &agents, now_ms());
            update(|all| {
                for s in &p.forget {
                    all.remove(s);
                }
                for s in &p.reset {
                    if let Some(r) = all.get_mut(s) {
                        r.attempts = 0;
                    }
                }
            });
            for sid in p.resume {
                let r = &records[&sid];
                mark_attempt(&sid, now_ms());
                log(format!("session {sid} was interrupted; resuming it in {} (attempt {} of {MAX_ATTEMPTS})", r.request.dir, r.attempts + 1));
                let mut req = r.request.clone();
                req.resume = Some(sid.clone());
                req.prompt = Some(RESUME_PROMPT.into());
                if let Err(e) = crate::api::spawn_session(&shared, req).await {
                    log(format!("could not resume session {sid}: {e}"));
                }
            }
        }
        tokio::time::sleep(EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_core::HostId;

    fn rec(attempts: u32, last: u64) -> Record {
        let request = SpawnRequest {
            host: HostId::Windows,
            dir: "C:/w".into(),
            prompt: None,
            resume: None,
            name: None,
            permission_mode: None,
            chrome: None,
            session_id: None,
            model: None,
            isolate: false,
            cols: 120,
            rows: 32,
        };
        Record { request, attempts, last_attempt_at: last }
    }

    fn one(state: AgentState, term: bool, r: Record) -> Plan {
        let records: Records = [("s".to_string(), r)].into();
        let agents: HashMap<_, _> = [("s".to_string(), (state, term))].into();
        plan(&records, &agents, 1_000_000_000)
    }

    #[test]
    fn a_crashed_session_is_resumed() {
        assert_eq!(one(AgentState::Crashed, false, rec(0, 0)).resume, ["s"]);
    }

    #[test]
    fn restarts_are_spaced_and_bounded() {
        let now = 1_000_000_000;
        assert!(one(AgentState::Crashed, false, rec(1, now - 1_000)).resume.is_empty(), "too soon after the last try");
        assert_eq!(one(AgentState::Crashed, false, rec(1, now - 200_000)).resume, ["s"]);
        assert!(one(AgentState::Crashed, false, rec(MAX_ATTEMPTS, 0)).resume.is_empty(), "gives up after the limit");
    }

    #[test]
    fn sessions_that_ended_or_are_gone_are_forgotten_not_resumed() {
        let p = one(AgentState::Ended, false, rec(0, 0));
        assert_eq!((p.resume.len(), p.forget.as_slice()), (0, ["s".to_string()].as_slice()));
        let records: Records = [("s".to_string(), rec(0, 0))].into();
        let p = plan(&records, &HashMap::new(), 1);
        assert_eq!(p.forget, ["s"]);
    }

    #[test]
    fn running_sessions_are_left_alone_and_count_afresh_once_healthy() {
        assert_eq!(one(AgentState::Working, true, rec(0, 0)), Plan::default());
        assert_eq!(one(AgentState::Working, true, rec(2, 1_000_000_000 - 60_000)), Plan::default());
        assert_eq!(one(AgentState::Working, true, rec(2, 0)).reset, ["s"]);
        // Mid-restart: crashed state but a terminal is already back.
        assert_eq!(one(AgentState::Crashed, true, rec(1, 0)).resume.len(), 0);
    }

    #[test]
    fn remembering_drops_the_first_prompt_but_keeps_the_count() {
        let mut all = Records::new();
        let mut req = rec(0, 0).request;
        req.prompt = Some("do the thing".into());
        note(&mut all, "s1", &req);
        all.get_mut("s1").unwrap().attempts = 1;
        note(&mut all, "s1", &req);
        assert_eq!(all["s1"].attempts, 1);
        assert_eq!(all["s1"].request.prompt, None);
    }
}
