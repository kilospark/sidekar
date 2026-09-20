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

/// Download a file's content as text.
///
/// Google-native docs cannot be downloaded directly and must be exported, so
/// a Doc comes back as plain text and a Sheet as CSV.
pub async fn get_text(token: &super::auth::TokenRef, id: &str) -> Result<String> {
    let meta = super::api_get(token, &format!("{FILES}/{id}?fields=name,mimeType")).await?;
    let mime = str_at(&meta, "mimeType");
    let url = match mime.as_str() {
        "application/vnd.google-apps.document" => {
            format!("{FILES}/{id}/export?mimeType=text/plain")
        }
        "application/vnd.google-apps.spreadsheet" => {
            format!("{FILES}/{id}/export?mimeType=text/csv")
        }
        "application/vnd.google-apps.presentation" => {
            format!("{FILES}/{id}/export?mimeType=text/plain")
        }
        _ => format!("{FILES}/{id}?alt=media"),
    };
    let token = super::auth::access_token_for(token).await?;
    let res = reqwest::Client::new()
        .get(&url)
        .bearer_auth(token)
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "{status} downloading {id}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    Ok(text)
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

    let token = super::auth::access_token_for(token).await?;
    let res = reqwest::Client::new()
        .post(format!("{UPLOAD}?uploadType=multipart"))
        .bearer_auth(token)
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
