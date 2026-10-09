//! Time-lapse: re-run logged events through a scratch `Colony` and sample the
//! agents' states at even steps, for the map's scrubber.

use colony_core::state::AgentKind;
use colony_core::{Colony, Envelope};
use serde_json::{json, Value};

/// Most frames one replay returns.
pub const MAX_FRAMES: usize = 600;

/// `frames` samples of the main agents between `from` and `to`, as
/// `{ type: "replay", from, to, frames: [{ ts, agents: [[id, name, project, state]] }] }`.
/// The scratch colony starts empty at the first event, so sessions that were
/// already running before `from` show up once they next do something.
pub fn build(events: &[Envelope], from: u64, to: u64, frames: usize) -> Value {
    let frames = frames.clamp(2, MAX_FRAMES);
    let step = (to.saturating_sub(from) / (frames as u64 - 1)).max(1);
    let mut colony = Colony::new();
    let mut out: Vec<Value> = Vec::with_capacity(frames);
    let mut next = from;
    let sample = |colony: &mut Colony, ts: u64, out: &mut Vec<Value>| {
        colony.tick(ts);
        let agents: Vec<Value> = colony
            .agents
            .values()
            .filter(|a| a.kind == AgentKind::Main)
            .map(|a| json!([a.id, a.name, a.project_name, a.state]))
            .collect();
        out.push(json!({ "ts": ts, "agents": agents }));
    };
    for e in events {
        while next <= e.ts && next <= to && out.len() < frames {
            sample(&mut colony, next, &mut out);
            next += step;
        }
        colony.apply(e);
    }
    while next <= to && out.len() < frames {
        sample(&mut colony, next, &mut out);
        next += step;
    }
    json!({ "type": "replay", "from": from, "to": to, "frames": out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_core::{DomainEvent, HostId};

    fn ev(ts: u64, event: DomainEvent) -> Envelope {
        Envelope { ts, host: HostId::Windows, session_id: "s1".into(), cwd: Some("C:/x/proj".into()), event }
    }

    #[test]
    fn samples_state_at_even_steps() {
        let events = vec![
            ev(1_000, DomainEvent::SessionStarted { source: None, model: None }),
            ev(2_000, DomainEvent::PromptSubmitted { preview: "go".into(), synthetic: false, task_ended: false, full: None }),
            ev(5_000, DomainEvent::TurnEnded { last_message: Some("Done. All tests pass.".into()) }),
        ];
        let r = build(&events, 0, 6_000, 7);
        let frames = r["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 7);
        let state = |i: usize| frames[i]["agents"].as_array().unwrap().first().map(|a| a[3].as_str().unwrap().to_string());
        assert_eq!(state(0), None, "nothing exists before the first event");
        assert_eq!(state(3).as_deref(), Some("working"));
        assert_eq!(state(6).as_deref(), Some("ready_to_review"));
        assert_eq!(frames[3]["agents"][0][2], "proj");
    }
}
