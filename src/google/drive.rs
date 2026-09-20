//! Google Drive over the API.

use anyhow::Result;
use serde_json::{Value, json};

const FILES: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";

pub struct Entry {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub modified: String,
    pub size: Option<String>,
}

/// List or search. `query` is Drive query syntax; empty lists the root.
pub async fn list(token: &super::auth::TokenRef, query: &str, limit: usize) -> Result<Vec<Entry>> {
    let q = if query.trim().is_empty() {
        "'root' in parents and trashed = false".to_string()
    } else if query.contains('=') || query.contains(" in ") || query.contains("contains") {
        query.to_string()
    } else {
        // A bare word is what people actually type; treat it as a name search
        // rather than making them learn Drive's query grammar first.
        format!(
            "name contains '{}' and trashed = false",
            query.replace('\'', "\\'")
        )
    };
    let url = format!(
        "{FILES}?q={}&pageSize={}&fields=files(id,name,mimeType,modifiedTime,size)",
        urlencoding::encode(&q),
        limit.clamp(1, 1000)
    );
    let res = super::api_get(token, &url).await?;
    Ok(res
        .get("files")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .map(|f| Entry {
                    id: str_at(f, "id"),
                    name: str_at(f, "name"),
                    mime: str_at(f, "mimeType"),
                    modified: str_at(f, "modifiedTime"),
                    size: f.get("size").and_then(|s| s.as_str()).map(String::from),
                })
                .collect()
        })
        .unwrap_or_default())
}

/// What came back from Drive for one file.
pub struct Download {
    pub bytes: Vec<u8>,
    pub name: String,
    /// Drive's own byte count, absent for Google-native files, which have none.
    pub reported_size: Option<u64>,
    /// True when the file was exported rather than downloaded, so it really is
    /// text: a Doc as plain text, a Sheet as CSV.
    pub exported: bool,
}

/// Download a file as bytes.
///
/// Bytes, never a String. Decoding a PDF or a .docx as UTF-8 replaces every
/// invalid sequence with U+FFFD, which is three bytes where one stood — so the
/// file arrives larger than it left, by an amount that tracks how binary it is,
/// and is unopenable. Google-native files have no bytes to download and are
/// exported instead, which genuinely is text.
pub async fn download(token: &super::auth::TokenRef, id: &str) -> Result<Download> {
    let meta = super::api_get(token, &format!("{FILES}/{id}?fields=name,mimeType,size")).await?;
    let mime = str_at(&meta, "mimeType");
    let name = str_at(&meta, "name");
    let reported_size = meta
        .get("size")
        .and_then(|s| s.as_str())
        .and_then(|s| s.parse().ok());

    let (url, exported) = match export_mime_for(&mime) {
        Some(export_as) => (
            format!(
                "{FILES}/{id}/export?mimeType={}",
                urlencoding::encode(export_as)
            ),
            true,
        ),
        None => (format!("{FILES}/{id}?alt=media"), false),
    };

    let access = super::auth::access_token_for(token).await?;
    let res = reqwest::Client::new()
        .get(&url)
        .bearer_auth(access)
        .send()
        .await?;
    let status = res.status();
    let bytes = res.bytes().await?.to_vec();
    if !status.is_success() {
        anyhow::bail!(
            "{status} downloading {id}: {}",
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(300)
                .collect::<String>()
        );
    }
    // Drive tells us how big the file is; a mismatch means we mangled it in
    // transit, which is precisely the failure this rewrite exists to end.
    if let Some(expected) = reported_size
        && !exported
        && bytes.len() as u64 != expected
    {
        anyhow::bail!(
            "{name} came down as {} bytes but Drive reports {expected}. Refusing to write a \
             file that does not match its source.",
            bytes.len()
        );
    }
    Ok(Download {
        bytes,
        name,
        reported_size,
        exported,
    })
}

/// The export format for a Google-native file, or `None` for a real file.
pub(crate) fn export_mime_for(mime: &str) -> Option<&'static str> {
    match mime {
        "application/vnd.google-apps.document" => Some("text/plain"),
        "application/vnd.google-apps.spreadsheet" => Some("text/csv"),
        "application/vnd.google-apps.presentation" => Some("text/plain"),
        _ => None,
    }
}

/// True when these bytes can be printed to a terminal without wrecking it.
pub(crate) fn looks_like_text(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return false;
    }
    std::str::from_utf8(bytes).is_ok()
}

/// Upload a local file. Multipart so name and content land in one request.
pub async fn put(
    token: &super::auth::TokenRef,
    path: &str,
    name: Option<&str>,
    folder: Option<&str>,
) -> Result<String> {
    let bytes = std::fs::read(path)?;
    let name = name
        .map(String::from)
        .or_else(|| {
            std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "upload".into());

    let mut meta = json!({"name": name});
    if let Some(f) = folder {
        meta["parents"] = json!([f]);
    }

    const BOUNDARY: &str = "sidekar-drive-boundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n\
             --{BOUNDARY}\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--").as_bytes());

    let access = super::auth::access_token_for(token).await?;
    let res = reqwest::Client::new()
        .post(format!("{UPLOAD}?uploadType=multipart"))
        .bearer_auth(access)
        .header(
            "Content-Type",
            format!("multipart/related; boundary={BOUNDARY}"),
        )
        .body(body)
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "{status} uploading: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    let v: Value = serde_json::from_str(&text)?;
    Ok(str_at(&v, "id"))
}

/// Move a file to the trash, or erase it outright.
///
/// Trashing is the default because it is recoverable for 30 days; permanent
/// deletion of someone's Drive file has no undo, so it has to be asked for.
pub async fn remove(token: &super::auth::TokenRef, id: &str, permanent: bool) -> Result<()> {
    let token = super::auth::access_token_for(token).await?;
    let client = reqwest::Client::new();
    let res = if permanent {
        client
            .delete(format!("{FILES}/{id}"))
            .bearer_auth(token)
            .send()
            .await?
    } else {
        client
            .patch(format!("{FILES}/{id}"))
            .bearer_auth(token)
            .json(&json!({"trashed": true}))
            .send()
            .await?
    };
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        anyhow::bail!(
            "{status} removing {id}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    Ok(())
}

pub(crate) fn str_at(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Drive reports sizes as strings, and Google-native files report none at all.
pub(crate) fn human_size(size: Option<&str>) -> String {
    let Some(n) = size.and_then(|s| s.parse::<u64>().ok()) else {
        return "-".into();
    };
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}

#[cfg(test)]
mod tests;
