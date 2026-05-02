//! Shared spinner format used by the REPL renderer (tool exec, model status)
//! and the `! cmd` shell-escape runner. Owns the braille tick set, color, tick rate,
//! and frame-string format so neither caller re-implements them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rand::Rng as _;

use super::editor::{clear_transient_status, emit_transient_status};

/// One-column Braille-pattern symbols ( Unicode U+28xx ). Randomized each tick.
const BRAILLE_TICKS: &[char] = &[
    '⠁', '⠂', '⠃', '⠄', '⠅', '⠆', '⠇', '⠈', '⠉', '⠊', '⠋', '⠌', '⠍', '⠎', '⠏', '⠐', '⠑', '⠒', '⠓',
    '⠔', '⠕', '⠖', '⠗', '⠘', '⠙', '⠚', '⠛', '⠜', '⠝', '⠞', '⠟', '⠠', '⠡', '⠢', '⠣', '⠤', '⠥', '⠦',
    '⠧', '⠨', '⠩', '⠪', '⠫', '⠬', '⠭', '⠮', '⠯', '⠰', '⠱', '⠲', '⠳', '⠴', '⠵', '⠶', '⠷', '⠸', '⠹',
    '⠺', '⠻', '⠼', '⠽', '⠾', '⠿',
];
pub(super) const COLOR: &str = "\x1b[36m";
const SHOW_AFTER: Duration = Duration::from_millis(250);
pub(super) const TICK: Duration = Duration::from_millis(250);

/// Build one spinner frame string. Format: `⠿ X.Xs label` (one random braille cell).
/// `idx` is kept for call-site compatibility; each frame picks a fresh random tick.
/// Caller is responsible for emitting it (raw write, transient status, etc).
pub(super) fn frame(_idx: usize, elapsed: Duration, label: &str) -> String {
    let label_part = if label.is_empty() {
        String::new()
    } else {
        format!(" {label}")
    };
    let tick = BRAILLE_TICKS[rand::rng().random_range(..BRAILLE_TICKS.len())];
    format!(
        "{COLOR}{tick} {:.1}s{label_part}\x1b[0m",
        elapsed.as_secs_f32(),
    )
}

/// Spinner that emits frames via the active prompt's transient-status row
/// (used by the renderer for tool exec / model status). Stops + clears the
/// status row on `stop()` / `Drop`.
pub(super) struct Spinner {
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    pub(super) fn start_with_label(label: String) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            std::thread::sleep(SHOW_AFTER);
            if !r.load(Ordering::Relaxed) {
                return;
            }
            let mut i = 0usize;
            while r.load(Ordering::Relaxed) {
                emit_transient_status(&frame(i, started.elapsed(), &label));
                i += 1;
                std::thread::sleep(TICK);
            }
            clear_transient_status();
        });
        Self {
            running,
            handle: Some(handle),
        }
    }

    pub(super) fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
