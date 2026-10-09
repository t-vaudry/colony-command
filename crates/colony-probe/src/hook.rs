//! Carries `colony-approve.sh` permission requests from this distro to colonyd.
//!
//! The hook can't reach colonyd's HTTP port reliably from WSL, and colonyd's
//! token lives on the Windows side. So the probe listens on a Unix socket in
//! `~/.colony` (mode 0600, so only this user can use it) and relays each
//! request over its own stdio channel, which colonyd already owns. The token
//! never enters the distro.
//!
//! Wire format on the stdio channel, one JSON object per line:
//!   probe -> colonyd  `{"permission":{"id":N,"body":"<hook stdin>"}}`
//!                     `{"permission_cancel":N}` (the hook went away)
//!   colonyd -> probe  `{"id":N,"output":"<hook stdout>"|null}`
//!
//! The socket speaks just enough HTTP for `curl --unix-socket`: the response
//! is 200 with colonyd's output, or 204 for "no decision". Any trouble also
//! ends as no decision, so the hook fails open.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_HEAD: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const CHECK: Duration = Duration::from_millis(300);

pub fn socket_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".colony").join("hook.sock"))
}

/// The probe's single writer to colonyd, plus the hooks waiting on answers.
pub struct Bridge {
    out: Mutex<Box<dyn Write + Send>>,
    waiting: Mutex<HashMap<u64, Sender<Option<String>>>>,
    next: Mutex<u64>,
}

impl Bridge {
    pub fn new(out: Box<dyn Write + Send>) -> Arc<Bridge> {
        Arc::new(Bridge { out: Mutex::new(out), waiting: Mutex::default(), next: Mutex::new(1) })
    }

    /// Writes one line and flushes. An error means colonyd is gone.
    pub fn send_line(&self, line: &str) -> io::Result<()> {
        let mut out = self.out.lock().unwrap();
        writeln!(out, "{line}")?;
        out.flush()
    }

    /// Feeds one line from colonyd (a permission answer) to its waiting hook.
    pub fn handle_reply(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
        let Some(id) = v.get("id").and_then(|i| i.as_u64()) else { return };
        let output = v.get("output").and_then(|o| o.as_str()).map(String::from);
        if let Some(tx) = self.waiting.lock().unwrap().remove(&id) {
            let _ = tx.send(output);
        }
    }

    /// Ends every waiting hook with no decision (colonyd went away).
    pub fn abandon_all(&self) {
        self.waiting.lock().unwrap().clear();
    }

    /// Relays one hook payload and waits for the answer. `gone` reports that
    /// the hook disconnected, which cancels the request on colonyd's side.
    fn relay(&self, body: String, gone: impl Fn() -> bool) -> Option<String> {
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        let (tx, rx) = channel();
        self.waiting.lock().unwrap().insert(id, tx);
        let msg = serde_json::json!({ "permission": { "id": id, "body": body } });
        if self.send_line(&msg.to_string()).is_err() {
            self.waiting.lock().unwrap().remove(&id);
            return None;
        }
        loop {
            match rx.recv_timeout(CHECK) {
                Ok(out) => return out,
                // abandon_all dropped our sender.
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) if gone() => {
                    self.waiting.lock().unwrap().remove(&id);
                    let _ = self.send_line(&serde_json::json!({ "permission_cancel": id }).to_string());
                    return None;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

/// Starts listening on `path`. A leftover socket from a dead probe is replaced.
pub fn serve(path: &Path, bridge: Arc<Bridge>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let bridge = bridge.clone();
            thread::spawn(move || {
                let _ = handle(stream, &bridge);
            });
        }
    });
    Ok(())
}

fn handle(mut stream: UnixStream, bridge: &Bridge) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let body = read_request(&mut stream)?;
    stream.set_read_timeout(None)?;
    let probe = stream.try_clone()?;
    probe.set_nonblocking(true)?;
    // The request is fully read, so any readable event now is the hook hanging up.
    let gone = move || {
        let mut one = [0u8; 1];
        !matches!((&probe).read(&mut one), Err(e) if e.kind() == io::ErrorKind::WouldBlock)
    };
    match bridge.relay(body, gone) {
        Some(out) => write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
            out.len()
        ),
        None => write!(stream, "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"),
    }
}

/// Reads an HTTP request and returns its body.
fn read_request(stream: &mut UnixStream) -> io::Result<String> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEAD {
            return Err(bad("headers too long"));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(bad("closed before the request was complete"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .ok_or_else(|| bad("no content-length"))?;
    if len > MAX_BODY {
        return Err(bad("body too large"));
    }
    while buf.len() < head_end + len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(bad("closed before the body was complete"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8(buf[head_end..head_end + len].to_vec()).map_err(|_| bad("body isn't UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Sink {
        fn lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap().lines().map(String::from).collect()
        }
        fn wait_for_lines(&self, n: usize) -> Vec<String> {
            let start = Instant::now();
            while self.lines().len() < n {
                assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {n} lines");
                thread::sleep(Duration::from_millis(10));
            }
            self.lines()
        }
    }

    fn setup() -> (PathBuf, Sink, Arc<Bridge>) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("colony-hook-test-{}-{}.sock", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let sink = Sink::default();
        let bridge = Bridge::new(Box::new(sink.clone()));
        serve(&path, bridge.clone()).unwrap();
        (path, sink, bridge)
    }

    fn post(path: &Path, body: &str) -> UnixStream {
        let mut s = UnixStream::connect(path).unwrap();
        write!(s, "POST /permission HTTP/1.1\r\nHost: probe\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        s
    }

    fn read_all(mut s: UnixStream) -> String {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn relays_a_request_and_its_answer() {
        let (path, sink, bridge) = setup();
        let body = r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#;
        let client = post(&path, body);
        let sent: serde_json::Value = serde_json::from_str(&sink.wait_for_lines(1)[0]).unwrap();
        assert_eq!(sent["permission"]["body"], body);
        let id = sent["permission"]["id"].as_u64().unwrap();
        bridge.handle_reply(&serde_json::json!({ "id": id, "output": "{\"ok\":true}" }).to_string());
        let resp = read_all(client);
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(resp.ends_with("{\"ok\":true}"), "{resp}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn no_decision_is_204() {
        let (path, sink, bridge) = setup();
        let client = post(&path, "{}");
        let sent: serde_json::Value = serde_json::from_str(&sink.wait_for_lines(1)[0]).unwrap();
        let id = sent["permission"]["id"].as_u64().unwrap();
        bridge.handle_reply(&serde_json::json!({ "id": id, "output": null }).to_string());
        assert!(read_all(client).starts_with("HTTP/1.1 204"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn abandoning_fails_open() {
        let (path, sink, bridge) = setup();
        let client = post(&path, "{}");
        sink.wait_for_lines(1);
        bridge.abandon_all();
        assert!(read_all(client).starts_with("HTTP/1.1 204"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_hook_that_hangs_up_cancels_its_request() {
        let (path, sink, _bridge) = setup();
        let client = post(&path, "{}");
        let id = serde_json::from_str::<serde_json::Value>(&sink.wait_for_lines(1)[0]).unwrap()["permission"]["id"].as_u64().unwrap();
        drop(client);
        let lines = sink.wait_for_lines(2);
        let cancel: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(cancel["permission_cancel"].as_u64(), Some(id));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_broken_request_gets_no_relay() {
        let (path, sink, _bridge) = setup();
        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(b"POST / HTTP/1.1\r\n\r\n").unwrap(); // no content-length
        drop(s);
        thread::sleep(Duration::from_millis(200));
        assert!(sink.lines().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn socket_is_private() {
        let (path, _sink, _bridge) = setup();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
        let _ = std::fs::remove_file(&path);
    }
}
