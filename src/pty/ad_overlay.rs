use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const AD_LINES: &[&str] = &[
    "Linear - ship issues faster",
    "Ramp - close books before lunch",
    "Sentry - fix production before users notice",
];
const AD_ROTATE_AFTER: Duration = Duration::from_secs(30);
const PATCH_REPEAT_AFTER: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentKind {
    Claude,
    Codex,
    Cursor,
    OpenCode,
    Grok,
}

impl AgentKind {
    pub(crate) fn parse(agent: &str) -> Option<Self> {
        match agent {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" | "agent" | "cursor-agent" => Some(Self::Cursor),
            "opencode" => Some(Self::OpenCode),
            "grok" => Some(Self::Grok),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ParseState {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
}

pub(crate) struct PtyAdOverlay {
    kind: AgentKind,
    rows: usize,
    cols: usize,
    cursor_row: usize,
    cursor_col: usize,
    saved_cursor: Option<(usize, usize)>,
    screen: Vec<Vec<char>>,
    dirty_rows: Vec<bool>,
    state: ParseState,
    csi: String,
    osc: Vec<u8>,
    print_buf: Vec<u8>,
    ad_idx: usize,
    ad_changed_at: Instant,
    anchor: Option<StatusAnchor>,
    alt_screen: bool,
    sync_update: bool,
    saw_tui_frame: bool,
    last_patch: Option<PatchState>,
}

#[derive(Debug, Clone, Copy)]
struct StatusAnchor {
    row: usize,
}

#[derive(Debug, Clone)]
struct PatchState {
    row: usize,
    start_col: usize,
    width: usize,
    ad_idx: usize,
    at: Instant,
}

impl PtyAdOverlay {
    pub(crate) fn new(kind: AgentKind, size: Option<(u16, u16)>) -> Self {
        let (cols, rows) = size.unwrap_or((80, 24));
        let rows = rows.max(1) as usize;
        let cols = cols.max(1) as usize;
        Self {
            kind,
            rows,
            cols,
            cursor_row: 0,
            cursor_col: 0,
            saved_cursor: None,
            screen: vec![Vec::new(); rows],
            dirty_rows: vec![false; rows],
            state: ParseState::Ground,
            csi: String::new(),
            osc: Vec::new(),
            print_buf: Vec::new(),
            ad_idx: initial_ad_idx(),
            ad_changed_at: Instant::now(),
            anchor: None,
            alt_screen: false,
            sync_update: false,
            saw_tui_frame: false,
            last_patch: None,
        }
    }

    pub(crate) fn resize(&mut self, size: Option<(u16, u16)>) {
        let Some((cols, rows)) = size else {
            return;
        };
        self.cols = cols.max(1) as usize;
        self.rows = rows.max(1) as usize;
        self.screen.resize_with(self.rows, Vec::new);
        self.dirty_rows.resize(self.rows, false);
        self.cursor_row = self.cursor_row.min(self.rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(self.cols.saturating_sub(1));
        if let Some(anchor) = self.anchor.as_mut() {
            anchor.row = anchor.row.min(self.rows.saturating_sub(1));
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.dirty_rows.fill(false);
        for &byte in bytes {
            match self.state {
                ParseState::Ground => self.feed_ground(byte),
                ParseState::Esc => self.feed_esc(byte),
                ParseState::Csi => self.feed_csi(byte),
                ParseState::Osc => self.feed_osc(byte),
                ParseState::OscEsc => self.feed_osc_esc(byte),
            }
        }
        self.flush_print();
        self.refresh_ad();
        self.update_anchor();
        self.overlay_bytes()
    }

    fn feed_ground(&mut self, byte: u8) {
        match byte {
            0x1b => {
                self.flush_print();
                self.state = ParseState::Esc;
            }
            b'\r' => {
                self.flush_print();
                self.cursor_col = 0;
            }
            b'\n' => {
                self.flush_print();
                self.line_feed();
            }
            0x08 => {
                self.flush_print();
                self.cursor_col = self.cursor_col.saturating_sub(1);
            }
            0x20..=0x7e | 0x80..=0xff => {
                self.print_buf.push(byte);
            }
            _ => {}
        }
    }

    fn feed_esc(&mut self, byte: u8) {
        match byte {
            b'[' => {
                self.csi.clear();
                self.state = ParseState::Csi;
            }
            b']' => {
                self.osc.clear();
                self.state = ParseState::Osc;
            }
            b'7' => {
                self.saved_cursor = Some((self.cursor_row, self.cursor_col));
                self.state = ParseState::Ground;
            }
            b'8' => {
                if let Some((row, col)) = self.saved_cursor {
                    self.cursor_row = row.min(self.rows.saturating_sub(1));
                    self.cursor_col = col.min(self.cols.saturating_sub(1));
                }
                self.state = ParseState::Ground;
            }
            b'D' => {
                self.line_feed();
                self.state = ParseState::Ground;
            }
            b'M' => {
                self.cursor_row = self.cursor_row.saturating_sub(1);
                self.state = ParseState::Ground;
            }
            b'E' => {
                self.cursor_col = 0;
                self.line_feed();
                self.state = ParseState::Ground;
            }
            _ => {
                self.state = ParseState::Ground;
            }
        }
    }

    fn feed_csi(&mut self, byte: u8) {
        if (0x40..=0x7e).contains(&byte) {
            let final_byte = byte as char;
            let params = self.csi.clone();
            self.apply_csi(&params, final_byte);
            self.csi.clear();
            self.state = ParseState::Ground;
        } else {
            self.csi.push(byte as char);
        }
    }

    fn feed_osc(&mut self, byte: u8) {
        match byte {
            0x07 => {
                self.apply_osc();
                self.state = ParseState::Ground;
            }
            0x1b => self.state = ParseState::OscEsc,
            _ => {
                if self.osc.len() < 512 {
                    self.osc.push(byte);
                }
            }
        }
    }

    fn feed_osc_esc(&mut self, byte: u8) {
        if byte == b'\\' {
            self.apply_osc();
            self.state = ParseState::Ground;
        } else {
            if self.osc.len() + 2 <= 512 {
                self.osc.push(0x1b);
                self.osc.push(byte);
            }
            self.state = ParseState::Osc;
        }
    }

    fn flush_print(&mut self) {
        if self.print_buf.is_empty() {
            return;
        }
        let mut bytes = std::mem::take(&mut self.print_buf);
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text.to_string(),
            Err(err) if err.error_len().is_none() => {
                let valid = err.valid_up_to();
                self.print_buf.extend_from_slice(&bytes[valid..]);
                bytes.truncate(valid);
                String::from_utf8_lossy(&bytes).into_owned()
            }
            Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
        };
        for ch in text.chars() {
            self.put_char(ch);
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.line_feed();
        }
        let line = &mut self.screen[self.cursor_row];
        if line.len() <= self.cursor_col {
            line.resize(self.cursor_col + 1, ' ');
        }
        line[self.cursor_col] = ch;
        self.mark_dirty();
        self.cursor_col += UnicodeWidthChar::width(ch).unwrap_or(1).max(1);
    }

    fn line_feed(&mut self) {
        if self.cursor_row + 1 >= self.rows {
            if !self.screen.is_empty() {
                self.screen.remove(0);
                self.screen.push(Vec::new());
                self.dirty_rows.fill(true);
            }
        } else {
            self.cursor_row += 1;
        }
    }

    fn apply_csi(&mut self, params: &str, final_byte: char) {
        let private = params.starts_with('?');
        let clean = params.trim_start_matches('?');
        let nums = parse_csi_nums(clean);
        match final_byte {
            'A' => {
                let n = param_or(&nums, 0, 1);
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'B' => {
                let n = param_or(&nums, 0, 1);
                self.cursor_row = (self.cursor_row + n).min(self.rows.saturating_sub(1));
            }
            'C' => {
                let n = param_or(&nums, 0, 1);
                self.cursor_col = (self.cursor_col + n).min(self.cols.saturating_sub(1));
            }
            'D' => {
                let n = param_or(&nums, 0, 1);
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'G' => {
                self.mark_tui_frame();
                let col = param_or(&nums, 0, 1);
                self.cursor_col = col.saturating_sub(1).min(self.cols.saturating_sub(1));
            }
            'H' | 'f' => {
                self.mark_tui_frame();
                let row = param_or(&nums, 0, 1);
                let col = param_or(&nums, 1, 1);
                self.cursor_row = row.saturating_sub(1).min(self.rows.saturating_sub(1));
                self.cursor_col = col.saturating_sub(1).min(self.cols.saturating_sub(1));
            }
            'J' if !private => match param_or(&nums, 0, 0) {
                2 | 3 => {
                    for line in &mut self.screen {
                        line.clear();
                    }
                    self.dirty_rows.fill(true);
                    self.anchor = None;
                    self.last_patch = None;
                    self.mark_tui_frame();
                }
                _ => {}
            },
            'K' => match param_or(&nums, 0, 0) {
                0 => {
                    if let Some(line) = self.screen.get_mut(self.cursor_row) {
                        line.truncate(self.cursor_col);
                    }
                    self.mark_dirty();
                }
                1 => {
                    if let Some(line) = self.screen.get_mut(self.cursor_row) {
                        for idx in 0..=self.cursor_col.min(line.len().saturating_sub(1)) {
                            line[idx] = ' ';
                        }
                    }
                    self.mark_dirty();
                }
                2 => {
                    if let Some(line) = self.screen.get_mut(self.cursor_row) {
                        line.clear();
                    }
                    self.mark_dirty();
                }
                _ => {}
            },
            's' => self.saved_cursor = Some((self.cursor_row, self.cursor_col)),
            'u' => {
                if let Some((row, col)) = self.saved_cursor {
                    self.cursor_row = row.min(self.rows.saturating_sub(1));
                    self.cursor_col = col.min(self.cols.saturating_sub(1));
                }
            }
            'h' if private => self.apply_private_mode(&nums, true),
            'l' if private => self.apply_private_mode(&nums, false),
            _ => {}
        }
    }

    fn overlay_bytes(&mut self) -> Option<Vec<u8>> {
        let row = self.anchor?.row.min(self.rows.saturating_sub(1));
        if self.last_patch.as_ref().is_some_and(|last| {
            last.row == row && last.ad_idx == self.ad_idx && last.at.elapsed() < PATCH_REPEAT_AFTER
        }) {
            return None;
        }
        let ad = self.current_ad_line();
        let ad = fit_to_width(&format!(" · [ad]  {}", sanitize_ad(ad)), self.cols / 2);
        if ad.is_empty() {
            return None;
        }
        let ad_width = UnicodeWidthStr::width(ad.as_str()).max(1);
        let start_col = self.cols.saturating_sub(ad_width);
        let pad_width = self.cols.saturating_sub(start_col + ad_width);
        let return_row = self.cursor_row.min(self.rows.saturating_sub(1)) + 1;
        let return_col = self.cursor_col.min(self.cols.saturating_sub(1)) + 1;
        let mut out = Vec::new();
        if let Some(last) = self.last_patch.as_ref()
            && last.row != row
        {
            let clear_start = last.start_col.min(self.cols.saturating_sub(1));
            let clear_width = last.width.min(self.cols.saturating_sub(clear_start));
            out.extend_from_slice(
                format!(
                    "\x1b[{};{}H{}",
                    last.row.min(self.rows.saturating_sub(1)) + 1,
                    clear_start + 1,
                    " ".repeat(clear_width)
                )
                .as_bytes(),
            );
        }
        self.last_patch = Some(PatchState {
            row,
            start_col,
            width: ad_width + pad_width,
            ad_idx: self.ad_idx,
            at: Instant::now(),
        });
        out.extend_from_slice(
            format!(
                "\x1b[{};{}H{}{}\x1b[{};{}H",
                row + 1,
                start_col + 1,
                ad,
                " ".repeat(pad_width),
                return_row,
                return_col
            )
            .as_bytes(),
        );
        Some(out)
    }

    fn current_ad_line(&self) -> &'static str {
        AD_LINES[self.ad_idx]
    }

    fn refresh_ad(&mut self) {
        if self.ad_changed_at.elapsed() < AD_ROTATE_AFTER {
            return;
        }
        self.ad_idx = (self.ad_idx + 1) % AD_LINES.len();
        self.ad_changed_at = Instant::now();
    }

    fn update_anchor(&mut self) {
        let detected = self.detect_status_row();
        let default = self.default_anchor_row();
        let row = match self.kind {
            AgentKind::Cursor => detected
                .filter(|row| *row >= self.rows.saturating_sub(3))
                .or(default),
            _ => detected.or(default),
        };
        if let Some(row) = row {
            self.anchor = Some(StatusAnchor {
                row: row.min(self.rows.saturating_sub(1)),
            });
        }
    }

    fn detect_status_row(&self) -> Option<usize> {
        self.screen
            .iter()
            .enumerate()
            .rev()
            .filter(|(row, _)| self.dirty_rows.get(*row).copied().unwrap_or(false))
            .find_map(|(row, line)| {
                let text = line.iter().collect::<String>();
                if status_line_matches(self.kind, &text) {
                    Some(row)
                } else {
                    None
                }
            })
    }

    fn mark_dirty(&mut self) {
        if let Some(dirty) = self.dirty_rows.get_mut(self.cursor_row) {
            *dirty = true;
        }
    }

    fn apply_osc(&mut self) {
        let text = String::from_utf8_lossy(&self.osc).to_string();
        self.osc.clear();
        let Some((kind, title)) = text.split_once(';') else {
            return;
        };
        let _ = (kind, title);
    }

    fn apply_private_mode(&mut self, nums: &[usize], enabled: bool) {
        for num in nums {
            match *num {
                1049 => {
                    self.alt_screen = enabled;
                    if enabled {
                        self.mark_tui_frame();
                    }
                    if !enabled {
                        self.anchor = None;
                        self.last_patch = None;
                    }
                }
                2026 => {
                    self.sync_update = enabled;
                    if enabled {
                        self.mark_tui_frame();
                    }
                }
                _ => {}
            }
        }
    }

    fn default_anchor_row(&self) -> Option<usize> {
        if !(self.alt_screen || self.sync_update || self.saw_tui_frame) {
            return None;
        }
        let row = match self.kind {
            AgentKind::Codex => self.default_codex_row(),
            AgentKind::Claude => self.rows.saturating_sub(1),
            AgentKind::Cursor | AgentKind::OpenCode | AgentKind::Grok => {
                self.rows.saturating_sub(2)
            }
        };
        Some(row.min(self.rows.saturating_sub(1)))
    }

    fn default_codex_row(&self) -> usize {
        2.min(self.rows.saturating_sub(1))
    }

    fn mark_tui_frame(&mut self) {
        self.saw_tui_frame = true;
        if self.kind == AgentKind::Codex && self.cursor_row == self.default_codex_row() {
            self.anchor = Some(StatusAnchor {
                row: self.default_codex_row(),
            });
        }
    }
}

fn parse_csi_nums(params: &str) -> Vec<usize> {
    params
        .split(';')
        .map(|part| {
            part.trim_matches(|c: char| !c.is_ascii_digit())
                .parse::<usize>()
                .unwrap_or(0)
        })
        .collect()
}

fn param_or(nums: &[usize], idx: usize, default: usize) -> usize {
    nums.get(idx).copied().filter(|n| *n > 0).unwrap_or(default)
}

fn initial_ad_idx() -> usize {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    (secs as usize / AD_ROTATE_AFTER.as_secs() as usize) % AD_LINES.len()
}

fn sanitize_ad(input: &str) -> String {
    input
        .chars()
        .filter(|ch| !ch.is_control())
        .take(96)
        .collect()
}

fn fit_to_width(input: &str, max_width: usize) -> String {
    let max_width = max_width.max(1);
    let mut out = String::new();
    let mut width = 0usize;
    for ch in input.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(1);
        if width + ch_width > max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out
}

fn status_line_matches(kind: AgentKind, text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 180 {
        return false;
    }
    match kind {
        AgentKind::Claude => claude_status(trimmed),
        AgentKind::Codex => codex_status(trimmed),
        AgentKind::Cursor => cursor_status(trimmed),
        AgentKind::OpenCode => opencode_status(trimmed),
        AgentKind::Grok => grok_status(trimmed),
    }
}

fn claude_status(s: &str) -> bool {
    let first = s.chars().next();
    matches!(first, Some('✢' | '✶' | '✻' | '✽' | '◉' | '✳'))
        || has_interrupt_hint(s)
        || s.contains(" | Model")
}

fn codex_status(s: &str) -> bool {
    let lower = status_prefix(s);
    starts_with_any(&lower, &["thinking", "working", "running", "reasoning"])
        || status_with_hint(&lower, &["thinking", "working", "running", "reasoning"])
}

fn cursor_status(s: &str) -> bool {
    let lower = status_prefix(s);
    starts_with_any(&lower, &["thinking", "generating", "working", "agent is "])
        || status_with_hint(&lower, &["thinking", "generating", "working", "agent is "])
        || lower.contains("composer")
        || lower.contains("run everything")
}

fn opencode_status(s: &str) -> bool {
    let lower = status_prefix(s);
    starts_with_any(&lower, &["thinking", "working", "running", "processing"])
        || status_with_hint(&lower, &["thinking", "working", "running", "processing"])
        || lower.contains("opencode")
}

fn grok_status(s: &str) -> bool {
    let lower = status_prefix(s);
    starts_with_any(&lower, &["thinking", "working", "reasoning", "grok is "])
        || status_with_hint(&lower, &["thinking", "working", "reasoning", "grok is "])
        || lower.contains("grok")
}

fn starts_with_any(s: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| s.starts_with(needle))
}

fn status_with_hint(s: &str, needles: &[&str]) -> bool {
    if !(has_spinner_prefix(s) || has_interrupt_hint(s) || s.len() <= 80) {
        return false;
    }
    needles.iter().any(|needle| s.contains(needle))
}

fn has_interrupt_hint(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("esc to interrupt")
        || lower.contains("press esc")
        || lower.contains("interrupt")
        || lower.contains("ctrl-c")
        || lower.contains("ctrl+c")
}

fn has_spinner_prefix(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|ch| is_status_prefix_char(ch) && !ch.is_ascii_punctuation())
}

fn status_prefix(s: &str) -> String {
    s.trim_start_matches(is_status_prefix_char)
        .to_ascii_lowercase()
}

fn is_status_prefix_char(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '>' | '-'
                | '|'
                | '/'
                | '\\'
                | '•'
                | '*'
                | '·'
                | '…'
                | '●'
                | '○'
                | '◆'
                | '◇'
                | '◐'
                | '◓'
                | '◑'
                | '◒'
                | '◜'
                | '◠'
                | '◝'
                | '◞'
                | '◡'
                | '◟'
                | '✢'
                | '✦'
                | '✧'
                | '✶'
                | '✻'
                | '✽'
                | '⠋'
                | '⠙'
                | '⠹'
                | '⠸'
                | '⠼'
                | '⠴'
                | '⠦'
                | '⠧'
                | '⠇'
                | '⠏'
                | '⣾'
                | '⣽'
                | '⣻'
                | '⢿'
                | '⡿'
                | '⣟'
                | '⣯'
                | '⣷'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay_text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).to_string()
    }

    #[test]
    fn codex_thinking_line_gets_overlay() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Codex, Some((80, 24)));
        let bytes = overlay.feed(b"\r\x1b[2KThinking");
        let text = overlay_text(&bytes.expect("overlay"));
        assert!(text.contains("\x1b[1;"));
        assert!(text.contains(" · [ad]  "));
        assert!(!text.contains("\x1b[2K"));
    }

    #[test]
    fn claude_spinner_line_gets_overlay() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Claude, Some((80, 24)));
        let bytes = overlay.feed("✻ Reticulating… (esc to interrupt)".as_bytes());
        assert!(overlay_text(&bytes.expect("overlay")).contains(" · [ad]  "));
    }

    #[test]
    fn cursor_movement_tracks_status_row() {
        let mut overlay = PtyAdOverlay::new(AgentKind::OpenCode, Some((80, 24)));
        assert!(overlay.feed(b"hello\nworld").is_none());
        let bytes = overlay.feed(b"\x1b[10;1HProcessing request");
        let text = overlay_text(&bytes.expect("overlay"));
        assert!(text.contains("\x1b[10;"));
        assert!(text.contains(" · [ad]  "));
    }

    #[test]
    fn non_status_line_does_not_overlay() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Grok, Some((80, 24)));
        assert!(overlay.feed(b"Here is your answer").is_none());
    }

    #[test]
    fn stale_status_row_does_not_repaint_on_unrelated_output() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Codex, Some((80, 24)));
        assert!(overlay.feed(b"\x1b[5;1HThinking").is_some());
        assert!(overlay.feed(b"\x1b[20;1HFinal answer").is_some());
    }

    #[test]
    fn sanitize_strips_control_bytes() {
        assert_eq!(sanitize_ad("ok\x1b]0;pwn\x07"), "ok]0;pwn");
    }

    #[test]
    fn ad_line_stays_stable_until_rotate_window_expires() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Codex, Some((80, 24)));
        overlay.ad_idx = 0;
        overlay.ad_changed_at = Instant::now();
        assert!(overlay.feed(b"Thinking").is_some());
        assert_eq!(overlay.current_ad_line(), AD_LINES[0]);
        overlay.ad_changed_at = Instant::now() - Duration::from_secs(29);
        let _ = overlay.feed(b"\r\x1b[2KThinking again");
        assert_eq!(overlay.current_ad_line(), AD_LINES[0]);
        overlay.ad_changed_at = Instant::now() - Duration::from_secs(30);
        assert!(overlay.feed(b"\r\x1b[2KThinking still").is_some());
        assert_eq!(overlay.current_ad_line(), AD_LINES[1]);
    }

    #[test]
    fn opencode_symbol_status_gets_overlay() {
        let mut overlay = PtyAdOverlay::new(AgentKind::OpenCode, Some((80, 24)));
        let bytes = overlay.feed("◐ openrouter/sonnet processing request".as_bytes());
        assert!(overlay_text(&bytes.expect("overlay")).contains(" · [ad]  "));
    }

    #[test]
    fn grok_symbol_status_gets_overlay() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Grok, Some((80, 24)));
        let bytes = overlay.feed("✦ grok is reasoning".as_bytes());
        assert!(overlay_text(&bytes.expect("overlay")).contains(" · [ad]  "));
    }

    #[test]
    fn codex_osc_title_spinner_alone_does_not_anchor() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Codex, Some((80, 24)));
        assert!(
            overlay
                .feed("hello\x1b]0;⠴ sidekar\x07".as_bytes())
                .is_none()
        );
    }

    #[test]
    fn codex_tui_frame_anchors_row_three() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Codex, Some((80, 24)));
        let bytes = overlay.feed("\x1b[?2026h\x1b[3;4H⠴ sidekar".as_bytes());
        let text = overlay_text(&bytes.expect("overlay"));
        assert!(text.contains("\x1b[3;"));
        assert!(text.contains(" · [ad]  "));
    }

    #[test]
    fn moving_anchor_clears_previous_patch_region() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Claude, Some((80, 24)));
        assert!(
            overlay
                .feed("✻ Reticulating… (esc to interrupt)".as_bytes())
                .is_some()
        );
        overlay.last_patch.as_mut().expect("last patch").at = Instant::now() - PATCH_REPEAT_AFTER;
        let bytes = overlay.feed(b"\x1b[24;1Hsidekar (main) | Model");
        let text = overlay_text(&bytes.expect("overlay"));
        assert!(text.contains("\x1b[1;"));
        assert!(text.contains("\x1b[24;"));
        assert!(text.contains(" · [ad]  "));
    }

    #[test]
    fn opencode_alt_screen_defaults_to_footer_band() {
        let mut overlay = PtyAdOverlay::new(AgentKind::OpenCode, Some((80, 24)));
        let bytes = overlay.feed(b"\x1b[?1049h\x1b[23;1H~/src/sidekar 1.17.15");
        let text = overlay_text(&bytes.expect("overlay"));
        assert!(text.contains("\x1b[23;"));
        assert!(text.contains(" · [ad]  "));
    }

    #[test]
    fn cursor_footer_anchors_without_status_words() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Cursor, Some((100, 30)));
        let bytes = overlay.feed(b"\x1b[2J\x1b[29;1HComposer 2.5 Fast Run Everything");
        let text = overlay_text(&bytes.expect("overlay"));
        assert!(text.contains("\x1b[29;"));
        assert!(text.contains(" · [ad]  "));
    }

    #[test]
    fn cursor_mid_screen_composer_does_not_steal_footer_anchor() {
        let mut overlay = PtyAdOverlay::new(AgentKind::Cursor, Some((120, 40)));
        let bytes = overlay.feed(b"\x1b[2J\x1b[15;1HComposer 2.5 Fast Run Everything");
        let text = overlay_text(&bytes.expect("overlay"));
        assert!(text.contains("\x1b[39;"));
        assert!(text.contains(" · [ad]  "));
    }
}
