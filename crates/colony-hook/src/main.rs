//! `colony-hook`: register for every Claude Code hook event (see the README).
//! Always exits 0; the only thing it ever prints is a permission decision.

use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;

use colony_hook::{run, Ctx, STDIN_BUDGET};

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let out = catch_unwind(AssertUnwindSafe(|| {
        // Read on a thread so a stdin that never closes can't hold the session
        // up; the payload is dropped in that case.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
        let buf = rx.recv_timeout(STDIN_BUDGET).ok()?;
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
