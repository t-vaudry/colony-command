//! Picking up sessions that were cut off.
//!
//! Colony remembers how it started each bot (`sessions.json`). If the bot's
//! process dies without the session ending (the terminal host was killed, the
//! machine rebooted), the map keeps it as crashed, its worktree is left alone,
//! and this supervisor restarts it with `--resume` in the same folder: at most
//! 3 times in a row, pausing longer each time, and at most 10 times a day. A
//! session that ends normally, is ended from Colony, or is dismissed is
//! forgotten here and never restarted. A bot that was idle or waiting for
//! review when it was cut off is left as it is. Creating
//! `<colony home>/no-auto-resume` turns restarting off.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use colony_core::AgentState;
use colony_source::{colony_home, now_ms};
use serde::{Deserialize, Serialize};

use crate::log;
use crate::pty::SpawnRequest;

/// Restarts tried in a row before leaving the session crashed for a person.
pub const MAX_ATTEMPTS: u32 = 3;
/// Restarts of one session in any 24 hours, however healthy it was between.
pub const DAILY_CAP: usize = 10;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// The first restart is immediate; each further one waits 30 s, then four times longer.
const FIRST_PAUSE_MS: u64 = 30_000;
/// A restarted session that has run this long counts as healthy again.
const HEALTHY_AFTER_MS: u64 = 10 * 60 * 1000;
/// A record whose session isn't on the map (yet, or any more) is kept this long
/// after it was last seen there or colonyd started, whichever is later.
const UNSEEN_GRACE_MS: u64 = 15 * 60 * 1000;
/// What the restarted bot is told, since `--resume` alone just waits at the prompt.
pub const RESUME_PROMPT: &str = "Colony restarted this session because its terminal was interrupted (not by you or the user). Continue the task from where you left off; if you were waiting for the user's answer, say so again briefly.";
/// Restarts begun in one pass; the rest wait for the next, so a reboot doesn't
/// start every bot at once.
const PER_PASS: usize = 2;
const EVERY: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub request: SpawnRequest,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_attempt_at: u64,
    /// When Colony first started the session.
    #[serde(default)]
    pub created_at: u64,
    /// Last time the session was on the map.
    #[serde(default)]
    pub last_seen_at: u64,
    /// It was idle or waiting for review when last seen: nothing to carry on with.
    #[serde(default)]
    pub quiet: bool,
    /// Times of restarts in the last 24 hours.
    #[serde(default)]
    pub recent: Vec<u64>,
    /// The map has been told restarting stopped; not repeated.
    #[serde(default)]
    pub gave_up: bool,
}

/// What the supervisor knows of a main agent.
#[derive(Debug, Clone, Copy)]
pub struct View {
    pub state: AgentState,
    pub has_terminal: bool,
    /// When it entered `state`.
    pub since: u64,
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
    /// Sessions out of restarts: tell the user once.
    pub give_up: Vec<String>,
}

fn pause_ms(attempts: u32) -> u64 {
    if attempts == 0 {
        0
    } else {
        FIRST_PAUSE_MS << (2 * (attempts - 1).min(8))
    }
}

/// `worktrees`: sessions that still have a worktree listed. `started`: when
/// this colonyd began, since replay takes a while to put sessions on the map.
pub fn plan(records: &Records, agents: &HashMap<String, View>, worktrees: &HashSet<String>, started: u64, now: u64) -> Plan {
    let mut p = Plan::default();
    for (sid, r) in records {
        match agents.get(sid) {
            // Not on the map: not known to be over. Replay may not have got
            // to it, or the machine was off longer than the replay reaches.
            None => {
                if now.saturating_sub(r.created_at.max(r.last_seen_at).max(started)) > UNSEEN_GRACE_MS && !worktrees.contains(sid) {
                    p.forget.push(sid.clone());
                }
            }
            Some(v) if v.state == AgentState::Ended => p.forget.push(sid.clone()),
            Some(v) if v.state == AgentState::Crashed && !v.has_terminal => {
                let recent = r.recent.iter().filter(|t| now.saturating_sub(**t) < DAY_MS).count();
                if r.attempts >= MAX_ATTEMPTS || recent >= DAILY_CAP {
                    if !r.gave_up {
                        p.give_up.push(sid.clone());
                    }
                } else if !r.quiet {
                    // From the later of the last restart and this crash.
                    let from = r.last_attempt_at.max(v.since);
                    if now.saturating_sub(from) >= pause_ms(r.attempts) {
                        p.resume.push(sid.clone());
                    }
                }
            }
            Some(v) if v.has_terminal && v.state != AgentState::Crashed => {
                if (r.attempts > 0 || r.gave_up) && now.saturating_sub(r.last_attempt_at) >= HEALTHY_AFTER_MS {
                    p.reset.push(sid.clone());
                }
            }
            Some(_) => {}
        }
    }
    p
}

// --- on disk -----------------------------------------------------------------

static STORE: Mutex<()> = Mutex::new(());

fn path() -> std::path::PathBuf {
    colony_home().join("sessions.json")
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A file that won't parse is set aside, not silently replaced by an empty one.
fn load_from(path: &std::path::Path) -> Records {
    let Ok(bytes) = std::fs::read(path) else { return Records::new() };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        log(format!("{} is unreadable ({e}); kept as sessions.json.bad", path.display()));
        let _ = std::fs::rename(path, path.with_extension("json.bad"));
        Records::new()
    })
}

/// Written to a side file and renamed, so a crash can't leave half a file.
fn save_to(path: &std::path::Path, all: &Records) {
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(all).expect("serializes");
    if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
        log(format!("could not save sessions.json: {e}"));
    }
}

fn load() -> Records {
    load_from(&path())
}

fn update(f: impl FnOnce(&mut Records)) {
    let _g = lock();
    let mut all = load();
    f(&mut all);
    save_to(&path(), &all);
}

/// Note how a session was started (a fresh start, or a restart of the same session).
pub fn remember(session_id: &str, req: &SpawnRequest) {
    update(|all| note(all, session_id, req, now_ms()));
}

/// Whether a restart of this session is still wanted: dismissing or ending it
/// removes the record. Checked just before a restart's terminal is launched.
pub fn still_wanted(session_id: &str) -> bool {
    let _g = lock();
    load().contains_key(session_id)
}

/// Never restart this session (dismissed, ended, or its worktree dealt with).
pub fn forget(session_id: &str) {
    update(|all| {
        all.remove(session_id);
    });
}

fn note(all: &mut Records, session_id: &str, req: &SpawnRequest, now: u64) {
    let mut request = req.clone();
    // What it was first asked to do is not repeated on a restart.
    request.prompt = None;
    request.resume = None;
    request.session_id = None;
    request.isolate = false;
    match all.get_mut(session_id) {
        Some(r) => r.request = request,
        None => {
            all.insert(session_id.to_string(), Record { request, attempts: 0, last_attempt_at: 0, created_at: now, last_seen_at: now, quiet: false, recent: Vec::new(), gave_up: false });
        }
    }
}

fn mark_attempt(session_id: &str, now: u64) {
    update(|all| {
        if let Some(r) = all.get_mut(session_id) {
            r.attempts += 1;
            r.last_attempt_at = now;
            r.recent.retain(|t| now.saturating_sub(*t) < DAY_MS);
            r.recent.push(now);
        }
    });
}

/// The restart never got going (the terminal host was down): it doesn't count.
fn refund_attempt(session_id: &str) {
    update(|all| {
        if let Some(r) = all.get_mut(session_id) {
            r.attempts = r.attempts.saturating_sub(1);
            r.recent.pop();
        }
    });
}

fn mark_gave_up(session_id: &str) {
    update(|all| {
        if let Some(r) = all.get_mut(session_id) {
            r.gave_up = true;
        }
    });
}

// --- supervisor ----------------------------------------------------------------

pub async fn supervise(shared: std::sync::Arc<crate::Shared>) {
    // Let the first replay of history settle before judging anything crashed.
    tokio::time::sleep(Duration::from_secs(60)).await;
    let started = now_ms();
    loop {
        let records = {
            let _g = lock();
            load()
        };
        if !records.is_empty() {
            let agents: HashMap<String, View> = {
                let colony = shared.colony.read().await;
                colony
                    .agents
                    .values()
                    .filter(|a| a.kind == colony_core::AgentKind::Main)
                    .map(|a| (a.session_id.clone(), View { state: a.state, has_terminal: a.terminal.is_some(), since: a.state_since }))
                    .collect()
            };
            update(|all| {
                let now = now_ms();
                for (sid, r) in all.iter_mut() {
                    let Some(v) = agents.get(sid) else { continue };
                    // Written when it changes, not every pass.
                    if now.saturating_sub(r.last_seen_at) > 5 * 60 * 1000 {
                        r.last_seen_at = now;
                    }
                    match v.state {
                        AgentState::Idle | AgentState::ReadyToReview => r.quiet = true,
                        AgentState::Crashed => {}
                        _ => r.quiet = false,
                    }
                }
            });
            // The plan sees what was just noted.
            let records = {
                let _g = lock();
                load()
            };
            let worktrees: HashSet<String> = crate::worktree::all().into_iter().map(|w| w.session_id).collect();
            let p = plan(&records, &agents, &worktrees, started, now_ms());
            update(|all| {
                for s in &p.forget {
                    all.remove(s);
                }
                for s in &p.reset {
                    if let Some(r) = all.get_mut(s) {
                        r.attempts = 0;
                        r.gave_up = false;
                    }
                }
            });
            for sid in p.give_up {
                mark_gave_up(&sid);
                let name = shared.colony.read().await.agents.get(&sid).map(|a| a.name.clone()).unwrap_or(sid.clone());
                log(format!("session {sid}: out of automatic restarts"));
                crate::api::notice(&shared, format!("{name}: was interrupted and Colony has stopped restarting it automatically; resume it yourself"));
            }
            let enabled = !colony_home().join("no-auto-resume").exists();
            for sid in p.resume.into_iter().filter(|_| enabled).take(PER_PASS) {
                // Dismissed, ended or resumed by hand since the plan was made?
                let still = {
                    let colony = shared.colony.read().await;
                    colony.agents.get(&sid).is_some_and(|a| a.state == AgentState::Crashed && a.terminal.is_none())
                };
                let record = {
                    let _g = lock();
                    load().remove(&sid)
                };
                let Some(r) = record.filter(|_| still) else { continue };
                mark_attempt(&sid, now_ms());
                log(format!("session {sid} was interrupted; resuming it in {} (attempt {} of {MAX_ATTEMPTS})", r.request.dir, r.attempts + 1));
                let mut req = r.request.clone();
                req.resume = Some(sid.clone());
                req.prompt = Some(RESUME_PROMPT.into());
                req.auto = true;
                if let Err(e) = crate::api::spawn_session(&shared, req).await {
                    log(format!("could not resume session {sid}: {e}"));
                    if e == crate::pty::NOT_CONNECTED {
                        refund_attempt(&sid);
                    }
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

    const NOW: u64 = 1_000_000_000;

    fn request() -> SpawnRequest {
        SpawnRequest {
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
            auto: false,
        }
    }

    fn rec(attempts: u32, last: u64) -> Record {
        Record { request: request(), attempts, last_attempt_at: last, created_at: 0, last_seen_at: 0, quiet: false, recent: Vec::new(), gave_up: false }
    }

    fn view(state: AgentState, has_terminal: bool, since: u64) -> View {
        View { state, has_terminal, since }
    }

    fn run(v: Option<View>, r: Record) -> Plan {
        run_with(v, r, &HashSet::new(), 0)
    }

    fn run_with(v: Option<View>, r: Record, worktrees: &HashSet<String>, started: u64) -> Plan {
        let records: Records = [("s".to_string(), r)].into();
        let agents: HashMap<_, _> = v.into_iter().map(|v| ("s".to_string(), v)).collect();
        plan(&records, &agents, worktrees, started, NOW)
    }

    fn crashed(since: u64) -> Option<View> {
        Some(view(AgentState::Crashed, false, since))
    }

    #[test]
    fn a_crashed_session_is_resumed_at_once() {
        assert_eq!(run(crashed(NOW - 6_000), rec(0, 0)).resume, ["s"]);
    }

    #[test]
    fn restarts_are_spaced_from_the_latest_crash_and_bounded() {
        assert!(run(crashed(NOW - 6_000), rec(1, NOW - 1_000)).resume.is_empty(), "too soon after the last try");
        assert_eq!(run(crashed(NOW - 50_000), rec(1, NOW - 40_000)).resume, ["s"]);
        // A bot that ran a while and then crashed anew is not restarted on the old clock.
        assert!(run(crashed(NOW - 1_000), rec(2, NOW - 400_000)).resume.is_empty());
        let out = run(crashed(NOW - 6_000), rec(MAX_ATTEMPTS, 0));
        assert!(out.resume.is_empty());
        assert_eq!(out.give_up, ["s"]);
        // Told once.
        let mut told = rec(MAX_ATTEMPTS, 0);
        told.gave_up = true;
        assert_eq!(run(crashed(NOW - 6_000), told), Plan::default());
    }

    #[test]
    fn a_daily_cap_holds_even_when_every_restart_was_healthy() {
        let mut r = rec(0, NOW - 3_600_000);
        r.recent = (0..DAILY_CAP as u64).map(|i| NOW - 1_000_000 - i).collect();
        let out = run(crashed(NOW - 6_000), r);
        assert!(out.resume.is_empty());
        assert_eq!(out.give_up, ["s"]);
    }

    #[test]
    fn ended_sessions_are_forgotten_but_unseen_ones_are_kept_a_while() {
        let ended = run(Some(view(AgentState::Ended, false, 0)), rec(0, 0));
        assert_eq!(ended.forget, ["s"]);
        assert!(ended.resume.is_empty());
        // Not on the map yet: just started, or off for longer than the replay reaches.
        let mut fresh = rec(0, 0);
        fresh.created_at = NOW - 1_000;
        assert_eq!(run(None, fresh), Plan::default());
        let wt: HashSet<String> = ["s".to_string()].into();
        assert_eq!(run_with(None, rec(0, 0), &wt, 0), Plan::default(), "its worktree is still listed");
        assert_eq!(run(None, rec(0, 0)).forget, ["s"]);
        // Just after colonyd started, replay may not have reached it; nor if it was seen lately.
        assert_eq!(run_with(None, rec(0, 0), &HashSet::new(), NOW - 1_000), Plan::default());
        let mut seen = rec(0, 0);
        seen.last_seen_at = NOW - 1_000;
        assert_eq!(run(None, seen), Plan::default());
    }

    #[test]
    fn idle_and_finished_bots_are_not_restarted() {
        let mut r = rec(0, 0);
        r.quiet = true;
        assert_eq!(run(crashed(NOW - 6_000), r), Plan::default());
    }

    #[test]
    fn running_sessions_are_left_alone_and_count_afresh_once_healthy() {
        let working = Some(view(AgentState::Working, true, 0));
        assert_eq!(run(working, rec(0, 0)), Plan::default());
        assert_eq!(run(working, rec(2, NOW - 60_000)), Plan::default());
        assert_eq!(run(working, rec(2, 0)).reset, ["s"]);
        // Mid-restart: crashed state but a terminal is already back.
        let mid = run(Some(view(AgentState::Crashed, true, 0)), rec(1, 0));
        assert_eq!(mid, Plan::default());
    }

    #[test]
    fn remembering_drops_the_first_prompt_but_keeps_the_count() {
        let mut all = Records::new();
        let mut req = request();
        req.prompt = Some("do the thing".into());
        note(&mut all, "s1", &req, 5);
        all.get_mut("s1").unwrap().attempts = 1;
        note(&mut all, "s1", &req, 99);
        assert_eq!((all["s1"].attempts, all["s1"].created_at), (1, 5));
        assert_eq!(all["s1"].request.prompt, None);
    }

    #[test]
    fn an_unreadable_file_is_set_aside_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("colony-resume-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sessions.json");
        std::fs::write(&file, b"{ \"s\": { truncated").unwrap();
        assert!(load_from(&file).is_empty());
        assert!(dir.join("sessions.json.bad").exists());
        let mut all = Records::new();
        note(&mut all, "s", &request(), 1);
        save_to(&file, &all);
        assert_eq!(load_from(&file).len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
