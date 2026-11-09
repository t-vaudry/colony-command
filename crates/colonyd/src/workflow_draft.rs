//! Drafting a workflow from a plain-language request, by asking Claude once.
//!
//! The answer is checked with the same rules as a file on disk (`colony_core::workflow::parse`);
//! if it breaks one, Claude is asked once more with the complaint. Nothing is saved here: the
//! app shows the draft for the person to review.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use colony_core::paths::to_wsl_path;
use colony_core::workflow::{self, SkillInfo, Workflow};
use colony_core::HostId;
use tokio::io::AsyncWriteExt;

/// How long to wait for Claude before giving up.
const WAIT: Duration = Duration::from_secs(180);
/// Skills listed to Claude; more than this is noise.
const MAX_SKILLS: usize = 80;

const FORMAT: &str = r#"You write workflow definitions for Colony Command, an app that runs Claude Code sessions. A workflow is one JSON object:

{
  "label": "Short name",
  "description": "One or two sentences on what it is for.",
  "phases": [ ... in the order they usually run ... ],
  "loops": [ ... optional ... ],
  "parallel": [ ... optional ... ]
}

Each phase (only "skill" is required):
- "skill": the phase's name, unique in the workflow, using letters, digits, - _ . and : only. Normally the name of an installed skill from the list below. If you give "instruction", it is only a label.
- "instruction": makes it a custom phase: what the agent should do, in one to three sentences (under 600 characters). Use it when no installed skill fits.
- "when": under 200 characters on when the phase is the right one.
- "skip_when": makes the phase optional: skipped only when this clearly holds (a fact about the repository or the earlier work, such as "the change is a single line").
- "approve": true to make the user approve before the phase starts. "approve_when": only ask when this holds (empty means always). "on_deny": the phase to go to if the user says no (another phase's name; empty ends the run).
- "inline": true runs the phase in the main session instead of a subagent. Use it only for a skill known to start agents of its own, which a phase's subagent cannot do. Never put it on a phase in a parallel group.
- "next": names of phases that may follow. One name goes there; several let a judge pick; none means the next phase in the list.
- "branches": conditional exits instead of "next": [{"to": "Phase", "condition": "it is a regression"}, {"to": "Other"}]. The first condition that holds wins; the entry with no condition is the otherwise. Never use both "next" and "branches" on one phase.

Each loop repeats a run of phases: {"from": "Develop", "to": "Review", "until": "the review finds nothing", "max": 4}. "from" comes before "to" in the list, "max" is 1 to 20, and loops nest or stand apart but never cross.

"parallel": [{"phases": ["Test", "Docs"]}] runs 2 to 6 phases that are next to each other in the list at the same time, joined before the next phase. They share one working folder, so only group work that does not edit the same files (tests, docs, review, scans). Only the last phase of a group may have "next" or "branches", and no member may have "on_deny". Nothing may route into the middle of a group, and a loop must contain a whole group or none of it.

Rules:
- Every phase name used in next, branches, on_deny, loops and parallel must be a phase in the list. At most 40 phases.
- Conditions are judged by a cheap model against the repository and earlier notes, so write them as checkable facts, not feelings.
- Phases do not share context: each runs in its own agent and hands work on through short notes, so each instruction must stand on its own.
- Keep it as simple as the request allows: no loops, branches, approvals or parallel groups unless the request calls for them.
- Reply with the JSON object only: no prose, no code fence."#;

/// What to ask Claude. `current` is a workflow to change rather than write from nothing;
/// `retry` is a previous answer and why it was refused.
pub fn build_prompt(request: &str, skills: &[SkillInfo], current: Option<&Workflow>, retry: Option<(&str, &str)>) -> String {
    let mut s = String::from(FORMAT);
    s.push_str("\n\nInstalled skills you can use as phases:\n");
    if skills.is_empty() {
        s.push_str("(none: use custom phases with an instruction)\n");
    }
    for k in skills.iter().take(MAX_SKILLS) {
        let d: String = k.description.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect();
        s.push_str(&format!("- {}{}\n", k.name, if d.is_empty() { String::new() } else { format!(": {d}") }));
    }
    if let Some(w) = current {
        s.push_str("\nThe workflow as it is now:\n");
        s.push_str(&serde_json::to_string_pretty(w).unwrap_or_default());
        s.push_str("\n\nChange it as the request says, keeping what the request doesn't mention.\n");
    }
    s.push_str(&format!("\nThe request:\n{}\n", request.trim()));
    if let Some((answer, why)) = retry {
        s.push_str(&format!("\nYour previous answer was refused: {why}\nThe previous answer was:\n{answer}\nReply again with a corrected JSON object only.\n"));
    }
    s
}

/// The workflow in Claude's reply, which may have prose or a code fence round it.
pub fn parse_reply(text: &str) -> Result<Workflow, String> {
    let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) else {
        return Err("the reply had no JSON object in it".into());
    };
    if b < a {
        return Err("the reply had no JSON object in it".into());
    }
    let mut w = workflow::parse("draft", &text[a..=b])?;
    // The id comes from the name when it is saved.
    w.id = String::new();
    Ok(w)
}

fn claude_args() -> Vec<String> {
    // No tools: it only has to write JSON. No session file: this isn't a session of the person's.
    ["-p", "--tools", "", "--no-session-persistence", "--model", "sonnet"].iter().map(|s| s.to_string()).collect()
}

async fn ask(exe: Option<&PathBuf>, host: &HostId, dir: &str, prompt: &str) -> Result<String, String> {
    let mut cmd = match host {
        HostId::Windows => {
            let exe = exe.ok_or("Claude Code CLI isn't installed on Windows")?;
            let mut c = crate::worktree::quiet(&exe.display().to_string());
            c.args(claude_args());
            if !dir.trim().is_empty() && std::path::Path::new(dir).is_dir() {
                c.current_dir(dir);
            }
            c
        }
        HostId::Wsl(distro) => {
            let mut c = crate::worktree::quiet("wsl.exe");
            c.args(["-d", distro]);
            if !dir.trim().is_empty() {
                c.args(["--cd", &to_wsl_path(dir)]);
            }
            c.args(["-e", "bash", "-lc", "exec claude \"$@\"", "claude"]).args(claude_args());
            c
        }
    };
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().map_err(|e| format!("couldn't start Claude Code: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let input = prompt.as_bytes().to_vec();
    // Written on its own task: a long prompt must not block on a full pipe while the reply is read.
    tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
    });
    let out = tokio::time::timeout(WAIT, child.wait_with_output()).await.map_err(|_| "Claude took too long to answer".to_string())?.map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() && text.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.trim().chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(format!("Claude Code failed{}", if tail.is_empty() { String::new() } else { format!(": {tail}") }));
    }
    Ok(text)
}

/// Write (or change) a workflow from a request. Asks again once if the first answer breaks a rule.
pub async fn draft(exe: Option<PathBuf>, host: &HostId, dir: &str, request: &str, current: Option<Workflow>) -> Result<Workflow, String> {
    if request.trim().is_empty() {
        return Err("Say what the workflow should do.".into());
    }
    if request.len() > 8000 {
        return Err("That request is too long: keep it under 8000 characters.".into());
    }
    let skills = crate::workflows::listing(host, dir).skills;
    let first = ask(exe.as_ref(), host, dir, &build_prompt(request, &skills, current.as_ref(), None)).await?;
    match parse_reply(&first) {
        Ok(w) => Ok(w),
        Err(why) => {
            let second = ask(exe.as_ref(), host, dir, &build_prompt(request, &skills, current.as_ref(), Some((&first, &why)))).await?;
            parse_reply(&second).map_err(|e| format!("Claude's draft wasn't a valid workflow ({e}). Try describing it again."))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"label":"Ship it","description":"d","phases":[{"skill":"Plan"},{"skill":"Build","instruction":"Make it."}]}"#;

    #[test]
    fn reads_the_json_out_of_prose_and_fences() {
        for text in [GOOD.to_string(), format!("```json\n{GOOD}\n```"), format!("Here you go:\n{GOOD}\nHope that helps.")] {
            let w = parse_reply(&text).unwrap();
            assert_eq!(w.label, "Ship it");
            assert_eq!(w.phases.len(), 2);
            assert_eq!(w.id, "", "the id comes from the name when it is saved");
        }
    }

    #[test]
    fn refuses_what_is_not_a_valid_workflow() {
        assert!(parse_reply("no json here").unwrap_err().contains("no JSON"));
        assert!(parse_reply("} {").unwrap_err().contains("no JSON"));
        assert!(parse_reply(r#"{"phases":[]}"#).unwrap_err().contains("no phases"));
        assert!(parse_reply(r#"{"phases":[{"skill":"a","next":["nope"]}]}"#).unwrap_err().contains("isn't a phase"));
    }

    #[test]
    fn the_prompt_carries_the_format_the_skills_the_request_and_any_complaint() {
        let skills = vec![SkillInfo { name: "code-review".into(), description: "Review\n  a diff".into(), source: "user" }];
        let p = build_prompt("  ship fast  ", &skills, None, None);
        assert!(p.contains("\"parallel\"") && p.contains("\"skip_when\"") && p.contains("\"approve\""));
        assert!(p.contains("- code-review: Review a diff"));
        assert!(p.contains("The request:\nship fast\n"));
        assert!(!p.contains("refused") && !p.contains("as it is now"));
        let none = build_prompt("x", &[], None, None);
        assert!(none.contains("use custom phases"));
        let w = parse_reply(GOOD).unwrap();
        let change = build_prompt("add review", &skills, Some(&w), None);
        assert!(change.contains("as it is now") && change.contains("\"Plan\""));
        let again = build_prompt("x", &skills, None, Some(("{bad", "no phases")));
        assert!(again.contains("refused: no phases") && again.contains("{bad"));
    }

    #[test]
    fn an_empty_or_huge_request_is_refused_without_asking() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert!(rt.block_on(draft(None, &HostId::Windows, "", "   ", None)).unwrap_err().contains("Say what"));
        assert!(rt.block_on(draft(None, &HostId::Windows, "", &"x".repeat(9000), None)).unwrap_err().contains("too long"));
    }

    /// Asks the real Claude Code: `cargo test -p colonyd live_draft -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_draft_from_a_request() {
        let w = draft(Some(PathBuf::from("claude")), &HostId::Windows, "", "Fix a bug: investigate, fix it with a test, review in a loop until clean, then open a PR. Ask me before touching database migrations. Run tests and docs checks at the same time.", None).await.unwrap();
        println!("{}", serde_json::to_string_pretty(&w).unwrap());
        assert!(w.phases.len() >= 3);
    }
}
