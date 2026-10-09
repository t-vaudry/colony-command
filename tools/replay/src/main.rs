//! colony-replay: feed a recorded event log back into a test colonyd.
//!
//!     colony-synth --spawn --duration 120 --record fleet.jsonl
//!     colony-replay fleet.jsonl --spawn --speed 10
//!
//! The log is JSON lines, one event envelope per line, oldest first (what
//! `colony-synth --record` writes). Timestamps are shifted so the first event
//! lands now, unless `--keep-ts`.

use std::io::BufRead;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use colony_core::Envelope;

const USAGE: &str = "colony-replay <log.jsonl>: feed a recorded event log to a test colonyd

  --spawn            start an isolated colonyd (own COLONY_HOME and port) and stop it on exit
  --home <dir>       COLONY_HOME of the daemon (with --spawn: where to put the new one)
  --port <n>         override the daemon's port (with --spawn: the port to use)
  --colonyd <path>   colonyd binary for --spawn (default: next to this tool, else PATH)
  --speed <x>        1 = original pace, 10 = ten times faster, 0 = as fast as possible (default 1)
  --keep-ts          send the recorded timestamps as they are instead of shifting them to now
  --loop             start over when the log ends
";

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("colony-replay: {msg}");
    std::process::exit(1);
}

fn load(path: &PathBuf) -> Vec<Envelope> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| fail(format!("cannot open {}: {e}", path.display())));
    let mut events = Vec::new();
    for (n, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.unwrap_or_else(|e| fail(format!("{}: {e}", path.display())));
        if line.trim().is_empty() {
            continue;
        }
        let e: Envelope = serde_json::from_str(&line).unwrap_or_else(|e| fail(format!("{} line {}: {e}", path.display(), n + 1)));
        events.push(e);
    }
    // Recorded logs can interleave sources slightly out of order.
    events.sort_by_key(|e| e.ts);
    events
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut spawn, mut home, mut port, mut colonyd, mut log) = (false, None, None, None, None);
    let (mut speed, mut keep_ts, mut looping) = (1.0f64, false, false);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().unwrap_or_else(|| fail(format!("{name} needs a value")));
        match a.as_str() {
            "--spawn" => spawn = true,
            "--home" => home = Some(PathBuf::from(value("--home"))),
            "--port" => port = Some(value("--port").parse().unwrap_or_else(|_| fail("--port must be a number"))),
            "--colonyd" => colonyd = Some(PathBuf::from(value("--colonyd"))),
            "--speed" => speed = value("--speed").parse().ok().filter(|s: &f64| *s >= 0.0).unwrap_or_else(|| fail("--speed must be 0 or more")),
            "--keep-ts" => keep_ts = true,
            "--loop" => looping = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            s if s.starts_with("--") => fail(format!("unknown option {s}\n\n{USAGE}")),
            _ => log = Some(PathBuf::from(a)),
        }
    }
    let log = log.unwrap_or_else(|| fail(format!("no log file given\n\n{USAGE}")));
    let events = load(&log);
    if events.is_empty() {
        fail(format!("{} has no events", log.display()));
    }
    let target = colony_synth::connect(spawn, home, port, colonyd).unwrap_or_else(|e| fail(e));

    let first = events[0].ts;
    let span = events[events.len() - 1].ts - first;
    eprintln!("replaying {} events spanning {}s", events.len(), span / 1000);
    loop {
        let started = Instant::now();
        let base = colony_source::now_ms();
        let mut i = 0;
        while i < events.len() {
            // Everything due by the (scaled) clock goes out in one request.
            let elapsed = started.elapsed().as_millis() as f64 * if speed == 0.0 { f64::INFINITY } else { speed };
            let mut batch = Vec::new();
            while i < events.len() && ((events[i].ts - first) as f64) <= elapsed && batch.len() < 500 {
                let mut e = events[i].clone();
                if !keep_ts {
                    e.ts = base + ((e.ts - first) as f64 / if speed == 0.0 { 1.0 } else { speed }) as u64;
                }
                batch.push(e);
                i += 1;
            }
            if let Err(e) = target.client.ingest(&batch) {
                fail(e);
            }
            if batch.is_empty() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if !looping {
            break;
        }
    }
    eprintln!("replayed {} events", events.len());
}
