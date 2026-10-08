//! The colony: every agent and its lifecycle state, folded from domain events.
//!
//! This is the "truth" layer of the behavioral model. The map may animate a
//! bot toward its new spot, but what it shows is always derived from here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{preview, DomainEvent, Envelope, HostId};
use crate::names;
use crate::paths;

/// A session with no prompt this long after starting is idle.
pub const SPAWN_TO_IDLE_MS: u64 = 60_000;
/// A session Colony started with no activity this long after launch is
/// waiting at its terminal for you.
pub const STARTUP_WAIT_MS: u64 = 8_000;
pub const STARTUP_REASON: &str = "Waiting in its terminal: answer the startup question or send a first message";
/// Grace between a registry file vanishing and calling the session crashed,
/// so a normal exit's `SessionEnd` can arrive first.
pub const CRASH_GRACE_MS: u64 = 5_000;
/// Working with no events and no tool running for this long counts as stuck.
pub const STALL_MS: u64 = 10 * 60_000;
/// A single tool call running this long counts as stuck.
pub const TOOL_STALL_MS: u64 = 30 * 60_000;
/// Consecutive failed tool calls before an agent counts as blocked.
pub const FAILURES_TO_BLOCK: u32 = 3;
/// Ended main agents stay visible this long, returned subagents this long.
pub const ENDED_TTL_MS: u64 = 10 * 60_000;
pub const SUB_ENDED_TTL_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Spawning,
    Working,
    /// A permission decision is pending.
    NeedsInput,
    /// The turn ended with a question.
    AwaitingReply,
    /// Repeated failures, API errors, or a stall.
    Blocked,
    /// The turn ended without a question: work to look at.
    ReadyToReview,
    Idle,
    Crashed,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Input,
    Review,
}

impl AgentState {
    /// The porch/dock bucket this state belongs to, if it needs a human.
    pub fn severity(self) -> Option<Severity> {
        match self {
            AgentState::Blocked | AgentState::Crashed => Some(Severity::Critical),
            AgentState::NeedsInput | AgentState::AwaitingReply => Some(Severity::Input),
            AgentState::ReadyToReview => Some(Severity::Review),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Main,
    Subagent,
}

/// A permission request Colony is holding for the map to answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionAsk {
    pub request_id: String,
    pub tool: String,
    pub target: Option<String>,
    pub asked_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurrentTool {
    pub name: String,
    pub target: Option<String>,
    pub tool_use_id: Option<String>,
    pub started_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    /// Session id for a main agent, `<session id>/<agent id>` for a subagent.
    pub id: String,
    pub session_id: String,
    pub parent_id: Option<String>,
    pub kind: AgentKind,
    pub name: String,
    pub host: HostId,
    /// Current working directory; moves when the session `cd`s.
    pub cwd: Option<String>,
    /// The folder the session was started in. Decides the project, so a
    /// session that `cd`s into a subfolder stays in its district.
    pub project_dir: Option<String>,
    pub project_key: Option<String>,
    pub project_name: Option<String>,
    /// Session title from the registry.
    pub title: Option<String>,
    pub entrypoint: Option<String>,
    pub version: Option<String>,
    /// Most recently seen process for this session.
    pub pid: Option<u32>,
    /// Every live process registered for this session. Usually one; two when
    /// the same conversation is open in two places (say the desktop app and a
    /// Colony terminal).
    #[serde(default)]
    pub pids: Vec<u32>,
    pub subagent_type: Option<String>,
    pub state: AgentState,
    pub state_since: u64,
    /// Why the agent is in its state: the question, permission, or error.
    pub reason: Option<String>,
    /// First prompt of the session, or the subagent's type.
    pub objective: Option<String>,
    pub last_prompt: Option<String>,
    pub last_message: Option<String>,
    pub current_tool: Option<CurrentTool>,
    pub tool_calls: u64,
    pub consecutive_failures: u32,
    pub children: Vec<String>,
    pub last_event_at: u64,
    /// True once any hook event arrived; until then the registry status
    /// drives the state.
    pub hooks_seen: bool,
    /// Set when Colony started this session in its own terminal, so the map
    /// can show the terminal and send input to it.
    pub terminal: Option<String>,
    /// A permission request waiting for an answer on the map.
    #[serde(default)]
    pub permission: Option<PermissionAsk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_gone_at: Option<u64>,
}

impl Agent {
    fn new_main(e: &Envelope) -> Agent {
        Agent {
            id: e.session_id.clone(),
            session_id: e.session_id.clone(),
            parent_id: None,
            kind: AgentKind::Main,
            name: names::main_name(&e.session_id),
            host: e.host.clone(),
            cwd: None,
            project_dir: None,
            project_key: None,
            project_name: None,
            title: None,
            entrypoint: None,
            version: None,
            pid: None,
            pids: Vec::new(),
            subagent_type: None,
            state: AgentState::Spawning,
            state_since: e.ts,
            reason: None,
            objective: None,
            last_prompt: None,
            last_message: None,
            current_tool: None,
            tool_calls: 0,
            consecutive_failures: 0,
            children: Vec::new(),
            last_event_at: e.ts,
            hooks_seen: false,
            terminal: None,
            permission: None,
            process_gone_at: None,
        }
    }

    fn set_state(&mut self, state: AgentState, reason: Option<String>, ts: u64) {
        if self.state != state {
            self.state = state;
            self.state_since = ts;
        }
        self.reason = reason;
    }

    fn set_cwd(&mut self, cwd: &str) {
        if self.cwd.as_deref() != Some(cwd) {
            self.cwd = Some(cwd.to_string());
        }
        if self.project_dir.is_none() {
            self.set_project(cwd);
        }
    }

    /// Pin the project to the folder the session started in.
    fn set_project_dir(&mut self, dir: &str) {
        self.project_dir = Some(dir.to_string());
        self.set_project(dir);
    }

    fn set_project(&mut self, dir: &str) {
        self.project_key = Some(paths::project_key(&self.host, dir));
        self.project_name = Some(paths::project_name(dir));
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, AgentState::Ended | AgentState::Crashed)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Colony {
    pub agents: BTreeMap<String, Agent>,
}

pub fn sub_id(session_id: &str, agent_id: &str) -> String {
    format!("{session_id}/{agent_id}")
}

impl Colony {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event in. Returns the ids of agents that changed; an id that
    /// is no longer in `agents` was removed.
    pub fn apply(&mut self, e: &Envelope) -> Vec<String> {
        let mut changed = Vec::new();
        let sid = e.session_id.clone();
        if !self.agents.contains_key(&sid) {
            let mut a = Agent::new_main(e);
            a.name = self.unique_name(&a.name);
            self.agents.insert(sid.clone(), a);
        }
        let main = self.agents.get_mut(&sid).expect("inserted above");
        match (&e.event, &e.cwd) {
            // Registry cwd is handled below as the project folder.
            (DomainEvent::SessionSeen { .. }, _) | (_, None) => {}
            // Where a session starts is its project, whatever it cds into later.
            (DomainEvent::SessionStarted { source }, Some(cwd))
                if main.project_dir.is_none() && source.as_deref() != Some("compact") =>
            {
                main.set_project_dir(cwd);
                main.set_cwd(cwd);
            }
            (_, Some(cwd)) => main.set_cwd(cwd),
        }
        let from_hooks = !matches!(
            e.event,
            DomainEvent::SessionSeen { .. }
                | DomainEvent::SessionGone { .. }
                | DomainEvent::TerminalAttached { .. }
                | DomainEvent::TerminalExited { .. }
                | DomainEvent::PermissionAsked { .. }
                | DomainEvent::PermissionSettled { .. }
        );
        if from_hooks {
            main.hooks_seen = true;
            main.last_event_at = main.last_event_at.max(e.ts);
            main.process_gone_at = None;
        }
        changed.push(sid.clone());

        match &e.event {
            DomainEvent::SessionSeen { record } => {
                // The registry keeps the folder the session started in.
                if let Some(dir) = &record.cwd {
                    main.set_project_dir(dir);
                    if main.cwd.is_none() {
                        main.cwd = Some(dir.clone());
                    }
                }
                main.pid = Some(record.pid);
                if !main.pids.contains(&record.pid) {
                    main.pids.push(record.pid);
                }
                main.title = record.name.clone().or(main.title.take());
                main.entrypoint = record.entrypoint.clone().or(main.entrypoint.take());
                main.version = record.version.clone().or(main.version.take());
                main.process_gone_at = None;
                if main.state == AgentState::Crashed {
                    main.set_state(AgentState::Idle, None, e.ts);
                }
                if !main.hooks_seen {
                    if let Some(state) = record.status.as_deref().and_then(registry_state) {
                        main.set_state(state, None, e.ts);
                    }
                }
            }
            DomainEvent::SessionGone { pid } => {
                let known = main.pid == Some(*pid) || main.pids.contains(pid);
                main.pids.retain(|p| p != pid);
                if main.pid == Some(*pid) {
                    main.pid = main.pids.last().copied();
                }
                // Only the last copy going away can mean a crash.
                if known && main.pids.is_empty() && main.terminal.is_none() && !main.is_finished() {
                    main.process_gone_at = Some(e.ts);
                }
            }
            DomainEvent::SessionStarted { source } => match source.as_deref() {
                Some("compact") => {}
                Some("clear") => main.set_state(AgentState::Idle, None, e.ts),
                _ => {
                    if main.is_finished() || main.state == AgentState::Spawning {
                        main.set_state(AgentState::Spawning, None, e.ts);
                    }
                }
            },
            DomainEvent::PromptSubmitted { preview, synthetic } => {
                settle_if_after(main, e.ts);
                if !synthetic && !preview.is_empty() {
                    if main.objective.is_none() {
                        main.objective = Some(preview.clone());
                    }
                    main.last_prompt = Some(preview.clone());
                }
                main.consecutive_failures = 0;
                main.set_state(AgentState::Working, None, e.ts);
            }
            DomainEvent::ToolStarted { agent_id, tool, target, tool_use_id } => {
                let a = self.target(e, agent_id.as_deref(), &mut changed);
                settle_if_after(a, e.ts);
                a.current_tool = Some(CurrentTool {
                    name: tool.clone(),
                    target: target.clone(),
                    tool_use_id: tool_use_id.clone(),
                    started_at: e.ts,
                });
                a.tool_calls += 1;
                if a.state != AgentState::Ended {
                    a.set_state(AgentState::Working, None, e.ts);
                }
                if agent_id.is_some() {
                    self.wake_main(&sid, e.ts);
                }
            }
            DomainEvent::ToolFinished { agent_id, tool, tool_use_id, ok, error } => {
                let a = self.target(e, agent_id.as_deref(), &mut changed);
                settle_if_after(a, e.ts);
                let same = a.current_tool.as_ref().is_some_and(|t| match (&t.tool_use_id, tool_use_id) {
                    (Some(x), Some(y)) => x == y,
                    _ => &t.name == tool,
                });
                if same {
                    a.current_tool = None;
                }
                if *ok {
                    a.consecutive_failures = 0;
                    if matches!(a.state, AgentState::NeedsInput | AgentState::Blocked) {
                        a.set_state(AgentState::Working, None, e.ts);
                    }
                } else {
                    a.consecutive_failures += 1;
                    if a.consecutive_failures >= FAILURES_TO_BLOCK {
                        let why = format!(
                            "{} failed tool calls in a row. Last: {}{}",
                            a.consecutive_failures,
                            tool,
                            error.as_deref().map(|s| format!(": {}", preview(s))).unwrap_or_default()
                        );
                        a.set_state(AgentState::Blocked, Some(why), e.ts);
                    } else if a.state == AgentState::NeedsInput {
                        // A denied permission comes back as a failure.
                        a.set_state(AgentState::Working, None, e.ts);
                    }
                }
            }
            DomainEvent::PermissionRequested { agent_id, tool, target } => {
                let a = self.target(e, agent_id.as_deref(), &mut changed);
                let what = match target {
                    Some(t) => format!("{tool}: {}", preview(t)),
                    None => tool.clone(),
                };
                a.set_state(AgentState::NeedsInput, Some(what), e.ts);
            }
            DomainEvent::Notified { kind, message } => match kind.as_deref() {
                Some("permission_prompt") | Some("agent_needs_input") | Some("elicitation_dialog") => {
                    if main.state != AgentState::NeedsInput {
                        main.set_state(AgentState::NeedsInput, message.clone(), e.ts);
                    }
                }
                _ => {}
            },
            DomainEvent::SubagentStarted { agent_id, agent_type } => {
                let a = self.target(e, Some(agent_id), &mut changed);
                if let Some(t) = agent_type {
                    let ordinal = a.name.rsplit(' ').next().and_then(|n| n.parse().ok()).unwrap_or(1);
                    let parent_name = a.name.split(" · ").next().unwrap_or("").to_string();
                    a.name = names::sub_name(&parent_name, Some(t), ordinal);
                    a.subagent_type = Some(t.clone());
                    a.objective = Some(t.clone());
                }
                a.set_state(AgentState::Working, None, e.ts);
            }
            DomainEvent::SubagentStopped { agent_id, last_message } => {
                let a = self.target(e, Some(agent_id), &mut changed);
                a.last_message = last_message.as_deref().map(preview);
                a.current_tool = None;
                a.set_state(AgentState::Ended, Some("returned".into()), e.ts);
            }
            DomainEvent::TurnEnded { last_message } => {
                settle_if_after(main, e.ts);
                main.current_tool = None;
                main.last_message = last_message.as_deref().map(preview);
                match last_message.as_deref().and_then(question_in) {
                    Some(q) => main.set_state(AgentState::AwaitingReply, Some(q), e.ts),
                    None => main.set_state(AgentState::ReadyToReview, main.last_message.clone(), e.ts),
                }
                self.end_children(&sid, e.ts, &mut changed);
            }
            DomainEvent::TurnFailed { error } => {
                settle_if_after(main, e.ts);
                main.current_tool = None;
                main.set_state(AgentState::Blocked, Some(format!("API error: {error}")), e.ts);
            }
            DomainEvent::Compacting => main.reason = Some("compacting context".into()),
            DomainEvent::Compacted => {
                if main.reason.as_deref() == Some("compacting context") {
                    main.reason = None;
                }
            }
            DomainEvent::SessionEnded => {
                main.permission = None;
                main.current_tool = None;
                main.set_state(AgentState::Ended, None, e.ts);
                self.end_children(&sid, e.ts, &mut changed);
            }
            DomainEvent::PermissionAsked { request_id, agent_id, tool, target } => {
                let a = self.target(e, agent_id.as_deref(), &mut changed);
                a.permission = Some(PermissionAsk {
                    request_id: request_id.clone(),
                    tool: tool.clone(),
                    target: target.clone(),
                    asked_at: e.ts,
                });
                let what = match target {
                    Some(t) => format!("{tool}: {}", preview(t)),
                    None => tool.clone(),
                };
                a.set_state(AgentState::NeedsInput, Some(what), e.ts);
            }
            DomainEvent::PermissionSettled { request_id } => {
                for a in self.agents.values_mut() {
                    if a.permission.as_ref().is_some_and(|p| &p.request_id == request_id) {
                        a.permission = None;
                        changed.push(a.id.clone());
                    }
                }
            }
            DomainEvent::TerminalAttached { term_id, dir } => {
                main.terminal = Some(term_id.clone());
                if main.project_dir.is_none() {
                    main.set_project_dir(dir);
                    main.cwd = Some(dir.clone());
                }
                main.entrypoint.get_or_insert_with(|| "colony".into());
                main.process_gone_at = None;
                if main.is_finished() {
                    // Resumed: the same session id comes back to life.
                    main.set_state(AgentState::Spawning, None, e.ts);
                }
            }
            DomainEvent::TerminalExited { term_id, requested } => {
                if main.terminal.as_deref() == Some(term_id.as_str()) {
                    main.terminal = None;
                    main.current_tool = None;
                    if *requested {
                        main.process_gone_at = None;
                        main.set_state(AgentState::Ended, Some("ended from Colony".into()), e.ts);
                        self.end_children(&sid, e.ts, &mut changed);
                    } else if !main.is_finished() && main.pids.is_empty() {
                        // A normal exit sends SessionEnd within the grace
                        // period; otherwise tick() marks it crashed. Not if
                        // another copy of the session is still running.
                        main.process_gone_at = Some(e.ts);
                    }
                }
            }
        }
        changed.dedup();
        changed
    }

    /// Time-based transitions: idle after spawn, crash after the process
    /// vanished, stalls, and removal of long-finished agents.
    pub fn tick(&mut self, now: u64) -> Vec<String> {
        let mut changed = Vec::new();
        let mut crashed_sessions = Vec::new();
        for a in self.agents.values_mut() {
            let before = (a.state, a.reason.clone());
            match a.state {
                // A session Colony started is waiting in its terminal: a
                // first-run question (folder trust, browser tools) or an empty
                // prompt. Either way it's waiting for you.
                AgentState::Spawning if a.terminal.is_some() && now.saturating_sub(a.state_since) > STARTUP_WAIT_MS => {
                    a.set_state(AgentState::NeedsInput, Some(STARTUP_REASON.into()), now);
                }
                AgentState::Spawning if a.terminal.is_none() && now.saturating_sub(a.state_since) > SPAWN_TO_IDLE_MS => {
                    a.set_state(AgentState::Idle, None, now);
                }
                AgentState::Working => {
                    let quiet = now.saturating_sub(a.last_event_at);
                    match &a.current_tool {
                        None if a.hooks_seen && quiet > STALL_MS => {
                            a.set_state(AgentState::Blocked, Some(format!("no activity for {} min", quiet / 60_000)), now);
                        }
                        Some(t) if now.saturating_sub(t.started_at) > TOOL_STALL_MS => {
                            let mins = now.saturating_sub(t.started_at) / 60_000;
                            a.set_state(AgentState::Blocked, Some(format!("{} running for {mins} min", t.name)), now);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            if let Some(gone) = a.process_gone_at {
                if now.saturating_sub(gone) > CRASH_GRACE_MS && !a.is_finished() {
                    a.current_tool = None;
                    a.set_state(AgentState::Crashed, Some("process exited without ending the session".into()), now);
                    crashed_sessions.push(a.session_id.clone());
                }
            }
            if (a.state, a.reason.clone()) != before {
                changed.push(a.id.clone());
            }
        }
        for sid in crashed_sessions {
            self.end_children(&sid, now, &mut changed);
        }
        let expired: Vec<String> = self
            .agents
            .values()
            .filter(|a| {
                a.state == AgentState::Ended
                    && now.saturating_sub(a.state_since)
                        > if a.kind == AgentKind::Subagent { SUB_ENDED_TTL_MS } else { ENDED_TTL_MS }
            })
            .map(|a| a.id.clone())
            .collect();
        for id in expired {
            self.agents.remove(&id);
            changed.push(id);
        }
        changed
    }

    /// The user looked at finished work or a crash: move it off the dock or porch.
    pub fn acknowledge(&mut self, id: &str, now: u64) -> Vec<String> {
        let Some(a) = self.agents.get_mut(id) else { return Vec::new() };
        match a.state {
            AgentState::ReadyToReview => a.set_state(AgentState::Idle, None, now),
            AgentState::Crashed => a.set_state(AgentState::Ended, None, now),
            _ => return Vec::new(),
        }
        vec![id.to_string()]
    }

    /// Names come from a fixed list, so two sessions can draw the same one.
    /// The later one gets a number: "Indigo 2".
    fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.agents.values().any(|a| a.kind == AgentKind::Main && a.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).expect("some number is free")
    }

    /// The agent an event is about: the main agent, or a subagent created on
    /// first sight with a link to its parent.
    fn target(&mut self, e: &Envelope, agent_id: Option<&str>, changed: &mut Vec<String>) -> &mut Agent {
        let sid = &e.session_id;
        let Some(agent_id) = agent_id else {
            return self.agents.get_mut(sid).expect("main agent exists");
        };
        let id = sub_id(sid, agent_id);
        if !self.agents.contains_key(&id) {
            let parent = self.agents.get_mut(sid).expect("main agent exists");
            parent.children.push(id.clone());
            let ordinal = parent.children.len();
            let mut a = Agent::new_main(e);
            a.id = id.clone();
            a.parent_id = Some(sid.clone());
            a.kind = AgentKind::Subagent;
            a.name = names::sub_name(&parent.name, None, ordinal);
            a.cwd = parent.cwd.clone();
            a.project_key = parent.project_key.clone();
            a.project_name = parent.project_name.clone();
            a.hooks_seen = true;
            self.agents.insert(id.clone(), a);
        }
        changed.push(id.clone());
        let a = self.agents.get_mut(&id).expect("just inserted");
        a.last_event_at = a.last_event_at.max(e.ts);
        a
    }

    /// A subagent is active, so its parent is working (delegating), not waiting.
    fn wake_main(&mut self, sid: &str, ts: u64) {
        if let Some(m) = self.agents.get_mut(sid) {
            if matches!(m.state, AgentState::Spawning | AgentState::Idle | AgentState::ReadyToReview) {
                m.set_state(AgentState::Working, None, ts);
            }
        }
    }

    fn end_children(&mut self, sid: &str, ts: u64, changed: &mut Vec<String>) {
        let kids = self.agents.get(sid).map(|m| m.children.clone()).unwrap_or_default();
        for k in kids {
            if let Some(a) = self.agents.get_mut(&k) {
                if a.state != AgentState::Ended {
                    a.current_tool = None;
                    a.set_state(AgentState::Ended, Some("parent stopped".into()), ts);
                    changed.push(k);
                }
            }
        }
    }

    /// Agents on the porch, most urgent first, then longest waiting.
    pub fn porch(&self) -> Vec<&Agent> {
        let mut q: Vec<&Agent> = self
            .agents
            .values()
            .filter(|a| matches!(a.state.severity(), Some(Severity::Critical | Severity::Input)))
            .collect();
        q.sort_by_key(|a| (a.state.severity(), a.state_since));
        q
    }
}

/// Clear a held permission request once the session has moved past it: a
/// later tool event or turn boundary means it was answered somewhere else.
/// Events from before the request (hooks and the request race) don't count.
fn settle_if_after(a: &mut Agent, ts: u64) {
    if a.permission.as_ref().is_some_and(|p| ts > p.asked_at) {
        a.permission = None;
    }
}

/// Map the registry's `status` for sessions without hooks.
fn registry_state(status: &str) -> Option<AgentState> {
    let s = status.to_ascii_lowercase();
    if s == "busy" || s.contains("work") || s.contains("run") {
        Some(AgentState::Working)
    } else if s.contains("wait") || s.contains("input") || s.contains("permission") {
        Some(AgentState::NeedsInput)
    } else if s == "idle" {
        Some(AgentState::Idle)
    } else {
        None
    }
}

/// If the end of an assistant message asks the user something, return that
/// question. Rules only; an optional model classifier can refine this later.
pub fn question_in(message: &str) -> Option<String> {
    let tail: String = {
        let trimmed = message.trim_end();
        let n = trimmed.chars().count();
        trimmed.chars().skip(n.saturating_sub(400)).collect()
    };
    let last_q = tail.rfind('?')?;
    // Only count a question near the end, not one quoted mid-report.
    let after = tail[last_q + 1..].trim();
    if after.chars().count() > 160 {
        return None;
    }
    let start = tail[..last_q].rfind(['.', '!', '?', '\n']).map(|i| i + 1).unwrap_or(0);
    let q = tail[start..=last_q].trim().trim_start_matches(['*', '-', '#', ' ']).trim();
    if q.chars().count() < 6 {
        return None;
    }
    Some(preview(q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_trailing_questions() {
        assert_eq!(
            question_in("Done with the parser.\n\nShould expired rooms be deleted or archived?").as_deref(),
            Some("Should expired rooms be deleted or archived?")
        );
        assert!(question_in("Fixed the bug and all 42 tests pass.").is_none());
    }

    #[test]
    fn ignores_questions_far_from_the_end() {
        let msg = format!("Why did it fail? Because the cache was stale. {}", "Details follow. ".repeat(20));
        assert!(question_in(&msg).is_none());
    }

    #[test]
    fn registry_status_mapping() {
        assert_eq!(registry_state("busy"), Some(AgentState::Working));
        assert_eq!(registry_state("idle"), Some(AgentState::Idle));
        assert_eq!(registry_state("something-new"), None);
    }
}
