//! Client and process manager for Claude Code's stream-json mode.
//!
//! Each draft owns one `claude` process, driven through [`Agent`]: JSONL
//! user messages and control requests on stdin; conversation frames,
//! control responses and incoming callbacks on stdout. The process starts
//! when the draft is focused or first used and stops when idle or switched
//! away from. A crash is reported as [`AgentEvent::Crashed`] and the next
//! request starts a new process, which continues the session named in the
//! [`LaunchSpec`].
//!
//! The protocol follows Claude Code 2.1.293 as observed, with the official
//! Python Agent SDK's `query.py` as reference:
//!
//! - A run ends when the CLI reports `session_state_changed` `idle`, not at
//!   its `result`, and not when an interrupt is acknowledged.
//! - Permission and hook callbacks are answered off the stdout reader, so
//!   controls such as an interrupt are handled while one is pending. The
//!   CLI withdraws a pending callback with `control_cancel_request`.
//! - Closing stdin doesn't cancel a run; the CLI finishes it first.
//!
//! [`Oneshot`] runs one-off requests, such as selection actions, on a copy
//! of a draft's conversation.
//!
//! The crate doesn't depend on gpui: requests return futures and events
//! arrive on a `futures` channel, both usable from gpui's executors.

mod agent;
mod connection;
mod contract;
mod history;
mod images;
mod instructions;
mod launch;
mod oneshot;
mod policy;
mod protocol;
mod session;
pub mod transcript;

pub use agent::{Agent, AgentEvent, Config, Error, HookRequest, PermissionRequest};
pub use contract::{ClaudeAgent, DraftContext, Record, instructions_block};
pub use history::history;
pub use launch::{
    LaunchSpec, PreToolUseHook, Session, SettingSource, Tool, check_version, version_command,
};
pub use oneshot::{Oneshot, Reply};
pub use protocol::{Init, ModelInfo};
pub use session::{DraftSession, Missing, Snapshot};
