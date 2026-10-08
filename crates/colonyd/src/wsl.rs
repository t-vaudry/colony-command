//! Attaches a colony-probe to every running WSL distro and forwards its
//! events. Distros are never started just to watch them: a stopped distro has
//! no sessions anyway. Also keeps the list of installed distros current for
//! the New session dialog.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use colony_core::Envelope;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::{log, Shared};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const RESCAN: Duration = Duration::from_secs(15);
/// The probe is installed per distro by `scripts/install-probe.sh`.
const PROBE: &str = "exec \"$HOME/.colony/bin/colony-probe\"";

pub async fn supervise(tx: mpsc::Sender<Envelope>, shared: Arc<Shared>) {
    let attached: Arc<Mutex<HashSet<String>>> = Arc::default();
    loop {
        *shared.distros.write().await = list_distros(false).await;
        for distro in list_distros(true).await {
            if !attached.lock().unwrap().insert(distro.clone()) {
                continue;
            }
            let (tx, attached) = (tx.clone(), attached.clone());
            tokio::spawn(async move {
                if let Err(e) = run_probe(&distro, tx).await {
                    log(format!("probe in {distro}: {e}"));
                }
                attached.lock().unwrap().remove(&distro);
            });
        }
        tokio::select! {
            _ = tokio::time::sleep(RESCAN) => {}
            // A session was just started in WSL; give the distro a moment to boot.
            _ = shared.wsl_wake.notified() => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
}

async fn list_distros(running_only: bool) -> Vec<String> {
    let args: &[&str] = if running_only { &["--list", "--running", "--quiet"] } else { &["--list", "--quiet"] };
    let out = Command::new("wsl.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await;
    let Ok(out) = out else { return Vec::new() };
    decode_wsl_output(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
        .collect()
}

/// wsl.exe writes UTF-16LE when its output is redirected.
fn decode_wsl_output(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes.iter().skip(1).step_by(2).all(|&b| b == 0) {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

async fn run_probe(distro: &str, tx: mpsc::Sender<Envelope>) -> std::io::Result<()> {
    let mut child = Command::new("wsl.exe")
        .args(["-d", distro, "-e", "sh", "-c", PROBE])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut stderr = BufReader::new(child.stderr.take().expect("piped")).lines();
    let mut hello = false;
    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(l) if !hello => {
                    hello = true;
                    log(format!("probe attached in {distro}: {l}"));
                }
                Some(l) => match serde_json::from_str::<Envelope>(&l) {
                    Ok(e) => if tx.send(e).await.is_err() { return Ok(()) },
                    Err(e) => log(format!("probe in {distro} sent a bad line ({e}): {l}")),
                },
                None => break,
            },
            Ok(Some(l)) = stderr.next_line() => log(format!("probe in {distro}: {l}")),
        }
    }
    let status = child.wait().await?;
    if !hello {
        log(format!(
            "probe in {distro} exited ({status}) before saying hello; is it installed? Run scripts/install-probe.sh in that distro"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf16_distro_list() {
        let raw: Vec<u8> = "Ubuntu\r\ndocker-desktop\r\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(decode_wsl_output(&raw), "Ubuntu\r\ndocker-desktop\r\n");
        assert_eq!(decode_wsl_output(b"Ubuntu\n"), "Ubuntu\n");
    }
}
