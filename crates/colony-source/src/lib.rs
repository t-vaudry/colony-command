//! Watches one host's Claude Code state on disk and turns it into domain events:
//!
//! - `<claude home>/sessions/<pid>.json`: the live session registry. A new or
//!   rewritten file is a `SessionSeen`; a vanished file is a `SessionGone`.
//! - `<colony home>/capture/<nanotime>-<pid>.json`: one hook payload per file,
//!   written by `colony-hook` (or moved in from `spool/`, see `drain_spool`). Processed in name (= time) order.
//!
//!   A file whose process has died (it was killed and could not clean up)
//!   also counts as gone.
//!
//! - `<claude home>/projects/**/*.jsonl`: transcripts, read for token usage (see [`usage`]).
//!
//! Polling a couple of small directories every few hundred milliseconds is
//! cheap and avoids file-watcher edge cases (buffer overflows, network paths).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use colony_core::{DomainEvent, Envelope, HookPayload, HostId, SessionRecord};

pub mod process;
pub mod usage;

/// A capture file that still fails to parse after this long is skipped; before
/// that it may simply be half-written.
const PARTIAL_WRITE_GRACE_MS: u64 = 5_000;
/// Polls between checks that registered processes are still running.
const ALIVE_CHECK_EVERY: u64 = 10;
/// Transcripts last written longer ago than this are not read for usage: it
/// covers "today" in any time zone, plus slack.
const USAGE_WINDOW_MS: u64 = 48 * 60 * 60 * 1000;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Colony's own folder: `COLONY_HOME` if set (tests use a separate one),
/// otherwise `~/.colony`.
pub fn colony_home() -> PathBuf {
    std::env::var_os("COLONY_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".colony"))
}

/// `~` for the current user: `USERPROFILE` on Windows, `HOME` elsewhere.
pub fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

struct RecordFile {
    modified: Option<SystemTime>,
    len: u64,
    pid: u32,
    proc_start: Option<u64>,
    session_id: String,
}

pub struct DirSource {
    host: HostId,
    sessions_dir: PathBuf,
    capture_dir: PathBuf,
    records: HashMap<String, RecordFile>,
    /// Registry files whose process is dead, by modification stamp, so they
    /// are not re-read until they change.
    stale: HashMap<String, (Option<SystemTime>, u64)>,
    polls: u64,
    /// Name of the last capture file processed; later names are newer.
    last_capture: Option<String>,
    replay_since_ms: u64,
    usage: usage::UsageReader,
}

impl DirSource {
    /// `replay_since_ms`: capture files older than this are skipped on the
    /// first pass, so a restart rebuilds recent history without replaying
    /// everything ever captured.
    pub fn new(host: HostId, claude_home: &Path, colony_home: &Path, replay_since_ms: u64) -> Self {
        drain_spool(colony_home);
        let host_for_usage = host.clone();
        DirSource {
            host,
            sessions_dir: claude_home.join("sessions"),
            capture_dir: colony_home.join("capture"),
            records: HashMap::new(),
            stale: HashMap::new(),
            polls: 0,
            last_capture: None,
            usage: usage::UsageReader::new(host_for_usage, claude_home, now_ms().saturating_sub(USAGE_WINDOW_MS)),
            replay_since_ms,
        }
    }

    /// Defaults for the current user: `~/.claude` and `~/.colony`.
    pub fn for_current_user(host: HostId, replay_since_ms: u64) -> Self {
        let home = home_dir();
        Self::new(host, &home.join(".claude"), &colony_home(), replay_since_ms)
    }

    pub fn poll(&mut self) -> Vec<Envelope> {
        let mut out = self.poll_captures();
        out.extend(self.poll_registry());
        // Last, so a session's registry entry is known before its usage arrives.
        out.extend(self.usage.poll());
        out
    }

    fn poll_registry(&mut self) -> Vec<Envelope> {
        let mut out = Vec::new();
        let now = now_ms();
        let mut present = Vec::new();
        // Checking every known process each poll is wasteful; every couple of
        // seconds is plenty to notice one that died without cleaning up.
        let check_alive = self.polls % ALIVE_CHECK_EVERY == 0;
        self.polls += 1;
        for entry in fs::read_dir(&self.sessions_dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !SessionRecord::is_record_file(&name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let stamp = (meta.modified().ok(), meta.len());
            if let Some(known) = self.records.get(&name) {
                if (known.modified, known.len) == stamp {
                    if !check_alive || process::alive(known.pid, known.proc_start) {
                        present.push(name);
                    } else {
                        // Left behind by a process that died: gone below.
                        self.stale.insert(name, stamp);
                    }
                    continue;
                }
            }
            if self.stale.get(&name) == Some(&stamp) {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path()) else { continue };
            // A half-written file fails to parse; it is retried next poll.
            let Ok(mut record) = SessionRecord::parse(&text) else { continue };
            // Inside WSL the session's environment says whether Colony
            // started it. (On Windows, colonyd tags records itself.)
            if let Some(term) = process::colony_term(record.pid) {
                record.extra.insert("colonyTerm".into(), term.into());
            }
            let proc_start = record.proc_start();
            if !process::alive(record.pid, proc_start) {
                self.stale.insert(name, stamp);
                continue;
            }
            self.stale.remove(&name);
            present.push(name.clone());
            self.records.insert(
                name,
                RecordFile {
                    modified: stamp.0,
                    len: stamp.1,
                    pid: record.pid,
                    proc_start,
                    session_id: record.session_id.clone(),
                },
            );
            out.push(Envelope::from_record(self.host.clone(), now, record));
        }
        let gone: Vec<String> = self.records.keys().filter(|k| !present.contains(k)).cloned().collect();
        for name in gone {
            let r = self.records.remove(&name).expect("listed above");
            out.push(Envelope {
                ts: now,
                host: self.host.clone(),
                session_id: r.session_id,
                cwd: None,
                event: DomainEvent::SessionGone { pid: r.pid },
            });
        }
        out
    }

    fn poll_captures(&mut self) -> Vec<Envelope> {
        let mut names: Vec<String> = fs::read_dir(&self.capture_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .filter(|n| self.last_capture.as_ref().is_none_or(|last| n > last))
            .collect();
        names.sort();
        let now = now_ms();
        let mut out = Vec::new();
        for name in names {
            let ts = capture_time_ms(&name).unwrap_or(now);
            if self.last_capture.is_none() && ts < self.replay_since_ms {
                continue;
            }
            let parsed = fs::read_to_string(self.capture_dir.join(&name))
                .ok()
                .and_then(|text| HookPayload::parse(&text).ok());
            match parsed {
                Some(p) => {
                    if let Some(e) = Envelope::from_hook(self.host.clone(), ts, &p) {
                        out.push(e);
                    }
                }
                // Possibly still being written: stop here and retry next poll,
                // keeping events in order. Give up on it after the grace period.
                None if now.saturating_sub(ts) < PARTIAL_WRITE_GRACE_MS => break,
                None => {}
            }
            self.last_capture = Some(name);
        }
        out
    }
}

/// Moves payloads that colony-hook spooled while no daemon was reachable into
/// `capture/`. The names are already time-ordered, so they keep their place.
pub fn drain_spool(colony_home: &Path) {
    let spool = colony_home.join("spool");
    let capture = colony_home.join("capture");
    let Ok(entries) = fs::read_dir(&spool) else { return };
    let _ = fs::create_dir_all(&capture);
    for e in entries.flatten() {
        let name = e.file_name();
        if name.to_string_lossy().ends_with(".json") {
            let _ = fs::rename(e.path(), capture.join(name));
        }
    }
}

/// `1791490765344322000-560.json` -> 1791490765344 (ms).
fn capture_time_ms(name: &str) -> Option<u64> {
    let nanos: u128 = name.split(['-', '.']).next()?.parse().ok()?;
    Some((nanos / 1_000_000) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("colony-source-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("claude/sessions")).unwrap();
        fs::create_dir_all(d.join("colony/capture")).unwrap();
        d
    }

    #[test]
    fn capture_names_give_millisecond_times() {
        assert_eq!(capture_time_ms("1791490765344322000-560.json"), Some(1791490765344));
        assert_eq!(capture_time_ms("notes.json"), None);
    }

    #[test]
    fn registry_files_appear_change_and_vanish() {
        let d = tmp("registry");
        let mut src = DirSource::new(HostId::Windows, &d.join("claude"), &d.join("colony"), 0);
        // A live pid: this test process.
        let pid = std::process::id();
        let f = d.join(format!("claude/sessions/{pid}.json"));
        fs::write(&f, format!(r#"{{"pid":{pid},"sessionId":"s1","status":"busy"}}"#)).unwrap();
        fs::write(d.join(format!("claude/sessions/{pid}.abc.key")), "secret").unwrap();
        let ev = src.poll();
        assert_eq!(ev.len(), 1);
        assert!(matches!(&ev[0].event, DomainEvent::SessionSeen { record } if record.pid == pid));
        assert!(src.poll().is_empty(), "unchanged file is not re-emitted");
        fs::write(&f, format!(r#"{{"pid":{pid},"sessionId":"s1","status":"idle","name":"longer now"}}"#)).unwrap();
        assert_eq!(src.poll().len(), 1);
        fs::remove_file(&f).unwrap();
        let ev = src.poll();
        assert!(matches!(&ev[0].event, DomainEvent::SessionGone { pid: p } if *p == pid));
        assert_eq!(ev[0].session_id, "s1");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn files_left_by_dead_processes_are_not_sessions() {
        let d = tmp("stale");
        let mut src = DirSource::new(HostId::Windows, &d.join("claude"), &d.join("colony"), 0);
        let dead = u32::MAX - 7;
        fs::write(d.join(format!("claude/sessions/{dead}.json")), format!(r#"{{"pid":{dead},"sessionId":"gone"}}"#)).unwrap();
        assert!(src.poll().is_empty());
        assert!(src.poll().is_empty(), "not re-read while unchanged");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn captures_are_read_in_order_once() {
        let d = tmp("capture");
        let cap = d.join("colony/capture");
        let t = (now_ms() as u128) * 1_000_000;
        fs::write(cap.join(format!("{}-2.json", t + 2_000_000)), r#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();
        fs::write(cap.join(format!("{}-1.json", t)), r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#).unwrap();
        let mut src = DirSource::new(HostId::Windows, &d.join("claude"), &d.join("colony"), 0);
        let ev = src.poll();
        assert_eq!(ev.len(), 2);
        assert!(matches!(ev[0].event, DomainEvent::PromptSubmitted { .. }));
        assert!(matches!(ev[1].event, DomainEvent::TurnEnded { .. }));
        assert!(src.poll().is_empty());
        // A half-written newest file holds the line until it parses.
        fs::write(cap.join(format!("{}-3.json", t + 3_000_000)), r#"{"session_id":"s","hook_ev"#).unwrap();
        assert!(src.poll().is_empty());
        fs::write(cap.join(format!("{}-3.json", t + 3_000_000)), r#"{"session_id":"s","hook_event_name":"SessionEnd"}"#).unwrap();
        assert_eq!(src.poll().len(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn spooled_payloads_join_the_captures_when_a_source_starts() {
        let d = tmp("spool");
        let spool = d.join("colony/spool");
        fs::create_dir_all(&spool).unwrap();
        let t = (now_ms() as u128) * 1_000_000;
        fs::write(spool.join(format!("{t}-1.json")), r#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();
        fs::write(spool.join(format!("{t}-2.tmp")), "half").unwrap();
        let mut src = DirSource::new(HostId::Windows, &d.join("claude"), &d.join("colony"), 0);
        assert_eq!(src.poll().len(), 1);
        assert!(!spool.join(format!("{t}-1.json")).exists());
        assert!(spool.join(format!("{t}-2.tmp")).exists(), "temp files are left alone");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn replay_window_skips_old_captures() {
        let d = tmp("replay");
        let cap = d.join("colony/capture");
        fs::write(cap.join("1000000000000000000-1.json"), r#"{"session_id":"s","hook_event_name":"Stop"}"#).unwrap();
        let mut src = DirSource::new(HostId::Windows, &d.join("claude"), &d.join("colony"), now_ms() - 60_000);
        assert!(src.poll().is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
