//! Google Docs over the API.
//!
//! Drive exports a doc as plain text, but only whole-file and read-only. This
//! also writes, which is what a doc has to do to be more than a report target.

use anyhow::Result;
use serde_json::{Value, json};

const BASE: &str = "https://docs.googleapis.com/v1/documents";

/// A document's text, paragraph by paragraph.
pub async fn get_text(id: &str) -> Result<String> {
    let doc = super::api_get(&format!("{BASE}/{id}")).await?;
    Ok(text_of(&doc))
}

pub async fn title(id: &str) -> Result<String> {
    let doc = super::api_get(&format!("{BASE}/{id}?fields=title")).await?;
    Ok(doc
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string())
}

pub async fn create(title: &str) -> Result<String> {
    let res = super::api_post(BASE, &json!({"title": title})).await?;
    Ok(res
        .get("documentId")
        .and_then(|i| i.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Add text to the end of a document.
pub async fn append(id: &str, text: &str) -> Result<()> {
    // endOfSegmentLocation rather than a computed index: the body's end index
    // moves with every edit, and asking Docs for "the end" avoids racing it.
    super::api_post(
        &format!("{BASE}/{id}:batchUpdate"),
        &json!({"requests": [{
            "insertText": {
                "endOfSegmentLocation": {},
                "text": text
            }
        }]}),
    )
    .await?;
    Ok(())
}

/// Replace every occurrence of `find` with `replace`.
pub async fn replace(id: &str, find: &str, replace: &str) -> Result<usize> {
    let res = super::api_post(
        &format!("{BASE}/{id}:batchUpdate"),
        &json!({"requests": [{
            "replaceAllText": {
                "containsText": {"text": find, "matchCase": true},
                "replaceText": replace
            }
        }]}),
    )
    .await?;
    Ok(res
        .get("replies")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .and_then(|r| r.get("replaceAllText"))
        .and_then(|r| r.get("occurrencesChanged"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0) as usize)
}

/// Flatten a document's structural content into plain text.
///
/// A Docs body is a list of structural elements, and only paragraphs carry
/// readable runs; tables and section breaks are skipped rather than rendered
/// wrong.
pub(crate) fn text_of(doc: &Value) -> String {
    let Some(content) = doc
        .get("body")
        .and_then(|b| b.get("content"))
        .and_then(|c| c.as_array())
    else {
        return String::new();
    };
    let mut out = String::new();
    for element in content {
        let Some(elems) = element
            .get("paragraph")
            .and_then(|p| p.get("elements"))
            .and_then(|e| e.as_array())
        else {
            continue;
        };
        for run in elems {
            if let Some(t) = run
                .get("textRun")
                .and_then(|t| t.get("content"))
                .and_then(|c| c.as_str())
            {
                out.push_str(t);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
