//! Claude Code's live session registry: one `~/.claude/sessions/<pid>.json`
//! per running session, rewritten as its status changes and removed on exit.
//!
//! The format is undocumented, so every field but `pid` and `sessionId` is
//! optional. Never read the sibling `*.key` files; they hold secrets.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub pid: u32,
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Unix milliseconds.
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub version: Option<String>,
    /// e.g. "interactive"
    #[serde(default)]
    pub kind: Option<String>,
    /// e.g. "claude-desktop", "cli"
    #[serde(default)]
    pub entrypoint: Option<String>,
    /// Session title, when the user or Claude named it.
    #[serde(default)]
    pub name: Option<String>,
    /// Observed: "busy". Other values are mapped leniently.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub updated_at: Option<u64>,
    #[serde(default)]
    pub status_updated_at: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl SessionRecord {
    pub fn parse(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    /// `procStart`: when the process was created (on Windows, a FILETIME in
    /// 100 ns units since 1601), to tell it apart from a later process that
    /// reused its pid. Written as a string; accepted as a number too.
    pub fn proc_start(&self) -> Option<u64> {
        match self.extra.get("procStart")? {
            Value::String(s) => s.parse().ok(),
            Value::Number(n) => n.as_u64(),
            _ => None,
        }
    }

    /// The Colony terminal this session runs in, when Colony started it.
    /// Not written by Claude Code: added by Colony when it reads the record.
    pub fn colony_term(&self) -> Option<&str> {
        self.extra.get("colonyTerm").and_then(Value::as_str)
    }

    /// Registry file names are `<pid>.json`; anything else in the folder is ignored.
    pub fn is_record_file(file_name: &str) -> bool {
        file_name
            .strip_suffix(".json")
            .is_some_and(|stem| !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()))
    }
}
