//! Claude Code hook payloads, as received on a hook command's stdin.
//!
//! Decoding is deliberately lenient: every field except the two that identify
//! the event is optional, and unknown fields are kept in `extra`, because the
//! hook schema grows between Claude Code releases.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HookPayload {
    pub session_id: String,
    pub hook_event_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Present when the event comes from inside a subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    /// String for `PostToolUseFailure` and `StopFailure`; kept as a value in
    /// case a release changes its shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    /// `SessionStart` source: startup, resume, clear, compact, fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl HookPayload {
    pub fn parse(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    /// The file, directory, or command a tool call is aimed at, for picking
    /// which building a bot walks to and for one-line summaries.
    pub fn tool_target(&self) -> Option<String> {
        let input = self.tool_input.as_ref()?.as_object()?;
        for key in ["file_path", "notebook_path", "path", "url", "pattern", "command", "description", "query"] {
            if let Some(v) = input.get(key).and_then(Value::as_str) {
                return Some(v.to_string());
            }
        }
        None
    }

    /// The call was started with `run_in_background`: it returns at once and
    /// the work carries on after the turn ends.
    pub fn runs_in_background(&self) -> bool {
        self.tool_input.as_ref().and_then(|i| i.get("run_in_background")).and_then(Value::as_bool).unwrap_or(false)
    }

    /// The `model` field as an id: a plain string, or an object's `id`.
    pub fn model_id(&self) -> Option<String> {
        match self.model.as_ref()? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Object(o) => o.get("id").or_else(|| o.get("display_name")).and_then(Value::as_str).map(str::to_string),
            _ => None,
        }
    }

    pub fn error_text(&self) -> Option<String> {
        match self.error.as_ref()? {
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }
}
