//! Patience: how long an item may wait for a human before Colony taps them on
//! the shoulder with an OS notification.
//!
//! Pure logic. The map hands the items on the porch (with their `state_since`
//! from the daemon) to a [`Notifier`], which says which have waited past their
//! budget and have not been announced yet. One notification per item per wait,
//! however long it lasts; an item that is answered and later waits again
//! starts over.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::AgentState;

const MIN_MS: u64 = 60_000;
/// How long an announced wait is remembered for a bot that has vanished from the list.
const KEEP_MS: u64 = 24 * 60 * MIN_MS;
/// More than this many due at once collapse into one summary notification.
pub const MAX_INDIVIDUAL: usize = 3;

/// What an item is waiting for, which decides its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Permission,
    Question,
    Review,
}

impl Kind {
    /// `None` for states that are not waiting on the human. A blocked or
    /// crashed bot needs a decision like a question does.
    pub fn of(state: AgentState) -> Option<Kind> {
        match state {
            AgentState::NeedsInput => Some(Kind::Permission),
            AgentState::AwaitingReply | AgentState::Blocked | AgentState::Crashed => Some(Kind::Question),
            AgentState::ReadyToReview => Some(Kind::Review),
            _ => None,
        }
    }
}

/// The user's settings. Budgets are in minutes; 0 turns that kind off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub permission_min: u32,
    pub question_min: u32,
    pub review_min: u32,
}

impl Default for Settings {
    fn default() -> Self {
        // A permission request stalls a bot outright; a question usually
        // does too but is worth thinking about; finished work can sit.
        Settings { enabled: true, permission_min: 5, question_min: 15, review_min: 60 }
    }
}

impl Settings {
    /// The budget for a kind, or `None` when it is off.
    pub fn budget_ms(&self, kind: Kind) -> Option<u64> {
        let min = match kind {
            Kind::Permission => self.permission_min,
            Kind::Question => self.question_min,
            Kind::Review => self.review_min,
        };
        (self.enabled && min > 0).then(|| u64::from(min) * MIN_MS)
    }
}

/// One thing on the porch, as the map sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub project: Option<String>,
    pub state: AgentState,
    pub state_since: u64,
}

/// An item that has waited past its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub id: String,
    pub name: String,
    pub project: Option<String>,
    pub kind: Kind,
    pub waited_ms: u64,
}

/// What to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub title: String,
    pub body: String,
    /// The bot to select when the user comes back to the map; none for a summary.
    pub agent: Option<String>,
}

/// Remembers which waits have been announced.
#[derive(Debug, Default)]
pub struct Notifier {
    /// Item id -> the `state_since` of the wait that was announced.
    announced: HashMap<String, u64>,
}

impl Notifier {
    /// The items to announce now, longest wait first. While `attended` (the
    /// map window has focus: the porch is always on screen) nothing fires and
    /// nothing is marked, so an item that is still overdue when the user looks
    /// away is announced then.
    pub fn due(&mut self, now: u64, items: &[Item], settings: &Settings, attended: bool) -> Vec<Due> {
        // Forget a wait once the bot is seen not waiting, so its next wait is announced.
        // A bot missing from the list (dropped and re-added by a reconnect) keeps its
        // entry, so the same wait isn't announced twice; old entries age out.
        let not_waiting: HashSet<&str> = items.iter().filter(|i| Kind::of(i.state).is_none()).map(|i| i.id.as_str()).collect();
        self.announced.retain(|id, since| !not_waiting.contains(id.as_str()) && now.saturating_sub(*since) < KEEP_MS);
        if attended {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in items {
            let Some(kind) = Kind::of(i.state) else { continue };
            let Some(budget) = settings.budget_ms(kind) else { continue };
            let waited = now.saturating_sub(i.state_since);
            if waited < budget || self.announced.get(&i.id) == Some(&i.state_since) {
                continue;
            }
            self.announced.insert(i.id.clone(), i.state_since);
            out.push(Due { id: i.id.clone(), name: i.name.clone(), project: i.project.clone(), kind, waited_ms: waited });
        }
        out.sort_by(|a, b| b.waited_ms.cmp(&a.waited_ms));
        out
    }
}

/// "7 min", "1 h 5 min".
pub fn duration_text(ms: u64) -> String {
    let min = ms / MIN_MS;
    if min < 60 {
        format!("{} min", min.max(1))
    } else if min % 60 == 0 {
        format!("{} h", min / 60)
    } else {
        format!("{} h {} min", min / 60, min % 60)
    }
}

/// The notifications for what is due: one each, or a single summary when
/// there are many (a long weekend should not be a wall of toasts).
pub fn compose(due: &[Due]) -> Vec<Notice> {
    if due.len() > MAX_INDIVIDUAL {
        let longest = due.iter().map(|d| d.waited_ms).max().unwrap_or(0);
        return vec![Notice {
            title: format!("{} bots are waiting for you", due.len()),
            body: format!("The longest has waited {}.", duration_text(longest)),
            agent: None,
        }];
    }
    due.iter()
        .map(|d| {
            let what = match d.kind {
                Kind::Permission => "needs your permission",
                Kind::Question => "has a question for you",
                Kind::Review => "finished and is ready to review",
            };
            let place = d.project.as_deref().map(|p| format!(" in {p}")).unwrap_or_default();
            Notice {
                title: format!("{} {what}", d.name),
                body: format!("Waiting {}{place}.", duration_text(d.waited_ms)),
                agent: Some(d.id.clone()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, state: AgentState, since: u64) -> Item {
        Item { id: id.into(), name: id.into(), project: None, state, state_since: since }
    }

    const M: u64 = MIN_MS;

    #[test]
    fn budgets_differ_by_kind() {
        let mut n = Notifier::default();
        let s = Settings::default();
        let items = [item("p", AgentState::NeedsInput, 0), item("q", AgentState::AwaitingReply, 0), item("r", AgentState::ReadyToReview, 0)];
        let ids = |d: Vec<Due>| d.into_iter().map(|d| d.id).collect::<Vec<_>>();
        assert!(n.due(4 * M, &items, &s, false).is_empty());
        assert_eq!(ids(n.due(5 * M, &items, &s, false)), ["p"]);
        assert_eq!(ids(n.due(15 * M, &items, &s, false)), ["q"]);
        assert_eq!(ids(n.due(60 * M, &items, &s, false)), ["r"]);
    }

    #[test]
    fn announces_once_per_wait() {
        let mut n = Notifier::default();
        let s = Settings::default();
        let waiting = [item("p", AgentState::NeedsInput, 0)];
        assert_eq!(n.due(6 * M, &waiting, &s, false).len(), 1);
        assert!(n.due(7 * M, &waiting, &s, false).is_empty());
        assert!(n.due(600 * M, &waiting, &s, false).is_empty());
        // Answered, then waiting again: a new wait.
        n.due(601 * M, &[item("p", AgentState::Working, 601 * M)], &s, false);
        let again = [item("p", AgentState::NeedsInput, 602 * M)];
        assert!(n.due(605 * M, &again, &s, false).is_empty());
        assert_eq!(n.due(607 * M, &again, &s, false).len(), 1);
    }

    #[test]
    fn a_new_wait_with_the_item_still_listed_is_announced_again() {
        let mut n = Notifier::default();
        let s = Settings::default();
        assert_eq!(n.due(6 * M, &[item("p", AgentState::NeedsInput, 0)], &s, false).len(), 1);
        // Straight from a permission to a question, so never seen as not waiting.
        assert_eq!(n.due(30 * M, &[item("p", AgentState::AwaitingReply, 10 * M)], &s, false).len(), 1);
    }

    #[test]
    fn a_bot_dropped_and_re_added_keeps_its_announcement() {
        let mut n = Notifier::default();
        let s = Settings::default();
        let waiting = [item("p", AgentState::NeedsInput, 0)];
        assert_eq!(n.due(6 * M, &waiting, &s, false).len(), 1);
        // A reconnect empties the list for a moment, then the same wait is back.
        assert!(n.due(7 * M, &[], &s, false).is_empty());
        assert!(n.due(8 * M, &waiting, &s, false).is_empty());
    }

    #[test]
    fn quiet_while_attended_then_announced_when_the_user_looks_away() {
        let mut n = Notifier::default();
        let s = Settings::default();
        let waiting = [item("p", AgentState::NeedsInput, 0)];
        assert!(n.due(20 * M, &waiting, &s, true).is_empty());
        assert_eq!(n.due(21 * M, &waiting, &s, false).len(), 1);
    }

    #[test]
    fn settings_turn_kinds_or_everything_off() {
        let mut n = Notifier::default();
        let waiting = [item("p", AgentState::NeedsInput, 0), item("r", AgentState::ReadyToReview, 0)];
        let s = Settings { enabled: true, permission_min: 0, review_min: 10, ..Settings::default() };
        let due = n.due(11 * M, &waiting, &s, false);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].kind, Kind::Review);
        let off = Settings { enabled: false, ..Settings::default() };
        assert!(Notifier::default().due(999 * M, &waiting, &off, false).is_empty());
    }

    #[test]
    fn not_waiting_states_never_fire() {
        let mut n = Notifier::default();
        let items = [item("w", AgentState::Working, 0), item("i", AgentState::Idle, 0), item("e", AgentState::Ended, 0)];
        assert!(n.due(999 * M, &items, &Settings::default(), false).is_empty());
    }

    #[test]
    fn blocked_and_crashed_use_the_question_budget() {
        assert_eq!(Kind::of(AgentState::Blocked), Some(Kind::Question));
        assert_eq!(Kind::of(AgentState::Crashed), Some(Kind::Question));
    }

    #[test]
    fn settings_tolerate_missing_fields() {
        let s: Settings = serde_json::from_str(r#"{"question_min": 30}"#).unwrap();
        assert_eq!((s.enabled, s.permission_min, s.question_min, s.review_min), (true, 5, 30, 60));
    }

    #[test]
    fn composes_individual_notices_or_a_summary() {
        let d = |id: &str, kind, waited_ms| Due { id: id.into(), name: format!("Atlas {id}"), project: Some("api".into()), kind, waited_ms };
        let one = compose(&[d("1", Kind::Question, 16 * M)]);
        assert_eq!(one[0].title, "Atlas 1 has a question for you");
        assert_eq!(one[0].body, "Waiting 16 min in api.");
        assert_eq!(one[0].agent.as_deref(), Some("1"));
        let many: Vec<_> = (0..4).map(|i| d(&i.to_string(), Kind::Review, (70 + i) * M)).collect();
        let s = compose(&many);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].title, "4 bots are waiting for you");
        assert_eq!(s[0].body, "The longest has waited 1 h 13 min.");
        assert_eq!(s[0].agent, None);
    }

    #[test]
    fn duration_text_reads_naturally() {
        assert_eq!(duration_text(30_000), "1 min");
        assert_eq!(duration_text(59 * M), "59 min");
        assert_eq!(duration_text(120 * M), "2 h");
    }
}
