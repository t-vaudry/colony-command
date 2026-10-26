//! Budget limits: when a spend cap is close or reached, and which step down a
//! bot takes when it is. Pure decisions; colonyd does the watching and acting.
//!
//! The step order follows what each model costs: the advisor goes first (the
//! main model keeps working alone), then the main model moves down a tier. The
//! swarm is already the cheap part, so it stays.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::models::{family, Family};
use crate::usage::{ProjectSpend, BUCKET_MS};

/// Share of a cap at which the first warning comes.
pub const WARN_AT: f64 = 0.8;
pub const DAY_MS: u64 = 24 * 60 * 60_000;

/// What a bot gives up when it reaches its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Stop consulting the advisor.
    DropAdvisor,
    /// Run the main model one tier down (an alias: "opus", "sonnet", "haiku").
    LowerModel(&'static str),
    /// Nothing cheaper to move to: notice only.
    Floor,
}

/// The next notch down for a bot on `model` with `advisor`. One notch only:
/// the caller asks again if the budget is hit again.
pub fn step_down(model: Option<&str>, advisor: Option<&str>) -> Step {
    if advisor.is_some() {
        return Step::DropAdvisor;
    }
    match model.and_then(family) {
        Some(Family::Fable) => Step::LowerModel("opus"),
        Some(Family::Opus) => Step::LowerModel("sonnet"),
        Some(Family::Sonnet) => Step::LowerModel("haiku"),
        // Haiku is the bottom, and an unknown model has no known tier below it.
        Some(Family::Haiku) | None => Step::Floor,
    }
}

/// What crossing a cap asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crossing {
    Nothing,
    /// Past `WARN_AT` of the cap, not yet at it.
    Warn,
    /// At or over the cap.
    Hit,
}

/// The caps already announced, so each crossing is handled once. A mark names
/// the cap it was set for: when the cap changes, its crossings count afresh.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    #[serde(default)]
    pub warned: Option<f64>,
    #[serde(default)]
    pub hit: Option<f64>,
}

impl Mark {
    /// Compare `spent` with `cap` and remember what was reported. A jump
    /// straight past the cap reports the hit only.
    pub fn cross(&mut self, spent: f64, cap: f64) -> Crossing {
        if !(cap > 0.0) {
            return Crossing::Nothing;
        }
        if spent >= cap && self.hit != Some(cap) {
            self.settle(cap);
            Crossing::Hit
        } else if spent >= cap * WARN_AT && spent < cap && self.warned != Some(cap) {
            self.warned = Some(cap);
            Crossing::Warn
        } else {
            Crossing::Nothing
        }
    }

    /// Count both crossings of `cap` as seen (after the user moved the cap).
    pub fn settle(&mut self, cap: f64) {
        self.warned = Some(cap);
        self.hit = Some(cap);
    }
}

/// A cap raised by the "Raise cap" button: half as much again, and always
/// room for what is already spent. Whole dollars.
pub fn raised(cap: f64, spent: f64) -> f64 {
    (cap * 1.5).max(spent * 1.2).ceil()
}

/// The cap "Undo" sets: 20% over what is spent, so the bot isn't stepped down
/// again at once. Rounded up to the cent.
pub fn headroom(spent: f64) -> f64 {
    (spent * 1.2 * 100.0).ceil() / 100.0
}

/// Day number in the user's time zone (`utc_offset_min` east of UTC).
pub fn local_day(now_ms: u64, utc_offset_min: i32) -> i64 {
    (now_ms as i64 + utc_offset_min as i64 * 60_000).div_euclid(DAY_MS as i64)
}

/// When the user's local day began, in unix ms.
pub fn local_midnight(now_ms: u64, utc_offset_min: i32) -> u64 {
    (local_day(now_ms, utc_offset_min) * DAY_MS as i64 - utc_offset_min as i64 * 60_000).max(0) as u64
}

/// Spend of all projects since `from_ms`, in whole ledger buckets like the
/// map's day total; the flag says some of it had no known price.
pub fn spent_since(spend: &BTreeMap<String, ProjectSpend>, from_ms: u64) -> (f64, bool) {
    let from = from_ms / BUCKET_MS;
    let mut usd = 0.0;
    let mut partial = false;
    for p in spend.values() {
        for (_, s) in p.buckets.range(from..) {
            usd += s.cost_usd;
            partial |= s.partial;
        }
    }
    (usd, partial)
}

/// "$5", "$4.02".
pub fn money(usd: f64) -> String {
    if (usd * 100.0).round() % 100.0 == 0.0 {
        format!("${usd:.0}")
    } else {
        format!("${usd:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::{check_pairing, Resolved};
    use crate::usage::Spend;

    #[test]
    fn an_advisor_goes_before_the_main_model_moves() {
        for model in [Some("opus"), Some("sonnet"), Some("haiku"), Some("fable"), None] {
            assert_eq!(step_down(model, Some("opus")), Step::DropAdvisor, "{model:?}");
        }
    }

    #[test]
    fn without_an_advisor_the_main_model_moves_one_tier() {
        assert_eq!(step_down(Some("fable"), None), Step::LowerModel("opus"));
        assert_eq!(step_down(Some("opus"), None), Step::LowerModel("sonnet"));
        assert_eq!(step_down(Some("claude-sonnet-5-5"), None), Step::LowerModel("haiku"));
        assert_eq!(step_down(Some("claude-opus-5-5[1m]"), None), Step::LowerModel("sonnet"));
        assert_eq!(step_down(Some("haiku"), None), Step::Floor);
        assert_eq!(step_down(Some("claude-haiku-5-5"), None), Step::Floor);
        // A model Colony doesn't know the tier of, or none chosen: nothing to do.
        assert_eq!(step_down(Some("gpt"), None), Step::Floor);
        assert_eq!(step_down(None, None), Step::Floor);
    }

    #[test]
    fn every_combination_steps_to_a_pairing_that_works() {
        let alone = |model: &str| Resolved { crew: None, model: Some(model.into()), advisor: None, effort: None, swarm_model: Some("haiku".into()) };
        for m in ["fable", "opus", "sonnet", "haiku"] {
            for a in [None, Some("fable"), Some("opus"), Some("sonnet")] {
                // Only combinations that could be running.
                if a.is_some_and(|a| check_pairing(m, a).is_err()) {
                    continue;
                }
                let after = match step_down(Some(m), a) {
                    Step::DropAdvisor => alone(m),
                    Step::LowerModel(to) => alone(to),
                    Step::Floor => continue,
                };
                assert!(after.check_pairing().is_ok(), "{m}/{a:?} -> {after:?}");
                // The swarm stays whatever happens.
                assert_eq!(after.swarm_model.as_deref(), Some("haiku"));
                // Stepping again always ends at the floor: no loop.
                let mut model = after.model.clone();
                for _ in 0..4 {
                    if let Step::LowerModel(to) = step_down(model.as_deref(), None) {
                        model = Some(to.into());
                    }
                }
                assert_eq!(step_down(model.as_deref(), None), Step::Floor, "{m}/{a:?}");
            }
        }
    }

    #[test]
    fn the_default_crew_drops_its_advisor_then_moves_down_then_stops() {
        assert_eq!(step_down(Some("sonnet"), Some("opus")), Step::DropAdvisor);
        assert_eq!(step_down(Some("sonnet"), None), Step::LowerModel("haiku"));
        assert_eq!(step_down(Some("haiku"), None), Step::Floor);
    }

    #[test]
    fn a_cap_is_warned_about_once_and_hit_once() {
        let mut m = Mark::default();
        assert_eq!(m.cross(3.0, 5.0), Crossing::Nothing);
        assert_eq!(m.cross(4.0, 5.0), Crossing::Warn);
        assert_eq!(m.cross(4.5, 5.0), Crossing::Nothing);
        assert_eq!(m.cross(5.0, 5.0), Crossing::Hit);
        assert_eq!(m.cross(6.0, 5.0), Crossing::Nothing);
    }

    #[test]
    fn jumping_past_the_cap_skips_the_warning() {
        let mut m = Mark::default();
        assert_eq!(m.cross(7.0, 5.0), Crossing::Hit);
        assert_eq!(m.cross(7.5, 5.0), Crossing::Nothing);
    }

    #[test]
    fn a_new_cap_counts_afresh() {
        let mut m = Mark::default();
        m.cross(5.0, 5.0);
        assert_eq!(m.cross(5.5, 10.0), Crossing::Nothing);
        assert_eq!(m.cross(8.0, 10.0), Crossing::Warn);
        assert_eq!(m.cross(10.0, 10.0), Crossing::Hit);
        m.settle(20.0);
        assert_eq!(m.cross(16.0, 20.0), Crossing::Nothing);
        // No cap, no crossing.
        assert_eq!(Mark::default().cross(99.0, 0.0), Crossing::Nothing);
    }

    #[test]
    fn raising_leaves_room() {
        assert_eq!(raised(5.0, 5.0), 8.0);
        assert_eq!(raised(5.0, 9.0), 11.0);
        assert_eq!(raised(0.5, 0.5), 1.0);
        assert_eq!(headroom(5.0), 6.0);
        assert_eq!(headroom(4.03), 4.84);
    }

    #[test]
    fn money_drops_empty_cents() {
        assert_eq!(money(5.0), "$5");
        assert_eq!(money(4.02), "$4.02");
        assert_eq!(money(15.996), "$16");
    }

    #[test]
    fn the_local_day_starts_at_the_users_midnight() {
        let now = 1_700_000_000_000u64;
        // UTC-5: local midnight comes five hours after UTC midnight.
        let m = local_midnight(now, -300);
        assert!(m <= now && now - m < DAY_MS);
        assert_eq!((m - 300 * 60_000) % DAY_MS, 0);
        assert_eq!(local_midnight(now, 0) % DAY_MS, 0);
        assert_eq!(local_day(m, -300), local_day(now, -300));
        assert_eq!(local_day(m - 1, -300), local_day(now, -300) - 1);
        // A :45 zone still lands on a ledger bucket edge.
        assert_eq!(local_midnight(now, 345) % BUCKET_MS, 0);
    }

    #[test]
    fn the_day_total_counts_whole_buckets_since_midnight() {
        let midnight = 100 * DAY_MS;
        let mut p = ProjectSpend::default();
        p.buckets.insert(midnight / BUCKET_MS - 1, Spend { cost_usd: 9.0, ..Default::default() });
        p.buckets.insert(midnight / BUCKET_MS, Spend { cost_usd: 2.0, ..Default::default() });
        p.buckets.insert(midnight / BUCKET_MS + 5, Spend { cost_usd: 1.5, partial: true, ..Default::default() });
        let spend = BTreeMap::from([("a".to_string(), p.clone()), ("b".to_string(), p)]);
        assert_eq!(spent_since(&spend, midnight), (7.0, true));
    }
}
