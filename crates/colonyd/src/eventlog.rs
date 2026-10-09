//! A record of the events the daemon applies, kept for time-lapse replay.
//!
//! One envelope per line, oldest first: the format `colony-replay` reads, so
//! a log can also be fed to a test daemon. Usage updates are left out (they
//! are most of the volume and change no lifecycle state).

use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use colony_core::{DomainEvent, Envelope};

/// Events older than this are dropped when the daemon starts.
pub const RETAIN_MS: u64 = 48 * 60 * 60_000;
/// If the file is still bigger than this after that, the oldest lines go too.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

pub fn path() -> PathBuf {
    colony_source::colony_home().join("events.jsonl")
}

pub struct EventLog {
    out: Option<BufWriter<std::fs::File>>,
    /// Events at or before this were already logged by an earlier run: the
    /// daemon replays recent captures at startup and must not log them twice.
    floor: u64,
}

impl EventLog {
    /// Trim the file to the retention window and open it for appending. If
    /// that fails the daemon runs on without a log.
    pub fn open(path: &Path, now_ms: u64) -> EventLog {
        let floor = trim(path, now_ms);
        let out = std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(BufWriter::new);
        EventLog { out, floor }
    }

    pub fn record(&mut self, e: &Envelope) {
        if matches!(e.event, DomainEvent::UsageUpdated { .. }) || e.ts <= self.floor {
            return;
        }
        let Some(out) = self.out.as_mut() else { return };
        if serde_json::to_writer(&mut *out, e).is_ok() {
            let _ = out.write_all(b"\n");
        }
    }

    pub fn flush(&mut self) {
        if let Some(out) = self.out.as_mut() {
            let _ = out.flush();
        }
    }
}

/// Drop old lines (and the oldest ones past the size cap), rewriting through a
/// temp file. Returns the newest timestamp left. Lines that do not parse are dropped.
fn trim(path: &Path, now_ms: u64) -> u64 {
    let Ok(file) = std::fs::File::open(path) else { return 0 };
    let oldest = now_ms.saturating_sub(RETAIN_MS);
    let mut kept: Vec<String> = Vec::new();
    let mut changed = false;
    let mut floor = 0;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        match serde_json::from_str::<Envelope>(&line) {
            Ok(e) if e.ts >= oldest => {
                floor = floor.max(e.ts);
                kept.push(line);
            }
            _ => changed = true,
        }
    }
    let mut bytes: u64 = kept.iter().map(|l| l.len() as u64 + 1).sum();
    let mut drop = 0;
    while bytes > MAX_BYTES && drop < kept.len() {
        bytes -= kept[drop].len() as u64 + 1;
        drop += 1;
        changed = true;
    }
    if changed {
        let mut body = String::new();
        for l in &kept[drop..] {
            body.push_str(l);
            body.push('\n');
        }
        let tmp = path.with_extension("jsonl.tmp");
        if std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    floor
}

/// The logged events with `from <= ts <= to`, oldest first.
pub fn read_window(path: &Path, from: u64, to: u64) -> Vec<Envelope> {
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let mut events: Vec<Envelope> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Envelope>(&l).ok())
        .filter(|e| e.ts >= from && e.ts <= to)
        .collect();
    // Sources interleave slightly out of order.
    events.sort_by_key(|e| e.ts);
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_core::HostId;

    fn ev(ts: u64, event: DomainEvent) -> Envelope {
        Envelope { ts, host: HostId::Windows, session_id: "s".into(), cwd: Some("C:/x/proj".into()), event }
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("colony-eventlog-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("events.jsonl")
    }

    fn started() -> DomainEvent {
        DomainEvent::SessionStarted { source: None, model: None }
    }

    #[test]
    fn records_events_and_reads_a_window() {
        let p = tmp();
        let now = 10 * RETAIN_MS;
        let mut log = EventLog::open(&p, now);
        log.record(&ev(now - 3_000, started()));
        log.record(&ev(now - 2_000, DomainEvent::TurnEnded { last_message: None }));
        log.record(&ev(now - 1_000, DomainEvent::TurnEnded { last_message: None }));
        log.flush();
        assert_eq!(read_window(&p, now - 2_500, now).len(), 2);
    }

    #[test]
    fn a_restart_does_not_log_replayed_events_twice_and_trims_old_ones() {
        let p = tmp();
        let now = 10 * RETAIN_MS;
        let mut log = EventLog::open(&p, now);
        log.record(&ev(now - RETAIN_MS - 5, started()));
        log.record(&ev(now - 2_000, started()));
        log.flush();
        drop(log);

        let mut again = EventLog::open(&p, now + 1_000);
        // The startup replay hands the same event back, then a new one arrives.
        again.record(&ev(now - 2_000, started()));
        again.record(&ev(now + 500, started()));
        again.flush();
        let kept = read_window(&p, 0, u64::MAX);
        assert_eq!(kept.iter().map(|e| e.ts).collect::<Vec<_>>(), vec![now - 2_000, now + 500]);
    }

    #[test]
    fn usage_updates_are_not_logged() {
        let p = tmp();
        let mut log = EventLog::open(&p, 1_000);
        log.record(&ev(2_000, DomainEvent::UsageUpdated { agent_id: None, seq: 1, model: None, tokens: Default::default() }));
        log.flush();
        assert!(read_window(&p, 0, u64::MAX).is_empty());
    }
}
