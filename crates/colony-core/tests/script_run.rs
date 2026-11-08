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
