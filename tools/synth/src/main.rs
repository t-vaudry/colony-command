//! colony-synth: send a synthetic fleet to a test colonyd.
//!
//!     colony-synth --spawn --agents 50          # start an isolated colonyd and feed it
//!     colony-synth --home /tmp/colony-test      # feed one already running with COLONY_INGEST=1
//!
//! Never talks to your real Colony: a daemon only takes events when started
//! with `COLONY_INGEST=1`, and `--spawn` gives it its own COLONY_HOME and port.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use colony_synth::fleet::{Config, Fleet};

const USAGE: &str = "colony-synth: feed a synthetic fleet to a test colonyd

  --spawn            start an isolated colonyd (own COLONY_HOME and port) and stop it on exit
  --home <dir>       COLONY_HOME of the daemon (with --spawn: where to put the new one)
  --port <n>         override the daemon's port (with --spawn: the port to use)
  --colonyd <path>   colonyd binary for --spawn (default: next to this tool, else PATH)
  --agents <n>       sessions alive at once (default 50)
  --projects <n>     projects they spread over, 1-12 (default 6)
  --speed <x>        1 = human pace, 10 = ten times faster (default 1)
  --duration <secs>  stop after this long (default: run until Ctrl+C)
  --seed <n>         same seed, same fleet (default 1)
  --record <file>    also write every event sent, as JSON lines, for colony-replay
";

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("colony-synth: {msg}");
    std::process::exit(1);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut spawn, mut home, mut port, mut colonyd, mut record) = (false, None, None, None, None);
    let mut cfg = Config { agents: 50, projects: 6, seed: 1, speed: 1.0 };
    let mut duration: Option<u64> = None;
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().unwrap_or_else(|| fail(format!("{name} needs a value")));
        match a.as_str() {
            "--spawn" => spawn = true,
            "--home" => home = Some(PathBuf::from(value("--home"))),
            "--port" => port = Some(value("--port").parse().unwrap_or_else(|_| fail("--port must be a number"))),
            "--colonyd" => colonyd = Some(PathBuf::from(value("--colonyd"))),
            "--agents" => cfg.agents = value("--agents").parse().unwrap_or_else(|_| fail("--agents must be a number")),
            "--projects" => cfg.projects = value("--projects").parse().unwrap_or_else(|_| fail("--projects must be a number")),
            "--speed" => cfg.speed = value("--speed").parse().ok().filter(|s: &f64| *s > 0.0).unwrap_or_else(|| fail("--speed must be above 0")),
            "--duration" => duration = Some(value("--duration").parse().unwrap_or_else(|_| fail("--duration must be seconds"))),
            "--seed" => cfg.seed = value("--seed").parse().unwrap_or_else(|_| fail("--seed must be a number")),
            "--record" => record = Some(PathBuf::from(value("--record"))),
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => fail(format!("unknown option {other}\n\n{USAGE}")),
        }
    }

    let target = colony_synth::connect(spawn, home, port, colonyd).unwrap_or_else(|e| fail(e));
    let mut log = record.map(|p| std::io::BufWriter::new(std::fs::File::create(&p).unwrap_or_else(|e| fail(format!("cannot write {}: {e}", p.display())))));

    let mut fleet = Fleet::new(cfg, colony_source::now_ms());
    eprintln!("sending {} synthetic sessions{}", fleet.len(), duration.map(|d| format!(" for {d}s")).unwrap_or_default());
    let started = Instant::now();
    let mut sent = 0u64;
    loop {
        if duration.is_some_and(|d| started.elapsed() >= Duration::from_secs(d)) {
            break;
        }
        let events = fleet.tick(colony_source::now_ms());
        if let Err(e) = target.client.ingest(&events) {
            fail(e);
        }
        if let Some(log) = log.as_mut() {
            for e in &events {
                writeln!(log, "{}", serde_json::to_string(e).expect("envelopes serialize")).unwrap_or_else(|e| fail(e));
            }
        }
        sent += events.len() as u64;
        std::thread::sleep(Duration::from_millis(50));
    }
    if let Some(log) = log.as_mut() {
        log.flush().ok();
    }
    eprintln!("sent {sent} events");
}
