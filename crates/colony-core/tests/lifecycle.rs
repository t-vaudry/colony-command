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
fn a_missing_login_blocks_at_once_until_signed_in() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "open a PR"}));
    r.hook(json!({"hook_event_name": "PostToolUseFailure", "tool_name": "Bash", "tool_use_id": "t1",
                  "error": "To get started with GitHub CLI, please run:  gh auth login"}));
    assert_eq!(r.state(SID), AgentState::Blocked);
    assert_eq!(r.colony.agents[SID].auth_need.as_ref().map(|n| n.provider.as_str()), Some("github"));
    // A later success elsewhere doesn't say the login was fixed.
    r.hook(json!({"hook_event_name": "PostToolUse", "tool_name": "Read", "tool_use_id": "t2"}));
    assert!(r.colony.agents[SID].auth_need.is_some());
    // Colony confirmed the sign-in.
    r.t += 1_000;
    let e = Envelope { ts: r.t, host: HostId::Windows, session_id: SID.into(), cwd: None, event: colony_core::DomainEvent::AuthResolved };
    r.colony.apply(&e);
    assert!(r.colony.agents[SID].auth_need.is_none());
    assert_eq!(r.state(SID), AgentState::Working);
}

#[test]
fn claude_being_signed_out_asks_for_a_sign_in() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi"}));
    r.hook(json!({"hook_event_name": "StopFailure", "error": "authentication_failed"}));
    assert_eq!(r.state(SID), AgentState::Blocked);
    let a = &r.colony.agents[SID];
    assert_eq!(a.auth_need.as_ref().map(|n| n.provider.as_str()), Some("claude"));
    assert_eq!(a.reason.as_deref(), Some("Claude needs you to sign in"));
    // Other API errors stay plain API errors.
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi"}));
    r.hook(json!({"hook_event_name": "StopFailure", "error": "rate_limit"}));
    assert!(r.colony.agents[SID].auth_need.is_none());
    assert_eq!(r.colony.agents[SID].reason.as_deref(), Some("API error: rate_limit"));
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
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: r"C:\Users\thoma\code\colony-command".into(), pid: None });
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
    term(&mut r, TerminalAttached { term_id: "t2".into(), dir: r"C:\Users\thoma\code\colony-command".into(), pid: None });
    assert_eq!(r.state(SID), AgentState::Spawning);
    assert_eq!(r.colony.agents[SID].terminal.as_deref(), Some("t2"));
}

#[test]
fn colony_session_waiting_at_startup_needs_you() {
    use colony_core::state::STARTUP_WAIT_MS;
    use colony_core::DomainEvent::TerminalAttached;
    let mut r = Run::new(HostId::Windows);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: r"C:\code\x".into(), pid: None });
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
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: r"C:\code\x".into(), pid: None });
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    term(&mut r, TerminalExited { term_id: "t1".into(), requested: true });
    r.colony.tick(r.t + CRASH_GRACE_MS + 1);
    assert_eq!(r.state(SID), AgentState::Ended);
    assert_eq!(r.colony.agents[SID].reason.as_deref(), Some("ended from Colony"));
}

fn seen(r: &mut Run, pid: u32) {
    r.t += 1000;
    let rec = SessionRecord::parse(&format!(r#"{{"pid":{pid},"sessionId":"{SID}","entrypoint":"claude-desktop","status":"idle"}}"#)).unwrap();
    r.colony.apply(&Envelope::from_record(r.host.clone(), r.t, rec));
}

#[test]
fn a_conversation_open_in_two_places_keeps_both_processes() {
    use colony_core::DomainEvent::{SessionGone, TerminalAttached, TerminalExited};
    let mut r = Run::new(HostId::Windows);
    // Open in the desktop app, then also resumed in a Colony terminal.
    seen(&mut r, 100);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: r"C:\code\x".into(), pid: None });
    assert_eq!(r.colony.agents[SID].pids, vec![100]);

    // Ending the Colony copy leaves the desktop copy known and running, so
    // the map can still warn before resuming again.
    term(&mut r, TerminalExited { term_id: "t1".into(), requested: true });
    assert_eq!(r.state(SID), AgentState::Ended);
    assert_eq!(r.colony.agents[SID].pids, vec![100]);

    // One of two registered copies exiting is not a crash.
    seen(&mut r, 200);
    term(&mut r, SessionGone { pid: 100 });
    let a = &r.colony.agents[SID];
    assert_eq!((a.pids.clone(), a.pid), (vec![200], Some(200)));
    r.colony.tick(r.t + CRASH_GRACE_MS + 1);
    assert_ne!(r.state(SID), AgentState::Crashed);
}

#[test]
fn held_permission_requests_show_and_clear() {
    use colony_core::DomainEvent::{PermissionAsked, PermissionSettled};
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "make a folder"}));
    let pre_tool_ts = r.t + 1000;
    term(&mut r, PermissionAsked { request_id: "q1".into(), agent_id: None, tool: "Bash".into(), target: Some("mkdir x".into()), input: None });
    let a = &r.colony.agents[SID];
    assert_eq!(a.state, AgentState::NeedsInput);
    assert_eq!(a.permission.as_ref().map(|p| p.request_id.as_str()), Some("q1"));
    assert_eq!(a.reason.as_deref(), Some("Bash: mkdir x"));

    // The PreToolUse capture can arrive after the request (it's polled from
    // disk); it happened before the request, so it doesn't clear it.
    let p = HookPayload::parse(&json!({"session_id": SID, "hook_event_name": "PreToolUse", "tool_name": "Bash"}).to_string()).unwrap();
    r.colony.apply(&Envelope::from_hook(HostId::Windows, pre_tool_ts - 500, &p).unwrap());
    assert!(r.colony.agents[SID].permission.is_some());

    // Answered on the map.
    term(&mut r, PermissionSettled { request_id: "q1".into() });
    assert!(r.colony.agents[SID].permission.is_none());

    // Answered somewhere else: the tool finishing clears it.
    term(&mut r, PermissionAsked { request_id: "q2".into(), agent_id: None, tool: "Bash".into(), target: None, input: None });
    r.hook(json!({"hook_event_name": "PostToolUse", "tool_name": "Bash"}));
    assert!(r.colony.agents[SID].permission.is_none());
    assert_eq!(r.state(SID), AgentState::Working);
}

#[test]
fn subagent_permission_requests_land_on_the_subagent() {
    use colony_core::DomainEvent::PermissionAsked;
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "Explore"}));
    term(&mut r, PermissionAsked { request_id: "q1".into(), agent_id: Some("a1".into()), tool: "Bash".into(), target: None, input: None });
    let sub = &r.colony.agents[&sub_id(SID, "a1")];
    assert_eq!(sub.state, AgentState::NeedsInput);
    assert!(sub.permission.is_some());
    assert!(r.colony.agents[SID].permission.is_none());
}

#[test]
fn a_colony_sessions_own_registration_is_not_another_copy() {
    use colony_core::DomainEvent::TerminalAttached;
    let mut r = Run::new(HostId::Windows);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: "C:/code/x".into(), pid: None });
    // The claude that Colony started registers itself; colonyd tags it.
    r.t += 1000;
    let rec = SessionRecord::parse(&format!(r#"{{"pid":35824,"sessionId":"{SID}","entrypoint":"cli","colonyTerm":"t1"}}"#)).unwrap();
    r.colony.apply(&Envelope::from_record(HostId::Windows, r.t, rec));
    let a = &r.colony.agents[SID];
    assert_eq!(a.terminal_pid, Some(35824));
    assert!(a.other_pids().is_empty(), "its own process isn't a second copy");
    // A desktop copy of the same conversation is.
    seen(&mut r, 100);
    assert_eq!(r.colony.agents[SID].other_pids(), vec![100]);
}

#[test]
fn unknown_hook_events_are_ignored() {
    let mut r = Run::new(HostId::Windows);
    assert!(r.hook(json!({"hook_event_name": "SomethingFromTheFuture", "weird": [1, 2]})).is_empty());
}

#[test]
fn reattaching_after_a_daemon_restart_keeps_the_session() {
    use colony_core::DomainEvent::TerminalAttached;
    let mut r = Run::new(HostId::Windows);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: "C:/x".into(), pid: Some(8748) });
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "build it"}));
    seen(&mut r, 8748);
    assert!(r.colony.agents[SID].other_pids().is_empty());
    // colonyd restarts; the terminal host reports the same terminal again.
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: "C:/x".into(), pid: Some(8748) });
    let a = &r.colony.agents[SID];
    assert_eq!(a.state, AgentState::Working);
    assert_eq!(a.terminal_pid, Some(8748));
    assert!(a.other_pids().is_empty());
}

#[test]
fn dismissed_sessions_leave_the_map_and_stay_gone() {
    use colony_core::DomainEvent::{SessionGone, TerminalAttached, TerminalExited};
    let mut r = Run::new(HostId::Windows);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: "C:/x".into(), pid: Some(8748) });
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "Explore"}));
    r.hook(json!({"hook_event_name": "Stop", "last_assistant_message": "Done."}));
    seen(&mut r, 100);
    let history_ts = r.t;

    let removed = r.colony.dismiss(SID, r.t + 1);
    assert!(removed.contains(&SID.to_string()) && removed.contains(&sub_id(SID, "a1")));
    assert!(r.colony.agents.is_empty());

    // The ended terminal, the vanished process, a late hook, and a replay of
    // its history after a daemon restart don't bring it back.
    term(&mut r, TerminalExited { term_id: "t1".into(), requested: true });
    term(&mut r, SessionGone { pid: 100 });
    seen(&mut r, 100);
    r.hook(json!({"hook_event_name": "SessionEnd"}));
    let p = HookPayload::parse(&json!({"session_id": SID, "hook_event_name": "UserPromptSubmit", "prompt": "old"}).to_string()).unwrap();
    r.colony.apply(&Envelope::from_hook(HostId::Windows, history_ts, &p).unwrap());
    assert!(r.colony.agents.is_empty());

    // Resuming it later does.
    r.hook(json!({"hook_event_name": "SessionStart", "source": "resume"}));
    assert_eq!(r.state(SID), AgentState::Spawning);
    assert!(r.colony.dismissed.is_empty());
}

#[test]
fn only_main_agents_can_be_dismissed() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "Explore"}));
    assert!(r.colony.dismiss(&sub_id(SID, "a1"), r.t).is_empty());
    assert_eq!(r.colony.agents.len(), 2);
}

#[test]
fn model_and_hint_follow_the_work() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup", "model": "claude-opus-5-5"}));
    assert_eq!(r.colony.agents[SID].model.as_deref(), Some("claude-opus-5-5"));
    for i in 0..10 {
        r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": if i % 2 == 0 { "Grep" } else { "Read" }}));
    }
    let hint = r.colony.agents[SID].model_hint.clone().expect("reading on Opus suggests Haiku");
    assert_eq!(hint.model, "haiku");
    // Switching clears the hint; the next edit keeps it gone.
    r.t += 1000;
    let e = Envelope { ts: r.t, host: HostId::Windows, session_id: SID.into(), cwd: None, event: colony_core::DomainEvent::ModelSet { model: "haiku".into() } };
    r.colony.apply(&e);
    assert_eq!(r.colony.agents[SID].model.as_deref(), Some("haiku"));
    assert!(r.colony.agents[SID].model_hint.is_none());
}

#[test]
fn a_name_chosen_at_spawn_replaces_the_drawn_one() {
    use colony_core::DomainEvent::{Renamed, TerminalAttached};
    let mut r = Run::new(HostId::Windows);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: "C:/x".into(), pid: None });
    term(&mut r, Renamed { name: " Ada ".into() });
    assert_eq!(r.colony.agents[SID].name, "Ada");
    // Naming it again does not make it collide with itself.
    term(&mut r, Renamed { name: "Ada".into() });
    assert_eq!(r.colony.agents[SID].name, "Ada");
}

#[test]
fn subagents_follow_a_renamed_parent() {
    use colony_core::DomainEvent::{Renamed, TerminalAttached};
    let mut r = Run::new(HostId::Windows);
    term(&mut r, TerminalAttached { term_id: "t1".into(), dir: "C:/x".into(), pid: None });
    let old = r.colony.agents[SID].name.clone();
    let id = colony_core::state::sub_id(SID, "a1");
    r.colony.agents.insert(id.clone(), {
        let mut c = r.colony.agents[SID].clone();
        c.id = id.clone();
        c.name = format!("{old} · Explore 1");
        c
    });
    r.colony.agents.get_mut(SID).unwrap().children.push(id.clone());
    term(&mut r, Renamed { name: "Ada".into() });
    assert_eq!(r.colony.agents[&id].name, "Ada · Explore 1");
}

#[test]
fn waiting_on_a_background_run_is_not_ready_for_review() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "fix the flaky test"}));
    r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_use_id": "t1",
                  "tool_input": {"command": "cargo test", "run_in_background": true}}));
    r.hook(json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_use_id": "t1"}));

    // The turn ends while the tests still run: still working, not for review.
    r.hook(json!({"hook_event_name": "Stop", "last_assistant_message": "Tests are running; I'll check back when they finish."}));
    assert_eq!(r.state(SID), AgentState::Working);
    assert_eq!(r.colony.agents[SID].reason.as_deref(), Some(colony_core::state::WAITING_ON_BACKGROUND));

    // Quiet waiting isn't a stall for as long as a hung tool would be.
    let t = r.t;
    r.colony.tick(t + colony_core::state::STALL_MS + 1);
    assert_eq!(r.state(SID), AgentState::Working);

    // The run reports back, the bot wakes, finishes, and now it's for review.
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "<task-notification>\n<task-id>t1</task-id>"}));
    assert_eq!(r.state(SID), AgentState::Working);
    r.hook(json!({"hook_event_name": "Stop", "last_assistant_message": "All tests pass."}));
    assert_eq!(r.state(SID), AgentState::ReadyToReview);
}

fn usage(r: &mut Run, sid: &str, agent: Option<&str>, seq: u64, model: &str, input: u64, output: u64) -> Vec<String> {
    r.t += 1_000;
    let e = Envelope {
        ts: r.t,
        host: r.host.clone(),
        session_id: sid.into(),
        cwd: Some("/mnt/c/Users/thoma/code/bingosync".into()),
        event: colony_core::DomainEvent::UsageUpdated {
            agent_id: agent.map(str::to_string),
            seq,
            model: Some(model.into()),
            tokens: colony_core::Tokens { input, output, cache_read: 0, cache_creation: 0 },
        },
    };
    r.colony.apply(&e)
}

#[test]
fn usage_accumulates_rolls_up_subagents_and_ignores_replays() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup"}));
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "Explore"}));
    let sub = sub_id(SID, "a1");

    let changed = usage(&mut r, SID, None, 1, "claude-sonnet-5-5", 1_000_000, 0);
    assert_eq!(changed, vec![SID.to_string()]);
    usage(&mut r, SID, Some("a1"), 1, "claude-haiku-5-5", 0, 1_000_000);
    let (main, s) = (&r.colony.agents[SID], &r.colony.agents[&sub]);
    // Sonnet: $2 for 1M input. Haiku: $0.50 for 1M output.
    assert!((s.cost_usd - 0.5).abs() < 1e-9 && s.tokens.output == 1_000_000);
    assert!((main.cost_usd - 2.5).abs() < 1e-9, "parent includes its subagent: {}", main.cost_usd);
    assert_eq!((main.tokens.input, main.tokens.output), (1_000_000, 1_000_000));
    assert!(!main.cost_partial);

    // A source that replays its history (restarted probe) is not counted twice;
    // a newer message is.
    assert!(usage(&mut r, SID, None, 1, "claude-sonnet-5-5", 1_000_000, 0).is_empty());
    usage(&mut r, SID, None, 2, "claude-sonnet-5-5", 500_000, 0);
    assert!((r.colony.agents[SID].cost_usd - 3.5).abs() < 1e-9);

    // Usage is not agent activity.
    assert_eq!(r.state(SID), AgentState::Spawning);
}

#[test]
fn unknown_models_count_tokens_but_not_cost_and_say_partial() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup"}));
    usage(&mut r, SID, None, 1, "claude-opus-5-5", 1_000_000, 0);
    usage(&mut r, SID, None, 2, "some-future-model", 1_000, 0);
    let a = &r.colony.agents[SID];
    assert_eq!(a.tokens.input, 1_001_000);
    assert!((a.cost_usd - 4.0).abs() < 1e-9);
    assert!(a.cost_partial);
    let ledger = r.colony.spend.values().next().unwrap();
    assert!(ledger.buckets.values().any(|b| b.partial));
}

#[test]
fn ledger_outlives_the_agent_and_feeds_the_project_rollup() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup"}));
    usage(&mut r, SID, None, 1, "claude-sonnet-5-5", 1_000_000, 0);
    // A session that is not on the map still counts toward its project,
    // but does not conjure an agent.
    usage(&mut r, "other-session", None, 1, "claude-sonnet-5-5", 1_000_000, 0);
    assert!(!r.colony.agents.contains_key("other-session"));
    assert_eq!(r.colony.spend.len(), 1, "same project");
    let total: f64 = r.colony.spend["c:/users/thoma/code/bingosync"].buckets.values().map(|b| b.cost_usd).sum();
    assert!((total - 4.0).abs() < 1e-9);

    let changes = r.colony.take_spend_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].1.name, "bingosync");
    assert!(r.colony.take_spend_changes().is_empty());

    // Dismissing the session does not erase what it spent.
    r.colony.dismiss(SID, r.t);
    assert!(!r.colony.spend.is_empty());
    // Old buckets are dropped; recent ones stay.
    r.colony.tick(r.t + 47 * 3_600_000);
    assert!(!r.colony.spend.is_empty());
    r.colony.tick(r.t + 49 * 3_600_000);
    assert!(r.colony.spend.is_empty());
}

#[test]
fn agents_from_older_daemons_parse_without_usage_fields() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup"}));
    let mut v = serde_json::to_value(&r.colony.agents[SID]).unwrap();
    for k in ["tokens", "cost_usd", "cost_partial"] {
        v.as_object_mut().unwrap().remove(k);
    }
    let a: colony_core::Agent = serde_json::from_value(v).unwrap();
    assert_eq!(a.cost_usd, 0.0);
    assert!(a.tokens.is_zero());
}

// ---- where agents work ------------------------------------------------------

const SID2: &str = "8f1c2d3e-0000-4000-8000-000000000002";
const REPO: &str = "/mnt/c/Users/thoma/code/bingosync";

/// A tool call that starts and, for the edit tools, finishes successfully.
fn edit(r: &mut Run, sid: &str, tool: &str, rel: &str) {
    let id = format!("t{}", r.t);
    let input = json!({"file_path": format!("{REPO}/{rel}")});
    r.hook(json!({"hook_event_name": "PreToolUse", "session_id": sid, "tool_name": tool, "tool_input": input, "tool_use_id": id}));
    r.hook(json!({"hook_event_name": "PostToolUse", "session_id": sid, "tool_name": tool, "tool_input": input, "tool_use_id": id}));
}

fn two_sessions() -> Run {
    let mut r = Run::new(HostId::Wsl("Ubuntu".into()));
    for sid in [SID, SID2] {
        r.hook(json!({"hook_event_name": "SessionStart", "session_id": sid, "source": "startup"}));
        r.hook(json!({"hook_event_name": "UserPromptSubmit", "session_id": sid, "prompt": "go"}));
    }
    r
}

#[test]
fn work_dir_follows_file_targets_rolled_up() {
    let mut r = two_sessions();
    edit(&mut r, SID, "Edit", "src/auth/jwt/sign.ts");
    assert_eq!(r.colony.agents[SID].work_dir.as_deref(), Some("src/auth"));
    edit(&mut r, SID, "Read", "README.md");
    assert_eq!(r.colony.agents[SID].work_dir.as_deref(), Some(""));
    // A command is not a place: the bot stays where it was.
    r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "/usr/bin/ls /tmp/x/y"}}));
    assert_eq!(r.colony.agents[SID].work_dir.as_deref(), Some(""));
    // Outside the project: ignored.
    r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Read", "tool_input": {"file_path": "/etc/hosts"}}));
    assert_eq!(r.colony.agents[SID].work_dir.as_deref(), Some(""));
    // A subagent places itself on its own.
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "Explore"}));
    r.hook(json!({"hook_event_name": "PreToolUse", "agent_id": "a1", "tool_name": "Grep",
                  "tool_input": {"pattern": "x", "path": format!("{REPO}/crates/core/src")}}));
    assert_eq!(r.colony.agents[&sub_id(SID, "a1")].work_dir.as_deref(), Some("crates/core"));
    assert_eq!(r.colony.agents[SID].work_dir.as_deref(), Some(""));
}

#[test]
fn same_file_edits_warn_both_then_decay() {
    let mut r = two_sessions();
    edit(&mut r, SID, "Edit", "src/a.ts");
    assert!(r.colony.agents[SID].collision.is_none());
    edit(&mut r, SID2, "Write", "src/a.ts");
    let c1 = r.colony.agents[SID].collision.clone().expect("first agent warned");
    let c2 = r.colony.agents[SID2].collision.clone().expect("second agent warned");
    assert_eq!(c1.scope, colony_core::workdir::CollisionScope::File);
    assert_eq!(c1.with, vec![SID2.to_string()]);
    assert_eq!(c2.with, vec![SID.to_string()]);
    // Never a state change.
    assert_eq!(r.state(SID), AgentState::Working);
    assert_eq!(r.state(SID2), AgentState::Working);
    // Fades once nobody keeps editing.
    let changed = r.colony.tick(r.t + colony_core::workdir::FILE_WINDOW_MS + 1);
    assert!(changed.contains(&SID.to_string()));
    assert!(r.colony.agents[SID].collision.is_none());
    assert!(r.colony.agents[SID2].collision.is_none());
}

#[test]
fn reads_and_one_agents_repeat_edits_do_not_warn() {
    let mut r = two_sessions();
    edit(&mut r, SID, "Read", "src/a.ts");
    edit(&mut r, SID2, "Read", "src/a.ts");
    edit(&mut r, SID, "Edit", "src/b.ts");
    edit(&mut r, SID, "Edit", "src/b.ts");
    edit(&mut r, SID2, "Read", "src/b.ts");
    assert!(r.colony.agents[SID].collision.is_none());
    assert!(r.colony.agents[SID2].collision.is_none());
}

#[test]
fn different_files_in_one_folder_warn_softly_and_quickly_fade() {
    let mut r = two_sessions();
    edit(&mut r, SID, "Edit", "src/a.ts");
    edit(&mut r, SID2, "Edit", "src/b.ts");
    let c = r.colony.agents[SID].collision.clone().expect("folder warning");
    assert_eq!(c.scope, colony_core::workdir::CollisionScope::Dir);
    assert!(c.path.ends_with("/src"), "{}", c.path);
    r.colony.tick(r.t + colony_core::workdir::DIR_WINDOW_MS + 1);
    assert!(r.colony.agents[SID].collision.is_none());
}

#[test]
fn a_main_agent_and_its_subagent_do_not_collide() {
    let mut r = two_sessions();
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "general-purpose"}));
    edit(&mut r, SID, "Edit", "src/a.ts");
    let input = json!({"file_path": format!("{REPO}/src/a.ts")});
    for ev in ["PreToolUse", "PostToolUse"] {
        r.hook(json!({"hook_event_name": ev, "agent_id": "a1", "tool_name": "Edit", "tool_input": input, "tool_use_id": "u1"}));
    }
    assert!(r.colony.agents[SID].collision.is_none());
    assert!(r.colony.agents[&sub_id(SID, "a1")].collision.is_none());
}

#[test]
fn a_collision_ends_when_the_other_agent_does() {
    let mut r = two_sessions();
    edit(&mut r, SID, "Edit", "src/a.ts");
    edit(&mut r, SID2, "Edit", "src/a.ts");
    r.hook(json!({"hook_event_name": "SessionEnd", "session_id": SID2}));
    r.colony.tick(r.t + 1);
    assert!(r.colony.agents[SID].collision.is_none());
}

#[test]
fn diff_stat_applies_only_to_the_review_it_was_counted_for() {
    use colony_core::workdir::DiffStat;
    let mut r = two_sessions();
    let stat = DiffStat { files: 3, added: 40, removed: 7 };
    // Still working: nothing to attach it to.
    assert!(r.colony.set_diff_stat(SID, 0, stat).is_empty());
    r.hook(json!({"hook_event_name": "Stop", "last_assistant_message": "Done."}));
    let since = r.colony.agents[SID].state_since;
    assert_eq!(r.state(SID), AgentState::ReadyToReview);
    assert!(r.colony.set_diff_stat(SID, since + 5, stat).is_empty(), "stale count ignored");
    assert_eq!(r.colony.set_diff_stat(SID, since, stat), vec![SID.to_string()]);
    assert_eq!(r.colony.agents[SID].diff_stat, Some(stat));
    // Back to work: the count no longer describes it.
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "also this"}));
    assert_eq!(r.colony.agents[SID].diff_stat, None);
}

#[test]
fn agents_from_older_daemons_parse_without_work_fields() {
    let mut r = Run::new(HostId::Windows);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "startup"}));
    let mut v = serde_json::to_value(&r.colony.agents[SID]).unwrap();
    for k in ["work_dir", "collision", "diff_stat"] {
        v.as_object_mut().unwrap().remove(k);
    }
    let a: colony_core::Agent = serde_json::from_value(v).unwrap();
    assert!(a.work_dir.is_none() && a.collision.is_none() && a.diff_stat.is_none());
}

#[test]
fn denied_or_failed_edits_do_not_collide() {
    let mut r = two_sessions();
    edit(&mut r, SID, "Edit", "src/a.ts");
    let input = json!({"file_path": format!("{REPO}/src/a.ts")});
    r.hook(json!({"hook_event_name": "PreToolUse", "session_id": SID2, "tool_name": "Edit", "tool_input": input, "tool_use_id": "x1"}));
    r.hook(json!({"hook_event_name": "PostToolUseFailure", "session_id": SID2, "tool_name": "Edit", "tool_input": input, "tool_use_id": "x1", "error": "denied"}));
    assert!(r.colony.agents[SID].collision.is_none());
    assert!(r.colony.agents[SID2].collision.is_none());
    // A started edit that hasn't finished yet isn't one either.
    r.hook(json!({"hook_event_name": "PreToolUse", "session_id": SID2, "tool_name": "Edit", "tool_input": input, "tool_use_id": "x2"}));
    assert!(r.colony.agents[SID].collision.is_none());
}

#[test]
fn sibling_subagents_in_one_folder_do_not_warn_but_one_file_does() {
    let mut r = two_sessions();
    for a in ["a1", "a2"] {
        r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": a, "agent_type": "general-purpose"}));
    }
    let go = |r: &mut Run, a: &str, f: &str| {
        let input = json!({"file_path": format!("{REPO}/src/{f}")});
        for ev in ["PreToolUse", "PostToolUse"] {
            r.hook(json!({"hook_event_name": ev, "agent_id": a, "tool_name": "Edit", "tool_input": input, "tool_use_id": format!("{a}{f}")}));
        }
    };
    go(&mut r, "a1", "x.ts");
    go(&mut r, "a2", "y.ts");
    assert!(r.colony.agents[&sub_id(SID, "a1")].collision.is_none());
    go(&mut r, "a2", "x.ts");
    assert!(r.colony.agents[&sub_id(SID, "a1")].collision.is_some());
}
