//! Crews: which models a bot started from Colony uses. A main model does the
//! work, an advisor (Claude Code's built-in advisor tool) is consulted at
//! decision points, and a swarm of cheap subagents takes the legwork.
//!
//! Everything is passed as launch flags; nothing is written to the user's
//! repositories or `~/.claude`. The sessions are marked with environment
//! variables (`COLONY_CREW`, `COLONY_ADVISOR`, `COLONY_SWARM`) so the hook can
//! tell which crew a session belongs to.

use serde::{Deserialize, Serialize};

use crate::models::{family, Family};
use crate::swarm;

/// Value of an advisor or swarm override that switches it off for one bot.
pub const NONE: &str = "none";
/// The crew a new bot gets unless told otherwise.
pub const DEFAULT_CREW: &str = "default";
/// Crew that sets nothing: Claude Code behaves as it does outside Colony.
pub const CLAUDE_DEFAULTS: &str = "claude-defaults";
/// `--effort` levels Claude Code accepts.
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// A set of model choices. `None` means "leave it to Claude Code".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crew {
    pub id: String,
    pub label: String,
    /// `--model`.
    #[serde(default)]
    pub model: Option<String>,
    /// `--advisor`.
    #[serde(default)]
    pub advisor: Option<String>,
    /// `--effort`: low, medium, high, xhigh or max.
    #[serde(default)]
    pub effort: Option<String>,
    /// Model of the subagent swarm; `None` means no swarm.
    #[serde(default)]
    pub swarm_model: Option<String>,
    /// One of Colony's own crews, possibly an edited copy (set when listing).
    #[serde(default, skip_deserializing)]
    pub builtin: bool,
}

/// What a bot asks for: a crew, and per-bot overrides of it. The main-model
/// override is `SpawnRequest::model`, so it works without a crew too.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CrewPick {
    #[serde(default)]
    pub crew: Option<String>,
    /// An advisor model, or `"none"` for no advisor.
    #[serde(default)]
    pub advisor: Option<String>,
    /// A swarm model, or `"none"` for no swarm.
    #[serde(default)]
    pub swarm: Option<String>,
}

/// The crews Colony always has.
pub fn builtins() -> Vec<Crew> {
    let crew = |id: &str, label: &str, model: Option<&str>, advisor: Option<&str>, swarm: Option<&str>| Crew {
        id: id.into(),
        label: label.into(),
        model: model.map(Into::into),
        advisor: advisor.map(Into::into),
        effort: None,
        swarm_model: swarm.map(Into::into),
        builtin: true,
    };
    vec![
        crew(DEFAULT_CREW, "Sonnet builds · Haiku swarms · Opus reviews", Some("sonnet"), Some("opus"), Some("haiku")),
        crew("opus-solo", "Opus alone", Some("opus"), None, None),
        crew(CLAUDE_DEFAULTS, "Claude Code's defaults", None, None, None),
    ]
}

/// Built-in crews, then the saved ones; a saved crew with a built-in's id
/// replaces it (an edited copy).
pub fn merged(saved: &[Crew]) -> Vec<Crew> {
    let mut all = builtins();
    for s in saved {
        let mut s = s.clone();
        match all.iter_mut().find(|c| c.id == s.id) {
            Some(slot) => {
                s.builtin = true;
                *slot = s;
            }
            None => {
                s.builtin = false;
                all.push(s);
            }
        }
    }
    all
}

/// A bot's models once crew and overrides are applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// Set when the bot belongs to a crew.
    pub crew: Option<String>,
    pub model: Option<String>,
    pub advisor: Option<String>,
    pub effort: Option<String>,
    pub swarm_model: Option<String>,
}

fn is_off(v: &str) -> bool {
    v.is_empty() || v.eq_ignore_ascii_case(NONE)
}

/// Apply a pick (and the main-model override `model`) on the crews known.
/// Without a crew or advisor/swarm overrides this is just `--model`, as
/// before crews existed.
pub fn resolve(saved: &[Crew], pick: &CrewPick, model: Option<&str>) -> Result<Resolved, String> {
    let model = model.filter(|m| !m.is_empty()).map(String::from);
    let crew_id = pick.crew.as_deref().filter(|c| !c.is_empty());
    if crew_id.is_none() && pick.advisor.is_none() && pick.swarm.is_none() {
        return Ok(Resolved { crew: None, model, advisor: None, effort: None, swarm_model: None });
    }
    let id = crew_id.unwrap_or(CLAUDE_DEFAULTS);
    let base = merged(saved).into_iter().find(|c| c.id == id).ok_or_else(|| format!("no crew named {id:?}"))?;
    let advisor = match pick.advisor.as_deref() {
        Some(a) if is_off(a) => None,
        Some(a) => Some(a.to_string()),
        None => base.advisor.clone(),
    };
    let swarm_model = match pick.swarm.as_deref() {
        Some(s) if is_off(s) => None,
        Some(s) => Some(s.to_string()),
        None => base.swarm_model.clone(),
    };
    let r = Resolved { crew: Some(base.id.clone()), model: model.or(base.model.clone()), advisor, effort: base.effort.clone(), swarm_model };
    if let Some(e) = &r.effort {
        if !EFFORTS.contains(&e.as_str()) {
            return Err(format!("unknown effort {e:?}; use one of {}", EFFORTS.join(", ")));
        }
    }
    r.check_pairing()?;
    Ok(r)
}

impl Resolved {
    /// Reject an advisor that can't work with the main model. With no main
    /// model set, Claude Code picks one and the pairing can't be known here.
    pub fn check_pairing(&self) -> Result<(), String> {
        match (&self.model, &self.advisor) {
            (Some(m), Some(a)) => check_pairing(m, a),
            _ => Ok(()),
        }
    }

    /// Flags for `claude`. Empty values are skipped, never passed as `{}` or "".
    pub fn args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(m) = &self.model {
            out.extend(["--model".into(), m.clone()]);
        }
        if let Some(a) = &self.advisor {
            out.extend(["--advisor".into(), a.clone()]);
        }
        if let Some(e) = &self.effort {
            out.extend(["--effort".into(), e.clone()]);
        }
        if let Some(s) = &self.swarm_model {
            let agents = swarm::agents_json(s);
            if !agents.trim().is_empty() && agents.trim() != "{}" {
                out.extend(["--agents".into(), agents]);
            }
        }
        let rules = swarm::rules(self.swarm_model.is_some(), self.advisor.is_some());
        if !rules.trim().is_empty() {
            out.extend(["--append-system-prompt".into(), rules]);
        }
        out
    }

    /// Variables that mark the session as a crew's.
    pub fn env(&self) -> Vec<(String, String)> {
        let Some(id) = &self.crew else { return Vec::new() };
        let mut out = vec![("COLONY_CREW".to_string(), id.clone())];
        if let Some(a) = &self.advisor {
            out.push(("COLONY_ADVISOR".into(), a.clone()));
        }
        if let Some(s) = &self.swarm_model {
            out.push(("COLONY_SWARM".into(), s.clone()));
        }
        out
    }
}

/// (family, version x 10): "claude-opus-5-5" and the alias "opus" -> (Opus, 55).
fn version(model: &str) -> Option<(Family, u32)> {
    let fam = family(model)?;
    let m = model.to_ascii_lowercase();
    let m = m.split('[').next().unwrap_or(&m);
    let name = match fam {
        Family::Opus => "opus",
        Family::Sonnet => "sonnet",
        Family::Haiku => "haiku",
        Family::Fable => "fable",
    };
    let after = &m[m.find(name)? + name.len()..];
    let nums: Vec<u32> = after.split(['-', '.', ' ', '_']).filter(|t| !t.is_empty()).map_while(|t| t.parse().ok()).collect();
    match nums.as_slice() {
        // The alias: Claude Code's current version of the family.
        [] if m == name => Some((fam, if fam == Family::Fable { 51 } else { 55 })),
        [] => None,
        [major, minor, ..] if *major < 10 && *minor < 10 => Some((fam, major * 10 + minor)),
        [major, ..] if *major < 10 => Some((fam, major * 10)),
        _ => None,
    }
}

/// Whether `advisor` ranks high enough for `main`, per Claude Code's advisor docs.
pub fn check_pairing(main: &str, advisor: &str) -> Result<(), String> {
    let known = |m: &str| version(m).ok_or_else(|| format!("can't tell which version {m:?} is; use an alias (opus, sonnet, haiku, fable) or a full model id"));
    let (mf, mv) = known(main)?;
    let (af, av) = known(advisor)?;
    if af == Family::Haiku && av < 55 {
        return Err(format!("{advisor} can't act as an advisor (Haiku 4.5 never can)"));
    }
    use Family::*;
    let ok = match (mf, mv) {
        (Fable, 51..) => af == Fable && av == 51,
        (Fable, _) => af == Fable && av >= 50,
        (Opus, 50..) => af == Fable || (af == Opus && av >= 50),
        (Opus, 47..=49) => af == Fable || (af == Opus && av >= 47) || (af == Sonnet && av >= 55),
        (Sonnet, 55..) => af == Fable || (af == Opus && av >= 50) || (af == Sonnet && av >= 55),
        (Sonnet, 50..=54) | (Haiku, 55..) => af == Fable || (af == Opus && av >= 47) || (af == Sonnet && av >= 50) || af == Haiku,
        (Opus, _) => af == Fable || af == Opus || (af == Sonnet && av >= 50) || af == Haiku,
        // Haiku 4.5, Sonnet 4.6 and older.
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{advisor} can't advise {main}: an advisor must rank at or above the main model"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(crew: &str) -> CrewPick {
        CrewPick { crew: Some(crew.into()), ..Default::default() }
    }

    #[test]
    fn default_crew_builds_with_all_three_models() {
        let r = resolve(&[], &pick("default"), None).unwrap();
        assert_eq!(&r.args()[..4], ["--model", "sonnet", "--advisor", "opus"]);
        assert_eq!(
            r.env(),
            vec![("COLONY_CREW".to_string(), "default".to_string()), ("COLONY_ADVISOR".into(), "opus".into()), ("COLONY_SWARM".into(), "haiku".into())]
        );
    }

    #[test]
    fn empty_swarm_values_are_never_passed() {
        let args = resolve(&[], &pick("default"), None).unwrap().args();
        if swarm::agents_json("haiku").trim() == "{}" {
            assert!(!args.contains(&"--agents".to_string()));
        }
        if swarm::rules(true, true).trim().is_empty() {
            assert!(!args.contains(&"--append-system-prompt".to_string()));
        }
        assert!(!args.iter().any(|a| a.is_empty() || a == "{}"));
    }

    #[test]
    fn claude_defaults_and_plain_model_stay_as_before() {
        let r = resolve(&[], &pick("claude-defaults"), None).unwrap();
        assert!(r.args().is_empty());
        assert_eq!(r.env(), vec![("COLONY_CREW".to_string(), "claude-defaults".to_string())]);
        let legacy = resolve(&[], &CrewPick::default(), Some("opus")).unwrap();
        assert_eq!(legacy.args(), ["--model", "opus"]);
        assert!(legacy.env().is_empty());
    }

    #[test]
    fn overrides_apply_to_one_bot() {
        let p = CrewPick { crew: Some("default".into()), advisor: Some("none".into()), swarm: Some("none".into()) };
        let r = resolve(&[], &p, Some("opus")).unwrap();
        assert_eq!(r.args(), ["--model", "opus"]);
        assert_eq!(r.env(), vec![("COLONY_CREW".to_string(), "default".to_string())]);
    }

    #[test]
    fn saved_copy_replaces_a_builtin_and_new_crews_are_added() {
        let mine = Crew { id: "default".into(), label: "Mine".into(), model: Some("haiku".into()), advisor: None, effort: Some("high".into()), swarm_model: None, builtin: false };
        let extra = Crew { id: "x".into(), label: "X".into(), ..mine.clone() };
        let all = merged(&[mine, extra]);
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].label, "Mine");
        assert!(all[0].builtin && !all[3].builtin);
        let args = resolve(&all, &pick("default"), None).unwrap().args();
        assert_eq!(args, ["--model", "haiku", "--effort", "high"]);
        assert!(resolve(&all, &pick("nope"), None).is_err());
    }

    #[test]
    fn bad_effort_is_rejected() {
        let c = Crew { id: "e".into(), label: "E".into(), model: None, advisor: None, effort: Some("turbo".into()), swarm_model: None, builtin: false };
        assert!(resolve(&[c], &pick("e"), None).is_err());
    }

    #[test]
    fn pairings_follow_the_docs() {
        assert!(check_pairing("sonnet", "opus").is_ok());
        assert!(check_pairing("sonnet", "fable").is_ok());
        assert!(check_pairing("sonnet", "sonnet").is_ok());
        assert!(check_pairing("sonnet", "haiku").is_err());
        assert!(check_pairing("claude-sonnet-5-5", "claude-opus-4-8").is_err());
        assert!(check_pairing("opus", "opus").is_ok());
        assert!(check_pairing("opus", "sonnet").is_err());
        assert!(check_pairing("claude-opus-5", "claude-opus-5-5").is_ok());
        assert!(check_pairing("haiku", "opus").is_ok());
        assert!(check_pairing("haiku", "haiku").is_ok());
        assert!(check_pairing("claude-haiku-4-5-20251001", "haiku").is_ok());
        assert!(check_pairing("sonnet", "claude-haiku-4-5-20251001").is_err());
        assert!(check_pairing("claude-opus-4-7", "sonnet").is_ok());
        assert!(check_pairing("claude-opus-4-7", "haiku").is_err());
        assert!(check_pairing("fable", "fable").is_ok());
        assert!(check_pairing("fable", "opus").is_err());
        assert!(check_pairing("claude-fable-5", "fable").is_ok());
        assert!(check_pairing("claude-opus-5-5[1m]", "opus").is_ok());
        assert!(check_pairing("sonnet", "gpt").is_err());
    }

    #[test]
    fn a_bad_pairing_stops_the_crew() {
        let p = CrewPick { crew: Some("default".into()), advisor: Some("haiku".into()), swarm: None };
        assert!(resolve(&[], &p, None).unwrap_err().contains("rank"));
    }
}
