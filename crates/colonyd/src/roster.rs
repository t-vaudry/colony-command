//! Bringing bots back after a restart.
//!
//! A machine restart stops every session, and the history colonyd replays
//! can't say which were still open: an idle bot may have done nothing for
//! longer than the replay reaches, and shutdown may even have sent its
//! `SessionEnd`. So colonyd keeps a roster (`roster.json`) of the main sessions
//! on the map that haven't ended. When it starts, once replay and Colony's
//! terminals have settled, each one that isn't running is put back on the map
//! paused (see `pause`), with Resume ready in its folder or worktree. Sessions
//! Colony started (`sessions.json`) and live worktrees count too, so bots from
//! before the roster existed come back as well.
//!
//! Sessions in worktrees Colony has no record of are offered to the map
//! instead (see `found`).
//!
//! Dismissing a bot takes it out of all of these. A session that ends while
//! colonyd is watching leaves the roster with it, and is not brought back.

use std::collections::BTreeMap;
use std::time::Duration;

use colony_core::{AgentKind, AgentState, Colony};
use colony_source::{colony_home, now_ms};

use crate::pause::Paused;
use crate::{log, Shared};

/// How often the roster is brought up to date.
const EVERY: Duration = Duration::from_secs(10);

pub type Roster = BTreeMap<String, Paused>;

fn path() -> std::path::PathBuf {
    colony_home().join("roster.json")
}

fn load() -> Roster {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn store(roster: &Roster) {
    let body = serde_json::to_vec_pretty(roster).expect("serializes");
    let tmp = path().with_extension("json.tmp");
    if let Err(e) = std::fs::create_dir_all(colony_home()).and_then(|_| std::fs::write(&tmp, body)).and_then(|_| std::fs::rename(&tmp, path())) {
        log(format!("could not save roster.json: {e}"));
    }
}

/// The map's main bots that haven't ended, with a folder to resume in. Paused
/// ones are already kept by `pause`. `request`: how Colony started a
/// terminal, by terminal id.
pub fn snapshot(colony: &Colony, request: impl Fn(&str) -> Option<crate::pty::SpawnRequest>) -> Roster {
    colony
        .agents
        .values()
        .filter(|a| a.kind == AgentKind::Main && a.state != AgentState::Ended && a.paused_at.is_none())
        .filter_map(|a| {
            let dir = a.project_dir.clone().or_else(|| a.cwd.clone())?;
            let req = a.terminal.as_deref().and_then(&request);
            Some((
                a.session_id.clone(),
                Paused {
                    host: a.host.clone(),
                    dir: req.as_ref().map(|r| r.dir.clone()).unwrap_or(dir),
                    name: a.name.clone(),
                    at: a.last_event_at,
                    model: req.as_ref().and_then(|r| r.model.clone()).or_else(|| a.model.clone()),
                    permission_mode: req.as_ref().and_then(|r| r.permission_mode.clone()),
                    chrome: req.as_ref().and_then(|r| r.chrome),
                    interrupted: true,
                },
            ))
        })
        .collect()
}

/// What to put back: the roster from before, then sessions Colony started and
/// live worktrees it didn't have, leaving out sessions that are running,
/// already paused, or were dismissed.
pub fn plan(roster: Roster, records: &crate::resume::Records, worktrees: &[crate::worktree::Worktree], paused: &crate::pause::Records, colony: &Colony, now: u64) -> Roster {
    let mut all = roster;
    for (sid, r) in records {
        all.entry(sid.clone()).or_insert_with(|| Paused {
            host: r.request.host.clone(),
            dir: r.request.dir.clone(),
            name: r.request.name.clone().unwrap_or_default(),
            at: r.last_seen_at,
            model: r.request.model.clone(),
            permission_mode: r.request.permission_mode.clone(),
            chrome: r.request.chrome,
            interrupted: true,
        });
    }
    // Ones with something left for the user to decide on were cleaned up: not a bot any more.
    for w in worktrees.iter().filter(|w| w.kept.is_none()) {
        all.entry(w.session_id.clone()).or_insert_with(|| Paused {
            host: w.host.clone(),
            dir: w.path.clone(),
            name: String::new(),
            at: w.created_at,
            model: None,
            permission_mode: None,
            chrome: None,
            interrupted: true,
        });
    }
    all.retain(|sid, _| {
        let a = colony.agents.get(sid);
        let running = a.is_some_and(|a| a.paused_at.is_some() || a.terminal.is_some() || !a.pids.is_empty());
        // Cut off mid-task: `resume` restarts it on its own.
        let restarting = a.is_some_and(|a| a.state == AgentState::Crashed) && records.get(sid).is_some_and(|r| !r.quiet);
        !running && !restarting && !paused.contains_key(sid) && !colony.dismissed.contains_key(sid)
    });
    for r in all.values_mut() {
        r.at = now;
    }
    all
}

/// At startup: put back the bots a restart stopped, then keep the roster
/// current. Call once replay and terminals have settled.
pub async fn run(shared: std::sync::Arc<Shared>) {
    let back = {
        let colony = shared.colony.read().await;
        plan(load(), &crate::resume::load_all(), &crate::worktree::all(), &crate::pause::load(), &colony, now_ms())
    };
    if !back.is_empty() {
        let mut paused = crate::pause::load();
        paused.extend(back.clone());
        crate::pause::store(&paused);
        for (sid, r) in &back {
            crate::pause::show(&shared, sid, r).await;
        }
        log(format!("put back {} session(s) that a restart stopped", back.len()));
    }
    crate::found::run(&shared).await;
    let mut last = None;
    loop {
        // Let the events just sent land first.
        tokio::time::sleep(EVERY).await;
        let (roster, revived) = {
            let colony = shared.colony.read().await;
            let roster = snapshot(&colony, |t| shared.pty.request_for(t));
            // Resumed outside Colony: running again, so no longer paused.
            let revived: Vec<String> = crate::pause::load()
                .into_iter()
                .filter(|(sid, r)| r.interrupted && colony.agents.get(sid).is_some_and(|a| a.paused_at.is_none()))
                .map(|(sid, _)| sid)
                .collect();
            (roster, revived)
        };
        for sid in revived {
            crate::pause::forget(&sid);
        }
        if last.as_ref() != Some(&roster) {
            store(&roster);
            last = Some(roster);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_core::{DomainEvent, Envelope, HostId};

    fn colony_with(events: Vec<(&str, DomainEvent)>) -> Colony {
        let mut c = Colony::new();
        for (i, (sid, event)) in events.into_iter().enumerate() {
            c.apply(&Envelope { ts: 1_000 + i as u64, host: HostId::Windows, session_id: sid.into(), cwd: Some(format!("C:/{sid}")), event });
        }
        c
    }

    fn entry(dir: &str) -> Paused {
        Paused { host: HostId::Windows, dir: dir.into(), name: "Indigo".into(), at: 1, model: None, permission_mode: None, chrome: None, interrupted: true }
    }

    fn started() -> DomainEvent {
        DomainEvent::SessionStarted { source: Some("startup".into()), model: None }
    }

    #[test]
    fn open_bots_are_on_the_roster_and_ended_ones_are_not() {
        let c = colony_with(vec![
            ("idle", started()),
            ("idle", DomainEvent::TurnEnded { last_message: Some("Done.".into()) }),
            ("gone", started()),
            ("gone", DomainEvent::SessionEnded),
        ]);
        let r = snapshot(&c, |_| None);
        assert_eq!(r.keys().collect::<Vec<_>>(), ["idle"]);
        assert_eq!(r["idle"].dir, "C:/idle");
        assert!(r["idle"].interrupted);
    }

    #[test]
    fn a_restart_brings_back_what_is_not_running_paused_or_dismissed() {
        let mut c = colony_with(vec![
            ("live", DomainEvent::TerminalAttached { term_id: "t".into(), dir: "C:/live".into(), pid: None }),
            // Replay saw shutdown's SessionEnd: still brought back.
            ("ended", DomainEvent::SessionEnded),
            ("bye", started()),
        ]);
        c.dismiss("bye", 2_000);
        let roster: Roster = ["live", "ended", "quiet", "bye", "held"].into_iter().map(|s| (s.to_string(), entry(&format!("C:/{s}")))).collect();
        let paused: crate::pause::Records = [("held".to_string(), entry("C:/held"))].into();
        let wt = crate::worktree::Worktree {
            session_id: "wt".into(),
            host: HostId::Windows,
            repo: "C:/repo".into(),
            path: "C:/repo/.colony/worktrees/x".into(),
            branch: "colony/x".into(),
            created_at: 0,
            kept: None,
            folder_gone: false,
        };
        let mut kept = wt.clone();
        kept.session_id = "kept".into();
        kept.kept = Some("uncommitted changes".into());
        let back = plan(roster, &crate::resume::Records::new(), &[wt, kept], &paused, &c, 9_000);
        assert_eq!(back.keys().collect::<Vec<_>>(), ["ended", "quiet", "wt"]);
        assert_eq!(back["wt"].dir, "C:/repo/.colony/worktrees/x");
        assert_eq!(back["wt"].name, "", "the map names it");
        assert!(back.values().all(|r| r.at == 9_000 && r.interrupted));
    }

    #[test]
    fn put_back_bots_show_as_paused_by_a_restart_and_ignore_older_history() {
        let mut c = colony_with(vec![("s", started()), ("s", DomainEvent::SessionEnded)]);
        c.apply(&Envelope { ts: 5_000, host: HostId::Windows, session_id: "s".into(), cwd: Some("C:/s".into()), event: DomainEvent::Restored });
        // Replayed late: from before the restart.
        c.apply(&Envelope { ts: 4_000, host: HostId::Windows, session_id: "s".into(), cwd: None, event: DomainEvent::SessionEnded });
        let a = &c.agents["s"];
        assert_eq!((a.state, a.paused_at, a.reason.as_deref()), (AgentState::Idle, Some(5_000), Some(colony_core::state::RESTORED_REASON)));
        assert!(snapshot(&c, |_| None).is_empty(), "paused bots are kept by pause, not the roster");
    }
}
