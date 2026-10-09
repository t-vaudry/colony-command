//! Regression: stdin used to be given 40 ms, so a hook started on a busy (or
//! virus-scanning) machine silently dropped the event.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[test]
fn a_slow_writer_does_not_lose_the_payload() {
    let home = std::env::temp_dir().join(format!("colony-hook-slow-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_colony-hook"))
        .env("COLONY_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    thread::sleep(Duration::from_millis(120));
    stdin.write_all(br#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    assert_eq!(fs::read_dir(home.join("spool")).unwrap().count(), 1);
    let _ = fs::remove_dir_all(&home);
}
