use anyhow::Result;
use std::time::{SystemTime, UNIX_EPOCH};
use super::oauth::{load_credentials, save_credentials};

pub fn current_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Mark a credential locked until the given Unix timestamp.
pub fn mark_locked(kv_key: &str, until_epoch: u64, message: &str) -> Result<()> {
    let mut creds = match load_credentials(kv_key)? {
        Some(c) => c,
        None => return Ok(()),
    };
    if !creds.metadata.is_object() {
        creds.metadata = serde_json::json!({});
    }
    creds.metadata["locked_until"] = serde_json::json!(until_epoch);
    let msg = message.chars().take(200).collect::<String>();
    creds.metadata["locked_message"] = serde_json::json!(msg);
    save_credentials(kv_key, &creds)
}

/// Anthropic 429: extract reset epoch from retry-after header or ISO timestamp in body.
pub fn parse_anthropic_lock(retry_after: Option<&str>, body: &str) -> Option<u64> {
    if let Some(ra) = retry_after { if let Some(e) = parse_retry_after(ra) { return Some(e); } }
    // Body usually contains: "...resets at 2026-04-26T18:32:00Z" or similar.
    for token in body.split(|c: char| !(c.is_ascii_alphanumeric() || c==':' || c=='-' || c=='.' || c=='Z' || c=='T')) {
        if token.len() >= 19 && token.contains('T') && token.ends_with('Z') {
            if let Some(e) = parse_iso8601(token) { return Some(e); }
        }
    }
    None
}

/// Read the locked-until epoch if still in the future. Returns None if not locked or expired.
pub fn read_locked(kv_key: &str) -> Option<u64> {
    let creds = load_credentials(kv_key).ok().flatten()?;
    let until = creds.metadata.get("locked_until")?.as_u64()?;
    if until > current_epoch() { Some(until) } else { None }
}

/// Clear the lock (call after a successful response).
pub fn clear_locked(kv_key: &str) -> Result<()> {
    let mut creds = match load_credentials(kv_key)? {
        Some(c) => c,
        None => return Ok(()),
    };
    if let Some(obj) = creds.metadata.as_object_mut() {
        let removed = obj.remove("locked_until").is_some();
        obj.remove("locked_message");
        if !removed { return Ok(()); }
    } else { return Ok(()); }
    save_credentials(kv_key, &creds)
}

/// Parse a "retry-after" header (HTTP-date OR seconds-from-now) into an absolute epoch.
pub fn parse_retry_after(value: &str) -> Option<u64> {
    if let Ok(secs) = value.trim().parse::<u64>() {
        return Some(current_epoch() + secs);
    }
    parse_http_date(value)
}

/// Parse an ISO-8601 timestamp (e.g. "2026-04-26T18:32:00Z") into epoch seconds.
pub fn parse_iso8601(s: &str) -> Option<u64> {
    let s = s.trim();
    let s = s.strip_suffix("Z").unwrap_or(s);
    let (date, time) = s.split_once("T")?;
    let mut dparts = date.split("-");
    let y: i64 = dparts.next()?.parse().ok()?;
    let mo: u32 = dparts.next()?.parse().ok()?;
    let d: u32 = dparts.next()?.parse().ok()?;
    let mut tparts = time.split(":");
    let h: u32 = tparts.next()?.parse().ok()?;
    let mi: u32 = tparts.next()?.parse().ok()?;
    let sec: u32 = tparts.next().and_then(|t| t.split(".").next()).and_then(|t| t.parse().ok()).unwrap_or(0);
    Some(date_to_epoch(y, mo, d, h, mi, sec))
}

fn parse_http_date(_s: &str) -> Option<u64> {
    // HTTP-date is rare for our use; rely on numeric retry-after instead.
    None
}

fn is_leap(y: i64) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + (d as i64) - 1;
    let doe = yoe as i64 * 365 + (yoe / 4) as i64 - (yoe / 100) as i64 + doy;
    era * 146097 + doe - 719468
}

fn date_to_epoch(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> u64 {
    let _ = is_leap;
    let days = days_from_civil(y, mo, d);
    (days * 86400 + (h as i64) * 3600 + (mi as i64) * 60 + s as i64) as u64
}
