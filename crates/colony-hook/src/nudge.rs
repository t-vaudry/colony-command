//! Advisor nudges for Colony crews (a Sonnet session with an Opus advisor and Haiku swarm).
//!
//! Only active when Colony set `COLONY_ADVISOR` on the session. Three moments, each a nudge
//! and never a gate:
//!
//! - the same tool error twice (`PostToolUseFailure`): `additionalContext`, once per error;
//! - a plan about to lock (`PreToolUse` of `ExitPlanMode`): `additionalContext`, once per turn;
//! - work about to be called done (`Stop`, files changed this turn): one `decision: block`.
//!
//! The hooks run in separate processes, so what they need to remember lives in a small state
//! file per session, `<colony home>/advisor/<session_id>.json`. It is only written when the
//! advisor is attached and something changed, and every failure here means "no nudge".

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

/// A state file bigger than this is treated as corrupt (a real one is a few hundred bytes).
const STATE_MAX_BYTES: u64 = 16 * 1024;
/// Error signatures remembered per session; the oldest are forgotten first.
const SIGS_MAX: usize = 24;
const SIG_MAX_CHARS: usize = 160;
/// State files are kept at most this many, and this long.
pub const STATE_MAX_FILES: usize = 300;
pub const STATE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

pub const TWICE: &str = "This error has now happened twice in this session. Before trying again, consult the advisor (Opus) on why it keeps failing and what to change.";
pub const PLAN: &str = "Before this plan locks, consult the advisor on it if you have not already this turn, then exit plan mode.";
pub const DONE_AUDIT: &str = "Files changed this turn and nothing has audited them. Before finishing, run the swarm-audit subagent on a snapshot of the change (`git stash create` prints a commit to diff against; it does not touch the stash stack) and get the advisor's sign-off. If the change is trivial, just finish.";
pub const DONE: &str = "Files changed this turn. Before finishing, get the advisor's sign-off on the work. If the change is trivial, just finish.";

#[derive(Default, PartialEq, Clone, Debug)]
struct State {
    /// `(signature, times seen, nudged)`, oldest first.
    sigs: Vec<(String, u32, bool)>,
    plan_nudged: bool,
    dirty: bool,
    audited: bool,
    stop_blocked: bool,
}

impl State {
    fn parse(text: &str) -> Option<State> {
        let v: Value = serde_json::from_str(text).ok()?;
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let sigs = v
            .get("sigs")?
            .as_array()?
            .iter()
            .filter_map(|e| {
                let a = e.as_array()?;
                Some((a.first()?.as_str()?.to_string(), u32::try_from(a.get(1)?.as_u64()?).ok()?, a.get(2)?.as_bool()?))
            })
            .collect();
        Some(State { sigs, plan_nudged: flag("plan"), dirty: flag("dirty"), audited: flag("audited"), stop_blocked: flag("blocked") })
    }

    fn to_json(&self) -> String {
        json!({
            "sigs": self.sigs.iter().map(|(s, n, d)| json!([s, n, d])).collect::<Vec<_>>(),
            "plan": self.plan_nudged, "dirty": self.dirty, "audited": self.audited, "blocked": self.stop_blocked,
        })
        .to_string()
    }
}

/// What the nudges print for this payload, if anything. `swarm` says whether the audit
/// subagent exists. Updates the session's state file when the event changes it.
pub fn nudge(colony_home: &Path, v: &Value, swarm: bool) -> Option<String> {
    let event = v.get("hook_event_name")?.as_str()?;
    let tool = v.get("tool_name").and_then(Value::as_str).unwrap_or("");
    // Skip the state file for the events that can't matter (most tool calls).
    let relevant = match event {
        "UserPromptSubmit" | "PostToolUseFailure" | "SubagentStart" | "SubagentStop" | "Stop" => true,
        "PostToolUse" => is_edit(tool),
        "PreToolUse" => tool == "ExitPlanMode",
        _ => false,
    };
    if !relevant {
        return None;
    }
    let sid = v.get("session_id")?.as_str()?;
    if sid.is_empty() || sid.len() > 128 || !sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let dir = colony_home.join("advisor");
    let path = dir.join(format!("{sid}.json"));
    let old = load(&path);
    let mut s = old.clone();
    let mut out = None;
    match event {
        // A new turn: the last one's changes were presented already.
        "UserPromptSubmit" => {
            s.plan_nudged = false;
            s.dirty = false;
            s.audited = false;
            s.stop_blocked = false;
        }
        "PostToolUse" => {
            s.dirty = true;
            s.audited = false;
        }
        "SubagentStart" | "SubagentStop" => {
            if v.get("agent_type").and_then(Value::as_str) == Some("swarm-audit") {
                s.audited = true;
            }
        }
        "PostToolUseFailure" => {
            if let Some(sig) = error_signature(v) {
                match s.sigs.iter().position(|e| e.0 == sig) {
                    Some(i) => {
                        s.sigs[i].1 = s.sigs[i].1.saturating_add(1);
                        if s.sigs[i].1 >= 2 && !s.sigs[i].2 {
                            s.sigs[i].2 = true;
                            out = Some(context("PostToolUseFailure", TWICE));
                        }
                    }
                    None => {
                        s.sigs.push((sig, 1, false));
                        if s.sigs.len() > SIGS_MAX {
                            s.sigs.remove(0);
                        }
                    }
                }
            }
        }
        "PreToolUse" => {
            if !s.plan_nudged {
                s.plan_nudged = true;
                out = Some(context("PreToolUse", PLAN));
            }
        }
        "Stop" => {
            let active = v.get("stop_hook_active").and_then(Value::as_bool).unwrap_or(false);
            if !active && s.dirty && !s.audited && !s.stop_blocked {
                s.stop_blocked = true;
                out = Some(json!({"decision": "block", "reason": if swarm { DONE_AUDIT } else { DONE }}).to_string());
            }
        }
        _ => {}
    }
    if s != old && save(&dir, &path, &s).is_err() {
        // Without the state the nudge would repeat on every call: say nothing instead.
        return None;
    }
    out
}

fn is_edit(tool: &str) -> bool {
    matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

fn context(event: &str, text: &str) -> String {
    json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}}).to_string()
}

/// A missing file is a fresh session; a corrupt or oversized one is too (and gets overwritten).
fn load(path: &Path) -> State {
    let small = fs::metadata(path).map(|m| m.len() <= STATE_MAX_BYTES).unwrap_or(false);
    if !small {
        return State::default();
    }
    fs::read_to_string(path).ok().and_then(|t| State::parse(&t)).unwrap_or_default()
}

fn save(dir: &Path, path: &Path, s: &State) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&tmp, s.to_json())?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Tool name plus the first meaningful line of the error, with numbers, paths and hashes
/// collapsed so the same failure at another line, file or address matches. A Bash error
/// that is only "Exit code N" falls back to the command, so unrelated failures don't match.
pub fn error_signature(v: &Value) -> Option<String> {
    let tool = v.get("tool_name")?.as_str()?;
    let err = match v.get("error")? {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let meaningful = |l: &&str| {
        let l = l.trim().to_ascii_lowercase();
        !l.is_empty() && !(l.starts_with("exit code") && l.split_whitespace().count() <= 3)
    };
    let line = match err.lines().find(meaningful) {
        Some(l) => l.to_string(),
        None => {
            let input = v.get("tool_input")?;
            let cmd = ["command", "file_path", "path", "pattern", "url"].iter().find_map(|k| input.get(*k)?.as_str())?;
            format!("exit {}", cmd.lines().next().unwrap_or(""))
        }
    };
    let norm: Vec<String> = line.split_whitespace().map(collapse).collect();
    let norm: String = norm.join(" ").chars().take(SIG_MAX_CHARS).collect();
    Some(format!("{tool}:{norm}"))
}

fn collapse(tok: &str) -> String {
    if tok.contains('/') || tok.contains('\\') {
        return "<p>".into();
    }
    let hexish = |t: &str| t.len() >= 7 && t.chars().all(|c| c.is_ascii_hexdigit()) && t.chars().any(|c| c.is_ascii_digit());
    if hexish(tok.trim_matches(|c: char| !c.is_ascii_alphanumeric()).trim_start_matches("0x")) {
        return "<h>".into();
    }
    let mut out = String::new();
    for c in tok.chars() {
        if c.is_ascii_digit() {
            if !out.ends_with('#') {
                out.push('#');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Deletes state files older than `max_age`, then the least recently written beyond `max_files`.
/// Best effort, like `prune_spool`.
pub fn prune_state(dir: &Path, now: SystemTime, max_files: usize, max_age: Duration) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json" || x == "tmp"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();
    let cutoff = now.checked_sub(max_age).unwrap_or(SystemTime::UNIX_EPOCH);
    let fresh = files.partition_point(|(t, _)| *t < cutoff);
    let excess = (files.len() - fresh).saturating_sub(max_files);
    for (_, p) in files.iter().take(fresh + excess) {
        let _ = fs::remove_file(p);
    }
}
