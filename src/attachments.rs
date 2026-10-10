//! Shared plumbing for the Slack and Linear file commands: sizes, where a
//! download lands, and which hosts may see a token.
//!
//! Naming and text detection come from the Gmail/Drive tools
//! ([`crate::google::gmail::safe_filename`], [`crate::google::drive::looks_like_text`])
//! so every `sidekar` tool saves and prints files the same way.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub(crate) use crate::google::drive::looks_like_text;
pub(crate) use crate::google::gmail::{mime_for, safe_filename};

/// `1536` → `1.5K`, as `gmail attachments` prints sizes.
pub(crate) fn human_bytes(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b}B"),
        b if b < 1024 * 1024 => format!("{:.1}K", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1}M", b as f64 / (1024.0 * 1024.0)),
        b => format!("{:.1}G", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}

/// Where a download goes: `--out` as given, or inside it when it names a
/// directory (existing, or written with a trailing `/`); else the file's own
/// name in the current directory. The remote name is attacker-chosen, so only
/// its last path component is ever used.
pub(crate) fn output_path(out: Option<&str>, remote_name: &str) -> PathBuf {
    let name = safe_filename(remote_name);
    match out {
        Some(o) if o.ends_with('/') || o.ends_with('\\') || Path::new(o).is_dir() => {
            Path::new(o).join(name)
        }
        Some(o) => PathBuf::from(o),
        None => PathBuf::from(name),
    }
}

/// Write bytes, creating the parent directory. Returns the path written.
pub(crate) fn save(out: Option<&str>, remote_name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let path = output_path(out, remote_name);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(&path, bytes).with_context(|| format!("could not write {}", path.display()))?;
    Ok(path)
}

/// Text for stdout, or a refusal that names the way around it, as
/// `drive get` does.
pub(crate) fn printable(name: &str, bytes: &[u8]) -> Result<String> {
    if looks_like_text(bytes) {
        Ok(String::from_utf8_lossy(bytes).into_owned())
    } else {
        bail!(
            "{name} is binary ({}). Save it with --out <path>; printing it would corrupt both \
             the file and your terminal.",
            human_bytes(bytes.len() as u64)
        )
    }
}

/// The host of an https URL (or http, for the local test mock), lowercased.
pub(crate) fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = if host.starts_with('[') {
        host.split(']').next().map(|h| format!("{h}]"))?
    } else {
        host.split(':').next()?.to_string()
    };
    Some(host.to_ascii_lowercase())
}

/// Whether a token may be sent to `url`: only to the service's own hosts
/// (`domain` itself or a subdomain of it) over https, or to `test_host`, the
/// host a test client points at. A file URL comes out of an API response, and
/// attaching a bearer token to whatever host it names would hand the token to
/// anyone who can post a link.
pub(crate) fn token_may_go_to(url: &str, domain: &str, test_host: Option<&str>) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    if test_host.is_some_and(|t| t.eq_ignore_ascii_case(&host)) {
        return true;
    }
    url.starts_with("https://") && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// Read a local file for upload: its bytes, file name and content type.
pub(crate) fn read_upload(path: &str) -> Result<(Vec<u8>, String, String)> {
    let p = Path::new(path);
    let bytes = std::fs::read(p).with_context(|| format!("could not read {path}"))?;
    let name = p
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("{path} has no file name"))?;
    if bytes.is_empty() {
        bail!("{path} is empty; there is nothing to upload");
    }
    let mime = mime_for(&name).to_string();
    Ok((bytes, name, mime))
}

/// The end of a URL found in prose, without the punctuation that closes the
/// sentence around it: `see https://x.dev/a.` links `https://x.dev/a`.
/// A closing bracket stays when the URL opened it
/// (`https://en.wikipedia.org/wiki/Rust_(language)`), and goes when it
/// belongs to the text (`(see https://x.dev/a)`).
pub(crate) fn trim_link_end(url: &str) -> &str {
    let mut u = url;
    loop {
        let Some(last) = u.chars().last() else {
            return u;
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' | '_' | '~' | '`' => true,
            ')' => u.matches('(').count() < u.matches(')').count(),
            ']' => u.matches('[').count() < u.matches(']').count(),
            '}' => u.matches('{').count() < u.matches('}').count(),
            _ => false,
        };
        if !drop {
            return u;
        }
        u = &u[..u.len() - last.len_utf8()];
    }
}

#[cfg(test)]
mod tests;
