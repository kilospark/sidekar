//! Answer terminal capability queries when no real terminal is attached.
//!
//! Agents probe the terminal on startup — device attributes, device status,
//! foreground/background colors — and block or render badly if nothing answers.
//! With a TTY on stdin the real terminal answers and sidekar stays out of the
//! way (see the passthrough in `event_loop`). Detached sessions have no such
//! terminal, so sidekar answers on its behalf, the way paseo's headless
//! emulator does.
//!
//! Cursor position (DSR 6) is reported as the home cell: sidekar keeps no
//! screen model, and a plausible answer unblocks a probing agent where silence
//! would hang it.

/// Response advertising a VT220 with ANSI colors — the same class paseo reports.
const DA1_RESPONSE: &[u8] = b"\x1b[?62;4;22c";
/// DSR 5: terminal is OK.
const DSR_OK: &[u8] = b"\x1b[0n";
/// Reported cursor position, in the absence of a screen model.
const CURSOR_POSITION: &[u8] = b"\x1b[1;1R";
const CURSOR_POSITION_PRIVATE: &[u8] = b"\x1b[?1;1R";

/// Canned answers for OSC color queries, matching a dark default theme.
const OSC_COLOR_RESPONSES: &[(&str, &str)] = &[
    ("10", "rgb:ffff/ffff/ffff"), // foreground
    ("11", "rgb:0000/0000/0000"), // background
    ("12", "rgb:ffff/ffff/ffff"), // cursor
];

/// Maximum bytes of a partial sequence carried between `feed` calls.
const MAX_PARSE_TAIL: usize = 64;

/// Scans agent output for capability queries and produces the replies.
#[derive(Debug, Default)]
pub(crate) struct QueryResponder {
    tail: Vec<u8>,
}

impl QueryResponder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed PTY output; returns bytes to write back to the PTY master, if any.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut data = std::mem::take(&mut self.tail);
        data.extend_from_slice(bytes);

        let mut out: Vec<u8> = Vec::new();
        let mut i = 0usize;
        let mut tail_from: Option<usize> = None;

        while i < data.len() {
            if data[i] != 0x1b {
                i += 1;
                continue;
            }
            let esc_start = i;
            if i + 1 >= data.len() {
                tail_from = Some(esc_start);
                break;
            }
            match data[i + 1] {
                b'[' => {
                    i += 2;
                    let prefix = if i < data.len() && matches!(data[i], b'?' | b'>' | b'=' | b'<') {
                        let p = data[i];
                        i += 1;
                        Some(p)
                    } else {
                        None
                    };
                    let params_start = i;
                    while i < data.len() && matches!(data[i], b'0'..=b'9' | b';') {
                        i += 1;
                    }
                    if i >= data.len() {
                        tail_from = Some(esc_start);
                        break;
                    }
                    let params = &data[params_start..i];
                    if let Some(reply) = csi_reply(prefix, params, data[i]) {
                        out.extend_from_slice(reply);
                    }
                    i += 1;
                }
                b']' => {
                    let body_start = i + 2;
                    let mut j = body_start;
                    let mut end: Option<(usize, usize)> = None; // (body_end, next_index)
                    while j < data.len() {
                        if data[j] == 0x07 {
                            end = Some((j, j + 1));
                            break;
                        }
                        if data[j] == 0x1b && j + 1 < data.len() && data[j + 1] == b'\\' {
                            end = Some((j, j + 2));
                            break;
                        }
                        j += 1;
                    }
                    match end {
                        Some((body_end, next)) => {
                            if let Some(reply) = osc_reply(&data[body_start..body_end]) {
                                out.extend_from_slice(reply.as_bytes());
                            }
                            i = next;
                        }
                        None => {
                            tail_from = Some(esc_start);
                            break;
                        }
                    }
                }
                _ => i += 2,
            }
        }

        if let Some(start) = tail_from {
            let pending = &data[start..];
            if pending.len() <= MAX_PARSE_TAIL {
                self.tail = pending.to_vec();
            }
        }

        (!out.is_empty()).then_some(out)
    }
}

fn csi_reply(prefix: Option<u8>, params: &[u8], final_byte: u8) -> Option<&'static [u8]> {
    match final_byte {
        // DA1: `CSI c` or `CSI 0 c`. Private-prefixed forms are responses, not queries.
        b'c' if prefix.is_none() && (params.is_empty() || params == b"0") => Some(DA1_RESPONSE),
        b'n' => match (prefix, params) {
            (None, b"5") => Some(DSR_OK),
            (None, b"6") => Some(CURSOR_POSITION),
            (Some(b'?'), b"6") => Some(CURSOR_POSITION_PRIVATE),
            _ => None,
        },
        _ => None,
    }
}

/// OSC color queries look like `10;?`; anything else is a set, not a query.
fn osc_reply(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    let (code, value) = text.split_once(';')?;
    if value.trim() != "?" {
        return None;
    }
    let response = OSC_COLOR_RESPONSES
        .iter()
        .find(|(c, _)| *c == code.trim())
        .map(|(_, r)| *r)?;
    Some(format!("\x1b]{code};{response}\x1b\\"))
}

#[cfg(test)]
mod tests;
