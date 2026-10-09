//! Token usage from Claude Code's transcripts.
//!
//! Each session writes `<claude home>/projects/<encoded folder>/<session id>.jsonl`;
//! subagents write `<session id>/subagents/agent-<id>.jsonl` beside it (older
//! releases put their lines in the session file, marked `isSidechain`). Every
//! assistant message carries a `usage` block. Colony reads the new bytes of
//! each file on every pass and emits one `UsageUpdated` per message.
//!
//! The format is undocumented, so everything is read leniently: unknown fields
//! are ignored, missing ones count as zero, and a line that doesn't parse is
//! skipped. A transcript grows while it is read, so only complete lines
//! (ending in a newline) are consumed; a partial last line is picked up on the
//! next pass. Colony only extracts numbers; it never stores transcript text.
//!
//! The same code runs for Windows (`%USERPROFILE%\.claude`) and, inside each
//! distro, in colony-probe (`~/.claude`), whose events reach colonyd with the
//! rest.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use colony_core::{DomainEvent, Envelope, HostId, Tokens};
use serde_json::Value;

/// Passes (polls) between looks at the transcripts: about a second.
const EVERY: u64 = 5;
/// Most bytes read from one file in one pass; the rest follows next pass.
const READ_CAP: u64 = 64 * 1024 * 1024;

struct FileState {
    offset: u64,
    /// Session and subagent this file belongs to, from its path.
    session: String,
    agent: Option<String>,
}

pub struct UsageReader {
    host: HostId,
    projects: PathBuf,
    /// Transcripts last written before this (unix ms) are not read.
    since_ms: u64,
    passes: u64,
    files: HashMap<PathBuf, FileState>,
    /// Line uuids already counted (the same line can be in two files, and
    /// resumed sessions copy history).
    seen: HashSet<String>,
    /// One API response is written as several lines (one per content block)
    /// sharing a message id, each with the usage so far. Keep the most seen,
    /// and count only the growth.
    by_message: HashMap<String, Tokens>,
    /// Usage messages emitted so far per (session, agent), for `seq`.
    seq: HashMap<(String, Option<String>), u64>,
}

impl UsageReader {
    pub fn new(host: HostId, claude_home: &Path, since_ms: u64) -> Self {
        UsageReader {
            host,
            projects: claude_home.join("projects"),
            since_ms,
            passes: 0,
            files: HashMap::new(),
            seen: HashSet::new(),
            by_message: HashMap::new(),
            seq: HashMap::new(),
        }
    }

    /// New usage since the last pass. Cheap to call often: it only looks
    /// every few calls.
    pub fn poll(&mut self) -> Vec<Envelope> {
        self.passes += 1;
        if (self.passes - 1) % EVERY != 0 {
            return Vec::new();
        }
        self.scan()
    }

    /// Look at every transcript now.
    pub fn scan(&mut self) -> Vec<Envelope> {
        let mut out = Vec::new();
        for path in self.transcripts() {
            self.read_new(&path, &mut out);
        }
        out
    }

    /// Transcript files that may have something new, oldest write first.
    fn transcripts(&self) -> Vec<PathBuf> {
        let mut found: Vec<(u64, PathBuf)> = Vec::new();
        let mut consider = |path: PathBuf| {
            if path.extension().is_none_or(|e| e != "jsonl") {
                return;
            }
            let Ok(meta) = fs::metadata(&path) else { return };
            let modified = meta.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
            let known = self.files.get(&path);
            if known.is_some_and(|f| f.offset == meta.len()) || (known.is_none() && modified < self.since_ms) {
                return;
            }
            found.push((modified, path));
        };
        for project in read_dir_paths(&self.projects) {
            for entry in read_dir_paths(&project) {
                if entry.is_dir() {
                    // <session id>/subagents/agent-*.jsonl
                    for sub in read_dir_paths(&entry.join("subagents")) {
                        consider(sub);
                    }
                } else {
                    consider(entry);
                }
            }
        }
        found.sort();
        found.into_iter().map(|(_, p)| p).collect()
    }

    fn read_new(&mut self, path: &Path, out: &mut Vec<Envelope>) {
        let state = self.files.entry(path.to_path_buf()).or_insert_with(|| {
            let (session, agent) = identify(path);
            FileState { offset: 0, session, agent }
        });
        let Ok(mut file) = File::open(path) else { return };
        let Ok(len) = file.metadata().map(|m| m.len()) else { return };
        // Rewritten shorter (truncated): start over; uuids already seen are skipped.
        if len < state.offset {
            state.offset = 0;
        }
        if len == state.offset || file.seek(SeekFrom::Start(state.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if file.take(READ_CAP).read_to_end(&mut buf).is_err() {
            return;
        }
        // Only whole lines; a partial last line waits for its newline.
        let Some(end) = buf.iter().rposition(|&b| b == b'\n').map(|i| i + 1) else { return };
        state.offset += end as u64;
        let (file_session, file_agent) = (state.session.clone(), state.agent.clone());
        for line in buf[..end].split(|&b| b == b'\n') {
            if let Some(e) = self.read_line(line, &file_session, &file_agent) {
                out.push(e);
            }
        }
    }

    fn read_line(&mut self, line: &[u8], file_session: &str, file_agent: &Option<String>) -> Option<Envelope> {
        // Most lines are not assistant messages; skip them without parsing.
        if !contains(line, b"\"usage\"") {
            return None;
        }
        let u = parse_line(line)?;
        if let Some(id) = &u.uuid {
            if !self.seen.insert(id.clone()) {
                return None;
            }
        }
        let mut tokens = u.tokens;
        if let Some(key) = &u.message_key {
            let before = self.by_message.entry(key.clone()).or_default();
            let grown = Tokens {
                input: u.tokens.input.saturating_sub(before.input),
                output: u.tokens.output.saturating_sub(before.output),
                cache_read: u.tokens.cache_read.saturating_sub(before.cache_read),
                cache_creation: u.tokens.cache_creation.saturating_sub(before.cache_creation),
            };
            before.input = before.input.max(u.tokens.input);
            before.output = before.output.max(u.tokens.output);
            before.cache_read = before.cache_read.max(u.tokens.cache_read);
            before.cache_creation = before.cache_creation.max(u.tokens.cache_creation);
            tokens = grown;
        }
        if tokens.is_zero() {
            return None;
        }
        let session = u.session.unwrap_or_else(|| file_session.to_string());
        let agent = file_agent.clone().or_else(|| if u.sidechain { u.agent } else { None });
        let seq = self.seq.entry((session.clone(), agent.clone())).or_insert(0);
        *seq += 1;
        Some(Envelope {
            ts: u.ts.unwrap_or_else(crate::now_ms),
            host: self.host.clone(),
            session_id: session,
            cwd: u.cwd,
            event: DomainEvent::UsageUpdated { agent_id: agent, seq: *seq, model: u.model, tokens },
        })
    }
}

fn read_dir_paths(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Session and subagent from a transcript's path.
fn identify(path: &Path) -> (String, Option<String>) {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match stem.strip_prefix("agent-") {
        Some(agent) if path.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "subagents") => {
            let session = path
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (session, Some(agent.to_string()))
        }
        _ => (stem, None),
    }
}

#[derive(Debug, Default)]
struct LineUsage {
    uuid: Option<String>,
    /// `<message id>/<request id>`: one API response.
    message_key: Option<String>,
    session: Option<String>,
    agent: Option<String>,
    sidechain: bool,
    cwd: Option<String>,
    model: Option<String>,
    ts: Option<u64>,
    tokens: Tokens,
}

/// The usage in one transcript line, if it is an assistant message with any.
fn parse_line(line: &[u8]) -> Option<LineUsage> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let msg = v.get("message")?;
    let is_assistant = v.get("type").and_then(Value::as_str) == Some("assistant")
        || msg.get("role").and_then(Value::as_str) == Some("assistant");
    if !is_assistant {
        return None;
    }
    let usage = msg.get("usage")?;
    let model = msg.get("model").and_then(Value::as_str).filter(|m| !m.is_empty() && !m.starts_with('<')).map(str::to_string);
    let str_of = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let cache_creation = count(usage, "cache_creation_input_tokens").unwrap_or_else(|| {
        // Newer releases split writes by lifetime.
        usage
            .get("cache_creation")
            .and_then(Value::as_object)
            .map_or(0, |o| o.values().filter_map(as_count).sum())
    });
    let message_key = msg.get("id").and_then(Value::as_str).map(|id| format!("{id}/{}", str_of("requestId").unwrap_or_default()));
    Some(LineUsage {
        uuid: str_of("uuid"),
        message_key,
        session: str_of("sessionId"),
        agent: str_of("agentId"),
        sidechain: v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false),
        cwd: str_of("cwd"),
        model,
        ts: str_of("timestamp").and_then(|t| parse_rfc3339_ms(&t)),
        tokens: Tokens {
            input: count(usage, "input_tokens").unwrap_or(0),
            output: count(usage, "output_tokens").unwrap_or(0),
            cache_read: count(usage, "cache_read_input_tokens").unwrap_or(0),
            cache_creation,
        },
    })
}

fn as_count(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
}

fn count(usage: &Value, key: &str) -> Option<u64> {
    usage.get(key).and_then(as_count)
}

/// `2026-10-09T14:03:22.123Z` (or with a `+hh:mm` offset) to unix ms.
pub fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once(['T', 't', ' '])?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let split = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (clock, zone) = rest.split_at(split);
    let mut c = clock.split(':');
    let (h, min): (i64, i64) = (c.next()?.parse().ok()?, c.next()?.parse().ok()?);
    let secs: f64 = c.next().map_or(Some(0.0), |x| x.parse().ok())?;
    let offset_min = match zone.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let (zh, zm) = zone[1..].split_once(':').unwrap_or((&zone[1..], "0"));
            let m = zh.parse::<i64>().ok()? * 60 + zm.parse::<i64>().ok()?;
            if sign == '+' {
                m
            } else {
                -m
            }
        }
        _ => 0,
    };
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms = ((days * 24 + h) * 60 + min - offset_min) * 60_000 + (secs * 1000.0).round() as i64;
    u64::try_from(ms).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const MAIN: &str = include_str!("../tests/fixtures/transcript-main.jsonl");
    const SUB: &str = include_str!("../tests/fixtures/transcript-subagent.jsonl");
    const SID: &str = "11111111-aaaa-4aaa-8aaa-000000000001";

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("colony-usage-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("projects/C--code-api")).unwrap();
        d
    }

    fn reader(d: &Path) -> UsageReader {
        UsageReader::new(HostId::Windows, d, 0)
    }

    fn usage_of(e: &Envelope) -> (Option<String>, u64, Option<String>, Tokens) {
        match &e.event {
            DomainEvent::UsageUpdated { agent_id, seq, model, tokens } => (agent_id.clone(), *seq, model.clone(), *tokens),
            other => panic!("not usage: {other:?}"),
        }
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(parse_rfc3339_ms("2026-10-09T14:03:22.500Z"), Some(1_791_554_602_500));
        assert_eq!(parse_rfc3339_ms("2026-10-09T10:03:22.500-04:00"), Some(1_791_554_602_500));
        assert_eq!(parse_rfc3339_ms("not a time"), None);
    }

    #[test]
    fn reads_usage_dedupes_and_ignores_the_rest() {
        let d = tmp("main");
        fs::write(d.join(format!("projects/C--code-api/{SID}.jsonl")), MAIN).unwrap();
        let ev = reader(&d).scan();
        let got: Vec<_> = ev.iter().map(usage_of).collect();
        // Four usage lines from three responses: the repeated uuid, the synthetic message, the user
        // line, the garbage line and the usage-less line are all skipped.
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(ev.iter().all(|e| e.session_id == SID && e.cwd.as_deref() == Some("C:\\code\\api")));
        assert_eq!(got[0].2.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(got[0].3, Tokens { input: 12, output: 340, cache_read: 20_000, cache_creation: 1_500 });
        assert_eq!(ev[0].ts, 1_791_554_602_500);
        assert_eq!(got.iter().map(|g| g.1).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        // Nested cache_creation (split by lifetime) adds up.
        assert_eq!(got[1].3.cache_creation, 300 + 200);
        // A response written as two lines with growing usage counts once:
        // 50 output, then 90 in total -> 50 + 40.
        assert_eq!((got[2].3.output, got[3].3.output), (50, 40));
        assert_eq!(got[3].3.input, 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn subagent_files_report_under_their_parent_session() {
        let d = tmp("sub");
        let dir = d.join(format!("projects/C--code-api/{SID}/subagents"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("agent-a1b2.jsonl"), SUB).unwrap();
        let ev = reader(&d).scan();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].session_id, SID);
        assert_eq!(usage_of(&ev[0]).0.as_deref(), Some("a1b2"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sidechain_lines_in_the_session_file_are_the_subagents() {
        let d = tmp("side");
        let line = r#"{"type":"assistant","isSidechain":true,"agentId":"zz9","uuid":"u-side","sessionId":"S","message":{"model":"claude-haiku-5-5","usage":{"input_tokens":5,"output_tokens":6}}}"#;
        fs::write(d.join("projects/C--code-api/S.jsonl"), format!("{line}\n")).unwrap();
        let ev = reader(&d).scan();
        assert_eq!(usage_of(&ev[0]).0.as_deref(), Some("zz9"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_growing_file_is_read_incrementally_and_a_partial_last_line_waits() {
        let d = tmp("grow");
        let f = d.join("projects/C--code-api/S.jsonl");
        let line = |n: u32| {
            format!(r#"{{"type":"assistant","uuid":"u{n}","message":{{"id":"m{n}","model":"claude-opus-5-5","usage":{{"input_tokens":{n},"output_tokens":1}}}}}}"#)
        };
        fs::write(&f, format!("{}\n{}", line(1), &line(2)[..40])).unwrap();
        let mut r = reader(&d);
        assert_eq!(r.scan().len(), 1);
        assert!(r.scan().is_empty(), "unchanged file is not re-read");
        // The rest of the partial line, then a new one, arrive.
        let mut file = fs::OpenOptions::new().append(true).open(&f).unwrap();
        write!(file, "{}\n{}\n", &line(2)[40..], line(3)).unwrap();
        let ev = r.scan();
        let inputs: Vec<_> = ev.iter().map(|e| usage_of(e).3.input).collect();
        assert_eq!(inputs, vec![2, 3]);
        assert_eq!(usage_of(&ev[0]).1, 2, "seq continues across passes");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn restarting_the_reader_replays_with_the_same_seq() {
        let d = tmp("replay");
        fs::write(d.join(format!("projects/C--code-api/{SID}.jsonl")), MAIN).unwrap();
        let a: Vec<_> = reader(&d).scan().iter().map(|e| usage_of(e).1).collect();
        let b: Vec<_> = reader(&d).scan().iter().map(|e| usage_of(e).1).collect();
        assert_eq!(a, b);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn old_transcripts_are_skipped_and_missing_folders_are_fine() {
        let d = tmp("old");
        fs::write(d.join("projects/C--code-api/S.jsonl"), MAIN).unwrap();
        let mut r = UsageReader::new(HostId::Windows, &d, crate::now_ms() + 60_000);
        assert!(r.scan().is_empty());
        let mut none = UsageReader::new(HostId::Windows, &d.join("nope"), 0);
        assert!(none.scan().is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
