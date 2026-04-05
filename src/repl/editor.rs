use anyhow::Result;
use std::io::{self, BufRead, Read, Write};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::broker;

use super::InputEvent;

const PROMPT_PREFIX: &str = "\x1b[36m›\x1b[0m ";
const PROMPT_VISIBLE: &str = "› ";

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
        Ok(Self { saved, fd })
    }

    /// Temporarily restore cooked mode (for subprocesses). On drop, raw mode is re-entered.
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

    fn feed_bytes(&mut self, bytes: &[u8], now: std::time::Instant) -> bool {
        for &byte in bytes {
            if self.pending_escape.is_some() {
                self.pending_escape = None;
            }
            if byte == 0x1b {
                self.pending_escape = Some(now);
            }
        }
        false
    }

    fn check_timeout(&mut self, now: std::time::Instant) -> bool {
        let Some(started) = self.pending_escape else {
            return false;
        };
        if now.duration_since(started) >= std::time::Duration::from_millis(75) {
            self.pending_escape = None;
            return true;
        }
        false
    }
}

pub(super) struct EscCancelWatcher {
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    raw_mode: Option<RawModeGuard>,
}

impl EscCancelWatcher {
    pub(super) fn start(
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        tunnel_fd: Option<i32>,
    ) -> Self {
        let raw_mode = RawModeGuard::enter().ok();
        let local_fd = if raw_mode.is_some() { Some(libc::STDIN_FILENO) } else { None };
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
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
                            if n > 0 && detector.feed_bytes(&buf[..n as usize], now) {
                                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                                return;
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
    rendered_rows: usize,
    rendered_cursor: CursorPos,
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
            rendered_rows: 0,
            rendered_cursor: CursorPos::default(),
        }
    }

    fn emit(&self, text: &str) {
        print!("{text}");
        crate::tunnel::tunnel_send(text.as_bytes().to_vec());
        let _ = io::stdout().flush();
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

    fn clear_rendered_prompt(&mut self) {
        if self.rendered_rows == 0 {
            return;
        }
        let mut clear = String::new();
        if self.rendered_cursor.row > 0 {
            clear.push_str(&format!("\x1b[{}A", self.rendered_cursor.row));
        }
        clear.push('\r');
        for row in 0..self.rendered_rows {
            clear.push_str("\x1b[2K");
            if row + 1 < self.rendered_rows {
                clear.push_str("\x1b[1B\r");
            }
        }
        if self.rendered_rows > 1 {
            clear.push_str(&format!("\x1b[{}A", self.rendered_rows - 1));
        }
        clear.push('\r');
        self.emit(&clear);
        self.rendered_rows = 0;
        self.rendered_cursor = CursorPos::default();
    }

    fn redraw(&mut self) {
        let cols = self.terminal_columns();
        let layout = self.compute_layout(cols);
        self.clear_rendered_prompt();

        let mut out = format!("\r{}{}", PROMPT_PREFIX, self.buffer);
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
        self.emit(&out);
        self.rendered_rows = layout.rows;
        self.rendered_cursor = layout.cursor;
    }

    fn clear_display(&mut self) {
        self.clear_rendered_prompt();
    }

    fn move_to_render_end(&mut self) {
        if self.cursor == self.buffer.len() {
            return;
        }
        let saved_cursor = self.cursor;
        self.cursor = self.buffer.len();
        self.redraw();
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
        self.rendered_rows = 0;
        self.rendered_cursor = CursorPos::default();
    }

    fn compute_layout(&self, cols: usize) -> RenderLayout {
        let cols = cols.max(1);
        let cursor_cells = UnicodeWidthStr::width(PROMPT_VISIBLE)
            + UnicodeWidthStr::width(&self.buffer[..self.cursor]);
        let total_cells =
            UnicodeWidthStr::width(PROMPT_VISIBLE) + UnicodeWidthStr::width(self.buffer.as_str());

        let cursor = visual_cursor_pos(cursor_cells, cols);
        let end = visual_cursor_pos(total_cells, cols);
        let rows = occupied_rows(total_cells, cols);

        RenderLayout { rows, cursor, end }
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
                self.emit("\r\n");
                let submitted = self.buffer.clone();
                self.record_submission(&submitted);
                self.reset();
                LineEditResult::Submit(submitted)
            }
            0x04 => {
                if self.buffer.is_empty() {
                    self.clear_rendered_prompt();
                    self.emit("\r\n");
                    self.reset();
                    LineEditResult::Eof
                } else {
                    self.delete_at_cursor();
                    self.redraw();
                    LineEditResult::Continue
                }
            }
            0x03 => {
                self.clear_rendered_prompt();
                self.emit(&format!("\r{}{}", PROMPT_PREFIX, self.buffer));
                self.emit("\r\n");
                self.cancel_line();
                LineEditResult::Eof
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
                self.history_prev();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'B'] => {
                self.history_next();
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'H'] | [0x1b, b'O', b'H'] => {
                self.cursor = 0;
                self.escape.clear();
                self.escape_started_at = None;
                true
            }
            [0x1b, b'[', b'F'] | [0x1b, b'O', b'F'] => {
                self.cursor = self.buffer.len();
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
            [0x1b] | [0x1b, b'['] | [0x1b, b'O'] | [0x1b, b'[', b'3'] => false,
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
        let Some(started) = self.escape_started_at else {
            return false;
        };
        if started.elapsed() < std::time::Duration::from_millis(75) {
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
    }

    fn move_left(&mut self) {
        self.cursor = prev_grapheme_boundary(&self.buffer, self.cursor);
    }

    fn move_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.buffer, self.cursor);
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.detach_history_nav();
        let prev = prev_grapheme_boundary(&self.buffer, self.cursor);
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
    }

    fn delete_at_cursor(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        self.detach_history_nav();
        let end = next_grapheme_boundary(&self.buffer, self.cursor);
        self.buffer.drain(self.cursor..end);
    }

    fn cancel_line(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.escape.clear();
        self.utf8.clear();
        self.history_index = None;
        self.history_draft = None;
        self.escape_started_at = None;
        self.rendered_rows = 0;
        self.rendered_cursor = CursorPos::default();
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
        }
    }
}

fn occupied_rows(cells: usize, cols: usize) -> usize {
    let cols = cols.max(1);
    if cells == 0 {
        1
    } else {
        (cells.saturating_sub(1) / cols) + 1
    }
}

fn visual_cursor_pos(cells: usize, cols: usize) -> CursorPos {
    let cols = cols.max(1);
    if cells == 0 {
        return CursorPos::default();
    }
    let row = (cells - 1) / cols;
    let rem = cells % cols;
    let col = if rem == 0 {
        cols.saturating_sub(1)
    } else {
        rem
    };
    CursorPos { row, col }
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
            let nfds: libc::nfds_t;
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
            nfds = if tunnel_fd.is_some() { 2 } else { 1 };

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

        detector.feed_bytes(&[0x1b], start);
        assert!(!detector.check_timeout(start + std::time::Duration::from_millis(20)));
        assert!(detector.check_timeout(start + std::time::Duration::from_millis(100)));

        let mut detector = EscDetector::new();
        detector.feed_bytes(&[0x1b, b'[', b'A'], start);
        assert!(!detector.check_timeout(start + std::time::Duration::from_millis(100)));
    }

    #[test]
    fn layout_wraps_prompt_and_text_by_display_width() {
        let mut editor = LineEditor::with_history(Vec::new());
        editor.buffer = "abcdef".to_string();
        editor.cursor = editor.buffer.len();

        let layout = editor.compute_layout(4);
        assert_eq!(layout.rows, 2);
        assert_eq!(layout.end, CursorPos { row: 1, col: 3 });
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
}
