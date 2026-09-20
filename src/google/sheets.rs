//! Google Sheets over the API.
//!
//! Drive can already export a sheet as CSV, but only whole-file and read-only.
//! This reads and writes ranges, which is what makes a sheet usable as storage
//! rather than as a document.

use anyhow::{Result, bail};
use serde_json::{Value, json};

const BASE: &str = "https://sheets.googleapis.com/v4/spreadsheets";

/// Read a range in A1 notation, e.g. `Sheet1!A1:D20` or just `Sheet1`.
pub async fn get(id: &str, range: &str) -> Result<Vec<Vec<String>>> {
    let url = format!("{BASE}/{id}/values/{}", urlencoding::encode(range));
    let res = super::api_get(&url).await?;
    Ok(rows_from(&res))
}

/// Overwrite a range. `values` is row-major.
pub async fn set(id: &str, range: &str, values: &[Vec<String>]) -> Result<usize> {
    // RAW, not USER_ENTERED: a cell starting with = or + would otherwise be
    // evaluated as a formula, which turns pasted data into something else.
    let url = format!(
        "{BASE}/{id}/values/{}?valueInputOption=RAW",
        urlencoding::encode(range)
    );
    let token = super::auth::access_token().await?;
    let res = reqwest::Client::new()
        .put(&url)
        .bearer_auth(token)
        .json(&json!({"values": values}))
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "{status} writing {range}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(v.get("updatedCells").and_then(|c| c.as_u64()).unwrap_or(0) as usize)
}

/// Append rows after the last row with data.
pub async fn append(id: &str, range: &str, values: &[Vec<String>]) -> Result<usize> {
    let url = format!(
        "{BASE}/{id}/values/{}:append?valueInputOption=RAW&insertDataOption=INSERT_ROWS",
        urlencoding::encode(range)
    );
    let res = super::api_post(&url, &json!({"values": values})).await?;
    Ok(res
        .get("updates")
        .and_then(|u| u.get("updatedCells"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0) as usize)
}

/// Tab names and the spreadsheet title.
pub async fn info(id: &str) -> Result<(String, Vec<String>)> {
    let res = super::api_get(&format!(
        "{BASE}/{id}?fields=properties.title,sheets.properties.title"
    ))
    .await?;
    let title = res
        .get("properties")
        .and_then(|p| p.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let tabs = res
        .get("sheets")
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    s.get("properties")
                        .and_then(|p| p.get("title"))
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((title, tabs))
}

pub async fn create(title: &str) -> Result<String> {
    let res = super::api_post(BASE, &json!({"properties": {"title": title}})).await?;
    Ok(res
        .get("spreadsheetId")
        .and_then(|i| i.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Sheets omits trailing empty cells, so rows come back ragged.
pub(crate) fn rows_from(res: &Value) -> Vec<Vec<String>> {
    res.get("values")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    r.as_array()
                        .map(|cells| {
                            cells
                                .iter()
                                .map(|c| match c {
                                    Value::String(s) => s.clone(),
                                    Value::Null => String::new(),
                                    other => other.to_string(),
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pad rows to equal width so a ragged response prints as a table.
pub(crate) fn squared(rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    rows.into_iter()
        .map(|mut r| {
            r.resize(width, String::new());
            r
        })
        .collect()
}

#[cfg(test)]
mod tests;
