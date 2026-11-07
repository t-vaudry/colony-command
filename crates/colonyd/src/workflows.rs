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
    /// Ids of the workflows that come from the project's own folder (the rest
    /// are the user's): a repo's text goes into the prompt, so say so.
    pub from_project: Vec<String>,
    /// Files that didn't load: (name, why).
    pub problems: Vec<(String, String)>,
    pub skills: Vec<SkillInfo>,
}

pub fn listing(host: &HostId, dir: &str) -> Listing {
    let dirs = workflow_dirs(host, dir);
    let (workflows, problems) = workflow::list(&dirs);
    let from_project = match (project(host, dir), dirs.first()) {
        (Some(_), Some(first)) => workflows.iter().filter(|w| first.join(format!("{}.json", w.id)).is_file()).map(|w| w.id.clone()).collect(),
        _ => Vec::new(),
    };
    Listing { workflows, from_project, problems, skills: workflow::skills(&skill_dirs(host, dir)) }
}

/// Where a workflow is kept: with the project, or for the user's every project.
fn scope_dir(host: &HostId, dir: &str, scope: &str) -> Result<PathBuf, String> {
    match scope {
        "user" => Ok(colony_home().join("workflows")),
        "project" => project(host, dir)
            .map(|p| p.join(".colony").join("workflows"))
            .ok_or_else(|| "Colony can only write a project's workflows for a Windows session; save it for you instead".to_string()),
        other => Err(format!("unknown place {other:?} to save a workflow")),
    }
}

/// Check `w` the way a file is checked on load, then write it as `<id>.json`.
pub fn save(host: &HostId, dir: &str, scope: &str, w: &Workflow) -> Result<(), String> {
    let json = serde_json::to_string(w).map_err(|e| e.to_string())?;
    let checked = workflow::parse(&w.id, &json)?;
    let folder = scope_dir(host, dir, scope)?;
    let mut body = serde_json::to_value(&checked).map_err(|e| e.to_string())?;
    // The file's name is its id.
    if let Some(o) = body.as_object_mut() {
        o.remove("id");
    }
    let text = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
    let path = folder.join(format!("{}.json", checked.id));
    let tmp = folder.join(format!("{}.json.tmp", checked.id));
    let written = std::fs::create_dir_all(&folder).and_then(|_| std::fs::write(&tmp, text)).and_then(|_| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(|e| format!("could not save {}: {e}", path.display()))
}

/// Remove workflow `id` from the place it was saved.
pub fn delete(host: &HostId, dir: &str, scope: &str, id: &str) -> Result<(), String> {
    // Same check as loading, so `id` can't be a path.
    workflow::parse(id, r#"{"phases":[{"skill":"x"}]}"#)?;
    let path = scope_dir(host, dir, scope)?.join(format!("{id}.json"));
    std::fs::remove_file(&path).map_err(|e| format!("could not remove {}: {e}", path.display()))
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
    fn saves_checks_and_deletes_a_projects_workflow() {
        let root = std::env::temp_dir().join(format!("colony-wfsave-{}", std::process::id()));
        let dir = root.display().to_string();
        let w = workflow::parse("ship", r#"{"label":"Ship","phases":[{"skill":"build","next":["test"]},{"skill":"test"}]}"#).unwrap();
        save(&HostId::Windows, &dir, "project", &w).unwrap();
        let file = root.join(".colony").join("workflows").join("ship.json");
        assert!(!std::fs::read_to_string(&file).unwrap().contains("\"id\""));
        let l = listing(&HostId::Windows, &dir);
        assert_eq!(l.workflows.iter().find(|x| x.id == "ship"), Some(&w));
        assert_eq!(l.from_project, ["ship"]);

        let mut bad = w.clone();
        bad.phases[0].next = vec!["nowhere".into()];
        assert!(save(&HostId::Windows, &dir, "project", &bad).unwrap_err().contains("isn't a phase"));
        bad.id = "../evil".into();
        assert!(save(&HostId::Windows, &dir, "project", &bad).is_err());
        assert!(save(&HostId::Windows, &dir, "elsewhere", &w).is_err());
        assert!(save(&HostId::Wsl("Ubuntu".into()), &dir, "project", &w).is_err());

        assert!(delete(&HostId::Windows, &dir, "project", "../ship").is_err());
        delete(&HostId::Windows, &dir, "project", "ship").unwrap();
        assert!(!file.exists());
        assert!(delete(&HostId::Windows, &dir, "project", "ship").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_wsl_session_reads_only_the_users_folder() {
        let dirs = workflow_dirs(&HostId::Wsl("Ubuntu".into()), "/home/t/api");
        assert_eq!(dirs.len(), 1);
        assert_eq!(workflow_dirs(&HostId::Windows, r"C:\code\api").len(), 2);
    }
}
