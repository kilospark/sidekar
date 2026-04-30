//! Unified Exec: long-running PTY-backed shell sessions for the
//! agent's `ExecSession` tool.
//!
//! This module implements the plumbing behind a single tool that can
//! spawn a command in a PTY, yield control back to the agent after a
//! configurable time, and let subsequent tool calls interact with the
//! still-running process — polling for new output, writing stdin,
//! killing, or enumerating live sessions.
//!
//! Design doc: `context/unified-exec.md`. Read that first if you're
//! touching this module.
//!
//! Milestone progression (each commit corresponds to one):
//!   M1  Buffer + Process + single-shot spawn   <-- you are here
//!   M2  Yield semantics + Notify wiring
//!   M3  ProcessManager + store + kill + list
//!   M4  Tool schema + dispatcher
//!   M5  Tests + dogfood + docs
//!
//! Unix-only for v1. Windows support would require fixing a few PTY
//! abstractions (child.wait() semantics, signal delivery) and is out
//! of scope.

pub(crate) mod buffer;
pub(crate) mod manager;
pub(crate) mod process;

pub(crate) use manager::{ExecOutput, ProcessManager, SessionInfo};
pub(crate) use manager::{DEFAULT_POLL_YIELD_MS, DEFAULT_SPAWN_YIELD_MS, DEFAULT_WRITE_YIELD_MS};
pub(crate) use process::SpawnOptions;
