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

/// Longest free text kept from a message or error.
const KEEP_CHARS: usize = 300;

/// The last `KEEP_CHARS` characters: the end of a reply is where its question
/// and its "Done." are.
fn tail(s: &str) -> String {
    let n = s.chars().count();
    if n <= KEEP_CHARS {
        s.to_string()
    } else {
        s.chars().skip(n - KEEP_CHARS).collect()
    }
}

/// What replay needs of an event: its kind and the flags that move a session
/// between states. Prompts, tool targets and inputs (commands, file paths,
/// anything a secret could be in) are dropped, and free text is cut short.
fn redact(e: &Envelope) -> Envelope {
    let mut e = e.clone();
    match &mut e.event {
        DomainEvent::PromptSubmitted { preview, full, .. } => {
            preview.clear();
            *full = None;
        }
        DomainEvent::ToolStarted { target, .. } | DomainEvent::PermissionRequested { target, .. } => *target = None,
        DomainEvent::PermissionAsked { target, input, .. } => {
            *target = None;
            *input = None;
        }
        DomainEvent::ToolFinished { error, .. } => *error = error.as_deref().map(tail),
        DomainEvent::Notified { message, .. } => *message = message.as_deref().map(tail),
        DomainEvent::SubagentStopped { last_message, .. } | DomainEvent::TurnEnded { last_message } => *last_message = last_message.as_deref().map(tail),
        DomainEvent::TurnFailed { error } => *error = tail(error),
        _ => {}
    }
    e
}

/// How often a running daemon trims the file.
const TRIM_EVERY_MS: u64 = 60 * 60_000;
/// Or sooner, once this much has been written since the last trim.
const TRIM_AFTER_BYTES: u64 = 8 * 1024 * 1024;

pub struct EventLog {
    path: PathBuf,
    out: Option<BufWriter<std::fs::File>>,
    /// Events at or before this were already logged by an earlier run: the
    /// daemon replays recent captures at startup and must not log them twice.
    floor: u64,
    last_trim: u64,
    written: u64,
}

impl EventLog {
    /// Trim the file to the retention window and open it for appending. If
    /// that fails the daemon runs on without a log.
    pub fn open(path: &Path, now_ms: u64) -> EventLog {
        let floor = trim(path, now_ms);
        let mut log = EventLog { path: path.to_path_buf(), out: None, floor, last_trim: now_ms, written: 0 };
        log.reopen();
        log
    }

    fn reopen(&mut self) {
        self.out = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).ok().map(BufWriter::new);
    }

    pub fn record(&mut self, e: &Envelope) {
        if matches!(e.event, DomainEvent::UsageUpdated { .. }) || e.ts <= self.floor {
            return;
        }
        let Some(out) = self.out.as_mut() else { return };
        let Ok(mut line) = serde_json::to_vec(&redact(e)) else { return };
        line.push(b'\n');
        if out.write_all(&line).is_ok() {
            self.written += line.len() as u64;
        }
    }

    pub fn flush(&mut self) {
        if let Some(out) = self.out.as_mut() {
            let _ = out.flush();
        }
    }

    /// Trim the file again if it has been an hour, or a lot has been written,
    /// since the last time. A daemon that stays up for days would otherwise
    /// outgrow the window and the size cap.
    pub fn maybe_trim(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_trim) < TRIM_EVERY_MS && self.written < TRIM_AFTER_BYTES {
            return;
        }
        self.flush();
        self.out = None;
        trim(&self.path, now_ms);
        self.last_trim = now_ms;
        self.written = 0;
        self.reopen();
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
    fn prompts_commands_and_long_messages_are_not_kept_verbatim() {
        let p = tmp();
        let mut log = EventLog::open(&p, 1_000);
        log.record(&ev(2_000, DomainEvent::PromptSubmitted { preview: "use key sk-secret".into(), synthetic: false, task_ended: false, full: Some("use key sk-secret".into()) }));
        log.record(&ev(
            3_000,
            DomainEvent::ToolStarted { agent_id: None, tool: "Bash".into(), target: Some("curl -H 'Authorization: sk-secret'".into()), tool_use_id: None, background: false },
        ));
        let long = format!("{}Should I continue?", "x".repeat(2_000));
        log.record(&ev(4_000, DomainEvent::TurnEnded { last_message: Some(long) }));
        log.flush();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("sk-secret"), "{text}");
        assert!(text.contains("Bash"));
        let events = read_window(&p, 0, u64::MAX);
        let DomainEvent::TurnEnded { last_message: Some(m) } = &events[2].event else { panic!("{:?}", events[2]) };
        assert_eq!(m.chars().count(), KEEP_CHARS);
        assert!(m.ends_with("Should I continue?"));
    }

    #[test]
    fn a_running_log_is_trimmed_after_an_hour() {
        let p = tmp();
        let now = 10 * RETAIN_MS;
        let mut log = EventLog::open(&p, now - RETAIN_MS - 10_000);
        log.record(&ev(now - RETAIN_MS - 5_000, started()));
        log.record(&ev(now - 1_000, started()));
        log.flush();
        log.maybe_trim(now - RETAIN_MS);
        assert_eq!(read_window(&p, 0, u64::MAX).len(), 2, "too soon to trim");
        log.maybe_trim(now);
        assert_eq!(read_window(&p, 0, u64::MAX).len(), 1);
        // Still writable afterwards.
        log.record(&ev(now + 1_000, started()));
        log.flush();
        assert_eq!(read_window(&p, 0, u64::MAX).len(), 2);
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
