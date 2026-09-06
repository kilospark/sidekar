//! PTY bus bridge — attaches to the daemon and writes delivered bus messages
//! into the wrapped agent's PTY.

use crate::activity::{ActivityState, PTY_OUTPUT_BUSY_MS, PTY_SPINNER_BUSY_MS};
use crate::input_mode::TerminalInputMode;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static POLLER_SHUTDOWN: AtomicBool = AtomicBool::new(false);

const USER_IDLE_BEFORE_INJECT: Duration = Duration::from_millis(5000);
/// How long the client waits on the socket before re-testing the input gate.
/// Also the rate at which a blocked batch picks up newly dispatched messages.
const BUS_READ_POLL: Duration = Duration::from_millis(200);
/// A message held back at least this long carries a written/delivered stamp.
const DELIVERY_STAMP_MIN_SECS: u64 = 60;
/// Ceiling on one coalesced paste. A larger backlog is split across turns.
const MAX_BATCH_BYTES: usize = 24 * 1024;
/// Spacing of the "inject blocked" line while a batch waits for the gate.
const BLOCK_LOG_INTERVAL_SECS: u64 = 5;
/// How long a working agent may hold off a batch before the paste goes in
/// anyway. Conditions that belong to the human at the keyboard are never
/// overridden; only the agent-is-busy veto expires here.
const INJECT_FORCE_AFTER_SECS: u64 = 300;
/// How long a detected question may defer injection before sidekar proceeds anyway.
const AWAITING_INPUT_MAX: Duration = Duration::from_secs(600);

pub struct UserInputState {
    last_user_input_at_ms: std::sync::atomic::AtomicU64,
    pending_line: Mutex<Vec<u8>>,
    /// Draft stashed when a bus message is injected while the user had partial input.
    stashed_draft: Mutex<Option<Vec<u8>>>,
    /// Set when the event loop should clear its local line buffer (after stash).
    line_tracking_reset: std::sync::atomic::AtomicBool,
    last_pty_output_at_ms: std::sync::atomic::AtomicU64,
    last_spinner_at_ms: std::sync::atomic::AtomicU64,
    /// Terminal modes the agent announced on its own output stream.
    input_mode: TerminalInputMode,
    /// When the agent's screen started showing an unanswered question (0 = none).
    awaiting_since_ms: std::sync::atomic::AtomicU64,
    /// The detector's account of the current question, for `bus explain`.
    awaiting_reason: Mutex<Option<String>>,
}

/// One bus message dispatched by the daemon and not yet pasted into the pane.
struct PendingMessage {
    id: i64,
    body: String,
    created_at: u64,
    interrupt: bool,
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
            input_mode: TerminalInputMode::new(),
            awaiting_since_ms: std::sync::atomic::AtomicU64::new(0),
            awaiting_reason: Mutex::new(None),
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
        !self.input_mode.alternate_screen() && !self.input_mode.cursor_hidden()
    }

    /// Terminal modes observed on the agent's output stream.
    pub fn input_mode(&self) -> &TerminalInputMode {
        &self.input_mode
    }

    pub fn mark_spinner_activity(&self) {
        self.last_spinner_at_ms
            .store(epoch_millis(), Ordering::Relaxed);
    }

    /// Stop treating the agent as working.
    ///
    /// Line-based spinner detection can only ever say "still going"; it never
    /// sees the frame that would say "finished", so the clock relies on
    /// [`PTY_SPINNER_BUSY_MS`] expiring. An agent that declares the end of its
    /// turn over OSC (see `pty::osc_state`) is more definite than that timeout,
    /// so it clears the clock outright.
    pub fn clear_spinner_activity(&self) {
        self.last_spinner_at_ms.store(0, Ordering::Relaxed);
    }

    /// Record whether the agent's screen currently shows an unanswered question.
    ///
    /// `reason` is the detector's account of why, kept so `bus explain` can
    /// report which rule fired rather than only that something did.
    pub fn set_awaiting_user_input_because(&self, reason: Option<String>) {
        let waiting = reason.is_some();
        *self
            .awaiting_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = reason;
        if !waiting {
            self.awaiting_since_ms.store(0, Ordering::Relaxed);
            return;
        }
        // Keep the original timestamp so a redrawn prompt does not refresh the cap.
        let _ = self.awaiting_since_ms.compare_exchange(
            0,
            epoch_millis(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// Record whether the agent's screen currently shows an unanswered question.
    pub fn set_awaiting_user_input(&self, waiting: bool) {
        self.set_awaiting_user_input_because(waiting.then(|| "question on screen".to_string()));
    }

    /// True when the agent is parked on a prompt that needs a human answer.
    ///
    /// Expires after [`AWAITING_INPUT_MAX`]: question detection reads the
    /// screen, and a false positive must not defer bus delivery forever.
    pub fn is_awaiting_user_input(&self) -> bool {
        let since = self.awaiting_since_ms.load(Ordering::Relaxed);
        if since == 0 {
            return false;
        }
        epoch_millis().saturating_sub(since) < AWAITING_INPUT_MAX.as_millis() as u64
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
        self.current_activity().0
    }

    /// The current state together with the evidence for it.
    ///
    /// Both come from one pass so a reported reason can never describe a
    /// different branch than the state it is stored beside.
    pub fn current_activity(&self) -> (ActivityState, String) {
        let now = epoch_millis();
        if !self.is_idle() {
            return (ActivityState::UserTyping, "user typed recently".into());
        }
        if self.has_pending_line() {
            return (
                ActivityState::UserTyping,
                "partial input line buffered".into(),
            );
        }
        if self.has_stashed_draft() {
            return (
                ActivityState::UserTyping,
                "draft stashed for an injected message".into(),
            );
        }
        if self.is_awaiting_user_input() {
            let reason = self
                .awaiting_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_else(|| "question on screen".into());
            return (ActivityState::NeedsInput, reason);
        }
        let output_at = self.last_pty_output_at_ms.load(Ordering::Relaxed);
        if output_at > 0 && now.saturating_sub(output_at) < PTY_OUTPUT_BUSY_MS {
            let ago = now.saturating_sub(output_at);
            return (
                ActivityState::AgentWorking,
                format!("PTY output {ago}ms ago"),
            );
        }
        let spinner_at = self.last_spinner_at_ms.load(Ordering::Relaxed);
        if spinner_at > 0 && now.saturating_sub(spinner_at) < PTY_SPINNER_BUSY_MS {
            let ago = now.saturating_sub(spinner_at);
            return (
                ActivityState::AgentWorking,
                format!("status indicator {ago}ms ago"),
            );
        }
        (
            ActivityState::Idle,
            "no output, no question on screen".into(),
        )
    }

    pub fn publish_activity(&self, agent_name: &str) {
        let (state, reason) = self.current_activity();
        crate::activity::publish_with_reason(agent_name, state, Some(reason));
    }

    fn update_terminal_state(&self, bytes: &[u8]) {
        self.input_mode.feed(bytes);
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

/// Start the PTY daemon bus client that delivers bus messages into the wrapped
/// agent's PTY.
pub fn start_poller(
    agent_name: String,
    agent_kind: String,
    pty_fd: Arc<OwnedFd>,
    input_state: Arc<UserInputState>,
    child_pid: i32,
    notice_tx: tokio::sync::mpsc::UnboundedSender<PtyNotice>,
) {
    POLLER_SHUTDOWN.store(false, Ordering::Relaxed);

    let inject_agent = agent_name.clone();
    std::thread::spawn(move || {
        let submit_encoding = submit_encoding_for_agent(&agent_kind);
        let interrupt_sequence = interrupt_sequence_for_agent(&agent_kind);
        while !POLLER_SHUTDOWN.load(Ordering::Relaxed) {
            if run_daemon_bus_client(
                &inject_agent,
                submit_encoding,
                interrupt_sequence,
                &pty_fd,
                &input_state,
                &notice_tx,
                child_pid,
            ) {
                break;
            }
            crate::broker::try_log_error(
                "poller",
                "daemon bus attach failed; retrying PTY bus delivery through daemon",
                Some(&inject_agent),
            );
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

fn run_daemon_bus_client(
    agent_name: &str,
    submit_encoding: PtySubmitEncoding,
    interrupt_sequence: Option<&'static [u8]>,
    pty_fd: &OwnedFd,
    input_state: &UserInputState,
    notice_tx: &tokio::sync::mpsc::UnboundedSender<PtyNotice>,
    child_pid: i32,
) -> bool {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    if crate::daemon::ensure_running().is_err() {
        return false;
    }
    let Ok(mut stream) = UnixStream::connect(crate::daemon::socket_path()) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(BUS_READ_POLL));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let attach = serde_json::json!({"type": "bus_attach", "agent": agent_name});
    let Ok(mut attach_line) = serde_json::to_string(&attach) else {
        return false;
    };
    attach_line.push('\n');
    if stream.write_all(attach_line.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }

    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let mut reader = std::io::BufReader::new(reader_stream);
    let mut line = String::new();
    if reader
        .read_line(&mut line)
        .ok()
        .filter(|n| *n > 0)
        .is_none()
    {
        return false;
    }
    let Ok(attached) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return false;
    };
    if attached.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }
    crate::broker::try_log_event(
        "debug",
        "poller",
        &format!("attached daemon bus delivery for {agent_name}"),
        None,
    );

    drop(line);
    let target = PtyTarget {
        fd: pty_fd,
        input_state,
        notice_tx,
        submit_encoding,
        child_pid,
    };
    bus_client_loop(stream, &target, interrupt_sequence)
}

/// Read dispatched frames, hold them until the pane accepts input, then paste
/// the whole backlog at once. Split out from the connection setup so it can be
/// driven over a socket pair in tests.
fn bus_client_loop(
    mut stream: std::os::unix::net::UnixStream,
    target: &PtyTarget<'_>,
    interrupt_sequence: Option<&[u8]>,
) -> bool {
    use std::io::BufRead;

    let input_state = target.input_state;
    let Ok(reader_stream) = stream.try_clone() else {
        return false;
    };
    let mut reader = std::io::BufReader::new(reader_stream);
    // Messages the daemon has handed over that the pane has not accepted yet.
    // Holding them here rather than blocking inside a single delivery is what
    // lets a later message join the same paste instead of queueing behind a
    // turn boundary of its own.
    let mut pending: Vec<PendingMessage> = Vec::new();
    let mut inbox: Vec<u8> = Vec::new();
    let mut blocked_since: Option<std::time::Instant> = None;
    let mut last_block_log = 0u64;

    loop {
        if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
            return true;
        }

        // `fill_buf` rather than `read_line`: the socket read times out every
        // BUS_READ_POLL so the gate below is re-tested, and `read_line` leaves
        // an unspecified amount of the partial line in its output buffer when it
        // returns an error. `fill_buf` consumes nothing on timeout, so a frame
        // split across polls is reassembled here instead of relied upon there.
        match reader.fill_buf() {
            Ok([]) => return false,
            Ok(chunk) => {
                let n = chunk.len();
                inbox.extend_from_slice(chunk);
                reader.consume(n);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return false,
        }
        while let Some(eol) = inbox.iter().position(|&b| b == b'\n') {
            let frame_bytes: Vec<u8> = inbox.drain(..=eol).collect();
            if let Some(msg) = parse_bus_frame(&frame_bytes[..eol]) {
                pending.push(msg);
            }
        }

        if pending.is_empty() {
            blocked_since = None;
            last_block_log = 0;
            continue;
        }

        let wants_interrupt = pending.iter().any(|m| m.interrupt) && interrupt_sequence.is_some();
        // A message with no deadline and no delivery record can be lost in
        // silence, which is worse than arriving late. Once the wait passes the
        // deadline the agent-is-busy veto stops counting.
        let waited_so_far = blocked_since.map_or(0, |t: std::time::Instant| t.elapsed().as_secs());
        let forced = waited_so_far >= INJECT_FORCE_AFTER_SECS && !user_blocks_submit(input_state);
        if !forced && pty_submit_wait_blocked_for(input_state, wants_interrupt) {
            let since = *blocked_since.get_or_insert_with(std::time::Instant::now);
            let waited = since.elapsed().as_secs();
            if waited >= last_block_log + BLOCK_LOG_INTERVAL_SECS {
                last_block_log = waited;
                crate::broker::try_log_event(
                    "debug",
                    "poller",
                    &format!(
                        "inject blocked: user_idle={} agent_working={} awaiting_input={} pending_line={} waited={waited}s queued={} bytes={}",
                        input_state.is_idle(),
                        input_state.is_agent_working(),
                        input_state.is_awaiting_user_input(),
                        input_state.has_pending_line(),
                        pending.len(),
                        pending.iter().map(|m| m.body.len()).sum::<usize>(),
                    ),
                    None,
                );
            }
            continue;
        }

        if forced {
            crate::broker::try_log_event(
                "info",
                "poller",
                &format!(
                    "inject forced after {waited_so_far}s: agent still working, delivering {} queued message(s)",
                    pending.len()
                ),
                None,
            );
        }
        blocked_since = None;
        last_block_log = 0;

        let take = batch_cutoff(&pending);
        let batch: Vec<PendingMessage> = pending.drain(..take).collect();
        // Only the messages actually in this paste get to force an interrupt.
        let batch_interrupts = batch.iter().any(|m| m.interrupt) || forced;
        let body = coalesce_batch(&batch, crate::message::epoch_secs());
        let ok = deliver_to_pty(
            target,
            &body,
            true,
            batch_interrupts.then_some(interrupt_sequence).flatten(),
            forced,
        );

        if !ok {
            // Hand the batch back to the daemon rather than retrying blind: the
            // gate closed between the check and the write, and another pass may
            // pick a different cut.
            for msg in &batch {
                if !ack_message(&mut stream, msg.id, false) {
                    return false;
                }
            }
            continue;
        }
        for msg in &batch {
            // Record the paste here, before the ack. An ack that never lands
            // would otherwise leave the row claimable again, and the next
            // dispatch would paste this message into the pane a second time.
            let _ = crate::broker::mark_message_delivered(msg.id);
        }
        for msg in &batch {
            if !ack_message(&mut stream, msg.id, true) {
                return false;
            }
        }
    }
}

/// Decode one newline-delimited daemon frame. Anything that is not a bus
/// message is ignored rather than treated as an error.
fn parse_bus_frame(raw: &[u8]) -> Option<PendingMessage> {
    let text = std::str::from_utf8(raw).ok()?;
    let frame = serde_json::from_str::<serde_json::Value>(text.trim()).ok()?;
    if frame.get("type").and_then(|v| v.as_str()) != Some("bus_message") {
        return None;
    }
    Some(PendingMessage {
        id: frame.get("id").and_then(|v| v.as_i64())?,
        body: frame
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        created_at: frame
            .get("created_at")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        interrupt: frame
            .get("interrupt")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Write one ack frame. Returns false when the socket is gone.
fn ack_message(stream: &mut std::os::unix::net::UnixStream, id: i64, ok: bool) -> bool {
    use std::io::Write;
    let ack = serde_json::json!({"type": "bus_ack", "id": id, "ok": ok});
    let Ok(mut ack_line) = serde_json::to_string(&ack) else {
        return false;
    };
    ack_line.push('\n');
    stream.write_all(ack_line.as_bytes()).is_ok() && stream.flush().is_ok()
}

/// How many queued messages fit in one paste. Always at least one, so an
/// oversized single message still moves.
fn batch_cutoff(pending: &[PendingMessage]) -> usize {
    let mut bytes = 0usize;
    for (i, msg) in pending.iter().enumerate() {
        bytes += msg.body.len();
        if bytes > MAX_BATCH_BYTES {
            return i.max(1);
        }
    }
    pending.len()
}

/// Render a batch as one paste.
///
/// A message that waited carries the time it was written next to the time it
/// landed. Without that pair a delayed reply reads as current, which is the
/// failure that makes late delivery worse than no delivery.
fn coalesce_batch(batch: &[PendingMessage], now: u64) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(batch.len());
    for msg in batch {
        let delay = now.saturating_sub(msg.created_at);
        if msg.created_at > 0 && delay >= DELIVERY_STAMP_MIN_SECS {
            parts.push(format!(
                "[sidekar] delayed {} · written {} · delivered {}\n{}",
                human_delay(delay),
                local_clock(msg.created_at),
                local_clock(now),
                msg.body
            ));
        } else {
            parts.push(msg.body.clone());
        }
    }
    if parts.len() > 1 {
        format!(
            "[sidekar] {} messages arrived while this pane was busy, delivered together:\n\n{}",
            parts.len(),
            parts.join("\n\n")
        )
    } else {
        parts.pop().unwrap_or_default()
    }
}

fn local_clock(epoch: u64) -> String {
    let t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn human_delay(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// The pane a delivery writes into. Fixed for the life of one bus client.
struct PtyTarget<'a> {
    fd: &'a OwnedFd,
    input_state: &'a UserInputState,
    notice_tx: &'a tokio::sync::mpsc::UnboundedSender<PtyNotice>,
    submit_encoding: PtySubmitEncoding,
    child_pid: i32,
}

/// Deliver one bus message. Returns true when the message can be acked/dequeued.
fn deliver_to_pty(
    target: &PtyTarget<'_>,
    message: &str,
    submit_input: bool,
    interrupt_sequence: Option<&[u8]>,
    forced: bool,
) -> bool {
    if POLLER_SHUTDOWN.load(Ordering::Relaxed) {
        return false;
    }

    let PtyTarget {
        fd,
        input_state,
        notice_tx,
        submit_encoding,
        child_pid,
    } = *target;
    let raw_fd = fd.as_raw_fd();

    // Wait until submitted input will not collide with user input or agent output.
    //
    // `sidecar_notice_allowed` only controls out-of-band terminal notices. It
    // must not gate real prompt submission: some TUIs, notably Cursor Agent,
    // hide the cursor or own the screen while idle at the prompt.
    // The caller owns the input gate. It waits there so that messages dispatched
    // during the wait join this paste instead of each claiming a turn of their
    // own, so by the time execution reaches here the gate was open on the last
    // check and only the narrow races below remain.
    let still_blocked = if forced {
        user_blocks_submit(input_state)
    } else {
        pty_submit_wait_blocked_for(input_state, interrupt_sequence.is_some())
    };
    if still_blocked {
        return false;
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

    if let Some(bytes) = interrupt_sequence {
        if let Err(e) = crate::pty::write_all_fd(raw_fd, bytes) {
            crate::broker::try_log_event(
                "error",
                "poller",
                &format!("interrupt write failed: {e}"),
                None,
            );
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    let submit_encoding = resolve_submit_encoding(input_state, submit_encoding);
    let submit_bytes = encode_submit_input(message, submit_encoding);
    if let Err(e) = crate::pty::write_all_fd(raw_fd, &submit_bytes) {
        crate::broker::try_log_event(
            "error",
            "poller",
            &format!("inject write failed: {e}"),
            None,
        );
        return false;
    }
    std::thread::sleep(Duration::from_millis(150));
    if let Err(e) = crate::pty::write_all_fd(raw_fd, b"\r") {
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
            "injected {}B via {:?} + SIGWINCH (stashed_draft={})",
            message.len(),
            submit_encoding,
            stashed_draft.is_some(),
        ),
        None,
    );

    // Put the draft back. The line was lifted out so the paste could not merge
    // with it; leaving the human to press a key to get their own typing back
    // makes a delivery they did not ask for into a chore they have to notice.
    if let Some(preview) = stashed_draft {
        std::thread::sleep(Duration::from_millis(200));
        match input_state.take_stashed_draft() {
            Some(draft) if !draft.is_empty() => {
                if let Err(e) = crate::pty::write_all_fd(raw_fd, &draft) {
                    // The message is already in. Say where the draft went
                    // rather than dropping it silently.
                    crate::broker::try_log_error(
                        "poller",
                        "failed to restore draft after inject",
                        Some(&format!("{e}")),
                    );
                    crate::tunnel::tunnel_println(&format!(
                        "\x1b[33m[sidekar]\x1b[0m Draft saved: \"{preview}\" — ↑ to restore"
                    ));
                } else {
                    input_state.set_pending_line(&draft);
                    crate::broker::try_log_event(
                        "debug",
                        "poller",
                        &format!("restored {}B draft after inject", draft.len()),
                        None,
                    );
                }
            }
            _ => {}
        }
    }
    true
}

fn interrupt_sequence_for_agent(agent_kind: &str) -> Option<&'static [u8]> {
    match agent_kind {
        "agent" | "claude" | "codex" | "copilot" | "cursor" | "cursor-agent" | "gemini"
        | "grok" | "opencode" | "pi" => Some(b"\x1b"),
        _ => None,
    }
}

/// Clear the child readline/PTY input line (Ctrl+U) before bus inject.
fn clear_pty_input_line(raw_fd: i32) -> anyhow::Result<()> {
    crate::pty::write_all_fd(raw_fd, b"\x15")?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PtySubmitEncoding {
    Raw,
    BracketedPaste,
}

/// Prefer the paste mode the agent announced on its own output stream over the
/// per-agent default. An agent that turned bracketed paste off — permission
/// prompts routinely do — would otherwise receive the `ESC [ 200 ~` wrapper as
/// literal text.
fn resolve_submit_encoding(
    input_state: &UserInputState,
    fallback: PtySubmitEncoding,
) -> PtySubmitEncoding {
    match input_state.input_mode().bracketed_paste_observed() {
        Some(true) => PtySubmitEncoding::BracketedPaste,
        Some(false) => PtySubmitEncoding::Raw,
        None => fallback,
    }
}

/// Default used until the agent announces a bracketed-paste mode of its own.
fn submit_encoding_for_agent(agent_kind: &str) -> PtySubmitEncoding {
    match agent_kind {
        "agent" | "claude" | "codex" | "copilot" | "cursor" | "cursor-agent" | "gemini"
        | "grok" | "opencode" | "pi" => PtySubmitEncoding::BracketedPaste,
        _ => PtySubmitEncoding::Raw,
    }
}

fn encode_submit_input(message: &str, encoding: PtySubmitEncoding) -> Vec<u8> {
    match encoding {
        PtySubmitEncoding::Raw => message.as_bytes().to_vec(),
        PtySubmitEncoding::BracketedPaste => {
            let mut out = Vec::with_capacity(message.len() + 12);
            out.extend_from_slice(b"\x1b[200~");
            out.extend_from_slice(message.as_bytes());
            out.extend_from_slice(b"\x1b[201~");
            out
        }
    }
}

/// Blocking conditions that belong to the human at the keyboard.
///
/// These are absolute. A paste that merges with a half-typed line, or that
/// answers an on-screen question on someone's behalf, is worse than a late
/// message, so neither the interrupt path nor the delivery deadline overrides
/// them. A stashed draft is deliberately absent: it is cleared only by a
/// keystroke, so blocking on it can deafen a pane for as long as its human is
/// away.
fn user_blocks_submit(input_state: &UserInputState) -> bool {
    !input_state.is_idle() || input_state.is_awaiting_user_input()
}

fn pty_submit_wait_blocked_for(input_state: &UserInputState, interrupt: bool) -> bool {
    user_blocks_submit(input_state) || (!interrupt && input_state.is_agent_working())
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
