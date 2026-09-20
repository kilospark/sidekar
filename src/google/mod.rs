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

/// GET a Google API endpoint with the caller's token.
pub(crate) async fn api_get(url: &str) -> Result<Value> {
    let token = auth::access_token().await?;
    let res = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await?;
    read_json(res, url).await
}

/// POST JSON to a Google API endpoint with the caller's token.
pub(crate) async fn api_post(url: &str, body: &Value) -> Result<Value> {
    let token = auth::access_token().await?;
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
