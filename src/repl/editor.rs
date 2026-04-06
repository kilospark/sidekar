use anyhow::Result;
use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::broker;

use super::InputEvent;

const PROMPT_PREFIX: &str = "\x1b[36m›\x1b[0m ";
const PROMPT_VISIBLE: &str = "› ";
const CONTINUATION_PREFIX: &str = "\x1b[2m·\x1b[0m ";
const CONTINUATION_VISIBLE: &str = "· ";
const ESC_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(75);

type SharedEditor = Arc<Mutex<LineEditor>>;

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
        editor.clear_prompt_and_status_inner();
        emit_raw("\r\x1b[2K");
        emit_raw(text);
        emit_raw("\n");
        editor.status_visible = true;
        editor.redraw_inner();
    })
    .is_none()
    {
        emit_raw(text);
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
                    let mut fds = [
                        libc::pollfd {
                            fd: local_fd.unwrap_or(-1),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: tunnel_fd.unwrap_or(-1),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    let nfds = match (local_fd.is_some(), tunnel_fd.is_some()) {
                        (true, true) => 2,
                        (true, false) | (false, true) => 1,
                        (false, false) => 0,
                    };
                    if nfds == 0 {
                        break;
                    }
                    let ready = unsafe { libc::poll(fds.as_mut_ptr(), nfds, 50) };
                    let now = std::time::Instant::now();
                    if ready > 0 {
                        for pollfd in fds.iter().take(nfds as usize) {
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
                                let _ = detector.feed_bytes(&buf[..n as usize], now);
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
    submitted_rx: mpsc::Receiver<String>,
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
        let local_fd = if raw_mode.is_some() {
            Some(libc::STDIN_FILENO)
        } else {
            None
        };
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_thread = running.clone();
        let editor_thread = editor.clone();
        let (submitted_tx, submitted_rx) = mpsc::channel();

        let handle = if local_fd.is_none() && tunnel_fd.is_none() {
            None
        } else {
            Some(std::thread::spawn(move || {
                let mut detector = EscDetector::new();
                let mut buf = [0u8; 64];
                while running_thread.load(std::sync::atomic::Ordering::Relaxed)
                    && !cancel.load(std::sync::atomic::Ordering::Relaxed)
                {
                    let mut fds = [
                        libc::pollfd {
                            fd: local_fd.unwrap_or(-1),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: tunnel_fd.unwrap_or(-1),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    let nfds = match (local_fd.is_some(), tunnel_fd.is_some()) {
                        (true, true) => 2,
                        (true, false) | (false, true) => 1,
                        (false, false) => 0,
                    };
                    if nfds == 0 {
                        break;
                    }
                    let ready = unsafe { libc::poll(fds.as_mut_ptr(), nfds, 50) };
                    let now = std::time::Instant::now();
                    if ready > 0 {
                        for pollfd in fds.iter().take(nfds as usize) {
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
                            let forwarded = detector.feed_bytes(&buf[..n as usize], now);
                            if forwarded.is_empty() {
                                continue;
                            }
                            if let Ok(mut editor) = editor_thread.lock() {
                                match editor.feed_bytes(&forwarded) {
                                    LineEditResult::Continue => {}
                                    LineEditResult::Submit(line) => {
                                        let _ = submitted_tx.send(line);
                                        editor.redraw_inner();
                                    }
                                    LineEditResult::Eof => {
                                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                                        return;
                                    }
                                }
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
            editor: Some(editor),
            running,
            handle,
            raw_mode,
            submitted_rx,
        }
    }

    pub(super) fn finish(mut self) -> (LineEditor, VecDeque<String>) {
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
        let editor = Arc::try_unwrap(editor_arc)
            .ok()
            .and_then(|mutex| mutex.into_inner().ok())
            .unwrap_or_default();

        let mut submitted = VecDeque::new();
        while let Ok(line) = self.submitted_rx.try_recv() {
            submitted.push_back(line);
        }
        (editor, submitted)
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

enum LineEditResult {
    Continue,
    Submit(String),
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
        }
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

    fn clear_prompt_and_status_inner(&mut self) {
        if self.rendered_rows == 0 && !self.status_visible {
            return;
        }
        let mut clear = String::new();
        if self.rendered_cursor.row > 0 {
            clear.push_str(&format!("\x1b[{}A", self.rendered_cursor.row));
        }
        clear.push('\r');
        if self.status_visible {
            clear.push_str("\x1b[1A\r");
        }

        let total_rows = self.rendered_rows + usize::from(self.status_visible);
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
        emit_raw(&clear);
        self.rendered_rows = 0;
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
        self.clear_rendered_prompt_inner();

        let rows = self.wrapped_rows(cols);
        let mut out = String::new();
        for (i, row) in rows.iter().enumerate() {
            if i == 0 {
                out.push_str(&format!("\r{}", PROMPT_PREFIX));
            } else {
                out.push_str("\r\n");
                if row.start > 0 && self.buffer.as_bytes()[row.start - 1] == b'\n' {
                    out.push_str(CONTINUATION_PREFIX);
                }
            }
            out.push_str(&self.buffer[row.start..row.end]);
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
        self.rendered_cursor = CursorPos::default();
        self.paste_buffer = None;
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

    fn feed_bytes(&mut self, bytes: &[u8]) -> LineEditResult {
        for &byte in bytes {
            let result = self.feed_byte(byte);
            if !matches!(result, LineEditResult::Continue) {
                return result;
            }
        }
        LineEditResult::Continue
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
                        self.insert_char(ch);
                        self.utf8.clear();
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
                self.move_to_render_end();
                emit_raw("\r\n");
                let submitted = self.buffer.clone();
                self.record_submission(&submitted);
                self.reset();
                LineEditResult::Submit(submitted)
            }
            0x04 => {
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
                self.cancel_line();
                self.redraw();
                LineEditResult::Continue
            }
            0x01 => {
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
                self.kill_to_start();
                self.redraw();
                LineEditResult::Continue
            }
            0x0b => {
                self.kill_to_end();
                self.redraw();
                LineEditResult::Continue
            }
            0x17 => {
                self.delete_backward_word();
                self.redraw();
                LineEditResult::Continue
            }
            0x19 => {
                self.yank();
                self.redraw();
                LineEditResult::Continue
            }
            0x02 => {
                self.move_left();
                self.redraw();
                LineEditResult::Continue
            }
            0x06 => {
                self.move_right();
                self.redraw();
                LineEditResult::Continue
            }
            0x10 => {
                self.history_prev();
                self.redraw();
                LineEditResult::Continue
            }
            0x0e => {
                self.history_next();
                self.redraw();
                LineEditResult::Continue
            }
            0x7f | 0x08 => {
                self.backspace();
                self.redraw();
                LineEditResult::Continue
            }
            0x1b => {
                self.escape.push(byte);
                self.escape_started_at = Some(std::time::Instant::now());
                LineEditResult::Continue
            }
            byte if byte.is_ascii_control() => LineEditResult::Continue,
            byte if byte.is_ascii() => {
                self.insert_char(byte as char);
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
                            self.detach_history_nav();
                            self.buffer.insert_str(self.cursor, &text);
                            self.cursor += text.len();
                            self.preferred_col = None;
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
            // Ctrl+Left: word backward
            [0x1b, b'[', b'1', b';', b'5', b'D'] => {
                self.cursor = self.beginning_of_previous_word();
                self.preferred_col = None;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            // Ctrl+Right: word forward
            [0x1b, b'[', b'1', b';', b'5', b'C'] => {
                self.cursor = self.end_of_next_word();
                self.preferred_col = None;
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
            | [0x1b, b'[', b'1']
            | [0x1b, b'[', b'1', b';']
            | [0x1b, b'[', b'1', b';', b'5']
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
            self.history_prev();
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
        let Some((first_non_ws_idx, ch)) =
            prefix.char_indices().rev().find(|&(_, ch)| !ch.is_whitespace())
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
        self.rendered_cursor = CursorPos::default();
        self.paste_buffer = None;
    }

    fn detach_history_nav(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.history_draft = None;
        }
    }

    fn record_submission(&mut self, line: &str) {
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
        self.row_prefix_width(rows, row_idx)
            + UnicodeWidthStr::width(&self.buffer[row.start..pos])
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
        .last()
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

    let _raw_mode = match RawModeGuard::enter() {
        Ok(guard) => guard,
        Err(_) => {
            let mut line_buf = String::new();
            match io::stdin().lock().read_line(&mut line_buf) {
                Ok(0) => return InputEvent::Eof,
                Ok(_) => return InputEvent::User(line_buf.trim_end_matches('\n').to_string()),
                Err(_) => return InputEvent::Eof,
            }
        }
    };

    let mut buf = [0u8; 64];

    loop {
        if broker::has_pending_messages(bus_name) {
            editor.clear_display();
            return InputEvent::Bus;
        }

        unsafe {
            let mut fds_arr = [
                libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: tunnel_fd.unwrap_or(-1),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let nfds = if tunnel_fd.is_some() { 2 } else { 1 };

            let ready = libc::poll(fds_arr.as_mut_ptr(), nfds, 100);
            if ready > 0 {
                if nfds > 1 && (fds_arr[1].revents & libc::POLLIN) != 0 {
                    match libc::read(
                        fds_arr[1].fd,
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    ) {
                        n if n > 0 => match editor.feed_bytes(&buf[..n as usize]) {
                            LineEditResult::Continue => {}
                            LineEditResult::Submit(line) => return InputEvent::User(line),
                            LineEditResult::Eof => return InputEvent::Eof,
                        },
                        _ => {}
                    }
                }
                if (fds_arr[0].revents & libc::POLLIN) != 0 {
                    match io::stdin().read(&mut buf) {
                        Ok(0) => return InputEvent::Eof,
                        Ok(n) => match editor.feed_bytes(&buf[..n]) {
                            LineEditResult::Continue => {}
                            LineEditResult::Submit(line) => return InputEvent::User(line),
                            LineEditResult::Eof => return InputEvent::Eof,
                        },
                        Err(_) => return InputEvent::Eof,
                    }
                }
            } else if ready == 0 {
                let _ = editor.maybe_resolve_pending_escape();
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
            "\x1b[36mmodel\x1b[0m {m}  \x1b[36mcredential\x1b[0m {c}  \x1b[2m/help commands · /quit exit\x1b[0m"
        ),
        (Some(m), None) => format!(
            "\x1b[36mmodel\x1b[0m {m}  \x1b[2m/credential <name> to get started · /help for commands\x1b[0m"
        ),
        (None, Some(c)) => format!(
            "\x1b[36mcredential\x1b[0m {c}  \x1b[2m/model <name> to select a model\x1b[0m"
        ),
        (None, None) => {
            "\x1b[2m/credential <name> and /model <name> to get started · /help for commands\x1b[0m"
                .to_string()
        }
    };
    println!("{line2}");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_detector_only_cancels_lone_escape_after_timeout() {
        let start = std::time::Instant::now();
        let mut detector = EscDetector::new();

        assert!(detector.feed_bytes(&[0x1b], start).is_empty());
        assert!(!detector.check_timeout(start + std::time::Duration::from_millis(20)));
        assert!(detector.check_timeout(start + std::time::Duration::from_millis(100)));

        let mut detector = EscDetector::new();
        let forwarded = detector.feed_bytes(&[0x1b, b'[', b'A'], start);
        assert_eq!(forwarded, vec![0x1b, b'[', b'A']);
        assert!(!detector.check_timeout(start + std::time::Duration::from_millis(100)));
    }

    #[test]
    fn layout_wraps_prompt_and_text_by_display_width() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "abcdef".to_string();
        editor.cursor = editor.buffer.len();

        let layout = editor.compute_layout(4);
        assert_eq!(layout.rows, 2);
        assert_eq!(layout.end, CursorPos { row: 1, col: 4 });
        assert_eq!(layout.cursor, layout.end);
    }

    #[test]
    fn combining_marks_move_as_one_grapheme() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "e\u{301}x".to_string();
        editor.cursor = editor.buffer.len();

        editor.move_left();
        assert_eq!(editor.cursor, "e\u{301}".len());

        editor.move_left();
        assert_eq!(editor.cursor, 0);

        editor.move_right();
        assert_eq!(editor.cursor, "e\u{301}".len());
    }

    #[test]
    fn backspace_deletes_whole_grapheme_cluster() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "a👍🏽b".to_string();
        editor.cursor = "a👍🏽".len();

        editor.backspace();
        assert_eq!(editor.buffer, "ab");
        assert_eq!(editor.cursor, 1);
    }

    #[test]
    fn delete_removes_combining_cluster_at_cursor() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "a\u{65}\u{301}b".to_string();
        editor.cursor = 1;

        editor.delete_at_cursor();
        assert_eq!(editor.buffer, "ab");
        assert_eq!(editor.cursor, 1);
    }

    #[test]
    fn wide_graphemes_affect_layout_width() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "👍👍".to_string();
        editor.cursor = editor.buffer.len();

        let layout = editor.compute_layout(3);
        assert_eq!(layout.rows, 2);
        assert_eq!(layout.end.row, 1);
    }

    #[test]
    fn up_down_move_between_wrapped_rows_before_history() {
        let mut editor = LineEditor::with_history(vec!["history-prev".to_string()]);
        editor.buffer = "abcdef".to_string();
        editor.cursor = 5;

        editor.move_up_for_cols(4);
        assert_eq!(editor.cursor, 1);
        assert_eq!(editor.buffer, "abcdef");

        editor.move_down_for_cols(4);
        assert_eq!(editor.cursor, 5);
        assert_eq!(editor.buffer, "abcdef");
    }

    #[test]
    fn up_down_fall_back_to_history_at_row_boundaries() {
        let mut editor = LineEditor::with_history(vec!["history-prev".to_string()]);
        editor.buffer = "abcdef".to_string();
        editor.cursor = 1;

        editor.move_up_for_cols(4);
        assert_eq!(editor.buffer, "history-prev");

        editor.move_down_for_cols(4);
        assert_eq!(editor.buffer, "abcdef");
    }

    #[test]
    fn ctrl_u_ctrl_k_and_yank_work() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "hello world".to_string();
        editor.cursor = 5;

        editor.kill_to_start();
        assert_eq!(editor.buffer, " world");
        assert_eq!(editor.cursor, 0);

        editor.yank();
        assert_eq!(editor.buffer, "hello world");
        assert_eq!(editor.cursor, 5);

        editor.kill_to_end();
        assert_eq!(editor.buffer, "hello");
        assert_eq!(editor.kill_buffer, " world");

        editor.yank();
        assert_eq!(editor.buffer, "hello world");
    }

    #[test]
    fn ctrl_c_clears_without_eof() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "pending".to_string();
        editor.cursor = editor.buffer.len();
        let result = editor.feed_byte(0x03);
        assert!(matches!(result, LineEditResult::Continue));
        assert!(editor.buffer.is_empty());
        assert_eq!(editor.cursor, 0);
    }

    // --- Word movement ---

    #[test]
    fn word_backward_skips_whitespace_then_word() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "hello world".to_string();
        editor.cursor = editor.buffer.len(); // end
        assert_eq!(editor.beginning_of_previous_word(), 6); // "world"
        editor.cursor = 6;
        assert_eq!(editor.beginning_of_previous_word(), 0); // "hello"
    }

    #[test]
    fn word_forward_skips_whitespace_then_word() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "hello world".to_string();
        editor.cursor = 0;
        assert_eq!(editor.end_of_next_word(), 5); // past "hello"
        editor.cursor = 5;
        assert_eq!(editor.end_of_next_word(), 11); // past "world"
    }

    #[test]
    fn word_boundary_stops_at_separator_class_change() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "path/to/file".to_string();
        editor.cursor = editor.buffer.len();
        assert_eq!(editor.beginning_of_previous_word(), 8); // "file"
        editor.cursor = 8;
        assert_eq!(editor.beginning_of_previous_word(), 7); // "/"
        editor.cursor = 7;
        assert_eq!(editor.beginning_of_previous_word(), 5); // "to"
    }

    #[test]
    fn delete_backward_word_kills_through_buffer() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "hello world".to_string();
        editor.cursor = editor.buffer.len();
        editor.delete_backward_word();
        assert_eq!(editor.buffer, "hello ");
        assert_eq!(editor.kill_buffer, "world");
        // Ctrl+Y recovers it
        editor.yank();
        assert_eq!(editor.buffer, "hello world");
    }

    #[test]
    fn delete_forward_word_kills_through_buffer() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "hello world".to_string();
        editor.cursor = 0;
        editor.delete_forward_word();
        assert_eq!(editor.buffer, " world");
        assert_eq!(editor.kill_buffer, "hello");
    }

    #[test]
    fn ctrl_w_deletes_backward_word() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "foo bar".to_string();
        editor.cursor = editor.buffer.len();
        editor.feed_byte(0x17); // Ctrl+W
        assert_eq!(editor.buffer, "foo ");
    }

    // --- Multiline ---

    #[test]
    fn wrapped_rows_handles_explicit_newlines() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "line1\nline2".to_string();
        let rows = editor.wrapped_rows(80);
        assert_eq!(rows.len(), 2);
        assert_eq!(&editor.buffer[rows[0].clone()], "line1");
        assert_eq!(&editor.buffer[rows[1].clone()], "line2");
    }

    #[test]
    fn wrapped_rows_combines_newlines_and_wrapping() {
        let mut editor = LineEditor::with_history(Vec::new());
        // prompt "› " is 2 cols, so with cols=4 we get 2 usable chars on first row
        // "ab\ncd" → row0="ab", row1="cd" at cols=80
        // but "abcdef\ngh" at cols=4 → row0="ab", row1="cdef"(wraps), row2="gh"
        editor.buffer = "ab\ncdefgh".to_string();
        let rows = editor.wrapped_rows(6);
        // row 0: "ab" (prompt takes 2, "ab" takes 2, fits in 6)
        // row 1: starts after \n. cont prefix "· " takes 2 cols, "cdef" takes 4, total 6 → fits
        // row 2: "gh" wraps from row 1
        assert_eq!(&editor.buffer[rows[0].clone()], "ab");
        assert!(rows.len() >= 2);
        assert_eq!(&editor.buffer[rows[1].clone()], "cdef");
    }

    #[test]
    fn kill_to_start_operates_on_current_line() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "first\nsecond".to_string();
        editor.cursor = 9; // mid "second" → "sec|ond"
        editor.kill_to_start();
        assert_eq!(editor.buffer, "first\nond");
        assert_eq!(editor.kill_buffer, "sec");
        assert_eq!(editor.cursor, 6); // at start of "ond"
    }

    #[test]
    fn kill_to_end_operates_on_current_line() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "first\nsecond".to_string();
        editor.cursor = 2; // "fi|rst"
        editor.kill_to_end();
        assert_eq!(editor.buffer, "fi\nsecond");
        assert_eq!(editor.kill_buffer, "rst");
    }

    #[test]
    fn kill_to_start_at_bol_kills_preceding_newline() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "first\nsecond".to_string();
        editor.cursor = 6; // start of "second"
        editor.kill_to_start();
        assert_eq!(editor.buffer, "firstsecond");
        assert_eq!(editor.cursor, 5);
    }

    #[test]
    fn kill_to_end_at_eol_kills_trailing_newline() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "first\nsecond".to_string();
        editor.cursor = 5; // end of "first"
        editor.kill_to_end();
        assert_eq!(editor.buffer, "firstsecond");
    }

    #[test]
    fn ctrl_a_ctrl_e_navigate_current_line() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "first\nsecond".to_string();
        editor.cursor = 9; // mid "second"
        assert_eq!(editor.beginning_of_current_line(), 6);
        assert_eq!(editor.end_of_current_line(), 12);
    }

    // --- Bracketed paste ---

    #[test]
    fn bracketed_paste_inserts_text() {
        let mut editor = LineEditor::with_history(Vec::new());
        // Begin paste marker: ESC[200~
        for &b in b"\x1b[200~" {
            editor.feed_byte(b);
        }
        assert!(editor.paste_buffer.is_some());
        // Paste content
        for &b in b"pasted text" {
            editor.feed_byte(b);
        }
        // End paste marker: ESC[201~
        for &b in b"\x1b[201~" {
            editor.feed_byte(b);
        }
        assert!(editor.paste_buffer.is_none());
        assert_eq!(editor.buffer, "pasted text");
        assert_eq!(editor.cursor, "pasted text".len());
    }

    #[test]
    fn bracketed_paste_with_newlines() {
        let mut editor = LineEditor::with_history(Vec::new());
        for &b in b"\x1b[200~line1\rline2\x1b[201~" {
            editor.feed_byte(b);
        }
        assert_eq!(editor.buffer, "line1\nline2");
    }

    #[test]
    fn bracketed_paste_ignores_escape_sequences_in_content() {
        let mut editor = LineEditor::with_history(Vec::new());
        // Paste content contains ESC[A (arrow up) — should be buffered, not dispatched
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(b"before\x1b[Aafter");
        bytes.extend_from_slice(b"\x1b[201~");
        for &b in &bytes {
            editor.feed_byte(b);
        }
        assert_eq!(editor.buffer, "before\x1b[Aafter");
    }

    #[test]
    fn bracketed_paste_with_utf8() {
        let mut editor = LineEditor::with_history(Vec::new());
        let content = "héllo wörld";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(content.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        for &b in &bytes {
            editor.feed_byte(b);
        }
        assert_eq!(editor.buffer, content);
    }
}
