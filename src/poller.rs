//! Bus message poller — reads from the SQLite bus_queue and delivers
//! messages to the local agent via PTY write.

use crate::activity::{ActivitySnapshot, ActivityState, PTY_OUTPUT_BUSY_MS, PTY_SPINNER_BUSY_MS};
use crate::broker;
use crate::message::Envelope;
use crate::transport::{RelayHttp, Transport};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static POLLER_SHUTDOWN: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const CLEANUP_INTERVAL_POLLS: u32 = 120; // clean old messages every 60s (120 * 500ms)
const NUDGE_INTERVAL_POLLS: u32 = 120; // check nudges every 60s
const MAX_MESSAGE_AGE_SECS: u64 = 3600;
const NUDGE_SCHEDULE_SECS: [u64; 5] = [60, 120, 300, 600, 900];
const NUDGE_MAX: u32 = 5;
const USER_IDLE_BEFORE_INJECT: Duration = Duration::from_millis(1000);
const INJECT_CHECK_INTERVAL: Duration = Duration::from_millis(100);

pub struct UserInputState {
    last_user_input_at_ms: std::sync::atomic::AtomicU64,
    pending_line: Mutex<Vec<u8>>,
    /// Draft stashed when a bus message is injected while the user had partial input.
    stashed_draft: Mutex<Option<Vec<u8>>>,
    /// Set when the event loop should clear its local line buffer (after stash).
    line_tracking_reset: std::sync::atomic::AtomicBool,
    last_pty_output_at_ms: std::sync::atomic::AtomicU64,
    last_spinner_at_ms: std::sync::atomic::AtomicU64,
    alternate_screen: std::sync::atomic::AtomicBool,
    cursor_hidden: std::sync::atomic::AtomicBool,
    terminal_parse_tail: Mutex<Vec<u8>>,
}

pub struct PtyNotice {
    pub message: String,
    pub stashed_draft: Option<String>,
    pub ack: std::sync::mpsc::Sender<bool>,
}

/// Result of feeding stdin bytes while a draft recall is offered.
#[derive(Debug, PartialEq, Eq)]
pub enum DraftRecallInput {
    /// No stashed draft; handle input normally.
    NotApplicable,
    /// Accumulating a possible Up-arrow sequence across reads.
    Pending,
    /// Up arrow — restore the stashed draft (do not forward).
    Restore,
    /// Something else — forward these bytes to the child and drop the stash.
    Forward(Vec<u8>),
}

impl UserInputState {
    pub fn new() -> Self {
        Self {
            last_user_input_at_ms: std::sync::atomic::AtomicU64::new(0),
            pending_line: Mutex::new(Vec::new()),
            stashed_draft: Mutex::new(None),
            line_tracking_reset: std::sync::atomic::AtomicBool::new(false),
            last_pty_output_at_ms: std::sync::atomic::AtomicU64::new(0),
            last_spinner_at_ms: std::sync::atomic::AtomicU64::new(0),
            alternate_screen: std::sync::atomic::AtomicBool::new(false),
            cursor_hidden: std::sync::atomic::AtomicBool::new(false),
            terminal_parse_tail: Mutex::new(Vec::new()),
        }
    }

    pub fn mark_activity(&self) {
        self.last_user_input_at_ms
            .store(epoch_millis(), Ordering::Relaxed);
    }

    pub fn mark_pty_output(&self) {
        self.last_pty_output_at_ms
            .store(epoch_millis(), Ordering::Relaxed);
    }

    pub fn mark_pty_output_bytes(&self, bytes: &[u8]) {
        self.mark_pty_output();
        self.update_terminal_state(bytes);
    }

    pub fn sidecar_notice_allowed(&self) -> bool {
        !self.alternate_screen.load(Ordering::Relaxed)
            && !self.cursor_hidden.load(Ordering::Relaxed)
    }

    pub fn mark_spinner_activity(&self) {
        self.last_spinner_at_ms
            .store(epoch_millis(), Ordering::Relaxed);
    }

    pub fn is_agent_working(&self) -> bool {
        let now = epoch_millis();
        let output_at = self.last_pty_output_at_ms.load(Ordering::Relaxed);
        if output_at > 0 && now.saturating_sub(output_at) < PTY_OUTPUT_BUSY_MS {
            return true;
        }
        let spinner_at = self.last_spinner_at_ms.load(Ordering::Relaxed);
        spinner_at > 0 && now.saturating_sub(spinner_at) < PTY_SPINNER_BUSY_MS
    }

    pub fn current_activity_state(&self) -> ActivityState {
        if !self.is_idle() || self.has_pending_line() || self.has_stashed_draft() {
            ActivityState::UserTyping
        } else if self.is_agent_working() {
            ActivityState::AgentWorking
        } else {
            ActivityState::Idle
        }
    }

    pub fn publish_activity(&self, agent_name: &str) {
        crate::activity::publish(agent_name, self.current_activity_state());
    }

    fn update_terminal_state(&self, bytes: &[u8]) {
        let mut data = Vec::new();
        if let Ok(mut tail) = self.terminal_parse_tail.lock() {
            if !tail.is_empty() {
                data.extend_from_slice(&tail);
                tail.clear();
            }
        }
        data.extend_from_slice(bytes);

        let bytes = data.as_slice();
        let mut next_tail = Vec::new();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != 0x1b {
                i += 1;
                continue;
            }
            let esc_start = i;
            if i + 1 >= bytes.len() {
                next_tail.extend_from_slice(&bytes[esc_start..]);
                break;
            }
            if bytes[i + 1] != b'[' {
                i += 2;
                continue;
            }
            if i + 2 >= bytes.len() {
                next_tail.extend_from_slice(&bytes[esc_start..]);
                break;
            }
            i += 2;
            let private = if i < bytes.len() && bytes[i] == b'?' {
                i += 1;
                true
            } else {
                false
            };
            let params_start = i;
            while i < bytes.len() && matches!(bytes[i], b'0'..=b'9' | b';') {
                i += 1;
            }
            if i >= bytes.len() {
                next_tail.extend_from_slice(&bytes[esc_start..]);
                break;
            }
            let final_byte = bytes[i];
            let params = &bytes[params_start..i];
            if private && (final_byte == b'h' || final_byte == b'l') {
                let enabled = final_byte == b'h';
                for param in params.split(|b| *b == b';') {
                    match param {
                        b"47" | b"1047" | b"1049" => {
                            self.alternate_screen.store(enabled, Ordering::Relaxed);
                        }
                        b"25" => {
                            self.cursor_hidden
                                .store(final_byte == b'l', Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
            }
            i += 1;
        }
        if let Ok(mut tail) = self.terminal_parse_tail.lock() {
            *tail = next_tail;
        }
    }

    pub fn set_pending_line(&self, line: &[u8]) {
        if let Ok(mut pending) = self.pending_line.lock() {
            pending.clear();
            pending.extend_from_slice(line);
        }
    }

    pub fn clear_pending_line(&self) {
        if let Ok(mut pending) = self.pending_line.lock() {
            pending.clear();
        }
    }

    pub fn has_pending_line(&self) -> bool {
        self.pending_line
            .lock()
            .map(|pending| !pending.is_empty())
            .unwrap_or(false)
    }

    pub fn has_stashed_draft(&self) -> bool {
        self.stashed_draft
            .lock()
            .map(|draft| draft.is_some())
            .unwrap_or(false)
    }

    pub fn take_stashed_draft(&self) -> Option<Vec<u8>> {
        self.stashed_draft.lock().ok()?.take()
    }

    pub fn discard_stashed_draft(&self) {
        if let Ok(mut draft) = self.stashed_draft.lock() {
            draft.take();
        }
    }

    /// Move the current pending line into `stashed_draft` so bus inject can proceed.
    /// Returns true when a draft was stashed.
    pub fn stash_pending_line_for_inject(&self) -> bool {
        let mut pending = match self.pending_line.lock() {
            Ok(p) => p,
            Err(_) => return false,
        };
        if pending.is_empty() {
            return false;
        }
        let mut stashed = match self.stashed_draft.lock() {
            Ok(s) => s,
            Err(_) => return false,
        };
        *stashed = Some(std::mem::take(&mut *pending));
        self.line_tracking_reset.store(true, Ordering::Relaxed);
        true
    }

    pub fn take_line_tracking_reset(&self) -> bool {
        self.line_tracking_reset.swap(false, Ordering::Relaxed)
    }

    /// When a draft is stashed, treat Up arrow as "recall draft" instead of forwarding
    /// to the child (which would only show the agent's own history).
    pub fn draft_recall_from_input(
        chunk: &[u8],
        pending_esc: &mut Vec<u8>,
        input_state: &UserInputState,
    ) -> DraftRecallInput {
        if !input_state.has_stashed_draft() {
            pending_esc.clear();
            return DraftRecallInput::NotApplicable;
        }

        pending_esc.extend_from_slice(chunk);

        const UP_SEQS: &[&[u8]] = &[b"\x1b[A", b"\x1bOA"];
        if UP_SEQS.contains(&pending_esc.as_slice()) {
            pending_esc.clear();
            return DraftRecallInput::Restore;
        }

        let waiting_for_more = UP_SEQS
            .iter()
            .any(|seq| seq.starts_with(pending_esc.as_slice()) && pending_esc.len() < seq.len());
        if waiting_for_more {
            return DraftRecallInput::Pending;
        }

        input_state.discard_stashed_draft();
        DraftRecallInput::Forward(std::mem::take(pending_esc))
    }

    pub fn is_idle(&self) -> bool {
        let last = self.last_user_input_at_ms.load(Ordering::Relaxed);
        if last == 0 {
            return true;
        }
        epoch_millis().saturating_sub(last) >= USER_IDLE_BEFORE_INJECT.as_millis() as u64
    }

    /// Stash any in-progress draft and clear the child PTY input line before bus inject.
    fn prepare_pty_for_inject(&self, raw_fd: i32) -> Option<String> {
        if !self.has_pending_line() {
            return None;
        }
        if !self.stash_pending_line_for_inject() {
            return None;
        }
        if clear_pty_input_line(raw_fd).is_err() {
            return None;
        }
        self.stashed_draft
            .lock()
            .ok()
            .and_then(|draft| draft.as_ref().map(|d| format_draft_preview(d)))
    }
}

impl Default for UserInputState {
    fn default() -> Self {
        Self::new()
    }
}

/// Signal all background workers started by this module to stop.
pub fn shutdown_poller() {
    POLLER_SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Start the full PTY poller: an inbound thread that delivers bus messages
/// into the wrapped agent's PTY, plus the shared nudge+cleanup sweep.
pub fn start_poller(
    agent_name: String,
    pty_fd: Arc<OwnedFd>,
    input_state: Arc<UserInputState>,
    child_pid: i32,
    notice_tx: tokio::sync::mpsc::UnboundedSender<PtyNotice>,
) {
    POLLER_SHUTDOWN.store(false, Ordering::Relaxed);

    let inject_agent = agent_name.clone();
    std::thread::spawn(move || {
        loop {
            if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
            if let Ok(messages) = broker::list_queued_messages(&inject_agent) {
                for msg in messages {
                    let Ok(Some(msg)) = broker::claim_queued_message(msg.id, &inject_agent) else {
                        continue;
                    };
                    if let Some(ref envelope) = msg.envelope {
                        if !envelope.requires_reply() {
                            let _ = broker::dismiss_terminal_ack_request(&envelope.id);
                        }
                    }
                    if let Some(msg_id) = crate::message::nudge_msg_id_from_body(&msg.body) {
                        if !broker::outbound_nudgeable(&msg_id).unwrap_or(false) {
                            let _ = broker::delete_queued_message(msg.id);
                            continue;
                        }
                    }
                    let submit =
                        should_submit_queued_message(msg.submit_input, &msg.envelope, &msg.body);
                    if deliver_to_pty(
                        &pty_fd,
                        &input_state,
                        &notice_tx,
                        &msg.body,
                        submit,
                        child_pid,
                    ) {
                        let _ = broker::delete_queued_message(msg.id);
                    } else {
                        let _ = broker::release_queued_message(msg.id);
                    }
                }
            }
        }
    });

    start_nudger(agent_name);
}

/// Start the nudge + cleanup sweep for this agent. No PTY delivery — use this
/// from embeds like the REPL that handle inbound messages on their own path.
/// Stopped by `shutdown_poller`.
pub fn start_nudger(agent_name: String) {
    POLLER_SHUTDOWN.store(false, Ordering::Relaxed);

    std::thread::spawn(move || {
        let _ = broker::repair_answered_outbounds(&agent_name);
        let _ = broker::repair_dismiss_terminal_ack_outbounds(&agent_name);
        let mut cleanup_poll_count: u32 = 0;
        let mut nudge_poll_count: u32 = 0;

        loop {
            if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);

            cleanup_poll_count += 1;
            if cleanup_poll_count >= CLEANUP_INTERVAL_POLLS {
                cleanup_poll_count = 0;
                let _ = broker::cleanup_old_messages(MAX_MESSAGE_AGE_SECS);
            }

            nudge_poll_count += 1;
            if nudge_poll_count >= NUDGE_INTERVAL_POLLS {
                nudge_poll_count = 0;
                send_nudges(&agent_name);
            }
        }
    });
}

/// Send nudges for this agent's unanswered outbound requests.
fn send_nudges(agent_name: &str) {
    let _ = broker::repair_answered_outbounds(agent_name);
    let _ = broker::repair_dismiss_terminal_ack_outbounds(agent_name);

    let requests = match broker::outbound_for_sender(agent_name) {
        Ok(r) => r,
        Err(_) => return,
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for request in requests {
        let wait_secs = NUDGE_SCHEDULE_SECS
            .get(request.nudge_count as usize)
            .copied()
            .unwrap_or(*NUDGE_SCHEDULE_SECS.last().unwrap_or(&900));
        let last_event_at = request.last_nudged_at.unwrap_or(request.created_at);
        let elapsed_since_last_event = now.saturating_sub(last_event_at);

        if elapsed_since_last_event < wait_secs {
            continue;
        }

        // Check if we've hit max nudges
        if request.nudge_count >= NUDGE_MAX {
            continue;
        }

        if !broker::outbound_nudgeable(&request.msg_id).unwrap_or(false) {
            continue;
        }

        // Check if recipient is still alive
        if !is_recipient_alive(&request.recipient_name) {
            let _ = broker::delete_outbound_request(&request.msg_id);
            let _ = broker::clear_pending(&request.msg_id);
            continue;
        }

        if recipient_should_defer_nudge(&request.transport_name, &request.transport_target) {
            crate::broker::try_log_event(
                "debug",
                "poller",
                &format!(
                    "nudge deferred: recipient busy transport={} target={} msg_id={}",
                    request.transport_name, request.transport_target, request.msg_id,
                ),
                None,
            );
            continue;
        }

        // Claim the nudge slot before delivery so a concurrent record_reply cannot
        // produce ghost text after the pre-check.
        if !broker::try_increment_nudge_count(&request.msg_id, now).unwrap_or(false) {
            continue;
        }

        if !broker::outbound_nudgeable(&request.msg_id).unwrap_or(false) {
            let _ = broker::revert_nudge_claim(&request.msg_id, now);
            continue;
        }

        let nudge_msg = format!(
            "[sidekar] You have an unanswered request from {}. Reply using bus send or bus done with --reply-to={}",
            request.sender_label, request.msg_id
        );

        let delivery_result = match request.transport_name.as_str() {
            "broker" => match broker::enqueue_bus_message(
                &request.transport_target,
                "sidekar",
                &nudge_msg,
                true,
                None,
            ) {
                Ok(()) => Ok(crate::message::DeliveryResult::Delivered),
                Err(e) => Ok(crate::message::DeliveryResult::Failed(e.to_string())),
            },
            "relay_http" => RelayHttp.deliver(&request.transport_target, &nudge_msg, "sidekar"),
            _ => {
                let _ = broker::revert_nudge_claim(&request.msg_id, now);
                continue;
            }
        };

        let delivered = matches!(
            delivery_result,
            Ok(crate::message::DeliveryResult::Delivered | crate::message::DeliveryResult::Queued)
        );
        if !delivered {
            let _ = broker::revert_nudge_claim(&request.msg_id, now);
            continue;
        }

        crate::broker::try_log_event(
            "debug",
            "poller",
            &format!(
                "nudge delivered transport={} target={} msg_id={}",
                request.transport_name, request.transport_target, request.msg_id,
            ),
            None,
        );
    }
}

/// Check if the recipient agent is still registered and alive.
fn is_recipient_alive(recipient_name: &str) -> bool {
    let agent = match broker::find_agent(recipient_name, None) {
        Ok(Some(a)) => a,
        _ => return false,
    };

    if let Some(ref pane) = agent.id.pane
        && let Some(pid_str) = pane.strip_prefix("pty-")
        && let Ok(pid) = pid_str.parse::<i32>()
    {
        return unsafe { libc::kill(pid, 0) } == 0;
    }

    // If we can't determine PID, assume alive (could be a relay agent)
    true
}

fn recipient_should_defer_nudge(transport_name: &str, transport_target: &str) -> bool {
    match transport_name {
        "broker" => broker::get_agent_activity(transport_target)
            .ok()
            .flatten()
            .unwrap_or(ActivitySnapshot::unknown())
            .should_defer_nudge(),
        "relay_http" => crate::transport::relay_recipient_should_defer_nudge(transport_target),
        _ => false,
    }
}

fn should_submit_queued_message(
    submit_input: bool,
    envelope: &Option<Envelope>,
    body: &str,
) -> bool {
    submit_input
        || envelope.as_ref().is_some_and(|e| e.requires_reply())
        || body.contains("[reply with: sidekar bus send")
        || crate::message::nudge_msg_id_from_body(body).is_some()
}

/// Deliver one bus message. Returns true when the message can be acked/dequeued.
fn deliver_to_pty(
    fd: &OwnedFd,
    input_state: &UserInputState,
    notice_tx: &tokio::sync::mpsc::UnboundedSender<PtyNotice>,
    message: &str,
    submit_input: bool,
    child_pid: i32,
) -> bool {
    if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
        return false;
    }

    let raw_fd = fd.as_raw_fd();

    // Wait until terminal-facing delivery will not collide with user input or agent output.
    let mut waited = 0u32;
    while !input_state.is_idle()
        || input_state.is_agent_working()
        || (submit_input && !input_state.sidecar_notice_allowed())
    {
        if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
            return false;
        }
        waited += 1;
        if waited.is_multiple_of(50) {
            crate::broker::try_log_event(
                "debug",
                "poller",
                &format!(
                    "inject blocked: idle={} agent_working={} pending_line={} waited={}s msg_len={} submit={}",
                    input_state.is_idle(),
                    input_state.is_agent_working(),
                    input_state.has_pending_line(),
                    waited / 10,
                    message.len(),
                    submit_input,
                ),
                None,
            );
        }
        std::thread::sleep(INJECT_CHECK_INTERVAL);
    }

    // Stash + Ctrl+U when Sidekar tracked a partial line so inject cannot merge with it.
    let stashed_draft = if input_state.has_pending_line() {
        match input_state.prepare_pty_for_inject(raw_fd) {
            Some(preview) => Some(preview),
            None => {
                crate::broker::try_log_event(
                    "debug",
                    "poller",
                    "inject deferred: failed to stash/clear pending draft",
                    None,
                );
                return false;
            }
        }
    } else {
        None
    };

    if !submit_input {
        if !input_state.sidecar_notice_allowed() {
            let detail = serde_json::json!({
                "body": message,
                "delivery": "suppressed_tui_notice",
            })
            .to_string();
            crate::broker::try_log_event(
                "info",
                "inbox",
                "received; terminal notice suppressed while TUI owned screen",
                Some(&detail),
            );
            crate::broker::try_log_event(
                "debug",
                "poller",
                &format!(
                    "suppressed {}B side notice while TUI owned screen",
                    message.len()
                ),
                None,
            );
            return true;
        }
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        if notice_tx
            .send(PtyNotice {
                message: message.to_string(),
                stashed_draft,
                ack: ack_tx,
            })
            .is_err()
        {
            return false;
        }
        let delivered = ack_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false);
        if delivered {
            crate::broker::try_log_event(
                "debug",
                "poller",
                &format!("displayed {}B via PTY notice channel", message.len()),
                None,
            );
        }
        return delivered;
    }

    // User may have typed during stash/clear. Recheck only user-idle here:
    // our own Ctrl+U can produce PTY output and briefly mark the agent busy.
    if !input_state.is_idle() {
        return false;
    }

    if let Err(e) = write_all_raw(raw_fd, message.as_bytes()) {
        crate::broker::try_log_event(
            "error",
            "poller",
            &format!("inject write failed: {e}"),
            None,
        );
        return false;
    }
    std::thread::sleep(Duration::from_millis(150));
    if let Err(e) = write_all_raw(raw_fd, b"\r") {
        crate::broker::try_log_event(
            "error",
            "poller",
            &format!("inject CR write failed: {e}"),
            None,
        );
        return false;
    }
    unsafe { libc::kill(child_pid, libc::SIGWINCH) };

    crate::broker::try_log_event(
        "debug",
        "poller",
        &format!(
            "injected {}B + CR + SIGWINCH (stashed_draft={})",
            message.len(),
            stashed_draft.is_some(),
        ),
        None,
    );

    if let Some(preview) = stashed_draft {
        crate::tunnel::tunnel_println(&format!(
            "\x1b[33m[sidekar]\x1b[0m Draft saved: \"{preview}\" — ↑ to restore"
        ));
    }
    true
}

/// Clear the child readline/PTY input line (Ctrl+U) before bus inject.
fn clear_pty_input_line(raw_fd: i32) -> anyhow::Result<()> {
    write_all_raw(raw_fd, b"\x15")?;
    std::thread::sleep(Duration::from_millis(50));
    Ok(())
}

fn format_draft_preview(draft: &[u8]) -> String {
    let one_line = String::from_utf8_lossy(draft).replace(['\r', '\n'], " ");
    const MAX_CHARS: usize = 80;
    if one_line.chars().count() > MAX_CHARS {
        let truncated: String = one_line.chars().take(MAX_CHARS.saturating_sub(1)).collect();
        format!("{truncated}…")
    } else {
        one_line
    }
}

fn write_all_raw(fd: i32, mut buf: &[u8]) -> anyhow::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
        } else if n == 0 {
            anyhow::bail!("write returned 0");
        } else {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == std::io::ErrorKind::WouldBlock {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            anyhow::bail!("write failed: {err}");
        }
    }
    Ok(())
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stash_moves_pending_line_and_clears_tracking() {
        let state = UserInputState::new();
        state.set_pending_line(b"fix the bu");
        assert!(state.stash_pending_line_for_inject());
        assert!(!state.has_pending_line());
        assert!(state.has_stashed_draft());
        assert!(state.take_line_tracking_reset());
        assert!(!state.take_line_tracking_reset());
    }

    #[test]
    fn stash_noop_when_pending_empty() {
        let state = UserInputState::new();
        assert!(!state.stash_pending_line_for_inject());
        assert!(!state.has_stashed_draft());
    }

    #[test]
    fn take_stashed_draft_clears_stash() {
        let state = UserInputState::new();
        state.set_pending_line(b"hello");
        state.stash_pending_line_for_inject();
        assert_eq!(
            state.take_stashed_draft().as_deref(),
            Some(b"hello".as_ref())
        );
        assert!(!state.has_stashed_draft());
    }

    #[test]
    fn draft_recall_up_arrow_csi() {
        let state = UserInputState::new();
        state.set_pending_line(b"hello");
        state.stash_pending_line_for_inject();
        let mut pending = Vec::new();
        assert!(matches!(
            UserInputState::draft_recall_from_input(b"\x1b", &mut pending, &state),
            DraftRecallInput::Pending
        ));
        assert!(matches!(
            UserInputState::draft_recall_from_input(b"[A", &mut pending, &state),
            DraftRecallInput::Restore
        ));
    }

    #[test]
    fn draft_recall_other_key_forwards_and_drops_stash() {
        let state = UserInputState::new();
        state.set_pending_line(b"hello");
        state.stash_pending_line_for_inject();
        let mut pending = Vec::new();
        match UserInputState::draft_recall_from_input(b"x", &mut pending, &state) {
            DraftRecallInput::Forward(bytes) => assert_eq!(bytes, b"x"),
            other => panic!("expected Forward, got {other:?}"),
        }
        assert!(!state.has_stashed_draft());
    }

    #[test]
    fn is_idle_can_be_true_while_draft_pending() {
        let state = UserInputState::new();
        state.set_pending_line(b"half typed");
        assert!(state.is_idle());
        assert!(state.has_pending_line());
    }

    #[test]
    fn agent_working_heuristic_tracks_recent_output() {
        let state = UserInputState::new();
        assert!(!state.is_agent_working());
        state.mark_pty_output();
        assert!(state.is_agent_working());
    }

    #[test]
    fn terminal_state_blocks_sidecar_notice_in_alternate_screen() {
        let state = UserInputState::new();
        assert!(state.sidecar_notice_allowed());
        state.mark_pty_output_bytes(b"\x1b[?1049h");
        assert!(!state.sidecar_notice_allowed());
        state.mark_pty_output_bytes(b"\x1b[?1049l");
        assert!(state.sidecar_notice_allowed());
    }

    #[test]
    fn terminal_state_blocks_sidecar_notice_while_cursor_hidden() {
        let state = UserInputState::new();
        assert!(state.sidecar_notice_allowed());
        state.mark_pty_output_bytes(b"\x1b[?25l");
        assert!(!state.sidecar_notice_allowed());
        state.mark_pty_output_bytes(b"\x1b[?25h");
        assert!(state.sidecar_notice_allowed());
    }

    #[test]
    fn terminal_state_tracks_split_escape_sequences() {
        let state = UserInputState::new();
        state.mark_pty_output_bytes(b"\x1b[?");
        assert!(state.sidecar_notice_allowed());
        state.mark_pty_output_bytes(b"1049h");
        assert!(!state.sidecar_notice_allowed());
    }

    #[test]
    fn format_draft_preview_truncates_long_lines() {
        let long = "a".repeat(100);
        let preview = format_draft_preview(long.as_bytes());
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= 80);
    }

    #[test]
    fn should_submit_queued_message_prefers_envelope_reply_requirement() {
        use crate::message::{AgentId, Envelope};
        let fyi = Envelope::new_fyi(AgentId::new("a"), "b", "closed.");
        assert!(!should_submit_queued_message(
            false,
            &Some(fyi),
            "[fyi from a]: closed."
        ));
        let req = Envelope::new_request(AgentId::new("a"), "b", "ping");
        assert!(should_submit_queued_message(
            false,
            &Some(req),
            "[request from a]: ping"
        ));
        assert!(should_submit_queued_message(true, &None, "ping"));
        assert!(should_submit_queued_message(
            false,
            &None,
            "[request from a]: ping\n[reply with: sidekar bus send a \"ok\" --reply-to=abc]"
        ));
        assert!(should_submit_queued_message(
            false,
            &None,
            "[sidekar] You have an unanswered request from a. Reply using bus send or bus done with --reply-to=abc"
        ));
    }
}
