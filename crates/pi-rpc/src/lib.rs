//! Client and process manager for `pi --mode rpc`.
//!
//! Each draft owns one pi process, driven through [`Agent`]: JSONL commands
//! on stdin, replies and events on stdout. The process starts when the
//! draft is focused or first used, stops when idle or switched away from,
//! and a crash is reported as [`AgentEvent::Crashed`] with the interrupted
//! prompt so the chat can offer a retry. Session files hold all state, so a
//! new process continues where the old one stopped.
//!
//! The crate doesn't depend on gpui: requests return futures and events
//! arrive on a `futures` channel, both usable from gpui's executors.
//!
//! A draft's session is created once at handoff by [`fork`], which forks the
//! source pi session and blocks until pi has exited.

mod agent;
mod connection;
mod fork;
mod protocol;

pub use agent::{Agent, AgentEvent, CommandFactory, Error, LaunchSpec};
pub use fork::{ForkSpec, Forked, ModelOverride, fork};
pub use protocol::{AssistantDelta, Disposition, Event, Model, State, ThinkingLevel};
