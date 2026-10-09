//! "Allow always for project" rules, kept in `~/.colony/policy.json`.
//!
//! A rule exists only because the user clicked that button on a permission
//! request: nothing else writes this file. It is deliberately dumb. Colony
//! never interprets a pattern; a rule is the exact tool name and rule content
//! Claude Code itself suggested for the request, and a later request matches
//! only if Claude Code suggests the very same rule(s) for it, from a session in
//! the same project. Anything unclear (no suggestion, a question tool, an
//! unreadable file) means no match, so Claude Code's own prompt decides.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log;

/// Tools whose "permission" is really a question or a plan for the user.
const NEVER_RULED: &[&str] = &["AskUserQuestion", "ExitPlanMode"];

/// One permission Claude Code suggested allowing: a tool, and optionally the
/// pattern within it (`Bash` + `npm test`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggested {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl Suggested {
    /// `Bash(npm test)`, as Claude Code writes rules.
    pub fn label(&self) -> String {
        match &self.content {
            Some(c) => format!("{}({c})", self.tool),
            None => self.tool.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub project_key: String,
    pub project_name: String,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub created_at: u64,
}

impl Rule {
    fn is(&self, project_key: &str, s: &Suggested) -> bool {
        self.project_key == project_key && self.tool == s.tool && self.content == s.content
    }
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    #[serde(default)]
    rules: Vec<Rule>,
}

/// The allow rules Claude Code suggested for a request (its first
/// `addRules`/`allow` suggestion), or none when it suggested nothing like that
/// or the tool is one that is never ruled.
pub fn suggested_rules(tool: &str, suggestions: &[Value]) -> Vec<Suggested> {
    if NEVER_RULED.contains(&tool) {
        return Vec::new();
    }
    // A request that also asks for access to another folder is about more than the rule.
    if suggestions.iter().any(|s| s.get("type").and_then(Value::as_str) == Some("addDirectories")) {
        return Vec::new();
    }
    let Some(s) = suggestions
        .iter()
        .find(|s| s.get("type").and_then(Value::as_str) == Some("addRules") && s.get("behavior").and_then(Value::as_str) == Some("allow"))
    else {
        return Vec::new();
    };
    let rules = s.get("rules").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for r in &rules {
        // One rule we can't read means we can't vouch for the whole suggestion.
        let Some(name) = r.get("toolName").and_then(Value::as_str).filter(|t| !t.is_empty()) else { return Vec::new() };
        // A rule for some other tool than the one asking is not what the user saw.
        if name != tool {
            return Vec::new();
        }
        let content = r.get("ruleContent").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
        out.push(Suggested { tool: name.to_string(), content });
    }
    out
}

pub struct Policy {
    path: PathBuf,
    /// Serializes read-modify-write of the file.
    lock: Mutex<()>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy::at(colony_source::colony_home().join("policy.json"))
    }
}

impl Policy {
    pub fn at(path: PathBuf) -> Self {
        Policy { path, lock: Mutex::new(()) }
    }

    /// The saved rules. A missing file is no rules; so is an unreadable one,
    /// which is left alone on disk rather than overwritten.
    pub fn list(&self) -> Vec<Rule> {
        self.read().unwrap_or_else(|e| {
            log(e);
            Vec::new()
        })
    }

    fn read(&self) -> Result<Vec<Rule>, String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("ignoring unreadable {}: {e}", self.path.display())),
        };
        serde_json::from_slice::<File>(&bytes).map(|f| f.rules).map_err(|e| format!("ignoring unreadable {}: {e}", self.path.display()))
    }

    /// Whether every rule Claude Code suggests for this request is one the user
    /// saved for this project.
    pub fn allows(&self, project_key: &str, tool: &str, suggestions: &[Value]) -> Option<Vec<Suggested>> {
        let wanted = suggested_rules(tool, suggestions);
        if wanted.is_empty() {
            return None;
        }
        let saved = self.list();
        wanted.iter().all(|w| saved.iter().any(|r| r.is(project_key, w))).then_some(wanted)
    }

    /// Save the suggested rules for a project. Returns the new full list.
    pub fn add(&self, project_key: &str, project_name: &str, wanted: &[Suggested], now: u64) -> Result<Vec<Rule>, String> {
        let _g = self.lock.lock().unwrap();
        let mut rules = self.read().map_err(|_| "the saved rules file can't be read; fix or delete policy.json in Colony's folder".to_string())?;
        for w in wanted {
            if !rules.iter().any(|r| r.is(project_key, w)) {
                rules.push(Rule {
                    id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
                    project_key: project_key.to_string(),
                    project_name: project_name.to_string(),
                    tool: w.tool.clone(),
                    content: w.content.clone(),
                    created_at: now,
                });
            }
        }
        self.save(&rules)?;
        Ok(rules)
    }

    /// Delete one rule. Returns the new full list.
    pub fn remove(&self, id: &str) -> Result<Vec<Rule>, String> {
        let _g = self.lock.lock().unwrap();
        let mut rules = self.read().map_err(|_| "the saved rules file can't be read; fix or delete policy.json in Colony's folder".to_string())?;
        let before = rules.len();
        rules.retain(|r| r.id != id);
        if rules.len() == before {
            return Err("that rule is already gone".into());
        }
        self.save(&rules)?;
        Ok(rules)
    }

    fn save(&self, rules: &[Rule]) -> Result<(), String> {
        let body = serde_json::to_vec_pretty(&File { rules: rules.to_vec() }).expect("serializes");
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("could not save the rule: {e}"))?;
        }
        // Write beside it and rename, so a crash never leaves half a file.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body)
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .map_err(|e| format!("could not save the rule: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp() -> Policy {
        let dir = std::env::temp_dir().join(format!("colony-policy-test-{}", uuid::Uuid::new_v4().simple()));
        Policy::at(dir.join("policy.json"))
    }

    fn bash(content: &str) -> Vec<Value> {
        vec![
            json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"}),
            json!({"type": "addRules", "behavior": "allow", "destination": "localSettings", "rules": [{"toolName": "Bash", "ruleContent": content}]}),
        ]
    }

    #[test]
    fn reads_the_suggested_rules() {
        let s = suggested_rules("Bash", &bash("npm test"));
        assert_eq!(s, vec![Suggested { tool: "Bash".into(), content: Some("npm test".into()) }]);
        assert_eq!(s[0].label(), "Bash(npm test)");
        // A whole-tool suggestion has no content.
        let mcp = vec![json!({"type": "addRules", "behavior": "allow", "rules": [{"toolName": "mcp__x__y"}]})];
        assert_eq!(suggested_rules("mcp__x__y", &mcp)[0].content, None);
        // No allow rule suggested, a deny rule, or an unreadable one: nothing.
        assert!(suggested_rules("Edit", &[json!({"type": "setMode", "mode": "acceptEdits"})]).is_empty());
        assert!(suggested_rules("Bash", &[json!({"type": "addRules", "behavior": "deny", "rules": [{"toolName": "Bash"}]})]).is_empty());
        assert!(suggested_rules("Bash", &[json!({"type": "addRules", "behavior": "allow", "rules": [{"toolName": "Bash"}, {"ruleContent": "x"}]})]).is_empty());
    }

    #[test]
    fn a_request_that_also_adds_a_folder_gets_no_rule() {
        let mut s = bash("ls");
        s.push(json!({"type": "addDirectories", "directories": ["C:/elsewhere"], "destination": "session"}));
        assert!(suggested_rules("Bash", &s).is_empty());
    }

    #[test]
    fn questions_and_plans_never_get_a_rule() {
        let s = vec![json!({"type": "addRules", "behavior": "allow", "rules": [{"toolName": "AskUserQuestion"}]})];
        assert!(suggested_rules("AskUserQuestion", &s).is_empty());
        let p = temp();
        assert!(p.allows("k", "AskUserQuestion", &s).is_none());
    }

    #[test]
    fn a_saved_rule_matches_only_the_same_tool_pattern_and_project() {
        let p = temp();
        assert!(p.allows("c:/code/api", "Bash", &bash("npm test")).is_none(), "nothing saved yet");
        let wanted = suggested_rules("Bash", &bash("npm test"));
        p.add("c:/code/api", "api", &wanted, 5).unwrap();
        assert!(p.allows("c:/code/api", "Bash", &bash("npm test")).is_some());
        assert!(p.allows("c:/code/api", "Bash", &bash("npm run build")).is_none(), "another pattern");
        assert!(p.allows("c:/code/api", "Bash", &bash("npm test:*")).is_none(), "colony does not widen patterns");
        assert!(p.allows("c:/code/web", "Bash", &bash("npm test")).is_none(), "another project");
        assert!(p.allows("c:/code/api", "PowerShell", &bash("npm test")).is_none(), "another tool");
        assert!(p.allows("c:/code/api", "Bash", &[]).is_none(), "claude code suggested nothing");
    }

    #[test]
    fn every_suggested_rule_must_be_saved() {
        let p = temp();
        let two = vec![json!({"type": "addRules", "behavior": "allow", "rules": [{"toolName": "Bash", "ruleContent": "cd x"}, {"toolName": "Bash", "ruleContent": "npm test"}]})];
        p.add("k", "k", &suggested_rules("Bash", &bash("npm test")), 1).unwrap();
        assert!(p.allows("k", "Bash", &two).is_none(), "one of the two is not saved");
        p.add("k", "k", &suggested_rules("Bash", &bash("cd x")), 2).unwrap();
        assert!(p.allows("k", "Bash", &two).is_some());
    }

    #[test]
    fn rules_can_be_removed_and_are_not_duplicated() {
        let p = temp();
        let w = suggested_rules("Bash", &bash("npm test"));
        p.add("k", "k", &w, 1).unwrap();
        let rules = p.add("k", "k", &w, 2).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(p.remove("nope").is_err());
        assert!(p.remove(&rules[0].id).unwrap().is_empty());
        assert!(p.allows("k", "Bash", &bash("npm test")).is_none());
    }

    #[test]
    fn an_unreadable_file_means_no_rules() {
        let p = temp();
        std::fs::create_dir_all(p.path.parent().unwrap()).unwrap();
        std::fs::write(&p.path, b"{ not json").unwrap();
        assert!(p.list().is_empty());
        assert!(p.allows("k", "Bash", &bash("npm test")).is_none());
    }
}
