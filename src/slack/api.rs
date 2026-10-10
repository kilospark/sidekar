//! Slack conversations, messages, search and people.

use super::Slack;
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;

/// Slack's own page-size ceiling for list methods.
const PAGE: usize = 200;

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}

/// Walk a cursor-paginated list method until `want` items or the end.
async fn paged(
    slack: &Slack,
    method: &str,
    params: &[(&str, String)],
    key: &str,
    want: usize,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut p: Vec<(&str, String)> = params.to_vec();
        if let Some(c) = &cursor {
            p.push(("cursor", c.clone()));
        }
        let v = slack.get(method, &p).await?;
        if let Some(items) = v.get(key).and_then(|i| i.as_array()) {
            out.extend(items.iter().cloned());
        }
        if out.len() >= want {
            break;
        }
        cursor = v
            .pointer("/response_metadata/next_cursor")
            .and_then(|c| c.as_str())
            .filter(|c| !c.is_empty())
            .map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    out.truncate(want);
    Ok(out)
}

/// Walk a list method page by page until `wanted` says stop. Returns every
/// item seen, so a miss can still suggest near names. Lookups use this so a
/// name found on page one costs one call, not a walk of the whole workspace.
async fn scan_paged(
    slack: &Slack,
    method: &str,
    params: &[(&str, String)],
    key: &str,
    mut stop: impl FnMut(&Value) -> bool,
) -> Result<(bool, Vec<Value>)> {
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut p: Vec<(&str, String)> = params.to_vec();
        if let Some(c) = &cursor {
            p.push(("cursor", c.clone()));
        }
        let v = slack.get(method, &p).await?;
        for item in v.get(key).and_then(|i| i.as_array()).into_iter().flatten() {
            seen.push(item.clone());
            if stop(item) {
                return Ok((true, seen));
            }
        }
        cursor = v
            .pointer("/response_metadata/next_cursor")
            .and_then(|c| c.as_str())
            .filter(|c| !c.is_empty())
            .map(String::from);
        if cursor.is_none() {
            return Ok((false, seen));
        }
    }
}

// ---------------------------------------------------------------------------
// Conversations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    /// Empty for a DM, which has a peer instead of a name.
    pub name: String,
    pub kind: &'static str,
    /// The other person in a DM.
    pub user: Option<String>,
    pub members: Option<u64>,
    pub topic: String,
    pub is_member: bool,
}

pub(crate) fn channel_from(v: &Value) -> Channel {
    let kind = if b(v, "is_im") {
        "dm"
    } else if b(v, "is_mpim") {
        "group-dm"
    } else if b(v, "is_private") {
        "private"
    } else {
        "public"
    };
    Channel {
        id: s(v, "id"),
        name: s(v, "name"),
        kind,
        user: v.get("user").and_then(|u| u.as_str()).map(String::from),
        members: v.get("num_members").and_then(|n| n.as_u64()),
        topic: v
            .pointer("/topic/value")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        // A DM is always "joined"; Slack omits the flag there.
        is_member: b(v, "is_member") || b(v, "is_im"),
    }
}

/// Which `conversations.list` types a `--types` value means. Accepts Slack's
/// own names and the short ones the output prints.
pub(crate) fn conversation_types(spec: &str) -> Result<String> {
    let mut out: Vec<&str> = Vec::new();
    for t in spec.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let slack = match t {
            "public" | "public_channel" => "public_channel",
            "private" | "private_channel" => "private_channel",
            "dm" | "im" => "im",
            "group-dm" | "mpim" => "mpim",
            "all" => {
                for each in ["public_channel", "private_channel", "im", "mpim"] {
                    if !out.contains(&each) {
                        out.push(each);
                    }
                }
                continue;
            }
            other => {
                bail!("unknown conversation type {other}; use public, private, dm, group-dm or all")
            }
        };
        if !out.contains(&slack) {
            out.push(slack);
        }
    }
    if out.is_empty() {
        bail!("--types needs at least one of public, private, dm, group-dm, all");
    }
    Ok(out.join(","))
}

/// Conversations the token can see, archived ones left out.
pub async fn channels(slack: &Slack, types: &str, limit: usize) -> Result<Vec<Channel>> {
    let items = paged(
        slack,
        "conversations.list",
        &[
            ("types", types.to_string()),
            ("exclude_archived", "true".into()),
            ("limit", PAGE.to_string()),
        ],
        "channels",
        limit,
    )
    .await?;
    Ok(items.iter().map(channel_from).collect())
}

/// Conversations the person is actually in, via `users.conversations`.
pub async fn my_channels(slack: &Slack, types: &str, limit: usize) -> Result<Vec<Channel>> {
    let items = paged(
        slack,
        "users.conversations",
        &[
            ("types", types.to_string()),
            ("exclude_archived", "true".into()),
            ("limit", PAGE.to_string()),
        ],
        "channels",
        limit,
    )
    .await?;
    Ok(items
        .iter()
        .map(|v| {
            let mut c = channel_from(v);
            c.is_member = true;
            c
        })
        .collect())
}

/// True for a conversation id: `C…` channel, `G…` private group, `D…` DM.
/// Channel names are lower-case, so an all-upper-case token cannot be one.
pub(crate) fn looks_like_conversation_id(s: &str) -> bool {
    looks_like_id(s, &['C', 'G', 'D'])
}

pub(crate) fn looks_like_user_id(s: &str) -> bool {
    looks_like_id(s, &['U', 'W', 'B'])
}

fn looks_like_id(s: &str, prefixes: &[char]) -> bool {
    s.len() >= 9
        && s.starts_with(prefixes)
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// A Slack message link, as copied from "Copy link":
/// `https://<ws>.slack.com/archives/C0123/p1700000000123456[?thread_ts=…]`.
/// Returns the channel, the message ts, and the thread it sits in, if any.
pub fn parse_permalink(url: &str) -> Option<(String, String, Option<String>)> {
    let rest = url.split("/archives/").nth(1)?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut parts = path.trim_end_matches('/').split('/');
    let channel = parts.next()?.to_string();
    if !looks_like_conversation_id(&channel) {
        return None;
    }
    let p = parts.next()?;
    let digits = p.strip_prefix('p')?;
    if digits.len() < 7 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (secs, micros) = digits.split_at(digits.len() - 6);
    let ts = format!("{secs}.{micros}");
    let thread = crate::oauth_loopback::query_params(&format!("?{query}"))
        .get("thread_ts")
        .cloned()
        .filter(|t| t != &ts);
    Some((channel, ts, thread))
}

/// A channel argument as the API wants it: an id, `#name`, a name, a message
/// link, or a person (`@name`, an email, a user id) meaning a DM with them.
/// Whether a `<channel>` argument names a person (their DM) rather than a
/// channel: `@handle`, a `U…` id or an email.
pub(crate) fn is_person(t: &str) -> bool {
    let t = t.trim();
    t.starts_with('@') || looks_like_user_id(t) || is_email(t)
}

/// The DM already open with a person, without opening one: `None` when the
/// two of you have never had a DM. (`conversations.open` would create it.)
pub async fn existing_dm(slack: &Slack, user_id: &str) -> Result<Option<String>> {
    let params = [("types", "im".to_string()), ("limit", PAGE.to_string())];
    let with = |v: &Value| s(v, "user") == user_id;
    let (found, seen) = scan_paged(slack, "users.conversations", &params, "channels", with).await?;
    Ok(found
        .then(|| seen.iter().find(|v| with(v)).map(|v| s(v, "id")))
        .flatten())
}

/// The most `search.messages` returns in one page.
pub const SEARCH_MAX: usize = 100;

pub async fn resolve_channel(slack: &Slack, input: &str) -> Result<String> {
    let t = input.trim();
    if let Some((c, _, _)) = parse_permalink(t) {
        return Ok(c);
    }
    if looks_like_conversation_id(t) {
        return Ok(t.to_string());
    }
    if is_person(t) {
        let user = resolve_user(slack, t).await?;
        return open_dm(slack, &[user.id]).await;
    }
    let name = t.trim_start_matches('#').to_lowercase();
    let cache_key = format!("channel:{name}");
    if let Some(id) = slack.cache_get(&cache_key) {
        return Ok(id);
    }
    let is_it = |v: &Value| s(v, "name") == name;
    let params = [
        ("types", "public_channel,private_channel".to_string()),
        ("exclude_archived", "true".to_string()),
        ("limit", PAGE.to_string()),
    ];
    // Channels the person is in first: that is where nearly every post goes,
    // and the list is far shorter than the whole workspace.
    let (found, mut seen) = scan_paged(slack, "users.conversations", &params, "channels", is_it)
        .await
        .unwrap_or((false, Vec::new()));
    if !found {
        let (_, all) = scan_paged(slack, "conversations.list", &params, "channels", is_it).await?;
        seen.extend(all);
    }
    if let Some(c) = seen.iter().find(|v| is_it(v)) {
        let id = s(c, "id");
        slack.cache_put(&cache_key, &id);
        return Ok(id);
    }
    let mut close: Vec<String> = seen
        .iter()
        .map(|v| s(v, "name"))
        .filter(|n| n.contains(&name))
        .map(|n| format!("#{n}"))
        .collect();
    close.sort();
    close.dedup();
    close.truncate(8);
    if close.is_empty() {
        bail!(
            "no channel named #{name} that this token can see; `sidekar slack channels` lists them"
        );
    }
    bail!(
        "no channel named #{name}. Did you mean: {}",
        close.join(", ")
    )
}

/// Open (or find) the DM with these people and return its id.
pub async fn open_dm(slack: &Slack, user_ids: &[String]) -> Result<String> {
    let v = slack
        .post("conversations.open", &json!({"users": user_ids.join(",")}))
        .await?;
    v.pointer("/channel/id")
        .and_then(|c| c.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("conversations.open returned no channel"))
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub ts: String,
    /// User id, empty for bot and system messages.
    pub user: String,
    /// A display name Slack attached itself (bots, integrations).
    pub username: String,
    pub text: String,
    pub thread_ts: Option<String>,
    pub reply_count: u64,
    pub files: Vec<SlackFile>,
    pub subtype: String,
}

pub(crate) fn message_from(v: &Value) -> Message {
    let username = Some(s(v, "username"))
        .filter(|u| !u.is_empty())
        .or_else(|| {
            v.pointer("/bot_profile/name")
                .and_then(|n| n.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    Message {
        ts: s(v, "ts"),
        user: s(v, "user"),
        username,
        text: s(v, "text"),
        thread_ts: v
            .get("thread_ts")
            .and_then(|t| t.as_str())
            .map(String::from),
        reply_count: v.get("reply_count").and_then(|n| n.as_u64()).unwrap_or(0),
        files: v
            .get("files")
            .and_then(|f| f.as_array())
            .map(|a| a.iter().map(file_from).collect())
            .unwrap_or_default(),
        subtype: s(v, "subtype"),
    }
}

/// The newest `limit` messages in a channel, oldest first so they read in
/// order.
pub async fn history(
    slack: &Slack,
    channel: &str,
    limit: usize,
    oldest: Option<&str>,
) -> Result<Vec<Message>> {
    let mut params = vec![
        ("channel", channel.to_string()),
        ("limit", limit.clamp(1, PAGE).to_string()),
    ];
    if let Some(o) = oldest {
        params.push(("oldest", o.to_string()));
    }
    let items = paged(slack, "conversations.history", &params, "messages", limit).await?;
    let mut out: Vec<Message> = items.iter().map(message_from).collect();
    out.reverse();
    Ok(out)
}

/// A thread: the parent and its replies, in order.
pub async fn replies(slack: &Slack, channel: &str, ts: &str, limit: usize) -> Result<Vec<Message>> {
    let items = paged(
        slack,
        "conversations.replies",
        &[
            ("channel", channel.to_string()),
            ("ts", ts.to_string()),
            ("limit", limit.clamp(1, PAGE).to_string()),
        ],
        "messages",
        limit,
    )
    .await?;
    Ok(items.iter().map(message_from).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub channel_id: String,
    pub channel_name: String,
    pub ts: String,
    pub user: String,
    pub username: String,
    pub text: String,
    pub permalink: String,
}

pub(crate) fn match_from(v: &Value) -> Match {
    Match {
        channel_id: v
            .pointer("/channel/id")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string(),
        channel_name: v
            .pointer("/channel/name")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string(),
        ts: s(v, "ts"),
        user: s(v, "user"),
        username: s(v, "username"),
        text: s(v, "text"),
        permalink: s(v, "permalink"),
    }
}

/// Search messages with Slack's own syntax (`from:@x in:#y after:2026-01-01`).
/// Newest first. User tokens only: Slack offers bots no search scope.
pub async fn search(slack: &Slack, query: &str, limit: usize) -> Result<Vec<Match>> {
    let v = slack
        .get(
            "search.messages",
            &[
                ("query", query.to_string()),
                ("count", limit.clamp(1, SEARCH_MAX).to_string()),
                ("sort", "timestamp".into()),
                ("sort_dir", "desc".into()),
            ],
        )
        .await?;
    Ok(v.pointer("/messages/matches")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().map(match_from).collect())
        .unwrap_or_default())
}

/// Post a message, optionally into a thread. Returns the channel id and the
/// new message's ts.
pub async fn post(
    slack: &Slack,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
    broadcast: bool,
) -> Result<(String, String)> {
    let mut body = json!({"channel": channel, "text": text});
    if let Some(t) = thread_ts {
        body["thread_ts"] = json!(t);
        if broadcast {
            body["reply_broadcast"] = json!(true);
        }
    }
    let v = slack.post("chat.postMessage", &body).await?;
    Ok((s(&v, "channel"), s(&v, "ts")))
}

pub async fn permalink(slack: &Slack, channel: &str, ts: &str) -> Result<String> {
    let v = slack
        .get(
            "chat.getPermalink",
            &[
                ("channel", channel.to_string()),
                ("message_ts", ts.to_string()),
            ],
        )
        .await?;
    Ok(s(&v, "permalink"))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackFile {
    pub id: String,
    pub name: String,
    pub title: String,
    pub mimetype: String,
    /// Slack's short type: `pdf`, `png`, `text`, `gdoc`…
    pub filetype: String,
    pub size: u64,
    /// `hosted`, `external` (Drive, Dropbox…), `snippet`, `post`, or
    /// `tombstone` for a deleted file.
    pub mode: String,
    pub user: String,
    pub created: i64,
    /// What to download: `url_private_download`, else `url_private`.
    pub url: String,
    pub permalink: String,
    /// For an external file, where it really lives.
    pub external_url: String,
    pub channels: Vec<String>,
}

impl SlackFile {
    /// The name to show and save under.
    pub fn display_name(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else if !self.title.is_empty() {
            &self.title
        } else {
            &self.id
        }
    }

    /// One line for a message listing: `F0… report.pdf (application/pdf, 1.2M)`.
    pub fn summary(&self) -> String {
        if self.mode == "tombstone" {
            return format!("{} (deleted)", self.id);
        }
        let kind = if self.mimetype.is_empty() {
            &self.filetype
        } else {
            &self.mimetype
        };
        let mut out = format!("{} {}", self.id, self.display_name());
        let mut tags = Vec::new();
        if !kind.is_empty() {
            tags.push(kind.to_string());
        }
        if self.size > 0 {
            tags.push(crate::attachments::human_bytes(self.size));
        }
        if self.mode == "external" {
            tags.push("external".into());
        }
        if !tags.is_empty() {
            out.push_str(&format!(" ({})", tags.join(", ")));
        }
        out
    }
}

pub(crate) fn file_from(v: &Value) -> SlackFile {
    let mut channels: Vec<String> = Vec::new();
    for k in ["channels", "groups", "ims"] {
        if let Some(a) = v.get(k).and_then(|x| x.as_array()) {
            channels.extend(a.iter().filter_map(|c| c.as_str().map(String::from)));
        }
    }
    let download = s(v, "url_private_download");
    SlackFile {
        id: s(v, "id"),
        name: s(v, "name"),
        title: s(v, "title"),
        mimetype: s(v, "mimetype"),
        filetype: s(v, "filetype"),
        size: v.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
        mode: s(v, "mode"),
        user: s(v, "user"),
        created: v.get("created").and_then(|x| x.as_i64()).unwrap_or(0),
        url: if download.is_empty() {
            s(v, "url_private")
        } else {
            download
        },
        permalink: s(v, "permalink"),
        external_url: s(v, "external_url"),
        channels,
    }
}

/// A file id from an id (`F0…`) or a file link
/// (`https://x.slack.com/files/U…/F…/name`).
pub fn parse_file_id(input: &str) -> Option<String> {
    let t = input.trim();
    let is_id = |w: &str| {
        w.len() >= 8
            && w.starts_with('F')
            && w.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    if is_id(t) {
        return Some(t.to_string());
    }
    if t.starts_with("http") {
        let path = t.split(['?', '#']).next().unwrap_or(t);
        return path
            .split('/')
            .find(|seg| is_id(seg))
            .map(String::from)
            .or_else(|| {
                // files-pri/T…-F…/name
                path.split('/')
                    .find_map(|seg| seg.split_once("-F").map(|(_, f)| format!("F{f}")))
                    .filter(|f| is_id(f))
            });
    }
    None
}

/// `files.info` (scope `files:read`).
pub async fn file_info(slack: &Slack, id: &str) -> Result<SlackFile> {
    let v = slack.get("files.info", &[("file", id.to_string())]).await?;
    Ok(file_from(v.get("file").unwrap_or(&Value::Null)))
}

/// A file's bytes. External files (Drive, Dropbox…) and deleted ones have
/// nothing to download through Slack.
pub async fn file_download(slack: &Slack, f: &SlackFile) -> Result<Vec<u8>> {
    match f.mode.as_str() {
        "tombstone" => bail!("{} was deleted", f.id),
        "external" => bail!(
            "{} is an external file; it lives at {}",
            f.id,
            if f.external_url.is_empty() {
                &f.url
            } else {
                &f.external_url
            }
        ),
        _ => {}
    }
    if f.url.is_empty() {
        bail!("Slack gave no download URL for {}", f.id);
    }
    let bytes = slack
        .download(&f.url, f.mimetype.starts_with("text/html"))
        .await?;
    if f.size > 0 && bytes.len() as u64 != f.size {
        bail!(
            "{} downloaded as {} bytes but Slack lists {}; not saving a partial file",
            f.display_name(),
            bytes.len(),
            f.size
        );
    }
    Ok(bytes)
}

/// A local file ready to upload.
#[derive(Debug, Clone)]
pub struct Upload {
    pub name: String,
    pub title: Option<String>,
    pub bytes: Vec<u8>,
}

/// Upload files and share them in one message, using Slack's current flow:
/// `files.getUploadURLExternal` per file, the bytes to each upload URL, then
/// one `files.completeUploadExternal` that posts them (with `text` as the
/// message) into the channel or thread. `files.upload` is retired.
///
/// Returns the file ids and their permalinks.
pub async fn upload_files(
    slack: &Slack,
    channel: &str,
    files: Vec<Upload>,
    text: Option<&str>,
    thread_ts: Option<&str>,
) -> Result<Vec<(String, String)>> {
    if files.is_empty() {
        bail!("no files to upload");
    }
    let mut entries = Vec::new();
    for f in files {
        let v = slack
            .post_form(
                "files.getUploadURLExternal",
                &[
                    ("filename", f.name.clone()),
                    ("length", f.bytes.len().to_string()),
                ],
            )
            .await?;
        let (url, id) = (s(&v, "upload_url"), s(&v, "file_id"));
        if url.is_empty() || id.is_empty() {
            bail!("Slack gave no upload URL for {}", f.name);
        }
        slack.put_upload(&url, f.bytes).await?;
        entries.push(json!({"id": id, "title": f.title.unwrap_or(f.name)}));
    }
    let mut fields = vec![
        ("files", Value::Array(entries).to_string()),
        ("channel_id", channel.to_string()),
    ];
    if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
        fields.push(("initial_comment", t.to_string()));
    }
    if let Some(ts) = thread_ts {
        fields.push(("thread_ts", ts.to_string()));
    }
    let v = slack
        .post_form("files.completeUploadExternal", &fields)
        .await?;
    Ok(v.get("files")
        .and_then(|f| f.as_array())
        .map(|a| a.iter().map(|f| (s(f, "id"), s(f, "permalink"))).collect())
        .unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Drafts
// ---------------------------------------------------------------------------

/// Text as the single rich-text block Slack's composer stores a draft as,
/// with bare URLs turned into link elements so they stay clickable.
pub(crate) fn text_to_blocks(text: &str) -> Value {
    let mut elements = Vec::new();
    let mut rest = text;
    while let Some(start) = ["https://", "http://"]
        .iter()
        .filter_map(|p| rest.find(p))
        .min()
    {
        if start > 0 {
            elements.push(json!({"type": "text", "text": &rest[..start]}));
        }
        let end = rest[start..]
            .find(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '|')
            .map(|e| start + e)
            .unwrap_or(rest.len());
        // "see https://x.dev/a." links the page, not "a."
        let end = start + crate::attachments::trim_link_end(&rest[start..end]).len();
        if rest[start..end].ends_with("://") {
            // A bare scheme with nothing after it is just text.
            elements.push(json!({"type": "text", "text": &rest[start..start + 1]}));
            rest = &rest[start + 1..];
            continue;
        }
        elements.push(json!({"type": "link", "url": &rest[start..end]}));
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        elements.push(json!({"type": "text", "text": rest}));
    }
    json!([{"type": "rich_text", "elements": [{"type": "rich_text_section", "elements": elements}]}])
}

/// A random v4 UUID, which Slack wants as the draft's `client_msg_id`.
pub(crate) fn uuid_v4() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Put a draft in the user's own Slack composer — in Drafts & Sent, not
/// posted. Returns the draft id.
///
/// `drafts.create` is not in Slack's published API. It is what Slack's own
/// clients call, and it accepts an OAuth user token; its siblings
/// (`drafts.list`, `drafts.update`, `drafts.delete`) do not, so a draft made
/// here is reviewed, edited, sent or discarded in Slack itself. Because it is
/// undocumented, Slack may change or close it without notice; errors are
/// reported as Slack gives them rather than papered over.
pub async fn draft_create(
    slack: &Slack,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<String> {
    let mut dest = json!({"channel_id": channel});
    if let Some(t) = thread_ts {
        dest["thread_ts"] = json!(t);
    }
    let v = slack
        .post(
            "drafts.create",
            &json!({
                "blocks": text_to_blocks(text),
                "destinations": [dest],
                "file_ids": [],
                "is_from_composer": false,
                "client_msg_id": uuid_v4(),
            }),
        )
        .await?;
    Ok(v.pointer("/draft/id")
        .and_then(|d| d.as_str())
        .unwrap_or_default()
        .to_string())
}

// ---------------------------------------------------------------------------
// Bookmarks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub id: String,
    pub title: String,
    pub link: String,
    pub kind: String,
    pub emoji: String,
}

pub(crate) fn bookmark_from(v: &Value) -> Bookmark {
    Bookmark {
        id: s(v, "id"),
        title: s(v, "title"),
        link: s(v, "link"),
        kind: s(v, "type"),
        emoji: s(v, "emoji"),
    }
}

/// The bookmarks bar of a channel (`bookmarks.list`, scope `bookmarks:read`).
pub async fn bookmarks(slack: &Slack, channel: &str) -> Result<Vec<Bookmark>> {
    let v = slack
        .get("bookmarks.list", &[("channel_id", channel.to_string())])
        .await?;
    Ok(v.get("bookmarks")
        .and_then(|b| b.as_array())
        .map(|a| a.iter().map(bookmark_from).collect())
        .unwrap_or_default())
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: String,
    /// The handle (`name`), which `@` mentions resolve against.
    pub name: String,
    pub real_name: String,
    pub display_name: String,
    pub email: String,
    pub is_bot: bool,
    pub deleted: bool,
    pub tz: String,
}

impl User {
    /// What to call them in output: display name, then real name, then handle.
    pub fn label(&self) -> &str {
        [&self.display_name, &self.real_name, &self.name, &self.id]
            .into_iter()
            .find(|x| !x.is_empty())
            .map(String::as_str)
            .unwrap_or("")
    }
}

pub(crate) fn user_from(v: &Value) -> User {
    let p = |k: &str| {
        v.pointer(&format!("/profile/{k}"))
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let real = Some(s(v, "real_name"))
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| p("real_name"));
    User {
        id: s(v, "id"),
        name: s(v, "name"),
        real_name: real,
        display_name: p("display_name"),
        email: p("email"),
        is_bot: b(v, "is_bot") || s(v, "id") == "USLACKBOT",
        deleted: b(v, "deleted"),
        tz: s(v, "tz"),
    }
}

pub async fn users(slack: &Slack, limit: usize) -> Result<Vec<User>> {
    let items = paged(
        slack,
        "users.list",
        &[("limit", PAGE.to_string())],
        "members",
        limit,
    )
    .await?;
    Ok(items.iter().map(user_from).collect())
}

pub async fn user_info(slack: &Slack, id: &str) -> Result<User> {
    let v = slack.get("users.info", &[("user", id.to_string())]).await?;
    Ok(user_from(v.get("user").unwrap_or(&Value::Null)))
}

fn is_email(s: &str) -> bool {
    let s = s.trim_start_matches('@');
    s.split_once('@')
        .is_some_and(|(a, d)| !a.is_empty() && d.contains('.'))
}

/// True when `u` answers to `query` by handle, display name or real name.
pub(crate) fn user_matches(u: &User, query: &str) -> bool {
    let q = query.trim_start_matches('@').to_lowercase();
    [&u.name, &u.display_name, &u.real_name]
        .iter()
        .any(|x| !x.is_empty() && x.to_lowercase() == q)
}

/// A person, from an id, an email, or `@handle` / display name / real name.
///
/// A handle is unique, so the walk of `users.list` stops at the first
/// handle match. A display or real name is not (two people can both be
/// "Alex"), so those keep looking for a second match before answering.
/// A name resolved exactly once is remembered for a few minutes.
pub async fn resolve_user(slack: &Slack, input: &str) -> Result<User> {
    let t = input.trim();
    if looks_like_user_id(t) {
        return user_info(slack, t).await;
    }
    if is_email(t) {
        let v = slack
            .get(
                "users.lookupByEmail",
                &[("email", t.trim_start_matches('@').to_string())],
            )
            .await?;
        return Ok(user_from(v.get("user").unwrap_or(&Value::Null)));
    }
    let q = t.trim_start_matches('@').to_lowercase();
    let cache_key = format!("user:{q}");
    if let Some(id) = slack.cache_get(&cache_key)
        && let Ok(u) = user_info(slack, &id).await
    {
        return Ok(u);
    }
    let mut found: Vec<User> = Vec::new();
    scan_paged(
        slack,
        "users.list",
        &[("limit", PAGE.to_string())],
        "members",
        |v| {
            let u = user_from(v);
            if u.deleted || !user_matches(&u, &q) {
                return false;
            }
            let by_handle = u.name.to_lowercase() == q;
            if by_handle {
                found = vec![u];
                return true;
            }
            found.push(u);
            false
        },
    )
    .await?;
    match found.as_slice() {
        [one] => {
            slack.cache_put(&cache_key, &one.id);
            Ok(one.clone())
        }
        [] => bail!(
            "no Slack user matches {t}; `sidekar slack users {}` searches",
            t.trim_start_matches('@')
        ),
        many => bail!(
            "{t} matches several people: {}. Use their id or email.",
            many.iter()
                .map(|u| format!("{} ({}, {})", u.label(), u.name, u.id))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

/// How many `users.info` calls run at once when naming people.
const NAME_LOOKUPS_AT_ONCE: usize = 8;

/// Display names for these user ids. Remembered names come from the cache;
/// the rest are looked up a few at a time rather than one after another. A
/// lookup that fails leaves the id in place rather than failing the read.
pub async fn names_for(slack: &Slack, ids: &[String]) -> HashMap<String, String> {
    use futures_util::StreamExt;
    let mut out = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    for id in ids {
        if id.is_empty() || out.contains_key(id) || missing.contains(id) {
            continue;
        }
        match slack.cache_get(&format!("name:{id}")) {
            Some(label) => {
                out.insert(id.clone(), label);
            }
            None => missing.push(id.clone()),
        }
    }
    let looked_up: Vec<(String, Option<User>)> = futures_util::stream::iter(missing)
        .map(|id| async move {
            let u = user_info(slack, &id).await.ok();
            (id, u)
        })
        .buffer_unordered(NAME_LOOKUPS_AT_ONCE)
        .collect()
        .await;
    for (id, u) in looked_up {
        if let Some(u) = u {
            let label = u.label().to_string();
            slack.cache_put(&format!("name:{id}"), &label);
            out.insert(id, label);
        }
    }
    out
}

/// Every user id a message mentions or was written by.
pub fn user_ids_in(messages: &[Message]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for m in messages {
        for id in std::iter::once(m.user.clone()).chain(mentioned_user_ids(&m.text)) {
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

pub(crate) fn mentioned_user_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("<@") {
        rest = &rest[i + 2..];
        let end = rest.find(['>', '|']).unwrap_or(rest.len());
        out.push(rest[..end].to_string());
    }
    out
}

/// Slack's wire markup made readable: `<@U1>` becomes `@name`, `<#C1|x>`
/// becomes `#x`, links show their label and target, and the three escaped
/// characters come back.
pub fn render_text(text: &str, names: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inner = &after[..end];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t, Some(l)),
            None => (inner, None),
        };
        let rendered = if let Some(id) = target.strip_prefix('@') {
            let name = names.get(id).map(String::as_str).or(label).unwrap_or(id);
            format!("@{name}")
        } else if let Some(id) = target.strip_prefix('#') {
            format!("#{}", label.unwrap_or(id))
        } else if let Some(special) = target.strip_prefix('!') {
            match label {
                Some(l) => l.to_string(),
                None => format!("@{}", special.split('^').next().unwrap_or(special)),
            }
        } else if let Some(addr) = target.strip_prefix("mailto:") {
            label.unwrap_or(addr).to_string()
        } else {
            match label {
                Some(l) if l != target => format!("{l} ({target})"),
                _ => target.to_string(),
            }
        };
        out.push_str(&rendered);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// A message ts (`1700000000.123456`) as a UTC date.
pub fn ts_to_date(ts: &str) -> String {
    ts.split('.')
        .next()
        .and_then(|secs| secs.parse::<i64>().ok())
        .map(crate::utils::epoch_to_date)
        .unwrap_or_else(|| ts.to_string())
}

#[cfg(test)]
mod tests;
