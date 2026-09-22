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

/// Assemble one RFC 5322 message, base64url-encoded the way Gmail wants it.
///
/// Shared by `send` and the draft calls rather than duplicated: a draft is mail
/// that gets sent later, so it needs the same header-injection refusal below.
/// Splitting them would mean the check protects the direct path and quietly
/// misses the one where a human sees a reviewed-looking draft and hits send.
fn encode_message(to: &str, subject: &str, body: &str) -> Result<String> {
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
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes()))
}

pub async fn send(
    token: &super::auth::TokenRef,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<String> {
    let encoded = encode_message(to, subject, body)?;
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

// ---------------------------------------------------------------------------
// Drafts
// ---------------------------------------------------------------------------
//
// A draft is the reviewable half of `send`: an agent composes, a human reads it
// in their own Gmail, and sending stays a human act. That makes it the right
// default for mail an agent did not have explicit instruction to send, and the
// reason these exist alongside `send` rather than instead of it.
//
// All of this is covered by the `gmail.modify` scope sidekar already requests,
// so no stored token needs re-consenting.

pub struct DraftSummary {
    pub id: String,
    pub to: String,
    pub subject: String,
}

/// Compose a draft. Returns its draft id, which is not the message id.
pub async fn draft_create(
    token: &super::auth::TokenRef,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<String> {
    let encoded = encode_message(to, subject, body)?;
    let res = super::api_post(
        token,
        &format!("{BASE}/drafts"),
        &json!({"message": {"raw": encoded}}),
    )
    .await?;
    draft_id(&res)
}

/// Replace a draft's contents. Gmail has no partial update here, so every
/// field is rewritten and omitting one would silently blank it — which is why
/// the CLI requires all three rather than merging.
pub async fn draft_update(
    token: &super::auth::TokenRef,
    id: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<String> {
    let encoded = encode_message(to, subject, body)?;
    let res = super::api_put(
        token,
        &format!("{BASE}/drafts/{id}"),
        &json!({"message": {"raw": encoded}}),
    )
    .await?;
    draft_id(&res)
}

/// Drafts with their To and Subject.
///
/// `drafts.list` returns bare ids, so each one costs a metadata fetch to say
/// anything a human can pick from. Bounded by `limit` for that reason.
pub async fn draft_list(token: &super::auth::TokenRef, limit: usize) -> Result<Vec<DraftSummary>> {
    let url = format!("{BASE}/drafts?maxResults={}", limit.clamp(1, 100));
    let list = super::api_get(token, &url).await?;
    let ids: Vec<String> = list
        .get("drafts")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|d| d.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for id in ids {
        let url = format!(
            "{BASE}/drafts/{id}?format=metadata\
             &metadataHeaders=To&metadataHeaders=Subject"
        );
        let d = super::api_get(token, &url).await?;
        let m = d.get("message").unwrap_or(&Value::Null);
        out.push(DraftSummary {
            id,
            to: header(m, "To"),
            subject: header(m, "Subject"),
        });
    }
    Ok(out)
}

/// One draft as readable text.
pub async fn draft_show(token: &super::auth::TokenRef, id: &str) -> Result<String> {
    let d = super::api_get(token, &format!("{BASE}/drafts/{id}?format=full")).await?;
    let m = d.get("message").unwrap_or(&Value::Null);
    let mut out = format!(
        "Draft: {id}\nTo: {}\nSubject: {}\n\n",
        header(m, "To"),
        header(m, "Subject")
    );
    out.push_str(&body_text(m.get("payload").unwrap_or(&Value::Null)));
    Ok(out)
}

/// Send an existing draft. Returns the sent message id.
pub async fn draft_send(token: &super::auth::TokenRef, id: &str) -> Result<String> {
    let res = super::api_post(token, &format!("{BASE}/drafts/send"), &json!({"id": id})).await?;
    res.get("id")
        .and_then(|i| i.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("Gmail sent the draft but returned no message id"))
}

/// Discard a draft. Unlike `drive rm` this has no trash to recover from.
pub async fn draft_delete(token: &super::auth::TokenRef, id: &str) -> Result<()> {
    super::api_delete(token, &format!("{BASE}/drafts/{id}")).await
}

fn draft_id(res: &Value) -> Result<String> {
    res.get("id")
        .and_then(|i| i.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("Gmail accepted the draft but returned no draft id"))
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
