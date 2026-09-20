//! Google Calendar over the API.

use anyhow::Result;
use serde_json::{Value, json};

const BASE: &str = "https://www.googleapis.com/calendar/v3";

pub struct Event {
    pub id: String,
    pub summary: String,
    pub start: String,
    pub end: String,
    pub attendees: usize,
}

/// Upcoming events, soonest first.
pub async fn list(calendar: &str, days: u32, limit: usize) -> Result<Vec<Event>> {
    let now = chrono_now();
    let until = rfc3339_in_days(days);
    let url = format!(
        "{BASE}/calendars/{}/events?timeMin={}&timeMax={}&maxResults={}\
         &singleEvents=true&orderBy=startTime",
        urlencoding::encode(calendar),
        urlencoding::encode(&now),
        urlencoding::encode(&until),
        limit.clamp(1, 250)
    );
    let res = super::api_get(&url).await?;
    Ok(res
        .get("items")
        .and_then(|i| i.as_array())
        .map(|a| a.iter().map(to_event).collect())
        .unwrap_or_default())
}

pub async fn create(
    calendar: &str,
    summary: &str,
    start: &str,
    end: &str,
    attendees: &[String],
) -> Result<String> {
    let mut body = json!({
        "summary": summary,
        "start": time_field(start),
        "end": time_field(end),
    });
    if !attendees.is_empty() {
        body["attendees"] = json!(
            attendees
                .iter()
                .map(|e| json!({"email": e}))
                .collect::<Vec<_>>()
        );
    }
    let res = super::api_post(
        &format!("{BASE}/calendars/{}/events", urlencoding::encode(calendar)),
        &body,
    )
    .await?;
    Ok(res
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or_default()
        .to_string())
}

pub(crate) fn to_event(v: &Value) -> Event {
    Event {
        id: v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        summary: v
            .get("summary")
            .and_then(|x| x.as_str())
            .unwrap_or("(no title)")
            .to_string(),
        start: when(v.get("start")),
        end: when(v.get("end")),
        attendees: v
            .get("attendees")
            .and_then(|a| a.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
    }
}

/// An event carries `dateTime` when it is timed and `date` when it is all-day.
pub(crate) fn when(slot: Option<&Value>) -> String {
    let Some(s) = slot else {
        return String::new();
    };
    s.get("dateTime")
        .and_then(|d| d.as_str())
        .or_else(|| s.get("date").and_then(|d| d.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// A bare `YYYY-MM-DD` means an all-day event; anything else is a timestamp.
pub(crate) fn time_field(value: &str) -> Value {
    if value.len() == 10 && value.chars().filter(|c| *c == '-').count() == 2 {
        json!({"date": value})
    } else {
        json!({"dateTime": value})
    }
}

fn chrono_now() -> String {
    rfc3339_in_days(0)
}

fn rfc3339_in_days(days: u32) -> String {
    let secs = crate::message::epoch_secs() + u64::from(days) * 86_400;
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&t, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests;
