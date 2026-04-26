//! Head-tail bounded buffer for PTY output capture.
//!
//! An `ExecSession` can run for hours and produce megabytes of output.
//! We can't hold all of it in memory, and the model doesn't need most
//! of it. What the model DOES need:
//!
//! 1. The first chunk — startup banner, version strings, help text,
//!    initial error messages. Often the most semantically meaningful
//!    bytes of the whole run.
//! 2. The most recent chunk — current state, latest request in a dev
//!    server log, latest stack trace.
//!
//! Everything in the middle is usually noise (reload messages, heartbeats,
//! request logs) that the model can safely skip. This buffer keeps the
//! first `HEAD_CAP` bytes and the last `TAIL_CAP` bytes of everything
//! ever written; middle bytes are dropped.
//!
//! This matches codex's `head_tail_buffer.rs` in strategy. Caps chosen
//! to keep worst-case memory per session at ~1 MiB × MAX_SESSIONS.
//!
//! The buffer also supports *incremental draining* via a logical
//! position cursor, so each tool call can return only what's new since
//! the previous call instead of the entire buffer. See `drain_since`.

use std::collections::VecDeque;

/// Bytes kept at the beginning of the stream. Sized to capture typical
/// startup output (help text, version banners, early errors) without
/// eating too much of the overall cap.
pub const HEAD_CAP: usize = 64 * 1024; // 64 KiB

/// Bytes kept at the end of the stream. Much larger than head because
/// "what's happening right now" is usually more valuable than mid-run
/// history, and the tail is what the model will poll repeatedly.
pub const TAIL_CAP: usize = 960 * 1024; // 960 KiB

/// Marker inserted into drained output when the middle was dropped.
/// Written out in full (including newlines) so the model sees an
/// obvious discontinuity rather than two concatenated chunks that look
/// like adjacent output. The `{}` placeholder is the count of dropped
/// bytes.
const TRUNCATION_MARKER_TEMPLATE: &str = "\n[... {} bytes truncated ...]\n";

/// A head-tail bounded byte buffer.
///
/// Writes append bytes; the buffer keeps the first `HEAD_CAP` bytes
/// pinned forever and the most recent `TAIL_CAP` bytes sliding as new
/// data arrives. Once total writes exceed `HEAD_CAP + TAIL_CAP`,
/// `truncated` becomes true and remains true for the rest of the
/// buffer's lifetime.
///
/// Reads use a logical position cursor: `drain_since(pos)` returns
/// everything from `pos` up to the current logical end, where "logical
/// position" is the byte index in the full stream as if nothing had
/// been dropped. This lets tool calls keep a cursor and only see new
/// output between calls.
#[derive(Debug)]
pub struct HeadTailBuffer {
    /// First HEAD_CAP bytes of the stream (possibly less if fewer were
    /// ever written). Never mutated after filling.
    head: Vec<u8>,

    /// Most recent TAIL_CAP bytes. Acts as a ring: pushes go to the
    /// back, oldest pops off the front when full.
    tail: VecDeque<u8>,

    /// Total bytes ever written, across the whole stream. This is the
    /// "logical end" cursor — bytes from `total_written - tail.len()`
    /// to `total_written` live in `tail`; bytes from 0 to `head.len()`
    /// live in `head`; anything strictly between is dropped.
    total_written: u64,

    /// Set once and latched when any byte has been dropped. Tells
    /// drain code to insert a truncation marker between head and tail
    /// when a caller reads from before the drop region.
    truncated: bool,
}

impl HeadTailBuffer {
    pub fn new() -> Self {
        Self {
            head: Vec::with_capacity(1024),
            tail: VecDeque::with_capacity(4096),
            total_written: 0,
            truncated: false,
        }
    }

    /// Append `bytes` to the buffer. Bytes that fit in the head go to
    /// the head; bytes beyond `HEAD_CAP` feed the tail ring. When the
    /// tail is full, oldest bytes are popped and `truncated` flips to
    /// true (if it wasn't already).
    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        // Phase 1: fill head while there's room.
        let mut cursor = 0;
        if self.head.len() < HEAD_CAP {
            let room = HEAD_CAP - self.head.len();
            let take = room.min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
            cursor = take;
        }

        // Phase 2: everything else goes into the tail ring. We may
        // evict head-adjacent bytes from the tail — those are the
        // dropped middle.
        for &b in &bytes[cursor..] {
            if self.tail.len() == TAIL_CAP {
                self.tail.pop_front();
                self.truncated = true;
            }
            self.tail.push_back(b);
        }

        self.total_written += bytes.len() as u64;
    }

    /// Returns the current logical end position (== total bytes
    /// written). A fresh cursor for "everything from now on" is this
    /// value.
    pub fn position(&self) -> u64 {
        self.total_written
    }

    /// Whether any middle bytes have been dropped. Sticky.
    #[allow(dead_code)]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Total bytes ever written (across the whole stream, including
    /// dropped bytes).
    pub fn total_written(&self) -> u64 {
        self.total_written
    }

    /// Return output from logical position `since` up to current end.
    ///
    /// Three cases by how `since` relates to what we still have:
    ///
    /// 1. `since >= total_written`: cursor is at or past the end —
    ///    no new output. Returns empty.
    /// 2. `since >= tail_start`: cursor is inside the tail region —
    ///    we can return exactly those bytes, no head or marker
    ///    needed. This is the hot path for polling: each poll advances
    ///    the cursor, and as long as it lives in the tail, drains are
    ///    precise.
    /// 3. `since < tail_start`: cursor predates the tail, so we owe
    ///    the caller some head bytes, a truncation marker (if dropping
    ///    has occurred and there's a gap), and then the tail.
    ///
    /// The returned `Vec<u8>` owns its bytes. The buffer is NOT
    /// consumed — `drain_since` can be called repeatedly with the
    /// same cursor. Callers advance the cursor separately by reading
    /// `position()` after the call.
    pub fn drain_since(&self, since: u64) -> Vec<u8> {
        if since >= self.total_written {
            return Vec::new();
        }

        let tail_start = self.total_written - self.tail.len() as u64;

        // Case 2: cursor fully inside the tail.
        if since >= tail_start {
            let skip = (since - tail_start) as usize;
            let (front, back) = self.tail.as_slices();
            let mut out = Vec::with_capacity(self.tail.len() - skip);
            if skip < front.len() {
                out.extend_from_slice(&front[skip..]);
                out.extend_from_slice(back);
            } else {
                let back_skip = skip - front.len();
                out.extend_from_slice(&back[back_skip..]);
            }
            return out;
        }

        // Case 3: cursor is before the tail. Return head[since..] +
        // optional marker + full tail.
        let mut out = Vec::new();

        let head_start = since as usize;
        if head_start < self.head.len() {
            out.extend_from_slice(&self.head[head_start..]);
        }

        // If anything was dropped AND there's a gap between where
        // head ended and where tail starts, insert the marker. The
        // gap is (tail_start - head.len()).
        let head_end = self.head.len() as u64;
        if self.truncated && tail_start > head_end {
            let dropped = tail_start - head_end;
            let marker = TRUNCATION_MARKER_TEMPLATE.replace("{}", &dropped.to_string());
            out.extend_from_slice(marker.as_bytes());
        }

        let (front, back) = self.tail.as_slices();
        out.extend_from_slice(front);
        out.extend_from_slice(back);

        out
    }

    /// Return the entire buffer as if `drain_since(0)` was called.
    /// Convenience for tests and one-shot reads.
    #[cfg(test)]
    pub fn snapshot(&self) -> Vec<u8> {
        self.drain_since(0)
    }
}

impl Default for HeadTailBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! Tests for HeadTailBuffer.
    //!
    //! The buffer has three regimes depending on total bytes written:
    //!   A) total <= HEAD_CAP             — head only, no tail,
    //!                                       never truncated.
    //!   B) HEAD_CAP < total <= HEAD_CAP + TAIL_CAP
    //!                                     — head full, tail filling,
    //!                                       not yet truncated.
    //!   C) total > HEAD_CAP + TAIL_CAP    — head full, tail full,
    //!                                       truncated==true, drops
    //!                                       accumulate.
    //!
    //! Tests cover all three regimes and the drain cursor's three
    //! cases (past-end, inside-tail, before-tail).

    use super::*;

    // Small caps for testing — we override the "real" constants by
    // writing byte counts relative to them. To exercise the middle-
    // drop logic without writing a megabyte+ in each test, we use
    // multiples of the head/tail caps.

    #[test]
    fn empty_buffer_has_position_zero() {
        let b = HeadTailBuffer::new();
        assert_eq!(b.position(), 0);
        assert!(!b.truncated());
        assert!(b.snapshot().is_empty());
    }

    #[test]
    fn write_nothing_is_noop() {
        let mut b = HeadTailBuffer::new();
        b.write(b"");
        assert_eq!(b.position(), 0);
        assert!(b.snapshot().is_empty());
    }

    #[test]
    fn regime_a_small_write_stays_in_head() {
        // Under head cap: everything in head, no tail, no truncation.
        let mut b = HeadTailBuffer::new();
        b.write(b"hello world");
        assert_eq!(b.position(), 11);
        assert!(!b.truncated());
        assert_eq!(b.snapshot(), b"hello world");
    }

    #[test]
    fn regime_a_multiple_writes_concatenate() {
        let mut b = HeadTailBuffer::new();
        b.write(b"foo");
        b.write(b" bar");
        b.write(b" baz");
        assert_eq!(b.snapshot(), b"foo bar baz");
        assert_eq!(b.position(), 11);
    }

    #[test]
    fn regime_b_overflow_head_spills_to_tail_without_truncation() {
        // Write exactly HEAD_CAP + a bit: head fills, tail gets the
        // remainder, no drops yet.
        let mut b = HeadTailBuffer::new();
        let head_fill = vec![b'H'; HEAD_CAP];
        let tail_fill = vec![b'T'; 1024];
        b.write(&head_fill);
        b.write(&tail_fill);
        assert_eq!(b.position(), (HEAD_CAP + 1024) as u64);
        assert!(!b.truncated(), "below total cap should not truncate");
        // Snapshot is head + tail concatenated, no marker.
        let snap = b.snapshot();
        assert_eq!(snap.len(), HEAD_CAP + 1024);
        assert!(snap.starts_with(&head_fill));
        assert!(snap.ends_with(&tail_fill));
    }

    #[test]
    fn regime_c_massive_write_triggers_truncation_with_marker() {
        // Write way past HEAD_CAP + TAIL_CAP so the middle is
        // definitely dropped. Use three distinct byte patterns so
        // we can verify head preserved, middle gone, tail kept.
        let mut b = HeadTailBuffer::new();
        let head_fill = vec![b'H'; HEAD_CAP];
        let middle = vec![b'M'; TAIL_CAP * 2]; // will be fully dropped
        let tail_fill = vec![b'T'; TAIL_CAP];
        b.write(&head_fill);
        b.write(&middle);
        b.write(&tail_fill);

        assert!(b.truncated(), "writes past head+tail must set truncated");
        assert_eq!(b.position(), (HEAD_CAP + TAIL_CAP * 2 + TAIL_CAP) as u64);

        let snap = b.snapshot();
        // Head preserved verbatim.
        assert_eq!(&snap[..HEAD_CAP], &head_fill[..]);
        // Marker immediately after head.
        let marker_start = HEAD_CAP;
        let marker_region = &snap[marker_start..];
        let marker_str =
            std::str::from_utf8(marker_region).expect("marker region must be utf8");
        assert!(
            marker_str.starts_with("\n[... "),
            "marker must begin after head; got start: {:?}",
            &marker_str[..40.min(marker_str.len())]
        );
        assert!(
            marker_str.contains("bytes truncated"),
            "marker must describe truncation"
        );
        // Tail at the end.
        assert!(snap.ends_with(&tail_fill));
    }

    #[test]
    fn drain_since_past_end_returns_empty() {
        let mut b = HeadTailBuffer::new();
        b.write(b"hello");
        assert!(b.drain_since(5).is_empty());
        assert!(b.drain_since(9999).is_empty());
    }

    #[test]
    fn drain_since_inside_head_returns_head_suffix() {
        // Cursor is in regime A territory: head-only buffer, cursor
        // partway through. Case 3 of drain_since (since < tail_start)
        // still applies because tail_start for a not-yet-overflowed
        // buffer equals HEAD_CAP - never reached, so we fall into the
        // case-2 branch logic for the all-tail case.
        //
        // Actually: when the buffer is still all-head, tail is empty
        // and tail_start == total_written. So since < tail_start ==
        // since < total_written, which is case 3. head suffix + no
        // marker (not truncated) + empty tail.
        let mut b = HeadTailBuffer::new();
        b.write(b"abcdef");
        let out = b.drain_since(3);
        assert_eq!(out, b"def");
    }

    #[test]
    fn drain_since_in_tail_returns_tail_suffix_only() {
        // Regime B: head full, tail has some bytes. Cursor placed
        // inside the tail → case 2 of drain_since: no head, no
        // marker, just the tail suffix from the cursor.
        let mut b = HeadTailBuffer::new();
        let head_fill = vec![b'H'; HEAD_CAP];
        b.write(&head_fill);
        b.write(b"0123456789");

        // tail_start = HEAD_CAP, total = HEAD_CAP + 10.
        // Cursor at HEAD_CAP + 4 → expect b"456789".
        let out = b.drain_since((HEAD_CAP + 4) as u64);
        assert_eq!(out, b"456789");
    }

    #[test]
    fn drain_since_before_tail_after_truncation_includes_marker() {
        // Regime C: truncated. Cursor at 0 must get head + marker +
        // tail.
        let mut b = HeadTailBuffer::new();
        b.write(&vec![b'H'; HEAD_CAP]);
        b.write(&vec![b'M'; TAIL_CAP * 2]);
        b.write(&vec![b'T'; TAIL_CAP]);

        let out = b.drain_since(0);
        // Must start with head, must end with tail, marker in middle.
        assert!(out.starts_with(&vec![b'H'; HEAD_CAP][..]));
        assert!(out.ends_with(&vec![b'T'; TAIL_CAP][..]));
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("bytes truncated"));
    }

    #[test]
    fn drain_since_repeatable_does_not_consume() {
        // drain_since is idempotent: calling it twice with the same
        // cursor returns identical bytes. Important — the tool
        // handler calls it exactly once per invocation, but bugs
        // that accidentally drain twice shouldn't lose data.
        let mut b = HeadTailBuffer::new();
        b.write(b"persistent");
        let first = b.drain_since(2);
        let second = b.drain_since(2);
        assert_eq!(first, second);
    }

    #[test]
    fn position_advances_exactly_by_bytes_written() {
        // total_written must track the full stream even when bytes
        // are being dropped from the tail. This is load-bearing for
        // the cursor: if position lied about how many bytes had been
        // written, callers would read duplicate or miss bytes.
        let mut b = HeadTailBuffer::new();
        b.write(&vec![0u8; HEAD_CAP]);
        assert_eq!(b.position(), HEAD_CAP as u64);
        b.write(&vec![0u8; TAIL_CAP]);
        assert_eq!(b.position(), (HEAD_CAP + TAIL_CAP) as u64);
        b.write(&vec![0u8; TAIL_CAP * 5]); // causes drops
        assert_eq!(b.position(), (HEAD_CAP + TAIL_CAP + TAIL_CAP * 5) as u64);
    }

    #[test]
    fn incremental_polling_pattern_returns_each_chunk_once() {
        // Simulates how the tool handler uses the cursor across
        // multiple calls: read position before write, perform work,
        // drain_since(prev_position), advance cursor.
        let mut b = HeadTailBuffer::new();
        b.write(b"chunk1 ");
        let pos1 = 0u64;
        let out1 = b.drain_since(pos1);
        assert_eq!(out1, b"chunk1 ");
        let pos2 = b.position();

        b.write(b"chunk2 ");
        let out2 = b.drain_since(pos2);
        assert_eq!(out2, b"chunk2 ");
        let pos3 = b.position();

        b.write(b"chunk3");
        let out3 = b.drain_since(pos3);
        assert_eq!(out3, b"chunk3");

        // Full snapshot should equal concatenation of chunks.
        assert_eq!(b.drain_since(0), b"chunk1 chunk2 chunk3");
    }

    #[test]
    fn truncation_marker_reports_exact_dropped_byte_count() {
        // Regression guard: the marker's number must equal the
        // actual drop count so the model (or a human reading the
        // log) can reason about gap size. Dropped count =
        // tail_start - head.len() = (total - tail.len()) - head.len().
        let mut b = HeadTailBuffer::new();
        b.write(&vec![b'H'; HEAD_CAP]);
        // Write exactly TAIL_CAP + 500 bytes of middle — the first
        // 500 get evicted from tail, so dropped count should be
        // 500 at the point we stop. Then write TAIL_CAP bytes of
        // tail, evicting all remaining middle.
        b.write(&vec![b'M'; TAIL_CAP + 500]);
        b.write(&vec![b'T'; TAIL_CAP]);

        // Total = HEAD_CAP + TAIL_CAP + 500 + TAIL_CAP.
        // tail_start = total - TAIL_CAP = HEAD_CAP + TAIL_CAP + 500.
        // dropped = tail_start - HEAD_CAP = TAIL_CAP + 500.
        let expected_dropped = TAIL_CAP + 500;
        let out = b.drain_since(0);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.contains(&format!("{} bytes truncated", expected_dropped)),
            "expected marker for {} bytes; got: {:?}",
            expected_dropped,
            &s[HEAD_CAP..HEAD_CAP + 100.min(s.len() - HEAD_CAP)]
        );
    }
}
