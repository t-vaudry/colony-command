//! Crew health: whether a bot uses its crew. Delegation versus searching itself, swarm agents
//! running out of turns, advisor use by subagents, and whether nudges are followed.

use colony_core::state::{sub_id, Colony};
use colony_core::swarm::max_turns;
use colony_core::usage::Tokens;
use colony_core::{DomainEvent, Envelope, HookPayload, HostId};
use serde_json::json;

const SID: &str = "8f1c2d3e-0000-4000-8000-000000000009";
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

    fn tool(&mut self, tool: &str, input: serde_json::Value) {
        self.hook(json!({"hook_event_name": "PreToolUse", "tool_name": tool, "tool_input": input}));
    }

    fn start(&mut self, id: &str, kind: &str) {
        self.hook(json!({"hook_event_name": "SubagentStart", "agent_id": id, "agent_type": kind}));
    }

    fn stop(&mut self, id: &str) {
        self.hook(json!({"hook_event_name": "SubagentStop", "agent_id": id, "agent_type": "x"}));
    }

    fn nudge(&mut self, kind: &str) {
        self.hook(json!({"hook_event_name": "ColonyNudge", "kind": kind, "ts": self.t}));
    }

    /// `n` assistant messages for an agent (None: the main session).
    fn messages(&mut self, agent: Option<&str>, n: u32) {
        for _ in 0..n {
            self.usage(agent, false, 0);
        }
    }

    fn consult(&mut self, agent: Option<&str>) {
        self.usage(agent, true, 1);
    }

    fn usage(&mut self, agent: Option<&str>, advisor: bool, consults: u32) {
        self.t += 1_000;
        self.seq += 1;
        self.colony.apply(&Envelope {
            ts: self.t,
            host: HostId::Windows,
            session_id: SID.into(),
            cwd: Some(CWD.into()),
            event: DomainEvent::UsageUpdated {
                agent_id: agent.map(str::to_string),
                seq: self.seq,
                model: Some(if advisor { "claude-opus-5-5" } else { "claude-haiku-5-5" }.into()),
                tokens: Tokens { input: 10, output: 10, ..Default::default() },
                advisor,
                consults,
                advisor_model: None,
            },
        });
    }

    fn main(&self) -> &colony_core::Agent {
        &self.colony.agents[SID]
    }

    fn sub(&self, id: &str) -> &colony_core::Agent {
        &self.colony.agents[&sub_id(SID, id)]
    }
}

fn n(list: &[colony_core::state::Count], name: &str) -> u32 {
    list.iter().find(|c| c.name == name).map_or(0, |c| c.n)
}

#[test]
fn searches_done_by_hand_and_swarm_agents_started_are_counted() {
    let mut r = Run::new();
    r.tool("Grep", json!({"pattern": "foo"}));
    r.tool("Read", json!({"file_path": "a.rs"}));
    r.tool("Bash", json!({"command": "cd src && rg --files | head"}));
    r.tool("PowerShell", json!({"command": "Get-ChildItem -Recurse"}));
    r.tool("Bash", json!({"command": "cargo test"}));
    r.tool("Edit", json!({"file_path": "a.rs"}));
    r.start("s1", "swarm-search");
    r.start("s2", "swarm-search");
    r.start("g1", "swarm-grep");
    r.start("x1", "Explore");
    // A subagent searching is not the main session doing it itself.
    r.hook(json!({"hook_event_name": "PreToolUse", "agent_id": "s1", "tool_name": "Grep", "tool_input": {"pattern": "x"}}));

    let h = &r.main().crew_health;
    assert_eq!(h.self_searches, 4);
    assert_eq!((n(&h.delegated, "swarm-search"), n(&h.delegated, "swarm-grep"), h.delegations()), (2, 1, 3), "only swarm agents count as delegated");
}

#[test]
fn a_swarm_agent_that_uses_all_its_turns_is_flagged() {
    let mut r = Run::new();
    r.start("g1", "swarm-grep");
    assert_eq!(r.sub("g1").max_turns, Some(max_turns("swarm-grep")));
    r.messages(Some("g1"), 10);
    r.stop("g1");
    assert!(r.sub("g1").hit_max_turns);
    assert_eq!(r.sub("g1").turns, 10);

    r.start("s1", "swarm-search");
    r.messages(Some("s1"), 4);
    r.stop("s1");
    assert!(!r.sub("s1").hit_max_turns);

    // Not a swarm agent: no limit known, never flagged.
    r.start("e1", "Explore");
    r.messages(Some("e1"), 40);
    r.stop("e1");
    assert_eq!((r.sub("e1").max_turns, r.sub("e1").hit_max_turns), (None, false));
}

#[test]
fn advisor_consults_are_told_apart_by_who_made_them() {
    let mut r = Run::new();
    r.start("g1", "swarm-grep");
    r.consult(None);
    r.consult(Some("g1"));
    r.consult(Some("g1"));
    let h = &r.main().crew_health;
    assert_eq!(h.main_consults, 1);
    assert_eq!(n(&h.sub_consults, "swarm-grep"), 2);
    assert_eq!(r.main().consults, 3, "the session's own total still includes its subagents");
    assert_eq!(r.sub("g1").consults, 2);
}

#[test]
fn a_nudge_is_followed_by_a_consult_before_the_next_prompt() {
    let mut r = Run::new();
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    r.nudge("twice");
    r.consult(None);
    r.nudge("plan");
    r.hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "next"}));
    r.consult(None); // too late for the plan nudge
    r.nudge("done");
    let stat = |k: &str| r.main().crew_health.nudges.iter().find(|s| s.kind == k).map(|s| (s.sent, s.followed));
    assert_eq!(stat("twice"), Some((1, 1)));
    assert_eq!(stat("plan"), Some((1, 0)));
    assert_eq!(stat("done"), Some((1, 0)));
}

#[test]
fn one_consult_answers_every_open_nudge() {
    let mut r = Run::new();
    r.nudge("twice");
    r.nudge("done");
    r.consult(None);
    let h = &r.main().crew_health;
    assert!(h.nudges.iter().all(|s| s.followed == 1) && h.open_nudges.is_empty());
}

#[test]
fn a_subagent_consult_does_not_follow_a_nudge() {
    let mut r = Run::new();
    r.start("g1", "swarm-grep");
    r.nudge("twice");
    r.consult(Some("g1"));
    assert_eq!(r.main().crew_health.nudges[0].followed, 0);
}

#[test]
fn a_done_nudge_is_also_followed_by_an_audit_but_other_kinds_are_not() {
    let mut r = Run::new();
    r.nudge("done");
    r.nudge("plan");
    r.start("a1", "swarm-audit");
    let h = &r.main().crew_health;
    let followed = |k: &str| h.nudges.iter().find(|s| s.kind == k).unwrap().followed;
    assert_eq!((followed("done"), followed("plan")), (1, 0));
    assert_eq!(h.open_nudges, vec!["plan".to_string()]);
    assert_eq!(n(&h.delegated, "swarm-audit"), 1);
}

#[test]
fn unknown_nudge_kinds_are_kept_and_nudges_are_not_activity() {
    let mut r = Run::new();
    r.nudge("from-the-future");
    assert_eq!(r.main().crew_health.nudges[0].kind, "from-the-future");
    // A kindless record is ignored.
    r.hook(json!({"hook_event_name": "ColonyNudge"}));
    assert_eq!(r.main().crew_health.nudges.len(), 1);
}

#[test]
fn the_crew_is_set_by_colonyd() {
    let mut r = Run::new();
    r.colony.apply(&Envelope {
        ts: 5_000_000,
        host: HostId::Windows,
        session_id: SID.into(),
        cwd: None,
        event: DomainEvent::CrewSet {
            id: "default".into(),
            label: "Sonnet builds".into(),
            model: Some("sonnet".into()),
            advisor: Some("opus".into()),
            swarm_model: Some("haiku".into()),
        },
    });
    let c = r.main().crew.as_ref().unwrap();
    assert_eq!((c.id.as_str(), c.advisor.as_deref(), c.swarm_model.as_deref()), ("default", Some("opus"), Some("haiku")));
}

#[test]
fn old_snapshots_without_crew_fields_still_load() {
    let r = Run::new();
    let mut v = serde_json::to_value(r.main()).unwrap();
    for k in ["crew", "crew_health", "turns", "max_turns", "hit_max_turns"] {
        v.as_object_mut().unwrap().remove(k);
    }
    let a: colony_core::Agent = serde_json::from_value(v).unwrap();
    assert!(a.crew.is_none() && a.crew_health.is_empty() && a.turns == 0);
}
