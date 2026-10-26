//! The context-size nudge for Colony crews (`COLONY_SWARM` and `COLONY_CONTEXT_NUDGE` set).
//!
//! Every request re-sends the main context, so a big one is the largest silent cost of a
//! session. On `UserPromptSubmit`, when the context is over the limit Colony set, one
//! `additionalContext` asks Claude to keep it from growing: the swarm takes searches and test
//! runs, and files already read are not read again. Claude can't compact itself, so that isn't
//! asked of it.
//!
//! The hook learns the size from the tail of the payload's `transcript_path`: the last
//! main-thread assistant line's usage (input, cache read and cache creation tokens). Only the
//! last [`TAIL_BYTES`] are read. Once per crossing: `<colony home>/context/<session_id>.json`
//! remembers that the session is over the limit, and forgets it when the size falls under
//! again (a `/compact` or `/clear`). Every failure means "no nudge".

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{json, Value};

/// How much of the end of a transcript is read.
pub const TAIL_BYTES: u64 = 256 * 1024;
/// State directory under the colony home; pruned like the advisor's (`nudge::prune_state`).
pub const STATE_DIR: &str = "context";

/// Text of the nudge for a context of `tokens`.
pub fn text(tokens: u64) -> String {
    format!(
        "Your context is large (about {}k tokens) and every request re-sends all of it. Hand searches and test runs to the swarm subagents (swarm-search, swarm-grep, swarm-audit) so their output stays out of this conversation, and don't re-read files you already have.",
        tokens / 1000
    )
}

/// What to print for this payload, if anything. `limit` is the token count the nudge starts at.
/// Updates the session's state file when the session crosses the limit either way.
pub fn nudge(colony_home: &Path, v: &Value, limit: u64) -> Option<String> {
    if v.get("hook_event_name")?.as_str()? != "UserPromptSubmit" {
        return None;
    }
    // A subagent's prompt carries the parent's session id; only the main thread is nudged.
    if v.get("agent_id").and_then(Value::as_str).is_some_and(|a| !a.is_empty()) {
        return None;
    }
    let sid = v.get("session_id")?.as_str()?;
    if sid.is_empty() || sid.len() > 128 || !sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let size = context_tokens(Path::new(v.get("transcript_path")?.as_str()?))?;
    let dir = colony_home.join(STATE_DIR);
    let path = dir.join(format!("{sid}.json"));
    let was_over = fs::read_to_string(&path).is_ok_and(|t| t.contains("true"));
    let over = size >= limit;
    if over == was_over {
        return None;
    }
    // Without the state the nudge would repeat on every prompt: say nothing instead.
    save(&dir, &path, over).ok()?;
    over.then(|| json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": text(size)}}).to_string())
}

fn save(dir: &Path, path: &Path, over: bool) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&tmp, json!({ "over": over }).to_string())?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// The context size from the newest main-thread assistant line with usage in the last
/// [`TAIL_BYTES`] of a transcript: `input_tokens + cache_read_input_tokens +
/// cache_creation_input_tokens`. A cut first line and a half-written last one are skipped.
pub fn context_tokens(transcript: &Path) -> Option<u64> {
    let mut file = File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.take(TAIL_BYTES).read_to_end(&mut buf).ok()?;
    // Starting mid-file, the first line is cut somewhere: drop it.
    let body = if start > 0 { &buf[buf.iter().position(|&b| b == b'\n')? + 1..] } else { &buf[..] };
    body.rsplit(|&b| b == b'\n').filter(|l| contains(l, b"\"usage\"")).find_map(line_context)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The input side of one transcript line, if it is a main-thread assistant message with usage.
fn line_context(line: &[u8]) -> Option<u64> {
    let v: Value = serde_json::from_slice(line).ok()?;
    if v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let msg = v.get("message")?;
    if v.get("type").and_then(Value::as_str) != Some("assistant") && msg.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let usage = msg.get("usage")?;
    let count = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
    let cache_creation = usage.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or_else(|| {
        // Newer releases split writes by lifetime.
        usage.get("cache_creation").and_then(Value::as_object).map_or(0, |o| o.values().filter_map(Value::as_u64).sum())
    });
    let total = count("input_tokens") + count("cache_read_input_tokens") + cache_creation;
    // A synthetic message (an error or interrupt notice) has all-zero usage: no measurement.
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("colony-hook-context-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn assistant(input: u64, read: u64, create: u64) -> String {
        format!(r#"{{"type":"assistant","message":{{"role":"assistant","usage":{{"input_tokens":{input},"cache_read_input_tokens":{read},"cache_creation_input_tokens":{create},"output_tokens":9}}}}}}"#)
    }

    fn write(d: &Path, lines: &[String]) -> std::path::PathBuf {
        let p = d.join("t.jsonl");
        fs::write(&p, lines.join("\n") + "\n").unwrap();
        p
    }

    #[test]
    fn sums_the_input_side_of_the_newest_assistant_line() {
        let d = tmp("sum");
        let p = write(&d, &[assistant(1, 1, 1), r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.into(), assistant(5, 100_000, 2_000)]);
        assert_eq!(context_tokens(&p), Some(102_005));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sidechain_synthetic_and_usage_less_lines_are_skipped() {
        let d = tmp("skip");
        let side = assistant(1, 900_000, 0).replace(r#"{"type":"assistant""#, r#"{"isSidechain":true,"type":"assistant""#);
        let synthetic = assistant(0, 0, 0);
        let nothing = r#"{"type":"assistant","message":{"role":"assistant","content":[]}}"#.to_string();
        let p = write(&d, &[assistant(3, 40_000, 0), side, synthetic, nothing]);
        assert_eq!(context_tokens(&p), Some(40_003));
        assert_eq!(context_tokens(&write(&d, &[r#"{"type":"user"}"#.into()])), None, "no usage at all");
        assert_eq!(context_tokens(&d.join("missing.jsonl")), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_truncated_last_line_falls_back_to_the_one_before() {
        let d = tmp("trunc");
        let p = d.join("t.jsonl");
        let half = &assistant(9, 9_999_999, 0)[..60];
        fs::write(&p, format!("{}\n{half}", assistant(7, 50_000, 0))).unwrap();
        assert_eq!(context_tokens(&p), Some(50_007));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_only_the_tail_of_a_big_file() {
        let d = tmp("big");
        let p = d.join("t.jsonl");
        // ~3 MB of filler lines, with a huge-looking usage line at the start that must not be
        // reached, and the real last usage near the end.
        let filler = format!(r#"{{"type":"user","message":{{"role":"user","content":"{}"}}}}"#, "x".repeat(900));
        let mut text = assistant(1, 8_000_000, 0) + "\n";
        for _ in 0..3_000 {
            text += &filler;
            text.push('\n');
        }
        text += &assistant(2, 61_000, 0);
        text.push('\n');
        text += &filler;
        text.push('\n');
        fs::write(&p, &text).unwrap();
        assert!(text.len() as u64 > 10 * TAIL_BYTES);
        assert_eq!(context_tokens(&p), Some(61_002));
        // If the only usage line is before the tail, there is nothing to measure.
        let mut only_early = assistant(1, 8_000_000, 0) + "\n";
        for _ in 0..1_000 {
            only_early += &filler;
            only_early.push('\n');
        }
        fs::write(&p, only_early).unwrap();
        assert_eq!(context_tokens(&p), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn nudges_once_per_crossing_and_again_after_the_size_drops() {
        let d = tmp("cross");
        let home = d.join("home");
        let t = write(&d, &[assistant(1, 100_000, 0)]);
        let payload = |event: &str| {
            json!({"session_id": "s-1", "hook_event_name": event, "transcript_path": t.to_str().unwrap()})
        };
        let go = |event: &str| nudge(&home, &payload(event), 140_000);
        assert_eq!(go("UserPromptSubmit"), None, "under the limit");
        assert!(!home.join(STATE_DIR).exists(), "nothing written while under");

        write(&d, &[assistant(1, 150_000, 0)]);
        let out: Value = serde_json::from_str(&go("UserPromptSubmit").expect("over the limit")).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        assert!(out["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("150k tokens"));
        assert_eq!(go("UserPromptSubmit"), None, "once per crossing");

        write(&d, &[assistant(1, 150_000, 0), assistant(1, 20_000, 0)]);
        assert_eq!(go("UserPromptSubmit"), None, "after a compact: quiet, and re-armed");
        write(&d, &[assistant(1, 160_000, 0)]);
        assert!(go("UserPromptSubmit").is_some(), "crossing again nudges again");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn only_main_thread_prompts_are_considered() {
        let d = tmp("events");
        let home = d.join("home");
        let t = write(&d, &[assistant(1, 500_000, 0)]);
        let p = t.to_str().unwrap();
        for v in [
            json!({"session_id": "s", "hook_event_name": "Stop", "transcript_path": p}),
            json!({"session_id": "s", "hook_event_name": "PostToolUse", "transcript_path": p}),
            json!({"session_id": "s", "hook_event_name": "UserPromptSubmit", "agent_id": "a1", "transcript_path": p}),
            json!({"session_id": "../x", "hook_event_name": "UserPromptSubmit", "transcript_path": p}),
            json!({"session_id": "s", "hook_event_name": "UserPromptSubmit"}),
        ] {
            assert_eq!(nudge(&home, &v, 140_000), None, "{v}");
        }
        // A state dir that can't be written means silence.
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(STATE_DIR), "a file where the directory should be").unwrap();
        assert_eq!(nudge(&home, &json!({"session_id": "s", "hook_event_name": "UserPromptSubmit", "transcript_path": p}), 140_000), None);
        let _ = fs::remove_dir_all(&d);
    }
}
