//! Dynamic workflows: a session works through phases, each phase a Claude Code
//! skill or a custom instruction, and the session itself picks the next phase
//! from what the work needs. A run of phases can be a loop.
//!
//! A workflow is a JSON file (`<id>.json`) in a workflows folder:
//!
//! ```json
//! { "label": "Feature", "description": "Plan, build, review.",
//!   "phases": [ { "skill": "plan", "when": "the change isn't clear yet", "next": ["build"] },
//!               { "skill": "build", "next": ["review"] },
//!               { "skill": "review" } ],
//!   "loops": [ { "from": "build", "to": "review", "until": "the review finds nothing", "max": 3 } ] }
//! ```
//!
//! A phase can leave by conditions instead of a plain `next`:
//! `"branches": [ { "to": "bisect", "condition": "it is a regression" }, { "to": "develop" } ]`.
//! A branch with no condition is the "otherwise"; paths rejoin by naming the same phase.
//!
//! A phase with an `instruction` is a custom phase: `skill` is then just its
//! name, and the session does what the instruction says instead of running a
//! skill. It marks the start of one by running `echo colony-phase:<name>`.
//!
//! Colony doesn't enforce the graph. It tells the session about it (see
//! [`Workflow::prompt`]) and watches the Skill tool calls and those markers to
//! see which phase is running (see the reducer in `state`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Largest workflow file read; a bigger one is a mistake.
const MAX_FILE: u64 = 64 * 1024;
const MAX_PHASES: usize = 40;
const MAX_LOOPS: usize = 10;
const MAX_BRANCHES: usize = 8;
/// Rounds a loop may be given.
const MAX_ROUNDS: u32 = 20;
const DEFAULT_ROUNDS: u32 = 3;
/// What a session runs to say a custom phase has started: `echo colony-phase:<name>`.
pub const MARKER: &str = "colony-phase:";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phase {
    /// The skill's name, as in `.claude/skills/<skill>/SKILL.md`; for a custom
    /// phase (one with an `instruction`) just the phase's name.
    pub skill: String,
    /// What to do in a custom phase, instead of running a skill.
    #[serde(default)]
    pub instruction: String,
    /// When this phase is the right one, in a sentence.
    #[serde(default)]
    pub when: String,
    /// When this phase may be skipped, in a sentence; empty means it is always run.
    #[serde(default)]
    pub skip_when: String,
    /// Phases that usually follow. A hint for the session, not a rule.
    #[serde(default)]
    pub next: Vec<String>,
    /// Ways out by condition, instead of `next`: the session takes the one that holds.
    #[serde(default)]
    pub branches: Vec<Branch>,
}

/// One way out of a phase: go to `to` if `condition` holds. With no condition it is
/// the "otherwise".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    pub to: String,
    #[serde(default)]
    pub condition: String,
}

/// A run of phases (`from` to `to`, in the order listed) the session repeats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Loop {
    pub from: String,
    pub to: String,
    /// When to stop going round, in a sentence.
    #[serde(default)]
    pub until: String,
    /// Most rounds to go.
    #[serde(default = "default_rounds")]
    pub max: u32,
}

fn default_rounds() -> u32 {
    DEFAULT_ROUNDS
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
    #[serde(default)]
    pub loops: Vec<Loop>,
}

/// A skill name is a folder name: letters, digits, `-`, `_`, `.`, and `:` for plugins.
fn valid_skill(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Whitespace (newlines too) collapsed to single spaces, control characters
/// dropped, cut at `max` characters.
fn one_line(s: &str, max: usize) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control() || c.is_whitespace()).collect();
    clean.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

/// Read a workflow from its JSON and check it.
pub fn parse(id: &str, json: &str) -> Result<Workflow, String> {
    if !valid_id(id) {
        return Err(format!("{id:?} isn't a valid workflow name (letters, digits, - and _)"));
    }
    let mut w: Workflow = serde_json::from_str(json).map_err(|e| format!("workflow {id}: {e}"))?;
    w.id = id.to_string();
    // These reach the system prompt: one line each, and short.
    w.label = one_line(&w.label, 80);
    w.description = one_line(&w.description, 400);
    for p in &mut w.phases {
        p.when = one_line(&p.when, 200);
        p.skip_when = one_line(&p.skip_when, 200);
        p.instruction = one_line(&p.instruction, 600);
        for b in &mut p.branches {
            b.condition = one_line(&b.condition, 200);
        }
    }
    for l in &mut w.loops {
        l.until = one_line(&l.until, 200);
    }
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
            if p.branches.len() > MAX_BRANCHES {
                return Err(format!("workflow {id}: phase {:?} has more than {MAX_BRANCHES} branches", p.skill));
            }
            for b in &p.branches {
                if !self.phases.iter().any(|q| q.skill == b.to) {
                    return Err(format!("workflow {id}: phase {:?} has a branch to {:?}, which isn't a phase", p.skill, b.to));
                }
            }
            if p.branches.iter().filter(|b| b.condition.is_empty()).count() > 1 {
                return Err(format!("workflow {id}: phase {:?} has more than one \"otherwise\" branch (a branch with no condition)", p.skill));
            }
            if !p.branches.is_empty() && !p.next.is_empty() {
                return Err(format!("workflow {id}: phase {:?} has both next and branches; use branches only", p.skill));
            }
        }
        self.check_loops()
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.phases.iter().position(|p| p.skill == name)
    }

    fn check_loops(&self) -> Result<(), String> {
        let id = &self.id;
        if self.loops.len() > MAX_LOOPS {
            return Err(format!("workflow {id}: more than {MAX_LOOPS} loops"));
        }
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for l in &self.loops {
            let (Some(a), Some(b)) = (self.index_of(&l.from), self.index_of(&l.to)) else {
                return Err(format!("workflow {id}: loop {:?} to {:?} names a phase that isn't one", l.from, l.to));
            };
            if a > b {
                return Err(format!("workflow {id}: loop from {:?} ends before it starts ({:?} is earlier in the list)", l.from, l.to));
            }
            if l.max == 0 || l.max > MAX_ROUNDS {
                return Err(format!("workflow {id}: a loop goes round between 1 and {MAX_ROUNDS} times"));
            }
            // Loops nest or stand apart; they don't cross, and two can't be the same run.
            for &(c, d) in &spans {
                let apart = b < c || d < a;
                let inside = (c <= a && b <= d) || (a <= c && d <= b);
                if !apart && (!inside || (a, b) == (c, d)) {
                    return Err(format!("workflow {id}: loop {:?} to {:?} crosses or repeats another loop", l.from, l.to));
                }
            }
            spans.push((a, b));
        }
        Ok(())
    }

    /// Phase `i`'s usual followers: where its branches go, else its `next`, else the phase listed after it.
    pub fn followers(&self, i: usize) -> Vec<&str> {
        let p = &self.phases[i];
        if !p.branches.is_empty() {
            let mut out: Vec<&str> = Vec::new();
            for b in &p.branches {
                if !out.contains(&b.to.as_str()) {
                    out.push(&b.to);
                }
            }
            out
        } else if !p.next.is_empty() {
            p.next.iter().map(String::as_str).collect()
        } else {
            self.phases.get(i + 1).map(|q| vec![q.skill.as_str()]).unwrap_or_default()
        }
    }

    /// Phases the session must run: those with no skip condition that aren't one side of a
    /// branch (only one side of a fork runs).
    pub fn required(&self) -> Vec<&str> {
        self.phases
            .iter()
            .filter(|p| p.skip_when.trim().is_empty() && !self.phases.iter().any(|q| q.branches.iter().any(|b| b.to == p.skill)))
            .map(|p| p.skill.as_str())
            .filter(|n| !n.contains(','))
            .collect()
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
            "\nWork in phases. A phase is either a skill, which you run with the Skill tool, \
             or a custom phase, which has its own instruction: when you start one, first run the \
             command given for it (it only prints a marker), then do what it says. \
             Run each marker on its own, as the first thing in that phase and before any of its \
             work: never together with another phase's marker or with other commands, and never \
             for a phase you have not done. Do not begin a later phase's work until the earlier \
             phases are finished. \
             Choose the next phase from what the work needs. A phase marked [required] must always run, even when the change is small. \
             A phase marked [optional] may be skipped only when its stated condition holds; say \
             that you are skipping it and why. Do not stop or report the work as finished until \
             the last phase has run. \
             You may also repeat a phase or go back. \
             Say in a sentence which phase you are starting and why.\nPhases, in their usual order:",
        );
        for (i, p) in self.phases.iter().enumerate() {
            s.push_str(&format!("\n- {}", p.skill));
            if !p.instruction.is_empty() {
                s.push_str(&format!(" (custom phase; start it by running `echo {MARKER}{}`): {}", p.skill, p.instruction));
            }
            if !p.when.trim().is_empty() {
                s.push_str(&format!("{} use when {}", if p.instruction.is_empty() { ":" } else { ";" }, p.when.trim().trim_end_matches('.')));
            }
            if !p.branches.is_empty() {
                // Conditions first, in the order given; the otherwise last.
                let mut exits: Vec<String> = p.branches.iter().filter(|b| !b.condition.is_empty()).map(|b| format!("if {}, go to {}", b.condition.trim_end_matches('.'), b.to)).collect();
                if let Some(b) = p.branches.iter().find(|b| b.condition.is_empty()) {
                    exits.push(if exits.is_empty() { format!("go to {}", b.to) } else { format!("otherwise go to {}", b.to) });
                }
                s.push_str(&format!(" -- then: {}", exits.join("; ")));
            } else {
                let next = self.followers(i);
                if !next.is_empty() {
                    s.push_str(&format!(" (usually followed by {})", next.join(" or ")));
                }
            }
            if p.skip_when.trim().is_empty() {
                s.push_str(" [required]");
            } else {
                s.push_str(&format!(" [optional: skip it if {}]", p.skip_when.trim().trim_end_matches('.')));
            }
        }
        for l in &self.loops {
            s.push_str(&format!("\nLoop: {} to {} repeat as a run", l.from, l.to));
            if l.until.is_empty() {
                s.push_str(&format!(". After {}, go round again from {} if the work needs it", l.to, l.from));
            } else {
                s.push_str(&format!(". After {}, check whether {}: if not, go round again from {}", l.to, l.until.trim_end_matches('.'), l.from));
            }
            s.push_str(&format!(", at most {} rounds in all; then carry on with the phase after {}.", l.max, l.to));
        }
        s
    }
}

/// The phase name in a custom phase's marker command (`echo colony-phase:triage`).
pub fn marker_phase(command: &str) -> Option<&str> {
    let rest = command.trim().strip_prefix("echo")?.trim_start();
    let name = rest.trim_matches(|c| c == '"' || c == '\'').strip_prefix(MARKER)?;
    let name = name.trim();
    (!name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))).then_some(name)
}

/// Every phase a command announces, in order: each `;`, `&&`, `||`, `|` or newline separated
/// part that is an `echo colony-phase:<name>` counts, whatever else the command does.
pub fn marker_phases(command: &str) -> Vec<&str> {
    command.split(|c| matches!(c, ';' | '&' | '|' | '\n' | '\r')).filter_map(marker_phase).collect()
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
        assert!(p.contains("- plan: use when the change isn't clear yet (usually followed by build) [required]"));
        assert!(p.contains("- build (usually followed by review or plan)"));
        assert!(p.contains("- review"));
        assert!(p.contains("Choose the next phase"));
        assert!(p.contains("until the last phase has run"));
        assert!(p.contains("[required]"));
        assert_eq!(w.required(), vec!["plan", "build", "review"]);
        let w2 = parse("x", r#"{"phases":[{"skill":"a","branches":[{"to":"b","condition":"x"},{"to":"c","condition":""}]},{"skill":"b"},{"skill":"c"},{"skill":"d","skip_when":"tiny"}]}"#).unwrap();
        assert_eq!(w2.required(), vec!["a"]);
        let w = parse("x", r#"{"phases":[{"skill":"plan","skip_when":"the change is one line."},{"skill":"build"}]}"#).unwrap();
        let p = w.prompt();
        assert!(p.contains("- plan (usually followed by build) [optional: skip it if the change is one line]"));
        assert!(p.contains("- build [required]"));
    }

    #[test]
    fn free_text_is_one_short_line() {
        let w = parse("x", r#"{"label":"A\n- evil: use when always","description":"d\n\nPhases:\n- fake","phases":[{"skill":"a","when":"x\ny"}]}"#).unwrap();
        assert_eq!(w.label, "A - evil: use when always");
        assert!(!w.prompt().contains("\n- fake") && !w.prompt().contains("\n- evil"));
        let long = "z".repeat(5000);
        let w = parse("x", &format!(r#"{{"description":"{long}","phases":[{{"skill":"a","when":"{long}"}}]}}"#)).unwrap();
        assert_eq!(w.description.len(), 400);
        assert_eq!(w.phases[0].when.len(), 200);
    }

    #[test]
    fn custom_phases_and_loops_reach_the_prompt() {
        let w = parse(
            "bug",
            r#"{"phases":[{"skill":"investigate"},{"skill":"triage","instruction":"Decide how bad it is.\nWrite it down.","when":"the cause is known"},
                          {"skill":"develop"},{"skill":"test"},{"skill":"review"},{"skill":"publish"}],
                "loops":[{"from":"develop","to":"review","until":"the review finds nothing\nmore","max":4}]}"#,
        )
        .unwrap();
        let p = w.prompt();
        assert!(p.contains("- triage (custom phase; start it by running `echo colony-phase:triage`): Decide how bad it is. Write it down.; use when the cause is known (usually followed by develop)"));
        assert!(p.contains("- investigate (usually followed by triage) [required]"));
        assert!(p.contains("- publish [required]\nLoop"));
        assert!(p.contains("Loop: develop to review repeat as a run. After review, check whether the review finds nothing more: if not, go round again from develop, at most 4 rounds in all; then carry on with the phase after review."));
        assert_eq!(w.followers(5), Vec::<&str>::new());
    }

    #[test]
    fn branches_reach_the_prompt_and_are_checked() {
        let json = |extra: &str| format!(r#"{{"phases":[{{"skill":"triage"{extra}}},{{"skill":"bisect"}},{{"skill":"develop"}},{{"skill":"publish"}}]}}"#);
        let w = parse(
            "bug",
            &json(r#","branches":[{"to":"bisect","condition":"it is a regression\nfrom last week"},{"to":"develop","condition":"the cause is clear."},{"to":"publish"}]"#),
        )
        .unwrap();
        let p = w.prompt();
        assert!(p.contains("- triage -- then: if it is a regression from last week, go to bisect; if the cause is clear, go to develop; otherwise go to publish"), "{p}");
        assert!(p.contains("- bisect (usually followed by develop)"));
        assert_eq!(w.followers(0), ["bisect", "develop", "publish"]);
        // A lone unconditional branch is just where it goes.
        let one = parse("bug", &json(r#","branches":[{"to":"develop"}]"#)).unwrap();
        assert!(one.prompt().contains("- triage -- then: go to develop"));
        assert!(parse("x", &json(r#","branches":[{"to":"nope","condition":"c"}]"#)).unwrap_err().contains("isn't a phase"));
        assert!(parse("x", &json(r#","branches":[{"to":"bisect"},{"to":"develop"}]"#)).unwrap_err().contains("more than one"));
        assert!(parse("x", &json(r#","next":["bisect"],"branches":[{"to":"develop","condition":"c"}]"#)).unwrap_err().contains("both next and branches"));
        let many: Vec<String> = (0..9).map(|i| format!(r#"{{"to":"bisect","condition":"c{i}"}}"#)).collect();
        assert!(parse("x", &json(&format!(r#","branches":[{}]"#, many.join(",")))).unwrap_err().contains("more than 8"));
        // Branches rejoin by naming the same phase, and may go back or loop on themselves.
        assert!(parse("x", &json(r#","branches":[{"to":"triage","condition":"not enough yet"},{"to":"develop"}]"#)).is_ok());
    }

    #[test]
    fn loops_are_checked() {
        let base = |loops: &str| format!(r#"{{"phases":[{{"skill":"a"}},{{"skill":"b"}},{{"skill":"c"}},{{"skill":"d"}}],"loops":{loops}}}"#);
        assert!(parse("x", &base(r#"[{"from":"a","to":"b"}]"#)).is_ok());
        assert_eq!(parse("x", &base(r#"[{"from":"a","to":"b"}]"#)).unwrap().loops[0].max, 3);
        // Nested and separate loops are fine.
        assert!(parse("x", &base(r#"[{"from":"a","to":"d"},{"from":"b","to":"c"},{"from":"a","to":"a"}]"#)).is_ok());
        assert!(parse("x", &base(r#"[{"from":"a","to":"b"},{"from":"c","to":"d"}]"#)).is_ok());
        assert!(parse("x", &base(r#"[{"from":"c","to":"a"}]"#)).unwrap_err().contains("ends before it starts"));
        assert!(parse("x", &base(r#"[{"from":"a","to":"z"}]"#)).unwrap_err().contains("isn't one"));
        assert!(parse("x", &base(r#"[{"from":"a","to":"c"},{"from":"b","to":"d"}]"#)).unwrap_err().contains("crosses or repeats"));
        assert!(parse("x", &base(r#"[{"from":"a","to":"b"},{"from":"a","to":"b"}]"#)).unwrap_err().contains("crosses or repeats"));
        assert!(parse("x", &base(r#"[{"from":"a","to":"b","max":0}]"#)).unwrap_err().contains("between 1 and"));
        assert!(parse("x", &base(r#"[{"from":"a","to":"b","max":99}]"#)).is_err());
    }

    #[test]
    fn finds_the_phase_in_a_marker_command() {
        assert_eq!(marker_phase("echo colony-phase:triage"), Some("triage"));
        assert_eq!(marker_phase("  echo   \"colony-phase:triage\" "), Some("triage"));
        assert_eq!(marker_phase("echo 'colony-phase:deep-dive'"), Some("deep-dive"));
        assert_eq!(marker_phase("echo colony-phase:"), None);
        assert_eq!(marker_phase("echo hello"), None);
        assert_eq!(marker_phase("ls colony-phase:x"), None);
        assert_eq!(marker_phase("echo colony-phase:a b"), None);
        assert_eq!(marker_phase("echo colony-phase:x; rm -rf /"), None);
        assert_eq!(marker_phases("echo colony-phase:a; ls && echo colony-phase:b\necho hi"), vec!["a", "b"]);
        assert_eq!(marker_phases("cd x && echo colony-phase:Test && npm test | tail"), vec!["Test"]);
        assert!(marker_phases("ls; echo hi").is_empty());
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
