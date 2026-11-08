//! Keeps a workflow session from stopping before its required phases have run.
//!
//! Active only when Colony set `COLONY_WORKFLOW_REQUIRED` (comma-separated phase names) on the
//! session. The hook sees the Skill calls and `echo colony-phase:<name>` markers that start
//! phases, remembers them in `<colony home>/workflow/<session_id>.json`, and when the session
//! tries to stop with required phases still unrun it blocks the stop, at most `MAX_BLOCKS`
//! times per user prompt so a stuck session still ends. Every failure here means "no block".

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

const STATE_MAX_BYTES: u64 = 16 * 1024;
/// Stops blocked per user prompt, so a session that can't make progress still ends.
pub const MAX_BLOCKS: u32 = 6;
pub const STATE_DIR: &str = "workflow";

#[derive(Default, PartialEq, Clone, Debug)]
struct State {
    seen: Vec<String>,
    blocks: u32,
}

/// The phase a tool call starts, if it starts one.
fn started(v: &Value) -> Option<String> {
    let input = v.get("tool_input")?;
    match v.get("tool_name")?.as_str()? {
        "Skill" => {
            let s = input.get("skill")?.as_str()?.trim().trim_start_matches('/');
            Some(s.rsplit(':').next().unwrap_or(s).to_string())
        }
        "Bash" => {
            let rest = input.get("command")?.as_str()?.trim().strip_prefix("echo")?.trim_start();
            let name = rest.trim_matches(|c| c == '"' || c == '\'').strip_prefix("colony-phase:")?.trim();
            (!name.is_empty() && name.len() <= 64).then(|| name.to_string())
        }
        _ => None,
    }
}

/// What to print for this payload, if anything: a block when the session tries to stop early.
pub fn gate(colony_home: &Path, v: &Value, required: &str) -> Option<String> {
    let required: Vec<&str> = required.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if required.is_empty() {
        return None;
    }
    let event = v.get("hook_event_name")?.as_str()?;
    // A swarm agent's calls carry the parent's session_id: they don't start phases.
    if v.get("agent_id").and_then(Value::as_str).is_some_and(|a| !a.is_empty()) {
        return None;
    }
    if !matches!(event, "UserPromptSubmit" | "PreToolUse" | "Stop") {
        return None;
    }
    let sid = v.get("session_id")?.as_str()?;
    if sid.is_empty() || sid.len() > 128 || !sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let dir = colony_home.join(STATE_DIR);
    let path = dir.join(format!("{sid}.json"));
    let old = load(&path);
    let mut s = old.clone();
    let mut out = None;
    match event {
        "UserPromptSubmit" => s.blocks = 0,
        "PreToolUse" => {
            if let Some(p) = started(v) {
                if !s.seen.iter().any(|x| x.eq_ignore_ascii_case(&p)) && s.seen.len() < 64 {
                    s.seen.push(p);
                }
            }
        }
        "Stop" => {
            let missing: Vec<&str> = required.iter().copied().filter(|r| !s.seen.iter().any(|x| x.eq_ignore_ascii_case(r))).collect();
            if !missing.is_empty() && s.blocks < MAX_BLOCKS {
                s.blocks += 1;
                let reason = format!(
                    "This session follows a workflow and these required phases have not run yet: {}. Do not stop. Start the next one, {}, now, as the workflow describes (run its skill, or its `echo colony-phase:<name>` marker first), then carry on through the rest. Only skip a phase marked optional.",
                    missing.join(", "),
                    missing[0]
                );
                out = Some(json!({"decision": "block", "reason": reason}).to_string());
            }
        }
        _ => {}
    }
    if s != old && save(&dir, &path, &s).is_err() {
        // Without the state a block could repeat without count: say nothing instead.
        return None;
    }
    out
}

fn load(path: &Path) -> State {
    let small = fs::metadata(path).map(|m| m.len() <= STATE_MAX_BYTES).unwrap_or(false);
    if !small {
        return State::default();
    }
    let Some(v) = fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return State::default() };
    State {
        seen: v.get("seen").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        blocks: v.get("blocks").and_then(Value::as_u64).and_then(|b| u32::try_from(b).ok()).unwrap_or(0),
    }
}

fn save(dir: &Path, path: &Path, s: &State) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&tmp, json!({"seen": s.seen, "blocks": s.blocks}).to_string())?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("colony-gate-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_dir_all(&d);
        d
    }
    fn ev(event: &str, extra: Value) -> Value {
        let mut v = json!({"hook_event_name": event, "session_id": "s1"});
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    }
    fn marker(name: &str) -> Value {
        ev("PreToolUse", json!({"tool_name": "Bash", "tool_input": {"command": format!("echo colony-phase:{name}")}}))
    }

    #[test]
    fn blocks_a_stop_until_required_phases_ran() {
        let h = home();
        let req = "Investigate,Plan,Publish";
        assert!(gate(&h, &marker("Investigate"), req).is_none());
        let out = gate(&h, &ev("Stop", json!({})), req).expect("blocked");
        assert!(out.contains("\"decision\":\"block\"") && out.contains("Plan, Publish"));
        gate(&h, &marker("Plan"), req);
        gate(&h, &ev("PreToolUse", json!({"tool_name": "Skill", "tool_input": {"skill": "plugin:Publish"}})), req);
        assert!(gate(&h, &ev("Stop", json!({})), req).is_none(), "everything ran");
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn gives_up_after_the_cap_and_a_new_prompt_resets_it() {
        let h = home();
        for _ in 0..MAX_BLOCKS {
            assert!(gate(&h, &ev("Stop", json!({})), "A").is_some());
        }
        assert!(gate(&h, &ev("Stop", json!({})), "A").is_none());
        gate(&h, &ev("UserPromptSubmit", json!({})), "A");
        assert!(gate(&h, &ev("Stop", json!({})), "A").is_some());
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn subagents_and_no_workflow_are_ignored() {
        let h = home();
        assert!(gate(&h, &ev("Stop", json!({})), "").is_none());
        assert!(gate(&h, &ev("Stop", json!({"agent_id": "a1"})), "A").is_none());
        let mut m = marker("A");
        m["agent_id"] = json!("a1");
        gate(&h, &m, "A");
        assert!(gate(&h, &ev("Stop", json!({})), "A").is_some(), "a subagent's marker doesn't count");
        let _ = fs::remove_dir_all(&h);
    }
}
