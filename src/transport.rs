//! Transport trait and implementations for message delivery.
//!
//! A transport is a way to deliver a message string to an agent.
//! The message model ([`crate::message`]) is transport-independent;
//! transports only care about getting bytes to a destination.

use crate::message::DeliveryResult;
use anyhow::Result;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// A mechanism for delivering messages to an agent.
pub trait Transport: Send + Sync {
    /// Deliver a pre-formatted message to `target`.
    ///
    /// What `target` means depends on the transport:
    /// - [`TmuxPaste`]: tmux pane display ID (e.g. `"0:0.1"`)
    /// - [`Broker`]: recipient agent name
    fn deliver(&self, target: &str, message: &str, from: &str) -> Result<DeliveryResult>;

    /// Transport name for logging.
    fn name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// Tmux paste transport
// ---------------------------------------------------------------------------

/// Delivers messages by pasting into a tmux pane.
pub struct TmuxPaste;

impl Transport for TmuxPaste {
    fn deliver(&self, target: &str, message: &str, _from: &str) -> Result<DeliveryResult> {
        match crate::ipc::send_to_pane(target, message) {
            Ok(()) => Ok(DeliveryResult::Delivered),
            Err(e) => Ok(DeliveryResult::Failed(e.to_string())),
        }
    }

    fn name(&self) -> &'static str {
        "tmux-paste"
    }
}

// ---------------------------------------------------------------------------
// Broker transport (SQLite queue)
// ---------------------------------------------------------------------------

/// Delivers messages by inserting into the SQLite bus_queue table.
/// The recipient's poller picks up and delivers the message.
pub struct Broker;

impl Transport for Broker {
    fn deliver(&self, target: &str, message: &str, from: &str) -> Result<DeliveryResult> {
        match crate::broker::enqueue_message(from, target, message) {
            Ok(()) => Ok(DeliveryResult::Delivered),
            Err(e) => Ok(DeliveryResult::Failed(e.to_string())),
        }
    }

    fn name(&self) -> &'static str {
        "broker"
    }
}
