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

/// Where a session's workflow script lives, as the session's own host sees it. It has to be
/// inside the project: Claude Code's Workflow tool only runs scripts it can read from the
/// working directory.
pub fn run_script_path(host: &HostId, dir: &str, session_id: &str) -> String {
    match host {
        HostId::Windows => PathBuf::from(dir).join(".colony").join("runs").join(format!("{session_id}.js")).to_string_lossy().into_owned(),
        HostId::Wsl(_) => format!("{}/.colony/runs/{session_id}.js", colony_core::paths::to_wsl_path(dir).trim_end_matches('/')),
    }
}

fn plain_id(session_id: &str) -> bool {
    !session_id.is_empty() && session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether the session already has its script (a resumed native session keeps it).
pub async fn run_script_exists(host: &HostId, dir: &str, session_id: &str) -> bool {
    if !plain_id(session_id) || dir.trim().is_empty() {
        return false;
    }
    let path = run_script_path(host, dir, session_id);
    match host {
        HostId::Windows => std::path::Path::new(&path).is_file(),
        HostId::Wsl(distro) => crate::worktree::quiet("wsl.exe")
            .args(["-d", distro, "-e", "test", "-f", &path])
            .stdin(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false),
    }
}

/// Write the native-workflow script for a session and return its path as the session's
/// host sees it. The folder is kept out of git, like the rest of Colony's files in the
/// project, and the phases' notes folder exists before the first phase so no agent needs
/// a shell to make it.
pub async fn write_run_script(host: &HostId, dir: &str, session_id: &str, w: &Workflow, opts: &colony_core::script::Options) -> Result<String, String> {
    if !plain_id(session_id) {
        return Err("odd session id".into());
    }
    if dir.trim().is_empty() {
        return Err("no project folder".into());
    }
    let script = colony_core::script::compile(w, opts)?;
    let path = run_script_path(host, dir, session_id);
    match host {
        HostId::Windows => {
            let path = PathBuf::from(&path);
            let folder = path.parent().ok_or("no folder")?;
            let colony = folder.parent().unwrap_or(folder);
            std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
            let _ = std::fs::write(colony.join(".gitignore"), "# Colony's files\n*\n");
            std::fs::create_dir_all(colony.join("notes")).map_err(|e| format!("notes folder: {e}"))?;
            let tmp = path.with_extension("js.tmp");
            std::fs::write(&tmp, script).map_err(|e| format!("{}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{}: {e}", path.display())
            })?;
            Ok(path.to_string_lossy().into_owned())
        }
        HostId::Wsl(distro) => {
            use tokio::io::AsyncWriteExt;
            // The script goes in on stdin: it can be far longer than a command line allows.
            const SH: &str = r#"set -e; d="$(dirname "$(dirname "$1")")"; mkdir -p "$d/runs" "$d/notes"; printf '# Colony files\n*\n' > "$d/.gitignore"; cat > "$1.tmp"; mv "$1.tmp" "$1""#;
            let mut child = crate::worktree::quiet("wsl.exe")
                .args(["-d", distro, "-e", "sh", "-c", SH, "sh", &path])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("couldn't run wsl.exe: {e}"))?;
            let mut stdin = child.stdin.take().ok_or("no stdin")?;
            stdin.write_all(script.as_bytes()).await.map_err(|e| format!("writing the script: {e}"))?;
            drop(stdin);
            let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(format!("couldn't write the script in WSL: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            Ok(path)
        }
    }
}

/// The rules a native-workflow session gets: run the script, don't do the work itself.
/// `task_known` is whether the script already holds the user's request.
pub fn native_prompt(path: &str, task_known: bool) -> String {
    let call = if task_known {
        format!("call the Workflow tool with scriptPath set to {path:?} and no other arguments")
    } else {
        format!("as soon as the user gives you a task, call the Workflow tool with scriptPath set to {path:?} and args {{\"task\": \"<their request, verbatim>\"}}")
    };
    format!(
        "This session runs its work through a Colony workflow: a Claude Code dynamic workflow script that runs each phase in its own agent. Your first action must be to {call}. Do not start the work yourself and do not edit the script; wait for it to finish. If there is no Workflow tool, this version of Claude Code is too old for workflows: tell the user to update it and stop.          If its result has a `blocked` entry, a phase needs the user: tell them plainly what it asks, and when they answer call the Workflow tool again with the same scriptPath, resumeFromRunId set to that run's id, and args {{\"answers\": {{\"<phase>\": \"<their answer>\"}}}}.          If the `blocked` entry has kind \"approval\", the user must say whether the named phase may start: ask them plainly, and call the Workflow tool again the same way with args {{\"approvals\": {{\"<phase>\": \"approved\"}}}}, or \"denied\" in place of \"approved\" (add their reason after \"denied: \" if they gave one). If the entry has a `phases` list, several phases that run together are waiting: ask about each and answer for every one. Keep every earlier answer and approval in the args. When it finishes, say in a few lines what each phase did, and which phases the user declined."
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

    #[tokio::test]
    async fn writes_the_script_inside_the_project_and_keeps_it_out_of_git() {
        let dir = std::env::temp_dir().join(format!("colony-native-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let w = workflow::parse("flow", r#"{"phases":[{"skill":"plan","instruction":"Plan."}]}"#).unwrap();
        let d = dir.to_string_lossy().to_string();
        let path = write_run_script(&HostId::Windows, &d, "abc-123", &w, &colony_core::script::Options::default()).await.unwrap();
        assert_eq!(path, run_script_path(&HostId::Windows, &d, "abc-123"));
        assert!(PathBuf::from(&path).starts_with(&dir) && std::fs::read_to_string(&path).unwrap().starts_with("export const meta"));
        assert!(run_script_exists(&HostId::Windows, &d, "abc-123").await && !run_script_exists(&HostId::Windows, &d, "other").await);
        assert!(std::fs::read_to_string(dir.join(".colony").join(".gitignore")).unwrap().contains('*'));
        assert!(dir.join(".colony").join("notes").is_dir());
        assert!(write_run_script(&HostId::Windows, &d, "../evil", &w, &Default::default()).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wsl_script_lives_at_the_linux_path_of_the_project() {
        let wsl = HostId::Wsl("Ubuntu".into());
        assert_eq!(run_script_path(&wsl, "/home/t/api", "s1"), "/home/t/api/.colony/runs/s1.js");
        assert_eq!(run_script_path(&wsl, r"C:\code\api", "s1"), "/mnt/c/code/api/.colony/runs/s1.js");
        assert_eq!(run_script_path(&wsl, "/home/t/api/", "s1"), "/home/t/api/.colony/runs/s1.js");
    }

    #[test]
    fn the_prompt_says_how_to_start_and_how_to_answer_a_blocked_phase() {
        let p = native_prompt("C:/p/.colony/runs/s.js", true);
        assert!(p.contains("scriptPath") && p.contains("no other arguments") && p.contains("resumeFromRunId") && p.contains("blocked") && p.contains("approvals") && p.contains("denied"));
        let q = native_prompt("C:/p/.colony/runs/s.js", false);
        assert!(q.contains("as soon as the user gives you a task") && q.contains("args"));
    }
}

#[cfg(test)]
mod wsl_tests {
    use super::*;

    async fn wsl(distro: &str, args: &[&str]) -> Option<String> {
        let out = crate::worktree::quiet("wsl.exe").args(["-d", distro, "-e"]).args(args).stdin(std::process::Stdio::null()).output().await.ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Needs a WSL distro; says so and passes without one.
    #[tokio::test]
    async fn a_script_written_into_a_wsl_project_is_there_and_is_found_again() {
        if !cfg!(windows) {
            return;
        }
        let Some(distro) = wsl("Ubuntu", &["echo", "Ubuntu"]).await.map(|_| "Ubuntu") else {
            eprintln!("no Ubuntu distro: skipping");
            return;
        };
        let host = HostId::Wsl(distro.into());
        let dir = format!("/tmp/colony-native-{}", std::process::id());
        let w = workflow::parse("flow", r#"{"phases":[{"skill":"plan","instruction":"Plan it."}]}"#).unwrap();
        let opts = colony_core::script::Options { task: "do 'it' \"now\"".into(), ..Default::default() };
        assert!(!run_script_exists(&host, &dir, "s-1").await);
        let path = write_run_script(&host, &dir, "s-1", &w, &opts).await.unwrap();
        assert_eq!(path, format!("{dir}/.colony/runs/s-1.js"));
        assert!(run_script_exists(&host, &dir, "s-1").await);
        let text = wsl(distro, &["cat", &path]).await.unwrap();
        assert!(text.starts_with("export const meta") && text.contains(r#"const TASK = "do 'it' \"now\"""#), "{text}");
        assert!(wsl(distro, &["test", "-d", &format!("{dir}/.colony/notes")]).await.is_some());
        assert!(wsl(distro, &["cat", &format!("{dir}/.colony/.gitignore")]).await.unwrap().contains('*'));
        assert!(wsl(distro, &["ls", &format!("{dir}/.colony/runs")]).await.unwrap().trim() == "s-1.js", "no temp file left behind");
        let _ = wsl(distro, &["rm", "-rf", &dir]).await;
    }
}
