//! Unblocking a bot that is stuck on a login (`gh`, `az`, ...).
//!
//! Colony opens the provider's login in a terminal on the bot's own host and
//! shows it in the map, where the user finishes the browser or device flow.
//! When that terminal closes, Colony runs the provider's status command; if it
//! passes, the bot's sign-in prompt clears and the bot is told to retry.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use colony_core::auth::{provider, Provider};
use colony_core::paths::to_wsl_path;
use colony_core::{DomainEvent, Envelope, HostId};
use colony_source::now_ms;
use tokio::process::Command;

use crate::pty::UtilityCommand;
use crate::{log, Shared};

/// The user has this long to finish signing in before Colony stops waiting.
const SIGN_IN_WINDOW: Duration = Duration::from_secs(30 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// Pause between pasting the retry message and pressing Enter.
const PASTE_SETTLE: Duration = Duration::from_millis(80);

/// Open the sign-in terminal for the login this agent is waiting on; returns
/// its terminal id for the map to show.
pub async fn start(shared: &Arc<Shared>, id: &str, cols: u16, rows: u16) -> Result<String, String> {
    let (need, host, dir, session_id, bot_term) = {
        let colony = shared.colony.read().await;
        let a = colony.agents.get(id).ok_or("no such session")?;
        let need = a.auth_need.clone().ok_or("this bot isn't waiting on a sign-in")?;
        let dir = a.project_dir.clone().or_else(|| a.cwd.clone()).ok_or("Colony doesn't know this session's folder")?;
        let bot_term = colony.agents.get(&a.session_id).and_then(|m| m.terminal.clone());
        (need, a.host.clone(), dir, a.session_id.clone(), bot_term)
    };
    let p = provider(&need.provider).ok_or("unknown sign-in")?;
    let term = shared.pty.spawn_utility(&host, &dir, cols, rows, &login_command(p)).await?;

    let shared = shared.clone();
    let watched = term.clone();
    tokio::spawn(async move {
        shared.pty.closed(&watched, SIGN_IN_WINDOW).await;
        finish(&shared, p, &host, &dir, &session_id, bot_term.as_deref()).await;
    });
    Ok(term)
}

/// The login steps, then a pause so the result can be read before the
/// terminal closes.
fn login_command(p: &Provider) -> UtilityCommand {
    let steps = p.login.iter().map(|argv| argv.join(" ")).collect::<Vec<_>>().join("; ");
    UtilityCommand {
        what: format!("{} sign-in", p.label),
        powershell: format!("{steps}; Write-Host ''; Read-Host 'Press Enter to close' | Out-Null"),
        bash: format!("{steps}; echo; read -rp 'Press Enter to close ' _"),
    }
}

async fn finish(shared: &Shared, p: &Provider, host: &HostId, dir: &str, session_id: &str, bot_term: Option<&str>) {
    if !signed_in(p, host, dir).await {
        log(format!("{} sign-in for session {session_id} didn't take; the bot keeps its prompt", p.label));
        return;
    }
    log(format!("signed in to {} for session {session_id}", p.label));
    let _ = shared
        .events
        .send(Envelope { ts: now_ms(), host: host.clone(), session_id: session_id.to_string(), cwd: None, event: DomainEvent::AuthResolved })
        .await;
    // The bot has probably told the user it's stuck and stopped; tell it to carry on.
    if let Some(term) = bot_term {
        let note = format!("I've signed in to {}. Please retry what failed.", p.label);
        if shared.pty.paste(term, &note).is_ok() {
            tokio::time::sleep(PASTE_SETTLE).await;
            let _ = shared.pty.input(term, b"\r");
        }
    }
}

/// Whether the provider's status command passes on that host.
async fn signed_in(p: &Provider, host: &HostId, dir: &str) -> bool {
    let mut cmd = match host {
        HostId::Windows => {
            let mut c = Command::new("cmd.exe");
            c.arg("/C").args(p.check).current_dir(dir);
            c
        }
        HostId::Wsl(distro) => {
            // A login shell, so tools installed under ~/.local/bin are found.
            let mut c = Command::new("wsl.exe");
            c.args(["-d", distro, "--cd", &to_wsl_path(dir), "-e", "bash", "-lc", "exec \"$@\"", "bash"]).args(p.check);
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
        let c = login_command(provider("github").unwrap());
        assert_eq!(c.bash, "gh auth login; gh auth setup-git; echo; read -rp 'Press Enter to close ' _");
        assert!(c.powershell.starts_with("gh auth login; gh auth setup-git; "));
    }

    /// Needs the GitHub CLI signed in on Windows: `cargo test -p colonyd -- --ignored signed_in`.
    #[test]
    #[ignore]
    fn signed_in_checks_the_providers_status_command() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let dir = std::env::temp_dir().display().to_string();
        assert!(rt.block_on(signed_in(provider("github").unwrap(), &HostId::Windows, &dir)));
        // Not installed in this distro, or not signed in: false either way.
        assert!(!rt.block_on(signed_in(provider("github").unwrap(), &HostId::Wsl("no-such-distro".into()), "/tmp")));
    }
}
