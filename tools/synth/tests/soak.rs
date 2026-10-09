//! Soak: a long synthetic run through the reducer must not grow without bound.
//!
//! Heap use is measured with a counting allocator (portable, unlike RSS), and
//! sampled at fixed event counts: the last of the run may not use meaningfully
//! more than the middle, which is what a leak looks like.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use colony_core::{Colony, DomainEvent};
use colony_synth::fleet::{Config, Fleet};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if new >= l.size() {
            LIVE.fetch_add(new - l.size(), Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(l.size() - new, Ordering::Relaxed);
        }
        System.realloc(p, l, new)
    }
}

#[global_allocator]
static A: Counting = Counting;

const HOUR: u64 = 3_600_000;

/// Runs the fleet for `events` events (usage included) in simulated time, ticking
/// the colony each simulated second, and returns heap samples taken every `events/9`.
fn soak(events: usize, agents: usize) -> (Vec<usize>, u64, Colony) {
    let start = 1_800_000_000_000u64;
    let mut fleet = Fleet::new(Config { agents, projects: 6, seed: 7, speed: 1.0 }, start);
    let mut colony = Colony::new();
    let (mut now, mut seen, mut next_sample) = (start, 0usize, events / 9);
    let mut samples = Vec::new();
    while seen < events {
        now += 1_000;
        for e in fleet.tick(now) {
            seen += 1;
            colony.apply(&e);
            if matches!(e.event, DomainEvent::UsageUpdated { .. }) {
                colony.take_spend_changes();
            }
        }
        colony.take_latency_changes();
        colony.tick(now);
        if seen >= next_sample {
            samples.push(LIVE.load(Ordering::Relaxed));
            next_sample += events / 9;
        }
    }
    (samples, now - start, colony)
}

fn check(events: usize, agents: usize, min_hours: u64) {
    let (s, elapsed, colony) = soak(events, agents);
    let hours = elapsed / HOUR;
    eprintln!("{events} events over {hours} simulated hours; heap samples (KB): {:?}", s.iter().map(|b| b / 1024).collect::<Vec<_>>());
    assert!(hours >= min_hours, "run too short to mean anything: {hours} h");
    // Finished agents are removed, so the map stays near the fleet size.
    assert!(colony.agents.len() < 400, "{} agents kept for a fleet of 50", colony.agents.len());
    // Ledgers are windowed: 48 h of 15 min buckets at most, per project.
    for p in colony.spend.values() {
        assert!(p.buckets.len() <= 48 * 4 + 2, "spend buckets: {}", p.buckets.len());
    }
    for p in colony.latency.values() {
        assert!(p.buckets.len() <= 48 * 4 + 2, "latency buckets: {}", p.buckets.len());
    }
    // Growth must level off once the first windows have filled.
    let (mid, last) = (s[5], *s.last().unwrap());
    assert!(last < mid + mid / 10 + 128 * 1024, "heap still growing: {} KB -> {} KB", mid / 1024, last / 1024);
}

#[test]
fn heap_levels_off_over_a_multi_hour_run() {
    check(100_000, 5, 5);
}

/// `cargo test -p colony-synth --release --test soak -- --ignored`
#[test]
#[ignore = "slow in debug builds"]
fn soak_one_million_events() {
    check(1_000_000, 5, 60);
}
