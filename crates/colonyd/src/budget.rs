//! Budget limits: watch spend against the caps in the settings, warn at 80%,
//! and at 100% step bots down a notch (see `colony_core::budget`).
//!
//! Only bots Colony started can be stepped down, since that takes a restart
//! (`api::restart_with`). It waits for a safe point between turns, so no work
//! is cut off, and it is one notch per crossing. Which caps were already
//! announced, and which steps wait for a turn to end, are kept in
//! `~/.colony/budget.json`, so a restart of colonyd neither repeats a toast
//! nor steps a bot twice.
//!
//! The watcher listens to the deltas the map gets, so it works when usage
//! moves and when a bot goes idle, and does nothing in between.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use colony_core::budget::{self, Crossing, Mark, Step};
use colony_core::crew::{self, Resolved};
use colony_core::models::family;
use colony_core::{AgentKind, AgentState, DomainEvent, Envelope};
use colony_source::{colony_home, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{log, Shared};

/// Replayed history is still arriving for this long after the daemon starts;
/// nothing is announced until it has settled.
const SETTLE: Duration = Duration::from_secs(20);
/// Usage arrives in bursts: look once per this long at most.
const COALESCE: Duration = Duration::from_secs(1);
/// Steps taken are remembered (for Undo) this long.
const KEEP_MS: u64 = 3 * 24 * 60 * 60 * 1000;
/// Largest cap accepted, in USD.
const MAX_CAP: f64 = 1_000_000.0;

/// Which cap a crossing was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Bot,
    Day,
}

/// A step taken, with the models to go back to if the user says Undo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stepped {
    pub scope: Scope,
    /// The bot's own `--model` and `--advisor` choices before the step.
    pub model: Option<String>,
    pub advisor: Option<String>,
    pub at: u64,
}

/// The local day a day cap's marks belong to.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayMark {
    pub day: i64,
    pub mark: Mark,
}

/// Everything that outlives the daemon.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Per-bot caps already announced, by agent id.
    pub bots: BTreeMap<String, Mark>,
    pub day: DayMark,
    /// Bots to step down at their next safe point.
    pub pending: BTreeMap<String, Scope>,
    pub stepped: BTreeMap<String, Stepped>,
}

pub fn state_path() -> PathBuf {
    colony_home().join("budget.json")
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).expect("serializes"))?;
        std::fs::rename(&tmp, path)
    }
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.get_or_init(|| Mutex::new(State::load(&state_path()))).lock().unwrap_or_else(|e| e.into_inner())
}

/// What the watcher needs to know about a bot.
#[derive(Debug, Clone)]
pub struct Bot {
    pub id: String,
    pub name: String,
    /// Its cost so far, subagents and advisor included.
    pub cost: f64,
    /// Colony started it, so it can be restarted.
    pub colony: bool,
    /// What it would give up next; `Floor` when there is nothing to change.
    pub step: Step,
    /// Between turns with nothing running, so a restart cuts nothing off.
    pub safe: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    pub bot: Option<f64>,
    pub day: Option<f64>,
}

/// All bots' spend since local midnight.
#[derive(Debug, Clone, Copy)]
pub struct Day {
    pub number: i64,
    pub spent: f64,
}

/// A button on a notice: `command` is sent back to colonyd as it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub label: &'static str,
    pub command: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub message: String,
    pub buttons: Vec<Button>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub notices: Vec<Notice>,
    /// Bots at a safe point with a step waiting.
    pub apply: Vec<(String, Scope)>,
}

fn raise_button(scope: Scope, id: &str) -> Button {
    Button { label: "Raise cap", command: json!({ "type": "raise_budget", "scope": scope, "id": id }) }
}

/// Decide what the new readings call for, updating `st` with what was announced
/// or queued. No I/O: the caller shows the notices and restarts the bots.
pub fn plan(st: &mut State, caps: Caps, day: Day, bots: &[Bot]) -> Plan {
    let mut out = Plan::default();

    // A new local day starts the day cap afresh; yesterday's queued steps lapse.
    if st.day.day != day.number {
        st.day = DayMark { day: day.number, mark: Mark::default() };
        st.pending.retain(|_, s| *s != Scope::Day);
    }
    // A step waits only while its cap is still exceeded, and its bot is still ours.
    st.pending.retain(|id, scope| {
        let Some(b) = bots.iter().find(|b| b.id == *id && b.colony) else { return false };
        match scope {
            Scope::Bot => caps.bot.is_some_and(|cap| b.cost >= cap),
            Scope::Day => caps.day.is_some_and(|cap| day.spent >= cap),
        }
    });

    if let Some(cap) = caps.bot {
        for b in bots {
            match st.bots.entry(b.id.clone()).or_default().cross(b.cost, cap) {
                Crossing::Nothing => {}
                Crossing::Warn => out.notices.push(Notice {
                    message: format!("{} has used {} of its {} budget", b.name, budget::money(b.cost), budget::money(cap)),
                    buttons: vec![raise_button(Scope::Bot, &b.id)],
                }),
                Crossing::Hit => {
                    let spent = budget::money(b.cost);
                    let message = if !b.colony {
                        format!("{} has reached its {} budget ({spent}). Colony didn't start it, so nothing was changed.", b.name, budget::money(cap))
                    } else if b.step == Step::Floor {
                        format!("{} has reached its {} budget ({spent}) and is already on the cheapest setup.", b.name, budget::money(cap))
                    } else {
                        st.pending.entry(b.id.clone()).or_insert(Scope::Bot);
                        format!("{} has reached its {} budget ({spent}). It steps down when this turn ends.", b.name, budget::money(cap))
                    };
                    out.notices.push(Notice { message, buttons: vec![raise_button(Scope::Bot, &b.id)] });
                }
            }
        }
    }

    if let Some(cap) = caps.day {
        match st.day.mark.cross(day.spent, cap) {
            Crossing::Nothing => {}
            Crossing::Warn => out.notices.push(Notice {
                message: format!("Today: {} of {}", budget::money(day.spent), budget::money(cap)),
                buttons: vec![raise_button(Scope::Day, "")],
            }),
            Crossing::Hit => {
                let mut queued = 0;
                let mut left = 0;
                for b in bots {
                    if b.colony && b.step != Step::Floor {
                        if !st.pending.contains_key(&b.id) {
                            st.pending.insert(b.id.clone(), Scope::Day);
                        }
                        queued += 1;
                    } else {
                        left += 1;
                    }
                }
                let what = match (queued, left) {
                    (0, 0) => "No bots are running.".to_string(),
                    (0, _) => "Nothing to step down: no running bot Colony started can go cheaper.".to_string(),
                    (n, 0) => format!("{n} {} will step down when {} turn ends.", bots_word(n), if n == 1 { "its" } else { "their" }),
                    (n, l) => format!("{n} {} will step down when their turn ends; {l} can't be changed.", bots_word(n)),
                };
                out.notices.push(Notice {
                    message: format!("Today's {} budget is reached ({} spent). {what}", budget::money(cap), budget::money(day.spent)),
                    buttons: vec![raise_button(Scope::Day, "")],
                });
            }
        }
    }

    out.apply = st.pending.iter().filter(|(id, _)| bots.iter().any(|b| b.id == **id && b.colony && b.safe)).map(|(id, s)| (id.clone(), *s)).collect();
    out
}

fn bots_word(n: usize) -> &'static str {
    if n == 1 {
        "bot"
    } else {
        "bots"
    }
}

/// "Opus", "Sonnet 5.5"-style names are not needed here: the family is enough.
fn label(model: Option<&str>) -> String {
    match model.and_then(family) {
        Some(f) => {
            let k = f.key();
            format!("{}{}", k[..1].to_uppercase(), &k[1..])
        }
        None => model.unwrap_or("its default model").to_string(),
    }
}

/// What a step did, for the activity feed and the notice.
fn describe(step: &Step, model: Option<&str>, advisor: Option<&str>) -> String {
    match step {
        Step::DropAdvisor => format!("dropped the {} advisor", label(advisor)),
        Step::LowerModel(to) => format!("moved from {} to {}", label(model), label(Some(to))),
        Step::Floor => "nothing to change".into(),
    }
}

/// The models a bot runs now: its launch request with the crew applied, and
/// the model Claude reports when the crew leaves it to Claude Code.
fn models_of(req: &crate::pty::SpawnRequest, crews: &[crew::Crew], reported: Option<&str>) -> Option<Resolved> {
    let mut r = crew::resolve(crews, &req.pick(), req.model.as_deref()).ok()?;
    if r.model.is_none() {
        r.model = reported.map(String::from);
    }
    Some(r)
}

/// Every bot the caps apply to, with what a step would change.
async fn observe(shared: &Shared, now: u64) -> (Vec<Bot>, Day) {
    let crews = crate::crews::saved();
    let colony = shared.colony.read().await;
    let off = shared.settings.lock().unwrap().utc_offset_min.unwrap_or(0);
    let (spent, _) = budget::spent_since(&colony.spend, budget::local_midnight(now, off));
    let bots = colony
        .agents
        .values()
        .filter(|a| a.kind == AgentKind::Main && a.state != AgentState::Ended && a.paused_at.is_none())
        .map(|a| {
            let resolved = a.terminal.as_deref().and_then(|t| shared.pty.request_for(t)).and_then(|req| models_of(&req, &crews, a.model.as_deref()));
            Bot {
                id: a.id.clone(),
                name: a.name.clone(),
                cost: a.cost_usd,
                colony: resolved.is_some(),
                step: resolved.map_or(Step::Floor, |r| budget::step_down(r.model.as_deref(), r.advisor.as_deref())),
                safe: crate::pause::at_safe_point(&colony, a),
            }
        })
        .collect();
    (bots, Day { number: budget::local_day(now, off), spent })
}

fn caps_of(shared: &Shared) -> Caps {
    let s = shared.settings.lock().unwrap();
    Caps { bot: s.budget_bot_usd, day: s.budget_day_usd }
}

fn show(shared: &Shared, n: Notice) {
    let actions: Vec<Value> = n.buttons.iter().map(|b| json!({ "label": b.label, "command": b.command })).collect();
    let _ = shared.deltas.send(json!({ "type": "notice", "message": n.message, "actions": actions }).to_string());
}

/// Look at the readings once: announce crossings, and step down bots that are
/// at a safe point.
pub async fn evaluate(shared: &Shared) {
    let caps = caps_of(shared);
    if caps.bot.is_none() && caps.day.is_none() && state().pending.is_empty() {
        return;
    }
    let now = now_ms();
    let (bots, day) = observe(shared, now).await;
    let (plan, snapshot) = {
        let mut st = state();
        let before = st.clone();
        let plan = plan(&mut st, caps, day, &bots);
        // Forget bots that are gone, and steps nobody can undo any more.
        st.bots.retain(|id, _| bots.iter().any(|b| b.id == *id));
        st.stepped.retain(|id, s| now.saturating_sub(s.at) < KEEP_MS && bots.iter().any(|b| b.id == *id));
        (plan, (*st != before).then(|| st.clone()))
    };
    if let Some(s) = snapshot {
        save(&s);
    }
    for n in plan.notices {
        show(shared, n);
    }
    for (id, scope) in plan.apply {
        let name = bots.iter().find(|b| b.id == id).map(|b| b.name.clone()).unwrap_or_default();
        // One try: the step is out of the queue either way, so it can't loop.
        state().pending.remove(&id);
        match step_bot(shared, &id, scope, day.spent).await {
            Ok(()) => {}
            Err(e) => {
                log(format!("budget: could not step {name} down: {e}"));
                show(shared, Notice { message: format!("{name} is over budget but could not be stepped down: {e}"), buttons: vec![raise_button(scope, &id)] });
            }
        }
        save(&state().clone());
    }
}

fn save(st: &State) {
    if let Err(e) = st.save(&state_path()) {
        log(format!("could not save budget.json: {e}"));
    }
}

/// Restart one bot a notch cheaper, and tell the user.
async fn step_bot(shared: &Shared, id: &str, scope: Scope, day_spent: f64) -> Result<(), String> {
    let (term, name, cost, reported, host) = {
        let colony = shared.colony.read().await;
        let a = colony.agents.get(id).ok_or("it is gone")?;
        (a.terminal.clone().ok_or("Colony didn't start it")?, a.name.clone(), a.cost_usd, a.model.clone(), a.host.clone())
    };
    let req = shared.pty.request_for(&term).ok_or("Colony has lost its launch settings")?;
    let now = models_of(&req, &crate::crews::saved(), reported.as_deref()).ok_or("its crew is unknown")?;
    let step = budget::step_down(now.model.as_deref(), now.advisor.as_deref());
    let to = match &step {
        Step::Floor => return Err("there is nothing cheaper to move to".into()),
        Step::LowerModel(to) => Some(to.to_string()),
        Step::DropAdvisor => None,
    };
    let edit_step = step.clone();
    let before = crate::api::restart_with(shared, id, None, to, move |req| {
        match edit_step {
            Step::DropAdvisor => req.advisor = Some(crew::NONE.into()),
            Step::LowerModel(to) => req.model = Some(to.into()),
            Step::Floor => {}
        }
        crate::crews::fit_model(req)
    })
    .await?;
    let did = describe(&step, now.model.as_deref(), now.advisor.as_deref());
    state().stepped.insert(id.to_string(), Stepped { scope, model: before.model, advisor: before.advisor, at: now_ms() });
    let (cap, spent) = match scope {
        Scope::Bot => (caps_of(shared).bot, cost),
        Scope::Day => (caps_of(shared).day, day_spent),
    };
    let why = match scope {
        Scope::Bot => format!("{name} reached its {} budget", cap.map_or("?".into(), budget::money)),
        Scope::Day => format!("Today's {} budget is reached", cap.map_or("?".into(), budget::money)),
    };
    let text = format!("{why} ({} spent): {did}.", budget::money(spent));
    let _ = shared.events.send(Envelope { ts: now_ms(), host, session_id: id.to_string(), cwd: None, event: DomainEvent::BudgetStep { text: text.clone() } }).await;
    show(
        shared,
        Notice {
            message: format!("{text} {name} carries on from where it was."),
            buttons: vec![raise_button(scope, id), Button { label: "Undo", command: json!({ "type": "undo_budget", "id": id }) }],
        },
    );
    Ok(())
}

/// Put the models back as they were before a step, and raise the cap that
/// triggered it to 20% over what is spent, so it doesn't trigger again at once.
pub async fn undo(shared: &Shared, id: &str) -> Result<(), String> {
    let stepped = state().stepped.get(id).cloned().ok_or("there is no step down to undo for that bot")?;
    let (name, cost, host) = {
        let colony = shared.colony.read().await;
        let a = colony.agents.get(id).ok_or("that bot is gone")?;
        (a.name.clone(), a.cost_usd, a.host.clone())
    };
    let (model, advisor) = (stepped.model.clone(), stepped.advisor.clone());
    let model_set = model.clone();
    crate::api::restart_with(shared, id, None, model_set, move |req| {
        req.model = model;
        req.advisor = advisor;
        Ok(None)
    })
    .await?;
    let (now, off) = (now_ms(), shared.settings.lock().unwrap().utc_offset_min.unwrap_or(0));
    let day_spent = {
        let colony = shared.colony.read().await;
        budget::spent_since(&colony.spend, budget::local_midnight(now, off)).0
    };
    let spent = if stepped.scope == Scope::Bot { cost } else { day_spent };
    let new_cap = budget::headroom(spent);
    let raised = update_caps(shared, |s| {
        let cap = if stepped.scope == Scope::Bot { &mut s.budget_bot_usd } else { &mut s.budget_day_usd };
        if cap.is_some_and(|c| c < new_cap) {
            *cap = Some(new_cap);
            true
        } else {
            false
        }
    });
    {
        let mut st = state();
        st.stepped.remove(id);
        st.pending.remove(id);
        // The new cap starts quiet: the bot is just under it by design.
        let cap = new_cap_for(shared, stepped.scope).unwrap_or(new_cap);
        match stepped.scope {
            Scope::Bot => st.bots.entry(id.to_string()).or_default().settle(cap),
            Scope::Day => st.day.mark.settle(cap),
        }
        save(&st);
    }
    let text = format!("Undid the budget step for {name}: back to its earlier models.");
    let _ = shared.events.send(Envelope { ts: now_ms(), host, session_id: id.to_string(), cwd: None, event: DomainEvent::BudgetStep { text: text.clone() } }).await;
    let note = if raised { format!(" The {} cap is now {}.", if stepped.scope == Scope::Bot { "per-bot" } else { "daily" }, budget::money(new_cap)) } else { String::new() };
    crate::api::notice(shared, format!("{text}{note}"));
    Ok(())
}

fn new_cap_for(shared: &Shared, scope: Scope) -> Option<f64> {
    let caps = caps_of(shared);
    match scope {
        Scope::Bot => caps.bot,
        Scope::Day => caps.day,
    }
}

/// Change the caps, save them and tell the maps. Returns what `f` returned.
fn update_caps(shared: &Shared, f: impl FnOnce(&mut crate::quality::Settings) -> bool) -> bool {
    let (changed, saved, public) = {
        let mut s = shared.settings.lock().unwrap();
        let changed = f(&mut s);
        (changed, s.save(&crate::quality::settings_path()), s.public())
    };
    if let Err(e) = saved {
        log(format!("could not save settings: {e}"));
    }
    let _ = shared.deltas.send(json!({ "type": "settings", "settings": public }).to_string());
    changed
}

/// A cap from the map: nothing for no cap, else a sensible amount in USD.
fn checked_cap(v: Option<f64>) -> Result<Option<f64>, String> {
    match v {
        None => Ok(None),
        Some(c) if c.is_finite() && c > 0.0 && c <= MAX_CAP => Ok(Some((c * 100.0).round() / 100.0)),
        Some(c) => Err(format!("a budget must be more than $0 and at most ${MAX_CAP}, not {c}")),
    }
}

/// Set both caps (`None` switches one off).
pub fn set_caps(shared: &Shared, bot: Option<f64>, day: Option<f64>) -> Result<(), String> {
    let (bot, day) = (checked_cap(bot)?, checked_cap(day)?);
    update_caps(shared, |s| {
        s.budget_bot_usd = bot;
        s.budget_day_usd = day;
        true
    });
    Ok(())
}

/// The "Raise cap" button: more room than what is spent so far.
pub async fn raise(shared: &Shared, scope: Scope, id: &str) -> Result<(), String> {
    let now = now_ms();
    let (bots, day) = observe(shared, now).await;
    let spent = match scope {
        Scope::Bot => bots.iter().find(|b| b.id == id).map(|b| b.cost).ok_or("that bot is gone")?,
        Scope::Day => day.spent,
    };
    let mut new_cap = 0.0;
    update_caps(shared, |s| {
        let cap = if scope == Scope::Bot { &mut s.budget_bot_usd } else { &mut s.budget_day_usd };
        let Some(old) = *cap else { return false };
        new_cap = budget::raised(old, spent);
        *cap = Some(new_cap);
        true
    });
    if new_cap > 0.0 {
        let mut st = state();
        st.pending.retain(|k, s| !(*s == scope && (scope == Scope::Day || k == id)));
        match scope {
            Scope::Bot => st.bots.entry(id.to_string()).or_default().settle(new_cap),
            Scope::Day => st.day.mark.settle(new_cap),
        }
        save(&st);
        crate::api::notice(shared, format!("The {} budget is now {}.", if scope == Scope::Bot { "per-bot" } else { "daily" }, budget::money(new_cap)));
    }
    Ok(())
}

/// The map's offset from UTC, reported when it connects.
pub fn set_clock(shared: &Shared, utc_offset_min: i32) {
    if utc_offset_min.abs() > 14 * 60 {
        return;
    }
    let mut s = shared.settings.lock().unwrap();
    if s.utc_offset_min != Some(utc_offset_min) {
        s.utc_offset_min = Some(utc_offset_min);
        if let Err(e) = s.save(&crate::quality::settings_path()) {
            log(format!("could not save settings: {e}"));
        }
    }
}

/// React to what the maps get: usage moving, bots changing state, caps changing.
pub async fn run(shared: std::sync::Arc<Shared>) {
    let mut rx = shared.deltas.subscribe();
    tokio::time::sleep(SETTLE).await;
    loop {
        match rx.recv().await {
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            _ => {}
        }
        // Let a burst finish, then look once.
        tokio::time::sleep(COALESCE).await;
        loop {
            match rx.try_recv() {
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
                _ => {}
            }
        }
        evaluate(&shared).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bot(id: &str, cost: f64) -> Bot {
        Bot { id: id.into(), name: id.into(), cost, colony: true, step: Step::DropAdvisor, safe: true }
    }

    fn day(spent: f64) -> Day {
        Day { number: 1, spent }
    }

    const BOT5: Caps = Caps { bot: Some(5.0), day: None };
    const DAY20: Caps = Caps { bot: None, day: Some(20.0) };

    #[test]
    fn warns_at_80_percent_once_with_the_spend_in_the_words() {
        let mut st = State::default();
        let p = plan(&mut st, BOT5, day(0.0), &[bot("a", 4.02)]);
        assert_eq!(p.notices.len(), 1);
        assert_eq!(p.notices[0].message, "a has used $4.02 of its $5 budget");
        assert_eq!(p.notices[0].buttons[0].label, "Raise cap");
        assert!(p.apply.is_empty());
        assert!(plan(&mut st, BOT5, day(0.0), &[bot("a", 4.5)]).notices.is_empty());
    }

    #[test]
    fn warns_about_the_day() {
        let mut st = State::default();
        let p = plan(&mut st, DAY20, day(16.0), &[bot("a", 1.0)]);
        assert_eq!(p.notices[0].message, "Today: $16 of $20");
        assert!(plan(&mut st, DAY20, day(17.0), &[bot("a", 1.0)]).notices.is_empty());
    }

    #[test]
    fn a_hit_steps_down_once_and_never_again_at_the_same_cap() {
        let mut st = State::default();
        let p = plan(&mut st, BOT5, day(0.0), &[bot("a", 5.1)]);
        assert_eq!(p.apply, vec![("a".to_string(), Scope::Bot)]);
        assert!(p.notices[0].message.contains("steps down when this turn ends"));
        // The caller takes it out of the queue when it acts; the cap stays hit.
        st.pending.clear();
        let p = plan(&mut st, BOT5, day(0.0), &[bot("a", 9.0)]);
        assert!(p.notices.is_empty() && p.apply.is_empty() && st.pending.is_empty());
    }

    #[test]
    fn a_working_bot_waits_for_its_turn_to_end() {
        let mut st = State::default();
        let busy = Bot { safe: false, ..bot("a", 6.0) };
        let p = plan(&mut st, BOT5, day(0.0), &[busy.clone()]);
        assert!(p.apply.is_empty());
        assert_eq!(st.pending.len(), 1);
        // Still working a moment later: still waiting, no second notice.
        let p = plan(&mut st, BOT5, day(0.0), &[Bot { cost: 6.5, ..busy }]);
        assert!(p.apply.is_empty() && p.notices.is_empty());
        // Idle now.
        let p = plan(&mut st, BOT5, day(0.0), &[bot("a", 6.5)]);
        assert_eq!(p.apply, vec![("a".to_string(), Scope::Bot)]);
    }

    #[test]
    fn bots_colony_did_not_start_get_a_notice_and_nothing_else() {
        let mut st = State::default();
        let outside = Bot { colony: false, step: Step::Floor, ..bot("x", 5.5) };
        let p = plan(&mut st, BOT5, day(0.0), &[outside]);
        assert!(p.notices[0].message.contains("Colony didn't start it"));
        assert!(p.apply.is_empty() && st.pending.is_empty());
    }

    #[test]
    fn a_bot_on_haiku_with_no_advisor_gets_a_notice_only() {
        let mut st = State::default();
        let floor = Bot { step: Step::Floor, ..bot("h", 5.5) };
        let p = plan(&mut st, BOT5, day(0.0), &[floor]);
        assert!(p.notices[0].message.contains("cheapest setup"));
        assert!(p.apply.is_empty() && st.pending.is_empty());
    }

    #[test]
    fn the_day_cap_steps_every_colony_bot_down_once() {
        let mut st = State::default();
        let outside = Bot { colony: false, step: Step::Floor, ..bot("x", 1.0) };
        let floor = Bot { step: Step::Floor, ..bot("h", 1.0) };
        let busy = Bot { safe: false, ..bot("b", 1.0) };
        let bots = [bot("a", 1.0), busy, outside, floor];
        let p = plan(&mut st, DAY20, day(20.5), &bots);
        assert_eq!(p.apply, vec![("a".to_string(), Scope::Day)]);
        assert_eq!(st.pending.len(), 2);
        assert_eq!(p.notices.len(), 1);
        assert!(p.notices[0].message.contains("2 bots will step down"), "{}", p.notices[0].message);
        assert!(p.notices[0].message.contains("2 can't be changed"), "{}", p.notices[0].message);
        // `a` was done; `b` goes when its turn ends. Nobody is queued twice.
        st.pending.remove("a");
        let bots = [bot("a", 1.0), bot("b", 1.0)];
        let p = plan(&mut st, DAY20, day(22.0), &bots);
        assert_eq!(p.apply, vec![("b".to_string(), Scope::Day)]);
        assert!(p.notices.is_empty());
    }

    #[test]
    fn raising_the_cap_cancels_a_waiting_step() {
        let mut st = State::default();
        let busy = Bot { safe: false, ..bot("a", 6.0) };
        plan(&mut st, BOT5, day(0.0), &[busy.clone()]);
        assert_eq!(st.pending.len(), 1);
        let p = plan(&mut st, Caps { bot: Some(10.0), day: None }, day(0.0), &[Bot { safe: true, ..busy }]);
        assert!(p.apply.is_empty() && st.pending.is_empty());
    }

    #[test]
    fn a_new_day_starts_the_day_cap_afresh() {
        let mut st = State::default();
        plan(&mut st, DAY20, day(25.0), &[bot("a", 1.0)]);
        let p = plan(&mut st, DAY20, Day { number: 2, spent: 17.0 }, &[bot("a", 1.0)]);
        assert_eq!(p.notices[0].message, "Today: $17 of $20");
    }

    #[test]
    fn what_was_announced_and_queued_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("colony-budget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("budget.json");
        let mut st = State::default();
        let busy = Bot { safe: false, ..bot("a", 6.0) };
        let first = plan(&mut st, BOT5, day(0.0), &[busy.clone(), bot("b", 4.1)]);
        assert_eq!(first.notices.len(), 2);
        st.stepped.insert("z".into(), Stepped { scope: Scope::Day, model: Some("sonnet".into()), advisor: None, at: 7 });
        st.save(&path).unwrap();

        // colonyd starts again with the same readings: nothing is announced
        // twice, and the waiting step is still waiting.
        let mut again = State::load(&path);
        assert_eq!(again, st);
        let second = plan(&mut again, BOT5, day(0.0), &[busy, bot("b", 4.1)]);
        assert!(second.notices.is_empty(), "{:?}", second.notices);
        assert!(second.apply.is_empty());
        assert_eq!(again.pending.get("a"), Some(&Scope::Bot));
        // A step already taken (queue emptied) is not taken again after a restart.
        again.pending.clear();
        again.save(&path).unwrap();
        let mut third = State::load(&path);
        let p = plan(&mut third, BOT5, day(0.0), &[bot("a", 6.0)]);
        assert!(p.notices.is_empty() && p.apply.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_old_file_is_an_empty_state() {
        assert_eq!(State::load(Path::new("nowhere/budget.json")), State::default());
        let dir = std::env::temp_dir().join(format!("colony-budget-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.json"), r#"{"bots":{"a":{"hit":5.0}}}"#).unwrap();
        assert_eq!(State::load(&dir.join("b.json")).bots["a"].hit, Some(5.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn caps_from_the_map_are_checked() {
        assert_eq!(checked_cap(None), Ok(None));
        assert_eq!(checked_cap(Some(5.0)), Ok(Some(5.0)));
        assert_eq!(checked_cap(Some(2.499)), Ok(Some(2.5)));
        assert!(checked_cap(Some(0.0)).is_err());
        assert!(checked_cap(Some(-1.0)).is_err());
        assert!(checked_cap(Some(f64::NAN)).is_err());
        assert!(checked_cap(Some(1e9)).is_err());
    }

    #[test]
    fn steps_read_naturally() {
        assert_eq!(describe(&Step::DropAdvisor, Some("sonnet"), Some("claude-opus-5-5")), "dropped the Opus advisor");
        assert_eq!(describe(&Step::LowerModel("haiku"), Some("claude-sonnet-5-5"), None), "moved from Sonnet to Haiku");
    }

    #[test]
    fn the_default_crew_is_seen_as_sonnet_with_an_opus_advisor() {
        let req: crate::pty::SpawnRequest = serde_json::from_value(json!({ "host": "win", "dir": "C:/w", "crew": "default" })).unwrap();
        let r = models_of(&req, &[], None).unwrap();
        assert_eq!(budget::step_down(r.model.as_deref(), r.advisor.as_deref()), Step::DropAdvisor);
        let alone = crew::Resolved { advisor: None, ..r };
        assert_eq!(budget::step_down(alone.model.as_deref(), None), Step::LowerModel("haiku"));
        // Claude's own defaults: the model Claude reports decides.
        let req: crate::pty::SpawnRequest = serde_json::from_value(json!({ "host": "win", "dir": "C:/w", "crew": "claude-defaults" })).unwrap();
        let r = models_of(&req, &[], Some("claude-opus-5-5")).unwrap();
        assert_eq!(budget::step_down(r.model.as_deref(), r.advisor.as_deref()), Step::LowerModel("sonnet"));
    }
}
