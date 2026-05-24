//! Human-like typing cadence for desktop `type`.

#[derive(Debug, Clone, Copy)]
pub enum TypingProfile {
    Linear { delay_ms: u32 },
    Human { wpm: u32 },
}

impl TypingProfile {
    pub fn from_args(profile: &str, delay_ms: Option<u32>, wpm: Option<u32>) -> Self {
        match profile.to_ascii_lowercase().as_str() {
            "human" => Self::Human {
                wpm: wpm.unwrap_or(120).clamp(80, 220),
            },
            _ => Self::Linear {
                delay_ms: delay_ms.unwrap_or(5).clamp(0, 200),
            },
        }
    }

    pub fn delay_before_char(&self, ch: char, word_chars_since_pause: u32) -> u32 {
        match self {
            Self::Linear { delay_ms } => *delay_ms,
            Self::Human { wpm } => {
                let base = 60_000 / (*wpm * 5).max(1);
                let mut delay = base;
                if ch.is_whitespace() || ".,;:!?".contains(ch) {
                    delay = delay.saturating_mul(135) / 100;
                }
                if word_chars_since_pause > 0 && word_chars_since_pause % 12 == 0 {
                    delay = delay.saturating_add(350);
                }
                let jitter = (delay / 5).max(1);
                delay.saturating_add(jitter)
            }
        }
    }
}
