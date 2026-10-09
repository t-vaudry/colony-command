//! Terminals Colony owns, through colony-ptyd.
//!
//! The terminals themselves live in colony-ptyd, a separate process, so the
//! sessions in them keep running when colonyd restarts or is updated. This
//! module is colonyd's client: it starts ptyd when needed, keeps a mirror of
//! each terminal's scrollback for maps that attach, rebroadcasts live output,
//! and turns ptyd's notices into TerminalAttached / TerminalExited events. On
//! reconnecting it relinks every live terminal to its bot.
//!
//! The session id is chosen here (`--session-id`) or reused (`--resume`), so
//! the terminal and the session's hook events are linked without guessing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use colony_core::paths::to_wsl_path;
use colony_core::ptyproto::{FromPtyd, TermInfo, ToPtyd};
use colony_core::{DomainEvent, Envelope, HostId};
use colony_source::{colony_home, now_ms};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::log;

const SCROLLBACK_BYTES: usize = 512 * 1024;
/// Permission modes the map may start a session in.
const PERMISSION_MODES: &[&str] = &["default", "acceptEdits", "plan", "auto"];
/// Start of and end of a bracketed paste: the message arrives as one block,
/// newlines included, instead of each line being submitted.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const NOT_CONNECTED: &str = "Colony's terminal host isn't running; try again in a moment";

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
    /// For a new conversation: use this session id instead of a fresh one.
    #[serde(default)]
    pub session_id: Option<String>,
    /// `--model`: an alias ("opus", "sonnet", "haiku") or a full model id.
    #[serde(default)]
    pub model: Option<String>,
    /// Start a new conversation in its own git worktree and branch, so bots
    /// on one repository don't share a checkout. Handled before `spawn`.
    #[serde(default)]
    pub isolate: bool,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

pub fn default_cols() -> u16 {
    120
}
pub fn default_rows() -> u16 {
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

/// A chunk of terminal output for maps attached to that terminal.
#[derive(Clone)]
pub struct Output {
    pub term_id: String,
    pub bytes: Arc<Vec<u8>>,
}

/// Processes Colony started in its terminals, by pid, with their terminal id.
pub type Owned = Arc<Mutex<HashMap<u32, String>>>;

/// colonyd's copy of one terminal in ptyd.
struct Mirror {
    info: TermInfo,
    scrollback: VecDeque<u8>,
    /// Set by `kill`, so the exit is reported as ended rather than crashed.
    kill_requested: bool,
}

type SpawnReply = oneshot::Sender<Result<u32, String>>;

pub struct PtyHost {
    terms: Mutex<HashMap<String, Mirror>>,
    /// So registry entries from these processes are recognized as Colony's
    /// own sessions, and never offered for termination as "other copies".
    pub owned: Owned,
    pub output: broadcast::Sender<Output>,
    events: mpsc::Sender<Envelope>,
    windows_claude: Option<PathBuf>,
    /// Write half of the connection to ptyd.
    conn: Mutex<Option<TcpStream>>,
    /// Spawns waiting for ptyd's answer, with the terminal they'll become.
    pending: Mutex<HashMap<String, (TermInfo, SpawnReply)>>,
    /// Terminals not yet confirmed by ptyd after reconnecting.
    unconfirmed: Mutex<HashSet<String>>,
    /// How each terminal was started, to restart it the same way (on another
    /// model). Not kept across colonyd restarts.
    requests: Mutex<HashMap<String, SpawnRequest>>,
}

/// A command to run in a terminal with no bot in it, spelled for each shell.
pub struct UtilityCommand {
    pub what: String,
    pub powershell: String,
    pub bash: String,
}

pub struct Spawned {
    pub term_id: String,
    pub session_id: String,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

impl PtyHost {
    pub fn new(events: mpsc::Sender<Envelope>) -> Arc<Self> {
        let windows_claude = find_windows_claude();
        match &windows_claude {
            Some(p) => log(format!("Claude Code on Windows: {}", p.display())),
            None => log("Claude Code CLI not found on Windows; Colony can start sessions in WSL only"),
        }
        let (output, _) = broadcast::channel(1024);
        let host = Arc::new(PtyHost {
            terms: Mutex::default(),
            owned: Owned::default(),
            output,
            events,
            windows_claude,
            conn: Mutex::default(),
            pending: Mutex::default(),
            unconfirmed: Mutex::default(),
            requests: Mutex::default(),
        });
        let runner = host.clone();
        std::thread::spawn(move || runner.run());
        host
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

    pub async fn spawn(&self, req: SpawnRequest) -> Result<Spawned, String> {
        let session_id = req.resume.clone().or_else(|| req.session_id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
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
        if let Some(m) = req.model.as_ref().filter(|m| !m.is_empty()) {
            claude_args.extend(["--model".into(), checked_model(m)?.into()]);
        }
        match req.chrome {
            Some(true) => claude_args.push("--chrome".into()),
            Some(false) => claude_args.push("--no-chrome".into()),
            None => {}
        }
        if let Some(p) = req.prompt.as_ref().filter(|p| !p.trim().is_empty()) {
            claude_args.push(p.clone());
        }

        let (program, args, cwd): (String, Vec<String>, Option<String>) = match &req.host {
            HostId::Windows => {
                let exe = self.windows_claude.as_ref().ok_or("Claude Code CLI isn't installed on Windows")?;
                (exe.display().to_string(), claude_args, Some(req.dir.clone()))
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

        let (term_id, pid) = self.launch(&session_id, &req.host, &req.dir, program, args, cwd, req.cols, req.rows).await?;
        log(format!("started session {session_id} in terminal {term_id} ({}, {}, pid {pid})", req.host, req.dir));
        self.requests.lock().unwrap().insert(term_id.clone(), req);
        Ok(Spawned { term_id, session_id })
    }

    /// Start a plain command (not a Claude session) in a terminal the map can
    /// show, such as a login. It has no bot: no session id, no events.
    pub async fn spawn_utility(&self, host: &HostId, dir: &str, cols: u16, rows: u16, cmd: &UtilityCommand) -> Result<String, String> {
        let (program, args, cwd): (String, Vec<String>, Option<String>) = match host {
            HostId::Windows => (
                "powershell.exe".into(),
                vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into(), cmd.powershell.clone()],
                Some(dir.to_string()),
            ),
            HostId::Wsl(distro) => (
                "wsl.exe".into(),
                ["-d", distro, "--cd", &to_wsl_path(dir), "-e", "bash", "-lc", &cmd.bash, "bash"].iter().map(|s| s.to_string()).collect(),
                None,
            ),
        };
        let (term_id, pid) = self.launch("", host, dir, program, args, cwd, cols, rows).await?;
        log(format!("started {} in terminal {term_id} ({host}, {dir}, pid {pid})", cmd.what));
        Ok(term_id)
    }

    /// Ask ptyd to start a process and wait for it. `session_id` is empty for
    /// a terminal that isn't a bot's.
    #[allow(clippy::too_many_arguments)]
    async fn launch(&self, session_id: &str, host: &HostId, dir: &str, program: String, args: Vec<String>, cwd: Option<String>, cols: u16, rows: u16) -> Result<(String, u32), String> {
        let term_id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        // Mark the session as Colony's: Windows recognizes it by process id;
        // inside WSL the probe reads this variable (WSLENV carries it across).
        let mut env = child_env();
        env.push(("COLONY_TERM_ID".into(), term_id.clone()));
        if matches!(host, HostId::Wsl(_)) {
            let wslenv = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("WSLENV")).map(|(_, v)| v.clone());
            env.retain(|(k, _)| !k.eq_ignore_ascii_case("WSLENV"));
            let joined = match wslenv.filter(|v| !v.is_empty()) {
                Some(v) => format!("{v}:COLONY_TERM_ID/u"),
                None => "COLONY_TERM_ID/u".into(),
            };
            env.push(("WSLENV".into(), joined));
        }

        // Right after colonyd starts, ptyd may still be starting up.
        for _ in 0..100 {
            if self.conn.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let info = TermInfo { term: term_id.clone(), session_id: session_id.to_string(), host: host.clone(), dir: dir.to_string(), pid: 0, started_at: now_ms() };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(term_id.clone(), (info.clone(), tx));
        let sent = self.send(&ToPtyd::Spawn { info, program, args, cwd, env, cols: cols.max(40), rows: rows.max(10) });
        if let Err(e) = sent {
            self.pending.lock().unwrap().remove(&term_id);
            return Err(e);
        }
        let pid = match tokio::time::timeout(Duration::from_secs(20), rx).await {
            Ok(Ok(r)) => r?,
            _ => {
                self.pending.lock().unwrap().remove(&term_id);
                return Err("the terminal host didn't answer".into());
            }
        };
        Ok((term_id, pid))
    }

    fn send(&self, msg: &ToPtyd) -> Result<(), String> {
        let line = serde_json::to_string(msg).expect("commands serialize");
        let mut conn = self.conn.lock().unwrap();
        let c = conn.as_mut().ok_or(NOT_CONNECTED)?;
        if writeln!(c, "{line}").and_then(|_| c.flush()).is_err() {
            *conn = None;
            return Err(NOT_CONNECTED.into());
        }
        Ok(())
    }

    fn require(&self, id: &str) -> Result<(), String> {
        if self.terms.lock().unwrap().contains_key(id) {
            Ok(())
        } else {
            Err("that terminal has closed".into())
        }
    }

    /// How a terminal was started, if this colonyd started it.
    pub fn request_for(&self, id: &str) -> Option<SpawnRequest> {
        self.requests.lock().unwrap().get(id).cloned()
    }

    /// Wait until a terminal has closed.
    pub async fn closed(&self, id: &str, within: Duration) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if !self.terms.lock().unwrap().contains_key(id) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    pub fn scrollback(&self, id: &str) -> Option<Vec<u8>> {
        self.terms.lock().unwrap().get(id).map(|m| m.scrollback.iter().copied().collect())
    }

    /// Raw keystrokes from the map's terminal pane.
    pub fn input(&self, id: &str, bytes: &[u8]) -> Result<(), String> {
        self.require(id)?;
        self.send(&ToPtyd::Input { term: id.into(), data: b64(bytes) })
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
        self.require(id)?;
        self.send(&ToPtyd::Resize { term: id.into(), cols, rows })
    }

    pub fn kill(&self, id: &str) -> Result<(), String> {
        match self.terms.lock().unwrap().get_mut(id) {
            Some(m) => m.kill_requested = true,
            None => return Err("that terminal has closed".into()),
        }
        self.send(&ToPtyd::Kill { term: id.into() })
    }

    // ---- connection to ptyd -----------------------------------------------

    fn run(self: Arc<Self>) {
        let mut warned = false;
        loop {
            match connect() {
                Ok(stream) => {
                    warned = false;
                    self.serve(stream);
                    log("lost the terminal host; reconnecting");
                }
                Err(e) if !warned => {
                    log(format!("terminal host unavailable: {e}"));
                    warned = true;
                }
                Err(_) => {}
            }
            *self.conn.lock().unwrap() = None;
            for (_, (_, tx)) in self.pending.lock().unwrap().drain() {
                let _ = tx.send(Err(NOT_CONNECTED.into()));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn serve(&self, stream: TcpStream) {
        let Ok(reader) = stream.try_clone() else { return };
        *self.conn.lock().unwrap() = Some(stream);
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<FromPtyd>(&line) {
                Ok(msg) => self.handle(msg),
                Err(e) => log(format!("terminal host sent a bad line ({e})")),
            }
        }
    }

    fn handle(&self, msg: FromPtyd) {
        match msg {
            FromPtyd::Ready { version } => {
                log(format!("connected to colony-ptyd {version}"));
                let known: HashSet<String> = self.terms.lock().unwrap().keys().cloned().collect();
                *self.unconfirmed.lock().unwrap() = known;
            }
            FromPtyd::Term { info, scrollback } => {
                self.unconfirmed.lock().unwrap().remove(&info.term);
                let sb = base64::engine::general_purpose::STANDARD.decode(scrollback).unwrap_or_default();
                self.adopt(info, sb.into());
            }
            FromPtyd::Synced => {
                // Terminals ptyd no longer has (it restarted): they're gone.
                let gone: Vec<String> = self.unconfirmed.lock().unwrap().drain().collect();
                for term in gone {
                    self.exited(&term, false);
                }
                let n = self.terms.lock().unwrap().len();
                if n > 0 {
                    log(format!("relinked {n} live terminal(s)"));
                }
            }
            FromPtyd::Spawned { term, pid } => {
                if let Some((mut info, tx)) = self.pending.lock().unwrap().remove(&term) {
                    info.pid = pid;
                    self.adopt(info, VecDeque::new());
                    let _ = tx.send(Ok(pid));
                }
            }
            FromPtyd::SpawnFailed { term, error } => {
                if let Some((_, tx)) = self.pending.lock().unwrap().remove(&term) {
                    let _ = tx.send(Err(error));
                }
            }
            FromPtyd::Output { term, data } => {
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else { return };
                if let Some(m) = self.terms.lock().unwrap().get_mut(&term) {
                    m.scrollback.extend(&bytes);
                    let excess = m.scrollback.len().saturating_sub(SCROLLBACK_BYTES);
                    m.scrollback.drain(..excess);
                }
                let _ = self.output.send(Output { term_id: term, bytes: Arc::new(bytes) });
            }
            FromPtyd::Exited { term, requested } => self.exited(&term, requested),
        }
    }

    /// Track a terminal and link it to its bot.
    fn adopt(&self, info: TermInfo, scrollback: VecDeque<u8>) {
        self.owned.lock().unwrap().insert(info.pid, info.term.clone());
        // A terminal that isn't a bot's (a login) has no session to attach to.
        if !info.session_id.is_empty() {
            let _ = self.events.blocking_send(Envelope {
                ts: now_ms(),
                host: info.host.clone(),
                session_id: info.session_id.clone(),
                cwd: None,
                event: DomainEvent::TerminalAttached { term_id: info.term.clone(), dir: info.dir.clone(), pid: Some(info.pid) },
            });
        }
        let mut terms = self.terms.lock().unwrap();
        let kill_requested = terms.get(&info.term).is_some_and(|m| m.kill_requested);
        terms.insert(info.term.clone(), Mirror { info, scrollback, kill_requested });
    }

    fn exited(&self, term: &str, requested: bool) {
        let Some(m) = self.terms.lock().unwrap().remove(term) else { return };
        self.owned.lock().unwrap().retain(|_, t| t != term);
        if !m.info.session_id.is_empty() {
            let _ = self.events.blocking_send(Envelope {
                ts: now_ms(),
                host: m.info.host.clone(),
                session_id: m.info.session_id.clone(),
                cwd: None,
                event: DomainEvent::TerminalExited { term_id: term.into(), requested: requested || m.kill_requested },
            });
        }
        let _ = self.output.send(Output { term_id: term.into(), bytes: Arc::new(b"\r\n\x1b[2m[session ended]\x1b[0m\r\n".to_vec()) });
    }
}

/// Connect to colony-ptyd, starting it if it isn't running.
fn connect() -> Result<TcpStream, String> {
    if let Ok(s) = try_connect() {
        return Ok(s);
    }
    start_ptyd()?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        if let Ok(s) = try_connect() {
            return Ok(s);
        }
    }
    Err("colony-ptyd didn't start; see ptyd.log in Colony's folder".into())
}

fn try_connect() -> Result<TcpStream, String> {
    let text = std::fs::read_to_string(colony_home().join("ptyd.json")).map_err(|e| e.to_string())?;
    let info: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let port = info["port"].as_u64().ok_or("no port")? as u16;
    let token = info["token"].as_str().ok_or("no token")?.to_string();
    let mut s = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(500)).map_err(|e| e.to_string())?;
    let _ = s.set_nodelay(true);
    let hello = serde_json::to_string(&ToPtyd::Hello { token }).expect("serializes");
    writeln!(s, "{hello}").map_err(|e| e.to_string())?;
    Ok(s)
}

/// colony-ptyd ships next to colonyd. It's started detached, outside any job
/// colonyd is in where possible, so it outlives colonyd.
fn start_ptyd() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = exe.with_file_name(if cfg!(windows) { "colony-ptyd.exe" } else { "colony-ptyd" });
    if !path.is_file() {
        return Err(format!("{} is missing", path.display()));
    }
    std::fs::create_dir_all(colony_home()).map_err(|e| e.to_string())?;
    let spawn = |flags: u32| {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(colony_home().join("ptyd.log")).map_err(|e| e.to_string())?;
        let mut cmd = std::process::Command::new(&path);
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(log);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = flags;
        cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
    };
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    // Breaking away fails when colonyd's job forbids it; then start it plainly.
    spawn(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP))
        .map_err(|e| format!("couldn't start {}: {e}", path.display()))?;
    log(format!("started {}", path.display()));
    Ok(())
}

/// A model name safe to pass to `--model` or type after `/model`: an alias or
/// an id, nothing that could be another flag or a second command.
pub fn checked_model(m: &str) -> Result<&str, String> {
    let ok = !m.is_empty()
        && m.len() <= 64
        && !m.starts_with('-')
        && m.chars().all(|c| c.is_ascii_alphanumeric() || "-._[]".contains(c));
    if ok {
        Ok(m)
    } else {
        Err(format!("not a model name: {m:?}"))
    }
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
    fn model_names_are_checked() {
        assert!(checked_model("opus").is_ok());
        assert!(checked_model("claude-sonnet-5-5").is_ok());
        assert!(checked_model("claude-opus-5-5[1m]").is_ok());
        assert!(checked_model("--dangerously-skip-permissions").is_err());
        assert!(checked_model("haiku; rm -rf /").is_err());
        assert!(checked_model("").is_err());
    }

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
