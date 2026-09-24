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
    let mut out = format!("From: {}\nTo: {}\n", header(&m, "From"), header(&m, "To"));
    let cc = header(&m, "Cc");
    if !cc.is_empty() {
        out.push_str(&format!("Cc: {cc}\n"));
    }
    out.push_str(&format!(
        "Date: {}\nSubject: {}\n\n",
        header(&m, "Date"),
        header(&m, "Subject")
    ));
    let payload = m.get("payload").unwrap_or(&Value::Null);
    let files = attachments(payload);
    if !files.is_empty() {
        // Named here because otherwise they are invisible: an agent reading a
        // message would have no idea there were files on it to fetch.
        out.push_str("Attachments:\n");
        for a in &files {
            out.push_str(&format!(
                "  {} ({} bytes, {})\n",
                a.filename, a.size, a.mime
            ));
        }
        out.push('\n');
    }
    out.push_str(&body_text(payload));
    Ok(out)
}

/// One file hanging off a message.
pub struct Attachment {
    pub id: String,
    pub filename: String,
    pub mime: String,
    pub size: u64,
}

/// Every attachment on a message, walking nested multiparts.
///
/// A part is an attachment when it has a filename; Gmail also gives inline
/// images filenames, so this deliberately catches those too — an agent asked to
/// "save the logo from that email" means the inline one.
pub(crate) fn attachments(payload: &Value) -> Vec<Attachment> {
    let mut out = Vec::new();
    collect_attachments(payload, &mut out);
    out
}

fn collect_attachments(part: &Value, out: &mut Vec<Attachment>) {
    let filename = part
        .get("filename")
        .and_then(|f| f.as_str())
        .unwrap_or_default();
    let body = part.get("body");
    let id = body
        .and_then(|b| b.get("attachmentId"))
        .and_then(|a| a.as_str());
    if !filename.is_empty()
        && let Some(id) = id
    {
        out.push(Attachment {
            id: id.to_string(),
            filename: filename.to_string(),
            mime: part
                .get("mimeType")
                .and_then(|m| m.as_str())
                .unwrap_or("application/octet-stream")
                .to_string(),
            size: body
                .and_then(|b| b.get("size"))
                .and_then(|s| s.as_u64())
                .unwrap_or(0),
        });
    }
    if let Some(parts) = part.get("parts").and_then(|p| p.as_array()) {
        for child in parts {
            collect_attachments(child, out);
        }
    }
}

/// Strip any directory part from an attachment filename.
///
/// The name comes off a received message, so it is attacker-chosen: one
/// containing `../` or a leading `/` would write outside the directory the user
/// named. Everything up to the last separator goes.
pub fn safe_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if base.is_empty() || base == "." || base == ".." {
        "attachment".to_string()
    } else {
        base.to_string()
    }
}

/// Fetch one attachment's bytes.
///
/// Bytes, never a String. Decoding through `String::from_utf8_lossy` replaces
/// every invalid sequence with U+FFFD — three bytes where one stood — so a PDF
/// or an image arrives larger than it left and will not open. Drive hit exactly
/// this; the size assertion below is the same guard.
pub async fn attachment_download(
    token: &super::auth::TokenRef,
    message_id: &str,
    attachment_id: &str,
) -> Result<Vec<u8>> {
    let url = format!("{BASE}/messages/{message_id}/attachments/{attachment_id}");
    let res = super::api_get(token, &url).await?;
    let data = res
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or_else(|| anyhow::anyhow!("attachment {attachment_id} came back with no data"))?;
    // Gmail hands attachment data back base64url, padded or not depending on
    // the part, so accept both rather than guessing.
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(data))
        .map_err(|e| anyhow::anyhow!("attachment {attachment_id} is not valid base64: {e}"))?;
    if let Some(expected) = res.get("size").and_then(|s| s.as_u64())
        && bytes.len() as u64 != expected
    {
        bail!(
            "attachment came down as {} bytes but Gmail reports {expected}. Refusing to write a \
             file that does not match.",
            bytes.len()
        );
    }
    Ok(bytes)
}

/// Attachments on a message, fetched by id.
pub async fn message_attachments(
    token: &super::auth::TokenRef,
    id: &str,
) -> Result<Vec<Attachment>> {
    let m = super::api_get(token, &format!("{BASE}/messages/{id}?format=full")).await?;
    Ok(attachments(m.get("payload").unwrap_or(&Value::Null)))
}

/// What to put in one outgoing message.
///
/// A struct rather than five positional arguments because `send`, `draft
/// create` and `draft update` all take exactly this, and a caller that swapped
/// `cc` for `bcc` by position would leak the recipient list to everyone.
#[derive(Default)]
pub struct Compose {
    pub to: String,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    pub subject: String,
    pub body: String,
    pub reply: Option<ReplyContext>,
    /// Files to attach, already read off disk.
    pub attachments: Vec<OutgoingAttachment>,
}

/// A file on its way out.
pub struct OutgoingAttachment {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// What Gmail accepts in one `raw` message on the plain (non-upload) endpoint.
///
/// Gmail's own ceiling is 25MB of attachments, but that only applies to the
/// `/upload/` endpoints; `messages.send` with a JSON `raw` field caps at 5MB,
/// and exceeding it returns a bare 413 that says nothing about attachments.
/// Checking here buys an error that names the cause and the way around it.
pub const MAX_RAW_BYTES: usize = 5 * 1024 * 1024;

/// Read a file for attaching, guessing its type from the extension.
pub fn attachment_from_path(path: &std::path::Path) -> Result<OutgoingAttachment> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("could not read --attach {}: {e}", path.display()))?;
    let filename = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("--attach {} has no filename", path.display()))?;
    // A quoted filename with a break in it would end the header early, the same
    // way an injected Cc does.
    reject_header_breaks("--attach", &filename)?;
    if filename.contains('"') {
        bail!("--attach {filename} contains a quote, which would break its Content-Disposition");
    }
    Ok(OutgoingAttachment {
        mime: mime_for(&filename).to_string(),
        filename,
        bytes,
    })
}

/// Content type from a file extension.
///
/// A short list rather than a dependency: getting this wrong costs a preview,
/// not correctness, since every client falls back on the filename. The default
/// is the one that makes clients offer to save rather than try to render.
pub(crate) fn mime_for(filename: &str) -> &'static str {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

/// What a reply needs from the message it answers.
///
/// Threading is not a Gmail flag — it is `threadId` on the API call plus the
/// `In-Reply-To` and `References` headers, and every mail client wants all
/// three. Setting only `threadId` threads it in Gmail's own UI and nowhere
/// else, which is the kind of half-working that gets noticed late.
pub struct ReplyContext {
    pub thread_id: String,
    /// The parent's `Message-ID`, for `In-Reply-To`.
    pub message_id: String,
    /// The parent's `References` chain with its own id appended.
    pub references: String,
    /// The parent's subject, so a reply can inherit it when none is given.
    pub subject: String,
}

/// Look up everything a reply to `id` needs.
pub async fn reply_context(token: &super::auth::TokenRef, id: &str) -> Result<ReplyContext> {
    let url = format!(
        "{BASE}/messages/{id}?format=metadata\
         &metadataHeaders=Message-ID&metadataHeaders=References&metadataHeaders=Subject"
    );
    let m = super::api_get(token, &url).await?;
    let thread_id = m
        .get("threadId")
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("message {id} has no threadId; cannot reply into it"))?;
    let message_id = header(&m, "Message-ID");
    let parent_refs = header(&m, "References");
    // References is the whole ancestry, oldest first, with the parent last.
    let references = match (parent_refs.is_empty(), message_id.is_empty()) {
        (_, true) => parent_refs,
        (true, false) => message_id.clone(),
        (false, false) => format!("{parent_refs} {message_id}"),
    };
    Ok(ReplyContext {
        thread_id,
        message_id,
        references,
        subject: header(&m, "Subject"),
    })
}

/// The subject a reply should carry when the caller gave none.
pub(crate) fn reply_subject(parent: &str) -> String {
    if parent.is_empty() {
        return String::new();
    }
    // Don't stack "Re: Re: Re:" — one prefix is the convention everywhere.
    if parent.len() >= 3 && parent[..3].eq_ignore_ascii_case("re:") {
        return parent.to_string();
    }
    format!("Re: {parent}")
}

/// Assemble one RFC 5322 message, base64url-encoded the way Gmail wants it.
///
/// Shared by `send` and the draft calls rather than duplicated: a draft is mail
/// that gets sent later, so it needs the same header-injection refusal below.
/// Splitting them would mean the check protects the direct path and quietly
/// misses the one where a human sees a reviewed-looking draft and hits send.
pub(crate) fn encode_message(msg: &Compose) -> Result<String> {
    // Headers end at the first blank line, so a CR or LF in a header value lets
    // the rest of that value become new headers. Sidekar reads mail, so a subject
    // assembled from a received message is untrusted input, and an injected
    // `Bcc:` would silently copy the mail somewhere the sender never named.
    // Reject rather than strip: quietly altering a header the caller asked for
    // is worse than refusing to send it.
    //
    // Every header value goes through this, including the ones lifted off a
    // parent message by `--reply`: that parent is mail somebody else sent, so
    // its Message-ID and References are exactly as untrusted as its subject.
    reject_header_breaks("--to", &msg.to)?;
    reject_header_breaks("--subject", &msg.subject)?;
    if let Some(cc) = &msg.cc {
        reject_header_breaks("--cc", cc)?;
    }
    if let Some(bcc) = &msg.bcc {
        reject_header_breaks("--bcc", bcc)?;
    }
    if let Some(r) = &msg.reply {
        reject_header_breaks("In-Reply-To", &r.message_id)?;
        reject_header_breaks("References", &r.references)?;
    }

    let mut headers = format!("To: {}\r\n", msg.to);
    if let Some(cc) = msg.cc.as_deref().filter(|c| !c.is_empty()) {
        headers.push_str(&format!("Cc: {cc}\r\n"));
    }
    if let Some(bcc) = msg.bcc.as_deref().filter(|b| !b.is_empty()) {
        headers.push_str(&format!("Bcc: {bcc}\r\n"));
    }
    headers.push_str(&format!("Subject: {}\r\n", encode_subject(&msg.subject)));
    if let Some(r) = &msg.reply {
        if !r.message_id.is_empty() {
            headers.push_str(&format!("In-Reply-To: {}\r\n", r.message_id));
        }
        if !r.references.is_empty() {
            headers.push_str(&format!("References: {}\r\n", r.references));
        }
    }
    let raw = if msg.attachments.is_empty() {
        headers.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
        format!("{headers}\r\n{}", msg.body)
    } else {
        // multipart/mixed: the text first, then one part per file. The boundary
        // must not occur in any part, which is why it carries a random tail
        // rather than being a fixed string.
        let boundary = mime_boundary();
        headers.push_str(&format!(
            "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\"\r\n"
        ));
        let mut out = format!(
            "{headers}\r\n--{boundary}\r\n\
             Content-Type: text/plain; charset=UTF-8\r\n\r\n{}\r\n",
            msg.body
        );
        for a in &msg.attachments {
            out.push_str(&format!(
                "--{boundary}\r\n\
                 Content-Type: {}; name=\"{}\"\r\n\
                 Content-Disposition: attachment; filename=\"{}\"\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
                a.mime,
                a.filename,
                a.filename,
                base64_mime(&a.bytes)
            ));
        }
        out.push_str(&format!("--{boundary}--\r\n"));
        out
    };

    if raw.len() > MAX_RAW_BYTES {
        bail!(
            "this message is {:.1}MB once encoded, over Gmail's {:.0}MB limit for a single \
             send. Put the large files in Drive and link them instead: \
             `sidekar drive put <file>` then paste the link in the body.",
            raw.len() as f64 / (1024.0 * 1024.0),
            MAX_RAW_BYTES as f64 / (1024.0 * 1024.0)
        );
    }
    // Gmail wants URL-safe base64 here, not the standard alphabet.
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes()))
}

/// A boundary no part can accidentally contain.
fn mime_boundary() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("=_sidekar_{n:x}_{:x}", std::process::id())
}

/// Standard-alphabet base64, wrapped at 76 columns.
///
/// MIME requires the padded standard alphabet and lines short enough to survive
/// transport; the URL-safe one used for the envelope would arrive as garbage.
fn base64_mime(bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    encoded
        .as_bytes()
        .chunks(76)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// The JSON body for a create/send call, carrying `threadId` when replying.
fn message_payload(msg: &Compose, encoded: &str) -> Value {
    match &msg.reply {
        Some(r) => json!({"raw": encoded, "threadId": r.thread_id}),
        None => json!({"raw": encoded}),
    }
}

pub async fn send(token: &super::auth::TokenRef, msg: &Compose) -> Result<String> {
    let encoded = encode_message(msg)?;
    let res = super::api_post(
        token,
        &format!("{BASE}/messages/send"),
        &message_payload(msg, &encoded),
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
pub async fn draft_create(token: &super::auth::TokenRef, msg: &Compose) -> Result<String> {
    let encoded = encode_message(msg)?;
    let res = super::api_post(
        token,
        &format!("{BASE}/drafts"),
        &json!({"message": message_payload(msg, &encoded)}),
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
    msg: &Compose,
) -> Result<String> {
    let encoded = encode_message(msg)?;
    let res = super::api_put(
        token,
        &format!("{BASE}/drafts/{id}"),
        &json!({"message": message_payload(msg, &encoded)}),
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
    let mut out = format!("Draft: {id}\nTo: {}\n", header(m, "To"));
    // Cc and Bcc only when set. Printing them matters more here than anywhere
    // else: a draft is reviewed before it goes out, and a recipient list the
    // reviewer cannot see is one they cannot check.
    for name in ["Cc", "Bcc"] {
        let v = header(m, name);
        if !v.is_empty() {
            out.push_str(&format!("{name}: {v}\n"));
        }
    }
    let in_reply_to = header(m, "In-Reply-To");
    if !in_reply_to.is_empty() {
        out.push_str(&format!("In-Reply-To: {in_reply_to}\n"));
    }
    out.push_str(&format!("Subject: {}\n\n", header(m, "Subject")));
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
