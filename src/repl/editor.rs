use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use regex::Regex;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::broker;

/// What `read_input_or_bus` returned.
pub(super) enum InputEvent {
    /// User typed a line (optional pasted image attachments).
    User(SubmittedLine),
    /// One or more bus messages arrived while idle.
    Bus,
    /// EOF / error.
    Eof,
}

/// One submitted REPL line (possibly with pasted image attachments).
#[derive(Debug, Clone)]
pub struct SubmittedLine {
    pub text: String,
    pub image_paths: Vec<PathBuf>,
}

const PROMPT_PREFIX: &str = "\x1b[36m›\x1b[0m ";
const PROMPT_VISIBLE: &str = "› ";
const CONTINUATION_PREFIX: &str = "\x1b[2m·\x1b[0m ";
const CONTINUATION_VISIBLE: &str = "· ";
const ESC_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(75);
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;
const PLACEHOLDER_DIM_OPEN: &str = "\x1b[2m";
const PLACEHOLDER_DIM_CLOSE: &str = "\x1b[22m";

#[derive(Debug, Clone)]
struct PendingPaste {
    placeholder: String,
    content: String,
}

type SharedEditor = Arc<Mutex<LineEditor>>;

fn build_input_pollfds(stdin_fd: Option<i32>, tunnel_fd: Option<i32>) -> Vec<libc::pollfd> {
    let mut pollfds = Vec::with_capacity(2);
    if let Some(fd) = stdin_fd {
        pollfds.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    if let Some(fd) = tunnel_fd {
        pollfds.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    pollfds
}

/// Append `buffer[row]` to `out`, wrapping any byte ranges in `spans` (full-buffer coords)
/// that intersect the row with dim ANSI so paste placeholders render distinctly.
fn append_row_with_placeholders(
    out: &mut String,
    buffer: &str,
    row: std::ops::Range<usize>,
    spans: &[std::ops::Range<usize>],
) {
    if spans.is_empty() {
        out.push_str(&buffer[row]);
        return;
    }
    let mut idx = row.start;
    for span in spans {
        if span.end <= row.start {
            continue;
        }
        if span.start >= row.end {
            break;
        }
        let s = span.start.max(row.start);
        let e = span.end.min(row.end);
        if s > idx {
            out.push_str(&buffer[idx..s]);
        }
        out.push_str(PLACEHOLDER_DIM_OPEN);
        out.push_str(&buffer[s..e]);
        out.push_str(PLACEHOLDER_DIM_CLOSE);
        idx = e;
    }
    if idx < row.end {
        out.push_str(&buffer[idx..row.end]);
    }
}

fn emit_raw(text: &str) {
    print!("{text}");
    crate::tunnel::tunnel_send(text.as_bytes().to_vec());
    let _ = io::stdout().flush();
}

fn active_prompt_slot() -> &'static Mutex<Option<Weak<Mutex<LineEditor>>>> {
    static SLOT: OnceLock<Mutex<Option<Weak<Mutex<LineEditor>>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn register_active_prompt(editor: &SharedEditor) {
    if let Ok(mut slot) = active_prompt_slot().lock() {
        *slot = Some(Arc::downgrade(editor));
    }
}

fn clear_active_prompt() {
    if let Ok(mut slot) = active_prompt_slot().lock() {
        *slot = None;
    }
}

fn with_active_prompt<R>(f: impl FnOnce(&mut LineEditor) -> R) -> Option<R> {
    let weak = active_prompt_slot().lock().ok()?.clone()?;
    let editor = weak.upgrade()?;
    let mut guard = editor.lock().ok()?;
    Some(f(&mut guard))
}

pub(super) fn emit_shared_output(text: &str) {
    if with_active_prompt(|editor| {
        editor.clear_prompt_and_status_inner();
        editor.status_visible = false;
        emit_raw(text);
        editor.redraw_inner();
    })
    .is_none()
    {
        emit_raw(text);
    }
}

pub(super) fn emit_shared_line(text: &str) {
    let mut line = String::with_capacity(text.len() + 1);
    line.push_str(text);
    line.push('\n');
    emit_shared_output(&line);
}

pub(super) fn emit_transient_status(text: &str) {
    if with_active_prompt(|editor| {
        let cols = editor.terminal_columns();
        let truncated = truncate_ansi_to_width(text, cols.saturating_sub(1));
        editor.clear_prompt_and_status_inner();
        emit_raw("\r\x1b[2K");
        emit_raw(&truncated);
        emit_raw("\n");
        editor.status_visible = true;
        editor.redraw_inner();
    })
    .is_none()
    {
        // Truncate to terminal width so the preview never wraps to a second
        // line — clear_transient_status can only erase one row.
        let cols = standalone_terminal_columns();
        let truncated = truncate_ansi_to_width(text, cols.saturating_sub(1));
        emit_raw("\r\x1b[2K");
        emit_raw(&truncated);
    }
}

pub(super) fn clear_transient_status() {
    if with_active_prompt(|editor| {
        if !editor.status_visible {
            return;
        }
        editor.clear_prompt_and_status_inner();
        editor.status_visible = false;
        editor.redraw_inner();
    })
    .is_none()
    {
        emit_raw("\r\x1b[K");
    }
}

pub(super) struct RawModeGuard {
    saved: libc::termios,
    fd: i32,
}

impl RawModeGuard {
    pub(super) fn enter() -> Result<Self> {
        let fd = libc::STDIN_FILENO;
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            anyhow::bail!("tcgetattr failed: {}", std::io::Error::last_os_error());
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            anyhow::bail!("tcsetattr failed: {}", std::io::Error::last_os_error());
        }
        // Enable bracketed paste mode
        emit_raw("\x1b[?2004h");
        Ok(Self { saved, fd })
    }

    pub(super) fn enter_cooked() -> Option<Self> {
        let fd = libc::STDIN_FILENO;
        let mut current: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut current) } != 0 {
            return None;
        }
        let mut cooked = current;
        cooked.c_lflag |= libc::ICANON | libc::ECHO | libc::ISIG;
        cooked.c_iflag |= libc::ICRNL;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &cooked) };
        Some(Self { saved: current, fd })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        emit_raw("\x1b[?2004l");
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

struct EscDetector {
    pending_escape: Option<std::time::Instant>,
}

impl EscDetector {
    fn new() -> Self {
        Self {
            pending_escape: None,
        }
    }

    fn feed_bytes(&mut self, bytes: &[u8], now: std::time::Instant) -> Vec<u8> {
        let mut forwarded = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            if self.pending_escape.is_some() {
                self.pending_escape = None;
                if byte == 0x1b {
                    forwarded.push(0x1b);
                    self.pending_escape = Some(now);
                    continue;
                }
                forwarded.push(0x1b);
                forwarded.push(byte);
                continue;
            }
            if byte == 0x1b {
                self.pending_escape = Some(now);
            } else {
                forwarded.push(byte);
            }
        }
        forwarded
    }

    fn check_timeout(&mut self, now: std::time::Instant) -> bool {
        let Some(started) = self.pending_escape else {
            return false;
        };
        if now.duration_since(started) >= ESC_TIMEOUT {
            self.pending_escape = None;
            return true;
        }
        false
    }
}

pub(super) struct EscCancelWatcher {
    running: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    raw_mode: Option<RawModeGuard>,
}

impl EscCancelWatcher {
    pub(super) fn start(
        cancel: Arc<std::sync::atomic::AtomicBool>,
        tunnel_fd: Option<i32>,
    ) -> Self {
        let raw_mode = RawModeGuard::enter().ok();
        let local_fd = if raw_mode.is_some() {
            Some(libc::STDIN_FILENO)
        } else {
            None
        };
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_thread = running.clone();
        let handle = if local_fd.is_none() && tunnel_fd.is_none() {
            None
        } else {
            Some(std::thread::spawn(move || {
                let mut detector = EscDetector::new();
                let mut buf = [0u8; 64];
                while running_thread.load(std::sync::atomic::Ordering::Relaxed)
                    && !cancel.load(std::sync::atomic::Ordering::Relaxed)
                {
                    let mut fds = build_input_pollfds(local_fd, tunnel_fd);
                    if fds.is_empty() {
                        break;
                    }
                    let ready =
                        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 50) };
                    let now = std::time::Instant::now();
                    if ready > 0 {
                        for pollfd in fds.iter() {
                            if (pollfd.revents & libc::POLLIN) == 0 {
                                continue;
                            }
                            let n = unsafe {
                                libc::read(
                                    pollfd.fd,
                                    buf.as_mut_ptr() as *mut libc::c_void,
                                    buf.len(),
                                )
                            };
                            if n > 0 {
                                let bytes = &buf[..n as usize];
                                // Ctrl+C (0x03): immediate cancel, same as bare ESC
                                if bytes.contains(&0x03) {
                                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                                    return;
                                }
                                let _ = detector.feed_bytes(bytes, now);
                            }
                        }
                    }
                    if detector.check_timeout(now) {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                }
            }))
        };

        Self {
            running,
            handle,
            raw_mode,
        }
    }
}

impl Drop for EscCancelWatcher {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = self.raw_mode.take();
    }
}

pub(super) struct ActivePromptSession {
    editor: Option<SharedEditor>,
    running: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    raw_mode: Option<RawModeGuard>,
}

impl ActivePromptSession {
    pub(super) fn start(
        editor: LineEditor,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        tunnel_fd: Option<i32>,
    ) -> Self {
        let editor = Arc::new(Mutex::new(editor));
        register_active_prompt(&editor);
        if let Ok(mut guard) = editor.lock() {
            guard.redraw_inner();
        }

        let raw_mode = RawModeGuard::enter().ok();
        let use_raw_stdin = raw_mode.is_some();
        let stdin_poll_fd = if use_raw_stdin {
            Some(libc::STDIN_FILENO)
        } else {
            None
        };
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_thread = running.clone();
        let editor_thread = editor.clone();

        let handle = if stdin_poll_fd.is_none() && tunnel_fd.is_none() {
            None
        } else {
            Some(std::thread::spawn(move || {
                let mut detector = EscDetector::new();
                let mut buf = [0u8; 64];
                while running_thread.load(std::sync::atomic::Ordering::Relaxed)
                    && !cancel.load(std::sync::atomic::Ordering::Relaxed)
                {
                    // Flush any paste-burst held chars before blocking on poll,
                    // mirroring read_input_or_bus's idle-tick behaviour.
                    let poll_timeout_ms = if let Ok(mut editor) = editor_thread.lock() {
                        editor.flush_paste_burst_if_due();
                        if editor.paste_burst_is_active() { 10 } else { 50 }
                    } else {
                        50
                    };

                    let mut fds = build_input_pollfds(stdin_poll_fd, tunnel_fd);
                    let ready = unsafe {
                        libc::poll(
                            fds.as_mut_ptr(),
                            fds.len() as libc::nfds_t,
                            poll_timeout_ms,
                        )
                    };
                    let now = std::time::Instant::now();
                    if ready > 0 {
                        for pollfd in fds.iter() {
                            if (pollfd.revents & libc::POLLIN) == 0 {
                                continue;
                            }

                            let n = unsafe {
                                libc::read(
                                    pollfd.fd,
                                    buf.as_mut_ptr() as *mut libc::c_void,
                                    buf.len(),
                                )
                            };
                            if n <= 0 {
                                continue;
                            }
                            let chunk = &buf[..n as usize];
                            let forwarded = detector.feed_bytes(chunk, now);
                            if forwarded.is_empty() {
                                continue;
                            }
                            if let Ok(mut editor) = editor_thread.lock() {
                                let r = editor.process_input_bytes(&forwarded, |ed, line| {
                                    ed.queue_pending_followup(line);
                                });
                                if r.is_err() {
                                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                                    return;
                                }
                            }
                        }
                    } else if ready == 0 {
                        // Idle tick — flush paste burst and resolve pending
                        // escape sequences, same as read_input_or_bus does.
                        if let Ok(mut editor) = editor_thread.lock() {
                            let _ = editor.maybe_resolve_pending_escape();
                            editor.flush_paste_burst_if_due();
                        }
                    }
                    if detector.check_timeout(now) {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                }
            }))
        };

        Self {
            editor: Some(editor),
            running,
            handle,
            raw_mode,
        }
    }

    pub(super) fn finish(mut self) -> LineEditor {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = self.raw_mode.take();

        clear_active_prompt();

        let editor_arc = self.editor.take().expect("active prompt editor");
        if let Ok(mut guard) = editor_arc.lock() {
            guard.clear_rendered_prompt_inner();
        }

        Arc::try_unwrap(editor_arc)
            .ok()
            .and_then(|mutex| mutex.into_inner().ok())
            .unwrap_or_default()
    }
}

impl Drop for ActivePromptSession {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = self.raw_mode.take();
        clear_active_prompt();
    }
}

const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

fn is_word_separator(ch: char) -> bool {
    WORD_SEPARATORS.contains(ch)
}

fn renumber_pending_image_labels(text: &str, offset: usize) -> String {
    if offset == 0 {
        return text.to_string();
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\[Image #(\d+)\]").expect("regex"));
    re.replace_all(text, |caps: &regex::Captures| {
        let n: usize = caps[1].parse().unwrap_or(0);
        format!("[Image #{}]", offset + n)
    })
    .into_owned()
}

fn standalone_terminal_columns() -> usize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = libc::STDOUT_FILENO;
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        80
    }
}

fn truncate_preview_to_width(s: &str, max_display_width: usize) -> String {
    if max_display_width == 0 {
        return String::new();
    }
    let mut used = 0usize;
    let mut out = String::new();
    let ellipsis_w = UnicodeWidthStr::width("…");
    for g in s.graphemes(true) {
        let gw = UnicodeWidthStr::width(g);
        if used + gw > max_display_width {
            if used + ellipsis_w <= max_display_width {
                out.push('…');
            }
            break;
        }
        out.push_str(g);
        used += gw;
    }
    out
}

/// Truncate a string to `max_display_width` visible columns, passing through
/// ANSI CSI escape sequences without counting them toward the budget. Strips
/// embedded control characters (tab, newline, vertical tab, CR) which would
/// otherwise occupy rows beyond the intended single-row status line. Always
/// emits `\x1b[0m` at the end as a defensive reset.
pub(super) fn truncate_ansi_to_width(s: &str, max_display_width: usize) -> String {
    let mut out = String::new();
    if max_display_width == 0 {
        out.push_str("\x1b[0m");
        return out;
    }
    let mut used = 0usize;
    let ellipsis_w = UnicodeWidthStr::width("…");
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut truncated = false;
    while i < bytes.len() {
        let b = bytes[i];
        // Pass through ANSI CSI escape sequences: ESC '[' ... final-byte [@-~]
        if b == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let start = i;
            i += 2;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                if (0x40..=0x7e).contains(&c) {
                    break;
                }
            }
            out.push_str(&s[start..i]);
            continue;
        }
        // Drop other control chars that could break single-row layout.
        if b < 0x20 || b == 0x7f {
            i += 1;
            continue;
        }
        // Decode a UTF-8 grapheme cluster starting at i.
        let rest = &s[i..];
        let g = match rest.graphemes(true).next() {
            Some(g) => g,
            None => break,
        };
        let gw = UnicodeWidthStr::width(g);
        if used + gw > max_display_width {
            truncated = true;
            break;
        }
        out.push_str(g);
        used += gw;
        i += g.len();
    }
    if truncated && used + ellipsis_w <= max_display_width {
        out.push('…');
    }
    out.push_str("\x1b[0m");
    out
}

enum LineEditResult {
    Continue,
    Submit(SubmittedLine),
    Eof,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CursorPos {
    row: usize,
    col: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RenderLayout {
    rows: usize,
    cursor: CursorPos,
    end: CursorPos,
}

pub(super) struct LineEditor {
    buffer: String,
    cursor: usize,
    escape: Vec<u8>,
    utf8: Vec<u8>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: Option<String>,
    escape_started_at: Option<std::time::Instant>,
    preferred_col: Option<usize>,
    status_visible: bool,
    rendered_rows: usize,
    rendered_cursor: CursorPos,
    kill_buffer: String,
    paste_buffer: Option<Vec<u8>>,
    /// Lines completed inside a single `read()` chunk (or across bursts); drained before blocking again.
    pending_submits: VecDeque<SubmittedLine>,
    /// Submitted while the agent was running; shown on the row above the prompt, pulled with ↑ on the top line.
    pub(super) pending_followups: VecDeque<SubmittedLine>,
    /// Pasted/dragged image paths; text uses `[Image #N]` matching 1-based indices here.
    attached_images: Vec<PathBuf>,
    /// Large pastes shown in the buffer as `[Pasted Content N chars]` placeholders;
    /// expanded back to full text on submit.
    pending_pastes: Vec<PendingPaste>,
    /// Per-char-count counter so duplicate paste sizes get `#2`, `#3`, ... suffix.
    large_paste_counters: HashMap<usize, usize>,
    rendered_pending_rows: usize,
    /// Detects keyboard-delivered paste bursts (drag-and-drop on terminals
    /// that don't wrap pastes in bracketed-paste sequences).
    paste_burst: PasteBurst,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::with_history(Vec::new())
    }
}

impl LineEditor {
    pub(super) fn with_history(history: Vec<String>) -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            escape: Vec::new(),
            utf8: Vec::new(),
            history,
            history_index: None,
            history_draft: None,
            escape_started_at: None,
            preferred_col: None,
            status_visible: false,
            rendered_rows: 0,
            rendered_cursor: CursorPos::default(),
            kill_buffer: String::new(),
            paste_buffer: None,
            pending_submits: VecDeque::new(),
            pending_followups: VecDeque::new(),
            attached_images: Vec::new(),
            pending_pastes: Vec::new(),
            large_paste_counters: HashMap::new(),
            rendered_pending_rows: 0,
            paste_burst: PasteBurst::default(),
        }
    }

    /// Returns a line that was already completed from a prior `process_input_bytes` burst.
    pub(super) fn take_next_pending_submit(&mut self) -> Option<SubmittedLine> {
        self.pending_submits.pop_front()
    }

    fn queue_pending_followup(&mut self, line: SubmittedLine) {
        self.pending_followups.push_back(line);
        self.redraw_inner();
    }

    /// Feeds every byte in `bytes`, invoking `on_submit` for each completed line. Stops on EOF (empty buffer + Ctrl-D).
    pub(super) fn process_input_bytes<F>(
        &mut self,
        bytes: &[u8],
        mut on_submit: F,
    ) -> Result<(), ()>
    where
        F: FnMut(&mut Self, SubmittedLine),
    {
        let images_before = self.attached_images.len();
        for &byte in bytes {
            match self.feed_byte(byte) {
                LineEditResult::Continue => {}
                LineEditResult::Submit(line) => {
                    on_submit(self, line);
                }
                LineEditResult::Eof => return Err(()),
            }
        }
        // Terminal.app appears to defer rendering while it's still processing
        // a drag-drop/bracketed-paste event. Intermediate redraws fired
        // during the chunk (e.g., one per `201~` end marker) land on stdout
        // but don't surface on screen until the next keypress. Re-emit once
        // the chunk is fully drained so the final frame is guaranteed to
        // render.
        if self.attached_images.len() != images_before {
            self.redraw_inner();
        }
        Ok(())
    }

    fn insert_pasted_text(&mut self, text: &str) {
        let t = text.trim_end_matches('\0');
        if t.is_empty() {
            return;
        }
        let char_count = t.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            self.detach_history_nav();
            self.buffer.insert_str(self.cursor, &placeholder);
            self.cursor += placeholder.len();
            self.pending_pastes.push(PendingPaste {
                placeholder,
                content: t.to_string(),
            });
            self.preferred_col = None;
            return;
        }
        // For paste-burst flushes, the user may have hit Enter before the
        // idle timeout, appending a trailing newline to the burst. Ignore any
        // leading/trailing whitespace when checking for a single-line image
        // path so drag-and-drop still attaches as an image.
        let trimmed = t.trim();
        if !trimmed.contains('\n')
            && trimmed.len() < 16_384
            && let Some(pb) = super::user_turn::normalize_pasted_path(trimmed)
            && looks_like_image_file(&pb)
        {
            self.attach_image(pb);
            return;
        }
        self.detach_history_nav();
        self.buffer.insert_str(self.cursor, t);
        self.cursor += t.len();
        self.preferred_col = None;
    }

    fn next_large_paste_placeholder(&mut self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let entry = self.large_paste_counters.entry(char_count).or_insert(0);
        *entry += 1;
        if *entry == 1 {
            base
        } else {
            format!("{base} #{}", *entry)
        }
    }

    /// All non-overlapping byte ranges in `buffer` covered by active paste placeholders,
    /// sorted by start. Longer placeholders win when they overlap shorter ones (e.g.,
    /// `[Pasted Content 100 chars]` vs `[Pasted Content 100 chars] #2`).
    fn placeholder_spans(&self) -> Vec<std::ops::Range<usize>> {
        let mut placeholders: Vec<&str> =
            self.pending_pastes.iter().map(|p| p.placeholder.as_str()).collect();
        placeholders.sort_by_key(|p| std::cmp::Reverse(p.len()));
        let mut taken = vec![false; self.buffer.len()];
        let mut spans = Vec::new();
        for ph in placeholders {
            if ph.is_empty() {
                continue;
            }
            let mut search_from = 0;
            while let Some(rel) = self.buffer[search_from..].find(ph) {
                let start = search_from + rel;
                let end = start + ph.len();
                if (start..end).all(|i| !taken[i]) {
                    for slot in &mut taken[start..end] {
                        *slot = true;
                    }
                    spans.push(start..end);
                }
                search_from = start + ph.len();
                if search_from >= self.buffer.len() {
                    break;
                }
            }
        }
        spans.sort_by_key(|r| r.start);
        spans
    }

    /// Drop pending paste entries whose placeholder no longer appears anywhere in `buffer`.
    fn prune_pending_pastes(&mut self) {
        self.pending_pastes
            .retain(|p| self.buffer.contains(&p.placeholder));
    }

    /// Replace every active placeholder occurrence with its full pasted content.
    /// Longer placeholders are expanded first so an entry like
    /// `[Pasted Content 100 chars] #2` isn't clobbered by `[Pasted Content 100 chars]`.
    fn expand_pending_pastes(&self, mut text: String) -> String {
        let mut entries: Vec<&PendingPaste> = self.pending_pastes.iter().collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.placeholder.len()));
        for entry in entries {
            text = text.replace(&entry.placeholder, &entry.content);
        }
        text
    }

    fn attach_image(&mut self, path: PathBuf) {
        let n = self.attached_images.len() + 1;
        let label = format!("[Image #{n}] ");
        self.detach_history_nav();
        self.buffer.insert_str(self.cursor, &label);
        self.cursor += label.len();
        self.attached_images.push(path);
        self.preferred_col = None;
    }

    fn terminal_columns(&self) -> usize {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let fd = libc::STDOUT_FILENO;
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
            ws.ws_col as usize
        } else {
            80
        }
    }

    fn pending_bar_line(&self, cols: usize) -> Option<String> {
        let n = self.pending_followups.len();
        if n == 0 {
            return None;
        }
        let head = self.pending_followups.front()?.text.as_str();
        let prefix_plain = format!("↑ {n} queued — ");
        let pw = UnicodeWidthStr::width(prefix_plain.as_str());
        let budget = cols.saturating_sub(pw);
        let preview = truncate_preview_to_width(head, budget);
        Some(format!("\x1b[2m{prefix_plain}{preview}\x1b[0m"))
    }

    /// Drain every queued follow-up into the buffer at once (joined with newlines), replacing the current draft.
    fn pull_all_pending_into_buffer(&mut self) {
        if self.pending_followups.is_empty() {
            return;
        }
        self.detach_history_nav();
        let items: Vec<SubmittedLine> = self.pending_followups.drain(..).collect();
        let mut merged = String::new();
        let mut paths = Vec::new();
        let mut offset = 0usize;
        for (i, sub) in items.into_iter().enumerate() {
            if i > 0 {
                merged.push('\n');
            }
            merged.push_str(&renumber_pending_image_labels(&sub.text, offset));
            let n = sub.image_paths.len();
            paths.extend(sub.image_paths);
            offset += n;
        }
        self.buffer = merged;
        self.attached_images = paths;
        self.cursor = self.buffer.len();
        self.preferred_col = None;
    }

    fn build_clear_string(&self) -> String {
        if self.rendered_rows == 0 && self.rendered_pending_rows == 0 && !self.status_visible {
            return String::new();
        }
        let mut clear = String::new();
        let cursor_up = self.rendered_pending_rows + self.rendered_cursor.row;
        if cursor_up > 0 {
            clear.push_str(&format!("\x1b[{}A", cursor_up));
        }
        clear.push('\r');
        if self.status_visible {
            clear.push_str("\x1b[1A\r");
        }

        let total_rows =
            self.rendered_pending_rows + self.rendered_rows + usize::from(self.status_visible);
        for row in 0..total_rows {
            clear.push_str("\x1b[2K");
            if row + 1 < total_rows {
                clear.push_str("\x1b[1B\r");
            }
        }
        if total_rows > 1 {
            clear.push_str(&format!("\x1b[{}A", total_rows - 1));
        }
        clear.push('\r');
        clear
    }

    fn clear_prompt_and_status_inner(&mut self) {
        let clear = self.build_clear_string();
        if clear.is_empty() {
            return;
        }
        emit_raw(&clear);
        self.rendered_rows = 0;
        self.rendered_pending_rows = 0;
        self.rendered_cursor = CursorPos::default();
    }

    fn clear_rendered_prompt_inner(&mut self) {
        let had_status = self.status_visible;
        self.status_visible = false;
        self.clear_prompt_and_status_inner();
        self.status_visible = had_status;
    }

    fn redraw_inner(&mut self) {
        let cols = self.terminal_columns();
        let layout = self.compute_layout(cols);
        // Build clear + paint as one string so the terminal sees an atomic
        // frame. Emitting clear and paint as separate writes let some
        // terminals (Terminal.app during rapid bracketed-paste processing)
        // drop intermediate frames entirely.
        let had_status = self.status_visible;
        self.status_visible = false;
        let mut out = self.build_clear_string();
        self.status_visible = had_status;
        self.rendered_rows = 0;
        self.rendered_pending_rows = 0;
        self.rendered_cursor = CursorPos::default();

        let pending_bar = self.pending_bar_line(cols);
        let pending_rows = usize::from(pending_bar.is_some());
        let rows = self.wrapped_rows(cols);
        let placeholder_spans = self.placeholder_spans();
        if let Some(ref bar) = pending_bar {
            out.push('\r');
            out.push_str(bar);
            out.push_str("\r\n");
        }
        for (i, row) in rows.iter().enumerate() {
            if i == 0 {
                out.push_str(&format!("\r{}", PROMPT_PREFIX));
            } else {
                out.push_str("\r\n");
                if row.start > 0 && self.buffer.as_bytes()[row.start - 1] == b'\n' {
                    out.push_str(CONTINUATION_PREFIX);
                }
            }
            append_row_with_placeholders(&mut out, &self.buffer, row.clone(), &placeholder_spans);
        }
        out.push_str("\x1b[999D");
        if layout.end.row > 0 {
            out.push_str(&format!("\x1b[{}A", layout.end.row));
        }
        if layout.cursor.row > 0 {
            out.push_str(&format!("\x1b[{}B", layout.cursor.row));
        }
        if layout.cursor.col > 0 {
            out.push_str(&format!("\x1b[{}C", layout.cursor.col));
        }
        emit_raw(&out);
        self.rendered_rows = layout.rows;
        self.rendered_pending_rows = pending_rows;
        self.rendered_cursor = layout.cursor;
    }

    fn redraw(&mut self) {
        self.redraw_inner();
    }

    fn clear_display(&mut self) {
        self.clear_rendered_prompt_inner();
    }

    fn move_to_render_end(&mut self) {
        if self.cursor == self.buffer.len() {
            return;
        }
        let saved_cursor = self.cursor;
        self.cursor = self.buffer.len();
        self.redraw_inner();
        self.cursor = saved_cursor;
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.escape.clear();
        self.utf8.clear();
        self.history_index = None;
        self.history_draft = None;
        self.escape_started_at = None;
        self.preferred_col = None;
        self.status_visible = false;
        self.rendered_rows = 0;
        self.rendered_pending_rows = 0;
        self.rendered_cursor = CursorPos::default();
        self.paste_buffer = None;
        self.attached_images.clear();
        self.pending_pastes.clear();
        self.large_paste_counters.clear();
        self.paste_burst.clear_after_explicit_paste();
    }

    fn compute_layout(&self, cols: usize) -> RenderLayout {
        let rows = self.wrapped_rows(cols);
        let cursor_row = wrapped_row_index_by_start(&rows, self.cursor).unwrap_or(0);
        let end_row = wrapped_row_index_by_start(&rows, self.buffer.len()).unwrap_or(0);
        let cursor = CursorPos {
            row: cursor_row,
            col: self.display_col_for_position(&rows, cursor_row, self.cursor),
        };
        let end = CursorPos {
            row: end_row,
            col: self.display_col_for_position(&rows, end_row, self.buffer.len()),
        };
        RenderLayout {
            rows: rows.len().max(1),
            cursor,
            end,
        }
    }

    fn feed_byte(&mut self, byte: u8) -> LineEditResult {
        // Inside bracketed paste: accumulate raw bytes, watch for end marker
        if self.paste_buffer.is_some() && self.escape.is_empty() && byte != 0x1b {
            if byte == b'\r' {
                self.paste_buffer.as_mut().unwrap().push(b'\n');
            } else {
                self.paste_buffer.as_mut().unwrap().push(byte);
            }
            return LineEditResult::Continue;
        }

        if !self.escape.is_empty() {
            self.escape.push(byte);
            if self.try_handle_escape() {
                self.redraw();
            }
            return LineEditResult::Continue;
        }

        if !self.utf8.is_empty() {
            self.utf8.push(byte);
            match std::str::from_utf8(&self.utf8) {
                Ok(s) => {
                    if let Some(ch) = s.chars().next() {
                        self.utf8.clear();
                        self.handle_plain_char(ch, false);
                        self.redraw();
                    }
                }
                Err(err) if err.error_len().is_none() => {}
                Err(_) => {
                    self.utf8.clear();
                }
            }
            return LineEditResult::Continue;
        }

        match byte {
            b'\r' | b'\n' => {
                let now = std::time::Instant::now();
                if self.paste_burst.newline_should_insert_instead_of_submit(now) {
                    if !self.paste_burst.append_newline_if_active(now) {
                        self.handle_plain_char('\n', false);
                    }
                    self.redraw();
                    return LineEditResult::Continue;
                }
                if let Some(flushed) = self.paste_burst.flush_before_modified_input() {
                    self.insert_pasted_text(&flushed);
                }
                self.move_to_render_end();
                emit_raw("\r\n");
                let text = self.expand_pending_pastes(self.buffer.clone());
                let image_paths = std::mem::take(&mut self.attached_images);
                self.reset();
                LineEditResult::Submit(SubmittedLine { text, image_paths })
            }
            0x04 => {
                self.flush_paste_burst_as_typed();
                if self.buffer.is_empty() {
                    self.clear_rendered_prompt_inner();
                    emit_raw("\r\n");
                    self.reset();
                    LineEditResult::Eof
                } else {
                    self.delete_at_cursor();
                    self.redraw();
                    LineEditResult::Continue
                }
            }
            0x03 => {
                // Ctrl+C: exit REPL (like /quit)
                self.cancel_line();
                self.redraw();
                LineEditResult::Eof
            }
            0x01 => {
                self.flush_paste_burst_as_typed();
                let bol = self.beginning_of_current_line();
                if self.cursor == bol && bol > 0 {
                    // Already at BOL: jump to previous line's BOL
                    self.cursor = self.buffer[..bol - 1]
                        .rfind('\n')
                        .map(|i| i + 1)
                        .unwrap_or(0);
                } else {
                    self.cursor = bol;
                }
                self.preferred_col = None;
                self.redraw();
                LineEditResult::Continue
            }
            0x05 => {
                self.flush_paste_burst_as_typed();
                let eol = self.end_of_current_line();
                if self.cursor == eol && eol < self.buffer.len() {
                    // Already at EOL: jump to next line's EOL
                    self.cursor = self.buffer[eol + 1..]
                        .find('\n')
                        .map(|i| eol + 1 + i)
                        .unwrap_or(self.buffer.len());
                } else {
                    self.cursor = eol;
                }
                self.preferred_col = None;
                self.redraw();
                LineEditResult::Continue
            }
            0x15 => {
                self.flush_paste_burst_as_typed();
                self.kill_to_start();
                self.redraw();
                LineEditResult::Continue
            }
            0x0b => {
                self.flush_paste_burst_as_typed();
                self.kill_to_end();
                self.redraw();
                LineEditResult::Continue
            }
            0x17 => {
                self.flush_paste_burst_as_typed();
                self.delete_backward_word();
                self.redraw();
                LineEditResult::Continue
            }
            0x19 => {
                self.flush_paste_burst_as_typed();
                self.yank();
                self.redraw();
                LineEditResult::Continue
            }
            0x02 => {
                self.flush_paste_burst_as_typed();
                self.move_left();
                self.redraw();
                LineEditResult::Continue
            }
            0x06 => {
                self.flush_paste_burst_as_typed();
                self.move_right();
                self.redraw();
                LineEditResult::Continue
            }
            0x10 => {
                self.flush_paste_burst_as_typed();
                self.history_prev();
                self.redraw();
                LineEditResult::Continue
            }
            0x0e => {
                self.flush_paste_burst_as_typed();
                self.history_next();
                self.redraw();
                LineEditResult::Continue
            }
            0x7f | 0x08 => {
                self.flush_paste_burst_as_typed();
                self.backspace();
                self.redraw();
                LineEditResult::Continue
            }
            0x1b => {
                self.flush_paste_burst_as_typed();
                self.escape.push(byte);
                self.escape_started_at = Some(std::time::Instant::now());
                LineEditResult::Continue
            }
            byte if byte.is_ascii_control() => LineEditResult::Continue,
            byte if byte.is_ascii() => {
                self.handle_plain_char(byte as char, true);
                self.redraw();
                LineEditResult::Continue
            }
            _ => {
                self.utf8.push(byte);
                LineEditResult::Continue
            }
        }
    }

    fn try_handle_escape(&mut self) -> bool {
        // During paste, only recognize the end marker — dump everything else
        if self.paste_buffer.is_some() {
            return match self.escape.as_slice() {
                [0x1b, b'[', b'2', b'0', b'1', b'~'] => {
                    if let Some(raw) = self.paste_buffer.take() {
                        let text = String::from_utf8_lossy(&raw);
                        if !text.is_empty() {
                            self.insert_pasted_text(text.trim_end_matches('\0'));
                        }
                    }
                    self.escape.clear();
                    self.escape_started_at = None;
                    true
                }
                [0x1b]
                | [0x1b, b'[']
                | [0x1b, b'[', b'2']
                | [0x1b, b'[', b'2', b'0']
                | [0x1b, b'[', b'2', b'0', b'1'] => false,
                _ => {
                    // Not the end marker — dump escape bytes into paste buffer
                    self.paste_buffer
                        .as_mut()
                        .unwrap()
                        .extend_from_slice(&self.escape);
                    self.escape.clear();
                    self.escape_started_at = None;
                    false
                }
            };
        }

        match self.escape.as_slice() {
            [0x1b, b'[', b'D'] => {
                self.move_left();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'C'] => {
                self.move_right();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'A'] => {
                self.move_up();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'B'] => {
                self.move_down();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'H'] | [0x1b, b'O', b'H'] => {
                self.cursor = self.beginning_of_current_line();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'F'] | [0x1b, b'O', b'F'] => {
                self.cursor = self.end_of_current_line();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'3', b'~'] => {
                self.delete_at_cursor();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Alt+Enter: insert newline
            [0x1b, b'\r'] => {
                self.insert_char('\n');
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Alt+B: word backward
            [0x1b, b'b'] => {
                self.cursor = self.beginning_of_previous_word();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Alt+F: word forward
            [0x1b, b'f'] => {
                self.cursor = self.end_of_next_word();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Alt+D: delete forward word
            [0x1b, b'd'] => {
                self.delete_forward_word();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Alt+Backspace: delete backward word
            [0x1b, 0x7f] => {
                self.delete_backward_word();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Ctrl+Left / Option+Left: word backward
            [0x1b, b'[', b'1', b';', b'5' | b'3', b'D'] => {
                self.cursor = self.beginning_of_previous_word();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Ctrl+Right / Option+Right: word forward
            [0x1b, b'[', b'1', b';', b'5' | b'3', b'C'] => {
                self.cursor = self.end_of_next_word();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Cmd+Left / Shift+Left: beginning of line
            [0x1b, b'[', b'1', b';', b'9' | b'2', b'D'] => {
                self.cursor = self.beginning_of_current_line();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Cmd+Right / Shift+Right: end of line
            [0x1b, b'[', b'1', b';', b'9' | b'2', b'C'] => {
                self.cursor = self.end_of_current_line();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Option+Delete: delete forward word
            [0x1b, b'[', b'3', b';', b'3', b'~'] => {
                self.delete_forward_word();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Bracketed paste begin
            [0x1b, b'[', b'2', b'0', b'0', b'~'] => {
                self.paste_buffer = Some(Vec::new());
                self.escape.clear();
                self.escape_started_at = None;
                false // no redraw needed
            }
            [0x1b]
            | [0x1b, b'[']
            | [0x1b, b'O']
            | [0x1b, b'[', b'3']
            | [0x1b, b'[', b'3', b';']
            | [0x1b, b'[', b'3', b';', b'3']
            | [0x1b, b'[', b'1']
            | [0x1b, b'[', b'1', b';']
            | [0x1b, b'[', b'1', b';', b'2' | b'3' | b'5' | b'9']
            | [0x1b, b'[', b'2']
            | [0x1b, b'[', b'2', b'0']
            | [0x1b, b'[', b'2', b'0', b'0'] => false,
            _ => {
                self.escape.clear();
                self.escape_started_at = None;
                false
            }
        }
    }

    fn maybe_resolve_pending_escape(&mut self) -> bool {
        if self.escape.as_slice() != [0x1b] {
            return false;
        }
        if self.paste_buffer.is_some() {
            // Lone ESC inside paste — dump to paste buffer, don't cancel
            self.paste_buffer.as_mut().unwrap().push(0x1b);
            self.escape.clear();
            self.escape_started_at = None;
            return false;
        }
        let Some(started) = self.escape_started_at else {
            return false;
        };
        if started.elapsed() < ESC_TIMEOUT {
            return false;
        }
        self.cancel_line();
        self.redraw();
        true
    }

    fn insert_char(&mut self, ch: char) {
        self.detach_history_nav();
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.preferred_col = None;
    }

    /// Route a printable char through the paste-burst detector.
    ///
    /// For bracketed-paste-less terminals (Apple Terminal, Windows consoles),
    /// drag-and-drop and clipboard pastes arrive as rapid sequential chars.
    /// The detector holds the first char, watches timing, and flushes the
    /// accumulated burst as a single paste via `flush_paste_burst_if_due`.
    ///
    /// When `allow_hold` is false, we skip the pending-first-char path
    /// (used from UTF-8 completion where we already committed to a char).
    fn handle_plain_char(&mut self, ch: char, allow_hold: bool) {
        let now = std::time::Instant::now();
        let decision = if allow_hold {
            self.paste_burst.on_plain_char(ch, now)
        } else if let Some(d) = self.paste_burst.on_plain_char_no_hold(now) {
            d
        } else {
            self.insert_char(ch);
            return;
        };
        match decision {
            CharDecision::RetainFirstChar => {
                // Held in burst; flush_if_due will surface it as Typed(ch).
            }
            CharDecision::BeginBufferFromPending => {
                self.paste_burst.append_char_to_buffer(ch, now);
            }
            CharDecision::BufferAppend => {
                self.paste_burst.append_char_to_buffer(ch, now);
            }
            CharDecision::BeginBuffer { retro_chars } => {
                let before = self.buffer[..self.cursor].to_string();
                if let Some(grab) =
                    self.paste_burst
                        .decide_begin_buffer(now, &before, retro_chars as usize)
                {
                    self.detach_history_nav();
                    self.buffer.drain(grab.start_byte..self.cursor);
                    self.cursor = grab.start_byte;
                    self.paste_burst.append_char_to_buffer(ch, now);
                    self.preferred_col = None;
                } else {
                    self.insert_char(ch);
                }
            }
        }
    }

    /// Flush any pending held char or accumulated burst as normal typing,
    /// before a non-char key (arrow, backspace, Ctrl-*) interrupts the burst.
    fn flush_paste_burst_as_typed(&mut self) {
        if let Some(flushed) = self.paste_burst.flush_before_modified_input() {
            self.insert_pasted_text(&flushed);
        }
        self.paste_burst.clear_window_after_non_char();
    }

    /// Called from the input poll idle tick. If the burst detector has timed
    /// out, emit the accumulated bytes as either a single typed char or a
    /// paste (which may be recognized as an image path and attached). A
    /// trailing newline in the flushed burst means the user pressed Enter
    /// while the burst was still accumulating — submit the line after
    /// attaching so drag-and-drop → Enter works as expected.
    pub(super) fn flush_paste_burst_if_due(&mut self) {
        let now = std::time::Instant::now();
        match self.paste_burst.flush_if_due(now) {
            FlushResult::None => {}
            FlushResult::Typed(ch) => {
                self.insert_char(ch);
                self.redraw();
            }
            FlushResult::Paste(s) => {
                let (body, submit) = match s.strip_suffix('\n') {
                    Some(rest) => (rest.to_string(), true),
                    None => (s, false),
                };
                self.insert_pasted_text(&body);
                if submit {
                    self.submit_current_line();
                } else {
                    self.redraw();
                }
            }
        }
    }

    fn submit_current_line(&mut self) {
        self.move_to_render_end();
        emit_raw("\r\n");
        let text = self.expand_pending_pastes(self.buffer.clone());
        let image_paths = std::mem::take(&mut self.attached_images);
        self.reset();
        self.pending_submits
            .push_back(SubmittedLine { text, image_paths });
        self.redraw_inner();
    }

    pub(super) fn paste_burst_is_active(&self) -> bool {
        self.paste_burst.is_active()
    }

    /// Test-only: force the paste-burst detector to flush immediately,
    /// bypassing the idle-time wait. Used by tests that synchronously feed
    /// bytes without natural time spacing.
    #[cfg(test)]
    pub(super) fn force_flush_paste_burst(&mut self) {
        match self.paste_burst.force_flush() {
            FlushResult::None => {}
            FlushResult::Typed(ch) => {
                self.insert_char(ch);
            }
            FlushResult::Paste(s) => {
                self.insert_pasted_text(&s);
            }
        }
    }

    fn move_left(&mut self) {
        self.cursor = prev_grapheme_boundary(&self.buffer, self.cursor);
        self.preferred_col = None;
    }

    fn move_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.buffer, self.cursor);
        self.preferred_col = None;
    }

    fn move_up(&mut self) {
        self.move_up_for_cols(self.terminal_columns());
    }

    fn move_up_for_cols(&mut self, cols: usize) {
        let rows = self.wrapped_rows(cols);
        let Some(row_idx) = wrapped_row_index_by_start(&rows, self.cursor) else {
            return;
        };
        if row_idx == 0 {
            if !self.pending_followups.is_empty() {
                self.pull_all_pending_into_buffer();
            } else {
                self.history_prev();
            }
            return;
        }
        let target_col = self
            .preferred_col
            .unwrap_or_else(|| self.display_col_for_position(&rows, row_idx, self.cursor));
        self.move_to_display_col_on_row(&rows, row_idx - 1, target_col);
        self.preferred_col = Some(target_col);
    }

    fn move_down(&mut self) {
        self.move_down_for_cols(self.terminal_columns());
    }

    fn move_down_for_cols(&mut self, cols: usize) {
        let rows = self.wrapped_rows(cols);
        let Some(row_idx) = wrapped_row_index_by_start(&rows, self.cursor) else {
            return;
        };
        if row_idx + 1 >= rows.len() {
            self.history_next();
            return;
        }
        let target_col = self
            .preferred_col
            .unwrap_or_else(|| self.display_col_for_position(&rows, row_idx, self.cursor));
        self.move_to_display_col_on_row(&rows, row_idx + 1, target_col);
        self.preferred_col = Some(target_col);
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.detach_history_nav();
        if let Some(span) = self
            .placeholder_spans()
            .into_iter()
            .find(|r| r.end == self.cursor)
        {
            self.buffer.drain(span.clone());
            self.cursor = span.start;
            self.preferred_col = None;
            self.prune_pending_pastes();
            return;
        }
        let prev = prev_grapheme_boundary(&self.buffer, self.cursor);
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
        self.preferred_col = None;
    }

    fn delete_at_cursor(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        self.detach_history_nav();
        if let Some(span) = self
            .placeholder_spans()
            .into_iter()
            .find(|r| r.start == self.cursor)
        {
            self.buffer.drain(span);
            self.preferred_col = None;
            self.prune_pending_pastes();
            return;
        }
        let end = next_grapheme_boundary(&self.buffer, self.cursor);
        self.buffer.drain(self.cursor..end);
        self.preferred_col = None;
    }

    fn beginning_of_current_line(&self) -> usize {
        self.buffer[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    fn end_of_current_line(&self) -> usize {
        self.buffer[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.buffer.len())
    }

    fn beginning_of_previous_word(&self) -> usize {
        let prefix = &self.buffer[..self.cursor];
        let Some((first_non_ws_idx, ch)) = prefix
            .char_indices()
            .rev()
            .find(|&(_, ch)| !ch.is_whitespace())
        else {
            return 0;
        };
        let is_sep = is_word_separator(ch);
        let mut start = first_non_ws_idx;
        for (idx, ch) in prefix[..first_non_ws_idx].char_indices().rev() {
            if ch.is_whitespace() || is_word_separator(ch) != is_sep {
                start = idx + ch.len_utf8();
                break;
            }
            start = idx;
        }
        start
    }

    fn end_of_next_word(&self) -> usize {
        let suffix = &self.buffer[self.cursor..];
        let Some(first_non_ws) = suffix.find(|c: char| !c.is_whitespace()) else {
            return self.buffer.len();
        };
        let word_start = self.cursor + first_non_ws;
        let mut iter = self.buffer[word_start..].char_indices();
        let Some((_, first_ch)) = iter.next() else {
            return word_start;
        };
        let is_sep = is_word_separator(first_ch);
        let mut end = self.buffer.len();
        for (idx, ch) in iter {
            if ch.is_whitespace() || is_word_separator(ch) != is_sep {
                end = word_start + idx;
                break;
            }
        }
        end
    }

    fn kill_range(&mut self, range: std::ops::Range<usize>) {
        if range.start >= range.end {
            return;
        }
        self.detach_history_nav();
        self.kill_buffer = self.buffer[range.clone()].to_string();
        self.cursor = range.start;
        self.buffer.drain(range);
        self.preferred_col = None;
        self.prune_pending_pastes();
    }

    fn delete_backward_word(&mut self) {
        let start = self.beginning_of_previous_word();
        self.kill_range(start..self.cursor);
    }

    fn delete_forward_word(&mut self) {
        let end = self.end_of_next_word();
        if end > self.cursor {
            self.kill_range(self.cursor..end);
        }
    }

    fn kill_to_start(&mut self) {
        let bol = self.beginning_of_current_line();
        if self.cursor == bol {
            // Already at BOL: kill the preceding newline if any
            if bol > 0 {
                self.kill_range(bol - 1..bol);
            }
        } else {
            self.kill_range(bol..self.cursor);
        }
    }

    fn kill_to_end(&mut self) {
        let eol = self.end_of_current_line();
        if self.cursor == eol {
            // Already at EOL: kill the trailing newline if any
            if eol < self.buffer.len() {
                self.kill_range(self.cursor..eol + 1);
            }
        } else {
            self.kill_range(self.cursor..eol);
        }
    }

    fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        self.detach_history_nav();
        let insert = self.kill_buffer.clone();
        self.buffer.insert_str(self.cursor, &insert);
        self.cursor += insert.len();
        self.preferred_col = None;
    }

    fn cancel_line(&mut self) {
        // Clear the currently rendered prompt before resetting bookkeeping.
        // Otherwise ESC/Ctrl-C can drop the old text from editor state while
        // leaving stale characters visible until the next redraw.
        self.clear_prompt_and_status_inner();
        self.buffer.clear();
        self.cursor = 0;
        self.escape.clear();
        self.utf8.clear();
        self.history_index = None;
        self.history_draft = None;
        self.escape_started_at = None;
        self.preferred_col = None;
        self.status_visible = false;
        self.rendered_rows = 0;
        self.rendered_pending_rows = 0;
        self.rendered_cursor = CursorPos::default();
        self.paste_buffer = None;
        self.attached_images.clear();
        self.pending_pastes.clear();
        self.large_paste_counters.clear();
        self.paste_burst.clear_after_explicit_paste();
    }

    fn detach_history_nav(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.history_draft = None;
        }
    }

    pub(super) fn push_history(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|prev| prev == line) {
            return;
        }
        self.history.push(line.to_string());
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.history_draft = Some(self.buffer.clone());
                self.history_index = Some(self.history.len() - 1);
            }
            Some(0) => {}
            Some(idx) => {
                self.history_index = Some(idx - 1);
            }
        }
        self.load_history_selection();
    }

    fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(idx) if idx + 1 < self.history.len() => {
                self.history_index = Some(idx + 1);
                self.load_history_selection();
            }
            Some(_) => {
                self.history_index = None;
                self.buffer = self.history_draft.take().unwrap_or_default();
                self.cursor = self.buffer.len();
            }
        }
    }

    fn load_history_selection(&mut self) {
        if let Some(idx) = self.history_index {
            self.buffer = self.history[idx].clone();
            self.cursor = self.buffer.len();
            self.preferred_col = None;
        }
    }

    fn wrapped_rows(&self, cols: usize) -> Vec<std::ops::Range<usize>> {
        let cols = cols.max(1);
        let prompt_w = UnicodeWidthStr::width(PROMPT_VISIBLE).min(cols);
        let cont_w = UnicodeWidthStr::width(CONTINUATION_VISIBLE).min(cols);
        let mut rows = Vec::new();
        let mut row_start = 0usize;
        let mut used = prompt_w;
        for (idx, grapheme) in self.buffer.grapheme_indices(true) {
            if grapheme == "\n" {
                rows.push(row_start..idx);
                row_start = idx + 1;
                used = cont_w;
                continue;
            }
            let width = UnicodeWidthStr::width(grapheme);
            if used + width > cols && row_start < idx {
                rows.push(row_start..idx);
                row_start = idx;
                used = 0;
            }
            used += width;
        }
        rows.push(row_start..self.buffer.len());
        rows
    }

    fn row_prefix_width(&self, rows: &[std::ops::Range<usize>], row_idx: usize) -> usize {
        if row_idx == 0 {
            UnicodeWidthStr::width(PROMPT_VISIBLE)
        } else if rows[row_idx].start > 0
            && self.buffer.as_bytes()[rows[row_idx].start - 1] == b'\n'
        {
            UnicodeWidthStr::width(CONTINUATION_VISIBLE)
        } else {
            0
        }
    }

    fn display_col_for_position(
        &self,
        rows: &[std::ops::Range<usize>],
        row_idx: usize,
        pos: usize,
    ) -> usize {
        let row = &rows[row_idx];
        self.row_prefix_width(rows, row_idx) + UnicodeWidthStr::width(&self.buffer[row.start..pos])
    }

    fn move_to_display_col_on_row(
        &mut self,
        rows: &[std::ops::Range<usize>],
        row_idx: usize,
        target_col: usize,
    ) {
        let row = &rows[row_idx];
        let mut width_so_far = self.row_prefix_width(rows, row_idx);
        for (offset, grapheme) in self.buffer[row.start..row.end].grapheme_indices(true) {
            width_so_far += UnicodeWidthStr::width(grapheme);
            if width_so_far > target_col {
                self.cursor = row.start + offset;
                return;
            }
        }
        self.cursor = row.end;
    }
}

/// A dropped path counts as an image if it has a common image extension.
/// We don't verify readability here — `image::image_dimensions` only supports
/// the png/jpeg features we build in, and macOS screenshot-thumbnail drags
/// deliver temp paths that may be cleaned up between drop and read. The
/// user-turn image loader will surface a real error if the file is gone.
fn looks_like_image_file(path: &std::path::Path) -> bool {
    const IMAGE_EXTS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "bmp", "tiff", "tif", "avif",
    ];
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

fn wrapped_row_index_by_start(rows: &[std::ops::Range<usize>], pos: usize) -> Option<usize> {
    let idx = rows.partition_point(|range| range.start <= pos);
    if idx == 0 { None } else { Some(idx - 1) }
}

fn prev_grapheme_boundary(text: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    text[..pos]
        .grapheme_indices(true)
        .next_back()
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, pos: usize) -> usize {
    if pos >= text.len() {
        return text.len();
    }
    text[pos..]
        .graphemes(true)
        .next()
        .map(|g| pos + g.len())
        .unwrap_or(text.len())
}

pub(super) fn read_input_or_bus(
    bus_name: &str,
    editor: &mut LineEditor,
    tunnel_fd: Option<i32>,
) -> InputEvent {
    editor.redraw();

    let raw_mode = RawModeGuard::enter().ok();
    if raw_mode.is_none() && tunnel_fd.is_none() {
        let mut line_buf = String::new();
        match io::stdin().lock().read_line(&mut line_buf) {
            Ok(0) => return InputEvent::Eof,
            Ok(_) => {
                return InputEvent::User(SubmittedLine {
                    text: line_buf.trim_end_matches('\n').to_string(),
                    image_paths: Vec::new(),
                });
            }
            Err(_) => return InputEvent::Eof,
        }
    }
    let _raw_mode = raw_mode;
    let stdin_fd = _raw_mode.as_ref().map(|_| libc::STDIN_FILENO);

    let mut buf = [0u8; 64];

    loop {
        editor.flush_paste_burst_if_due();
        if let Some(line) = editor.take_next_pending_submit() {
            return InputEvent::User(line);
        }

        if broker::has_pending_messages(bus_name) {
            editor.clear_display();
            return InputEvent::Bus;
        }

        unsafe {
            let mut fds_arr = build_input_pollfds(stdin_fd, tunnel_fd);
            if fds_arr.is_empty() {
                return InputEvent::Eof;
            }

            let poll_timeout_ms = if editor.paste_burst_is_active() { 10 } else { 100 };
            let ready = libc::poll(
                fds_arr.as_mut_ptr(),
                fds_arr.len() as libc::nfds_t,
                poll_timeout_ms,
            );
            if ready > 0 {
                for pollfd in fds_arr.iter() {
                    if (pollfd.revents & libc::POLLIN) == 0 {
                        continue;
                    }
                    if tunnel_fd.is_some_and(|fd| fd == pollfd.fd) {
                        match libc::read(
                            pollfd.fd,
                            buf.as_mut_ptr() as *mut libc::c_void,
                            buf.len(),
                        ) {
                            n if n > 0 => {
                                if editor
                                    .process_input_bytes(&buf[..n as usize], |ed, line| {
                                        ed.pending_submits.push_back(line);
                                        ed.redraw_inner();
                                    })
                                    .is_err()
                                {
                                    return InputEvent::Eof;
                                }
                                if let Some(line) = editor.take_next_pending_submit() {
                                    return InputEvent::User(line);
                                }
                            }
                            _ => {}
                        }
                    } else if stdin_fd.is_some_and(|fd| fd == pollfd.fd) {
                        match io::stdin().read(&mut buf) {
                            Ok(0) => return InputEvent::Eof,
                            Ok(n) => {
                                if editor
                                    .process_input_bytes(&buf[..n], |ed, line| {
                                        ed.pending_submits.push_back(line);
                                        ed.redraw_inner();
                                    })
                                    .is_err()
                                {
                                    return InputEvent::Eof;
                                }
                                if let Some(line) = editor.take_next_pending_submit() {
                                    return InputEvent::User(line);
                                }
                            }
                            Err(_) => return InputEvent::Eof,
                        }
                    }
                }
            } else if ready == 0 {
                let _ = editor.maybe_resolve_pending_escape();
                editor.flush_paste_burst_if_due();
                if let Some(line) = editor.take_next_pending_submit() {
                    return InputEvent::User(line);
                }
            } else if ready < 0 {
                continue;
            }
        }
    }
}

pub(super) fn print_banner(model: Option<&str>, credential: Option<&str>) {
    println!("\x1b[1;36mSidekar REPL\x1b[0m");
    let line2 = match (model, credential) {
        (Some(m), Some(c)) => format!(
            "\x1b[36mmodel\x1b[0m {m}  \x1b[36mcredential\x1b[0m {c}  \x1b[2m/help · /quit · ↑ pulls queued input\x1b[0m"
        ),
        (Some(m), None) => format!(
            "\x1b[36mmodel\x1b[0m {m}  \x1b[2m/credential <name> · /help · ↑ pulls queued input\x1b[0m"
        ),
        (None, Some(c)) => format!(
            "\x1b[36mcredential\x1b[0m {c}  \x1b[2m/model <name> · ↑ pulls queued input\x1b[0m"
        ),
        (None, None) => {
            "\x1b[2m/credential + /model to start · /help · ↑ pulls queued input\x1b[0m".to_string()
        }
    };
    println!("{line2}");
    println!();
}

mod paste_burst;
use paste_burst::{CharDecision, FlushResult, PasteBurst};

#[cfg(test)]
mod tests;
