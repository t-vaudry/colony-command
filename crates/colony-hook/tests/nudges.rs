//! The advisor nudges of crew sessions, end to end through the hook binary: each one fires once,
//! nothing happens without `COLONY_ADVISOR`, and a broken state dir means silence, never a failure.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use colony_hook::nudge;
use serde_json::Value;

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("colony-hook-nudge-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Runs the hook; `crew` is (advisor, swarm) as the environment of the session.
fn hook(home: &Path, crew: (Option<&str>, Option<&str>), input: &str) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_colony-hook"));
    cmd.env("COLONY_HOME", home).env("COLONY_CLAUDE_VERSION", "9.9.9").env_remove("CLAUDE_PID");
    cmd.env_remove("COLONY_ADVISOR").env_remove("COLONY_SWARM");
    if let Some(a) = crew.0 {
        cmd.env("COLONY_ADVISOR", a);
    }
    if let Some(s) = crew.1 {
        cmd.env("COLONY_SWARM", s);
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "hook must always exit 0");
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

const ADV: (Option<&str>, Option<&str>) = (Some("opus"), None);
const CREW: (Option<&str>, Option<&str>) = (Some("opus"), Some("haiku"));

fn ev(event: &str, extra: &str) -> String {
    format!(r#"{{"session_id":"sess-1","hook_event_name":"{event}"{extra}}}"#)
}

fn fail(error: &str) -> String {
    let e = serde_json::to_string(error).unwrap();
    ev("PostToolUseFailure", &format!(r#","tool_name":"Bash","tool_input":{{"command":"cargo test"}},"error":{e}"#))
}

fn edit() -> String {
    ev("PostToolUse", r#","tool_name":"Edit","tool_input":{"file_path":"/p/a.rs"},"tool_response":{"type":"update"}"#)
}

fn json(out: &str) -> Value {
    serde_json::from_str(out).unwrap_or_else(|e| panic!("not json ({e}): {out:?}"))
}

#[test]
fn the_same_error_twice_nudges_once() {
    let home = tmp("twice");
    assert_eq!(hook(&home, ADV, &fail("Exit code 101\nerror[E0308]: mismatched types at /a/b.rs:12:5")), "");
    // Same failure, other line number and path: still the same error.
    let out = json(&hook(&home, ADV, &fail("Exit code 101\nerror[E0308]: mismatched types at /c/d.rs:99:1")));
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PostToolUseFailure");
    assert_eq!(out["hookSpecificOutput"]["additionalContext"], nudge::TWICE);
    assert_eq!(hook(&home, ADV, &fail("Exit code 101\nerror[E0308]: mismatched types at /c/d.rs:7:1")), "", "once per error");
    // A different error starts over.
    assert_eq!(hook(&home, ADV, &fail("Exit code 1\nthread 'main' panicked")), "");
}

#[test]
fn exit_code_only_errors_are_told_apart_by_command() {
    let home = tmp("exitonly");
    let a = ev("PostToolUseFailure", r#","tool_name":"Bash","tool_input":{"command":"cargo test"},"error":"Exit code 101""#);
    let b = ev("PostToolUseFailure", r#","tool_name":"Bash","tool_input":{"command":"npm run build"},"error":"Exit code 101""#);
    assert_eq!(hook(&home, ADV, &a), "");
    assert_eq!(hook(&home, ADV, &b), "", "another command is another error");
    assert!(hook(&home, ADV, &a).contains("happened twice"));
}

#[test]
fn a_plan_is_nudged_once_per_turn_and_never_denied() {
    let home = tmp("plan");
    let plan = ev("PreToolUse", r#","tool_name":"ExitPlanMode","tool_input":{"plan":"x"}"#);
    let out = json(&hook(&home, ADV, &plan));
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(out["hookSpecificOutput"]["additionalContext"], nudge::PLAN);
    assert!(out["hookSpecificOutput"].get("permissionDecision").is_none());
    assert_eq!(hook(&home, ADV, &plan), "", "once");
    hook(&home, ADV, &ev("UserPromptSubmit", r#","prompt":"go""#));
    assert!(hook(&home, ADV, &plan).contains("consult the advisor"), "next turn nudges again");
    // Other tools are left alone.
    assert_eq!(hook(&home, ADV, &ev("PreToolUse", r#","tool_name":"Edit","tool_input":{}"#)), "");
}

#[test]
fn changed_files_block_the_stop_once() {
    let home = tmp("stop");
    hook(&home, CREW, &ev("UserPromptSubmit", r#","prompt":"go""#));
    assert_eq!(hook(&home, CREW, &ev("Stop", r#","stop_hook_active":false"#)), "", "nothing changed: no block");
    hook(&home, CREW, &edit());
    let out = json(&hook(&home, CREW, &ev("Stop", r#","stop_hook_active":false"#)));
    assert_eq!(out["decision"], "block");
    assert_eq!(out["reason"], nudge::DONE_AUDIT);
    assert_eq!(hook(&home, CREW, &ev("Stop", r#","stop_hook_active":false"#)), "", "never twice in a turn");
    hook(&home, CREW, &ev("UserPromptSubmit", r#","prompt":"more""#));
    hook(&home, CREW, &edit());
    assert!(hook(&home, CREW, &ev("Stop", "")).contains("block"), "a new turn can block again");
}

#[test]
fn the_audit_reason_is_only_for_crews_with_a_swarm() {
    let home = tmp("noswarm");
    hook(&home, ADV, &edit());
    let out = json(&hook(&home, ADV, &ev("Stop", "")));
    assert_eq!(out["reason"], nudge::DONE);
    assert!(!nudge::DONE.contains("swarm-audit"));
}

#[test]
fn stop_hook_active_is_never_blocked() {
    let home = tmp("active");
    hook(&home, CREW, &edit());
    assert_eq!(hook(&home, CREW, &ev("Stop", r#","stop_hook_active":true"#)), "");
    // And it didn't use up the block.
    assert!(hook(&home, CREW, &ev("Stop", r#","stop_hook_active":false"#)).contains("block"));
}

#[test]
fn an_audit_after_the_last_change_lets_the_stop_through() {
    let home = tmp("audited");
    hook(&home, CREW, &edit());
    hook(&home, CREW, &ev("SubagentStart", r#","agent_id":"a1","agent_type":"swarm-audit""#));
    hook(&home, CREW, &ev("SubagentStop", r#","agent_id":"a1","agent_type":"swarm-audit""#));
    assert_eq!(hook(&home, CREW, &ev("Stop", "")), "");
    // A change after the audit needs a new one.
    hook(&home, CREW, &edit());
    assert!(hook(&home, CREW, &ev("Stop", "")).contains("block"));
    // Other subagents don't count.
    let home = tmp("explore");
    hook(&home, CREW, &edit());
    hook(&home, CREW, &ev("SubagentStop", r#","agent_id":"a1","agent_type":"Explore""#));
    assert!(hook(&home, CREW, &ev("Stop", "")).contains("block"));
}

#[test]
fn without_the_env_vars_nothing_is_printed_or_written() {
    let home = tmp("plain");
    let plan = ev("PreToolUse", r#","tool_name":"ExitPlanMode","tool_input":{}"#);
    for input in [fail("boom"), fail("boom"), plan, edit(), ev("Stop", "")] {
        assert_eq!(hook(&home, (None, None), &input), "");
    }
    // The swarm alone is not an advisor.
    assert_eq!(hook(&home, (None, Some("haiku")), &ev("Stop", "")), "");
    assert!(!home.join("advisor").exists(), "no state files");
}

#[test]
fn a_corrupt_or_unwritable_state_dir_prints_nothing() {
    let home = tmp("broken");
    // The state dir is a file: nothing can be written.
    fs::write(home.join("advisor"), "x").unwrap();
    for _ in 0..3 {
        assert_eq!(hook(&home, ADV, &fail("boom")), "");
    }
    hook(&home, ADV, &edit());
    assert_eq!(hook(&home, ADV, &ev("Stop", "")), "");
    // Corrupt and oversized state files read as a fresh session.
    let home = tmp("corrupt");
    fs::create_dir_all(home.join("advisor")).unwrap();
    fs::write(home.join("advisor/sess-1.json"), "{not json").unwrap();
    assert_eq!(hook(&home, ADV, &fail("boom")), "");
    fs::write(home.join("advisor/sess-1.json"), "x".repeat(100_000)).unwrap();
    assert_eq!(hook(&home, ADV, &fail("boom")), "");
    assert!(hook(&home, ADV, &fail("boom")).contains("twice"), "state recovers");
    // A session id that isn't a plain name never touches a path.
    let weird = r#"{"session_id":"../../x","hook_event_name":"Stop"}"#;
    assert_eq!(hook(&home, ADV, weird), "");
}

#[test]
fn payloads_are_still_recorded_first() {
    let home = tmp("record");
    hook(&home, ADV, &fail("boom"));
    assert_eq!(fs::read_dir(home.join("spool")).unwrap().count(), 1);
}

#[test]
fn state_files_are_pruned_by_age_and_count() {
    let dir = tmp("prune");
    let now = SystemTime::now();
    let age = |name: &str, days: u64| {
        let f = fs::File::create(dir.join(name)).unwrap();
        f.set_modified(now - Duration::from_secs(days * 24 * 3600)).unwrap();
    };
    age("old1.json", 10);
    age("old2.json", 9);
    age("a.json", 3);
    age("b.json", 2);
    age("c.json", 1);
    age("notes.txt", 30);
    nudge::prune_state(&dir, now, 2, Duration::from_secs(7 * 24 * 3600));
    let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["b.json", "c.json", "notes.txt"]);
}

#[test]
fn signatures_collapse_numbers_paths_and_hashes() {
    let sig = |e: &str| nudge::error_signature(&serde_json::from_str(&fail(e)).unwrap()).unwrap();
    assert_eq!(sig("error at /a/b.rs:12 (0xdeadbeef1)"), sig("error at C:\\x\\y.rs:98 (0xabc1234567)"));
    assert_ne!(sig("error one"), sig("error two"));
}
