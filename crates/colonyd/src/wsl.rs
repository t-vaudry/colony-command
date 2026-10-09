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

/// A probe that lived this long counts as healthy: its next restart is immediate.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);
const BACKOFF_FIRST: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// After a probe exits having lived `lived`: the new consecutive-failure count
/// and how long to leave the distro alone. A probe that is not installed, or
/// that dies as it starts, would otherwise be relaunched (and logged) forever
/// at the rescan rate; this backs off 5 s, 10 s, ... up to 5 min.
pub(crate) fn backoff(failures: u32, lived: Duration) -> (u32, Duration) {
    if lived >= HEALTHY_AFTER {
        return (0, Duration::ZERO);
    }
    let delay = BACKOFF_FIRST.saturating_mul(1u32 << failures.min(10)).min(BACKOFF_MAX);
    (failures.saturating_add(1), delay)
}

pub async fn supervise(tx: mpsc::Sender<Envelope>, shared: Arc<Shared>) {
    let attached: Arc<Mutex<HashSet<String>>> = Arc::default();
    // Per distro: consecutive quick failures and the earliest next attempt.
    let cooling: Arc<Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>>> = Arc::default();
    loop {
        *shared.distros.write().await = list_distros(false).await;
        for distro in list_distros(true).await {
            if cooling.lock().unwrap().get(&distro).is_some_and(|(_, until)| std::time::Instant::now() < *until) {
                continue;
            }
            if !attached.lock().unwrap().insert(distro.clone()) {
                continue;
            }
            let (tx, attached, shared, cooling) = (tx.clone(), attached.clone(), shared.clone(), cooling.clone());
            tokio::spawn(async move {
                let started = std::time::Instant::now();
                if let Err(e) = run_probe(&distro, tx, shared).await {
                    log(format!("probe in {distro}: {e}"));
                }
                let mut cooling = cooling.lock().unwrap();
                let failures = cooling.get(&distro).map_or(0, |(f, _)| *f);
                let (failures, delay) = backoff(failures, started.elapsed());
                cooling.insert(distro.clone(), (failures, std::time::Instant::now() + delay));
                attached.lock().unwrap().remove(&distro);
            });
        }
        tokio::select! {
            _ = tokio::time::sleep(RESCAN) => {}
            // A session was just started in WSL; give the distro a moment to boot.
            // That is also news that the probe may be wanted again, so forgive past failures.
            _ = shared.wsl_wake.notified() => {
                cooling.lock().unwrap().clear();
                tokio::time::sleep(Duration::from_secs(2)).await
            }
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

/// What a probe sends besides events: relayed approval-hook requests.
#[derive(Debug)]
enum ProbeLine {
    Permission { id: u64, body: String },
    Cancel(u64),
    Event(Box<Envelope>),
}

fn parse_probe_line(l: &str) -> Result<ProbeLine, serde_json::Error> {
    if l.starts_with("{\"permission") {
        let v: serde_json::Value = serde_json::from_str(l)?;
        if let Some(p) = v.get("permission") {
            if let (Some(id), Some(body)) = (p.get("id").and_then(|i| i.as_u64()), p.get("body").and_then(|b| b.as_str())) {
                return Ok(ProbeLine::Permission { id, body: body.to_string() });
            }
        }
        if let Some(id) = v.get("permission_cancel").and_then(|i| i.as_u64()) {
            return Ok(ProbeLine::Cancel(id));
        }
    }
    serde_json::from_str(l).map(|e| ProbeLine::Event(Box::new(e)))
}

/// The line the probe expects back for request `id`; `None` is "no decision".
fn reply_line(id: u64, output: Option<String>) -> String {
    serde_json::json!({ "id": id, "output": output }).to_string()
}

async fn run_probe(distro: &str, tx: mpsc::Sender<Envelope>, shared: Arc<Shared>) -> std::io::Result<()> {
    let mut child = Command::new("wsl.exe")
        .args(["-d", distro, "-e", "sh", "-c", PROBE])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut stderr = BufReader::new(child.stderr.take().expect("piped")).lines();
    // Answers to approval requests go back down the probe's stdin. Closing it
    // (when this function ends) also tells the probe to exit.
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();
    let mut stdin = child.stdin.take().expect("piped");
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Some(line) = reply_rx.recv().await {
            if stdin.write_all(format!("{line}\n").as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    let mut held: std::collections::HashMap<u64, tokio::task::JoinHandle<()>> = Default::default();
    let result = async {
        let mut hello = false;
        loop {
            tokio::select! {
                line = lines.next_line() => match line? {
                    Some(l) if !hello => {
                        hello = true;
                        log(format!("probe attached in {distro}: {l}"));
                    }
                    Some(l) => match parse_probe_line(&l) {
                        Ok(ProbeLine::Event(e)) => if tx.send(*e).await.is_err() { return Ok(hello) },
                        Ok(ProbeLine::Permission { id, body }) => {
                            held.retain(|_, h| !h.is_finished());
                            let (shared, reply_tx, host) = (shared.clone(), reply_tx.clone(), colony_core::HostId::Wsl(distro.to_string()));
                            held.insert(id, tokio::spawn(async move {
                                let out = crate::api::hold_permission(&shared, host, &body).await;
                                let _ = reply_tx.send(reply_line(id, out));
                            }));
                        }
                        // The hook went away; dropping the task settles the request on the map.
                        Ok(ProbeLine::Cancel(id)) => if let Some(h) = held.remove(&id) { h.abort() },
                        Err(e) => log(format!("probe in {distro} sent a bad line ({e}): {l}")),
                    },
                    None => break,
                },
                Ok(Some(l)) = stderr.next_line() => log(format!("probe in {distro}: {l}")),
            }
        }
        Ok::<bool, std::io::Error>(hello)
    }
    .await;
    for (_, h) in held {
        h.abort();
    }
    drop(reply_tx);
    writer.abort();
    let hello = result?;
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

    #[test]
    fn parses_relayed_permission_lines() {
        let line = serde_json::json!({ "permission": { "id": 7, "body": "{\"a\":1}" } }).to_string();
        assert!(matches!(parse_probe_line(&line), Ok(ProbeLine::Permission { id: 7, body }) if body == "{\"a\":1}"));
        assert!(matches!(parse_probe_line("{\"permission_cancel\":7}"), Ok(ProbeLine::Cancel(7))));
    }

    #[test]
    fn events_still_parse_and_junk_is_rejected() {
        let env = Envelope { ts: 1, host: colony_core::HostId::Wsl("Ubuntu".into()), session_id: "s".into(), cwd: None, event: colony_core::DomainEvent::PermissionSettled { request_id: "r".into() } };
        let line = serde_json::to_string(&env).unwrap();
        assert!(matches!(parse_probe_line(&line), Ok(ProbeLine::Event(_))));
        assert!(parse_probe_line("{\"permission\":{\"id\":\"x\"}}").is_err());
        assert!(parse_probe_line("nonsense").is_err());
    }

    #[test]
    fn a_probe_that_keeps_dying_is_retried_less_and_less_often_then_forgiven() {
        let quick = Duration::from_secs(1);
        let mut failures = 0;
        let mut delays = Vec::new();
        for _ in 0..12 {
            let (f, d) = backoff(failures, quick);
            failures = f;
            delays.push(d.as_secs());
        }
        assert_eq!(&delays[..4], &[5, 10, 20, 40]);
        assert!(delays.windows(2).all(|w| w[0] <= w[1]), "never speeds up: {delays:?}");
        assert_eq!(*delays.last().unwrap(), 300, "capped at five minutes");
        // A probe that ran for a while was healthy: restart at once, forget the failures.
        assert_eq!(backoff(failures, Duration::from_secs(61)), (0, Duration::ZERO));
        // Counting never overflows however long a distro stays broken.
        assert_eq!(backoff(u32::MAX, quick).1, Duration::from_secs(300));
    }

    #[test]
    fn a_probe_stream_that_is_cut_or_corrupted_never_panics() {
        let env = Envelope { ts: 1, host: colony_core::HostId::Wsl("Ubuntu".into()), session_id: "s".into(), cwd: None, event: colony_core::DomainEvent::SessionEnded };
        let good = serde_json::to_string(&env).unwrap();
        // The stream dies mid-line: every prefix is either an event or an error.
        for cut in 0..good.len() {
            if good.is_char_boundary(cut) {
                let _ = parse_probe_line(&good[..cut]);
            }
        }
        for junk in ["", " ", "{", "{\"permission", "{\"permission\":null}", "{\"permission_cancel\":-1}", "{\"permission_cancel\":\"x\"}", "\u{0}\u{1}", "null", "[]", "{\"ts\":\"x\"}"] {
            assert!(parse_probe_line(junk).is_err(), "{junk:?}");
        }
        // A newer probe sending fields we do not know about still gets through.
        let mut v: serde_json::Value = serde_json::from_str(&good).unwrap();
        v["newField"] = serde_json::json!({"a": [1, 2]});
        assert!(matches!(parse_probe_line(&v.to_string()), Ok(ProbeLine::Event(_))));
        // A multi-megabyte line is just an error, not a problem.
        assert!(parse_probe_line(&"x".repeat(5_000_000)).is_err());
    }

    #[test]
    fn replies_carry_the_id_and_null_for_no_decision() {
        let v: serde_json::Value = serde_json::from_str(&reply_line(3, None)).unwrap();
        assert_eq!(v, serde_json::json!({ "id": 3, "output": null }));
        let v: serde_json::Value = serde_json::from_str(&reply_line(4, Some("x".into()))).unwrap();
        assert_eq!(v["output"], "x");
    }
}
