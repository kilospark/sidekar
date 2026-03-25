//! Inter-process communication — legacy module.
//!
//! IPC sockets and tmux paste have been replaced by SQLite-backed
//! message queuing (broker.rs + poller.rs). This module is retained
//! as a stub; all public APIs have been removed.
