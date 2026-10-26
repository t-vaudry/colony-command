//! The Haiku swarm: cheap subagents passed to a launched `claude` session with
//! `--agents`, and the short system-prompt rules that tell the main session
//! when to use them (and when to consult the advisor).
//!
//! The rules text is paid on every request of every session, so keep it brief.

use serde_json::{json, Value};

/// Shared tail of every swarm prompt.
const COMMON: &str = "Never consult the advisor: you inherit the main session's advisor and \
Opus usage must stay low. Return a compact summary, never raw dumps. Give paths as file:line.";

/// The `--agents` JSON defining the swarm subagents, all on `model`.
pub fn agents_json(model: &str) -> String {
    let search = json!({
        "description": "Maps the repo: layout, where a concept lives, entry points. For questions \
needing more than one or two lookups.",
        "prompt": format!(
            "You map codebases. Answer the question with the few files and directories that \
matter: layout, where the concept lives, entry points. Use Glob, Grep and Read only. {COMMON}"),
        "tools": ["Read", "Glob", "Grep"],
    });
    let grep = json!({
        "description": "Finds every usage of a symbol as file:line with context. For symbols \
with many usages.",
        "prompt": format!(
            "You find every usage of the symbol you are given (definitions, calls, re-exports, \
tests). Use Glob, Grep and Read only. Report one line per hit: file:line, then a few words of \
context. {COMMON}"),
        "tools": ["Read", "Glob", "Grep"],
    });
    let audit = json!({
        "description": "Reviews and tests current changes in an isolated worktree. For test runs, \
builds with long output, and pre-completion review.",
        "prompt": format!(
            "You audit changes in your own worktree, which starts from the default branch, not the \
caller's checkout. You are given a snapshot SHA, a base commit (default: merge-base with the \
default branch), the caller's checkout path, and maybe test/build commands. First run \
`git checkout --detach <sha>`. Then copy the caller's untracked files into your worktree: \
`git -C <checkout> ls-files --others --exclude-standard` lists them; copy each, reading only \
from the caller's side. Run `git add -A` in your worktree and review `git diff --cached <base>`. \
Run the test and build commands you were given, or find them. Report ONLY failures, risky lines \
(file:line) and missing tests, as a short structured list. Never push, never commit to branches, \
never modify the caller's checkout. {COMMON}"),
        "tools": ["Read", "Glob", "Grep", "Bash", "PowerShell"],
        "isolation": "worktree",
    });
    let mut agents = serde_json::Map::new();
    for (name, mut def, turns) in [
        ("swarm-search", search, 12),
        ("swarm-grep", grep, 10),
        ("swarm-audit", audit, 25),
    ] {
        def["model"] = Value::from(model);
        def["effort"] = Value::from("low");
        def["omitClaudeMd"] = Value::from(true);
        def["maxTurns"] = Value::from(turns);
        agents.insert(name.to_string(), def);
    }
    Value::Object(agents).to_string()
}

/// Text for `--append-system-prompt`: delegation rules when `swarm`, when to
/// consult the advisor when `advisor`. Empty when both are false.
pub fn rules(swarm: bool, advisor: bool) -> String {
    let mut parts = Vec::new();
    if swarm {
        parts.push(
            "Swarm: delegate to swarm-search or swarm-grep any search needing more than one or \
two lookups, sweeping many files, or returning large output. Do one-shot lookups yourself (a known \
path, one targeted grep). Delegate test runs and builds with long output to swarm-audit, passing \
a snapshot SHA from `git stash create` (empty output: use HEAD), the base commit and your \
checkout path. Run \
independent swarm agents in parallel. Don't repeat a search a swarm agent already answered.",
        );
    }
    if advisor {
        parts.push(
            "Advisor: consult it (1) before locking a plan or committing to an approach on \
non-trivial work, (2) when the same error appears a second time, (3) before declaring non-trivial \
work done, after the audit. Not for routine steps.",
        );
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed() -> serde_json::Map<String, Value> {
        let v: Value = serde_json::from_str(&agents_json("claude-haiku-5-5")).unwrap();
        v.as_object().unwrap().clone()
    }

    fn tools(def: &Value) -> Vec<&str> {
        def["tools"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect()
    }

    #[test]
    fn exactly_the_three_agents() {
        let agents = parsed();
        let mut names: Vec<_> = agents.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names, ["swarm-audit", "swarm-grep", "swarm-search"]);
    }

    #[test]
    fn every_agent_is_cheap_and_lean() {
        for (name, def) in parsed() {
            assert_eq!(def["model"], "claude-haiku-5-5", "{name}");
            assert_eq!(def["effort"], "low", "{name}");
            assert_eq!(def["omitClaudeMd"], true, "{name}");
            assert!(def["maxTurns"].as_u64().unwrap() > 0, "{name}");
            let prompt = def["prompt"].as_str().unwrap();
            assert!(prompt.contains("Never consult the advisor"), "{name}");
            assert!(prompt.contains("file:line"), "{name}");
            assert!(!def["description"].as_str().unwrap().is_empty(), "{name}");
        }
    }

    #[test]
    fn only_the_audit_gets_a_worktree() {
        for (name, def) in parsed() {
            if name == "swarm-audit" {
                assert_eq!(def["isolation"], "worktree");
            } else {
                assert!(def.get("isolation").is_none(), "{name}");
            }
        }
    }

    #[test]
    fn search_and_grep_are_read_only() {
        let agents = parsed();
        for name in ["swarm-search", "swarm-grep"] {
            assert_eq!(tools(&agents[name]), ["Read", "Glob", "Grep"]);
        }
        let audit = tools(&agents["swarm-audit"]);
        assert!(audit.contains(&"Bash") && audit.contains(&"PowerShell"));
        assert!(!audit.contains(&"Edit") && !audit.contains(&"Write"));
    }

    #[test]
    fn audit_prompt_pins_the_snapshot() {
        let agents = parsed();
        let p = agents["swarm-audit"]["prompt"].as_str().unwrap();
        assert!(p.contains("git checkout --detach <sha>"));
        assert!(p.contains("ls-files --others --exclude-standard"));
        assert!(p.contains("checkout path"));
        assert!(p.contains("git diff --cached <base>"));
        assert!(p.contains("Never push"));
    }

    #[test]
    fn rules_follow_the_flags() {
        assert_eq!(rules(false, false), "");
        let swarm = rules(true, false);
        assert!(swarm.contains("swarm-search") && swarm.contains("git stash create") && swarm.contains("checkout path"));
        assert!(!swarm.contains("Advisor"));
        let adv = rules(false, true);
        assert!(adv.contains("Advisor") && !adv.contains("swarm-search"));
        let both = rules(true, true);
        assert!(both.contains("Swarm:") && both.contains("Advisor:"));
    }

    #[test]
    fn rules_stay_short() {
        let words = rules(true, true).split_whitespace().count();
        assert!(words < 200, "{words} words");
    }
}
