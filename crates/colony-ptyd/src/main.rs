//! colony-ptyd: owns Colony's terminals, so the sessions in them survive
//! colonyd restarts and updates.
//!
//! colonyd starts it on demand and connects over localhost TCP
//! (`127.0.0.1:7879`, or `COLONY_PTYD_PORT`), proving itself with the token in
//! `<colony home>/ptyd.json`. One client at a time; a new connection replaces
//! the old one and is sent every live terminal with its scrollback. With no
//! terminals and no client for `IDLE_EXIT`, ptyd exits.
//!
//! It is deliberately small and changes rarely: restarting it does end the
//! sessions it holds.

#[cfg(windows)]
mod conpty;

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use colony_core::ptyproto::{FromPtyd, TermInfo, ToPtyd};
use colony_source::{colony_home, now_ms};

const DEFAULT_PORT: u16 = 7879;
const SCROLLBACK_BYTES: usize = 512 * 1024;
const IDLE_EXIT: Duration = Duration::from_secs(5 * 60);

fn log(msg: impl AsRef<str>) {
    eprintln!("[colony-ptyd {}] {}", now_ms(), msg.as_ref());
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

struct Term {
    info: TermInfo,
    writer: Mutex<File>,
    #[cfg(windows)]
    pty: Arc<conpty::Pty>,
    scrollback: Mutex<VecDeque<u8>>,
    kill_requested: AtomicBool,
}

#[derive(Default)]
struct State {
    terms: Mutex<HashMap<String, Arc<Term>>>,
    /// Write half of the connected colonyd, if any.
    client: Mutex<Option<TcpStream>>,
    /// Exits that happened with no client connected, delivered on connect.
    missed_exits: Mutex<Vec<FromPtyd>>,
    last_activity: Mutex<Option<Instant>>,
}

impl State {
    /// Send to colonyd. With nobody connected, output just stays in the
    /// scrollback, and exits are kept for the next client.
    fn send(&self, msg: FromPtyd) {
        let mut client = self.client.lock().unwrap();
        if let Some(c) = client.as_mut() {
            let line = serde_json::to_string(&msg).expect("messages serialize");
            if writeln!(c, "{line}").and_then(|_| c.flush()).is_ok() {
                return;
            }
            *client = None;
        }
        if matches!(msg, FromPtyd::Exited { .. }) {
            self.missed_exits.lock().unwrap().push(msg);
        }
    }

    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Some(Instant::now());
    }
}

fn main() {
    let port: u16 = std::env::var("COLONY_PTYD_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    // The port doubles as a single-instance lock.
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            log(format!("not starting: 127.0.0.1:{port} is taken ({e}); another colony-ptyd is probably running"));
            return;
        }
    };
    let token = uuid::Uuid::new_v4().simple().to_string();
    let info_path = colony_home().join("ptyd.json");
    let info = serde_json::json!({ "port": port, "token": token, "pid": std::process::id() });
    if let Err(e) = std::fs::create_dir_all(colony_home()).and_then(|_| std::fs::write(&info_path, info.to_string())) {
        log(format!("can't write {}: {e}", info_path.display()));
        return;
    }
    log(format!("listening on 127.0.0.1:{port}"));
    let state = Arc::new(State::default());
    state.touch();

    let idle = state.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(15));
        let quiet = idle.terms.lock().unwrap().is_empty() && idle.client.lock().unwrap().is_none();
        let since = idle.last_activity.lock().unwrap().map(|t| t.elapsed()).unwrap_or_default();
        if quiet && since > IDLE_EXIT {
            log("no terminals and no colonyd for a while; exiting");
            let _ = std::fs::remove_file(colony_home().join("ptyd.json"));
            std::process::exit(0);
        }
    });

    for conn in listener.incoming().flatten() {
        let state = state.clone();
        let token = token.clone();
        std::thread::spawn(move || serve(state, conn, &token));
    }
}

fn serve(state: Arc<State>, conn: TcpStream, token: &str) {
    let _ = conn.set_nodelay(true);
    let Ok(writer) = conn.try_clone() else { return };
    let mut lines = BufReader::new(conn).lines();
    // Authenticate before anything else.
    match lines.next().and_then(Result::ok).and_then(|l| serde_json::from_str::<ToPtyd>(&l).ok()) {
        Some(ToPtyd::Hello { token: t }) if t == token => {}
        _ => return,
    }
    {
        // Take over as the client, then catch the new client up.
        *state.client.lock().unwrap() = Some(writer);
        state.send(FromPtyd::Ready { version: env!("CARGO_PKG_VERSION").into() });
        let terms: Vec<Arc<Term>> = state.terms.lock().unwrap().values().cloned().collect();
        for t in terms {
            let scrollback = b64(&t.scrollback.lock().unwrap().iter().copied().collect::<Vec<u8>>());
            state.send(FromPtyd::Term { info: t.info.clone(), scrollback });
        }
        for m in std::mem::take(&mut *state.missed_exits.lock().unwrap()) {
            state.send(m);
        }
        state.send(FromPtyd::Synced);
        log(format!("colonyd connected; {} terminal(s) live", state.terms.lock().unwrap().len()));
    }
    for line in lines {
        let Ok(line) = line else { break };
        state.touch();
        match serde_json::from_str::<ToPtyd>(&line) {
            Ok(cmd) => handle(&state, cmd),
            Err(e) => log(format!("bad command ({e})")),
        }
    }
    log("colonyd disconnected; terminals keep running");
    state.touch();
}

fn handle(state: &Arc<State>, cmd: ToPtyd) {
    let term = |id: &str| state.terms.lock().unwrap().get(id).cloned();
    match cmd {
        ToPtyd::Hello { .. } => {}
        ToPtyd::Spawn { info, program, args, cwd, env, cols, rows } => spawn(state, info, &program, &args, cwd.as_deref(), &env, cols, rows),
        ToPtyd::Input { term: id, data } => {
            let Some(t) = term(&id) else { return };
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else { return };
            let mut w = t.writer.lock().unwrap();
            let _ = w.write_all(&bytes).and_then(|_| w.flush());
        }
        ToPtyd::Resize { term: id, cols, rows } => {
            #[cfg(windows)]
            if let Some(t) = term(&id) {
                let _ = t.pty.resize(cols.max(20), rows.max(5));
            }
            #[cfg(not(windows))]
            let _ = (id, cols, rows);
        }
        ToPtyd::Kill { term: id } => {
            if let Some(t) = term(&id) {
                t.kill_requested.store(true, Ordering::SeqCst);
                #[cfg(windows)]
                let _ = t.pty.kill();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn(state: &Arc<State>, mut info: TermInfo, program: &str, args: &[String], cwd: Option<&str>, env: &[(String, String)], cols: u16, rows: u16) {
    #[cfg(windows)]
    {
        let (pty, reader, writer) = match conpty::Pty::spawn(program, args, cwd, env, cols, rows) {
            Ok(p) => p,
            Err(e) => {
                state.send(FromPtyd::SpawnFailed { term: info.term, error: format!("could not start {program}: {e}") });
                return;
            }
        };
        let pty = Arc::new(pty);
        info.pid = pty.pid;
        info.started_at = now_ms();
        let id = info.term.clone();
        let t = Arc::new(Term {
            info,
            writer: Mutex::new(writer),
            pty: pty.clone(),
            scrollback: Mutex::default(),
            kill_requested: AtomicBool::new(false),
        });
        state.terms.lock().unwrap().insert(id.clone(), t.clone());
        state.send(FromPtyd::Spawned { term: id.clone(), pid: pty.pid });
        log(format!("started terminal {id} (pid {}) for session {}", pty.pid, t.info.session_id));

        let pump_state = state.clone();
        std::thread::spawn(move || pump(pump_state, t, reader));
        // When the process exits, close the console so the reader sees EOF.
        std::thread::spawn(move || {
            let status = pty.wait();
            log(format!("terminal {id} exited: {status:?}"));
            pty.close();
        });
    }
    #[cfg(not(windows))]
    {
        let _ = (program, args, cwd, env, cols, rows, &mut info);
        state.send(FromPtyd::SpawnFailed { term: info.term, error: "colony-ptyd only hosts terminals on Windows".into() });
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn pump(state: Arc<State>, t: Arc<Term>, mut reader: File) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                {
                    let mut sb = t.scrollback.lock().unwrap();
                    sb.extend(&buf[..n]);
                    let excess = sb.len().saturating_sub(SCROLLBACK_BYTES);
                    sb.drain(..excess);
                }
                state.send(FromPtyd::Output { term: t.info.term.clone(), data: b64(&buf[..n]) });
            }
        }
    }
    state.terms.lock().unwrap().remove(&t.info.term);
    state.touch();
    state.send(FromPtyd::Exited { term: t.info.term.clone(), requested: t.kill_requested.load(Ordering::SeqCst) });
}
