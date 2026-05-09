use crate::providers::RateLimitSnapshot;

pub fn format_rate_limit(rl: &RateLimitSnapshot) -> Option<String> {
    let mut parts = Vec::new();

    // Anthropic OAuth (Pro/Team) unified 5h + 7d caps — primary signal for subscription users.
    if let Some(pct) = rl.util_5h_pct {
        let mut s = format!("5h {}%", pct);
        if let Some(reset) = rl.reset_5h_at
            && let Some(t) = format_reset_time(reset)
        {
            s.push_str(&format!(" (resets {})", t));
        }
        parts.push(s);
    }
    if let Some(pct) = rl.util_7d_pct {
        let mut s = format!("7d {}%", pct);
        if let Some(reset) = rl.reset_7d_at
            && let Some(t) = format_reset_time(reset)
        {
            s.push_str(&format!(" (resets {})", t));
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
        if let Some(reset) = rl.reset_at
            && let Some(s) = format_reset_time(reset)
        {
            parts.push(format!("reset {}", s));
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stress {
    Ok,
    Warn,
    Critical,
}

fn stress_tier(rl: &RateLimitSnapshot) -> Stress {
    let mut t = Stress::Ok;
    if let Some(u) = rl.util_5h_pct {
        if u >= 92 {
            return Stress::Critical;
        }
        if u >= 75 {
            t = Stress::Warn;
        }
    }
    if let Some(u) = rl.util_7d_pct {
        if u >= 92 {
            return Stress::Critical;
        }
        if u >= 75 {
            t = t.max(Stress::Warn);
        }
    }
    if let (Some(rem), Some(lim)) = (rl.tokens_remaining, rl.tokens_limit) {
        if lim > 0 {
            let left = rem.saturating_mul(100) / lim;
            if left < 10 {
                return Stress::Critical;
            }
            if left < 25 {
                t = t.max(Stress::Warn);
            }
        }
    }
    if let (Some(rem), Some(lim)) = (rl.requests_remaining, rl.requests_limit) {
        if lim > 0 {
            let left = rem.saturating_mul(100) / lim;
            if left < 12 {
                return Stress::Critical;
            }
            if left < 35 {
                t = t.max(Stress::Warn);
            }
        }
    }
    t
}

/// Dim bracketed line when `wham/usage` filled cache after stream omitted quota.
pub(super) fn format_plan_quota_fallback_line(snap: &RateLimitSnapshot) -> Option<String> {
    let text = format_rate_limit(snap)?;
    let mid = match stress_tier(snap) {
        Stress::Critical => format!("\x1b[0m · \x1b[31m{text}\x1b[0m"),
        Stress::Warn => format!("\x1b[0m · \x1b[33m{text}\x1b[0m"),
        Stress::Ok => format!("\x1b[0m · {text}\x1b[0m"),
    };
    Some(format!("\x1b[2m[plan quota{mid}\x1b[2m]\x1b[0m"))
}

/// Colored ` · quota…` segment: ends with `\x1b[0m`; caller re-applies dim before `]`.
pub(super) fn quota_colored_mid(rl: Option<&RateLimitSnapshot>) -> String {
    let Some(rl) = rl else {
        return String::new();
    };
    let Some(text) = format_rate_limit(rl) else {
        return String::new();
    };
    match stress_tier(rl) {
        Stress::Critical => format!("\x1b[0m · \x1b[31m{text}\x1b[0m"),
        Stress::Warn => format!("\x1b[0m · \x1b[33m{text}\x1b[0m"),
        Stress::Ok => format!("\x1b[0m · {text}\x1b[0m"),
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
