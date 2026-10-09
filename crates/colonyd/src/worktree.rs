//! Git worktrees for isolated sessions.
//!
//! Bots working on different issues in one repository can't share a checkout:
//! they'd switch branches and edit files under each other. When a session is
//! started isolated, Colony gives it its own worktree and branch under
//! `<repo>/.colony/worktrees/<name>` and starts `claude` there. The map still
//! files such bots in their repository's district (see `paths::worktree_split`).
//! Dismissing the bot removes its worktree again, unless it holds work git
//! would lose; then the worktree stays for the user to deal with.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use colony_core::paths::{to_wsl_path, worktree_split, WORKTREE_DIR};
use colony_core::HostId;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::log;

/// A worktree Colony made for a session, kept on disk so it can be cleaned up
/// after a colonyd restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub session_id: String,
    pub host: HostId,
    /// Repository root, in the form git printed it.
    pub repo: String,
    /// The worktree's folder, in the same form.
    pub path: String,
    pub branch: String,
    /// When it was made (ms), so records of sessions that have left the map's
    /// history can still be cleaned up.
    #[serde(default)]
    pub created_at: u64,
    /// Set when cleanup left something behind that the user should decide on:
    /// why (uncommitted changes, unmerged commits). The map lists these.
    #[serde(default)]
    pub kept: Option<String>,
    /// The folder is gone and only the branch (with unmerged commits) remains.
    #[serde(default)]
    pub folder_gone: bool,
}

pub struct Created {
    /// Folder to start the session in: the worktree, or the matching subfolder
    /// of it when the session was asked for inside one.
    pub dir: String,
    pub record: Worktree,
}

/// Make a worktree and branch for a new session started in `dir`.
pub async fn create(host: &HostId, dir: &str, session_id: &str, label: &str) -> Result<Created, String> {
    let out = git(host, dir, &["rev-parse", "--show-toplevel", "--show-prefix"])
        .await
        .map_err(|e| format!("can't isolate this session in a worktree: {e}"))?;
    let mut lines = out.lines();
    let top = lines.next().unwrap_or("").trim().to_string();
    let prefix = lines.next().unwrap_or("").trim().trim_end_matches('/').to_string();
    if top.is_empty() {
        return Err("can't isolate this session in a worktree: that folder isn't in a git repository".into());
    }
    // Started from inside another Colony worktree: branch off the same repository.
    let repo = worktree_split(&top).map(|(r, _)| r).unwrap_or(top);

    let tag: String = session_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(4).collect();
    let slug = format!("{}-{}", slugify(label), tag.to_ascii_lowercase());
    let branch = format!("colony/{slug}");
    let path = format!("{repo}/{WORKTREE_DIR}/{slug}");

    ensure_excluded(host, &repo).await;
    let base = base_ref(host, &repo).await;
    // --no-track: the branch is the bot's own, not a local copy of origin/main.
    worktree_add(host, &repo, &["--no-track", "-b", &branch, &path, &base]).await?;

    // Keep to the subfolder the session was asked for, if the new checkout has it.
    let mut start = path.clone();
    if !prefix.is_empty() && git(host, &path, &["ls-tree", "HEAD", &format!("{prefix}/")]).await.is_ok_and(|o| !o.trim().is_empty()) {
        start = format!("{path}/{prefix}");
    }
    let dir = if matches!(host, HostId::Windows) { start.replace('/', "\\") } else { start };
    log(format!("made worktree {path} on {branch} from {base} for session {session_id}"));
    let record = Worktree { session_id: session_id.to_string(), host: host.clone(), repo, path, branch, created_at: colony_source::now_ms(), kept: None, folder_gone: false };
    Ok(Created { dir, record })
}

/// What a new worktree starts from: the latest origin/main (or whatever
/// origin's default branch is), so a bot never starts on stale code. Falls back
/// to the last fetched copy when offline, and to the current HEAD when the
/// repository has no remote.
pub async fn base_ref(host: &HostId, repo: &str) -> String {
    // Never hang a spawn on a slow or unreachable remote.
    let fetch = |h: HostId, dir: String| async move {
        match tokio::time::timeout(Duration::from_secs(30), git(&h, &dir, &["fetch", "--quiet", "origin"])).await {
            Ok(r) => r.map(|_| ()),
            Err(_) => Err("timed out".to_string()),
        }
    };
    if let Err(e) = fetch(host.clone(), repo.to_string()).await {
        log(format!("could not fetch origin in {repo}: {e}"));
        // WSL has no GitHub login of its own; a repository on a Windows drive
        // can be fetched by Windows git, which has yours. Refs are shared.
        if let (HostId::Wsl(_), Some(win)) = (host, windows_form(repo)) {
            if let Err(e) = fetch(HostId::Windows, win.clone()).await {
                log(format!("could not fetch origin in {win} either: {e}; using the last fetched origin"));
            }
        }
    }
    let default = git(host, repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).await.ok().map(|s| s.trim().to_string());
    for r in default.into_iter().chain(["origin/main".to_string(), "origin/master".to_string()]) {
        if !r.is_empty() && git(host, repo, &["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")]).await.is_ok() {
            return r;
        }
    }
    "HEAD".into()
}

/// The commit a revision names, if it exists.
pub async fn resolve(host: &HostId, repo: &str, rev: &str) -> Option<String> {
    git(host, repo, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).await.ok().map(|s| s.trim().to_string())
}

/// What `sync_with_base` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Synced {
    UpToDate,
    /// Rebased onto the base; `pushed`: the branch also exists on a remote, so
    /// its next push must be a force push.
    Rebased { commits: u32, pushed: bool },
    /// Left alone, and why.
    Skipped(&'static str),
    /// Everything this branch changes is already in the base (a squash or
    /// rebase merge): rebasing would only conflict with its own commits.
    AlreadyMerged,
    /// The rebase hit conflicts and was aborted; the branch is as it was.
    Conflict,
}

/// Rebase a worktree's branch onto `base` (a freshly fetched origin/main) when
/// it's behind. Never touches uncommitted work and never force-pushes: a dirty
/// worktree, or one already mid-rebase, is skipped; a conflicting rebase is
/// aborted.
pub async fn sync_with_base(w: &Worktree, base: &str) -> Result<Synced, String> {
    let behind: u32 = git(&w.host, &w.path, &["rev-list", "--count", &format!("HEAD..{base}")]).await?.trim().parse().map_err(|_| "unreadable commit count".to_string())?;
    if behind == 0 {
        return Ok(Synced::UpToDate);
    }
    if git(&w.host, &w.path, &["rev-parse", "--verify", "--quiet", "REBASE_HEAD"]).await.is_ok() {
        return Ok(Synced::Skipped("a rebase is already in progress"));
    }
    if !git(&w.host, &w.path, &["status", "--porcelain"]).await?.trim().is_empty() {
        return Ok(Synced::Skipped("it has uncommitted changes"));
    }
    // Merged by squash? Then merging the branch into the base changes nothing,
    // and rebasing it would conflict with the squashed copy of its own commits.
    let own: u32 = git(&w.host, &w.path, &["rev-list", "--count", &format!("{base}..HEAD")]).await?.trim().parse().unwrap_or(0);
    if own > 0 {
        let base_tree = git(&w.host, &w.path, &["rev-parse", &format!("{base}^{{tree}}")]).await.ok();
        let merged_tree = git(&w.host, &w.path, &["merge-tree", "--write-tree", base, "HEAD"]).await.ok();
        if let (Some(b), Some(m)) = (base_tree, merged_tree) {
            if m.lines().next() == Some(b.trim()) {
                return Ok(Synced::AlreadyMerged);
            }
        }
    }
    // Rebasing rewrites commits, which needs a committer identity. If none is
    // configured, use the branch's own author rather than failing.
    let mut args: Vec<String> = Vec::new();
    for (key, field) in [("user.name", "%an"), ("user.email", "%ae")] {
        if git(&w.host, &w.path, &["config", key]).await.map_or(true, |v| v.trim().is_empty()) {
            if let Ok(v) = git(&w.host, &w.path, &["log", "-1", &format!("--format={field}")]).await {
                args.extend(["-c".into(), format!("{key}={}", v.trim())]);
            }
        }
    }
    args.extend(["rebase".into(), base.into()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match git(&w.host, &w.path, &args).await {
        Ok(_) => {
            let pushed = git(&w.host, &w.repo, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/origin/{}", w.branch)]).await.is_ok();
            log(format!("rebased {} onto {base} ({behind} new commit{})", w.branch, if behind == 1 { "" } else { "s" }));
            Ok(Synced::Rebased { commits: behind, pushed })
        }
        Err(e) => {
            // Put the branch back exactly as it was, whatever went wrong.
            let _ = git(&w.host, &w.path, &["rebase", "--abort"]).await;
            if e.contains("could not apply") || e.to_lowercase().contains("conflict") {
                log(format!("rebasing {} onto {base} conflicts; aborted", w.branch));
                Ok(Synced::Conflict)
            } else {
                Err(e)
            }
        }
    }
}

/// Bring back the worktree of a session being resumed if it was cleaned up
/// meanwhile (its branch survives cleanup). Does nothing if it's still there.
pub async fn restore(w: &Worktree) -> Result<(), String> {
    if git(&w.host, &w.path, &["rev-parse", "--is-inside-work-tree"]).await.is_ok() {
        return Ok(());
    }
    let _ = git(&w.host, &w.repo, &["worktree", "prune"]).await;
    let branch_exists = git(&w.host, &w.repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", w.branch)]).await.is_ok();
    let result = if branch_exists {
        worktree_add(&w.host, &w.repo, &[&w.path, &w.branch]).await
    } else {
        // An untouched branch is deleted with its worktree; start it again.
        let base = base_ref(&w.host, &w.repo).await;
        worktree_add(&w.host, &w.repo, &["--no-track", "-b", &w.branch, &w.path, &base]).await
    };
    result.map_err(|e| format!("this session's worktree is gone and couldn't be restored: {e}"))
}

/// `git worktree add`. A repository on a Windows drive is used from both
/// Windows and WSL, whose git each write absolute paths the other can't read
/// (`C:/x` vs `/mnt/c/x`), leaving the worktree broken on one side. Relative
/// links work on both, so use them there (git 2.48+; older git gets the plain form).
async fn worktree_add(host: &HostId, repo: &str, args: &[&str]) -> Result<(), String> {
    if windows_form(repo).is_some() || matches!(host, HostId::Windows) {
        let mut rel = vec!["worktree", "add", "--relative-paths"];
        rel.extend_from_slice(args);
        match git(host, repo, &rel).await {
            Ok(_) => return Ok(()),
            Err(e) if e.contains("unknown option") || e.contains("usage:") => log(format!("git here lacks --relative-paths; using absolute worktree paths ({host})")),
            Err(e) => return Err(e),
        }
    }
    let mut plain = vec!["worktree", "add"];
    plain.extend_from_slice(args);
    git(host, repo, &plain).await.map(|_| ())
}

/// `/mnt/c/x/y` -> `C:/x/y`: a WSL path on a Windows drive, as Windows spells it.
fn windows_form(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/mnt/")?;
    let (drive, tail) = rest.split_once('/').unwrap_or((rest, ""));
    (drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic()).then(|| format!("{}:/{tail}", drive.to_ascii_uppercase()))
}

/// Remove a session's worktree and its branch. Fails, leaving both in place,
/// if the worktree has uncommitted changes. The branch is only deleted once
/// it's merged: `Ok(true)` means the folder is gone but the branch, holding
/// unmerged commits, was kept.
pub async fn remove(w: &Worktree) -> Result<bool, String> {
    if !w.folder_gone {
        remove_folder(w, false).await?;
        log(format!("removed worktree {}", w.path));
    }
    match git(&w.host, &w.repo, &["branch", "-d", &w.branch]).await {
        Ok(_) => Ok(false),
        Err(e) if e.contains("not found") => Ok(false),
        Err(e) => {
            // `-d` compares with the local HEAD, but the branch was cut from
            // origin/main, which may be ahead of it. What matters is whether
            // the branch holds commits that exist nowhere else.
            let own = git(&w.host, &w.repo, &["rev-list", "--count", &w.branch, "--not", "--remotes"]).await.ok().and_then(|s| s.trim().parse::<u32>().ok());
            if own == Some(0) && git(&w.host, &w.repo, &["branch", "-D", &w.branch]).await.is_ok() {
                return Ok(false);
            }
            log(format!("kept branch {}: {e}", w.branch));
            Ok(true)
        }
    }
}

/// Throw a leftover away for good: the folder with whatever is uncommitted in
/// it, and the branch with whatever is unmerged on it.
pub async fn discard(w: &Worktree) -> Result<(), String> {
    if !w.folder_gone {
        match remove_folder(w, true).await {
            Ok(()) => {}
            // Already deleted by hand.
            Err(e) if e.contains("is not a working tree") || e.contains("No such file") => {}
            Err(e) => return Err(e),
        }
    }
    let _ = git(&w.host, &w.repo, &["worktree", "prune"]).await;
    sweep_empty_folder(w).await;
    match git(&w.host, &w.repo, &["branch", "-D", &w.branch]).await {
        Ok(_) => {}
        Err(e) if e.contains("not found") => {}
        Err(e) => return Err(e),
    }
    log(format!("discarded {} and branch {}", w.path, w.branch));
    Ok(())
}

/// Take the worktree folder away. Windows won't delete a folder some process
/// still has open, and a bot that was just ended can hold on for a moment, so
/// a refusal is retried; git may also have unregistered the worktree and
/// emptied it by then, leaving only the empty folder to sweep.
async fn remove_folder(w: &Worktree, force: bool) -> Result<(), String> {
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&w.path);
    let mut attempt = 0;
    loop {
        match git(&w.host, &w.repo, &args).await {
            Ok(_) => return Ok(()),
            Err(e) if e.contains("Permission denied") || e.contains("failed to delete") || e.contains("being used by another process") => {
                if !is_registered(w).await {
                    // Git is done with it; whatever is left is an empty folder.
                    sweep_empty_folder(w).await;
                    return Ok(());
                }
                attempt += 1;
                if attempt >= 6 {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn is_registered(w: &Worktree) -> bool {
    let want = w.path.trim_end_matches('/').to_lowercase();
    git(&w.host, &w.repo, &["worktree", "list", "--porcelain"])
        .await
        .map(|out| out.lines().filter_map(|l| l.strip_prefix("worktree ")).any(|p| p.trim_end_matches('/').to_lowercase() == want))
        .unwrap_or(true)
}

/// Delete what's left of a worktree folder once git no longer knows it, if
/// Windows can reach it (best effort; only ever empty or already unregistered).
async fn sweep_empty_folder(w: &Worktree) {
    let folder = windows_folder(w);
    if folder.starts_with("\\\\") {
        return;
    }
    // The process holding it open may take a few seconds to let go.
    for _ in 0..10 {
        if std::fs::remove_dir_all(&folder).is_ok() || !std::path::Path::new(&folder).exists() {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    log(format!("could not delete the empty folder {folder}"));
}

/// Why cleanup stopped, in words for the map.
pub fn kept_reason(err: &str) -> String {
    if err.contains("modified or untracked") {
        "Has uncommitted changes".into()
    } else if err.contains("Permission denied") || err.contains("failed to delete") || err.contains("another process") {
        "A program still has files in it open".into()
    } else {
        err.lines().next().unwrap_or("Couldn't be removed").trim_start_matches("fatal: ").to_string()
    }
}

/// The folder as Windows' file manager spells it.
pub fn windows_folder(w: &Worktree) -> String {
    match &w.host {
        HostId::Windows => w.path.replace('/', "\\"),
        HostId::Wsl(distro) => match windows_form(&w.path) {
            Some(win) => win.replace('/', "\\"),
            None => format!("\\\\wsl.localhost\\{distro}{}", w.path.replace('/', "\\")),
        },
    }
}

/// Show the folder in the file manager.
pub fn open_folder(w: &Worktree) -> Result<(), String> {
    if w.folder_gone {
        return Err("only the branch is left; there's no folder to open".into());
    }
    std::process::Command::new("explorer.exe").arg(windows_folder(w)).spawn().map(|_| ()).map_err(|e| format!("couldn't open the folder: {e}"))
}

/// Worktree and branch names: lowercase letters, digits and dashes.
fn slugify(label: &str) -> String {
    let mut s = String::new();
    for c in label.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s: String = s.trim_end_matches('-').chars().take(24).collect();
    let s = s.trim_end_matches('-');
    if s.is_empty() { "bot".into() } else { s.into() }
}

/// Keep `.colony/` out of `git status` through the repository's own exclude
/// file, so nothing in the user's tracked files changes. Best effort.
async fn ensure_excluded(host: &HostId, repo: &str) {
    let result: Result<(), String> = async {
        let rel = git(host, repo, &["rev-parse", "--git-path", "info/exclude"]).await?;
        let rel = rel.trim();
        let file = if rel.starts_with('/') || rel.chars().nth(1) == Some(':') { rel.to_string() } else { format!("{repo}/{rel}") };
        match host {
            HostId::Windows => {
                let path = PathBuf::from(file.replace('/', "\\"));
                let old = std::fs::read_to_string(&path).unwrap_or_default();
                if !old.lines().any(|l| l.trim() == ".colony/") {
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                    }
                    let sep = if old.is_empty() || old.ends_with('\n') { "" } else { "\n" };
                    std::fs::write(&path, format!("{old}{sep}.colony/\n")).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            HostId::Wsl(distro) => {
                let script = r#"grep -qxF "$2" "$1" 2>/dev/null || { mkdir -p "$(dirname "$1")" && printf '%s\n' "$2" >> "$1"; }"#;
                let out = quiet("wsl.exe")
                    .args(["-d", distro, "-e", "sh", "-c", script, "sh", &file, ".colony/"])
                    .stdin(Stdio::null())
                    .output()
                    .await
                    .map_err(|e| e.to_string())?;
                if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
            }
        }
    }
    .await;
    if let Err(e) = result {
        log(format!("could not hide .colony/ from git status in {repo}: {e}"));
    }
}

/// Run git in `dir` on the session's host; its stdout, or its error text.
async fn git(host: &HostId, dir: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = match host {
        HostId::Windows => {
            let mut c = quiet("git.exe");
            c.arg("-C").arg(dir);
            c
        }
        HostId::Wsl(distro) => {
            let mut c = quiet("wsl.exe");
            // The variable has to be set inside WSL; without it a fetch that
            // needs credentials waits for a prompt nobody can answer.
            c.args(["-d", distro, "--cd", &to_wsl_path(dir), "-e", "env", "GIT_TERMINAL_PROMPT=0", "git"]);
            c
        }
    };
    let out = cmd.args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).output().await.map_err(|e| format!("couldn't run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(if err.is_empty() { format!("git {} failed", args.first().unwrap_or(&"")) } else { err.to_string() })
    }
}

/// A command that won't flash a console window.
pub(crate) fn quiet(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd
}

// --- what's been made, on disk -------------------------------------------

static STORE: Mutex<()> = Mutex::new(());

fn store_path() -> PathBuf {
    colony_source::colony_home().join("worktrees.json")
}

fn load() -> Vec<Worktree> {
    std::fs::read(store_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(all: &[Worktree]) {
    let body = serde_json::to_vec_pretty(all).expect("serializes");
    if let Err(e) = std::fs::write(store_path(), body) {
        log(format!("could not save worktrees: {e}"));
    }
}

pub fn remember(w: Worktree) {
    let _g = STORE.lock().unwrap();
    let mut all = load();
    all.retain(|x| x.session_id != w.session_id);
    all.push(w);
    save(&all);
}

pub fn all() -> Vec<Worktree> {
    let _g = STORE.lock().unwrap();
    load()
}

pub fn find(session_id: &str) -> Option<Worktree> {
    let _g = STORE.lock().unwrap();
    load().into_iter().find(|w| w.session_id == session_id)
}

/// Leave a worktree (or its branch) where it is and tell the user.
pub fn mark_kept(session_id: &str, reason: String, folder_gone: bool) {
    let _g = STORE.lock().unwrap();
    let mut all = load();
    if let Some(w) = all.iter_mut().find(|w| w.session_id == session_id) {
        w.kept = Some(reason);
        w.folder_gone = folder_gone;
        save(&all);
    }
}

/// What cleanup left behind for the user to decide on.
pub fn leftovers() -> Vec<Worktree> {
    all().into_iter().filter(|w| w.kept.is_some()).collect()
}

pub fn forget(session_id: &str) {
    let _g = STORE.lock().unwrap();
    let mut all = load();
    all.retain(|x| x.session_id != session_id);
    save(&all);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Make a repository with an origin whose main is ahead of the clone's
    /// checkout, in `base` on `host`; returns the clone's folder.
    async fn fixture(host: &HostId, base: &str) -> String {
        let sep = if matches!(host, HostId::Windows) { "\\" } else { "/" };
        let origin = format!("{base}{sep}origin.git");
        let seed = format!("{base}{sep}seed");
        let clone = format!("{base}{sep}clone");
        let id = ["-c", "user.name=t", "-c", "user.email=t@t"];
        let run = |dir: String, args: Vec<&'static str>| {
            let host = host.clone();
            async move { git(&host, &dir, &args).await.unwrap_or_else(|e| panic!("git {args:?} in {dir}: {e}")) }
        };
        // `git -C <dir>` needs the folder to exist; make them through git itself.
        let parent = base.to_string();
        match host {
            HostId::Windows => std::fs::create_dir_all(base).unwrap(),
            HostId::Wsl(d) => {
                let st = quiet("wsl.exe").args(["-d", d, "-e", "mkdir", "-p", &to_wsl_path(base)]).status().await.unwrap();
                assert!(st.success());
            }
        }
        git(host, &parent, &["init", "-q", "--bare", "-b", "main", &inhost(host, &origin)]).await.unwrap();
        git(host, &parent, &["clone", "-q", &inhost(host, &origin), &inhost(host, &seed)]).await.unwrap();
        run(seed.clone(), vec!["checkout", "-q", "-B", "main"]).await;
        git(host, &seed, &[id[0], id[1], id[2], id[3], "commit", "-q", "--allow-empty", "-m", "one"]).await.unwrap();
        git(host, &seed, &["push", "-q", "origin", "main"]).await.unwrap();
        git(host, &parent, &["clone", "-q", &inhost(host, &origin), &inhost(host, &clone)]).await.unwrap();
        // Origin moves on; the clone doesn't know yet.
        git(host, &seed, &[id[0], id[1], id[2], id[3], "commit", "-q", "--allow-empty", "-m", "two"]).await.unwrap();
        git(host, &seed, &["push", "-q", "origin", "main"]).await.unwrap();
        clone
    }

    /// A path as the host's own git wants it on a command line.
    fn inhost(host: &HostId, p: &str) -> String {
        if matches!(host, HostId::Windows) { p.to_string() } else { to_wsl_path(p) }
    }

    async fn exercise(host: HostId, base: String) {
        let clone = fixture(&host, &base).await;
        let latest = git(&host, &format!("{clone}/../seed"), &["rev-parse", "HEAD"]).await.unwrap();

        let made = create(&host, &clone, "1234abcd-0000", "Fix the bug").await.unwrap();
        assert!(made.record.path.ends_with("/.colony/worktrees/fix-the-bug-1234"), "{}", made.record.path);
        // Starts on origin's latest main, fetched just now.
        let head = git(&host, &made.dir, &["rev-parse", "HEAD"]).await.unwrap();
        assert_eq!(head, latest, "worktree is not at origin/main");
        // Hidden from git status, and its branch tracks nothing.
        assert_eq!(git(&host, &clone, &["status", "--porcelain"]).await.unwrap().trim(), "");
        assert!(git(&host, &made.dir, &["rev-parse", "--abbrev-ref", "@{upstream}"]).await.is_err());

        // A repository on a Windows drive is shared by both hosts: the worktree
        // has to work, and not look prunable, from the other side too.
        let other = match &host {
            HostId::Windows => Some(HostId::Wsl(first_distro())),
            HostId::Wsl(_) => windows_form(&made.record.path).map(|_| HostId::Windows),
        };
        if let Some(other) = other.filter(|_| windows_form(&made.record.repo).is_some() || matches!(host, HostId::Windows)) {
            let there = if matches!(other, HostId::Windows) { windows_form(&made.record.path).unwrap() } else { made.record.path.clone() };
            git(&other, &there, &["status", "--porcelain"]).await.unwrap_or_else(|e| panic!("worktree broken from {other}: {e}"));
            let list = git(&other, &there, &["worktree", "list"]).await.unwrap();
            assert!(!list.contains("prunable"), "{other} sees the worktree as prunable:\n{list}");
        }

        // A second bot gets its own checkout.
        let other = create(&host, &clone, "Fix the bug", "5678efgh").await.unwrap();
        assert_ne!(other.dir, made.dir);

        // Removing and restoring (a resumed session).
        // No commits of its own, so even with origin ahead of local main the branch goes too.
        assert!(!remove(&made.record).await.unwrap(), "an untouched branch should not be reported as holding work");
        assert!(git(&host, &made.dir, &["rev-parse", "--is-inside-work-tree"]).await.is_err());
        assert!(git(&host, &clone, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", made.record.branch)]).await.is_err());
        restore(&made.record).await.unwrap();
        assert!(git(&host, &made.dir, &["rev-parse", "--is-inside-work-tree"]).await.is_ok());

        // A commit of its own is work worth keeping: the folder goes, the branch stays.
        let id = ["-c", "user.name=t", "-c", "user.email=t@t"];
        git(&host, &made.dir, &[id[0], id[1], id[2], id[3], "commit", "-q", "--allow-empty", "-m", "mine"]).await.unwrap();
        assert!(remove(&made.record).await.unwrap(), "unmerged commits must keep the branch");
        let mut gone = made.record.clone();
        gone.folder_gone = true;
        // Discarding deletes the leftover branch outright.
        discard(&gone).await.unwrap();
        assert!(git(&host, &clone, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", made.record.branch)]).await.is_err());
        restore(&made.record).await.unwrap();

        // Uncommitted work blocks removal and is left alone.
        let file = if matches!(host, HostId::Windows) { format!("{}\\scratch.txt", made.dir) } else { format!("{}/scratch.txt", made.dir) };
        match &host {
            HostId::Windows => std::fs::write(&file, "x").unwrap(),
            HostId::Wsl(d) => assert!(quiet("wsl.exe").args(["-d", d, "-e", "touch", &to_wsl_path(&file)]).status().await.unwrap().success()),
        }
        assert!(remove(&made.record).await.is_err());
        assert!(git(&host, &made.dir, &["rev-parse", "--is-inside-work-tree"]).await.is_ok());
    }

    async fn write_file(host: &HostId, path: &str, content: &str) {
        match host {
            HostId::Windows => std::fs::write(path, content).unwrap(),
            HostId::Wsl(d) => {
                let st = quiet("wsl.exe").args(["-d", d, "-e", "sh", "-c", "printf '%s' \"$2\" > \"$1\"", "sh", &to_wsl_path(path), content]).status().await.unwrap();
                assert!(st.success());
            }
        }
    }

    /// Origin's main moves on while a worktree exists: it is rebased when it
    /// can be, and left alone when it can't.
    async fn exercise_sync(host: HostId, base: String) {
        let sep = if matches!(host, HostId::Windows) { "\\" } else { "/" };
        let clone = fixture(&host, &base).await;
        let seed = format!("{base}{sep}seed");
        let id = ["-c", "user.name=t", "-c", "user.email=t@t"];
        let push_commit = |file: &'static str, content: &'static str| {
            let (host, seed) = (host.clone(), seed.clone());
            async move {
                write_file(&host, &format!("{seed}{}{file}", if matches!(host, HostId::Windows) { "\\" } else { "/" }), content).await;
                git(&host, &seed, &["add", file]).await.unwrap();
                git(&host, &seed, &[id[0], id[1], id[2], id[3], "commit", "-q", "-m", file]).await.unwrap();
                git(&host, &seed, &["push", "-q", "origin", "main"]).await.unwrap();
            }
        };

        // Behind by nothing yet (the fixture's own extra commit was fetched at creation).
        let made = create(&host, &clone, "aaaa1111", "sync me").await.unwrap();
        let w = &made.record;
        let base_now = base_ref(&host, &clone).await;
        assert_eq!(sync_with_base(w, &base_now).await.unwrap(), Synced::UpToDate);

        // Main moves; the worktree is clean, so it is rebased and carries its own commit along.
        write_file(&host, &format!("{}{sep}mine.txt", made.dir), "mine").await;
        git(&host, &made.dir, &["add", "mine.txt"]).await.unwrap();
        git(&host, &made.dir, &[id[0], id[1], id[2], id[3], "commit", "-q", "-m", "mine"]).await.unwrap();
        push_commit("a.txt", "from main").await;
        let base_now = base_ref(&host, &clone).await;
        assert_eq!(sync_with_base(w, &base_now).await.unwrap(), Synced::Rebased { commits: 1, pushed: false });
        let log = git(&host, &made.dir, &["log", "--format=%s", "-3"]).await.unwrap();
        assert_eq!(log.lines().collect::<Vec<_>>(), vec!["mine", "a.txt", "two"], "own commit should sit on top of main");

        // Uncommitted work is never touched.
        push_commit("b.txt", "more").await;
        write_file(&host, &format!("{}{sep}scratch.txt", made.dir), "wip").await;
        let base_now = base_ref(&host, &clone).await;
        assert_eq!(sync_with_base(w, &base_now).await.unwrap(), Synced::Skipped("it has uncommitted changes"));
        assert_eq!(git(&host, &made.dir, &["status", "--porcelain"]).await.unwrap().trim(), "?? scratch.txt");

        // Clean again, but now both sides changed the same file: conflict, aborted, branch untouched.
        git(&host, &made.dir, &["add", "scratch.txt"]).await.unwrap();
        git(&host, &made.dir, &[id[0], id[1], id[2], id[3], "commit", "-q", "-m", "scratch"]).await.unwrap();
        write_file(&host, &format!("{}{sep}c.txt", made.dir), "theirs").await;
        git(&host, &made.dir, &["add", "c.txt"]).await.unwrap();
        git(&host, &made.dir, &[id[0], id[1], id[2], id[3], "commit", "-q", "-m", "c mine"]).await.unwrap();
        push_commit("c.txt", "ours").await;
        let before = git(&host, &made.dir, &["rev-parse", "HEAD"]).await.unwrap();
        let base_now = base_ref(&host, &clone).await;
        assert_eq!(sync_with_base(w, &base_now).await.unwrap(), Synced::Conflict);
        assert_eq!(git(&host, &made.dir, &["rev-parse", "HEAD"]).await.unwrap(), before, "a conflicting rebase must leave the branch as it was");
        assert_eq!(git(&host, &made.dir, &["status", "--porcelain"]).await.unwrap().trim(), "", "and the worktree clean");
        assert!(git(&host, &made.dir, &["rev-parse", "--verify", "--quiet", "REBASE_HEAD"]).await.is_err());

        // Squash-merged: the branch's two commits land on main as one. Rebasing would
        // conflict with the squashed copy of its own work, so it is recognised instead.
        let sq = create(&host, &clone, "bbbb2222", "squashed").await.unwrap();
        write_file(&host, &format!("{}{sep}d.txt", sq.dir), "1").await;
        git(&host, &sq.dir, &["add", "d.txt"]).await.unwrap();
        git(&host, &sq.dir, &[id[0], id[1], id[2], id[3], "commit", "-q", "-m", "d one"]).await.unwrap();
        write_file(&host, &format!("{}{sep}d.txt", sq.dir), "12").await;
        git(&host, &sq.dir, &[id[0], id[1], id[2], id[3], "commit", "-q", "-am", "d two"]).await.unwrap();
        push_commit("d.txt", "12").await;
        let base_now = base_ref(&host, &clone).await;
        assert_eq!(sync_with_base(&sq.record, &base_now).await.unwrap(), Synced::AlreadyMerged);
        // A branch with no work of its own is simply moved forward, not mistaken for merged.
        let fresh = create(&host, &clone, "cccc3333", "fresh").await.unwrap();
        push_commit("e.txt", "later").await;
        let base_now = base_ref(&host, &clone).await;
        assert_eq!(sync_with_base(&fresh.record, &base_now).await.unwrap(), Synced::Rebased { commits: 1, pushed: false });
    }

    fn first_distro() -> String {
        let out = std::process::Command::new("wsl.exe").args(["--list", "--quiet"]).output().unwrap();
        let units: Vec<u16> = out.stdout.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units).lines().map(|l| l.trim().to_string()).find(|l| !l.is_empty() && !l.starts_with("docker-desktop")).expect("a WSL distro")
    }

    fn rt() ->tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    /// Needs git on Windows: `cargo test -p colonyd -- --ignored worktree`.
    #[test]
    #[ignore]
    fn worktrees_on_windows() {
        let base = std::env::temp_dir().join(format!("colony-wt-{}", std::process::id()));
        let base_s = base.display().to_string();
        rt().block_on(exercise(HostId::Windows, base_s.clone()));
        rt().block_on(exercise_sync(HostId::Windows, format!("{base_s}-sync")));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(format!("{base_s}-sync"));
    }

    /// Needs a WSL distro with git; uses the first, a Linux path.
    #[test]
    #[ignore]
    fn worktrees_in_wsl() {
        let distro = first_distro();
        let base = format!("/tmp/colony-wt-{}", std::process::id());
        rt().block_on(exercise(HostId::Wsl(distro.clone()), base.clone()));
        rt().block_on(exercise_sync(HostId::Wsl(distro.clone()), format!("{base}-sync")));
        let _ = std::process::Command::new("wsl.exe").args(["-d", &distro, "-e", "rm", "-rf", &base, &format!("{base}-sync")]).status();
    }

    /// A bot that was just ended can still hold its folder open for a moment,
    /// which Windows answers with "Permission denied". Cleanup must ride it out.
    #[test]
    #[ignore]
    fn removal_waits_out_a_process_still_inside_the_folder() {
        let base = std::env::temp_dir().join(format!("colony-wtlock-{}", std::process::id()));
        let base_s = base.display().to_string();
        rt().block_on(async {
            let clone = fixture(&HostId::Windows, &base_s).await;
            let made = create(&HostId::Windows, &clone, "dddd4444", "locked").await.unwrap();
            // Stands in for the ended bot: a process whose working directory is the worktree.
            let mut holder = std::process::Command::new("cmd.exe").args(["/C", "ping -n 4 127.0.0.1 >nul"]).current_dir(&made.dir).spawn().unwrap();
            let kept_branch = remove(&made.record).await.unwrap();
            assert!(!kept_branch, "no commits of its own, so the branch goes too");
            assert!(!std::path::Path::new(&made.dir).exists(), "the folder should be gone once the holder lets go");
            let _ = holder.wait();
        });
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A Windows folder opened from WSL (`/mnt/c/...`), the usual way to share
    /// a checkout between both.
    #[test]
    #[ignore]
    fn worktrees_in_wsl_on_a_windows_folder() {
        let distro = first_distro();
        let base = std::env::temp_dir().join(format!("colony-wtm-{}", std::process::id()));
        let base_s = base.display().to_string().replace('\\', "/");
        rt().block_on(exercise(HostId::Wsl(distro.clone()), base_s.clone()));
        rt().block_on(exercise_sync(HostId::Wsl(distro), format!("{base_s}-sync")));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(format!("{base_s}-sync"));
    }

    fn record(host: HostId, path: &str) -> Worktree {
        Worktree { session_id: "s".into(), host, repo: "r".into(), path: path.into(), branch: "colony/x".into(), created_at: 0, kept: None, folder_gone: false }
    }

    #[test]
    fn folders_are_shown_the_way_windows_spells_them() {
        assert_eq!(windows_folder(&record(HostId::Windows, "C:/code/api/.colony/worktrees/x")), r"C:\code\api\.colony\worktrees\x");
        let wsl = HostId::Wsl("Ubuntu".into());
        assert_eq!(windows_folder(&record(wsl.clone(), "/mnt/c/code/api/.colony/worktrees/x")), r"C:\code\api\.colony\worktrees\x");
        assert_eq!(windows_folder(&record(wsl, "/home/t/api/.colony/worktrees/x")), r"\\wsl.localhost\Ubuntu\home\t\api\.colony\worktrees\x");
    }

    #[test]
    fn kept_reasons_are_plain() {
        assert_eq!(kept_reason("fatal: 'C:/x' contains modified or untracked files, use --force to delete it"), "Has uncommitted changes");
        assert_eq!(kept_reason("fatal: something else broke\nmore"), "something else broke");
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Fix the login bug!"), "fix-the-login-bug");
        assert_eq!(slugify("  "), "bot");
        assert_eq!(slugify("a/b\\c"), "a-b-c");
        assert!(slugify(&"x".repeat(60)).len() <= 24);
    }
}
