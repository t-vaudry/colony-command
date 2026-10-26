//! Per-model usage: tokens and cost split by model, advisor consultations
//! priced at the advisor's model, and the hint that follows the advisor.

use colony_core::state::{sub_id, ActivityKind, Colony};
use colony_core::usage::ModelCost;
use colony_core::{DomainEvent, Envelope, HookPayload, HostId, Tokens};
use serde_json::json;

const SID: &str = "8f1c2d3e-0000-4000-8000-000000000003";
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

    fn usage(&mut self, agent: Option<&str>, model: &str, input: u64, output: u64, advisor: bool, consults: u32, attached: Option<&str>) {
        self.t += 1_000;
        self.seq += 1;
        let tokens = Tokens { input, output, ..Default::default() };
        self.colony.apply(&Envelope {
            ts: self.t,
            host: HostId::Windows,
            session_id: SID.into(),
            cwd: Some(CWD.into()),
            event: DomainEvent::UsageUpdated {
                agent_id: agent.map(str::to_string),
                seq: self.seq,
                model: Some(model.into()),
                tokens,
                advisor,
                consults,
                advisor_model: attached.map(str::to_string),
            },
        });
    }
}

const M: u64 = 1_000_000;

fn of<'a>(v: &'a [ModelCost], key: &str) -> &'a ModelCost {
    v.iter().find(|m| m.model == key).unwrap_or_else(|| panic!("no {key} in {v:?}"))
}

#[test]
fn usage_is_split_by_model_and_advisor_tokens_cost_the_advisors_rate() {
    let mut r = Run::new();
    // 1M in + 1M out each: Sonnet 5.5 $2 + $10, Haiku 5.5 $0.10 + $0.50, Opus 5.5 $4 + $20.
    r.usage(None, "claude-sonnet-5-5", M, M, false, 0, Some("claude-opus-5-5"));
    r.hook(json!({"hook_event_name": "SubagentStart", "agent_id": "h1", "agent_type": "swarm-grep"}));
    r.usage(Some("h1"), "claude-haiku-5-5", M, M, false, 0, None);
    r.usage(None, "claude-opus-5-5", M, M, true, 1, Some("claude-opus-5-5"));
    r.usage(None, "claude-opus-5-5", M, M, true, 2, Some("claude-opus-5-5"));

    let main = &r.colony.agents[SID];
    let cost = |k: &str| of(&main.by_model, k).cost_usd;
    assert!((cost("sonnet") - 12.0).abs() < 1e-9);
    assert!((cost("haiku") - 0.6).abs() < 1e-9, "subagent usage counts toward its parent");
    assert!((cost("advisor:opus") - 48.0).abs() < 1e-9);
    assert!(!main.by_model.iter().any(|m| m.model == "opus"), "advisor tokens are not the main model's");
    assert_eq!(of(&main.by_model, "advisor:opus").consults, 3);
    assert_eq!((main.consults, main.advisor_model.as_deref()), (3, Some("claude-opus-5-5")));
    assert!((main.cost_usd - (12.0 + 0.6 + 48.0)).abs() < 1e-9);
    assert!(!main.cost_partial);

    // The subagent's own record has only its model, and shows it.
    let sub = &r.colony.agents[&sub_id(SID, "h1")];
    assert_eq!(sub.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(sub.by_model.iter().map(|m| m.model.as_str()).collect::<Vec<_>>(), vec!["haiku"]);
    assert_eq!(sub.consults, 0);
}

#[test]
fn the_ledger_splits_the_day_by_model_too() {
    let mut r = Run::new();
    r.usage(None, "claude-sonnet-5-5", M, 0, false, 0, None);
    r.usage(None, "claude-opus-5-5", M, 0, true, 1, None);
    let spend = r.colony.spend.values().next().unwrap().buckets.values().next().unwrap();
    assert!((spend.cost_usd - 6.0).abs() < 1e-9);
    assert!((spend.model("sonnet").unwrap().cost_usd - 2.0).abs() < 1e-9);
    assert!((spend.model("advisor:opus").unwrap().cost_usd - 4.0).abs() < 1e-9);
    assert_eq!(spend.model("advisor:opus").unwrap().consults, 1);
}

#[test]
fn an_unpriced_model_keeps_the_split_partial() {
    let mut r = Run::new();
    r.usage(None, "claude-mystery-9", 10, 10, false, 0, None);
    let main = &r.colony.agents[SID];
    assert!(main.cost_partial && of(&main.by_model, "other").partial);
    assert_eq!(of(&main.by_model, "other").tokens, 20);
}

#[test]
fn each_consult_is_an_activity_for_the_map() {
    let mut r = Run::new();
    r.usage(None, "claude-opus-5-5", 100, 10, true, 1, Some("claude-opus-5-5"));
    let main = &r.colony.agents[SID];
    let last = main.activity.last().unwrap();
    assert_eq!(last.kind, ActivityKind::Advisor);
    assert_eq!(last.text, "Consulted the opus advisor");
    assert_eq!(main.last_consult_at, Some(r.t));
    // A replay of the same usage is ignored, so the count doesn't move.
    let again = main.consults;
    let replay = Envelope {
        ts: r.t + 1,
        host: HostId::Windows,
        session_id: SID.into(),
        cwd: None,
        event: DomainEvent::UsageUpdated { agent_id: None, seq: 1, model: Some("claude-opus-5-5".into()), tokens: Tokens { input: 1, ..Default::default() }, advisor: true, consults: 1, advisor_model: None },
    };
    r.colony.apply(&replay);
    assert_eq!(r.colony.agents[SID].consults, again);
}

#[test]
fn an_opus_advisor_stops_the_plan_on_opus_hint() {
    let plan_heavy = |r: &mut Run| {
        for i in 0..4 {
            r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "TodoWrite", "tool_use_id": format!("p{i}"), "tool_input": {}}));
            r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Read", "tool_use_id": format!("r{i}"), "tool_input": {"file_path": "a.rs"}}));
        }
    };
    let mut plain = Run::new();
    plain.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "plan it"}));
    plan_heavy(&mut plain);
    assert_eq!(plain.colony.agents[SID].model_hint.as_ref().map(|h| h.model.as_str()), Some("opus"));

    // Same work, but the advisor shows up in the transcript: the hint goes away, and stays away.
    let mut advised = Run::new();
    advised.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "plan it"}));
    plan_heavy(&mut advised);
    advised.usage(None, "claude-sonnet-5-5", 10, 10, false, 0, Some("claude-opus-5-5"));
    assert!(advised.colony.agents[SID].model_hint.is_none());
    plan_heavy(&mut advised);
    assert!(advised.colony.agents[SID].model_hint.is_none());
}
