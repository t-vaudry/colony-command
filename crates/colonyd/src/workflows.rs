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

/// Settings a native workflow session starts with: the Workflow tool and the phase
/// marker commands need no permission prompt.
pub const NATIVE_SETTINGS: &str = r#"{"permissions":{"allow":["Workflow","Bash(echo colony-phase:*)","Read(./.colony/notes/**)","Edit(./.colony/notes/**)"]}}"#;

/// Where a session's workflow script lives. It has to be inside the project: Claude
/// Code's Workflow tool only runs scripts it can read from the working directory.
pub fn run_script_path(dir: &str, session_id: &str) -> PathBuf {
    PathBuf::from(dir).join(".colony").join("runs").join(format!("{session_id}.js"))
}

/// Write the native-workflow script for a session and return its path. The folder is
/// kept out of git, like the rest of Colony's files in the project.
pub fn write_run_script(host: &HostId, dir: &str, session_id: &str, w: &Workflow, opts: &colony_core::script::Options) -> Result<PathBuf, String> {
    let root = project(host, dir).ok_or("native workflows need a Windows project folder")?;
    if session_id.is_empty() || !session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("odd session id".into());
    }
    let script = colony_core::script::compile(w, opts)?;
    let path = run_script_path(&root.to_string_lossy(), session_id);
    let folder = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    // Keep the scripts and the phases' notes out of commits, without touching the project's own ignore file.
    let _ = std::fs::write(folder.parent().unwrap_or(folder).join(".gitignore"), "# Colony's files
*
");
    // The phases' notes folder exists before the first phase, so no agent needs a shell to make it.
    std::fs::create_dir_all(folder.parent().unwrap_or(folder).join("notes")).map_err(|e| format!("notes folder: {e}"))?;
    let tmp = path.with_extension("js.tmp");
    std::fs::write(&tmp, script).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(path)
}

/// The rules a native-workflow session gets: run the script, don't do the work itself.
/// `task_known` is whether the script already holds the user's request.
pub fn native_prompt(script: &std::path::Path, task_known: bool) -> String {
    let path = script.display();
    let call = if task_known {
        format!("call the Workflow tool with scriptPath set to {path:?} and no other arguments")
    } else {
        format!("as soon as the user gives you a task, call the Workflow tool with scriptPath set to {path:?} and args {{\"task\": \"<their request, verbatim>\"}}")
    };
    format!(
        "This session runs its work through a Colony workflow: a Claude Code dynamic workflow script that runs each phase in its own agent. Your first action must be to {call}. Do not start the work yourself and do not edit the script; wait for it to finish.          If its result has a `blocked` entry, a phase needs the user: tell them plainly what it asks, and when they answer call the Workflow tool again with the same scriptPath, resumeFromRunId set to that run's id, and args {{\"answers\": {{\"<phase>\": \"<their answer>\"}}}}.          When it finishes, say in a few lines what each phase did."
    )
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

#[cfg(test)]
mod native_tests {
    use super::*;

    #[test]
    fn writes_the_script_inside_the_project_and_keeps_it_out_of_git() {
        let dir = std::env::temp_dir().join(format!("colony-native-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let w = workflow::parse("flow", r#"{"phases":[{"skill":"plan","instruction":"Plan."}]}"#).unwrap();
        let d = dir.to_string_lossy().to_string();
        let path = write_run_script(&HostId::Windows, &d, "abc-123", &w, &colony_core::script::Options::default()).unwrap();
        assert_eq!(path, run_script_path(&d, "abc-123"));
        assert!(path.starts_with(&dir) && std::fs::read_to_string(&path).unwrap().starts_with("export const meta"));
        assert!(std::fs::read_to_string(dir.join(".colony").join(".gitignore")).unwrap().contains('*'));
        assert!(dir.join(".colony").join("notes").is_dir());
        assert!(write_run_script(&HostId::Windows, &d, "../evil", &w, &Default::default()).is_err());
        assert!(write_run_script(&HostId::Wsl("Ubuntu".into()), &d, "abc", &w, &Default::default()).is_err(), "Windows projects only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_prompt_says_how_to_start_and_how_to_answer_a_blocked_phase() {
        let p = native_prompt(std::path::Path::new("C:/p/.colony/runs/s.js"), true);
        assert!(p.contains("scriptPath") && p.contains("no other arguments") && p.contains("resumeFromRunId") && p.contains("blocked"));
        let q = native_prompt(std::path::Path::new("C:/p/.colony/runs/s.js"), false);
        assert!(q.contains("as soon as the user gives you a task") && q.contains("args"));
    }
}
