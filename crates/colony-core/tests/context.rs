//! Context size per agent: the newest response replaces the old size (never a max), and
//! compacting or clearing resets it.

use colony_core::state::{sub_id, Colony};
use colony_core::{DomainEvent, Envelope, HookPayload, HostId, Tokens};
use serde_json::json;

const SID: &str = "8f1c2d3e-0000-4000-8000-000000000004";
const CWD: &str = "C:\\code\\api";

struct Run {
    colony: Colony,
    t: u64,
    seq: u64,
}

impl Run {
    fn new() -> Self {
        let mut r = Run { colony: Colony::new(), t: 1_000_000, seq: 0 };
        r.hook(json!({"hook_event_name": "SessionStart", "source": "startup", "model": "claude-sonnet-5-5"}));
        r
    }

    fn hook(&mut self, mut v: serde_json::Value) {
        self.t += 1_000;
        let obj = v.as_object_mut().unwrap();
        obj.entry("session_id").or_insert(json!(SID));
        obj.entry("cwd").or_insert(json!(CWD));
        let p = HookPayload::parse(&v.to_string()).expect("payload parses");
        if let Some(e) = Envelope::from_hook(HostId::Windows, self.t, &p) {
            self.colony.apply(&e);
        }
    }

    /// A response whose input side is `context` tokens, usage message at time `ts` (or now).
    fn usage_at(&mut self, ts: Option<u64>, agent: Option<&str>, context: u64, advisor: bool) {
        self.t += 1_000;
        self.seq += 1;
        self.colony.apply(&Envelope {
            ts: ts.unwrap_or(self.t),
            host: HostId::Windows,
            session_id: SID.into(),
            cwd: Some(CWD.into()),
            event: DomainEvent::UsageUpdated {
                agent_id: agent.map(str::to_string),
                seq: self.seq,
                model: Some("claude-sonnet-5-5".into()),
                tokens: Tokens { input: context, output: 10, ..Default::default() },
                advisor,
                consults: u32::from(advisor),
                advisor_model: None,
                new_message: false,
                context: if advisor { 0 } else { context },
            },
        });
    }

    fn usage(&mut self, context: u64) {
        self.usage_at(None, None, context, false);
    }

    fn context(&self) -> u64 {
        self.colony.agents[SID].context_tokens
    }
}

#[test]
fn the_newest_response_sets_the_size_even_when_smaller() {
    let mut r = Run::new();
    assert_eq!(r.context(), 0);
    r.usage(40_000);
    assert_eq!(r.context(), 40_000);
    assert!(r.colony.agents[SID].context_at.is_some());
    r.usage(90_000);
    assert_eq!(r.context(), 90_000);
    // Smaller again, as after a summarization: replaces, does not keep the max.
    r.usage(25_000);
    assert_eq!(r.context(), 25_000);
}

#[test]
fn compacting_resets_until_the_next_response_and_stale_ones_are_ignored() {
    let mut r = Run::new();
    r.usage(150_000);
    let before_compact = r.t;
    r.hook(json!({"hook_event_name": "PreCompact", "trigger": "manual"}));
    // The summarizing call is a response too, written before the compaction is done.
    r.hook(json!({"hook_event_name": "PostCompact", "trigger": "manual"}));
    assert_eq!(r.context(), 0, "unknown until the next response");
    r.usage_at(Some(before_compact + 500), None, 150_000, false);
    assert_eq!(r.context(), 0, "a response written before the reset is stale");
    r.usage(18_000);
    assert_eq!(r.context(), 18_000);
}

#[test]
fn clear_and_compact_session_starts_reset_the_size() {
    let mut r = Run::new();
    r.usage(120_000);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "clear", "model": "claude-sonnet-5-5"}));
    assert_eq!(r.context(), 0);
    r.usage(9_000);
    r.hook(json!({"hook_event_name": "SessionStart", "source": "compact", "model": "claude-sonnet-5-5"}));
    assert_eq!(r.context(), 0);
}

#[test]
fn subagents_have_their_own_size_and_advisors_do_not_count() {
    let mut r = Run::new();
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "a1", "agent_type": "swarm-search"}));
    r.usage(60_000);
    r.usage_at(None, Some("a1"), 12_000, false);
    assert_eq!(r.context(), 60_000, "a subagent's size is not the main one's");
    assert_eq!(r.colony.agents[&sub_id(SID, "a1")].context_tokens, 12_000);
    r.usage_at(None, None, 500_000, true);
    assert_eq!(r.context(), 60_000, "advisor consultations don't touch it");
}

#[test]
fn replayed_messages_do_not_change_the_size() {
    let mut r = Run::new();
    r.usage(60_000);
    let seq = r.seq;
    r.colony.apply(&Envelope {
        ts: r.t + 10,
        host: HostId::Windows,
        session_id: SID.into(),
        cwd: Some(CWD.into()),
        event: DomainEvent::UsageUpdated {
            agent_id: None,
            seq,
            model: None,
            tokens: Tokens { input: 1, ..Default::default() },
            advisor: false,
            consults: 0,
            advisor_model: None,
            new_message: false,
            context: 999_000,
        },
    });
    assert_eq!(r.context(), 60_000);
}
