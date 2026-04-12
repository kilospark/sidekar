//! Minimal RFC 6455 WebSocket frame reader for MITM logging.
//!
//! We already sit in the middle of a decrypted TLS tunnel and forward bytes
//! bidirectionally between client and server. To capture streaming traffic
//! (e.g. codex's `wss://chatgpt.com/backend-api/codex/responses`) in
//! `proxy_log`, we need to parse the frame stream — raw `tokio::io::copy`
//! gives us nothing to log.
//!
//! Responsibilities:
//!   - Read a single frame from an `AsyncRead`, returning both the decoded
//!     frame and the raw bytes that were read (so the caller can forward
//!     the exact bytes unchanged to the peer).
//!   - Reassemble fragmented messages (continuation frames).
//!   - Inflate permessage-deflate payloads, optionally preserving decoder
//!     state across messages (context takeover).
//!
//! We do NOT re-frame or re-compress: raw bytes pass through untouched.
//! This module is strictly observational.
//!
//! References:
//!   - RFC 6455 §5 (framing)
//!   - RFC 7692 §7 (permessage-deflate extension)

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const OP_CONTINUATION: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

/// One parsed WebSocket frame. `payload` is unmasked.
pub struct Frame {
    pub fin: bool,
    pub rsv1: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
}

/// Read one frame from `reader`. Returns `(raw_bytes, frame)` where
/// `raw_bytes` is the exact on-wire byte sequence (header + extended length
/// + mask key + masked payload) that the caller must forward to the peer.
///   Returns `Ok(None)` on clean EOF before any header bytes.
pub async fn read_frame<R>(reader: &mut R) -> Result<Option<(Vec<u8>, Frame)>>
where
    R: AsyncRead + Unpin,
{
    let mut raw: Vec<u8> = Vec::with_capacity(64);

    let mut header = [0u8; 2];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    raw.extend_from_slice(&header);

    let b0 = header[0];
    let b1 = header[1];
    let fin = (b0 & 0x80) != 0;
    let rsv1 = (b0 & 0x40) != 0;
    let opcode = b0 & 0x0F;
    let masked = (b1 & 0x80) != 0;
    let len7 = b1 & 0x7F;

    let payload_len: usize = if len7 == 126 {
        let mut ext = [0u8; 2];
        reader.read_exact(&mut ext).await?;
        raw.extend_from_slice(&ext);
        u16::from_be_bytes(ext) as usize
    } else if len7 == 127 {
        let mut ext = [0u8; 8];
        reader.read_exact(&mut ext).await?;
        raw.extend_from_slice(&ext);
        u64::from_be_bytes(ext) as usize
    } else {
        len7 as usize
    };

    let mut mask_key = [0u8; 4];
    if masked {
        reader.read_exact(&mut mask_key).await?;
        raw.extend_from_slice(&mask_key);
    }

    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        reader.read_exact(&mut payload).await?;
        raw.extend_from_slice(&payload);
    }

    if masked {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask_key[i % 4];
        }
    }

    Ok(Some((
        raw,
        Frame {
            fin,
            rsv1,
            opcode,
            payload,
        },
    )))
}

/// Complete application-level message after reassembling fragments.
pub enum Message {
    /// Text or binary data message, reassembled from one or more frames.
    Data {
        opcode: u8,
        /// RSV1 set on first fragment → permessage-deflate compressed.
        compressed: bool,
        payload: Vec<u8>,
    },
    Close,
    Ping,
    Pong,
}

/// Stateful accumulator that reassembles fragmented data messages.
/// Control frames (ping/pong/close) are returned immediately.
pub struct MessageAcc {
    buf: Vec<u8>,
    first_opcode: u8,
    compressed: bool,
    active: bool,
}

impl MessageAcc {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            first_opcode: 0,
            compressed: false,
            active: false,
        }
    }

    pub fn push(&mut self, frame: Frame) -> Option<Message> {
        match frame.opcode {
            OP_PING => Some(Message::Ping),
            OP_PONG => Some(Message::Pong),
            OP_CLOSE => Some(Message::Close),
            OP_TEXT | OP_BINARY => {
                // New data message: start fresh (be lenient if previous was unfinished).
                self.first_opcode = frame.opcode;
                self.compressed = frame.rsv1;
                self.buf = frame.payload;
                if frame.fin {
                    self.active = false;
                    Some(Message::Data {
                        opcode: self.first_opcode,
                        compressed: self.compressed,
                        payload: std::mem::take(&mut self.buf),
                    })
                } else {
                    self.active = true;
                    None
                }
            }
            OP_CONTINUATION => {
                if !self.active {
                    return None;
                }
                self.buf.extend_from_slice(&frame.payload);
                if frame.fin {
                    self.active = false;
                    Some(Message::Data {
                        opcode: self.first_opcode,
                        compressed: self.compressed,
                        payload: std::mem::take(&mut self.buf),
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Inflate a permessage-deflate message payload. Per RFC 7692 §7.2.2 the
/// `0x00 0x00 0xff 0xff` tail is appended before feeding the raw deflate
/// stream. Pass a persistent `Decompress` for context takeover, or a fresh
/// one for `no_context_takeover`. Output is capped at `max_out` bytes.
pub fn inflate_with(
    decomp: &mut flate2::Decompress,
    compressed: &[u8],
    max_out: usize,
) -> Result<Vec<u8>> {
    let mut input = Vec::with_capacity(compressed.len() + 4);
    input.extend_from_slice(compressed);
    input.extend_from_slice(&[0x00, 0x00, 0xff, 0xff]);

    let start_in = decomp.total_in();
    let mut out: Vec<u8> = Vec::with_capacity(compressed.len() * 2);
    let mut scratch = vec![0u8; 16 * 1024];

    // Loop until the decoder has consumed all input AND stopped producing
    // output. Exiting as soon as `total_in` catches up is NOT safe: zlib may
    // have emitted less than the scratch buffer allowed because it ran out of
    // input to read, yet the internal state still holds no more bytes to
    // flush — but there are also cases (large back-references that spill past
    // the 16 KB scratch) where inflate has CONSUMED all input but still has
    // buffered output to emit. Call inflate with empty input until it
    // produces zero bytes, then stop.
    loop {
        let consumed_so_far = (decomp.total_in() - start_in) as usize;
        let remaining = if consumed_so_far >= input.len() {
            &[][..]
        } else {
            &input[consumed_so_far..]
        };
        let out_before = decomp.total_out();
        let status = decomp.decompress(remaining, &mut scratch, flate2::FlushDecompress::Sync)?;
        let produced = (decomp.total_out() - out_before) as usize;
        if produced > 0 {
            if out.len() + produced > max_out {
                let take = max_out.saturating_sub(out.len());
                out.extend_from_slice(&scratch[..take]);
                break;
            }
            out.extend_from_slice(&scratch[..produced]);
        }
        // All input drained AND nothing more to flush: we're done.
        if remaining.is_empty() && produced == 0 {
            break;
        }
        // Pathological guard: no progress at all.
        if produced == 0 && matches!(status, flate2::Status::BufError) {
            break;
        }
        if matches!(status, flate2::Status::StreamEnd) {
            break;
        }
    }

    Ok(out)
}
