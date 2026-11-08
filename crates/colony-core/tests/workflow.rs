//! Workflow tracking: Skill calls move a session through its phases, and
//! subagents are filed under the phase that was running when they started.

use colony_core::state::{sub_id, Colony};
use colony_core::workflow;
use colony_core::{DomainEvent, Envelope, HookPayload, HostId};
use serde_json::json;

const SID: &str = "8f1c2d3e-0000-4000-8000-0000000000aa";
const CWD: &str = "C:\\code\\api";

struct Run {
    colony: Colony,
    t: u64,
}

impl Run {
    fn new(with_workflow: bool) -> Self {
        let mut r = Run { colony: Colony::new(), t: 1_000_000 };
        r.hook(json!({"hook_event_name": "SessionStart", "source": "startup", "model": "claude-sonnet-5-5"}));
        if with_workflow {
            let w = workflow::parse(
                "feature",
                r#"{"label":"Feature","phases":[{"skill":"plan","next":["build"]},{"skill":"build","next":["review"]},{"skill":"review"}]}"#,
            )
            .unwrap();
            r.t += 1_000;
            r.colony.apply(&Envelope { ts: r.t, host: HostId::Windows, session_id: SID.into(), cwd: None, event: DomainEvent::WorkflowSet { workflow: w } });
        }
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

    fn skill(&mut self, name: &str) {
        self.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Skill", "tool_input": {"skill": name, "args": "x"}}));
    }

    fn start(&mut self, id: &str) {
        self.hook(json!({"hook_event_name": "SubagentStart", "agent_id": id, "agent_type": "swarm-search"}));
    }

    fn info(&self) -> &colony_core::state::WorkflowInfo {
        self.colony.agents.get(SID).and_then(|a| a.workflow.as_ref()).expect("workflow set")
    }
}

#[test]
fn skill_calls_walk_the_phases() {
    let mut r = Run::new(true);
    assert_eq!(r.info().active, None);
    r.skill("plan");
    r.skill("plan"); // the same phase again is not a new visit
    r.skill("not-a-phase");
    r.skill("build");
    r.skill("plan"); // back again
    let path: Vec<_> = r.info().visits.iter().map(|v| v.skill.as_str()).collect();
    assert_eq!(path, ["plan", "build", "plan"]);
    assert_eq!(r.info().active.as_deref(), Some("plan"));
    let v = &r.info().visits;
    assert!(v[0].ended_at.is_some() && v[1].ended_at.is_some() && v[2].ended_at.is_none());
}

#[test]
fn subagents_are_filed_under_the_running_phase() {
    let mut r = Run::new(true);
    r.start("a0"); // before any phase
    r.skill("build");
    r.start("a1");
    r.start("a2");
    r.skill("review");
    r.start("a3");
    let phase = |id: &str| r.colony.agents.get(&sub_id(SID, id)).and_then(|a| a.phase.clone());
    assert_eq!(phase("a0"), None);
    assert_eq!(phase("a1").as_deref(), Some("build"));
    assert_eq!(phase("a2").as_deref(), Some("build"));
    assert_eq!(phase("a3").as_deref(), Some("review"));
    let counts: Vec<_> = r.info().visits.iter().map(|v| v.subagents).collect();
    assert_eq!(counts, [2, 1]);
}

#[test]
fn a_custom_phase_starts_with_its_marker_command() {
    let mut r = Run::new(false);
    let w = workflow::parse(
        "bug",
        r#"{"phases":[{"skill":"triage","instruction":"Decide how bad it is."},{"skill":"build"}],"loops":[{"from":"triage","to":"build"}]}"#,
    )
    .unwrap();
    r.t += 1_000;
    r.colony.apply(&Envelope { ts: r.t, host: HostId::Windows, session_id: SID.into(), cwd: None, event: DomainEvent::WorkflowSet { workflow: w } });
    let bash = |r: &mut Run, command: &str| r.hook(json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": command}}));
    bash(&mut r, "echo hello");
    bash(&mut r, "cargo test");
    assert!(r.info().visits.is_empty());
    bash(&mut r, "echo colony-phase:triage");
    r.start("a1");
    r.skill("build");
    bash(&mut r, "echo colony-phase:triage");
    let path: Vec<_> = r.info().visits.iter().map(|v| v.skill.as_str()).collect();
    assert_eq!(path, ["triage", "build", "triage"]);
    assert_eq!(r.info().visits[0].subagents, 1);
}

#[test]
fn a_session_without_a_workflow_ignores_skill_calls() {
    let mut r = Run::new(false);
    r.skill("plan");
    r.start("a1");
    let a = r.colony.agents.get(SID).unwrap();
    assert!(a.workflow.is_none());
    assert_eq!(r.colony.agents.get(&sub_id(SID, "a1")).and_then(|a| a.phase.clone()), None);
}

#[test]
fn a_subagents_skill_call_does_not_move_the_main_session() {
    let mut r = Run::new(true);
    r.start("a1");
    r.hook(json!({"hook_event_name": "PreToolUse", "agent_id": "a1", "tool_name": "Skill", "tool_input": {"skill": "build"}}));
    assert!(r.info().visits.is_empty());
}

#[test]
fn a_native_workflow_agent_announcing_its_phase_moves_the_workflow_and_tags_the_agent() {
    use colony_core::{DomainEvent, Envelope};
    let wf = colony_core::workflow::parse("f", r#"{"phases":[{"skill":"Investigate","instruction":"Look."},{"skill":"Develop","instruction":"Make."}]}"#).unwrap();
    let ev = |ts, event| Envelope { ts, host: colony_core::HostId::Windows, session_id: "s".into(), cwd: None, event };
    let mut c = colony_core::Colony::new();
    for e in [
        ev(1, DomainEvent::SessionStarted { source: None, model: None }),
        ev(2, DomainEvent::WorkflowSet { workflow: wf }),
        ev(3, DomainEvent::SubagentStarted { agent_id: "a1".into(), agent_type: Some("workflow-subagent".into()) }),
        ev(4, DomainEvent::ToolStarted { agent_id: Some("a1".into()), tool: "Bash".into(), target: Some("echo colony-phase:Investigate; ls notes".into()), tool_use_id: None, background: false }),
        ev(5, DomainEvent::SubagentStarted { agent_id: "a2".into(), agent_type: Some("workflow-subagent".into()) }),
        ev(6, DomainEvent::ToolStarted { agent_id: Some("a2".into()), tool: "Bash".into(), target: Some("echo colony-phase:Develop".into()), tool_use_id: None, background: false }),
        // A subagent's Skill call is still not a phase.
        ev(7, DomainEvent::ToolStarted { agent_id: Some("a2".into()), tool: "Skill".into(), target: Some("Investigate".into()), tool_use_id: None, background: false }),
    ] {
        c.apply(&e);
    }
    let w = c.agents.get("s").and_then(|a| a.workflow.as_ref()).unwrap();
    assert_eq!(w.visits.iter().map(|v| v.skill.as_str()).collect::<Vec<_>>(), ["Investigate", "Develop"]);
    assert_eq!(w.active.as_deref(), Some("Develop"));
    assert_eq!(c.agents.get("s/a1").and_then(|a| a.phase.as_deref()), Some("Investigate"));
    assert_eq!(c.agents.get("s/a2").and_then(|a| a.phase.as_deref()), Some("Develop"));
}
