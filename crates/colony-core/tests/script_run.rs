//! Runs compiled workflow scripts under Node with stubbed `agent()`, to check the control flow:
//! skips, loops, branches and blocked phases. Skipped when Node isn't installed.

use std::path::PathBuf;
use std::process::Command;

use colony_core::script::{compile, Options};
use colony_core::workflow::parse;
use serde_json::{json, Value};

fn node() -> bool {
    Command::new("node").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn run(workflow: &str, opts: &Options, scenario: Value) -> Option<Value> {
    if !node() {
        eprintln!("node not found: skipping");
        return None;
    }
    let wf = parse("flow", workflow).unwrap();
    let script = compile(&wf, opts).unwrap();
    let dir = std::env::temp_dir().join(format!("colony-script-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file: PathBuf = dir.join("flow.js");
    std::fs::write(&file, &script).unwrap();
    let harness = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("script_harness.mjs");
    let out = Command::new("node").arg(&harness).arg(&file).arg(scenario.to_string()).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "script failed: {}\n{script}", String::from_utf8_lossy(&out.stderr));
    Some(serde_json::from_slice(&out.stdout).unwrap())
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v["result"][key].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
}

const LINEAR: &str = r#"{"phases":[{"skill":"Investigate","instruction":"Look."},{"skill":"Plan","instruction":"Plan it.","skip_when":"the change is one line"},{"skill":"Develop"},{"skill":"Publish","instruction":"Push."}]}"#;

#[test]
fn runs_the_phases_in_order_and_skips_an_optional_one_when_its_condition_holds() {
    let Some(r) = run(LINEAR, &Options { task: "fix it".into(), ..Default::default() }, json!({"skip": {"Plan": true}})) else { return };
    assert_eq!(names(&r, "ran"), ["Investigate", "Develop", "Publish"]);
    assert!(r["logs"][0].as_str().unwrap().contains("Skipping Plan"));
    let Some(r) = run(LINEAR, &Options::default(), json!({"skip": {}})) else { return };
    assert_eq!(names(&r, "ran"), ["Investigate", "Plan", "Develop", "Publish"]);
    // A required phase never gets a skip check.
    let labels: Vec<&str> = r["calls"].as_array().unwrap().iter().map(|c| c["label"].as_str().unwrap()).collect();
    assert_eq!(labels.iter().filter(|l| l.starts_with("skip?")).count(), 1);
}

const LOOPED: &str = r#"{"phases":[{"skill":"Investigate"},{"skill":"Develop"},{"skill":"Test"},{"skill":"Review"},{"skill":"Publish"}],"loops":[{"from":"Develop","to":"Review","until":"the review approved","max":3}]}"#;

#[test]
fn goes_round_a_loop_until_the_exit_condition_holds() {
    let Some(r) = run(LOOPED, &Options::default(), json!({"loop": [false, true]})) else { return };
    assert_eq!(names(&r, "ran"), ["Investigate", "Develop", "Test", "Review", "Develop", "Test", "Review", "Publish"]);
    assert_eq!(r["result"]["rounds"], json!([2]));
}

#[test]
fn a_loop_stops_at_its_maximum_even_when_never_satisfied() {
    let Some(r) = run(LOOPED, &Options::default(), json!({"loop": [false, false, false, false, false]})) else { return };
    let ran = names(&r, "ran");
    assert_eq!(ran.iter().filter(|n| *n == "Review").count(), 3);
    assert_eq!(ran.last().unwrap(), "Publish");
}

const FORKED: &str = r#"{"phases":[{"skill":"Triage","branches":[{"to":"Bisect","condition":"it is a regression"},{"to":"Develop","condition":"the cause is clear"},{"to":"Publish","condition":""}]},{"skill":"Bisect","next":["Develop"]},{"skill":"Develop","next":["Publish"]},{"skill":"Publish"}]}"#;

#[test]
fn takes_the_branch_whose_condition_holds_else_the_otherwise() {
    let Some(r) = run(FORKED, &Options::default(), json!({"branch": [1]})) else { return };
    assert_eq!(names(&r, "ran"), ["Triage", "Bisect", "Develop", "Publish"]);
    let Some(r) = run(FORKED, &Options::default(), json!({"branch": [2]})) else { return };
    assert_eq!(names(&r, "ran"), ["Triage", "Develop", "Publish"]);
    let Some(r) = run(FORKED, &Options::default(), json!({"branch": [0]})) else { return };
    assert_eq!(names(&r, "ran"), ["Triage", "Publish"]);
}

#[test]
fn a_blocked_phase_stops_the_run_and_resumes_with_the_answer() {
    let Some(r) = run(LINEAR, &Options::default(), json!({"skip": {}, "blocked": {"Plan": "Which database?"}})) else { return };
    assert_eq!(names(&r, "ran"), ["Investigate"]);
    assert_eq!(r["result"]["blocked"], json!({"phase": "Plan", "question": "Which database?"}));
    let Some(r) = run(LINEAR, &Options::default(), json!({"skip": {}, "blocked": {"Plan": "Which database?"}, "answers": {"Plan": "Postgres"}, "args": {"answers": {"Plan": "Postgres"}}})) else { return };
    assert_eq!(names(&r, "ran"), ["Investigate", "Plan", "Develop", "Publish"]);
    assert_eq!(r["result"]["blocked"], Value::Null);
}

#[test]
fn judges_use_the_crew_swarm_model_and_phase_agents_inherit() {
    let Some(r) = run(LINEAR, &Options { judge_model: Some("haiku".into()), ..Default::default() }, json!({"skip": {}})) else { return };
    for c in r["calls"].as_array().unwrap() {
        let judge = c["label"].as_str().unwrap().starts_with("skip?");
        assert_eq!(c["model"], if judge { json!("haiku") } else { Value::Null });
        assert_eq!(c["schema"], json!(judge));
    }
}

const GATED: &str = r#"{"phases":[{"skill":"Plan"},{"skill":"Build","approve":true,"approve_when":"the plan touches more than 5 files","on_deny":"Plan"},{"skill":"Publish"}]}"#;

#[test]
fn a_phase_needing_approval_stops_the_run_and_asks() {
    let Some(r) = run(GATED, &Options::default(), json!({"need": {}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan"]);
    assert_eq!(r["result"]["blocked"]["kind"], json!("approval"));
    assert_eq!(r["result"]["blocked"]["phase"], json!("Build"));
}

#[test]
fn an_approved_phase_runs_and_the_rest_follow() {
    let Some(r) = run(GATED, &Options::default(), json!({"args": {"approvals": {"Build": "approved"}}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan", "Build", "Publish"]);
    assert_eq!(r["result"]["blocked"], Value::Null);
}

#[test]
fn an_approval_whose_condition_does_not_hold_is_not_asked() {
    let Some(r) = run(GATED, &Options::default(), json!({"need": {"Build": false}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan", "Build", "Publish"]);
}

#[test]
fn a_denial_goes_to_the_named_phase_and_a_second_denial_ends_the_run() {
    // Plan -> (Build denied) -> Plan -> Build denied again: stop rather than go round for ever.
    let Some(r) = run(GATED, &Options::default(), json!({"args": {"approvals": {"Build": "denied: too risky"}}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan", "Plan"]);
    assert_eq!(names(&r, "denied"), ["Build"]);
    assert!(r["logs"][0].as_str().unwrap().contains("going to Plan"));
}

#[test]
fn a_denial_with_nowhere_to_go_ends_the_run() {
    let wf = r#"{"phases":[{"skill":"Plan"},{"skill":"Build","approve":true},{"skill":"Publish"}]}"#;
    let Some(r) = run(wf, &Options::default(), json!({"args": {"approvals": {"Build": "no"}}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan"]);
    assert_eq!(names(&r, "denied"), ["Build"]);
}

const SIDE: &str = r#"{"phases":[{"skill":"Plan"},{"skill":"Test"},{"skill":"Docs","skip_when":"nothing to document"},{"skill":"Ship"}],"parallel":[{"phases":["Test","Docs"]}]}"#;

#[test]
fn phases_in_a_group_run_together_and_the_run_joins_after_them() {
    let Some(r) = run(SIDE, &Options::default(), json!({"skip": {}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan", "Test", "Docs", "Ship"]);
    assert_eq!(r["result"]["blocked"], Value::Null);
}

#[test]
fn a_member_can_be_skipped_on_its_own() {
    let Some(r) = run(SIDE, &Options::default(), json!({"skip": {"Docs": true}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan", "Test", "Ship"]);
}

#[test]
fn a_blocked_member_lets_the_others_finish_then_stops_the_run() {
    let Some(r) = run(SIDE, &Options::default(), json!({"skip": {}, "blocked": {"Test": "Which suite?"}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan", "Docs"]);
    assert_eq!(r["result"]["blocked"], json!({"phase": "Test", "question": "Which suite?"}));
}

const SIDE_GATED: &str = r#"{"phases":[{"skill":"Plan"},{"skill":"Test","approve":true},{"skill":"Docs","approve":true},{"skill":"Ship"}],"parallel":[{"phases":["Test","Docs"]}]}"#;

#[test]
fn a_group_asks_for_all_its_approvals_at_once_and_a_denied_member_is_left_out() {
    let Some(r) = run(SIDE_GATED, &Options::default(), json!({})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan"]);
    assert_eq!(r["result"]["blocked"]["kind"], json!("approval"));
    assert_eq!(r["result"]["blocked"]["phases"], json!(["Test", "Docs"]));
    let Some(r) = run(SIDE_GATED, &Options::default(), json!({"args": {"approvals": {"Test": "approved", "Docs": "denied"}}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan", "Test", "Ship"]);
    assert_eq!(names(&r, "denied"), ["Docs"]);
}

#[test]
fn a_loop_can_go_round_a_whole_group() {
    let wf = r#"{"phases":[{"skill":"Develop"},{"skill":"Test"},{"skill":"Review"},{"skill":"Ship"}],"parallel":[{"phases":["Test","Review"]}],"loops":[{"from":"Develop","to":"Review","until":"all is well","max":3}]}"#;
    let Some(r) = run(wf, &Options::default(), json!({"loop": [false, true]})) else { return };
    assert_eq!(names(&r, "ran"), ["Develop", "Test", "Review", "Develop", "Test", "Review", "Ship"]);
}

#[test]
fn a_member_is_told_who_it_runs_beside() {
    let wf = r#"{"phases":[{"skill":"Test"},{"skill":"Docs"}],"parallel":[{"phases":["Test","Docs"]}]}"#;
    let Some(r) = run(wf, &Options::default(), json!({})) else { return };
    assert_eq!(names(&r, "ran"), ["Test", "Docs"]);
    let test = r["calls"].as_array().unwrap().iter().find(|c| c["label"] == "Test").unwrap();
    assert!(test["prompt"].as_str().unwrap().contains("at the same time as \"Docs\""));
}

const INLINE: &str = r#"{"phases":[{"skill":"Plan"},{"skill":"deep-review","inline":true},{"skill":"Publish"}],"loops":[{"from":"Plan","to":"deep-review","until":"clean","max":2}]}"#;

#[test]
fn an_inline_phase_stops_the_run_and_hands_it_to_the_main_session() {
    let Some(r) = run(INLINE, &Options::default(), json!({"skip": {}, "loop": [true]})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan"]);
    let b = &r["result"]["blocked"];
    assert_eq!(b["kind"], json!("inline"));
    assert_eq!(b["phase"], json!("deep-review"));
    assert_eq!(b["key"], json!("deep-review"));
    assert_eq!(b["note"], json!(".colony/notes/02-deep-review.md"));
    let q = b["question"].as_str().unwrap();
    assert!(q.contains("Skill tool") && q.contains("deep-review") && q.contains("echo colony-phase:deep-review"), "{q}");
    // No agent was spawned for it.
    assert!(r["calls"].as_array().unwrap().iter().all(|c| c["label"] != json!("deep-review")));
}

#[test]
fn an_inline_phase_marked_done_counts_as_run_and_the_rest_follow() {
    let Some(r) = run(INLINE, &Options::default(), json!({"skip": {}, "loop": [true], "args": {"done": {"deep-review": "reviewed"}}})) else { return };
    assert_eq!(names(&r, "ran"), ["Plan", "deep-review", "Publish"]);
    assert_eq!(r["result"]["blocked"], Value::Null);
}

#[test]
fn an_inline_phase_in_a_second_round_is_asked_for_again() {
    // The first round's answer does not stand for the second.
    let Some(r) = run(INLINE, &Options::default(), json!({"skip": {}, "loop": [false], "args": {"done": {"deep-review": "reviewed"}}})) else { return };
    assert_eq!(r["result"]["blocked"]["key"], json!("deep-review (round 2)"));
    assert_eq!(names(&r, "ran"), ["Plan", "deep-review", "Plan"]);
}
