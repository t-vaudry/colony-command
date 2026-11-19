//! Push notifications to a phone or tablet that has the app closed: a session needs approval,
//! is waiting for a reply, is blocked, or is done.
//!
//! The channel is ntfy (https://ntfy.sh), which has an Android app and can be self-hosted:
//! colonyd POSTs a short text to one topic URL. Off unless `ntfy_url` is set in
//! `~/.colony/settings.json`. A notification says only which project and what kind of
//! wait. It never carries a prompt, a command, a reply, a file path or a session title:
//! those can hold secrets, and a notification passes through the topic's server and the
//! phone's lock screen.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use colony_core::state::{Agent, AgentKind, AgentState};

use crate::Shared;

const POLL: Duration = Duration::from_secs(2);
const TIMEOUT: Duration = Duration::from_secs(10);

/// One notification. `title` and `body` are fixed phrases plus the project's folder name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    /// ntfy priority, 1 (min) to 5 (max).
    pub priority: u8,
    /// ntfy tag (an emoji short code).
    pub tag: &'static str,
}

/// What a bot's arrival in `state` is worth telling someone away from the PC, if anything.
fn kind_of(state: AgentState) -> Option<(&'static str, &'static str, u8, &'static str)> {
    match state {
        AgentState::NeedsInput => Some(("Needs your approval", "A permission request is waiting.", 4, "lock")),
        AgentState::AwaitingReply => Some(("Waiting for your reply", "It asked a question.", 4, "speech_balloon")),
        AgentState::Blocked => Some(("Blocked", "It is stuck and needs a look.", 4, "warning")),
        AgentState::ReadyToReview => Some(("Done", "Work is ready to review.", 3, "white_check_mark")),
        _ => None,
    }
}

/// The notification for `a` having just entered its current state. Only the project's folder
/// name is used from the agent; see the module comment for why nothing else is.
pub fn notice_for(a: &Agent) -> Option<Notice> {
    let (title, body, priority, tag) = kind_of(a.state)?;
    let project = a.project_name.as_deref().filter(|p| !p.trim().is_empty()).unwrap_or("a session");
    Some(Notice { title: format!("{project}: {}", title.to_lowercase()), body: body.into(), priority, tag })
}

/// Remembers each bot's last state, so only a change is announced.
#[derive(Default)]
pub struct Tracker {
    seen: HashMap<String, AgentState>,
    primed: bool,
}

impl Tracker {
    /// The notices for bots that entered a notifiable state since the last call. The first call
    /// only learns what is already going on: a restart must not announce every waiting bot again.
    pub fn changes<'a>(&mut self, agents: impl Iterator<Item = &'a Agent>) -> Vec<Notice> {
        let mut out = Vec::new();
        let mut now = HashMap::new();
        for a in agents.filter(|a| a.kind == AgentKind::Main) {
            let before = self.seen.get(&a.id).copied();
            if self.primed && before != Some(a.state) {
                out.extend(notice_for(a));
            }
            now.insert(a.id.clone(), a.state);
        }
        self.seen = now;
        self.primed = true;
        out
    }
}

/// The `ntfy_url` setting from what the user typed: empty turns alerts off, anything else must be
/// an http(s) URL (a topic on your own server, or on ntfy.sh).
pub fn clean_url(text: &str) -> Result<Option<String>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let rest = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).ok_or("The topic address must start with http:// or https://.")?;
    if rest.is_empty() || rest.starts_with('/') || t.chars().any(|c| c.is_whitespace() || c.is_control()) || t.len() > 500 {
        return Err("That doesn't look like a topic address, like https://ntfy.sh/my-secret-topic.".into());
    }
    Ok(Some(t.to_string()))
}

/// POST one notice to an ntfy topic URL.
pub fn send(url: &str, n: &Notice) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).build().into();
    agent
        .post(url)
        .header("Title", &n.title)
        .header("Priority", &n.priority.to_string())
        .header("Tags", n.tag)
        .send(n.body.as_str())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub async fn run(shared: Arc<Shared>) {
    let mut tracker = Tracker::default();
    loop {
        tokio::time::sleep(POLL).await;
        let notices = {
            let colony = shared.colony.read().await;
            tracker.changes(colony.agents.values())
        };
        if notices.is_empty() {
            continue;
        }
        let url = shared.settings.lock().unwrap().ntfy_url.clone().filter(|u| !u.trim().is_empty());
        // The app in the foreground already shows it.
        let Some(url) = url.filter(|_| !shared.approvals.remote_in_foreground()) else { continue };
        for n in notices {
            let url = url.clone();
            let sent = tokio::task::spawn_blocking(move || send(&url, &n)).await;
            if let Ok(Err(e)) = sent {
                crate::log(format!("push notification not sent: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, state: AgentState) -> Agent {
        use colony_core::{Envelope, HookPayload, HostId};
        let mut c = colony_core::Colony::new();
        let p = HookPayload::parse(r#"{"session_id":"s1","cwd":"C:\\code\\billing-api","hook_event_name":"Stop","last_assistant_message":"All done."}"#).unwrap();
        c.apply(&Envelope::from_hook(HostId::Windows, 5_000, &p).unwrap());
        let mut a = c.agents["s1"].clone();
        a.id = id.into();
        a.name = "secret session title".into();
        a.project_name = Some("billing-api".into());
        a.state = state;
        a.reason = Some("rm -rf /secret and the prompt text".into());
        a
    }

    #[test]
    fn says_what_kind_of_wait_and_which_project() {
        let n = notice_for(&agent("a", AgentState::NeedsInput)).unwrap();
        assert_eq!(n.title, "billing-api: needs your approval");
        assert_eq!(notice_for(&agent("a", AgentState::ReadyToReview)).unwrap().title, "billing-api: done");
        assert!(notice_for(&agent("a", AgentState::Working)).is_none());
        assert!(notice_for(&agent("a", AgentState::Idle)).is_none());
    }

    #[test]
    fn carries_no_prompt_command_or_title_text() {
        for s in [AgentState::NeedsInput, AgentState::AwaitingReply, AgentState::Blocked, AgentState::ReadyToReview] {
            let n = notice_for(&agent("a", s)).unwrap();
            let all = format!("{} {}", n.title, n.body);
            assert!(!all.contains("secret"), "{all}");
            assert!(!all.contains("rm -rf"), "{all}");
        }
    }

    #[test]
    fn announces_a_change_once_and_not_what_was_already_there() {
        let mut t = Tracker::default();
        let waiting = [agent("a", AgentState::NeedsInput)];
        assert!(t.changes(waiting.iter()).is_empty(), "the first look only learns");
        assert!(t.changes(waiting.iter()).is_empty(), "no change");
        let working = [agent("a", AgentState::Working)];
        assert!(t.changes(working.iter()).is_empty());
        assert_eq!(t.changes(waiting.iter()).len(), 1, "back to waiting is news");
        assert!(t.changes(waiting.iter()).is_empty(), "once");
    }

    #[test]
    fn a_new_bot_already_blocked_is_announced() {
        let mut t = Tracker::default();
        t.changes([agent("a", AgentState::Working)].iter());
        let both = [agent("a", AgentState::Working), agent("b", AgentState::Blocked)];
        assert_eq!(t.changes(both.iter()).len(), 1);
    }

    #[test]
    fn a_typed_topic_is_checked_and_blank_turns_alerts_off() {
        assert_eq!(clean_url(" https://ntfy.sh/abc ").unwrap().as_deref(), Some("https://ntfy.sh/abc"));
        assert_eq!(clean_url("http://100.64.0.1:8080/t").unwrap().as_deref(), Some("http://100.64.0.1:8080/t"));
        assert_eq!(clean_url("").unwrap(), None);
        for bad in ["ntfy.sh/abc", "ftp://x/y", "https://", "https:///x", "https://a b/c"] {
            assert!(clean_url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn subagents_are_not_announced() {
        let mut t = Tracker::default();
        t.changes(std::iter::empty());
        let mut sub = agent("s", AgentState::Blocked);
        sub.kind = AgentKind::Subagent;
        assert!(t.changes([sub].iter()).is_empty());
    }
}
