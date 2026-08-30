//! Bounded replay buffer of recent PTY output for late-joining viewers.
//!
//! A viewer that attaches mid-session sees nothing until the agent happens to
//! repaint. Keeping the tail of the output stream lets the tunnel hand a new or
//! resynced viewer something to render immediately.
//!
//! Chunks are retained and dropped whole, never sliced, so a replay never
//! begins in the middle of an escape sequence. The one exception is a single
//! chunk larger than the cap, whose tail is kept so the cap stays hard.

use std::collections::VecDeque;

/// Retained output, sized for roughly a screenful of a busy TUI redraw.
pub(crate) const REPLAY_CAPACITY_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub(crate) struct ReplayBuffer {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    capacity: usize,
}

impl ReplayBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            capacity,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.chunks.push_back(data.to_vec());
        self.bytes += data.len();

        // Drop whole leading chunks while what remains still covers the cap, so
        // the retained stream always contains at least `capacity` trailing bytes.
        while self.chunks.len() > 1
            && self.bytes - self.chunks.front().map_or(0, |c| c.len()) >= self.capacity
        {
            if let Some(front) = self.chunks.pop_front() {
                self.bytes -= front.len();
            }
        }

        // A lone chunk bigger than the cap would grow the buffer without bound.
        if self.chunks.len() == 1
            && self.bytes > self.capacity
            && let Some(only) = self.chunks.front_mut()
        {
            let tail = only.split_off(only.len() - self.capacity);
            *only = tail;
            self.bytes = only.len();
        }
    }

    /// Flatten the retained output for delivery to a viewer.
    pub(crate) fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bytes);
        for chunk in &self.chunks {
            out.extend_from_slice(chunk);
        }
        out
    }
}

impl Default for ReplayBuffer {
    fn default() -> Self {
        Self::new(REPLAY_CAPACITY_BYTES)
    }
}

#[cfg(test)]
mod tests;
