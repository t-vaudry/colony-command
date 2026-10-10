//! Contract tests: the hook payloads colonyd consumes, as captured from real
//! Claude Code releases (tests/fixtures/<version>/<Event>.json), must keep
//! decoding into domain events, and the hook must record them untouched.
//!
//! To add a release: run it with colony-hook registered, copy
//! `~/.colony/fixtures/<version>/` here, and scrub paths, prompts and ids.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use colony_core::{DomainEvent, Envelope, HookPayload, HostId};

/// Events the hook is registered for.
const EVENTS: &[&str] = &[
    "SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse",
    "PostToolUseFailure", "Notification", "SubagentStart", "SubagentStop", "Stop", "StopFailure", "PreCompact",
    "PostCompact",
];

fn fixtures() -> Vec<(String, String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out = Vec::new();
    for version in fs::read_dir(&root).unwrap().flatten() {
        for f in fs::read_dir(version.path()).unwrap().flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let event = name.strip_suffix(".json").expect("fixtures are .json").to_string();
            out.push((version.file_name().to_string_lossy().into_owned(), event, fs::read_to_string(f.path()).unwrap()));
        }
    }
    assert!(!out.is_empty(), "no fixtures found");
    out
}

#[test]
fn every_fixture_decodes_into_a_domain_event() {
    for (version, event, text) in fixtures() {
        let at = format!("{version}/{event}");
        let p = HookPayload::parse(&text).unwrap_or_else(|e| panic!("{at}: {e}"));
        assert_eq!(p.hook_event_name, event, "{at}: file name and event disagree");
        let env = Envelope::from_hook(HostId::Windows, 1, &p).unwrap_or_else(|| panic!("{at}: no domain event"));
        assert_eq!(env.session_id, p.session_id, "{at}");
    }
}

#[test]
fn every_registered_event_has_a_fixture_in_each_version() {
    let all = fixtures();
    let mut versions: Vec<&str> = all.iter().map(|(v, _, _)| v.as_str()).collect();
    versions.dedup();
    for v in versions {
        for e in EVENTS {
            assert!(all.iter().any(|(fv, fe, _)| fv == v && fe == e), "{v} is missing a {e} fixture");
        }
    }
}

#[test]
fn fixtures_carry_the_fields_colony_relies_on() {
    for (version, event, text) in fixtures() {
        let p = HookPayload::parse(&text).unwrap();
        let at = format!("{version}/{event}");
        match event.as_str() {
            "PreToolUse" | "PermissionRequest" => {
                assert!(p.tool_name.is_some() && p.tool_target().is_some(), "{at}: tool and target");
            }
            "PostToolUse" => assert!(p.tool_use_id.is_some(), "{at}: tool_use_id"),
            "PostToolUseFailure" => assert!(p.tool_use_id.is_some() && p.error_text().is_some(), "{at}: tool_use_id and error"),
            "SubagentStart" | "SubagentStop" => assert!(p.agent_id.is_some(), "{at}: agent_id"),
            "SessionStart" => assert!(p.source.is_some(), "{at}: source"),
            "Notification" => assert!(p.notification_type.is_some(), "{at}: notification_type"),
            _ => {}
        }
    }
}

// --- the hook binary ---

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("colony-hook-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Runs the hook with `input` on stdin in an isolated colony home.
fn hook(home: &Path, input: &str) -> (String, Duration) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_colony-hook"))
        .env("COLONY_HOME", home)
        .env("COLONY_CLAUDE_VERSION", "9.9.9")
        .env_remove("CLAUDE_PID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let took = start.elapsed();
    assert!(out.status.success(), "hook must always exit 0");
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    (String::from_utf8(out.stdout).unwrap(), took)
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

/// Stands in for colonyd: accepts connections, optionally answers HTTP.
fn fake_daemon(home: &Path, reply: Option<&'static str>) -> thread::JoinHandle<String> {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    fs::write(home.join("daemon.json"), format!(r#"{{"pid":1,"port":{port},"token":"abc123"}}"#)).unwrap();
    thread::spawn(move || {
        let mut seen = String::new();
        // The hook probes first, then (for permission requests) posts.
        l.set_nonblocking(false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
            let mut buf = vec![0u8; 65536];
            let n = s.read(&mut buf).unwrap_or(0);
            if n == 0 {
                if reply.is_none() {
                    break;
                }
                continue;
            }
            seen = String::from_utf8_lossy(&buf[..n]).into_owned();
            if let Some(body) = reply {
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
            break;
        }
        seen
    })
}

#[test]
fn without_a_daemon_payloads_are_spooled_byte_for_byte() {
    for (version, event, text) in fixtures() {
        let home = tmp("spool");
        let (out, _) = hook(&home, &text);
        assert_eq!(out, "", "{version}/{event}: the hook prints nothing");
        let spooled = files(&home.join("spool"));
        assert_eq!(spooled.len(), 1, "{version}/{event}");
        assert_eq!(fs::read_to_string(&spooled[0]).unwrap(), text);
        assert!(files(&home.join("capture")).is_empty());
        let _ = fs::remove_dir_all(&home);
    }
}

#[test]
fn with_a_daemon_payloads_go_straight_to_capture() {
    let home = tmp("capture");
    let daemon = fake_daemon(&home, None);
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/2.1.295/Stop.json")).unwrap();
    hook(&home, &text);
    let cap = files(&home.join("capture"));
    assert_eq!(cap.len(), 1);
    assert_eq!(fs::read_to_string(&cap[0]).unwrap(), text);
    assert!(files(&home.join("spool")).is_empty());
    let _ = daemon.join();
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_stale_daemon_file_means_spool() {
    let home = tmp("stale");
    // A port nothing listens on: bind, note it, release it.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    fs::write(home.join("daemon.json"), format!(r#"{{"pid":1,"port":{port},"token":"abc123"}}"#)).unwrap();
    hook(&home, r#"{"session_id":"s","hook_event_name":"Stop"}"#);
    assert_eq!(files(&home.join("spool")).len(), 1);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn the_first_payload_per_event_is_kept_as_a_fixture_for_the_version() {
    let home = tmp("fixture");
    hook(&home, r#"{"session_id":"s","hook_event_name":"Stop","last_assistant_message":"first"}"#);
    hook(&home, r#"{"session_id":"s","hook_event_name":"Stop","last_assistant_message":"second"}"#);
    let kept = home.join("fixtures/9.9.9/Stop.json");
    assert!(fs::read_to_string(kept).unwrap().contains("first"));
    assert_eq!(files(&home.join("fixtures/9.9.9")).len(), 1);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn permission_requests_print_colonyds_decision() {
    let home = tmp("gate");
    let decision = r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#;
    let daemon = fake_daemon(&home, Some(decision));
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/2.1.295/PermissionRequest.json")).unwrap();
    let (out, _) = hook(&home, &text);
    assert_eq!(out, decision);
    let seen = daemon.join().unwrap();
    assert!(seen.starts_with("POST /api/permission?token=abc123 "), "{seen}");
    assert!(seen.contains("rm -rf target"));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn no_daemon_means_no_decision() {
    let home = tmp("nogate");
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/2.1.295/PermissionRequest.json")).unwrap();
    let (out, _) = hook(&home, &text);
    assert_eq!(out, "");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn bad_input_fails_open() {
    for input in ["", "not json", "{", "[1,2]", "{\"hook_event_name\":42}", "\u{0}\u{1}"] {
        let home = tmp("bad");
        let (out, _) = hook(&home, input);
        assert_eq!(out, "", "{input:?}");
        let _ = fs::remove_dir_all(&home);
    }
}

#[test]
fn an_unwritable_home_fails_open() {
    let home = tmp("ro");
    // COLONY_HOME is a file, so no directory can be made under it.
    let file = home.join("not-a-dir");
    fs::write(&file, "x").unwrap();
    let (out, _) = hook(&file, r#"{"session_id":"s","hook_event_name":"Stop"}"#);
    assert_eq!(out, "");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn it_returns_quickly_when_not_gating() {
    let home = tmp("fast");
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/2.1.295/PostToolUse.json")).unwrap();
    hook(&home, &text); // warm the disk cache and create directories
    let best = (0..5).map(|_| hook(&home, &text).1).min().unwrap();
    // The budget is 50 ms; process start-up on a loaded CI box varies, so the
    // test allows slack but still catches anything that waits or retries.
    assert!(best < Duration::from_millis(250), "took {best:?}");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_stdin_that_never_closes_does_not_hold_the_session() {
    let home = tmp("hang");
    let mut child = Command::new(env!("CARGO_BIN_EXE_colony-hook"))
        .env("COLONY_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = child.wait().unwrap(); // stdin stays open
    assert!(status.success());
    assert!(start.elapsed() < Duration::from_secs(2));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_payload_that_arrives_late_is_still_recorded() {
    // Claude Code can be slow to write the payload on a busy machine; it must not be dropped.
    let home = tmp("late");
    let mut child = Command::new(env!("CARGO_BIN_EXE_colony-hook"))
        .env("COLONY_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    thread::sleep(Duration::from_millis(300));
    stdin.write_all(br#"{"session_id":"late","hook_event_name":"UserPromptSubmit"}"#).unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    assert_eq!(files(&home.join("spool")).len(), 1, "the late payload was dropped");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_complete_payload_is_recorded_even_if_stdin_stays_open() {
    // Claude Code can write the payload at once and close the pipe late.
    let home = tmp("open");
    let mut child = Command::new(env!("CARGO_BIN_EXE_colony-hook"))
        .env("COLONY_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(br#"{"session_id":"open","hook_event_name":"SessionStart"}"#).unwrap();
    stdin.flush().unwrap();
    let start = Instant::now();
    assert!(child.wait().unwrap().success()); // stdin is still open here
    assert!(start.elapsed() < Duration::from_millis(900), "waited for the end of stdin");
    assert_eq!(files(&home.join("spool")).len(), 1, "the payload was dropped");
    drop(stdin);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn spooled_files_become_events_once_a_source_starts() {
    let home = tmp("drain");
    hook(&home, r#"{"session_id":"s","hook_event_name":"Stop","last_assistant_message":"done"}"#);
    let mut src = colony_source::DirSource::new(HostId::Windows, &home.join("claude"), &home, 0);
    let ev = src.poll();
    assert!(ev.iter().any(|e| matches!(e.event, DomainEvent::TurnEnded { .. })), "{ev:?}");
    assert!(files(&home.join("spool")).is_empty());
    let _ = fs::remove_dir_all(&home);
}
