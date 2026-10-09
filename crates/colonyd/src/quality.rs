//! State quality: the feedback log, tunable thresholds, daemon settings, and
//! the CPU watcher that keeps a long-running tool from being called a stall.
//!
//! All of it lives under `~/.colony`:
//! - `feedback.jsonl`    one line per "Wrong state" click
//! - `thresholds.json`   optional overrides for stall and question limits
//! - `settings.json`     `{"haiku_classifier": false, "anthropic_api_key": null}`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use colony_core::state::{Agent, AgentState, Thresholds};
use colony_source::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{log, Shared};

pub fn feedback_path() -> PathBuf {
    colony_source::colony_home().join("feedback.jsonl")
}

pub fn thresholds_path() -> PathBuf {
    colony_source::colony_home().join("thresholds.json")
}

pub fn settings_path() -> PathBuf {
    colony_source::colony_home().join("settings.json")
}

/// Thresholds from the file, or the defaults when it is missing or unreadable.
pub fn load_thresholds(path: &Path) -> Thresholds {
    match std::fs::read_to_string(path) {
        Ok(text) => Thresholds::from_json(&text),
        Err(_) => Thresholds::default(),
    }
}

/// What the user can change from the map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Ask a small model about unclear question-or-done endings. Off by default.
    pub haiku_classifier: bool,
    /// Key for that, if `ANTHROPIC_API_KEY` is not set in the environment.
    /// Never sent to the map.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<String>,
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).expect("serializes"))?;
        std::fs::rename(&tmp, path)
    }

    /// The key to use: the environment's, else the settings file's.
    pub fn api_key(&self) -> Option<String> {
        std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .or_else(|| self.anthropic_api_key.clone())
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
    }

    /// What the map is told: whether the classifier is on and whether it could run.
    pub fn public(&self) -> serde_json::Value {
        json!({ "haiku_classifier": self.haiku_classifier, "haiku_key_present": self.api_key().is_some() })
    }
}

/// States a bot can be reported as having been in by mistake, and the ones
/// the user may say it should have been in.
const STATES: &[&str] = &["working", "needs_input", "awaiting_reply", "blocked", "ready_to_review", "idle"];

/// One line of the feedback log.
pub fn feedback_line(a: &Agent, should_be: &str, now: u64) -> serde_json::Value {
    json!({
        "at": now,
        "session_id": a.session_id,
        "agent": a.id,
        "project": a.project_name,
        "state": a.state,
        "state_since": a.state_since,
        "reason": a.reason,
        "basis": a.basis,
        "should_be": should_be,
    })
}

/// Record that the user thinks a bot's state is wrong. Returns what was filed.
pub fn record_feedback(path: &Path, a: &Agent, should_be: &str, now: u64) -> Result<(), String> {
    if !STATES.contains(&should_be) {
        return Err(format!("unknown state: {should_be}"));
    }
    if a.state_label_is(should_be) {
        return Err("that is the state it is in".into());
    }
    colony_source::rotate_log(path);
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("could not open the feedback log: {e}"))?;
    let mut line = feedback_line(a, should_be, now).to_string();
    line.push('\n');
    f.write_all(line.as_bytes()).map_err(|e| format!("could not write the feedback log: {e}"))
}

trait StateLabel {
    fn state_label_is(&self, label: &str) -> bool;
}

impl StateLabel for Agent {
    fn state_label_is(&self, label: &str) -> bool {
        serde_json::to_value(self.state).ok().and_then(|v| v.as_str().map(|s| s == label)).unwrap_or(false)
    }
}

/// CPU use needs to rise by at least this much between looks to count as computing.
const ACTIVE_DELTA_MS: u64 = 100;
const LOOK_EVERY: Duration = Duration::from_secs(5);
/// Only tool calls running this long are watched: shorter ones can't be stalls yet.
const WATCH_AFTER_MS: u64 = 60_000;

/// Whether a reading moved enough from the last one to mean the tree is computing.
pub fn is_computing(previous: Option<u64>, now: u64) -> bool {
    previous.is_some_and(|p| now.saturating_sub(p) >= ACTIVE_DELTA_MS)
}

/// Watch the CPU of sessions with a long tool call running, and tell the
/// colony when they are busy, so a long build or test run isn't called stuck.
/// Sessions on another machine or distro can't be read; they keep the plain time limit.
pub async fn watch_cpu(shared: Arc<Shared>) {
    let mut last: HashMap<String, u64> = HashMap::new();
    loop {
        tokio::time::sleep(LOOK_EVERY).await;
        let now = now_ms();
        // (agent id, process to read)
        let watched: Vec<(String, u32)> = {
            let colony = shared.colony.read().await;
            colony
                .agents
                .values()
                .filter(|a| a.state == AgentState::Working)
                .filter(|a| a.current_tool.as_ref().is_some_and(|t| now.saturating_sub(t.started_at) >= WATCH_AFTER_MS))
                .filter_map(|a| {
                    let main = colony.agents.get(&a.session_id)?;
                    if main.host != crate::local_host() {
                        return None;
                    }
                    Some((a.id.clone(), main.terminal_pid.or(main.pid)?))
                })
                .collect()
        };
        last.retain(|id, _| watched.iter().any(|(w, _)| w == id));
        if watched.is_empty() {
            continue;
        }
        let readings: Vec<(String, Option<u64>)> = tokio::task::spawn_blocking(move || {
            watched.into_iter().map(|(id, pid)| (id, colony_source::process::tree_cpu_ms(pid))).collect()
        })
        .await
        .unwrap_or_default();
        let mut busy = Vec::new();
        for (id, cpu) in readings {
            let Some(cpu) = cpu else { continue };
            if is_computing(last.insert(id.clone(), cpu), cpu) {
                busy.push(id);
            }
        }
        if !busy.is_empty() {
            let mut colony = shared.colony.write().await;
            for id in busy {
                colony.note_cpu_active(&id, now);
            }
        }
    }
}

pub fn log_loaded(t: &Thresholds) {
    if *t != Thresholds::default() {
        log(format!("using thresholds from {}: {t:?}", thresholds_path().display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_core::{Envelope, HookPayload, HostId};

    fn agent() -> Agent {
        let mut c = colony_core::Colony::new();
        let p = HookPayload::parse(r#"{"session_id":"s1","cwd":"C:\\code\\app","hook_event_name":"Stop","last_assistant_message":"All done."}"#).unwrap();
        c.apply(&Envelope::from_hook(HostId::Windows, 5_000, &p).unwrap());
        c.agents["s1"].clone()
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("colony-quality-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn feedback_is_appended_as_lines_with_state_reason_and_expectation() {
        let dir = temp("fb");
        let path = dir.join("feedback.jsonl");
        let a = agent();
        assert_eq!(a.state, AgentState::ReadyToReview);
        record_feedback(&path, &a, "awaiting_reply", 9_000).unwrap();
        record_feedback(&path, &a, "working", 9_500).unwrap();
        let lines: Vec<serde_json::Value> =
            std::fs::read_to_string(&path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["session_id"], "s1");
        assert_eq!(lines[0]["state"], "ready_to_review");
        assert_eq!(lines[0]["should_be"], "awaiting_reply");
        assert_eq!(lines[0]["reason"], "All done.");
        assert!(lines[0]["basis"].as_str().unwrap().contains("Rule"));
        assert_eq!(lines[1]["should_be"], "working");
    }

    #[test]
    fn nonsense_feedback_is_refused() {
        let dir = temp("bad");
        let path = dir.join("feedback.jsonl");
        let a = agent();
        assert!(record_feedback(&path, &a, "banana", 1).is_err());
        assert!(record_feedback(&path, &a, "ready_to_review", 1).is_err(), "same as the current state");
        assert!(!path.exists());
    }

    #[test]
    fn thresholds_file_overrides_and_missing_file_means_defaults() {
        let dir = temp("th");
        assert_eq!(load_thresholds(&dir.join("none.json")), Thresholds::default());
        let p = dir.join("thresholds.json");
        std::fs::write(&p, r#"{"stall_ms": 900000, "question_tail_chars": 600}"#).unwrap();
        let t = load_thresholds(&p);
        assert_eq!((t.stall_ms, t.question_tail_chars), (900_000, 600));
        assert_eq!(t.tool_stall_ms, Thresholds::default().tool_stall_ms);
    }

    #[test]
    fn settings_default_off_and_round_trip() {
        let dir = temp("set");
        let p = dir.join("settings.json");
        assert!(!Settings::load(&p).haiku_classifier);
        Settings { haiku_classifier: true, anthropic_api_key: Some("k".into()) }.save(&p).unwrap();
        let s = Settings::load(&p);
        assert!(s.haiku_classifier);
        assert_eq!(s.anthropic_api_key.as_deref(), Some("k"));
        // The map never sees the key itself.
        assert!(!s.public().to_string().contains("\"k\""));
    }

    #[test]
    fn computing_means_cpu_time_rose() {
        assert!(!is_computing(None, 5_000));
        assert!(!is_computing(Some(5_000), 5_050));
        assert!(is_computing(Some(5_000), 5_400));
        assert!(!is_computing(Some(5_400), 5_000), "a child exiting lowers the total");
    }
}
