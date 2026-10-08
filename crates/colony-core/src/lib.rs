//! Colony Command domain core.
//!
//! Turns raw signals from Claude Code (hook payloads, the session registry in
//! `~/.claude/sessions/<pid>.json`) into domain events, and folds those events
//! into a [`Colony`]: the set of agents and their lifecycle states that the map
//! renders. Pure logic only, so the daemon, the WSL probe, and tests share it.

pub mod event;
pub mod hook;
pub mod names;
pub mod paths;
pub mod registry;
pub mod state;

pub use event::{DomainEvent, Envelope, HostId};
pub use hook::HookPayload;
pub use registry::SessionRecord;
pub use state::{Agent, AgentKind, AgentState, Colony};
