//! Files and lines changed for work that is ready to review.
//!
//! A background task watches for main agents that just became ready to review,
//! runs git in the session's folder (a Colony worktree for isolated sessions),
//! and puts the count on the agent. It runs off the reducer's path, each git
//! call has a timeout, and a repository git cannot read, a missing folder, or
//! anything unexpected gives no count at all: the map shows nothing rather than
//! something wrong. Uncommitted and untracked work is counted; for an isolated
//! session, so are the commits it made on its branch.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use colony_core::paths::to_wsl_path;
use colony_core::state::{AgentKind, AgentState};
use colony_core::workdir::DiffStat;
use colony_core::HostId;

use crate::{delta_messages, worktree, Shared};

const POLL: Duration = Duration::from_secs(2);
const GIT_TIMEOUT: Duration = Duration::from_secs(8);
/// One count in all, however many git calls it takes, so a slow repository or a
/// cold WSL distro can't hold up the agents queued behind it.
const COUNT_DEADLINE: Duration = Duration::from_secs(15);
/// Counted per pass, so a fleet turning ready together doesn't flood git.
const PER_PASS: usize = 3;
/// More untracked files than this and their lines aren't counted one by one.
const MAX_UNTRACKED: usize = 30;

/// Count changes for agents that become ready to review, for the life of the daemon.
pub async fn run(shared: Arc<Shared>) {
    // Agent id -> the review (state_since) already tried, so a repository git
    // cannot read isn't asked again every pass.
    let mut tried: HashMap<String, u64> = HashMap::new();
    let synthetic = crate::api::ingest_enabled();
    loop {
        tokio::time::sleep(POLL).await;
        let todo: Vec<(String, u64, HostId, String)> = {
            let colony = shared.colony.read().await;
            tried.retain(|id, _| colony.agents.contains_key(id));
            colony
                .agents
                .values()
                .filter(|a| a.kind == AgentKind::Main && a.state == AgentState::ReadyToReview && a.diff_stat.is_none())
                .filter(|a| tried.get(&a.id) != Some(&a.state_since))
                .filter_map(|a| Some((a.id.clone(), a.state_since, a.host.clone(), a.cwd.clone()?)))
                .take(PER_PASS)
                .collect()
        };
        for (id, since, host, cwd) in todo {
            tried.insert(id.clone(), since);
            let stat = if synthetic { Some(fake(&id)) } else { tokio::time::timeout(COUNT_DEADLINE, count(&host, &cwd, worktree::find(&id).is_some())).await.ok().flatten() };
            let Some(stat) = stat else { continue };
            let mut colony = shared.colony.write().await;
            let changed = colony.set_diff_stat(&id, since, stat);
            for msg in delta_messages(&colony, &changed) {
                let _ = shared.deltas.send(msg);
            }
        }
    }
}

/// Test daemons (fed by tools/synth) have no repositories: show a stable
/// made-up count so the dock can be seen under load.
fn fake(id: &str) -> DiffStat {
    let h = id.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
    DiffStat { files: 1 + h % 12, added: (h >> 4) % 400, removed: (h >> 14) % 150 }
}

/// Changed files and lines in the repository containing `dir`, or None when
/// git cannot say.
pub async fn count(host: &HostId, dir: &str, isolated: bool) -> Option<DiffStat> {
    let top = git(host, dir, &["rev-parse", "--show-toplevel"], &[0]).await?;
    let top = top.lines().next()?.trim().to_string();
    if top.is_empty() {
        return None;
    }
    let top = top.as_str();
    // An isolated session's branch may hold commits of its own: count from
    // where it left the default branch. Otherwise, from the last commit.
    let mut base = "HEAD".to_string();
    if isolated {
        // Without a known default branch the branch's own commits can't be told
        // apart, and counting only the working tree would understate the work.
        let mut found = None;
        let head = git(host, top, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], &[0]).await;
        let mut candidates: Vec<String> = head.into_iter().map(|s| s.trim().to_string()).collect();
        candidates.extend(["origin/main", "origin/master"].map(String::from));
        for def in candidates.into_iter().filter(|s| !s.is_empty()) {
            if let Some(mb) = git(host, top, &["merge-base", "HEAD", &def], &[0]).await {
                if !mb.trim().is_empty() {
                    found = Some(mb.trim().to_string());
                    break;
                }
            }
        }
        base = found?;
    }
    let tracked = git(host, top, &["diff", "--numstat", "--no-ext-diff", "--no-renames", &base, "--"], &[0]).await?;
    let mut stat = parse_numstat(&tracked);

    let untracked = git(host, top, &["ls-files", "-z", "--others", "--exclude-standard"], &[0]).await?;
    let files: Vec<&str> = untracked.split('\0').filter(|f| !f.is_empty()).collect();
    if files.len() > MAX_UNTRACKED {
        return None;
    }
    for f in files {
        // `--no-index` exits 1 when the files differ, which is the point.
        let out = git(host, top, &["diff", "--no-index", "--numstat", "--no-ext-diff", "--", "/dev/null", f], &[0, 1]).await?;
        let one = parse_numstat(&out);
        stat.files += one.files.max(1);
        stat.added += one.added;
        stat.removed += one.removed;
    }
    Some(stat)
}

/// `added<TAB>removed<TAB>path` per file; binary files show `-` and count as a
/// changed file with no lines.
pub fn parse_numstat(out: &str) -> DiffStat {
    let mut s = DiffStat { files: 0, added: 0, removed: 0 };
    for line in out.lines() {
        let mut it = line.splitn(3, '\t');
        let (Some(a), Some(r), Some(_)) = (it.next(), it.next(), it.next()) else { continue };
        s.files += 1;
        s.added = s.added.saturating_add(a.parse().unwrap_or(0));
        s.removed = s.removed.saturating_add(r.parse().unwrap_or(0));
    }
    s
}

/// Run git in `dir` on the session's host with a timeout. Stdout when git
/// exits with one of `ok`, else None.
async fn git(host: &HostId, dir: &str, args: &[&str], ok: &[i32]) -> Option<String> {
    let mut cmd = match host {
        HostId::Windows => {
            let mut c = worktree::quiet("git.exe");
            c.arg("-C").arg(dir);
            c
        }
        HostId::Wsl(distro) => {
            let mut c = worktree::quiet("wsl.exe");
            c.args(["-d", distro, "--cd", &to_wsl_path(dir), "-e", "env", "GIT_TERMINAL_PROMPT=0", "GIT_OPTIONAL_LOCKS=0", "git"]);
            c
        }
    };
    // Optional locks off: a read-only look must not fight the agent's own git over the index.
    cmd.args(["-c", "core.quotepath=off"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    let code = out.status.code()?;
    ok.contains(&code).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_sums_files_and_lines() {
        let s = parse_numstat("10\t2\tsrc/a.rs\n3\t0\tb.txt\n-\t-\timg.png\n");
        assert_eq!(s, DiffStat { files: 3, added: 13, removed: 2 });
        assert_eq!(parse_numstat(""), DiffStat { files: 0, added: 0, removed: 0 });
        assert_eq!(parse_numstat("garbage").files, 0);
    }

    #[test]
    fn made_up_counts_are_stable() {
        assert_eq!(fake("s1"), fake("s1"));
    }

    /// Counts tracked and untracked changes in a scratch repository (Windows git).
    #[cfg(windows)]
    #[tokio::test]
    async fn counts_a_real_repository() {
        let dir = std::env::temp_dir().join(format!("colony-diffstat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().to_string();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&d).args(args).output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        run(&["add", "."]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap(); // +2 -1
        std::fs::write(dir.join("new.txt"), "x\ny\n").unwrap(); // untracked, +2
        let got = count(&HostId::Windows, &d, false).await;
        let missing = count(&HostId::Windows, &format!("{d}\\nope"), false).await;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, Some(DiffStat { files: 2, added: 4, removed: 1 }));
        assert_eq!(missing, None);
    }
}
