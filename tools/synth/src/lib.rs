//! Shared by `colony-synth` and `colony-replay`: a minimal client for a test
//! colonyd's `POST /api/ingest`, and a way to start an isolated colonyd.

pub mod fleet;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use colony_core::Envelope;

/// Talks to a colonyd over plain HTTP on loopback. No TLS or keep-alive, so
/// there is nothing to depend on.
pub struct Client {
    pub port: u16,
    pub token: String,
}

impl Client {
    /// Connect using the port and token a daemon wrote to `<home>/daemon.json`.
    pub fn from_home(home: &Path) -> Result<Client, String> {
        let path = home.join("daemon.json");
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("bad {}: {e}", path.display()))?;
        Ok(Client {
            port: v["port"].as_u64().ok_or("daemon.json has no port")? as u16,
            token: v["token"].as_str().ok_or("daemon.json has no token")?.to_string(),
        })
    }

    fn request(&self, method: &str, path: &str, body: &str) -> Result<(u16, String), String> {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).map_err(|e| format!("connect: {e}"))?;
        s.set_read_timeout(Some(Duration::from_secs(30))).ok();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.port,
            self.token,
            body.len()
        );
        s.write_all(req.as_bytes()).map_err(|e| format!("send: {e}"))?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).map_err(|e| format!("read: {e}"))?;
        let text = String::from_utf8_lossy(&raw);
        let status = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or("no HTTP status in reply")?;
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        Ok((status, body))
    }

    /// Send a batch of events. Fails with a hint when the daemon was not
    /// started with `COLONY_INGEST=1`.
    pub fn ingest(&self, events: &[Envelope]) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let body = serde_json::to_string(events).map_err(|e| e.to_string())?;
        match self.request("POST", "/api/ingest", &body)? {
            (200, _) => Ok(()),
            (404, _) => Err("the daemon refused /api/ingest; start it with COLONY_INGEST=1".into()),
            (code, text) => Err(format!("ingest failed ({code}): {}", text.trim())),
        }
    }

    /// The daemon's current snapshot (`GET /api/agents`).
    pub fn snapshot(&self) -> Result<serde_json::Value, String> {
        match self.request("GET", "/api/agents", "")? {
            (200, body) => serde_json::from_str(&body).map_err(|e| e.to_string()),
            (code, _) => Err(format!("snapshot failed ({code})")),
        }
    }
}

/// An isolated colonyd this process started. Killed on drop.
pub struct TestDaemon {
    pub home: PathBuf,
    pub client: Client,
    child: Child,
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A port nothing is listening on right now.
pub fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    Ok(l.local_addr().map_err(|e| e.to_string())?.port())
}

/// Start `colonyd` on `port` with its own `COLONY_HOME`. The user's home is
/// pointed there too, so none of the real Claude sessions show up in it.
pub fn spawn_daemon(colonyd: &Path, home: &Path, port: u16) -> Result<TestDaemon, String> {
    std::fs::create_dir_all(home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let _ = std::fs::remove_file(home.join("daemon.json"));
    let child = Command::new(colonyd)
        .env("COLONY_HOME", home)
        .env("COLONY_PORT", port.to_string())
        .env("COLONY_INGEST", "1")
        .env(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", colonyd.display()))?;
    let mut daemon = TestDaemon { home: home.to_path_buf(), client: Client { port, token: String::new() }, child };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(c) = Client::from_home(home) {
            if c.port == port && c.snapshot().is_ok() {
                daemon.client = c;
                return Ok(daemon);
            }
        }
        if Instant::now() > deadline {
            return Err(format!("colonyd did not come up on port {port}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The `colonyd` binary next to the running tool, or on PATH.
pub fn find_colonyd() -> PathBuf {
    let name = if cfg!(windows) { "colonyd.exe" } else { "colonyd" };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Common command-line handling for both tools: where the daemon is.
pub struct Target {
    pub client: Client,
    /// Held so a spawned daemon lives as long as the run.
    pub _daemon: Option<TestDaemon>,
}

/// `--spawn` starts an isolated daemon (in `--home`, default a temp folder, on a
/// free port); otherwise `--home` (or `COLONY_HOME`) names the running daemon.
pub fn connect(spawn: bool, home: Option<PathBuf>, port: Option<u16>, colonyd: Option<PathBuf>) -> Result<Target, String> {
    if spawn {
        let home = home.unwrap_or_else(|| std::env::temp_dir().join(format!("colony-synth-{}", std::process::id())));
        let port = match port {
            Some(p) => p,
            None => free_port()?,
        };
        let d = spawn_daemon(&colonyd.unwrap_or_else(find_colonyd), &home, port)?;
        eprintln!("started colonyd on 127.0.0.1:{port}, COLONY_HOME={}", home.display());
        eprintln!("point the map at it with COLONY_HOME set to that folder");
        return Ok(Target { client: Client { port: d.client.port, token: d.client.token.clone() }, _daemon: Some(d) });
    }
    let home = home
        .or_else(|| std::env::var_os("COLONY_HOME").map(PathBuf::from))
        .ok_or("say which daemon: --spawn, or --home <COLONY_HOME of a colonyd started with COLONY_INGEST=1>")?;
    let mut client = Client::from_home(&home)?;
    if let Some(p) = port {
        client.port = p;
    }
    Ok(Target { client, _daemon: None })
}
