//! A made-up fleet: N sessions across a few projects, each cycling through
//! prompt, tool calls, permission waits, subagents, and finished turns the way
//! real sessions do. Deterministic for a seed, so a visual or load test can be
//! repeated.

use colony_core::{DomainEvent, Envelope, HostId, Tokens};

const PROJECTS: &[&str] = &[
    "harbor-api", "atlas-web", "ledger", "nightly-etl", "docs-site", "mobile-app", "infra", "billing",
    "search", "auth-service", "dashboards", "ml-eval",
];
const TOOLS: &[(&str, &str)] = &[
    ("Read", "src/lib.rs"),
    ("Grep", "TODO"),
    ("Glob", "**/*.ts"),
    ("Edit", "src/main.rs"),
    ("Write", "notes.md"),
    ("Bash", "cargo test"),
    ("Bash", "npm run build"),
    ("WebFetch", "https://docs.example.com"),
];
const DIRS: &[&str] = &["src/auth", "src/api", "src/ui/components", "tests", "docs", "crates/core/src", "scripts"];
const FILES: &[&str] = &["index.ts", "main.rs", "lib.rs", "README.md", "util.ts", "config.ts", "types.ts", "routes.rs", "handler.rs", "schema.sql", "App.tsx", "mod.rs"];
const MODELS: &[&str] = &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];
/// Now and then a session runs a model with no known price, so the "partial" cost shows.
const UNPRICED_MODEL: &str = "claude-experimental-x";
/// Subagents run on this one.
const SUB_MODEL: &str = "claude-haiku-5-5";
const PROMPTS: &[&str] = &[
    "Add retry with backoff to the sync client",
    "Fix the flaky login test",
    "Migrate the settings page to the new form components",
    "Why is the nightly job slow? Profile it and fix the worst offender",
    "Write docs for the export endpoint",
    "Upgrade the HTTP library and fix what breaks",
];

/// xorshift64*: small, seedable, and good enough for scenery.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// In `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Start,
    Prompt,
    ToolStart,
    ToolEnd,
    /// Waiting for a (simulated) person to allow a tool.
    Permission,
    SubagentWork,
    TurnEnd,
    /// Between turns, until the (simulated) person sends the next prompt.
    Idle,
}

struct Session {
    id: String,
    host: HostId,
    cwd: String,
    model: &'static str,
    phase: Phase,
    next_at: u64,
    tools_left: u32,
    tool_n: u32,
    tool: (&'static str, &'static str),
    /// What the current tool call is aimed at: a file path under the project for file tools.
    target: String,
    /// The running subagent and how many tool calls it has left.
    sub: Option<(String, u32)>,
    turns_left: u32,
    /// Usage messages sent so far for the main agent and for the running subagent.
    usage_seq: u64,
    sub_usage_seq: u64,
}

pub struct Config {
    pub agents: usize,
    pub projects: usize,
    pub seed: u64,
    /// 1.0 is human pace; 10.0 is ten times faster.
    pub speed: f64,
}

pub struct Fleet {
    cfg: Config,
    rng: Rng,
    sessions: Vec<Session>,
    spawned: u64,
}

impl Fleet {
    pub fn new(cfg: Config, now: u64) -> Fleet {
        let mut f = Fleet { rng: Rng::new(cfg.seed), cfg, sessions: Vec::new(), spawned: 0 };
        for _ in 0..f.cfg.agents {
            // Spread the starts so the map fills in rather than appearing at once.
            let delay = f.scaled(0, 3_000);
            let s = f.new_session(now + delay);
            f.sessions.push(s);
        }
        f
    }

    /// One assistant message's worth of tokens: mostly cached context, a little new input and output.
    fn usage(&mut self, agent_id: Option<String>, seq: u64, model: &str) -> DomainEvent {
        let tokens = Tokens {
            input: self.rng.range(20, 3_000),
            output: self.rng.range(100, 2_500),
            cache_read: self.rng.range(5_000, 80_000),
            cache_creation: if self.rng.chance(30) { self.rng.range(500, 6_000) } else { 0 },
        };
        DomainEvent::UsageUpdated { agent_id, seq, model: Some(model.into()), tokens }
    }

    /// What the tool call is aimed at. File tools name a file in one of a few
    /// folders of the project, so sessions in a project share places and
    /// sometimes the same file, as real agents do.
    fn target_for(&mut self, s: &Session) -> String {
        if !matches!(s.tool.0, "Read" | "Edit" | "Write" | "Grep") {
            return s.tool.1.to_string();
        }
        let dir = *self.rng.pick(DIRS);
        let sep = if matches!(s.host, HostId::Windows) { '\\' } else { '/' };
        let mut p = format!("{}{sep}{}", s.cwd, dir.replace('/', &sep.to_string()));
        if s.tool.0 != "Grep" || self.rng.chance(50) {
            p.push(sep);
            p.push_str(self.rng.pick(FILES));
        }
        p
    }

    fn scaled(&mut self, lo_ms: u64, hi_ms: u64) -> u64 {
        (self.rng.range(lo_ms, hi_ms) as f64 / self.cfg.speed) as u64
    }

    fn new_session(&mut self, at: u64) -> Session {
        self.spawned += 1;
        let n = self.cfg.projects.clamp(1, PROJECTS.len()) as u64;
        let project = PROJECTS[(self.rng.next() % n) as usize];
        let (host, cwd) = if self.rng.chance(75) {
            (HostId::Windows, format!("C:\\synth\\{project}"))
        } else {
            (HostId::Wsl("Ubuntu".into()), format!("/home/synth/{project}"))
        };
        Session {
            id: format!("synth-{:x}-{:05}", self.cfg.seed, self.spawned),
            host,
            cwd,
            model: if self.rng.chance(4) { UNPRICED_MODEL } else { self.rng.pick(MODELS) },
            phase: Phase::Start,
            next_at: at,
            tools_left: 0,
            tool_n: 0,
            tool: TOOLS[0],
            target: String::new(),
            sub: None,
            turns_left: self.rng.range(1, 4) as u32,
            usage_seq: 0,
            sub_usage_seq: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Everything that is due by `now`, oldest first.
    pub fn tick(&mut self, now: u64) -> Vec<Envelope> {
        let mut out = Vec::new();
        for i in 0..self.sessions.len() {
            while self.sessions[i].next_at <= now {
                let at = self.sessions[i].next_at;
                if !self.step(i, at, &mut out) {
                    // Session over: a new one takes its place.
                    let delay = self.scaled(500, 5_000);
                    self.sessions[i] = self.new_session(at + delay);
                    break;
                }
            }
        }
        out.sort_by_key(|e| e.ts);
        out
    }

    /// Advance session `i` by one event. False once it has ended.
    fn step(&mut self, i: usize, at: u64, out: &mut Vec<Envelope>) -> bool {
        let mut s = std::mem::replace(&mut self.sessions[i], placeholder());
        let env = |s: &Session, ts: u64, event| Envelope {
            ts,
            host: s.host.clone(),
            session_id: s.id.clone(),
            cwd: Some(s.cwd.clone()),
            event,
        };
        let mut keep = true;
        match s.phase {
            Phase::Start => {
                out.push(env(&s, at, DomainEvent::SessionStarted { source: Some("startup".into()), model: Some(s.model.into()) }));
                s.phase = Phase::Prompt;
                s.next_at = at + self.scaled(300, 2_000);
            }
            Phase::Prompt => {
                let preview = self.rng.pick(PROMPTS).to_string();
                out.push(env(&s, at, DomainEvent::PromptSubmitted { preview, synthetic: false, task_ended: false }));
                s.tools_left = self.rng.range(3, 14) as u32;
                s.phase = Phase::ToolStart;
                s.next_at = at + self.scaled(400, 2_500);
            }
            Phase::ToolStart => {
                s.tool = *self.rng.pick(TOOLS);
                s.target = self.target_for(&s);
                s.tool_n += 1;
                // Edits and shell commands are what ask permission.
                if matches!(s.tool.0, "Edit" | "Write" | "Bash") && self.rng.chance(12) {
                    let ask = DomainEvent::PermissionRequested { agent_id: None, tool: s.tool.0.into(), target: Some(s.target.clone()) };
                    out.push(env(&s, at, ask));
                    s.phase = Phase::Permission;
                    s.next_at = at + self.scaled(8_000, 60_000);
                } else {
                    out.push(env(&s, at, tool_started(&s, None)));
                    s.phase = Phase::ToolEnd;
                    s.next_at = at + self.scaled(300, 6_000);
                }
            }
            Phase::Permission => {
                out.push(env(&s, at, tool_started(&s, None)));
                s.phase = Phase::ToolEnd;
                s.next_at = at + self.scaled(300, 6_000);
            }
            Phase::ToolEnd => {
                let ok = !self.rng.chance(6);
                out.push(env(&s, at, tool_finished(&s, None, ok)));
                s.usage_seq += 1;
                let (model, seq) = (s.model, s.usage_seq);
                out.push(env(&s, at, self.usage(None, seq, model)));
                s.tools_left = s.tools_left.saturating_sub(1);
                if s.sub.is_none() && s.tools_left > 1 && self.rng.chance(8) {
                    let sub = format!("sub{}", s.tool_n);
                    out.push(env(&s, at, DomainEvent::SubagentStarted { agent_id: sub.clone(), agent_type: Some("Explore".into()) }));
                    s.sub_usage_seq = 0;
                    s.sub = Some((sub, self.rng.range(2, 5) as u32));
                    s.phase = Phase::SubagentWork;
                    s.next_at = at + self.scaled(500, 2_000);
                } else if s.tools_left == 0 {
                    s.phase = Phase::TurnEnd;
                    s.next_at = at + self.scaled(300, 2_000);
                } else {
                    s.phase = Phase::ToolStart;
                    s.next_at = at + self.scaled(200, 3_000);
                }
            }
            Phase::SubagentWork => {
                let (sub, left) = s.sub.clone().expect("subagent phase has a subagent");
                if left == 0 {
                    let stop = DomainEvent::SubagentStopped { agent_id: sub, last_message: Some("Found the relevant files.".into()) };
                    out.push(env(&s, at, stop));
                    s.sub = None;
                    s.phase = Phase::ToolStart;
                    s.next_at = at + self.scaled(300, 2_000);
                } else {
                    let id = format!("{sub}-t{left}");
                    out.push(env(&s, at, tool_started(&s, Some((&sub, &id)))));
                    out.push(env(&s, at + 1, tool_finished(&s, Some((&sub, &id)), true)));
                    s.sub_usage_seq += 1;
                    let seq = s.sub_usage_seq;
                    out.push(env(&s, at + 1, self.usage(Some(sub.clone()), seq, SUB_MODEL)));
                    s.sub = Some((sub, left - 1));
                    s.next_at = at + self.scaled(400, 2_500);
                }
            }
            Phase::TurnEnd => {
                s.turns_left = s.turns_left.saturating_sub(1);
                let event = if self.rng.chance(4) {
                    DomainEvent::TurnFailed { error: "API error: overloaded".into() }
                } else if self.rng.chance(20) {
                    DomainEvent::TurnEnded { last_message: Some("Which database should I target for the migration?".into()) }
                } else {
                    DomainEvent::TurnEnded { last_message: Some("Done. The change is in place and the tests pass.".into()) }
                };
                out.push(env(&s, at, event));
                s.phase = Phase::Idle;
                s.next_at = at + self.scaled(5_000, 45_000);
            }
            Phase::Idle => {
                if s.turns_left == 0 {
                    out.push(env(&s, at, DomainEvent::SessionEnded));
                    keep = false;
                } else {
                    s.phase = Phase::Prompt;
                    s.next_at = at;
                }
            }
        }
        self.sessions[i] = s;
        keep
    }
}

fn placeholder() -> Session {
    Session {
        id: String::new(),
        host: HostId::Windows,
        cwd: String::new(),
        model: MODELS[0],
        phase: Phase::Idle,
        next_at: u64::MAX,
        tools_left: 0,
        tool_n: 0,
        tool: TOOLS[0],
        target: String::new(),
        sub: None,
        turns_left: 0,
        usage_seq: 0,
        sub_usage_seq: 0,
    }
}

fn tool_use_id(s: &Session, sub: Option<(&str, &str)>) -> String {
    sub.map(|(_, t)| t.to_string()).unwrap_or_else(|| format!("{}-t{}", s.id, s.tool_n))
}

fn tool_started(s: &Session, sub: Option<(&str, &str)>) -> DomainEvent {
    DomainEvent::ToolStarted {
        agent_id: sub.map(|(a, _)| a.to_string()),
        tool: s.tool.0.into(),
        target: Some(s.target.clone()),
        tool_use_id: Some(tool_use_id(s, sub)),
        background: false,
    }
}

fn tool_finished(s: &Session, sub: Option<(&str, &str)>, ok: bool) -> DomainEvent {
    DomainEvent::ToolFinished {
        agent_id: sub.map(|(a, _)| a.to_string()),
        tool: s.tool.0.into(),
        tool_use_id: Some(tool_use_id(s, sub)),
        ok,
        error: (!ok).then(|| "exit status 1".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_core::Colony;

    fn run(seed: u64, ms: u64) -> Vec<Envelope> {
        let mut f = Fleet::new(Config { agents: 20, projects: 4, seed, speed: 20.0 }, 0);
        (0..ms).step_by(50).flat_map(|t| f.tick(t)).collect()
    }

    #[test]
    fn same_seed_same_events() {
        assert_eq!(run(7, 60_000), run(7, 60_000));
        assert_ne!(run(7, 60_000), run(8, 60_000));
    }

    #[test]
    fn keeps_fleet_size_and_reaches_the_reducer() {
        let mut f = Fleet::new(Config { agents: 20, projects: 4, seed: 1, speed: 20.0 }, 0);
        let mut colony = Colony::new();
        for t in (0..120_000).step_by(50) {
            for e in f.tick(t) {
                colony.apply(&e);
            }
        }
        assert_eq!(f.len(), 20);
        let mains = colony.agents.values().filter(|a| a.parent_id.is_none()).count();
        assert!(mains >= 20, "{mains} main agents");
        let projects: std::collections::HashSet<_> = colony.agents.values().filter_map(|a| a.project_key.clone()).collect();
        assert!(projects.len() > 1 && projects.len() <= 8, "{} projects", projects.len());
    }

    #[test]
    fn fleet_spends_money_with_some_unpriced_models() {
        let mut f = Fleet::new(Config { agents: 40, projects: 4, seed: 3, speed: 20.0 }, 0);
        let mut colony = Colony::new();
        for t in (0..120_000).step_by(50) {
            for e in f.tick(t) {
                colony.apply(&e);
            }
        }
        let mains: Vec<_> = colony.agents.values().filter(|a| a.parent_id.is_none()).collect();
        assert!(mains.iter().map(|a| a.cost_usd).sum::<f64>() > 0.0);
        assert!(mains.iter().any(|a| a.cost_partial), "some session runs an unpriced model");
        assert!(colony.spend.values().all(|p| !p.buckets.is_empty()));
        let subs = colony.agents.values().filter(|a| a.parent_id.is_some() && a.tokens.total() > 0).count();
        assert!(subs > 0, "subagents spend too");
    }
}
