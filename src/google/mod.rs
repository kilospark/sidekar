//! Gmail, Drive and Calendar over their real APIs.
//!
//! Driving these through a browser worked but was flaky: pages change shape,
//! selectors rot, and anything behind a login is one re-auth away from stalling.
//! These call the APIs directly with a token from [`auth`].

pub mod auth;
pub mod calendar;
pub mod docs;
pub mod drive;
pub mod gmail;
pub mod sheets;

use anyhow::{Result, bail};
use serde_json::Value;

/// Which of the five APIs a token can actually reach.
///
/// Each is a real, cheap call rather than a config lookup: an API can be enabled
/// on the project and still fail because the scope was never granted, and the
/// two failures look identical from the outside until you try.
pub async fn probe(token: &auth::TokenRef) -> Vec<(&'static str, Result<()>)> {
    let checks: [(&str, &str); 5] = [
        (
            "gmail",
            "https://gmail.googleapis.com/gmail/v1/users/me/profile",
        ),
        (
            "drive",
            "https://www.googleapis.com/drive/v3/about?fields=user",
        ),
        (
            "calendar",
            "https://www.googleapis.com/calendar/v3/users/me/calendarList?maxResults=1",
        ),
        (
            "sheets",
            "https://sheets.googleapis.com/v4/spreadsheets/0000000000000000000000000000",
        ),
        (
            "docs",
            "https://docs.googleapis.com/v1/documents/0000000000000000000000000000",
        ),
    ];
    let mut out = Vec::new();
    for (name, url) in checks {
        let r = api_get(token, url).await.map(|_| ());
        // Sheets and Docs are probed with an id that cannot exist. A 404 proves
        // the API is on and the scope granted, which is what we are asking; only
        // 401 and 403 mean it is not reachable.
        let r = match r {
            Err(e) if matches!(name, "sheets" | "docs") && is_not_found(&e) => Ok(()),
            other => other,
        };
        out.push((name, r));
    }
    out
}

fn is_not_found(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("404") || s.to_lowercase().contains("not found")
}

/// GET a Google API endpoint with the caller's token.
pub(crate) async fn api_get(token: &auth::TokenRef, url: &str) -> Result<Value> {
    let token = auth::access_token_for(token).await?;
    let res = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await?;
    read_json(res, url).await
}

/// POST JSON to a Google API endpoint with the caller's token.
pub(crate) async fn api_post(token: &auth::TokenRef, url: &str, body: &Value) -> Result<Value> {
    let token = auth::access_token_for(token).await?;
    let res = reqwest::Client::new()
        .post(url)
        .bearer_auth(token)
        .json(body)
        .send()
        .await?;
    read_json(res, url).await
}

/// Surface Google's own error text rather than a bare status code.
///
/// Its messages name the missing scope or the disabled API, which is the
/// difference between a one-line fix and an afternoon.
async fn read_json(res: reqwest::Response, url: &str) -> Result<Value> {
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| text.chars().take(300).collect());
        bail!("{status} from {url}: {detail}");
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_str(&text)?)
}
