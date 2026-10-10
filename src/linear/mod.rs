//! Linear over its GraphQL API.
//!
//! One endpoint, `https://api.linear.app/graphql`, for everything. The
//! credential is either a personal API key (sent bare in `Authorization`, as
//! Linear documents) or an OAuth access token (sent as `Bearer`); [`auth`]
//! decides which and keeps OAuth tokens fresh.

pub mod api;
pub mod auth;

use anyhow::{Result, bail};
use serde_json::{Value, json};

pub const GRAPHQL: &str = "https://api.linear.app/graphql";

pub struct Linear {
    http: reqwest::Client,
    url: String,
    authorization: String,
}

impl Linear {
    /// A client for the stored token, refreshing an OAuth token first if due.
    pub async fn connect(token: &auth::TokenRef) -> Result<Self> {
        Ok(Self::new(auth::authorization_for(token).await?))
    }

    /// `authorization` is the whole header value: `lin_api_…` or `Bearer …`.
    pub fn new(authorization: String) -> Self {
        Self::with_url(crate::http_client::client(), GRAPHQL, authorization)
    }

    /// Point at another endpoint. Tests use this to aim at a local mock.
    pub fn with_url(http: reqwest::Client, url: &str, authorization: String) -> Self {
        Self {
            http,
            url: url.to_string(),
            authorization,
        }
    }

    /// Run a query or mutation and return its `data`.
    pub async fn query(&self, query: &str, variables: Value) -> Result<Value> {
        let res = self
            .http
            .post(&self.url)
            .header("Authorization", &self.authorization)
            .json(&json!({"query": query, "variables": variables}))
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        read_graphql(status, &text)
    }
}

/// Unwrap a GraphQL response, turning `errors` into one readable error.
///
/// Linear reports a bad query, a missing entity and a refused credential all
/// through `errors`, sometimes with a 200 and sometimes with a 400, so the
/// body decides, not the status. `userPresentableMessage` is preferred where
/// given: it is the sentence Linear's own UI would show.
pub(crate) fn read_graphql(status: reqwest::StatusCode, text: &str) -> Result<Value> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        if status.as_u16() == 401 {
            bail!("{}", auth_hint("401 Unauthorized"));
        }
        bail!(
            "{status} from Linear: {}",
            text.chars().take(300).collect::<String>()
        );
    };
    if let Some(errors) = v.get("errors").and_then(|e| e.as_array())
        && !errors.is_empty()
    {
        let mut auth_failed = status.as_u16() == 401;
        let mut lines = Vec::new();
        for e in errors {
            let code = e
                .pointer("/extensions/code")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if code == "AUTHENTICATION_ERROR" {
                auth_failed = true;
            }
            let msg = e
                .pointer("/extensions/userPresentableMessage")
                .and_then(|m| m.as_str())
                .or_else(|| e.get("message").and_then(|m| m.as_str()))
                .unwrap_or("unknown error");
            let line = if code == "RATELIMITED" {
                format!("{msg} (rate limited; wait and retry)")
            } else {
                msg.to_string()
            };
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        let joined = lines.join("; ");
        if auth_failed {
            bail!("{}", auth_hint(&joined));
        }
        bail!("Linear: {joined}");
    }
    if !status.is_success() {
        bail!("{status} from Linear");
    }
    Ok(v.get("data").cloned().unwrap_or(Value::Null))
}

fn auth_hint(detail: &str) -> String {
    format!(
        "Linear refused the credential: {detail}\n\
         Re-authorize with `sidekar linear login …`, or store a fresh API key and run \
         `sidekar linear add --token <KV_KEY>`. `sidekar linear doctor` checks it."
    )
}

#[cfg(test)]
mod tests;
