//! Permission requests held for the map.
//!
//! A session's approval hook (`colony-approve.sh`, on `PermissionRequest`)
//! POSTs the request to colonyd and waits. colonyd shows it on the map and
//! answers the hook when someone clicks Allow, Always allow, or Deny.
//!
//! It always fails open: with no map open, after `MAX_WAIT`, when the session
//! is answered somewhere else (the desktop app's own prompt, the terminal), or
//! when colonyd isn't running at all, the hook gets no decision and Claude
//! Code's normal permission prompt handles the request.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use colony_core::Colony;
use crate::policy::{self, Policy, Suggested};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::oneshot;

/// Longest a request is held. The hook's own timeout is 600 s.
pub const MAX_WAIT: Duration = Duration::from_secs(570);

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    Allow,
    /// Allow, and add Claude Code's suggested allow rule so it won't ask again.
    AllowAlways,
    /// Allow, and save a Colony rule so the same tool and pattern is allowed
    /// from now on in this project (see `policy`).
    AllowProject,
    /// No decision: Claude Code's own prompt in the terminal handles it.
    Pass,
    Deny,
}

/// What the hook prints. `Pass` prints nothing: Claude Code's prompt decides.
#[derive(Debug, PartialEq)]
pub enum Decision {
    Allow { rule: Option<Value>, updated_input: Option<Value> },
    Deny { message: String },
    Pass,
}

impl Decision {
    /// The hook's stdout, or `None` for no decision.
    pub fn hook_output(&self) -> Option<String> {
        let decision = match self {
            Decision::Allow { rule: None, updated_input } => with_input(json!({ "behavior": "allow" }), updated_input),
            Decision::Allow { rule: Some(r), updated_input } => with_input(json!({ "behavior": "allow", "updatedPermissions": [r] }), updated_input),
            Decision::Deny { message } => json!({ "behavior": "deny", "message": message }),
            Decision::Pass => return None,
        };
        Some(json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } }).to_string())
    }
}

struct Pending {
    agent_id: String,
    suggestions: Vec<Value>,
    /// The project the request came from, and the rules a project rule would save.
    project: Option<(String, String)>,
    offer: Vec<Suggested>,
    /// The tool call's input as received, echoed back with answers filled in.
    input: Option<Value>,
    /// Set once the colony shows this request on the agent; only then can its
    /// disappearance mean "answered elsewhere".
    shown: bool,
    tx: oneshot::Sender<Decision>,
}

#[derive(Default)]
pub struct Approvals {
    pending: Mutex<HashMap<String, Pending>>,
    pub policy: Policy,
    /// Connected maps. With none, nobody could answer, so nothing is held.
    pub maps: AtomicUsize,
}

impl Approvals {
    pub fn anyone_watching(&self) -> bool {
        self.maps.load(Ordering::SeqCst) > 0
    }

    pub fn hold(&self, request_id: &str, held: Held) -> oneshot::Receiver<Decision> {
        let (tx, rx) = oneshot::channel();
        // A project rule needs a project to scope it to.
        let offer = if held.project.is_some() { policy::suggested_rules(&held.tool, &held.suggestions) } else { Vec::new() };
        let Held { agent_id, suggestions, input, project, .. } = held;
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.to_string(), Pending { agent_id, suggestions, project, offer, input, shown: false, tx });
        rx
    }

    /// What "Allow always for project" would save for this request, if Claude
    /// Code suggested something it can be built from.
    pub fn offer(&self, request_id: &str) -> Vec<Suggested> {
        self.pending.lock().unwrap().get(request_id).map(|p| p.offer.clone()).unwrap_or_default()
    }

    /// The offers on every held request, for a map that has just connected.
    pub fn offers(&self) -> Value {
        let pending = self.pending.lock().unwrap();
        let map: serde_json::Map<String, Value> = pending
            .iter()
            .filter(|(_, p)| !p.offer.is_empty())
            .map(|(id, p)| (id.clone(), offer_json(&p.offer, p.project.as_ref().map(|(_, n)| n.as_str()))))
            .collect();
        Value::Object(map)
    }

    pub fn forget(&self, request_id: &str) {
        self.pending.lock().unwrap().remove(request_id);
    }

    /// Answer from the map. Err if the request is no longer waiting. Ok(true)
    /// when a project rule was saved.
    pub fn decide(&self, request_id: &str, choice: Choice, message: Option<String>, answers: Option<Value>) -> Result<bool, String> {
        let mut pending = self.pending.lock().unwrap();
        // Saving a rule can fail; then the request is still waiting for an answer.
        let mut saved = false;
        if choice == Choice::AllowProject {
            let p = pending.get(request_id).ok_or(GONE)?;
            let (key, name) = p.project.as_ref().filter(|_| !p.offer.is_empty()).ok_or("Claude Code didn't suggest a rule for this request, so there is nothing to save. Use Allow instead.")?;
            self.policy.add(key, name, &p.offer, colony_source::now_ms())?;
            saved = true;
        }
        let p = pending.remove(request_id).ok_or(GONE)?;
        drop(pending);
        let decision = match choice {
            Choice::Allow => Decision::Allow { rule: None, updated_input: answered(p.input.as_ref(), answers) },
            Choice::AllowAlways => Decision::Allow { rule: allow_rule(&p.suggestions), updated_input: None },
            // The rule lives in Colony's folder; Claude Code's own settings stay untouched.
            Choice::AllowProject => Decision::Allow { rule: None, updated_input: None },
            Choice::Pass => Decision::Pass,
            Choice::Deny => Decision::Deny {
                message: message
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| "The user denied this from Colony Command.".into()),
            },
        };
        p.tx.send(decision).map_err(|_| "The session stopped waiting for an answer.".to_string())?;
        Ok(saved)
    }

    /// After the colony changes: release requests the session has moved past,
    /// because they were answered in the session itself.
    pub fn release_answered(&self, colony: &Colony) {
        let mut pending = self.pending.lock().unwrap();
        let mut done = Vec::new();
        for (id, p) in pending.iter_mut() {
            let showing = colony
                .agents
                .get(&p.agent_id)
                .and_then(|a| a.permission.as_ref())
                .is_some_and(|q| &q.request_id == id);
            if showing {
                p.shown = true;
            } else if p.shown {
                done.push(id.clone());
            }
        }
        for id in done {
            if let Some(p) = pending.remove(&id) {
                let _ = p.tx.send(Decision::Pass);
            }
        }
    }
}

const GONE: &str = "That request isn't waiting any more; it was answered or timed out.";

/// What the map shows on the "Allow always for project" button.
pub fn offer_json(offer: &[Suggested], project: Option<&str>) -> Value {
    json!({ "rules": offer.iter().map(Suggested::label).collect::<Vec<_>>(), "project": project })
}

/// A permission request about to be held.
pub struct Held {
    pub agent_id: String,
    pub tool: String,
    pub suggestions: Vec<Value>,
    pub input: Option<Value>,
    /// (project key, project name) of the session asking.
    pub project: Option<(String, String)>,
}

/// A question tool's input with the chosen answers added (question text -> answer),
/// which is how a hook answers AskUserQuestion without its terminal prompt.
fn answered(input: Option<&Value>, answers: Option<Value>) -> Option<Value> {
    let mut input = input?.clone();
    input.as_object_mut()?.insert("answers".into(), answers?);
    Some(input)
}

fn with_input(mut decision: Value, updated_input: &Option<Value>) -> Value {
    if let Some(i) = updated_input {
        decision["updatedInput"] = i.clone();
    }
    decision
}

/// The allow rule Claude Code suggested for this request, to echo back so it
/// won't ask again for the same thing.
fn allow_rule(suggestions: &[Value]) -> Option<Value> {
    suggestions
        .iter()
        .find(|s| s.get("type").and_then(Value::as_str) == Some("addRules") && s.get("behavior").and_then(Value::as_str) == Some("allow"))
        .cloned()
}

/// Removes a request when its HTTP call finishes for any reason, including
/// the hook being killed because the session was answered elsewhere.
pub struct Hold {
    pub approvals: Arc<Approvals>,
    pub request_id: String,
    pub on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.approvals.forget(&self.request_id);
        if let Some(f) = self.on_drop.take() {
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(tool: &str, suggestions: Vec<Value>) -> Held {
        Held { agent_id: "s".into(), tool: tool.into(), suggestions, input: None, project: Some(("c:/code/api".into(), "api".into())) }
    }

    fn approvals() -> Approvals {
        let dir = std::env::temp_dir().join(format!("colony-approvals-test-{}", uuid::Uuid::new_v4().simple()));
        Approvals { policy: Policy::at(dir.join("policy.json")), ..Approvals::default() }
    }

    #[test]
    fn hook_output_shapes() {
        assert_eq!(Decision::Pass.hook_output(), None);
        let allow: Value = serde_json::from_str(&Decision::Allow { rule: None, updated_input: None }.hook_output().unwrap()).unwrap();
        assert_eq!(allow["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(allow["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let deny: Value = serde_json::from_str(&Decision::Deny { message: "no".into() }.hook_output().unwrap()).unwrap();
        assert_eq!(deny["hookSpecificOutput"]["decision"]["message"], "no");
    }

    #[test]
    fn always_allow_echoes_the_suggested_allow_rule() {
        let a = approvals();
        let rule = json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "npm test"}], "behavior": "allow", "destination": "localSettings"});
        let mode = json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"});
        let mut rx = a.hold("q", held("Bash", vec![mode, rule.clone()]));
        a.decide("q", Choice::AllowAlways, None, None).unwrap();
        assert_eq!(rx.try_recv().unwrap(), Decision::Allow { rule: Some(rule), updated_input: None });
        assert!(a.decide("q", Choice::Allow, None, None).is_err(), "can't answer twice");
    }

    #[test]
    fn allow_always_for_project_saves_a_colony_rule_and_allows_this_once() {
        let a = approvals();
        let rule = json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "npm test"}], "behavior": "allow", "destination": "localSettings"});
        let mut rx = a.hold("q", held("Bash", vec![rule.clone()]));
        assert_eq!(a.offer("q")[0].label(), "Bash(npm test)");
        assert_eq!(a.decide("q", Choice::AllowProject, None, None), Ok(true));
        // Allowed for now, without writing anything into Claude Code's settings.
        assert_eq!(rx.try_recv().unwrap(), Decision::Allow { rule: None, updated_input: None });
        assert!(a.policy.allows("c:/code/api", "Bash", &[rule.clone()]).is_some());
        assert!(a.policy.allows("c:/code/web", "Bash", &[rule]).is_none());
    }

    #[test]
    fn no_suggestion_or_no_project_means_no_project_rule_and_the_request_keeps_waiting() {
        let a = approvals();
        let mut rx = a.hold("q", held("Edit", vec![json!({"type": "setMode", "mode": "acceptEdits"})]));
        assert!(a.offer("q").is_empty());
        assert!(a.decide("q", Choice::AllowProject, None, None).is_err());
        assert!(rx.try_recv().is_err(), "still waiting for a real answer");
        assert!(a.policy.list().is_empty());
        assert!(a.decide("q", Choice::Allow, None, None).is_ok());

        let rule = json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "ls"}], "behavior": "allow"});
        let mut no_project = held("Bash", vec![rule]);
        no_project.project = None;
        a.hold("r", no_project);
        assert!(a.decide("r", Choice::AllowProject, None, None).is_err());
        assert!(a.policy.list().is_empty());
    }

    #[test]
    fn only_an_explicit_choice_writes_rules() {
        let a = approvals();
        let rule = json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "ls"}], "behavior": "allow"});
        for choice in [Choice::Allow, Choice::AllowAlways, Choice::Deny, Choice::Pass] {
            let _rx = a.hold("q", held("Bash", vec![rule.clone()]));
            a.decide("q", choice, None, None).unwrap();
        }
        assert!(a.policy.list().is_empty());
    }

    #[test]
    fn released_only_after_being_shown() {
        use colony_core::{DomainEvent, Envelope, HostId};
        let a = approvals();
        let mut rx = a.hold("q", held("Bash", vec![]));
        let mut colony = Colony::new();
        // Not shown yet (its event hasn't been applied): not released.
        colony.apply(&Envelope { ts: 1, host: HostId::Windows, session_id: "s".into(), cwd: None, event: DomainEvent::SessionStarted { source: None, model: None } });
        a.release_answered(&colony);
        assert!(rx.try_recv().is_err());
        // Shown, then the session moves on: released with no decision.
        let ask = DomainEvent::PermissionAsked { request_id: "q".into(), agent_id: None, tool: "Bash".into(), target: None, input: None };
        colony.apply(&Envelope { ts: 2, host: HostId::Windows, session_id: "s".into(), cwd: None, event: ask });
        a.release_answered(&colony);
        colony.apply(&Envelope { ts: 3, host: HostId::Windows, session_id: "s".into(), cwd: None, event: DomainEvent::TurnEnded { last_message: None } });
        a.release_answered(&colony);
        assert_eq!(rx.try_recv().unwrap(), Decision::Pass);
    }
}
