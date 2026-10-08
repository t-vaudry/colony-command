//! Domain events: the small vocabulary every source is normalized into. The UI
//! and the state reducer only ever see these.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::hook::HookPayload;
use crate::registry::SessionRecord;

/// Where a session runs. Serialized as `"win"` or `"wsl:<distro>"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum HostId {
    Windows,
    Wsl(String),
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostId::Windows => f.write_str("win"),
            HostId::Wsl(d) => write!(f, "wsl:{d}"),
        }
    }
}

impl From<HostId> for String {
    fn from(h: HostId) -> String {
        h.to_string()
    }
}

impl TryFrom<String> for HostId {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        match s.as_str() {
            "win" => Ok(HostId::Windows),
            _ => s
                .strip_prefix("wsl:")
                .filter(|d| !d.is_empty())
                .map(|d| HostId::Wsl(d.to_string()))
                .ok_or_else(|| format!("unknown host id {s:?}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Unix milliseconds when the source observed the event.
    pub ts: u64,
    pub host: HostId,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub event: DomainEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DomainEvent {
    /// A registry file was created or rewritten.
    SessionSeen { record: SessionRecord },
    /// A registry file disappeared: the process is gone.
    SessionGone { pid: u32 },
    SessionStarted { source: Option<String> },
    PromptSubmitted { preview: String },
    ToolStarted { agent_id: Option<String>, tool: String, target: Option<String>, tool_use_id: Option<String> },
    ToolFinished { agent_id: Option<String>, tool: String, tool_use_id: Option<String>, ok: bool, error: Option<String> },
    PermissionRequested { agent_id: Option<String>, tool: String, target: Option<String> },
    Notified { kind: Option<String>, message: Option<String> },
    SubagentStarted { agent_id: String, agent_type: Option<String> },
    SubagentStopped { agent_id: String, last_message: Option<String> },
    TurnEnded { last_message: Option<String> },
    TurnFailed { error: String },
    Compacting,
    Compacted,
    SessionEnded,
}

const PREVIEW_CHARS: usize = 160;

pub fn preview(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= PREVIEW_CHARS {
        one_line
    } else {
        let cut: String = one_line.chars().take(PREVIEW_CHARS - 1).collect();
        format!("{cut}…")
    }
}

impl Envelope {
    /// Normalize one hook payload. Returns `None` for events the map does not
    /// use, so new hook events in later Claude Code releases are ignored safely.
    pub fn from_hook(host: HostId, ts: u64, p: &HookPayload) -> Option<Envelope> {
        let tool = || p.tool_name.clone().unwrap_or_else(|| "tool".into());
        let event = match p.hook_event_name.as_str() {
            "SessionStart" => DomainEvent::SessionStarted { source: p.source.clone() },
            "UserPromptSubmit" => DomainEvent::PromptSubmitted { preview: preview(p.prompt.as_deref().unwrap_or("")) },
            "PreToolUse" => DomainEvent::ToolStarted {
                agent_id: p.agent_id.clone(),
                tool: tool(),
                target: p.tool_target(),
                tool_use_id: p.tool_use_id.clone(),
            },
            "PostToolUse" => DomainEvent::ToolFinished {
                agent_id: p.agent_id.clone(),
                tool: tool(),
                tool_use_id: p.tool_use_id.clone(),
                ok: true,
                error: None,
            },
            "PostToolUseFailure" => DomainEvent::ToolFinished {
                agent_id: p.agent_id.clone(),
                tool: tool(),
                tool_use_id: p.tool_use_id.clone(),
                // A user interrupt is not the tool failing.
                ok: p.extra.get("is_interrupt").and_then(|v| v.as_bool()).unwrap_or(false),
                error: p.error_text(),
            },
            "PermissionRequest" => DomainEvent::PermissionRequested {
                agent_id: p.agent_id.clone(),
                tool: tool(),
                target: p.tool_target(),
            },
            "Notification" => DomainEvent::Notified { kind: p.notification_type.clone(), message: p.message.clone() },
            "SubagentStart" => DomainEvent::SubagentStarted { agent_id: p.agent_id.clone()?, agent_type: p.agent_type.clone() },
            "SubagentStop" => DomainEvent::SubagentStopped {
                agent_id: p.agent_id.clone()?,
                last_message: p.last_assistant_message.clone(),
            },
            "Stop" => DomainEvent::TurnEnded { last_message: p.last_assistant_message.clone() },
            "StopFailure" => DomainEvent::TurnFailed { error: p.error_text().unwrap_or_else(|| "unknown".into()) },
            "PreCompact" => DomainEvent::Compacting,
            "PostCompact" => DomainEvent::Compacted,
            "SessionEnd" => DomainEvent::SessionEnded,
            _ => return None,
        };
        Some(Envelope { ts, host, session_id: p.session_id.clone(), cwd: p.cwd.clone(), event })
    }

    pub fn from_record(host: HostId, ts: u64, record: SessionRecord) -> Envelope {
        Envelope {
            ts,
            host,
            session_id: record.session_id.clone(),
            cwd: record.cwd.clone(),
            event: DomainEvent::SessionSeen { record },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_id_round_trips() {
        for h in [HostId::Windows, HostId::Wsl("Ubuntu".into())] {
            let s = serde_json::to_string(&h).unwrap();
            assert_eq!(serde_json::from_str::<HostId>(&s).unwrap(), h);
        }
        assert_eq!(serde_json::to_string(&HostId::Wsl("Ubuntu".into())).unwrap(), "\"wsl:Ubuntu\"");
    }

    #[test]
    fn preview_collapses_and_truncates() {
        assert_eq!(preview("fix\n  the   bug"), "fix the bug");
        let long = "x".repeat(500);
        assert_eq!(preview(&long).chars().count(), PREVIEW_CHARS);
    }
}
