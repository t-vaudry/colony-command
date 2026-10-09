//! Fault injection for the on-disk sources: payloads from a future Claude Code,
//! corrupt or partial files, a spool that built up while no daemon ran, and
//! a daemon restarting in the middle of a session.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use colony_core::{DomainEvent, Envelope, HostId};
use colony_source::{now_ms, DirSource};

fn home(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("colony-src-robust-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("claude/sessions")).unwrap();
    fs::create_dir_all(d.join("colony/capture")).unwrap();
    d
}

fn source(d: &std::path::Path, since: u64) -> DirSource {
    DirSource::new(HostId::Windows, &d.join("claude"), &d.join("colony"), since)
}

/// A capture name `ms_ago` milliseconds in the past, `n` to keep names unique.
fn name(ms_ago: u64, n: u32) -> String {
    format!("{}-{n}.json", (now_ms() - ms_ago) as u128 * 1_000_000)
}

fn kinds(ev: &[Envelope]) -> Vec<&'static str> {
    ev.iter()
        .map(|e| match e.event {
            DomainEvent::SessionStarted { .. } => "start",
            DomainEvent::PromptSubmitted { .. } => "prompt",
            DomainEvent::ToolStarted { .. } => "tool",
            DomainEvent::TurnEnded { .. } => "stop",
            DomainEvent::SessionEnded => "end",
            _ => "other",
        })
        .collect()
}

// --- hook payload schema drift ---

#[test]
fn payloads_with_new_fields_new_events_and_odd_types_still_decode() {
    let d = home("drift");
    let cap = d.join("colony/capture");
    let payloads = [
        // Unknown extra fields, nested.
        r#"{"session_id":"s","hook_event_name":"SessionStart","source":"startup","future":{"a":[1,2,{"b":null}]},"x":1.5}"#,
        // A brand new event type: skipped, not fatal.
        r#"{"session_id":"s","hook_event_name":"QuantumFlux","whatever":true}"#,
        // Fields of the wrong type are ignored, the event survives.
        r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":42,"cwd":{"path":"x"},"transcript_path":null}"#,
        // Missing optional fields everywhere.
        r#"{"session_id":"s","hook_event_name":"PreToolUse"}"#,
        // tool_input as a string instead of an object; error as an object.
        r#"{"session_id":"s","hook_event_name":"PostToolUseFailure","tool_input":"ls","error":{"code":7},"agent_id":9}"#,
        r#"{"session_id":"s","hook_event_name":"Stop","last_assistant_message":["parts"]}"#,
        r#"{"session_id":"s","hook_event_name":"SessionEnd","reason":"other"}"#,
    ];
    let t = (now_ms() as u128) * 1_000_000;
    for (i, p) in payloads.iter().enumerate() {
        fs::write(cap.join(format!("{}-{i}.json", t + i as u128 * 1_000_000)), p).unwrap();
    }
    let ev = source(&d, 0).poll();
    assert_eq!(kinds(&ev), ["start", "prompt", "tool", "other", "stop", "end"], "{ev:?}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_payload_with_no_session_id_does_not_hold_up_the_ones_behind_it() {
    let d = home("nosid");
    let cap = d.join("colony/capture");
    // Fresh, so the half-written grace period would apply if it were treated as partial.
    fs::write(cap.join(name(20, 1)), r#"{"hook_event_name":"Stop"}"#).unwrap();
    fs::write(cap.join(name(10, 2)), r#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();
    let ev = source(&d, 0).poll();
    assert_eq!(kinds(&ev), ["stop"], "{ev:?}");
    let _ = fs::remove_dir_all(&d);
}

// --- corrupt and partial files ---

#[test]
fn garbage_in_the_capture_folder_is_survived() {
    let d = home("garbage");
    let cap = d.join("colony/capture");
    // Old garbage of every kind (past the half-written grace), then a good one.
    fs::write(cap.join(name(60_000, 1)), "").unwrap();
    fs::write(cap.join(name(59_000, 2)), [0xff, 0xfe, 0x00, 0x80]).unwrap();
    fs::write(cap.join(name(58_000, 3)), "{\"session_id\":\"s\",\"hook_ev").unwrap();
    fs::write(cap.join(name(57_000, 4)), "[]").unwrap();
    fs::create_dir(cap.join(name(56_000, 5))).unwrap();
    fs::write(cap.join("not-a-timestamp.json"), "{}").unwrap();
    fs::write(cap.join(name(1_000, 6)), r#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();
    let ev = source(&d, 0).poll();
    assert_eq!(kinds(&ev), ["stop"], "{ev:?}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_fresh_partial_capture_is_retried_then_read_once_complete() {
    let d = home("partial");
    let cap = d.join("colony/capture");
    let f = cap.join(name(10, 1));
    fs::write(&f, r#"{"session_id":"s","hook_event_name":"Sto"#).unwrap();
    let mut src = source(&d, 0);
    assert!(src.poll().is_empty());
    assert!(src.poll().is_empty(), "still waiting, not lost");
    fs::write(&f, r#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();
    assert_eq!(kinds(&src.poll()), ["stop"]);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn corrupt_and_odd_registry_files_are_ignored() {
    let d = home("registry");
    let s = d.join("claude/sessions");
    let pid = std::process::id();
    fs::write(s.join("111.json"), "").unwrap();
    fs::write(s.join("112.json"), "{\"pid\":112,\"sessionI").unwrap();
    fs::write(s.join("113.json"), [0xff, 0xff, 0xfe]).unwrap();
    fs::write(s.join("114.json"), "null").unwrap();
    fs::write(s.join("115.json"), r#"{"pid":"x","sessionId":3}"#).unwrap();
    fs::write(s.join("0.json"), r#"{"pid":0,"sessionId":"zero"}"#).unwrap();
    fs::write(s.join("99999999999999999999.json"), r#"{"pid":4294967290,"sessionId":"huge"}"#).unwrap();
    fs::create_dir(s.join("116.json")).unwrap();
    // A good one, with unknown fields and a wrong-typed optional field.
    fs::write(s.join(format!("{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"good","newThing":[1],"status":"busy"}}"#)).unwrap();
    let mut src = source(&d, 0);
    for _ in 0..3 {
        let ev = src.poll();
        let seen: Vec<_> = ev.iter().filter(|e| matches!(e.event, DomainEvent::SessionSeen { .. })).collect();
        assert!(seen.len() <= 1, "{ev:?}");
        if let Some(e) = seen.first() {
            assert_eq!(e.session_id, "good");
        }
    }
    // The torn file is completed later: it is picked up (this process is alive).
    fs::write(s.join("112.json"), format!(r#"{{"pid":{pid},"sessionId":"healed"}}"#)).unwrap();
    let ev = src.poll();
    assert!(ev.iter().any(|e| e.session_id == "healed"), "{ev:?}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_missing_claude_home_is_not_an_error() {
    let d = std::env::temp_dir().join(format!("colony-src-robust-missing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    let mut src = DirSource::new(HostId::Windows, &d.join("nope"), &d.join("nada"), 0);
    assert!(src.poll().is_empty());
}

// --- spool replay after a daemon outage ---

#[test]
fn a_long_outage_replays_in_order_without_loss_or_duplicates() {
    let d = home("outage");
    let spool = d.join("colony/spool");
    fs::create_dir_all(&spool).unwrap();
    // 500 payloads spooled while no daemon ran, interleaved across two sessions,
    // written out of directory order by using descending creation.
    let t0 = (now_ms() - 600_000) as u128 * 1_000_000;
    for i in (0..500u32).rev() {
        let sid = if i % 2 == 0 { "a" } else { "b" };
        let ev = if i == 0 { "SessionStart" } else { "PreToolUse" };
        let body = format!(r#"{{"session_id":"{sid}","hook_event_name":"{ev}","tool_name":"Read","tool_use_id":"t{i}"}}"#);
        fs::write(spool.join(format!("{}-{i}.json", t0 + i as u128 * 1_000_000)), body).unwrap();
    }
    fs::write(spool.join(format!("{}-x.tmp", t0)), "half").unwrap();
    let mut src = source(&d, 0);
    let ev = src.poll();
    assert_eq!(ev.len(), 500);
    assert!(ev.windows(2).all(|w| w[0].ts <= w[1].ts), "time order kept");
    let ids: Vec<_> = ev
        .iter()
        .filter_map(|e| match &e.event {
            DomainEvent::ToolStarted { tool_use_id, .. } => tool_use_id.clone(),
            _ => None,
        })
        .collect();
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 499);
    assert!(src.poll().is_empty(), "nothing is delivered twice");
    // New hook output during the same run goes through capture/ directly.
    fs::write(d.join("colony/capture").join(name(0, 900)), r#"{"session_id":"a","hook_event_name":"Stop"}"#).unwrap();
    assert_eq!(kinds(&src.poll()), ["stop"]);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn spool_is_drained_even_when_capture_does_not_exist_yet() {
    let d = std::env::temp_dir().join(format!("colony-src-robust-nocap-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("colony/spool")).unwrap();
    fs::write(d.join("colony/spool").join(name(10, 1)), r#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();
    let mut src = DirSource::new(HostId::Windows, &d.join("claude"), &d.join("colony"), 0);
    assert_eq!(kinds(&src.poll()), ["stop"]);
    let _ = fs::remove_dir_all(&d);
}

// --- daemon restart mid-session ---

#[test]
fn a_restarted_daemon_rebuilds_recent_history_and_then_only_sees_new_events() {
    let d = home("restart");
    let cap = d.join("colony/capture");
    let w = |ago: u64, n: u32, body: &str| fs::write(cap.join(name(ago, n)), body).unwrap();
    w(900_000, 1, r#"{"session_id":"s","hook_event_name":"SessionStart"}"#);
    w(50_000, 2, r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"go"}"#);
    w(40_000, 3, r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash"}"#);

    let mut first = source(&d, now_ms() - 600_000);
    assert_eq!(kinds(&first.poll()), ["prompt", "tool"], "events past the window are not replayed");
    drop(first); // the daemon dies

    // The hook keeps writing while it is down (to spool/ or capture/).
    fs::create_dir_all(d.join("colony/spool")).unwrap();
    fs::write(d.join("colony/spool").join(name(5_000, 4)), r#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();

    let mut second = source(&d, now_ms() - 600_000);
    assert_eq!(kinds(&second.poll()), ["prompt", "tool", "stop"], "history is rebuilt in order, including the outage");
    w(0, 5, r#"{"session_id":"s","hook_event_name":"SessionEnd"}"#);
    assert_eq!(kinds(&second.poll()), ["end"]);
    assert!(second.poll().is_empty());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn registry_entries_survive_a_source_restart_as_fresh_sightings() {
    let d = home("restart-reg");
    let pid = std::process::id();
    fs::write(d.join(format!("claude/sessions/{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"s1"}}"#)).unwrap();
    let mut a = source(&d, 0);
    assert_eq!(a.poll().len(), 1);
    drop(a);
    let mut b = source(&d, 0);
    let ev = b.poll();
    assert!(matches!(ev[0].event, DomainEvent::SessionSeen { .. }), "{ev:?}");
    assert!(b.poll().is_empty());
    let _ = fs::remove_dir_all(&d);
}
