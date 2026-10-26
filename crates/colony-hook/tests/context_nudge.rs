//! The context-size nudge, end to end through the hook binary: one nudge per crossing, nothing
//! without `COLONY_SWARM` and `COLONY_CONTEXT_NUDGE`, and a `ColonyNudge` record for the map.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("colony-hook-ctx-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Runs the hook with `env` as the session's COLONY_SWARM and COLONY_CONTEXT_NUDGE.
fn hook(home: &Path, swarm: Option<&str>, limit: Option<&str>, input: &str) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_colony-hook"));
    cmd.env("COLONY_HOME", home).env("COLONY_CLAUDE_VERSION", "9.9.9").env_remove("CLAUDE_PID");
    cmd.env_remove("COLONY_ADVISOR").env_remove("COLONY_SWARM").env_remove("COLONY_CONTEXT_NUDGE");
    if let Some(s) = swarm {
        cmd.env("COLONY_SWARM", s);
    }
    if let Some(l) = limit {
        cmd.env("COLONY_CONTEXT_NUDGE", l);
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "hook must always exit 0");
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn transcript(dir: &Path, context: u64) -> PathBuf {
    let p = dir.join("t.jsonl");
    let line = json!({"type": "assistant", "message": {"role": "assistant", "usage": {"input_tokens": 3, "cache_read_input_tokens": context, "cache_creation_input_tokens": 0, "output_tokens": 5}}});
    fs::write(&p, format!("{{\"type\":\"user\"}}\n{line}\n")).unwrap();
    p
}

fn prompt(t: &Path) -> String {
    json!({"session_id": "sess-1", "hook_event_name": "UserPromptSubmit", "prompt": "go on", "transcript_path": t.to_str().unwrap()}).to_string()
}

/// The synthetic ColonyNudge payloads written to capture/ or spool/.
fn nudge_records(home: &Path) -> Vec<Value> {
    ["capture", "spool"]
        .iter()
        .flat_map(|d| fs::read_dir(home.join(d)).into_iter().flatten().flatten())
        .filter_map(|e| serde_json::from_str::<Value>(&fs::read_to_string(e.path()).ok()?).ok())
        .filter(|v| v["hook_event_name"] == "ColonyNudge")
        .collect()
}

#[test]
fn a_big_context_in_a_crew_session_nudges_once_and_is_reported() {
    let home = tmp("once");
    let t = transcript(&home, 150_000);
    let out: Value = serde_json::from_str(&hook(&home, Some("haiku"), Some("140000"), &prompt(&t))).expect("a nudge");
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let text = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(text.contains("swarm") && text.contains("150k"), "{text}");
    assert!(!text.to_lowercase().contains("compact"), "Claude can't compact itself: {text}");

    let records = nudge_records(&home);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!((records[0]["session_id"].as_str(), records[0]["kind"].as_str()), (Some("sess-1"), Some("context")));
    assert!(records[0]["ts"].as_u64().unwrap() > 1_700_000_000_000, "unix ms");

    assert_eq!(hook(&home, Some("haiku"), Some("140000"), &prompt(&t)), "", "once per crossing");
    assert_eq!(nudge_records(&home).len(), 1);
    // Down after a compact, then up again: a new crossing.
    let t = transcript(&home, 20_000);
    assert_eq!(hook(&home, Some("haiku"), Some("140000"), &prompt(&t)), "");
    let t = transcript(&home, 145_000);
    assert!(!hook(&home, Some("haiku"), Some("140000"), &prompt(&t)).is_empty());
    assert_eq!(nudge_records(&home).len(), 2);
}

#[test]
fn nothing_without_the_crew_variables_or_under_the_limit() {
    let home = tmp("quiet");
    let t = transcript(&home, 500_000);
    assert_eq!(hook(&home, None, Some("140000"), &prompt(&t)), "", "no swarm: not a crew session");
    assert_eq!(hook(&home, Some("haiku"), None, &prompt(&t)), "", "no limit set");
    assert_eq!(hook(&home, Some("haiku"), Some("lots"), &prompt(&t)), "", "limit isn't a number");
    let small = transcript(&home, 50_000);
    assert_eq!(hook(&home, Some("haiku"), Some("140000"), &prompt(&small)), "", "under the limit");
    assert!(nudge_records(&home).is_empty());
    // Not a prompt, a broken transcript path, or a payload that isn't JSON: silence.
    let stop = json!({"session_id": "sess-1", "hook_event_name": "Stop", "transcript_path": t.to_str().unwrap()}).to_string();
    assert_eq!(hook(&home, Some("haiku"), Some("140000"), &stop), "");
    let gone = json!({"session_id": "sess-1", "hook_event_name": "UserPromptSubmit", "transcript_path": "/no/such/file"}).to_string();
    assert_eq!(hook(&home, Some("haiku"), Some("140000"), &gone), "");
    assert_eq!(hook(&home, Some("haiku"), Some("140000"), "not json"), "");
}
