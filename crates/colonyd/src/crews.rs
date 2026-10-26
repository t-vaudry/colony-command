//! Crews on disk, and the checks the New session dialog and spawning share.
//!
//! `crews.json` in Colony's folder holds the crews the user made and edited
//! copies of the built-in ones (see `colony_core::crew`). Built-ins always
//! exist, so removing a saved copy of one resets it.

use std::sync::Mutex;

use colony_core::crew::{self, Crew, CrewPick};
use colony_core::HostId;
use colony_source::colony_home;
use serde::Serialize;

use crate::log;
use crate::pty::{checked_model, child_env, SpawnRequest};

static STORE: Mutex<()> = Mutex::new(());

/// Variables that stop Claude Code fetching the feature flag the advisor needs.
const ADVISOR_BLOCKERS: &[&str] = &["DISABLE_TELEMETRY", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "CLAUDE_CODE_DISABLE_ADVISOR_TOOL"];

fn path() -> std::path::PathBuf {
    colony_home().join("crews.json")
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The crews saved on disk (not the built-ins).
pub fn saved() -> Vec<Crew> {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Every crew: built-ins (or their saved copies), then the user's.
pub fn all() -> Vec<Crew> {
    crew::merged(&saved())
}

fn store(crews: &[Crew]) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(crews).expect("serializes");
    let tmp = path().with_extension("json.tmp");
    std::fs::create_dir_all(colony_home())
        .and_then(|_| std::fs::write(&tmp, body))
        .and_then(|_| std::fs::rename(&tmp, path()))
        .map_err(|e| format!("could not save crews.json: {e}"))
}

fn slug(label: &str) -> String {
    let s: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

/// Check a crew before saving it: safe names, a pairing that works.
fn validate(c: &mut Crew) -> Result<(), String> {
    c.label = c.label.trim().to_string();
    if c.label.is_empty() || c.label.chars().count() > 60 {
        return Err("give the crew a name of up to 60 characters".into());
    }
    if c.id.trim().is_empty() {
        c.id = slug(&c.label);
    }
    if c.id.is_empty() || c.id.len() > 40 || !c.id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-') {
        return Err(format!("not a crew id: {:?}", c.id));
    }
    for m in [&mut c.model, &mut c.advisor, &mut c.effort, &mut c.swarm_model] {
        if m.as_deref().is_some_and(str::is_empty) {
            *m = None;
        }
    }
    for m in [&c.model, &c.advisor, &c.swarm_model].into_iter().flatten() {
        checked_model(m)?;
    }
    if let Some(e) = &c.effort {
        if !crew::EFFORTS.contains(&e.as_str()) {
            return Err(format!("unknown effort {e:?}"));
        }
    }
    if let (Some(m), Some(a)) = (&c.model, &c.advisor) {
        crew::check_pairing(m, a)?;
    }
    Ok(())
}

/// Save a crew (new, or an edited copy of a built-in). Returns all crews.
pub fn save(mut c: Crew) -> Result<Vec<Crew>, String> {
    validate(&mut c)?;
    let _g = lock();
    let mut saved = saved();
    saved.retain(|s| s.id != c.id);
    saved.push(Crew { builtin: false, ..c });
    store(&saved)?;
    Ok(crew::merged(&saved))
}

/// Remove a saved crew, or reset a built-in to its original. Returns all crews.
pub fn delete(id: &str) -> Result<Vec<Crew>, String> {
    let _g = lock();
    let mut saved = saved();
    let before = saved.len();
    saved.retain(|s| s.id != id);
    if saved.len() == before {
        return Err(if crew::builtins().iter().any(|b| b.id == id) { "that crew is already as Colony ships it".into() } else { "no such crew".into() });
    }
    store(&saved)?;
    Ok(crew::merged(&saved))
}

/// What the dialog shows before Start.
#[derive(Debug, Serialize)]
pub struct Check {
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

/// Resolve a pick and say whether it can start, and what will be limited.
pub fn check(host: Option<&HostId>, pick: &CrewPick, model: Option<&str>) -> Check {
    let resolved = match crew::resolve(&saved(), pick, model) {
        Ok(r) => r,
        Err(e) => return Check { error: Some(e), warnings: Vec::new() },
    };
    for m in [&resolved.model, &resolved.advisor, &resolved.swarm_model].into_iter().flatten() {
        if let Err(e) = checked_model(m) {
            return Check { error: Some(e), warnings: Vec::new() };
        }
    }
    let warnings = if resolved.advisor.is_some() { advisor_warning(host).into_iter().collect() } else { Vec::new() };
    Check { error: None, warnings }
}

/// Why an advisor would be silently off for a session started on `host`, if it would.
pub fn advisor_warning(host: Option<&HostId>) -> Option<String> {
    let env = child_env();
    let wsl_passes = |name: &str| {
        env.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("WSLENV"))
            .is_some_and(|(_, v)| v.split(':').any(|e| e.split('/').next().is_some_and(|n| n.eq_ignore_ascii_case(name))))
    };
    let blocker = ADVISOR_BLOCKERS.iter().find(|name| {
        env.iter().any(|(k, v)| k.eq_ignore_ascii_case(name) && !v.is_empty() && v != "0")
            && match host {
                Some(HostId::Wsl(_)) => wsl_passes(name),
                _ => true,
            }
    })?;
    Some(format!("{blocker} is set where Colony starts sessions. The advisor needs Claude Code to fetch feature flags, so it will stay off."))
}

/// A request that has been given another main model keeps its advisor when
/// the pairing still works; otherwise the advisor is dropped. Returns what
/// to tell the user in that case.
pub fn fit_model(req: &mut SpawnRequest) -> Result<Option<String>, String> {
    fit_with(&saved(), req)
}

fn fit_with(crews: &[Crew], req: &mut SpawnRequest) -> Result<Option<String>, String> {
    if crew::resolve(crews, &req.pick(), req.model.as_deref()).is_ok() {
        return Ok(None);
    }
    let kept = req.advisor.take();
    req.advisor = Some(crew::NONE.into());
    let r = crew::resolve(crews, &req.pick(), req.model.as_deref())?;
    // Say which advisor went: the one this bot asked for, else its crew's.
    let was = kept.or_else(|| {
        let id = req.crew.as_deref().unwrap_or(crew::CLAUDE_DEFAULTS);
        crew::merged(crews).into_iter().find(|c| c.id == id).and_then(|c| c.advisor)
    });
    let note = format!("Dropped the {} advisor: it can't advise {}.", was.unwrap_or_else(|| "crew's".into()), r.model.as_deref().unwrap_or("that model"));
    log(note.clone());
    Ok(Some(note))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crew(label: &str, model: &str, advisor: Option<&str>) -> Crew {
        Crew { id: String::new(), label: label.into(), model: Some(model.into()), advisor: advisor.map(Into::into), effort: None, swarm_model: None, builtin: false }
    }

    #[test]
    fn saving_validates_and_names_the_crew() {
        let mut c = crew("  My Crew! ", "sonnet", Some("opus"));
        validate(&mut c).unwrap();
        assert_eq!((c.id.as_str(), c.label.as_str()), ("my-crew", "My Crew!"));
        assert!(validate(&mut crew("Bad", "sonnet", Some("haiku"))).is_err());
        assert!(validate(&mut crew("Bad", "--model", None)).is_err());
        assert!(validate(&mut crew("", "sonnet", None)).is_err());
    }

    #[test]
    fn switching_the_main_model_drops_an_advisor_that_no_longer_fits() {
        let mut req = request();
        req.crew = Some("default".into());
        req.model = Some("haiku".into());
        assert_eq!(fit_with(&[], &mut req).unwrap(), None);
        req.model = Some("opus".into());
        // Opus 5.5 main with the crew's Opus advisor still works.
        assert_eq!(fit_with(&[], &mut req).unwrap(), None);
        req.advisor = Some("sonnet".into());
        let note = fit_with(&[], &mut req).unwrap().unwrap();
        assert!(note.contains("sonnet"), "{note}");
        assert_eq!(req.advisor.as_deref(), Some("none"));
    }

    fn request() -> SpawnRequest {
        serde_json::from_value(serde_json::json!({ "host": "win", "dir": "C:/w" })).unwrap()
    }
}
