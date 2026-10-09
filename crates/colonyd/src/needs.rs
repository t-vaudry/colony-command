//! Unblocking a bot that is stuck on something only the user can fix: a
//! login (`gh`, `az`, ...) or a program that isn't installed.
//!
//! Colony opens the login or the install in a terminal on the bot's own host
//! and shows it in the map, where the user finishes it (a browser flow, a
//! sudo password). When that terminal closes, Colony runs the provider's
//! check; if it passes, the bot's prompt clears and the bot is told to retry.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use colony_core::auth::{provider, tool, NeedKind};
use colony_core::paths::to_wsl_path;
use colony_core::{DomainEvent, Envelope, HostId};
use colony_source::now_ms;
use tokio::process::Command;

use crate::pty::UtilityCommand;
use crate::{log, Shared};

/// The user has this long to finish before Colony stops waiting.
const WINDOW: Duration = Duration::from_secs(30 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Pause between pasting the retry message and pressing Enter.
const PASTE_SETTLE: Duration = Duration::from_millis(80);

/// WSL has no browser of its own, so a login that opens one (`gh auth login`)
/// fails there. Unless the user chose a BROWSER, open links in the Windows
/// default browser: through wslview if it's installed, else PowerShell.
const OPEN_BROWSER_ON_WINDOWS: &str = "if [ -z \"$BROWSER\" ]; then if command -v wslview >/dev/null 2>&1; then export BROWSER=wslview; \
     else export BROWSER='powershell.exe -NoProfile -Command Start-Process'; fi; fi;";

/// What to run for one need, and how to tell it worked.
struct Plan {
    label: String,
    kind: NeedKind,
    command: UtilityCommand,
    check: &'static [&'static str],
}

/// Open the terminal that fixes the need this agent is waiting on; returns its
/// terminal id for the map to show.
pub async fn start(shared: &Arc<Shared>, id: &str, cols: u16, rows: u16) -> Result<String, String> {
    let (need, host, dir, session_id, bot_term) = {
        let colony = shared.colony.read().await;
        let a = colony.agents.get(id).ok_or("no such session")?;
        let need = a.auth_need.clone().ok_or("this bot isn't waiting on a sign-in or an install")?;
        let dir = a.project_dir.clone().or_else(|| a.cwd.clone()).ok_or("Colony doesn't know this session's folder")?;
        let bot_term = colony.agents.get(&a.session_id).and_then(|m| m.terminal.clone());
        (need, a.host.clone(), dir, a.session_id.clone(), bot_term)
    };
    let plan = plan(need.kind, &need.provider)?;
    let term = shared.pty.spawn_utility(&host, &dir, cols, rows, &plan.command).await?;

    let shared = shared.clone();
    let watched = term.clone();
    tokio::spawn(async move {
        shared.pty.closed(&watched, WINDOW).await;
        finish(&shared, &plan, &host, &dir, &session_id, bot_term.as_deref()).await;
    });
    Ok(term)
}

fn plan(kind: NeedKind, id: &str) -> Result<Plan, String> {
    match kind {
        NeedKind::SignIn => {
            let p = provider(id).ok_or("unknown sign-in")?;
            let steps = p.login.iter().map(|argv| argv.join(" ")).collect::<Vec<_>>().join("; ");
            let bash = format!("{OPEN_BROWSER_ON_WINDOWS} {steps}");
            Ok(Plan { label: p.label.into(), kind, command: pause_after(format!("{} sign-in", p.label), &steps, &bash), check: p.check })
        }
        NeedKind::Install => {
            let t = tool(id).ok_or("unknown program")?;
            let powershell = format!("winget install --id {} -e --accept-source-agreements --accept-package-agreements", t.winget);
            let bash = format!(
                "if command -v apt-get >/dev/null 2>&1; then sudo apt-get update && {}; else echo 'No apt-get in this distro; install {} with its package manager.'; fi",
                t.apt, t.label
            );
            Ok(Plan { label: t.label.into(), kind, command: pause_after(format!("install of {}", t.label), &powershell, &bash), check: t.check })
        }
    }
}

/// A command in each shell's syntax, then a pause so the result can be read
/// before the terminal closes.
fn pause_after(what: String, powershell: &str, bash: &str) -> UtilityCommand {
    UtilityCommand {
        what,
        powershell: format!("{powershell}; Write-Host ''; Read-Host 'Press Enter to close' | Out-Null"),
        bash: format!("{bash}; echo; read -rp 'Press Enter to close ' _"),
    }
}

async fn finish(shared: &Shared, plan: &Plan, host: &HostId, dir: &str, session_id: &str, bot_term: Option<&str>) {
    if !passes(plan.check, host, dir).await {
        log(format!("{} didn't take for session {session_id}; the bot keeps its prompt", plan.label));
        return;
    }
    log(format!("{} is ready for session {session_id}", plan.label));
    let note = match plan.kind {
        NeedKind::SignIn => format!("I've signed in to {}. Please retry what failed.", plan.label),
        NeedKind::Install => format!("I've installed {}. Please retry what failed.", plan.label),
    };
    // A running Claude keeps the PATH it started with, so on Windows it can't
    // see a program installed since. Restart it (same conversation, fresh
    // environment) with the retry request as its first message. Before the
    // prompt clears: a bot that looks busy can't be restarted.
    let mut told = false;
    if plan.kind == NeedKind::Install && matches!(host, HostId::Windows) && bot_term.is_some() {
        match crate::api::restart_session(shared, session_id, None, Some(note.clone())).await {
            Ok(()) => told = true,
            Err(e) => log(format!("couldn't restart session {session_id} to pick up {}: {e}", plan.label)),
        }
    }
    let _ = shared
        .events
        .send(Envelope { ts: now_ms(), host: host.clone(), session_id: session_id.to_string(), cwd: None, event: DomainEvent::AuthResolved })
        .await;
    // The bot has probably told the user it's stuck and stopped; tell it to carry on.
    if let (false, Some(term)) = (told, bot_term) {
        tell(shared, term, &note).await;
    }
}

/// Type a message to a bot in its terminal and submit it.
pub async fn tell(shared: &Shared, term: &str, text: &str) {
    if shared.pty.paste(term, text).is_ok() {
        tokio::time::sleep(PASTE_SETTLE).await;
        let _ = shared.pty.input(term, b"\r");
    }
}

/// Whether the check command passes on that host.
async fn passes(check: &[&str], host: &HostId, dir: &str) -> bool {
    let mut cmd = match host {
        HostId::Windows => {
            let mut c = Command::new("cmd.exe");
            // The PATH a new session would get, which includes anything just installed.
            c.arg("/C").args(check).current_dir(dir).env_clear().envs(crate::pty::child_env());
            c
        }
        HostId::Wsl(distro) => {
            // A login shell, so tools installed under ~/.local/bin are found.
            let mut c = Command::new("wsl.exe");
            c.args(["-d", distro, "--cd", &to_wsl_path(dir), "-e", "bash", "-lc", "exec \"$@\"", "bash"]).args(check);
            c
        }
    };
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(CHECK_TIMEOUT, cmd.status()).await {
        Ok(Ok(status)) => status.success(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_login_signs_in_then_sets_up_git() {
        let p = plan(NeedKind::SignIn, "github").unwrap();
        assert!(p.command.bash.ends_with(" gh auth login; gh auth setup-git; echo; read -rp 'Press Enter to close ' _"), "{}", p.command.bash);
        assert!(p.command.powershell.starts_with("gh auth login; gh auth setup-git; "));
        // Only WSL needs a browser pointed at Windows.
        assert!(p.command.bash.contains("BROWSER") && !p.command.powershell.contains("BROWSER"));
    }

    #[test]
    fn installs_use_winget_on_windows_and_apt_in_wsl() {
        let p = plan(NeedKind::Install, "gh").unwrap();
        assert!(p.command.powershell.starts_with("winget install --id GitHub.cli -e "));
        assert!(p.command.bash.contains("sudo apt-get update && sudo apt-get install -y gh;"), "{}", p.command.bash);
        assert!(p.command.bash.contains("else echo 'No apt-get"));
    }

    /// Needs the GitHub CLI signed in on Windows: `cargo test -p colonyd -- --ignored passes`.
    #[test]
    #[ignore]
    fn passes_runs_the_check_on_the_host() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let dir = std::env::temp_dir().display().to_string();
        assert!(rt.block_on(passes(&["gh", "auth", "status"], &HostId::Windows, &dir)));
        assert!(!rt.block_on(passes(&["gh", "--version"], &HostId::Wsl("no-such-distro".into()), "/tmp")));
        assert!(!rt.block_on(passes(&["no-such-program-xyz"], &HostId::Windows, &dir)));
    }
}
