//! Terminals Colony owns. Each runs one `claude` session in a pseudo-console
//! (ConPTY on Windows): on Windows directly, or in a WSL distro through
//! `wsl.exe`. Output is kept in a bounded scrollback for maps that attach
//! later and broadcast live to maps that are attached now.
//!
//! The session id is chosen here (`--session-id`) or reused (`--resume`), so
//! the terminal and the session's hook events are linked without guessing.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use colony_core::paths::to_wsl_path;
use colony_core::{DomainEvent, Envelope, HostId};
use colony_source::now_ms;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

#[cfg(windows)]
use crate::conpty::Pty;
use crate::log;

const SCROLLBACK_BYTES: usize = 512 * 1024;
/// Permission modes the map may start a session in.
const PERMISSION_MODES: &[&str] = &["default", "acceptEdits", "plan", "auto"];
/// Start of and end of a bracketed paste: the message arrives as one block,
/// newlines included, instead of each line being submitted.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnRequest {
    pub host: HostId,
    /// Folder to start in, as given by the map (Windows or Linux form).
    pub dir: String,
    #[serde(default)]
    pub prompt: Option<String>,
    /// Resume this session id instead of starting a new conversation.
    #[serde(default)]
    pub resume: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// `--permission-mode`; omitted means the user's own default.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// Claude in Chrome: `Some(false)` passes `--no-chrome` (and skips its
    /// first-run question), `Some(true)` `--chrome`, `None` neither.
    #[serde(default)]
    pub chrome: Option<bool>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

fn default_cols() -> u16 {
    120
}
fn default_rows() -> u16 {
    32
}

/// Where Colony can start sessions, for the map's New session dialog.
#[derive(Debug, Clone, Serialize)]
pub struct HostOption {
    pub id: HostId,
    pub label: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

struct Term {
    session_id: String,
    host: HostId,
    /// Set by `kill`, so the exit is reported as ended rather than crashed.
    kill_requested: AtomicBool,
    writer: Mutex<File>,
    #[cfg(windows)]
    pty: Arc<Pty>,
    scrollback: Mutex<VecDeque<u8>>,
}

/// A chunk of terminal output for maps attached to that terminal.
#[derive(Clone)]
pub struct Output {
    pub term_id: String,
    pub bytes: Arc<Vec<u8>>,
}

/// Processes Colony started in its terminals, by pid, with their terminal id.
pub type Owned = Arc<Mutex<HashMap<u32, String>>>;

pub struct PtyHost {
    terms: Mutex<HashMap<String, Arc<Term>>>,
    /// So registry entries from these processes are recognized as Colony's
    /// own sessions, and never offered for termination as "other copies".
    pub owned: Owned,
    pub output: broadcast::Sender<Output>,
    events: mpsc::Sender<Envelope>,
    windows_claude: Option<PathBuf>,
}

pub struct Spawned {
    pub term_id: String,
    pub session_id: String,
}

impl PtyHost {
    pub fn new(events: mpsc::Sender<Envelope>) -> Arc<Self> {
        let windows_claude = find_windows_claude();
        match &windows_claude {
            Some(p) => log(format!("Claude Code on Windows: {}", p.display())),
            None => log("Claude Code CLI not found on Windows; Colony can start sessions in WSL only"),
        }
        let (output, _) = broadcast::channel(1024);
        Arc::new(PtyHost { terms: Mutex::default(), owned: Owned::default(), output, events, windows_claude })
    }

    pub fn hosts(&self, distros: &[String]) -> Vec<HostOption> {
        let mut out = Vec::new();
        if cfg!(windows) {
            out.push(HostOption {
                id: HostId::Windows,
                label: "Windows".into(),
                available: self.windows_claude.is_some(),
                note: self.windows_claude.is_none().then(|| "Claude Code CLI isn't installed on Windows".into()),
            });
        }
        for d in distros {
            out.push(HostOption { id: HostId::Wsl(d.clone()), label: format!("WSL · {d}"), available: true, note: None });
        }
        out
    }

    pub fn spawn(self: &Arc<Self>, req: SpawnRequest) -> Result<Spawned, String> {
        let session_id = req.resume.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut claude_args: Vec<String> = match &req.resume {
            Some(id) => vec!["--resume".into(), id.clone()],
            None => vec!["--session-id".into(), session_id.clone()],
        };
        if let Some(n) = req.name.as_ref().filter(|n| !n.trim().is_empty()) {
            claude_args.extend(["--name".into(), n.trim().to_string()]);
        }
        if let Some(mode) = req.permission_mode.as_ref().filter(|m| !m.is_empty()) {
            // Never bypassPermissions from a button.
            if !PERMISSION_MODES.contains(&mode.as_str()) {
                return Err(format!("unknown permission mode {mode:?}"));
            }
            claude_args.extend(["--permission-mode".into(), mode.clone()]);
        }
        match req.chrome {
            Some(true) => claude_args.push("--chrome".into()),
            Some(false) => claude_args.push("--no-chrome".into()),
            None => {}
        }
        if let Some(p) = req.prompt.as_ref().filter(|p| !p.trim().is_empty()) {
            claude_args.push(p.clone());
        }

        let (program, args, cwd): (String, Vec<String>, Option<&str>) = match &req.host {
            HostId::Windows => {
                let exe = self.windows_claude.as_ref().ok_or("Claude Code CLI isn't installed on Windows")?;
                (exe.display().to_string(), claude_args, Some(req.dir.as_str()))
            }
            HostId::Wsl(distro) => {
                // A login shell puts ~/.local/bin on PATH; "$@" passes the
                // arguments through without the shell interpreting them.
                let mut a: Vec<String> = ["-d", distro, "--cd", &to_wsl_path(&req.dir), "-e", "bash", "-lc", "exec claude \"$@\"", "claude"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                a.extend(claude_args);
                ("wsl.exe".into(), a, None)
            }
        };
        let term_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        // Mark the session as Colony's: Windows recognizes it by process id;
        // inside WSL the probe reads this variable (WSLENV carries it across).
        let mut env = child_env();
        env.push(("COLONY_TERM_ID".into(), term_id.clone()));
        if matches!(req.host, HostId::Wsl(_)) {
            let wslenv = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("WSLENV")).map(|(_, v)| v.clone());
            env.retain(|(k, _)| !k.eq_ignore_ascii_case("WSLENV"));
            let joined = match wslenv.filter(|v| !v.is_empty()) {
                Some(v) => format!("{v}:COLONY_TERM_ID/u"),
                None => "COLONY_TERM_ID/u".into(),
            };
            env.push(("WSLENV".into(), joined));
        }
        let (pty, reader, writer) = spawn_pty(&program, &args, cwd, &env, req.cols.max(40), req.rows.max(10))?;
        #[cfg(windows)]
        self.owned.lock().unwrap().insert(pty.pid, term_id.clone());
        let term = Arc::new(Term {
            session_id: session_id.clone(),
            host: req.host.clone(),
            kill_requested: AtomicBool::new(false),
            writer: Mutex::new(writer),
            #[cfg(windows)]
            pty: pty.clone(),
            scrollback: Mutex::default(),
        });
        self.terms.lock().unwrap().insert(term_id.clone(), term.clone());
        let _ = self.events.try_send(Envelope {
            ts: now_ms(),
            host: req.host.clone(),
            session_id: session_id.clone(),
            cwd: None,
            event: DomainEvent::TerminalAttached { term_id: term_id.clone(), dir: req.dir.clone() },
        });
        #[cfg(windows)]
        log(format!("started session {session_id} in terminal {term_id} ({}, {}, pid {})", req.host, req.dir, pty.pid));
        #[cfg(not(windows))]
        log(format!("started session {session_id} in terminal {term_id} ({}, {})", req.host, req.dir));

        // Reader: scrollback + live broadcast until the terminal closes.
        let host = self.clone();
        let tid = term_id.clone();
        std::thread::spawn(move || host.pump(tid, term, reader));
        // Reaper: when the process exits, close the console so the reader
        // reaches end of file and the terminal is torn down.
        #[cfg(windows)]
        {
            let tid = term_id.clone();
            std::thread::spawn(move || {
                let status = pty.wait();
                log(format!("terminal {tid} exited: {status:?}"));
                pty.close();
            });
        }
        Ok(Spawned { term_id, session_id })
    }

    fn pump(self: Arc<Self>, term_id: String, term: Arc<Term>, mut reader: File) {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    {
                        let mut sb = term.scrollback.lock().unwrap();
                        sb.extend(&chunk);
                        let excess = sb.len().saturating_sub(SCROLLBACK_BYTES);
                        sb.drain(..excess);
                    }
                    let _ = self.output.send(Output { term_id: term_id.clone(), bytes: Arc::new(chunk) });
                }
            }
        }
        self.terms.lock().unwrap().remove(&term_id);
        self.owned.lock().unwrap().retain(|_, t| t != &term_id);
        let _ = self.events.blocking_send(Envelope {
            ts: now_ms(),
            host: term.host.clone(),
            session_id: term.session_id.clone(),
            cwd: None,
            event: DomainEvent::TerminalExited {
                term_id: term_id.clone(),
                requested: term.kill_requested.load(Ordering::SeqCst),
            },
        });
        let _ = self.output.send(Output { term_id, bytes: Arc::new(b"\r\n\x1b[2m[session ended]\x1b[0m\r\n".to_vec()) });
    }

    fn term(&self, id: &str) -> Result<Arc<Term>, String> {
        self.terms.lock().unwrap().get(id).cloned().ok_or_else(|| "that terminal has closed".into())
    }

    pub fn scrollback(&self, id: &str) -> Option<Vec<u8>> {
        let t = self.term(id).ok()?;
        let sb = t.scrollback.lock().unwrap();
        Some(sb.iter().copied().collect())
    }

    /// Raw keystrokes from the map's terminal pane.
    pub fn input(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        let t = self.term(id)?;
        let mut w = t.writer.lock().unwrap();
        w.write_all(bytes).and_then(|_| w.flush()).map_err(|e| e.to_string())
    }

    /// A message from the reply box, pasted as one block. The caller sends
    /// Enter a moment later, once the TUI has taken the paste.
    pub fn paste(&self, id: &str, text: &str) -> Result<(), String> {
        let mut bytes = PASTE_START.to_vec();
        bytes.extend(text.replace("\r\n", "\n").as_bytes());
        bytes.extend(PASTE_END);
        self.input(id, &bytes)
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let t = self.term(id)?;
        #[cfg(windows)]
        return t.pty.resize(cols.max(20), rows.max(5)).map_err(|e| e.to_string());
        #[cfg(not(windows))]
        {
            let _ = (t, cols, rows);
            Err(UNSUPPORTED.into())
        }
    }

    pub fn kill(&self, id: &str) -> Result<(), String> {
        let t = self.term(id)?;
        t.kill_requested.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        return t.pty.kill().map_err(|e| e.to_string());
        #[cfg(not(windows))]
        {
            let _ = t;
            Err(UNSUPPORTED.into())
        }
    }
}

#[cfg(not(windows))]
const UNSUPPORTED: &str = "Colony only hosts terminals on Windows";

#[cfg(windows)]
#[allow(clippy::type_complexity)]
fn spawn_pty(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &[(String, String)],
    cols: u16,
    rows: u16,
) -> Result<(Arc<Pty>, File, File), String> {
    let (pty, reader, writer) = Pty::spawn(program, args, cwd, env, cols, rows).map_err(|e| format!("could not start {program}: {e}"))?;
    Ok((Arc::new(pty), reader, writer))
}

#[cfg(not(windows))]
fn spawn_pty(_: &str, _: &[String], _: Option<&str>, _: &[(String, String)], _: u16, _: u16) -> Result<((), File, File), String> {
    Err(UNSUPPORTED.into())
}

/// Variables a running Claude Code session sets for its children. If the
/// daemon was started from inside a session (say, by Claude itself), passing
/// them on would make the new session think it is that session's child.
const SESSION_VAR_PREFIXES: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_",
    "CLAUDE_AGENT_SDK_",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_PREVIEW_",
    "MCP_CONNECTION_NONBLOCKING",
    "MCP_SERVER_CONNECTION_BATCH_SIZE",
    "ANTHROPIC_BASE_URL",
];

/// The daemon's environment minus inherited session variables. A variable
/// you set yourself in your Windows user or system environment is kept.
fn child_env() -> Vec<(String, String)> {
    static PERSISTENT: OnceLock<Vec<String>> = OnceLock::new();
    let persistent = PERSISTENT.get_or_init(persistent_env_names);
    std::env::vars()
        .filter(|(k, _)| {
            let upper = k.to_ascii_uppercase();
            !SESSION_VAR_PREFIXES.iter().any(|p| upper.starts_with(p)) || persistent.contains(&upper)
        })
        .collect()
}

/// Names of variables in the user's and the machine's saved environment.
fn persistent_env_names() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let keys = [r"HKCU\Environment", r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment"];
    let mut names = Vec::new();
    for key in keys {
        let Ok(out) = std::process::Command::new("reg.exe").args(["query", key]).output() else { continue };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            // "    NAME    REG_SZ    value"
            let mut parts = line.split("    ").filter(|s| !s.is_empty());
            if let (Some(name), Some(kind)) = (parts.next(), parts.next()) {
                if kind.starts_with("REG_") && line.starts_with("    ") {
                    names.push(name.trim().to_ascii_uppercase());
                }
            }
        }
    }
    names
}

/// Claude Code's CLI on Windows: on PATH, or where the native installer puts it.
fn find_windows_claude() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("claude.exe")).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("USERPROFILE") {
        candidates.push(PathBuf::from(home).join(".local").join("bin").join("claude.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_env_drops_inherited_session_markers() {
        std::env::set_var("CLAUDE_CODE_CHILD_SESSION", "1");
        std::env::set_var("COLONY_TEST_KEEP", "yes");
        let env = child_env();
        assert!(!env.iter().any(|(k, _)| k == "CLAUDE_CODE_CHILD_SESSION"));
        assert!(env.iter().any(|(k, v)| k == "COLONY_TEST_KEEP" && v == "yes"));
        assert!(env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH")));
    }
}
