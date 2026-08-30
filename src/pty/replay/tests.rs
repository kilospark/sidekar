use super::*;

#[test]
fn retains_everything_under_capacity() {
    let mut buf = ReplayBuffer::new(64);
    buf.push(b"hello ");
    buf.push(b"world");
    assert_eq!(buf.snapshot(), b"hello world");
    assert_eq!(buf.len(), 11);
}

#[test]
fn drops_whole_chunks_and_keeps_at_least_capacity() {
    let mut buf = ReplayBuffer::new(10);
    for _ in 0..6 {
        buf.push(b"abcd");
    }
    // Never slices a retained chunk, so the total stays a multiple of 4.
    assert!(buf.len() >= 10, "kept {} bytes", buf.len());
    assert_eq!(buf.len() % 4, 0);
    assert!(buf.snapshot().ends_with(b"abcd"));
}

#[test]
fn never_splits_an_escape_sequence() {
    let mut buf = ReplayBuffer::new(16);
    for _ in 0..10 {
        buf.push(b"\x1b[?2004h");
    }
    let snapshot = buf.snapshot();
    assert_eq!(snapshot.len() % 8, 0);
    assert!(snapshot.starts_with(b"\x1b["));
}

#[test]
fn oversized_single_chunk_is_tail_trimmed() {
    let mut buf = ReplayBuffer::new(8);
    let big: Vec<u8> = (0..32u8).collect();
    buf.push(&big);
    assert_eq!(buf.len(), 8);
    assert_eq!(buf.snapshot(), (24..32u8).collect::<Vec<_>>());
}

#[test]
fn empty_pushes_are_ignored() {
    let mut buf = ReplayBuffer::new(16);
    buf.push(b"");
    assert!(buf.is_empty());
}
