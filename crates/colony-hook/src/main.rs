//! `colony-hook`: register for every Claude Code hook event (see the README).
//! Always exits 0; the only thing it ever prints is a permission decision.

use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use colony_hook::{run, Ctx, STDIN_BUDGET};

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let out = catch_unwind(AssertUnwindSafe(|| {
        // Read on a thread so a stdin that never closes can't hold the session up. The payload is
        // complete as soon as it parses as one JSON value: Claude Code can write it at once yet close
        // the pipe late (its event loop is busy while a session starts), and waiting for the end of
        // the stream dropped those payloads. Not complete by the deadline: dropped.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stdin.read(&mut chunk) {
                if n == 0 || tx.send(chunk[..n].to_vec()).is_err() {
                    break;
                }
            }
            // Dropping `tx` tells the main thread the stream ended.
        });
        let deadline = Instant::now() + STDIN_BUDGET;
        let mut buf = Vec::new();
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk);
                    // Only a buffer that ends like a JSON object is worth parsing (a big payload
                    // arrives in many chunks; parsing each prefix would be quadratic).
                    let ends = buf.iter().rev().find(|b| !b.is_ascii_whitespace()) == Some(&b'}');
                    if ends && serde_json::from_slice::<serde_json::Value>(&buf).is_ok() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => return None,
            }
        }
        let input = String::from_utf8(buf).ok()?;
        run(&input, &Ctx::from_env())
    }))
    .ok()
    .flatten();
    if let Some(out) = out {
        let mut so = std::io::stdout();
        let _ = so.write_all(out.as_bytes());
        let _ = so.flush();
    }
    // exit() rather than returning, so a still-blocked reader thread can't delay us.
    std::process::exit(0);
}
