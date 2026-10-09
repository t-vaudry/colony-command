//! State quality: stall limits, CPU-aware long tools, tunable thresholds, and
//! the reason/basis that explains a state.

use colony_core::state::{
    AgentState, Colony, QuestionVerdict, Thresholds, CPU_ACTIVE_GRACE_MS, STALL_MS, TOOL_STALL_MS,
};
use colony_core::{Envelope, HookPayload, HostId};
use serde_json::json;

const SID: &str = "8f1c2d3e-0000-4000-8000-000000000002";

struct Run {
    colony: Colony,
    t: u64,
}

impl Run {
    fn new() -> Self {
        Run { colony: Colony::new(), t: 1_000_000 }
    }

    fn hook(&mut self, mut v: serde_json::Value) {
        self.t += 1_000;
        let obj = v.as_object_mut().unwrap();
        obj.entry("session_id").or_insert(json!(SID));
        obj.entry("cwd").or_insert(json!("C:\\code\\app"));
        let p = HookPayload::parse(&v.to_string()).expect("payload parses");
        if let Some(e) = Envelope::from_hook(HostId::Windows, self.t, &p) {
            self.colony.apply(&e);
        }
    }

    fn state(&self) -> AgentState {
        self.colony.agents[SID].state
    }

    fn bash_running(&mut self) {
        self.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "run the full build"}));
        self.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_use_id": "b1",
                         "tool_input": {"command": "cargo build --release"}}));
    }

    fn stop(&mut self, message: &str) {
        self.hook(json!({"hook_event_name": "Stop", "last_assistant_message": message}));
    }
}

#[test]
fn a_long_bash_with_cpu_activity_is_not_a_stall() {
    let mut r = Run::new();
    r.bash_running();
    let late = r.t + TOOL_STALL_MS + 1;
    // Its processes were using CPU a moment ago.
    r.colony.note_cpu_active(SID, late - 1_000);
    r.colony.tick(late);
    assert_eq!(r.state(), AgentState::Working);

    // The CPU goes quiet: after the grace period it counts as stuck, and says why.
    r.colony.tick(late + CPU_ACTIVE_GRACE_MS + 1);
    assert_eq!(r.state(), AgentState::Blocked);
    let a = &r.colony.agents[SID];
    assert!(a.reason.as_deref().unwrap().starts_with("Bash running for"));
    assert!(a.basis.as_deref().unwrap().contains("not using CPU"));
}

#[test]
fn a_long_bash_without_cpu_reports_is_a_stall_as_before() {
    let mut r = Run::new();
    r.bash_running();
    let t = r.t;
    r.colony.tick(t + TOOL_STALL_MS - 1);
    assert_eq!(r.state(), AgentState::Working);
    r.colony.tick(t + TOOL_STALL_MS + 1);
    assert_eq!(r.state(), AgentState::Blocked);
}

#[test]
fn cpu_activity_does_not_excuse_silence_with_no_tool_running() {
    let mut r = Run::new();
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "think"}));
    let t = r.t;
    r.colony.note_cpu_active(SID, t + STALL_MS);
    r.colony.tick(t + STALL_MS + 1);
    assert_eq!(r.state(), AgentState::Blocked);
    assert!(r.colony.agents[SID].basis.as_deref().unwrap().contains("no tool running"));
}

#[test]
fn thresholds_come_from_config_and_defaults_are_unchanged() {
    let d = Thresholds::default();
    assert_eq!((d.stall_ms, d.tool_stall_ms, d.question_tail_chars), (STALL_MS, TOOL_STALL_MS, 400));
    assert_eq!((d.question_trailing_chars, d.question_min_chars), (160, 6));
    // Partial files keep defaults for what they leave out; junk is ignored.
    let t = Thresholds::from_json(r#"{"tool_stall_ms": 120000}"#);
    assert_eq!((t.stall_ms, t.tool_stall_ms), (STALL_MS, 120_000));
    assert_eq!(Thresholds::from_json("not json"), d);
    assert_eq!(Thresholds::from_json(r#"{"stall_ms": 5}"#).stall_ms, STALL_MS);

    let mut r = Run::new();
    r.colony.thresholds = t;
    r.bash_running();
    let at = r.t;
    r.colony.tick(at + 120_001);
    assert_eq!(r.state(), AgentState::Blocked);
}

#[test]
fn the_question_window_is_tunable() {
    let msg = format!("Should it go in the cache? {}", "Notes follow here. ".repeat(14));
    assert_eq!(colony_core::state::question_in(&msg), None);
    let wide = Thresholds { question_trailing_chars: 400, question_tail_chars: 600, ..Thresholds::default() };
    assert!(colony_core::state::question_in_with(&msg, &wide).is_some());
}

#[test]
fn a_turn_ending_records_which_rule_decided() {
    let mut r = Run::new();
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    r.stop("Which table should hold it?");
    assert_eq!(r.state(), AgentState::AwaitingReply);
    assert!(r.colony.agents[SID].basis.as_deref().unwrap().contains("question mark"));
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "orders"}));
    assert!(r.colony.agents[SID].basis.is_none());
}

#[test]
fn an_unclear_ending_can_be_flipped_by_a_verdict() {
    let mut r = Run::new();
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    // No question mark, but it asks in words.
    r.stop("Done with the parser. Let me know if you want it split into two crates.");
    assert_eq!(r.state(), AgentState::ReadyToReview);
    let (since, msg) = r.colony.take_ambiguous(SID).expect("flagged");
    assert!(msg.contains("Let me know"));
    assert!(r.colony.take_ambiguous(SID).is_none(), "taken once");

    let ask = QuestionVerdict { asks_user: true, question: Some("Split it into two crates?".into()) };
    // A verdict for a state that has since moved on is dropped.
    assert!(r.colony.apply_verdict(SID, since + 1, &ask).is_empty());
    assert_eq!(r.colony.apply_verdict(SID, since, &ask), vec![SID.to_string()]);
    let a = &r.colony.agents[SID];
    assert_eq!(a.state, AgentState::AwaitingReply);
    assert_eq!(a.reason.as_deref(), Some("Split it into two crates?"));
    assert_eq!(a.state_since, since, "the wait is not restarted");
    assert!(a.basis.as_deref().unwrap().contains("Haiku"));
}

#[test]
fn a_clear_ending_is_never_sent_to_a_classifier() {
    let mut r = Run::new();
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    r.stop("Fixed the bug and all 42 tests pass.");
    assert!(r.colony.take_ambiguous(SID).is_none());
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "ok"}));
    r.stop("Which table should hold it?");
    assert!(r.colony.take_ambiguous(SID).is_none());
}

#[test]
fn a_question_mark_followed_by_a_wrap_up_is_unclear_and_can_flip_to_done() {
    let mut r = Run::new();
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "ok"}));
    r.stop("Is this what you meant? Either way the change is merged.");
    assert_eq!(r.state(), AgentState::AwaitingReply);
    let (since, _) = r.colony.take_ambiguous(SID).expect("flagged");
    let no = QuestionVerdict { asks_user: false, question: None };
    r.colony.apply_verdict(SID, since, &no);
    assert_eq!(r.state(), AgentState::ReadyToReview);
    assert_eq!(r.colony.agents[SID].state_since, since);
}
