//! Gmail over the API.

use anyhow::{Result, bail};
use base64::Engine;
use serde_json::{Value, json};

const BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

pub struct Summary {
    pub id: String,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub snippet: String,
}

/// Search with Gmail's own query syntax: `from:x`, `is:unread`, `newer_than:2d`.
pub async fn search(
    token: &super::auth::TokenRef,
    query: &str,
    limit: usize,
) -> Result<Vec<Summary>> {
    let url = format!(
        "{BASE}/messages?q={}&maxResults={}",
        urlencoding::encode(query),
        limit.clamp(1, 100)
    );
    let list = super::api_get(token, &url).await?;
    let ids: Vec<String> = list
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for id in ids {
        // metadata format: headers and snippet only, so a search over a hundred
        // messages does not drag their bodies across the wire.
        let url = format!(
            "{BASE}/messages/{id}?format=metadata\
             &metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date"
        );
        let m = super::api_get(token, &url).await?;
        out.push(Summary {
            id: id.clone(),
            from: header(&m, "From"),
            subject: header(&m, "Subject"),
            date: header(&m, "Date"),
            snippet: m
                .get("snippet")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
        });
    }
    Ok(out)
}

/// One message as readable text.
pub async fn read(token: &super::auth::TokenRef, id: &str) -> Result<String> {
    let m = super::api_get(token, &format!("{BASE}/messages/{id}?format=full")).await?;
    let mut out = format!(
        "From: {}\nTo: {}\nDate: {}\nSubject: {}\n\n",
        header(&m, "From"),
        header(&m, "To"),
        header(&m, "Date"),
        header(&m, "Subject")
    );
    out.push_str(&body_text(m.get("payload").unwrap_or(&Value::Null)));
    Ok(out)
}

pub async fn send(
    token: &super::auth::TokenRef,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<String> {
    // Headers end at the first blank line, so a CR or LF in a header value lets
    // the rest of that value become new headers. Sidekar reads mail, so a subject
    // assembled from a received message is untrusted input, and an injected
    // `Bcc:` would silently copy the mail somewhere the sender never named.
    // Reject rather than strip: quietly altering a header the caller asked for
    // is worse than refusing to send it.
    reject_header_breaks("--to", to)?;
    reject_header_breaks("--subject", subject)?;

    let raw = format!(
        "To: {to}\r\nSubject: {}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{body}",
        encode_subject(subject)
    );
    // Gmail wants URL-safe base64 here, not the standard alphabet.
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes());
    let res = super::api_post(
        token,
        &format!("{BASE}/messages/send"),
        &json!({"raw": encoded}),
    )
    .await?;
    res.get("id")
        .and_then(|i| i.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("Gmail accepted the send but returned no message id"))
}

pub async fn labels(token: &super::auth::TokenRef) -> Result<Vec<String>> {
    let res = super::api_get(token, &format!("{BASE}/labels")).await?;
    Ok(res
        .get("labels")
        .and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

/// Add and remove labels. `UNREAD` is a label, so this is also mark-as-read.
pub async fn modify(
    token: &super::auth::TokenRef,
    id: &str,
    add: &[String],
    remove: &[String],
) -> Result<()> {
    if add.is_empty() && remove.is_empty() {
        bail!("nothing to change: pass --add or --remove");
    }
    super::api_post(
        token,
        &format!("{BASE}/messages/{id}/modify"),
        &json!({"addLabelIds": add, "removeLabelIds": remove}),
    )
    .await?;
    Ok(())
}

fn reject_header_breaks(label: &str, value: &str) -> Result<()> {
    if value.contains('\r') || value.contains('\n') || value.contains('\0') {
        bail!("{label} contains a line break or NUL, which would inject an email header");
    }
    Ok(())
}

/// RFC 2047 encode a subject that is not plain ASCII.
///
/// A raw UTF-8 subject is not legal in a header and arrives mangled often enough
/// to matter; base64 in a charset-tagged word is the portable spelling.
pub(crate) fn encode_subject(subject: &str) -> String {
    if subject.is_ascii() {
        return subject.to_string();
    }
    format!(
        "=?UTF-8?B?{}?=",
        base64::engine::general_purpose::STANDARD.encode(subject.as_bytes())
    )
}

pub(crate) fn header(msg: &Value, name: &str) -> String {
    msg.get("payload")
        .and_then(|p| p.get("headers"))
        .and_then(|h| h.as_array())
        .and_then(|hs| {
            hs.iter().find(|h| {
                h.get("name")
                    .and_then(|n| n.as_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
        })
        .and_then(|h| h.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Pull the plain-text body out of Gmail's MIME tree.
///
/// Walks parts depth-first preferring text/plain, because a multipart/alternative
/// message carries the same content twice and the HTML copy is unreadable as text.
pub(crate) fn body_text(payload: &Value) -> String {
    if let Some(parts) = payload.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            if part.get("mimeType").and_then(|m| m.as_str()) == Some("text/plain")
                && let Some(d) = decode_part(part)
            {
                return d;
            }
        }
        for part in parts {
            let nested = body_text(part);
            if !nested.is_empty() {
                return nested;
            }
        }
        return String::new();
    }
    decode_part(payload).unwrap_or_default()
}

fn decode_part(part: &Value) -> Option<String> {
    let data = part.get("body")?.get("data")?.as_str()?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data.replace('-', "-").replace('_', "_"))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(data))
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests;
