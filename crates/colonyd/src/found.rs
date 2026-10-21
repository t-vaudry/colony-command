//! Offering back sessions Colony has lost track of.
//!
//! `roster` brings back the bots Colony has records of. Some it doesn't: a
//! worktree from before those records existed, a record lost to a crash, a
//! session whose record was cleaned up while its worktree stayed. Their
//! transcripts are still in `<claude home>/projects`, each saying the folder it
//! ran in. So at startup colonyd looks there, on Windows and in each running
//! WSL distro, for sessions that ran in one of Colony's worktrees
//! (`<repo>/.colony/worktrees/<name>`) whose folder is still there, keeps the
//! latest session per worktree, and offers the ones it doesn't know to the map.
//! The user picks which come back: those return paused (see `pause`), with
//! Resume ready, and their worktrees are Colony's again. The rest are
//! remembered as declined (`found.json`) and not offered again.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use colony_core::paths::{worktree_split, WORKTREE_DIR};
use colony_core::{AgentState, Colony, HostId};
use colony_source::{colony_home, home_dir, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::pause::Paused;
use crate::{log, Shared};

/// Transcript lines read looking for the session's folder.
const HEAD_LINES: usize = 400;

/// A session in one of Colony's worktrees that Colony has no record of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub session_id: String,
    pub host: HostId,
    /// The folder it ran in: the worktree, or a folder inside it.
    pub dir: String,
    /// Repository root and worktree folder, with `/` separators.
    pub repo: String,
    pub worktree: String,
    /// The session's own name, if it was given one.
    pub name: String,
    /// When its transcript was last written (ms).
    pub last_at: u64,
    pub permission_mode: Option<String>,
}

/// Sessions on offer to the map, until the user answers.
static OFFER: Mutex<Vec<Found>> = Mutex::new(Vec::new());

pub fn offered() -> Vec<Found> {
    OFFER.lock().unwrap().clone()
}

fn message() -> String {
    json!({ "type": "found", "items": offered() }).to_string()
}

// --- reading transcripts -------------------------------------------------

/// What the start of a transcript says about its session.
#[derive(Default)]
struct Head {
    session_id: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
    permission_mode: Option<String>,
}

fn read_head(path: &Path) -> Head {
    let mut head = Head::default();
    let Ok(file) = std::fs::File::open(path) else { return head };
    for line in BufReader::new(file).lines().take(HEAD_LINES) {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_string);
        match v.get("type").and_then(|t| t.as_str()) {
            Some("custom-title") => head.name = s("customTitle").or(head.name.take()),
            Some("agent-name") if head.name.is_none() => head.name = s("agentName"),
            Some("permission-mode") => head.permission_mode = s("permissionMode"),
            _ => {}
        }
        if let Some(cwd) = s("cwd") {
            head.session_id = s("sessionId");
            head.cwd = Some(cwd);
            break;
        }
    }
    head
}

/// Sessions in `projects` (a Claude home's `projects` folder) that ran in one
/// of Colony's worktrees, the latest per worktree, leaving out worktrees whose
/// folder is gone. `local`: where a folder of `host` is on this machine.
pub fn scan(projects: &Path, host: &HostId, local: impl Fn(&str) -> PathBuf) -> Vec<Found> {
    let mut latest: BTreeMap<String, Found> = BTreeMap::new();
    let Ok(dirs) = std::fs::read_dir(projects) else { return Vec::new() };
    for dir in dirs.flatten() {
        // A project folder is named after its path, `.colony/worktrees` and all,
        // with every other character turned into `-`.
        if !dir.file_name().to_string_lossy().contains("-colony-worktrees-") {
            continue;
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let modified = f.metadata().and_then(|m| m.modified()).ok();
            let last_at = modified.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
            let head = read_head(&path);
            let Some(cwd) = head.cwd else { continue };
            let Some((repo, name)) = worktree_split(&cwd) else { continue };
            let worktree = format!("{repo}/{WORKTREE_DIR}/{name}");
            let key = worktree_key(host, &worktree);
            if latest.get(&key).is_some_and(|f| f.last_at >= last_at) {
                continue;
            }
            let session_id = head.session_id.or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned())).unwrap_or_default();
            latest.insert(
                key,
                Found { session_id, host: host.clone(), dir: cwd, repo, worktree, name: head.name.unwrap_or_default(), last_at, permission_mode: head.permission_mode },
            );
        }
    }
    latest.into_values().filter(|f| !f.session_id.is_empty() && local(&f.worktree).is_dir()).collect()
}

/// A worktree folder, comparable however it was spelled.
fn worktree_key(host: &HostId, path: &str) -> String {
    let p = path.replace('\\', "/").trim_end_matches('/').to_string();
    match host {
        HostId::Windows => p.to_lowercase(),
        HostId::Wsl(d) => format!("wsl:{d}:{p}"),
    }
}

// --- deciding what to offer ----------------------------------------------

/// What Colony already knows about, so isn't offered.
#[derive(Default)]
pub struct Known {
    /// Sessions Colony started, has paused, or the user declined.
    pub sessions: BTreeSet<String>,
    /// Colony's own worktree records.
    pub worktrees: Vec<crate::worktree::Worktree>,
}

/// The found sessions to offer, newest first: not on the map, dismissed, or
/// known, and in a worktree no bot on the map is using.
pub fn plan(found: Vec<Found>, known: &Known, colony: &Colony) -> Vec<Found> {
    let taken: BTreeSet<String> = known.worktrees.iter().map(|w| worktree_key(&w.host, &w.path)).collect();
    let mut offer: Vec<Found> = found
        .into_iter()
        .filter(|f| {
            let key = worktree_key(&f.host, &f.worktree);
            let in_use = colony.agents.values().any(|a| {
                a.state != AgentState::Ended
                    && a.host == f.host
                    && a.project_dir.as_deref().or(a.cwd.as_deref()).is_some_and(|d| {
                        let d = worktree_key(&a.host, d);
                        d == key || d.starts_with(&format!("{key}/"))
                    })
            });
            let on_map = colony.agents.get(&f.session_id).is_some_and(|a| a.state != AgentState::Ended || a.paused_at.is_some());
            !in_use && !on_map && !taken.contains(&key) && !known.sessions.contains(&f.session_id) && !colony.dismissed.contains_key(&f.session_id)
        })
        .collect();
    offer.sort_by(|a, b| b.last_at.cmp(&a.last_at));
    offer
}

// --- declined, on disk ---------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    declined: BTreeSet<String>,
}

fn path() -> PathBuf {
    colony_home().join("found.json")
}

fn load() -> Store {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn store(s: &Store) {
    let body = serde_json::to_vec_pretty(s).expect("serializes");
    if let Err(e) = std::fs::create_dir_all(colony_home()).and_then(|_| std::fs::write(path(), body)) {
        log(format!("could not save found.json: {e}"));
    }
}

// --- at startup, and the user's answer -----------------------------------

/// Every Claude home to look in: this machine's, and each running WSL distro's.
async fn homes() -> Vec<(HostId, PathBuf)> {
    let mut homes = vec![(crate::local_host(), home_dir().join(".claude").join("projects"))];
    #[cfg(windows)]
    for distro in crate::wsl::list_distros(true).await {
        let out = crate::worktree::quiet("wsl.exe")
            .args(["-d", &distro, "-e", "sh", "-c", "printf %s \"$HOME\""])
            .stdin(std::process::Stdio::null())
            .output()
            .await;
        let Some(home) = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| h.starts_with('/')) else {
            log(format!("could not find the home folder in {distro}; not looking for its sessions"));
            continue;
        };
        let host = HostId::Wsl(distro);
        homes.push((host.clone(), PathBuf::from(crate::worktree::local_folder(&host, &format!("{home}/.claude/projects")))));
    }
    homes
}

/// Look for sessions Colony lost track of and offer them to the map. Call
/// after `roster` has put back the ones it knows.
pub async fn run(shared: &Shared) {
    let homes = homes().await;
    let here = crate::local_host();
    let found = tokio::task::spawn_blocking(move || {
        homes
            .iter()
            .flat_map(|(host, projects)| scan(projects, host, |p| PathBuf::from(if *host == here { p.to_string() } else { crate::worktree::local_folder(host, p) })))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let mut known = Known { worktrees: crate::worktree::all(), ..Known::default() };
    known.sessions.extend(crate::resume::load_all().into_keys());
    known.sessions.extend(crate::pause::load().into_keys());
    known.sessions.extend(load().declined);
    let offer = plan(found, &known, &*shared.colony.read().await);
    if offer.is_empty() {
        return;
    }
    log(format!("found {} session(s) from before in Colony worktrees; offering them to the map", offer.len()));
    *OFFER.lock().unwrap() = offer;
    let _ = shared.deltas.send(message());
}

/// The user's answer: the picked sessions come back paused; the rest of the
/// offer is declined and not offered again.
pub async fn bring_back(shared: &Shared, pick: &[String]) -> Result<(), String> {
    let offer = std::mem::take(&mut *OFFER.lock().unwrap());
    let _ = shared.deltas.send(message());
    let (back, declined): (Vec<Found>, Vec<Found>) = offer.into_iter().partition(|f| pick.contains(&f.session_id));
    if !declined.is_empty() {
        let mut s = load();
        s.declined.extend(declined.into_iter().map(|f| f.session_id));
        store(&s);
    }
    if back.is_empty() {
        return Ok(());
    }
    let now = now_ms();
    let mut paused = crate::pause::load();
    for f in &back {
        // A Colony branch makes the worktree Colony's again: kept up to date,
        // and cleaned up when the bot is dismissed.
        match crate::worktree::branch_of(&f.host, &f.worktree).await {
            Some(branch) if branch.starts_with("colony/") => crate::worktree::remember(crate::worktree::Worktree {
                session_id: f.session_id.clone(),
                host: f.host.clone(),
                repo: f.repo.clone(),
                path: f.worktree.clone(),
                branch,
                created_at: f.last_at,
                kept: None,
                folder_gone: false,
            }),
            _ => log(format!("{} isn't on a Colony branch; leaving it out of cleanup", f.worktree)),
        }
        paused.insert(
            f.session_id.clone(),
            Paused { host: f.host.clone(), dir: f.dir.clone(), name: f.name.clone(), at: now, model: None, permission_mode: f.permission_mode.clone(), chrome: None, interrupted: true },
        );
    }
    crate::pause::store(&paused);
    for f in &back {
        crate::pause::show(shared, &f.session_id, &paused[&f.session_id]).await;
    }
    log(format!("brought back {} session(s) from before, paused", back.len()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_core::{DomainEvent, Envelope};

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp() -> Dir {
        let d = std::env::temp_dir().join(format!("colony-found-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn transcript(projects: &Path, folder: &str, sid: &str, lines: &[serde_json::Value]) {
        let d = projects.join(folder);
        std::fs::create_dir_all(&d).unwrap();
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(d.join(format!("{sid}.jsonl")), body).unwrap();
    }

    fn found(sid: &str, worktree: &str) -> Found {
        Found {
            session_id: sid.into(),
            host: HostId::Windows,
            dir: worktree.replace('/', "\\"),
            repo: "C:/r".into(),
            worktree: worktree.into(),
            name: String::new(),
            last_at: 1,
            permission_mode: None,
        }
    }

    #[test]
    fn finds_the_latest_session_of_each_worktree_that_is_still_there() {
        let root = temp();
        let projects = root.0.join("projects");
        let repo = root.0.join("repo");
        std::fs::create_dir_all(repo.join(".colony/worktrees/fix-ab12")).unwrap();
        let wt = repo.join(".colony").join("worktrees").join("fix-ab12");
        let cwd = wt.to_string_lossy().to_string();
        let gone = repo.join(".colony").join("worktrees").join("gone-cd34").to_string_lossy().to_string();
        let user = |sid: &str, cwd: &str| json!({ "type": "user", "sessionId": sid, "cwd": cwd, "message": { "content": "hi" } });
        transcript(&projects, "C--repo--colony-worktrees-fix-ab12", "old", &[user("old", &cwd)]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        transcript(
            &projects,
            "C--repo--colony-worktrees-fix-ab12",
            "new",
            &[json!({ "type": "custom-title", "customTitle": "ronnie", "sessionId": "new" }), json!({ "type": "permission-mode", "permissionMode": "auto", "sessionId": "new" }), user("new", &cwd)],
        );
        transcript(&projects, "C--repo--colony-worktrees-gone-cd34", "g", &[user("g", &gone)]);
        transcript(&projects, "C--repo", "plain", &[user("plain", &repo.to_string_lossy())]);

        let f = scan(&projects, &HostId::Windows, |p| PathBuf::from(p));
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!((f[0].session_id.as_str(), f[0].name.as_str(), f[0].permission_mode.as_deref()), ("new", "ronnie", Some("auto")));
        assert_eq!(f[0].dir, cwd);
        assert!(f[0].worktree.ends_with("/.colony/worktrees/fix-ab12"));
        assert!(!f[0].repo.contains('\\'));
    }

    #[test]
    fn offers_only_what_colony_does_not_know() {
        let mut c = Colony::new();
        let at = |sid: &str, cwd: &str, event| Envelope { ts: 1_000, host: HostId::Windows, session_id: sid.into(), cwd: Some(cwd.into()), event };
        // A bot on the map working in the "busy" worktree under another session id.
        c.apply(&at("other", r"C:\r\.colony\worktrees\busy\src", DomainEvent::SessionStarted { source: None, model: None }));
        c.apply(&at("bye", r"C:\r\.colony\worktrees\bye", DomainEvent::SessionStarted { source: None, model: None }));
        c.dismiss("bye", 2_000);
        let mut known = Known::default();
        known.sessions.insert("started".into());
        known.worktrees.push(crate::worktree::Worktree {
            session_id: "x".into(),
            host: HostId::Windows,
            repo: "C:/r".into(),
            path: "C:/r/.colony/worktrees/kept".into(),
            branch: "colony/kept".into(),
            created_at: 0,
            kept: Some("uncommitted changes".into()),
            folder_gone: false,
        });
        let mut newer = found("lost2", "C:/r/.colony/worktrees/lost2");
        newer.last_at = 5;
        let all = vec![
            found("lost", "C:/r/.colony/worktrees/lost"),
            newer,
            found("other2", "C:/r/.colony/worktrees/busy"),
            found("bye", "C:/r/.colony/worktrees/bye"),
            found("started", "C:/r/.colony/worktrees/started"),
            found("k", "C:/R/.colony/worktrees/Kept"),
        ];
        let ids: Vec<_> = plan(all, &known, &c).into_iter().map(|f| f.session_id).collect();
        assert_eq!(ids, ["lost2", "lost"]);
    }
}
