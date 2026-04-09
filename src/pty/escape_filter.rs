/// Filter out OSC color query/response sequences (OSC 10-19) from input data.
/// These are terminal color queries (ESC ] 10 ; ? BEL) and responses (ESC ] 10 ; rgb:... BEL)
/// that can leak through when the browser's xterm.js queries colors or when the host
/// terminal responds to queries. Filtering prevents them from appearing as literal text.
pub(crate) fn filter_osc_color_sequences(data: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    // Fast path: no ESC in data
    if !data.contains(&0x1b) {
        return std::borrow::Cow::Borrowed(data);
    }

    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;

    while i < data.len() {
        // Look for ESC ] (0x1b 0x5d) - start of OSC sequence
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == 0x5d {
            // Check for OSC 10-19 (color-related sequences)
            if i + 2 < data.len() && data[i + 2] == b'1' {
                // OSC 10-19: look for the next digit
                let is_color_osc = if i + 3 < data.len() {
                    let d = data[i + 3];
                    // OSC 10, 11, 12, ... 19 followed by ; or terminator
                    d == b';'
                        || d == b'0'
                        || d == b'1'
                        || d == b'2'
                        || d == b'3'
                        || d == b'4'
                        || d == b'5'
                        || d == b'6'
                        || d == b'7'
                        || d == b'8'
                        || d == b'9'
                        || d == 0x07
                } else {
                    false
                };

                if is_color_osc {
                    // Skip the entire OSC sequence until BEL or ST
                    let mut j = i + 2;
                    while j < data.len() {
                        if data[j] == 0x07 {
                            i = j + 1;
                            break;
                        }
                        if data[j] == 0x1b && j + 1 < data.len() && data[j + 1] == b'\\' {
                            i = j + 2;
                            break;
                        }
                        j += 1;
                    }
                    if j >= data.len() {
                        // Unterminated sequence, skip rest
                        i = data.len();
                    }
                    continue;
                }
            }
        }

        out.push(data[i]);
        i += 1;
    }

    if out.len() == data.len() {
        std::borrow::Cow::Borrowed(data)
    } else {
        std::borrow::Cow::Owned(out)
    }
}

/// Rewrite OSC 0/2 title sequences (ESC ] 0; ... BEL / ESC ] 2; ... BEL)
/// to prepend the nick prefix, so the terminal title always shows the agent nickname.
pub(crate) fn rewrite_osc_titles<'a>(data: &'a [u8], prefix: &str) -> std::borrow::Cow<'a, [u8]> {
    // Fast path: no ESC in data, nothing to rewrite
    if !data.contains(&0x1b) {
        return std::borrow::Cow::Borrowed(data);
    }

    let mut out = Vec::with_capacity(data.len() + 64);
    let mut i = 0;

    while i < data.len() {
        // Look for ESC ] (0x1b 0x5d)
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == 0x5d {
            // Check for OSC 0; or OSC 2; (set window title)
            if i + 3 < data.len()
                && (data[i + 2] == b'0' || data[i + 2] == b'2')
                && data[i + 3] == b';'
            {
                // Find the terminator: BEL (0x07) or ST (ESC \)
                let start = i + 4; // start of title text
                let mut end = start;
                while end < data.len() {
                    if data[end] == 0x07 {
                        break;
                    }
                    if data[end] == 0x1b && end + 1 < data.len() && data[end + 1] == b'\\' {
                        break;
                    }
                    end += 1;
                }

                if end < data.len() {
                    // Write rewritten OSC: ESC ] <digit> ; <prefix><original title> <terminator>
                    out.push(0x1b);
                    out.push(0x5d);
                    out.push(data[i + 2]); // '0' or '2'
                    out.push(b';');
                    out.extend_from_slice(prefix.as_bytes());
                    out.extend_from_slice(&data[start..end]);

                    if data[end] == 0x07 {
                        out.push(0x07);
                        i = end + 1;
                    } else {
                        // ST: ESC backslash
                        out.push(0x1b);
                        out.push(b'\\');
                        i = end + 2;
                    }
                    continue;
                }
            }
        }

        out.push(data[i]);
        i += 1;
    }

    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_osc_titles_prepends_formatted_nick_prefix() {
        let raw = b"\x1b]0;claude\x07";
        let rewritten = rewrite_osc_titles(raw, "borzoi - ");
        assert_eq!(rewritten.as_ref(), b"\x1b]0;borzoi - claude\x07");
    }
}
