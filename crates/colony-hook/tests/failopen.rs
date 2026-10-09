//! The gate hook must never get in a session's way: every way colonyd can
//! misbehave ends with exit 0 and no output (Claude Code reads that as "no
//! opinion" and asks the user as usual), and a hook that is not gating returns
//! at once even if the daemon is hung.

use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const REQUEST: &str = r#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf target"}}"#;
const ALLOW: &str = r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#;

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("colony-hook-failopen-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn hook(home: &Path, input: &[u8]) -> (String, Duration) {
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
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(input);
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let took = start.elapsed();
    assert!(out.status.success(), "must exit 0, got {:?}", out.status);
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    (String::from_utf8_lossy(&out.stdout).into_owned(), took)
}

/// A fake colonyd: how it treats the request that follows the hook's probe.
#[derive(Clone, Copy)]
enum Mode {
    /// Reads the request, then closes without answering.
    CloseAfterRequest,
    /// Closes as soon as it accepts (an RST-ish drop).
    CloseAtOnce,
    /// Answers with this raw response.
    Raw(&'static str),
    /// Reads the request and sends half of a response, then closes.
    Truncated,
    /// Accepts and says nothing, ever (until the test ends).
    Silent,
}

fn fake_daemon(home: &Path, mode: Mode) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    fs::write(home.join("daemon.json"), format!(r#"{{"pid":1,"port":{port},"token":"abc123"}}"#)).unwrap();
    thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let mut conn = conn;
            thread::spawn(move || {
                if matches!(mode, Mode::CloseAtOnce) {
                    let _ = conn.shutdown(Shutdown::Both);
                    return;
                }
                conn.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
                let mut buf = vec![0u8; 65536];
                let n = conn.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return; // the hook's connectivity probe
                }
                match mode {
                    Mode::Raw(r) => {
                        let _ = conn.write_all(r.as_bytes());
                    }
                    Mode::Truncated => {
                        let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\n{\"hookSpec");
                    }
                    Mode::Silent => thread::sleep(Duration::from_secs(30)),
                    Mode::CloseAfterRequest | Mode::CloseAtOnce => {}
                }
            });
        }
    });
}

fn captured(home: &Path) -> usize {
    fs::read_dir(home.join("capture")).map(|d| d.count()).unwrap_or(0)
}

#[test]
fn every_daemon_failure_ends_with_no_output() {
    let cases: &[(&str, Mode)] = &[
        ("close after request", Mode::CloseAfterRequest),
        ("close at once", Mode::CloseAtOnce),
        ("500", Mode::Raw("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\noops")),
        ("401", Mode::Raw("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")),
        ("204 no content", Mode::Raw("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")),
        ("200 empty", Mode::Raw("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")),
        ("200 html", Mode::Raw("HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<html>no</html>")),
        ("200 json but not an object", Mode::Raw("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntrue")),
        ("200 truncated json", Mode::Raw("HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{\"hookSpe")),
        ("200 binary", Mode::Raw("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n\u{0}\u{1}\u{2}")),
        ("not http", Mode::Raw("garbage")),
        ("no header terminator", Mode::Raw("HTTP/1.1 200 OK")),
        ("truncated response", Mode::Truncated),
    ];
    for (what, mode) in cases {
        let home = tmp("fail");
        fake_daemon(&home, *mode);
        let (out, took) = hook(&home, REQUEST.as_bytes());
        assert_eq!(out, "", "{what}: must print nothing");
        assert!(took < Duration::from_secs(3), "{what}: took {took:?}");
        assert_eq!(captured(&home), 1, "{what}: the request is still recorded");
        let _ = fs::remove_dir_all(&home);
    }
}

#[test]
fn a_good_decision_is_still_relayed() {
    let home = tmp("good");
    // Content-Length shorter than reality is tolerated; Connection: close frames it.
    fake_daemon(&home, Mode::Raw(Box::leak(format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{ALLOW}\n").into_boxed_str())));
    let (out, _) = hook(&home, REQUEST.as_bytes());
    assert_eq!(out, ALLOW);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_damaged_daemon_file_means_spool_and_no_output() {
    let bad: &[&str] = &[
        "",
        "{",
        "null",
        "[]",
        r#"{"port":"7878","token":"abc"}"#,
        r#"{"port":70000,"token":"abc"}"#,
        r#"{"port":-1,"token":"abc"}"#,
        r#"{"port":7878}"#,
        r#"{"port":7878,"token":""}"#,
        r#"{"port":7878,"token":"a b&c=d"}"#,
        r#"{"port":0,"token":"abc"}"#,
    ];
    for content in bad {
        let home = tmp("daemonfile");
        fs::write(home.join("daemon.json"), content).unwrap();
        let (out, took) = hook(&home, REQUEST.as_bytes());
        assert_eq!(out, "", "{content:?}");
        assert!(took < Duration::from_secs(2), "{content:?}: {took:?}");
        assert_eq!(fs::read_dir(home.join("spool")).unwrap().count(), 1, "{content:?}");
        let _ = fs::remove_dir_all(&home);
    }
    // daemon.json that is a directory, or non-UTF-8.
    let home = tmp("daemonfile-dir");
    fs::create_dir(home.join("daemon.json")).unwrap();
    assert_eq!(hook(&home, REQUEST.as_bytes()).0, "");
    let home = tmp("daemonfile-bin");
    fs::write(home.join("daemon.json"), [0xff, 0xfe, 0xfd]).unwrap();
    assert_eq!(hook(&home, REQUEST.as_bytes()).0, "");
}

#[test]
fn a_hung_daemon_does_not_slow_events_that_are_not_gated() {
    let home = tmp("hung");
    fake_daemon(&home, Mode::Silent);
    for event in ["PreToolUse", "PostToolUse", "Stop", "Notification", "UserPromptSubmit", "SessionStart", "SomeFutureEvent"] {
        let body = format!(r#"{{"session_id":"s","hook_event_name":"{event}","tool_name":"Bash"}}"#);
        let (out, took) = hook(&home, body.as_bytes());
        assert_eq!(out, "");
        assert!(took < Duration::from_millis(2500), "{event} took {took:?}");
    }
    assert_eq!(captured(&home), 7);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_paused_gate_returns_fast_even_with_a_hung_daemon() {
    let home = tmp("paused");
    fake_daemon(&home, Mode::Silent);
    fs::write(colony_source::gating::marker_path(&home), b"").unwrap();
    let (out, took) = hook(&home, REQUEST.as_bytes());
    assert_eq!(out, "");
    assert!(took < Duration::from_millis(2500), "{took:?}");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn hostile_payloads_cannot_escape_the_colony_folder_or_break_the_hook() {
    let home = tmp("hostile");
    let mut cases: Vec<Vec<u8>> = vec![
        br#"{"session_id":"s","hook_event_name":"../../escape"}"#.to_vec(),
        br#"{"session_id":"s","hook_event_name":"a/b"}"#.to_vec(),
        br#"{"session_id":"s","hook_event_name":"a\\b"}"#.to_vec(),
        br#"{"session_id":"s","hook_event_name":"CON"}"#.to_vec(),
        br#"{"session_id":"s","hook_event_name":""}"#.to_vec(),
        format!(r#"{{"session_id":"s","hook_event_name":"{}"}}"#, "E".repeat(5000)).into_bytes(),
        vec![0xff; 100],
    ];
    // A 2 MB payload is recorded whole.
    cases.push(format!(r#"{{"session_id":"s","hook_event_name":"Stop","last_assistant_message":"{}"}}"#, "x".repeat(2_000_000)).into_bytes());
    for c in &cases {
        let (out, took) = hook(&home, c);
        assert_eq!(out, "");
        assert!(took < Duration::from_secs(2), "{took:?}");
    }
    let parent = home.parent().unwrap();
    assert!(!parent.join("escape.json").exists());
    assert!(!home.join("fixtures/9.9.9").join("..").join("..").join("escape.json").exists());
    // Nothing but captures/spool/fixtures appeared under the home.
    for e in fs::read_dir(&home).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        assert!(["spool", "fixtures"].contains(&n.as_str()), "unexpected {n}");
    }
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn many_hooks_at_once_lose_nothing() {
    let home = tmp("burst");
    let handles: Vec<_> = (0..24)
        .map(|i| {
            let home = home.clone();
            thread::spawn(move || {
                let body = format!(r#"{{"session_id":"s{i}","hook_event_name":"PostToolUse","tool_use_id":"t{i}"}}"#);
                hook(&home, body.as_bytes());
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let spooled = fs::read_dir(home.join("spool")).unwrap().count();
    assert_eq!(spooled, 24, "each concurrent hook gets its own file");
    let _ = fs::remove_dir_all(&home);
}
