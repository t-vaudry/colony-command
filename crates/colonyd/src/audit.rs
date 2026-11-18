//! Audit trail of what remote devices asked the daemon to do
//! (`~/.colony/audit.jsonl`, one entry per line, oldest first).
//!
//! Follows the same privacy rule as `events.jsonl`: it records who, what kind
//! of command and when, and nothing about the command itself. `record` takes
//! only the command's type name, so prompt text, command text and file paths
//! can't get in. Presence heartbeats are left out (one every ~20 s per device).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Kept when trimming at startup.
const KEEP_LINES: usize = 5000;
/// The file is trimmed at startup once it passes this.
const TRIM_OVER_BYTES: u64 = 2 * 1024 * 1024;

pub fn path() -> PathBuf {
    colony_source::colony_home().join("audit.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub ts: u64,
    pub device_id: String,
    pub device_name: String,
    /// The command's type name, e.g. "input" or "permission". Nothing of its content.
    pub command: String,
    /// False when the device's scope refused it.
    pub allowed: bool,
}

pub struct Audit {
    path: Option<PathBuf>,
    lock: Mutex<()>,
}

impl Default for Audit {
    fn default() -> Self {
        Audit { path: None, lock: Mutex::new(()) }
    }
}

/// Command names are lowercase words with underscores; anything else is not logged as sent.
fn clean(kind: &str) -> String {
    if !kind.is_empty() && kind.len() <= 40 && kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        kind.to_string()
    } else {
        "unknown".into()
    }
}

impl Audit {
    /// Opens the log, dropping the oldest entries if it has grown large.
    pub fn open(path: PathBuf) -> Audit {
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > TRIM_OVER_BYTES) {
            let lines: Vec<String> = std::fs::read_to_string(&path).unwrap_or_default().lines().map(String::from).collect();
            let from = lines.len().saturating_sub(KEEP_LINES);
            let _ = std::fs::write(&path, lines[from..].join("\n") + "\n");
        }
        Audit { path: Some(path), lock: Mutex::new(()) }
    }

    /// Whether this kind of command is worth an entry.
    pub fn counts(kind: &str) -> bool {
        kind != "presence"
    }

    pub fn record(&self, ts: u64, device_id: &str, device_name: &str, kind: &str, allowed: bool) {
        let Some(path) = &self.path else { return };
        let e = Entry { ts, device_id: device_id.into(), device_name: device_name.into(), command: clean(kind), allowed };
        let _guard = self.lock.lock().unwrap();
        let line = serde_json::to_string(&e).unwrap();
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        let written = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| writeln!(f, "{line}"));
        if written.is_err() {
            crate::log("could not write the remote audit log");
        }
    }

    /// The newest `limit` entries, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Entry> {
        let Some(path) = &self.path else { return Vec::new() };
        let _guard = self.lock.lock().unwrap();
        let text = std::fs::read_to_string(path).unwrap_or_default();
        text.lines().rev().filter_map(|l| serde_json::from_str(l).ok()).take(limit).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("colony-audit-test-{}", uuid::Uuid::new_v4().simple())).join("audit.jsonl")
    }

    #[test]
    fn each_command_is_logged_with_device_and_time_newest_first() {
        let a = Audit::open(temp());
        a.record(10, "d1", "Tablet", "permission", true);
        a.record(20, "d1", "Tablet", "input", false);
        let r = a.recent(10);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].ts, r[0].command.as_str(), r[0].allowed, r[0].device_name.as_str()), (20, "input", false, "Tablet"));
        assert_eq!((r[1].ts, r[1].device_id.as_str(), r[1].command.as_str()), (10, "d1", "permission"));
        assert_eq!(a.recent(1).len(), 1);
    }

    #[test]
    fn the_file_holds_no_prompt_command_text_or_paths() {
        // `record` never sees a command's content, and an odd "type" is not written as sent.
        let p = temp();
        let a = Audit::open(p.clone());
        a.record(1, "d", "Tablet", "send", true);
        a.record(2, "d", "Tablet", "C:\\Users\\me\\secret.txt rm -rf /", true);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("secret") && !text.contains("rm -rf") && !text.contains("C:"));
        assert_eq!(a.recent(1)[0].command, "unknown");
        let keys: std::collections::BTreeSet<_> =
            serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap()).unwrap().as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys.into_iter().collect::<Vec<_>>(), ["allowed", "command", "device_id", "device_name", "ts"]);
    }

    #[test]
    fn heartbeats_are_not_entries_and_everything_else_is() {
        assert!(!Audit::counts("presence"));
        for k in ["input", "permission", "attach", "resize", "kill"] {
            assert!(Audit::counts(k), "{k}");
        }
    }

    #[test]
    fn a_big_log_is_trimmed_to_the_newest_entries_on_open() {
        let p = temp();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let line = r#"{"ts":1,"device_id":"d","device_name":"T","command":"input","allowed":true}"#;
        let n = (TRIM_OVER_BYTES as usize / line.len()) + 10;
        std::fs::write(&p, vec![line; n].join("\n") + "\n").unwrap();
        let a = Audit::open(p.clone());
        assert_eq!(std::fs::read_to_string(&p).unwrap().lines().count(), KEEP_LINES);
        assert_eq!(a.recent(usize::MAX).len(), KEEP_LINES);
    }
}
