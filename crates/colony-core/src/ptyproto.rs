//! Protocol between colonyd and colony-ptyd, the process that owns Colony's
//! terminals so sessions survive colonyd restarts. Newline-delimited JSON
//! over a localhost TCP connection; the first line must be `Hello` with the
//! token from `ptyd.json`. Terminal bytes travel base64-encoded.

use serde::{Deserialize, Serialize};

use crate::event::HostId;

/// One terminal and the session in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TermInfo {
    pub term: String,
    pub session_id: String,
    pub host: HostId,
    /// Folder the session was started in, as the map gave it.
    pub dir: String,
    /// The process ptyd started (on Windows, `claude.exe` itself).
    pub pid: u32,
    pub started_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPtyd {
    Hello {
        token: String,
    },
    Spawn {
        info: TermInfo,
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
    },
    Input {
        term: String,
        data: String,
    },
    Resize {
        term: String,
        cols: u16,
        rows: u16,
    },
    Kill {
        term: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromPtyd {
    Ready {
        version: String,
    },
    /// A live terminal, with everything it printed so far (sent for every
    /// terminal right after `Ready`).
    Term {
        info: TermInfo,
        scrollback: String,
    },
    /// All live terminals have been listed.
    Synced,
    Spawned {
        term: String,
        pid: u32,
    },
    SpawnFailed {
        term: String,
        error: String,
    },
    Output {
        term: String,
        data: String,
    },
    /// `requested`: ended through `Kill`, not on its own.
    Exited {
        term: String,
        requested: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let info = TermInfo {
            term: "t1".into(),
            session_id: "s".into(),
            host: HostId::Wsl("Ubuntu".into()),
            dir: "C:/x".into(),
            pid: 7,
            started_at: 1,
        };
        for m in [
            FromPtyd::Term { info: info.clone(), scrollback: "aGk=".into() },
            FromPtyd::Exited { term: "t1".into(), requested: true },
        ] {
            let line = serde_json::to_string(&m).unwrap();
            assert_eq!(serde_json::from_str::<FromPtyd>(&line).unwrap(), m);
        }
        let spawn = ToPtyd::Spawn {
            info,
            program: "wsl.exe".into(),
            args: vec!["-d".into()],
            cwd: None,
            env: vec![("A".into(), "b".into())],
            cols: 80,
            rows: 24,
        };
        let line = serde_json::to_string(&spawn).unwrap();
        assert!(line.contains(r#""type":"spawn""#));
        assert_eq!(serde_json::from_str::<ToPtyd>(&line).unwrap(), spawn);
    }
}
