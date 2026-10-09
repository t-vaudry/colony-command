//! The colony: every agent and its lifecycle state, folded from domain events.
//!
//! This is the "truth" layer of the behavioral model. The map may animate a
//! bot toward its new spot, but what it shows is always derived from here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{preview, DomainEvent, Envelope, HostId};
use crate::models::{self, ModelHint, ToolKind};
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
/// How long a dismissal is remembered. Longer than colonyd's history replay,
/// so a restart doesn't bring dismissed sessions back.
pub const DISMISSED_KEEP_MS: u64 = 24 * 60 * 60_000;

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
    /// The tool call's input, with long strings trimmed.
    #[serde(default)]
    pub input: Option<serde_json::Value>,
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
    /// The whole of that message (capped), for reading a question in full.
    #[serde(default)]
    pub last_message_full: Option<String>,
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
    /// The registered process running in that terminal: this session's own
    /// copy, as opposed to another copy open somewhere else.
    #[serde(default)]
    pub terminal_pid: Option<u32>,
    /// A permission request waiting for an answer on the map.
    #[serde(default)]
    pub permission: Option<PermissionAsk>,
    /// The model the session runs, when known.
    #[serde(default)]
    pub model: Option<String>,
    /// Kinds of the most recent tool calls, oldest first.
    #[serde(default)]
    pub recent_tools: Vec<ToolKind>,
    /// A model better suited to what it's doing lately, if any.
    #[serde(default)]
    pub model_hint: Option<ModelHint>,
    /// A tool failed because a login is missing; the map offers to sign in.
    /// Stays until the sign-in is confirmed (`AuthResolved`).
    #[serde(default)]
    pub auth_need: Option<crate::auth::AuthNeed>,
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
            last_message_full: None,
            current_tool: None,
            tool_calls: 0,
            consecutive_failures: 0,
            children: Vec::new(),
            last_event_at: e.ts,
            hooks_seen: false,
            terminal: None,
            terminal_pid: None,
            permission: None,
            model: None,
            recent_tools: Vec::new(),
            model_hint: None,
            auth_need: None,
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

    /// Copies of this session running outside Colony's terminal.
    pub fn other_pids(&self) -> Vec<u32> {
        self.pids.iter().copied().filter(|p| Some(*p) != self.terminal_pid).collect()
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, AgentState::Ended | AgentState::Crashed)
    }
}

/// A session the user cleared off the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dismissed {
    pub at: u64,
    /// Its processes when dismissed, so their late registry sightings don't
    /// count as the session coming back.
    pub pids: Vec<u32>,
}

impl Dismissed {
    /// Whether this event means the session is in use again (resumed), rather
    /// than the tail of the one that was dismissed or a replay of its history.
    fn revived_by(&self, e: &Envelope) -> bool {
        e.ts > self.at
            && match &e.event {
                DomainEvent::SessionStarted { source, .. } => source.as_deref() != Some("compact"),
                DomainEvent::PromptSubmitted { .. } | DomainEvent::TerminalAttached { .. } => true,
                DomainEvent::SessionSeen { record } => !self.pids.contains(&record.pid),
                _ => false,
            }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Colony {
    pub agents: BTreeMap<String, Agent>,
    /// Sessions the user dismissed, by session id.
    #[serde(default)]
    pub dismissed: BTreeMap<String, Dismissed>,
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
        if let Some(d) = self.dismissed.get(&sid) {
            if !d.revived_by(e) {
                return changed;
            }
            self.dismissed.remove(&sid);
        }
        if !self.agents.contains_key(&sid) {
            let mut a = Agent::new_main(e);
            a.name = self.unique_name(&a.name);
            self.agents.insert(sid.clone(), a);
        }
        // A name the user chose when starting the session replaces the drawn one.
        if let DomainEvent::Renamed { name } = &e.event {
            let name = self.unique_name_except(name.trim(), &sid);
            let main = self.agents.get_mut(&sid).expect("inserted above");
            let old = std::mem::replace(&mut main.name, name.clone());
            let children = main.children.clone();
            // Subagents are named after their parent, so they follow it.
            for id in children {
                if let Some(rest) = self.agents.get(&id).and_then(|c| c.name.strip_prefix(&format!("{old} · ")).map(str::to_string)) {
                    self.agents.get_mut(&id).expect("child exists").name = format!("{name} · {rest}");
                    changed.push(id);
                }
            }
        }
        let main = self.agents.get_mut(&sid).expect("inserted above");
        match (&e.event, &e.cwd) {
            // Registry cwd is handled below as the project folder.
            (DomainEvent::SessionSeen { .. }, _) | (_, None) => {}
            // Where a session starts is its project, whatever it cds into later.
            (DomainEvent::SessionStarted { source, .. }, Some(cwd))
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
                | DomainEvent::AuthResolved
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
                if record.colony_term().is_some() && record.colony_term() == main.terminal.as_deref() {
                    main.terminal_pid = Some(record.pid);
                }
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
            DomainEvent::SessionStarted { source, model } => {
                if model.is_some() {
                    main.model = model.clone();
                    main.model_hint = models::suggest(main.model.as_deref(), &main.recent_tools);
                }
                match source.as_deref() {
                    Some("compact") => {}
                    Some("clear") => main.set_state(AgentState::Idle, None, e.ts),
                    _ => {
                        if main.is_finished() || main.state == AgentState::Spawning {
                            main.set_state(AgentState::Spawning, None, e.ts);
                        }
                    }
                }
            }
            DomainEvent::Renamed { .. } => {}
            DomainEvent::ModelSet { model } => {
                main.model = Some(model.clone());
                main.model_hint = models::suggest(main.model.as_deref(), &main.recent_tools);
            }
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
                a.recent_tools.push(models::tool_kind(tool));
                if a.recent_tools.len() > models::RECENT_TOOLS {
                    a.recent_tools.remove(0);
                }
                // Only main agents: a subagent's model isn't reported.
                if a.kind == AgentKind::Main {
                    a.model_hint = models::suggest(a.model.as_deref(), &a.recent_tools);
                }
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
                    if let Some(need) = error.as_deref().and_then(crate::auth::detect) {
                        // Waiting on a person, not on retries: don't wait for three failures.
                        a.set_state(AgentState::Blocked, Some(need.reason()), e.ts);
                        a.auth_need = Some(need);
                    } else if a.consecutive_failures >= FAILURES_TO_BLOCK {
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
                main.last_message_full = last_message.as_deref().map(|m| trim_text(m, FULL_TEXT_CHARS));
                match last_message.as_deref().and_then(question_in) {
                    Some(q) => main.set_state(AgentState::AwaitingReply, Some(q), e.ts),
                    None => main.set_state(AgentState::ReadyToReview, main.last_message.clone(), e.ts),
                }
                self.end_children(&sid, e.ts, &mut changed);
            }
            DomainEvent::TurnFailed { error } => {
                settle_if_after(main, e.ts);
                main.current_tool = None;
                match crate::auth::detect(error) {
                    // Claude itself is signed out: only the user can fix that.
                    Some(need) => {
                        main.set_state(AgentState::Blocked, Some(need.reason()), e.ts);
                        main.auth_need = Some(need);
                    }
                    None => main.set_state(AgentState::Blocked, Some(format!("API error: {error}")), e.ts),
                }
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
            DomainEvent::PermissionAsked { request_id, agent_id, tool, target, input } => {
                let a = self.target(e, agent_id.as_deref(), &mut changed);
                a.permission = Some(PermissionAsk {
                    request_id: request_id.clone(),
                    tool: tool.clone(),
                    target: target.clone(),
                    input: input.clone(),
                    asked_at: e.ts,
                });
                let what = match target {
                    Some(t) => format!("{tool}: {}", preview(t)),
                    None => tool.clone(),
                };
                a.set_state(AgentState::NeedsInput, Some(what), e.ts);
            }
            DomainEvent::AuthResolved => {
                // The session and its subagents were all waiting on the same login.
                for a in self.agents.values_mut().filter(|a| a.session_id == sid) {
                    if a.auth_need.take().is_some() {
                        if a.state == AgentState::Blocked {
                            a.set_state(AgentState::Working, None, e.ts);
                        }
                        a.consecutive_failures = 0;
                        changed.push(a.id.clone());
                    }
                }
            }
            DomainEvent::PermissionSettled { request_id } => {
                for a in self.agents.values_mut() {
                    if a.permission.as_ref().is_some_and(|p| &p.request_id == request_id) {
                        a.permission = None;
                        changed.push(a.id.clone());
                    }
                }
            }
            DomainEvent::TerminalAttached { term_id, dir, pid } => {
                // Re-attaching the same terminal (colonyd restarted) keeps
                // what's known about it; a new terminal starts fresh.
                if main.terminal.as_deref() != Some(term_id.as_str()) {
                    main.terminal_pid = None;
                }
                main.terminal = Some(term_id.clone());
                if pid.is_some() {
                    main.terminal_pid = *pid;
                }
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
                    main.terminal_pid = None;
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
        self.dismissed.retain(|_, d| now.saturating_sub(d.at) <= DISMISSED_KEEP_MS);
        changed
    }

    /// The user is done with a session: take it and its subagents off the map
    /// now, and ignore what it sends from here on unless it's resumed. Ending
    /// its processes is the caller's job.
    pub fn dismiss(&mut self, id: &str, now: u64) -> Vec<String> {
        let Some(a) = self.agents.get(id).filter(|a| a.kind == AgentKind::Main) else { return Vec::new() };
        let mut pids = a.pids.clone();
        pids.extend(a.pid.into_iter().chain(a.terminal_pid).filter(|p| !a.pids.contains(p)));
        let mut changed = a.children.clone();
        changed.push(id.to_string());
        for c in &changed {
            self.agents.remove(c);
        }
        self.dismissed.insert(id.to_string(), Dismissed { at: now, pids });
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
        self.unique_name_except(base, "")
    }

    /// As `unique_name`, ignoring the session being named.
    fn unique_name_except(&self, base: &str, session_id: &str) -> String {
        let taken = |n: &str| self.agents.values().any(|a| a.kind == AgentKind::Main && a.session_id != session_id && a.name == n);
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

/// Longest text kept whole for the map to show in full.
const FULL_TEXT_CHARS: usize = 6000;

fn trim_text(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// A copy of a JSON value with every string trimmed to a size the map can hold.
pub fn trim_strings(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => Value::String(trim_text(s, FULL_TEXT_CHARS)),
        Value::Array(a) => Value::Array(a.iter().map(trim_strings).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), trim_strings(v))).collect()),
        other => other.clone(),
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
