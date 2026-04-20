//! Shared spinner format used by the REPL renderer (tool exec, model status)
//! and the `! cmd` shell-escape runner. Owns the frames, color, tick rate,
//! and frame-string format so neither caller re-implements them.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::editor::{clear_transient_status, emit_transient_status};

pub(super) const FRAMES: &[&str] = &[
    "[    ]", "[=   ]", "[==  ]", "[=== ]", "[ ===]", "[  ==]", "[   =]", "[    ]",
];
pub(super) const COLOR: &str = "\x1b[36m";
pub(super) const TICK: Duration = Duration::from_millis(80);

/// Build one spinner frame string. Format: `[####] X.Xs label`.
/// Caller is responsible for emitting it (raw write, transient status, etc).
pub(super) fn frame(idx: usize, elapsed: Duration, label: &str) -> String {
    let label_part = if label.is_empty() {
        String::new()
    } else {
        format!(" {label}")
    };
    format!(
        "{COLOR}{} {:.1}s{label_part}\x1b[0m",
        FRAMES[idx % FRAMES.len()],
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
