//! Workflows on disk (see `colony_core::workflow`) and the skills their phases use.
//!
//! A workflow is looked up in the project's `.colony/workflows` first, then in
//! `workflows` in Colony's own folder. Skills come from the project's
//! `.claude/skills`, then the user's, as Claude Code reads them.

use std::path::PathBuf;

use colony_core::workflow::{self, SkillInfo, Workflow};
use colony_core::HostId;
use colony_source::{colony_home, home_dir};
use serde::Serialize;

/// The project folder is only readable from here for a Windows session.
fn project(host: &HostId, dir: &str) -> Option<PathBuf> {
    (matches!(host, HostId::Windows) && !dir.trim().is_empty()).then(|| PathBuf::from(dir))
}

fn workflow_dirs(host: &HostId, dir: &str) -> Vec<PathBuf> {
    project(host, dir).map(|p| p.join(".colony").join("workflows")).into_iter().chain([colony_home().join("workflows")]).collect()
}

fn skill_dirs(host: &HostId, dir: &str) -> Vec<(PathBuf, &'static str)> {
    project(host, dir)
        .map(|p| (p.join(".claude").join("skills"), "project"))
        .into_iter()
        .chain([(home_dir().join(".claude").join("skills"), "user")])
        .collect()
}

/// The workflow a new session asked for, checked against the skills it can run.
pub fn resolve(host: &HostId, dir: &str, id: &str) -> Result<Workflow, String> {
    workflow::load(&workflow_dirs(host, dir), id)
}

/// For the New session dialog and the editor.
#[derive(Debug, Serialize)]
pub struct Listing {
    pub workflows: Vec<Workflow>,
    /// Files that didn't load: (name, why).
    pub problems: Vec<(String, String)>,
    pub skills: Vec<SkillInfo>,
}

pub fn listing(host: &HostId, dir: &str) -> Listing {
    let (workflows, problems) = workflow::list(&workflow_dirs(host, dir));
    Listing { workflows, problems, skills: workflow::skills(&skill_dirs(host, dir)) }
}

/// Add `text` to the `--append-system-prompt` already in `args` (a crew's
/// delegation rules), or start one: Claude Code takes only one.
pub fn append_prompt(args: &mut Vec<String>, text: &str) {
    match args.iter().position(|a| a == "--append-system-prompt") {
        Some(i) if i + 1 < args.len() => {
            args[i + 1].push_str("\n\n");
            args[i + 1].push_str(text);
        }
        _ => args.extend(["--append-system-prompt".into(), text.into()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_joins_the_crews_rules_or_stands_alone() {
        let mut a: Vec<String> = vec!["--model".into(), "opus".into(), "--append-system-prompt".into(), "rules".into()];
        append_prompt(&mut a, "flow");
        assert_eq!(a, ["--model", "opus", "--append-system-prompt", "rules\n\nflow"]);
        let mut b: Vec<String> = vec!["--model".into(), "opus".into()];
        append_prompt(&mut b, "flow");
        assert_eq!(b, ["--model", "opus", "--append-system-prompt", "flow"]);
    }

    #[test]
    fn a_wsl_session_reads_only_the_users_folder() {
        let dirs = workflow_dirs(&HostId::Wsl("Ubuntu".into()), "/home/t/api");
        assert_eq!(dirs.len(), 1);
        assert_eq!(workflow_dirs(&HostId::Windows, r"C:\code\api").len(), 2);
    }
}
