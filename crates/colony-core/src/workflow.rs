//! Dynamic workflows: a session works through phases, each phase a Claude Code
//! skill, and the session itself picks the next phase from what the work needs.
//!
//! A workflow is a JSON file (`<id>.json`) in a workflows folder:
//!
//! ```json
//! { "label": "Feature", "description": "Plan, build, review.",
//!   "phases": [ { "skill": "plan", "when": "the change isn't clear yet", "next": ["build"] },
//!               { "skill": "build", "next": ["review"] },
//!               { "skill": "review" } ] }
//! ```
//!
//! Colony doesn't enforce the graph. It tells the session about it (see
//! [`Workflow::prompt`]) and watches the Skill tool calls to see which phase is
//! running (see the reducer in `state`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Largest workflow file read; a bigger one is a mistake.
const MAX_FILE: u64 = 64 * 1024;
const MAX_PHASES: usize = 40;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phase {
    /// The skill's name, as in `.claude/skills/<skill>/SKILL.md`.
    pub skill: String,
    /// When this phase is the right one, in a sentence.
    #[serde(default)]
    pub when: String,
    /// Phases that usually follow. A hint for the session, not a rule.
    #[serde(default)]
    pub next: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    /// The file's name without `.json`; set on load.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub phases: Vec<Phase>,
}

/// A skill name is a folder name: letters, digits, `-`, `_`, `.`, and `:` for plugins.
fn valid_skill(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Read a workflow from its JSON and check it.
pub fn parse(id: &str, json: &str) -> Result<Workflow, String> {
    if !valid_id(id) {
        return Err(format!("{id:?} isn't a valid workflow name (letters, digits, - and _)"));
    }
    let mut w: Workflow = serde_json::from_str(json).map_err(|e| format!("workflow {id}: {e}"))?;
    w.id = id.to_string();
    if w.label.trim().is_empty() {
        w.label = id.to_string();
    }
    w.check()?;
    Ok(w)
}

impl Workflow {
    pub fn check(&self) -> Result<(), String> {
        let id = &self.id;
        if self.phases.is_empty() {
            return Err(format!("workflow {id}: no phases"));
        }
        if self.phases.len() > MAX_PHASES {
            return Err(format!("workflow {id}: more than {MAX_PHASES} phases"));
        }
        for (i, p) in self.phases.iter().enumerate() {
            if !valid_skill(&p.skill) {
                return Err(format!("workflow {id}: {:?} isn't a valid skill name", p.skill));
            }
            if self.phases[..i].iter().any(|q| q.skill == p.skill) {
                return Err(format!("workflow {id}: phase {:?} appears twice", p.skill));
            }
            for n in &p.next {
                if !self.phases.iter().any(|q| &q.skill == n) {
                    return Err(format!("workflow {id}: phase {:?} lists next phase {n:?}, which isn't a phase", p.skill));
                }
            }
        }
        Ok(())
    }

    /// The phase a Skill tool call names, if it's one of this workflow's.
    /// A plugin skill (`plugin:name`) matches a phase named either way.
    pub fn phase_for(&self, skill: &str) -> Option<&Phase> {
        let skill = skill.trim().trim_start_matches('/');
        let bare = skill.rsplit(':').next().unwrap_or(skill);
        self.phases.iter().find(|p| p.skill.eq_ignore_ascii_case(skill) || p.skill.eq_ignore_ascii_case(bare))
    }

    /// What the session is told: the phases, and that it chooses among them.
    pub fn prompt(&self) -> String {
        let mut s = format!("This session follows the \"{}\" workflow.", self.label);
        if !self.description.trim().is_empty() {
            s.push(' ');
            s.push_str(self.description.trim());
        }
        s.push_str(
            "\nWork in phases. Each phase is a skill: run it with the Skill tool. \
             Choose the next phase from what the work needs: skip a phase that doesn't apply, \
             repeat one, go back, or finish when the work is done. \
             Say in a sentence which phase you are starting and why.\nPhases:",
        );
        for p in &self.phases {
            s.push_str(&format!("\n- {}", p.skill));
            if !p.when.trim().is_empty() {
                s.push_str(&format!(": use when {}", p.when.trim().trim_end_matches('.')));
            }
            if !p.next.is_empty() {
                s.push_str(&format!(" (usually followed by {})", p.next.join(" or ")));
            }
        }
        s
    }
}

fn read_json(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_FILE {
        return Err(format!("{} is too big for a workflow", path.display()));
    }
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Load workflow `id`; the first folder in `dirs` that has it wins, so list the
/// project's folder before the user's.
pub fn load(dirs: &[PathBuf], id: &str) -> Result<Workflow, String> {
    if !valid_id(id) {
        return Err(format!("{id:?} isn't a valid workflow name"));
    }
    for d in dirs {
        let p = d.join(format!("{id}.json"));
        if p.is_file() {
            return parse(id, &read_json(&p)?);
        }
    }
    Err(format!("no workflow named {id:?}"))
}

/// Every workflow that loads, project folder first; a name found twice is
/// listed once. Files that don't parse are returned as `(file stem, error)`.
pub fn list(dirs: &[PathBuf]) -> (Vec<Workflow>, Vec<(String, String)>) {
    let (mut ok, mut bad): (Vec<Workflow>, Vec<(String, String)>) = (Vec::new(), Vec::new());
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        for p in files {
            let Some(id) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };
            if ok.iter().any(|w| w.id == id) || bad.iter().any(|(b, _)| *b == id) {
                continue;
            }
            match read_json(&p).and_then(|j| parse(&id, &j)) {
                Ok(w) => ok.push(w),
                Err(e) => bad.push((id, e)),
            }
        }
    }
    (ok, bad)
}

/// A skill found on disk, for the phase picker.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// "project" or "user".
    pub source: &'static str,
}

/// Skills in `<dir>/<name>/SKILL.md` for each `(dir, source)`; the first
/// folder with a name wins, as in Claude Code.
pub fn skills(dirs: &[(PathBuf, &'static str)]) -> Vec<SkillInfo> {
    let mut out: Vec<SkillInfo> = Vec::new();
    for (d, source) in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        let mut found: Vec<SkillInfo> = Vec::new();
        for e in rd.flatten() {
            let Some(name) = e.file_name().to_str().map(String::from) else { continue };
            let md = e.path().join("SKILL.md");
            if !md.is_file() || !valid_skill(&name) || out.iter().any(|s| s.name == name) {
                continue;
            }
            let text = std::fs::read_to_string(&md).unwrap_or_default();
            found.push(SkillInfo { name, description: front_matter(&text, "description"), source });
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        out.extend(found);
    }
    out
}

/// A one-line `key: value` from a file's `---` front matter.
fn front_matter(text: &str, key: &str) -> String {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    for l in lines {
        if l.trim() == "---" {
            break;
        }
        if let Some(v) = l.strip_prefix(key).and_then(|r| r.strip_prefix(':')) {
            return v.trim().trim_matches(|c| c == '"' || c == '\'').chars().take(200).collect();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEATURE: &str = r#"{ "label": "Feature", "description": "Plan, build, review.",
        "phases": [ {"skill":"plan","when":"the change isn't clear yet.","next":["build"]},
                    {"skill":"build","next":["review","plan"]}, {"skill":"review"} ] }"#;

    #[test]
    fn parses_and_prompts() {
        let w = parse("feature", FEATURE).unwrap();
        assert_eq!(w.id, "feature");
        let p = w.prompt();
        assert!(p.contains("\"Feature\" workflow"));
        assert!(p.contains("- plan: use when the change isn't clear yet (usually followed by build)"));
        assert!(p.contains("- build (usually followed by review or plan)"));
        assert!(p.contains("- review"));
        assert!(p.contains("Choose the next phase"));
    }

    #[test]
    fn label_defaults_to_id() {
        let w = parse("quick", r#"{"phases":[{"skill":"fix"}]}"#).unwrap();
        assert_eq!(w.label, "quick");
    }

    #[test]
    fn rejects_bad_workflows() {
        assert!(parse("x", r#"{"phases":[]}"#).unwrap_err().contains("no phases"));
        assert!(parse("x", r#"{"phases":[{"skill":"a"},{"skill":"a"}]}"#).unwrap_err().contains("twice"));
        assert!(parse("x", r#"{"phases":[{"skill":"a","next":["z"]}]}"#).unwrap_err().contains("isn't a phase"));
        assert!(parse("x", r#"{"phases":[{"skill":"a b"}]}"#).unwrap_err().contains("valid skill"));
        assert!(parse("../x", r#"{"phases":[{"skill":"a"}]}"#).is_err());
        assert!(parse("x", "nope").is_err());
    }

    #[test]
    fn matches_skill_calls_to_phases() {
        let w = parse("feature", FEATURE).unwrap();
        assert_eq!(w.phase_for("plan").map(|p| p.skill.as_str()), Some("plan"));
        assert_eq!(w.phase_for("/Build").map(|p| p.skill.as_str()), Some("build"));
        assert_eq!(w.phase_for("myplugin:review").map(|p| p.skill.as_str()), Some("review"));
        assert!(w.phase_for("deploy").is_none());
    }

    #[test]
    fn loads_from_folders_and_lists_failures() {
        let root = std::env::temp_dir().join(format!("colony-wf-{}", std::process::id()));
        let (proj, user) = (root.join("proj"), root.join("user"));
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(proj.join("feature.json"), FEATURE).unwrap();
        std::fs::write(user.join("feature.json"), r#"{"label":"Other","phases":[{"skill":"x"}]}"#).unwrap();
        std::fs::write(user.join("broken.json"), "{").unwrap();
        let dirs = vec![proj.clone(), user.clone()];
        assert_eq!(load(&dirs, "feature").unwrap().label, "Feature");
        assert!(load(&dirs, "missing").is_err());
        let (ok, bad) = list(&dirs);
        assert_eq!(ok.len(), 1);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].0, "broken");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_skills_with_descriptions() {
        let root = std::env::temp_dir().join(format!("colony-skills-{}", std::process::id()));
        let (proj, user) = (root.join("proj"), root.join("user"));
        for (d, n, desc) in [(&proj, "plan", "Plan the work"), (&user, "plan", "Shadowed"), (&user, "ship", "Ship it")] {
            std::fs::create_dir_all(d.join(n)).unwrap();
            std::fs::write(d.join(n).join("SKILL.md"), format!("---\nname: {n}\ndescription: {desc}\n---\nbody")).unwrap();
        }
        let found = skills(&[(proj, "project"), (user, "user")]);
        let names: Vec<_> = found.iter().map(|s| (s.name.as_str(), s.source, s.description.as_str())).collect();
        assert_eq!(names, vec![("plan", "project", "Plan the work"), ("ship", "user", "Ship it")]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
