use crate::providers::RateLimitSnapshot;

pub fn format_rate_limit(rl: &RateLimitSnapshot) -> Option<String> {
    let mut parts = Vec::new();

    // Anthropic OAuth (Pro/Team) unified 5h + 7d caps — primary signal for subscription users.
    if let Some(pct) = rl.util_5h_pct {
        let mut s = format!("5h {}%", pct);
        if let Some(reset) = rl.reset_5h_at {
            if let Some(t) = format_reset_time(reset) {
                s.push_str(&format!(" (resets {})", t));
            }
        }
        parts.push(s);
    }
    if let Some(pct) = rl.util_7d_pct {
        let mut s = format!("7d {}%", pct);
        if let Some(reset) = rl.reset_7d_at {
            if let Some(t) = format_reset_time(reset) {
                s.push_str(&format!(" (resets {})", t));
            }
        }
        parts.push(s);
    }

    // API-tier (per-minute) limits — used by raw API key billing and other providers.
    // Hide if unified is present (subscription users don't care about ITPM).
    if rl.util_5h_pct.is_none() && rl.util_7d_pct.is_none() {
        if let (Some(rem), Some(lim)) = (rl.requests_remaining, rl.requests_limit) {
            parts.push(format!("req {}/{}", rem, lim));
        }
        if let (Some(rem), Some(lim)) = (rl.tokens_remaining, rl.tokens_limit) {
            parts.push(format!("tok {}/{}", short_num(rem), short_num(lim)));
        }
        if let Some(reset) = rl.reset_at {
            if let Some(s) = format_reset_time(reset) {
                parts.push(format!("reset {}", s));
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

fn short_num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

fn format_reset_time(epoch_secs: u64) -> Option<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    if epoch_secs <= now {
        return Some("now".into());
    }
    let delta = epoch_secs - now;
    if delta < 60 {
        Some(format!("{}s", delta))
    } else if delta < 3600 {
        Some(format!("{}m", delta / 60))
    } else {
        Some(format!("{}h{}m", delta / 3600, (delta % 3600) / 60))
    }
}

/// Returns " · req X/Y · tok A/B · reset HH:MM" or empty string for direct interpolation.
pub fn format_compact(rl: Option<&RateLimitSnapshot>) -> String {
    match rl.and_then(format_rate_limit) {
        Some(s) => format!(" · {}", s),
        None => String::new(),
    }
}
