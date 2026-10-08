//! Plays hook payloads shaped like the documented Claude Code schema through
//! the reducer. Replace or extend with real captures from `~/.colony/capture`.

use colony_core::state::{sub_id, AgentKind, AgentState, Colony, CRASH_GRACE_MS, SPAWN_TO_IDLE_MS};
use colony_core::{Envelope, HookPayload, HostId, SessionRecord};
use serde_json::json;

const SID: &str = "8f1c2d3e-0000-4000-8000-000000000001";

struct Run {
    colony: Colony,
    host: HostId,
    t: u64,
}

impl Run {
    fn new(host: HostId) -> Self {
        Run { colony: Colony::new(), host, t: 1_000_000 }
    }

    fn hook(&mut self, mut v: serde_json::Value) -> Vec<String> {
        self.t += 1_000;
        let obj = v.as_object_mut().unwrap();
        obj.entry("session_id").or_insert(json!(SID));
        obj.entry("cwd").or_insert(json!("/mnt/c/Users/thoma/code/bingosync"));
        let p = HookPayload::parse(&v.to_string()).expect("payload parses");
        match Envelope::from_hook(self.host.clone(), self.t, &p) {
            Some(e) => self.colony.apply(&e),
            None => Vec::new(),
        }
    }

    fn state(&self, id: &str) -> AgentState {
        self.colony.agents[id].state
    }
}

#[test]
fn parent_with_subagents_permission_question() {
    let mut r = Run::new(HostId::Wsl("Ubuntu".into()));
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup"}));
    assert_eq!(r.state(SID), AgentState::Spawning);
    let main = &r.colony.agents[SID];
    assert_eq!(main.project_key.as_deref(), Some("c:/users/thoma/code/bingosync"));
    assert_eq!(main.project_name.as_deref(), Some("bingosync"));

    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "Add room expiry to the board API"}));
    assert_eq!(r.state(SID), AgentState::Working);
    assert_eq!(r.colony.agents[SID].objective.as_deref(), Some("Add room expiry to the board API"));

    // Two subagents start and work.
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "Explore"}));
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a2", "agent_type": "general-purpose"}));
    let w1 = sub_id(SID, "a1");
    let w2 = sub_id(SID, "a2");
    assert_eq!(r.colony.agents[&w1].kind, AgentKind::Subagent);
    assert_eq!(r.colony.agents[&w1].parent_id.as_deref(), Some(SID));
    assert!(r.colony.agents[&w1].name.contains("Explore 1"), "{}", r.colony.agents[&w1].name);
    r.hook(json!({"hook_event_name": "PreToolUse", "agent_id": "a1", "tool_name": "Grep",
                  "tool_input": {"pattern": "Room"}, "tool_use_id": "t1"}));
    assert_eq!(r.colony.agents[&w1].current_tool.as_ref().unwrap().name, "Grep");

    // w2 needs permission; only w2 goes to the porch.
    r.hook(json!({"hook_event_name": "PreToolUse", "agent_id": "a2", "tool_name": "Bash",
                  "tool_input": {"command": "pytest -x"}, "tool_use_id": "t2"}));
    r.hook(json!({"hook_event_name": "PermissionRequest", "agent_id": "a2", "tool_name": "Bash",
                  "tool_input": {"command": "pytest -x"}}));
    assert_eq!(r.state(&w2), AgentState::NeedsInput);
    assert_eq!(r.colony.agents[&w2].reason.as_deref(), Some("Bash: pytest -x"));
    assert_eq!(r.state(SID), AgentState::Working);
    assert_eq!(r.colony.porch().len(), 1);

    // Approved; tool fails once (tests fail) but that is not blocked yet.
    r.hook(json!({"hook_event_name": "PostToolUseFailure", "agent_id": "a2", "tool_name": "Bash",
                  "tool_use_id": "t2", "error": "Exit code 1"}));
    assert_eq!(r.state(&w2), AgentState::Working);
    assert!(r.colony.porch().is_empty());

    r.hook(json!({"hook_event_name": "SubagentStop", "agent_id": "a1", "last_assistant_message": "Found 4 usages."}));
    r.hook(json!({"hook_event_name": "SubagentStop", "agent_id": "a2", "last_assistant_message": "Migration written."}));
    assert_eq!(r.state(&w1), AgentState::Ended);
    assert_eq!(r.colony.agents[&w1].reason.as_deref(), Some("returned"));

    // Turn ends with a question.
    r.hook(json!({"hook_event_name": "Stop",
                  "last_assistant_message": "Expiry is in place.\n\nShould I also update the API docs?"}));
    assert_eq!(r.state(SID), AgentState::AwaitingReply);
    assert_eq!(r.colony.agents[SID].reason.as_deref(), Some("Should I also update the API docs?"));

    // Reply, then a turn that ends with a report: ready to review, then acknowledged.
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "yes"}));
    assert_eq!(r.state(SID), AgentState::Working);
    r.hook(json!({"hook_event_name": "Stop", "last_assistant_message": "Docs updated. All 58 tests pass."}));
    assert_eq!(r.state(SID), AgentState::ReadyToReview);
    let t = r.t;
    r.colony.acknowledge(SID, t);
    assert_eq!(r.state(SID), AgentState::Idle);
}

#[test]
fn repeated_failures_block_and_success_unblocks() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "fix tests"}));
    for i in 0..3 {
        r.hook(json!({"hook_event_name": "PostToolUseFailure", "tool_name": "Bash",
                      "tool_use_id": format!("t{i}"), "error": "npm test exited 1"}));
    }
    assert_eq!(r.state(SID), AgentState::Blocked);
    assert!(r.colony.agents[SID].reason.as_deref().unwrap().starts_with("3 failed tool calls"));
    r.hook(json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_use_id": "t9"}));
    assert_eq!(r.state(SID), AgentState::Working);
}

#[test]
fn interrupts_are_not_failures() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    for i in 0..5 {
        r.hook(json!({"hook_event_name": "PostToolUseFailure", "tool_name": "Bash",
                      "tool_use_id": format!("t{i}"), "error": "aborted", "is_interrupt": true}));
    }
    assert_eq!(r.state(SID), AgentState::Working);
}

#[test]
fn api_error_blocks() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    r.hook(json!({"hook_event_name": "StopFailure", "error": "rate_limit"}));
    assert_eq!(r.state(SID), AgentState::Blocked);
    assert_eq!(r.colony.agents[SID].reason.as_deref(), Some("API error: rate_limit"));
}

#[test]
fn registry_only_session_and_crash() {
    let mut r = Run::new(HostId::Windows);
    let rec = SessionRecord::parse(
        r#"{"pid":35756,"sessionId":"8f1c2d3e-0000-4000-8000-000000000001","cwd":"C:\\Users\\thoma\\code\\colony-command",
            "startedAt":1791488398091,"version":"2.1.293","kind":"interactive","entrypoint":"claude-desktop",
            "name":"AI agent simulation desktop app","status":"busy","updatedAt":1791490587505,"somethingNew":1}"#,
    )
    .unwrap();
    r.t += 1000;
    r.colony.apply(&Envelope::from_record(HostId::Windows, r.t, rec));
    let a = &r.colony.agents[SID];
    assert_eq!(a.state, AgentState::Working, "busy with no hooks maps to working");
    assert_eq!(a.entrypoint.as_deref(), Some("claude-desktop"));
    assert_eq!(a.project_name.as_deref(), Some("colony-command"));

    // Process disappears without SessionEnd: crashed after the grace period.
    r.t += 1000;
    let gone = Envelope {
        ts: r.t,
        host: HostId::Windows,
        session_id: SID.into(),
        cwd: None,
        event: colony_core::DomainEvent::SessionGone { pid: 35756 },
    };
    r.colony.apply(&gone);
    assert_eq!(r.state(SID), AgentState::Working);
    r.colony.tick(r.t + CRASH_GRACE_MS + 1);
    assert_eq!(r.state(SID), AgentState::Crashed);
}

#[test]
fn normal_exit_is_not_a_crash() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup"}));
    r.colony.agents.get_mut(SID).unwrap().pid = Some(42);
    r.t += 100;
    r.colony.apply(&Envelope {
        ts: r.t,
        host: HostId::Windows,
        session_id: SID.into(),
        cwd: None,
        event: colony_core::DomainEvent::SessionGone { pid: 42 },
    });
    r.hook(json!({"hook_event_name": "SessionEnd"}));
    r.colony.tick(r.t + CRASH_GRACE_MS + 1);
    assert_eq!(r.state(SID), AgentState::Ended);
}

#[test]
fn spawn_goes_idle_without_a_prompt() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup"}));
    r.colony.tick(r.t + SPAWN_TO_IDLE_MS + 1);
    assert_eq!(r.state(SID), AgentState::Idle);
}

#[test]
fn cd_into_a_subfolder_keeps_the_project() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup", "cwd": r"C:\code\colony-command"}));
    r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "cwd": r"C:\code\colony-command\app"}));
    let a = &r.colony.agents[SID];
    assert_eq!(a.project_name.as_deref(), Some("colony-command"));
    assert_eq!(a.cwd.as_deref(), Some(r"C:\code\colony-command\app"));
}

#[test]
fn registry_folder_overrides_a_hook_cwd() {
    let mut r = Run::new(HostId::Windows);
    // Hooks seen first from a subfolder (session started before Colony was running).
    r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "cwd": r"C:\code\colony-command\app"}));
    assert_eq!(r.colony.agents[SID].project_name.as_deref(), Some("app"));
    let rec = SessionRecord::parse(&format!(r#"{{"pid":7,"sessionId":"{SID}","cwd":"C:\\code\\colony-command","status":"busy"}}"#)).unwrap();
    r.t += 1000;
    r.colony.apply(&Envelope::from_record(HostId::Windows, r.t, rec));
    let a = &r.colony.agents[SID];
    assert_eq!(a.project_name.as_deref(), Some("colony-command"));
    assert_eq!(a.cwd.as_deref(), Some(r"C:\code\colony-command\app"), "shell cwd is kept");
}

#[test]
fn colliding_names_get_numbers() {
    let mut r = Run::new(HostId::Windows);
    // Find a second session id that hashes to the same name as SID.
    let name = colony_core::names::main_name(SID);
    let other = (0..10_000)
        .map(|i| format!("other-{i}"))
        .find(|s| colony_core::names::main_name(s) == name)
        .expect("a collision exists among 10k ids");
    r.hook(json!({"hook_event_name": "SessionStart"}));
    r.hook(json!({"hook_event_name": "SessionStart", "session_id": other}));
    assert_eq!(r.colony.agents[SID].name, name);
    assert_eq!(r.colony.agents[&other].name, format!("{name} 2"));
}

fn term(r: &mut Run, event: colony_core::DomainEvent) {
    r.t += 1000;
    let e = Envelope { ts: r.t, host: r.host.clone(), session_id: SID.into(), cwd: None, event };
    r.colony.apply(&e);
}

#[test]
fn colony_started_session_links_its_terminal() {
    use colony_core::DomainEvent::{TerminalAttached, TerminalExited};
    let mut r = Run::new(HostId::Wsl("Ubuntu".into()));
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: r"C:\Users\thoma\code\colony-command".into() });
    let a = &r.colony.agents[SID];
    assert_eq!(a.terminal.as_deref(), Some("t1"));
    assert_eq!(a.state, AgentState::Spawning);
    assert_eq!(a.project_name.as_deref(), Some("colony-command"));
    assert_eq!(a.entrypoint.as_deref(), Some("colony"));

    // Hooks from the same session arrive with the WSL view of the folder: same project.
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup", "cwd": "/mnt/c/Users/thoma/code/colony-command"}));
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "continue"}));
    assert_eq!(r.state(SID), AgentState::Working);
    assert_eq!(r.colony.agents[SID].terminal.as_deref(), Some("t1"));

    // Terminal closes without SessionEnd: crashed after the grace period.
    term(&mut r, TerminalExited { term_id: "t1".into(), requested: false });
    assert_eq!(r.colony.agents[SID].terminal, None);
    r.colony.tick(r.t + CRASH_GRACE_MS + 1);
    assert_eq!(r.state(SID), AgentState::Crashed);

    // Resuming it in a new terminal brings the same bot back.
    term(&mut r, TerminalAttached { term_id: "t2".into(), dir: r"C:\Users\thoma\code\colony-command".into() });
    assert_eq!(r.state(SID), AgentState::Spawning);
    assert_eq!(r.colony.agents[SID].terminal.as_deref(), Some("t2"));
}

#[test]
fn colony_session_waiting_at_startup_needs_you() {
    use colony_core::state::STARTUP_WAIT_MS;
    use colony_core::DomainEvent::TerminalAttached;
    let mut r = Run::new(HostId::Windows);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: r"C:\code\x".into() });
    r.colony.tick(r.t + STARTUP_WAIT_MS + 1);
    assert_eq!(r.state(SID), AgentState::NeedsInput);
    assert_eq!(r.colony.porch().len(), 1);
    // Sending the first message puts it to work.
    r.t += STARTUP_WAIT_MS + 1000;
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "continue the build"}));
    assert_eq!(r.state(SID), AgentState::Working);
}

#[test]
fn ending_a_session_from_colony_is_not_a_crash() {
    use colony_core::DomainEvent::{TerminalAttached, TerminalExited};
    let mut r = Run::new(HostId::Windows);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: r"C:\code\x".into() });
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    term(&mut r, TerminalExited { term_id: "t1".into(), requested: true });
    r.colony.tick(r.t + CRASH_GRACE_MS + 1);
    assert_eq!(r.state(SID), AgentState::Ended);
    assert_eq!(r.colony.agents[SID].reason.as_deref(), Some("ended from Colony"));
}

#[test]
fn unknown_hook_events_are_ignored() {
    let mut r = Run::new(HostId::Windows);
    assert!(r.hook(json!({"hook_event_name": "SomethingFromTheFuture", "weird": [1, 2]})).is_empty());
}
