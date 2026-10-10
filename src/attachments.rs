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

/// A remote file name made safe to save under: its last path component only
/// ([`safe_filename`]), and never a hidden file. A file named `.bashrc` or
/// `.npmrc` by whoever shared it lands as `_bashrc`, not as configuration
/// something else on this machine will read.
pub(crate) fn local_name(remote_name: &str) -> String {
    let name = safe_filename(remote_name);
    let shown = name.trim_start_matches('.');
    if shown.len() == name.len() {
        name
    } else if shown.is_empty() {
        "attachment".to_string()
    } else {
        format!("_{shown}")
    }
}

/// Where a download goes: `--out` as given, or inside it when it names a
/// directory (existing, or written with a trailing `/`); else the file's own
/// name in the current directory. The remote name is attacker-chosen, so only
/// its last path component is ever used, and never as a hidden file.
pub(crate) fn output_path(out: Option<&str>, remote_name: &str) -> PathBuf {
    match out {
        Some(o) if out_is_dir(o) => Path::new(o).join(local_name(remote_name)),
        Some(o) => PathBuf::from(o),
        None => PathBuf::from(local_name(remote_name)),
    }
}

fn out_is_dir(o: &str) -> bool {
    o.ends_with('/') || o.ends_with('\\') || Path::new(o).is_dir()
}

/// File names for saving several files into one directory, made distinct:
/// the second `image.png` becomes `image-2.png`, so one download never
/// overwrites another. Names are compared as they will be saved
/// ([`safe_filename`]) and ignoring case, for case-insensitive filesystems.
pub(crate) fn distinct_names(names: &[String]) -> Vec<String> {
    let mut taken = std::collections::HashSet::new();
    names
        .iter()
        .map(|n| {
            let n = local_name(n);
            let (stem, ext) = match n.rfind('.') {
                Some(i) if i > 0 => (&n[..i], &n[i..]),
                _ => (n.as_str(), ""),
            };
            let mut candidate = n.clone();
            let mut k = 2;
            while !taken.insert(candidate.to_lowercase()) {
                candidate = format!("{stem}-{k}{ext}");
                k += 1;
            }
            candidate
        })
        .collect()
}

/// Write bytes, creating the parent directory. Returns the path written.
///
/// A path the user typed (`--out report.pdf`) is written as asked. A name
/// that came from the remote side never replaces a file already there: it
/// becomes `name-2.ext` (and so on) instead, created exclusively, so a shared
/// file cannot overwrite `~/.profile`, a project's files, or anything a
/// symlink points at.
pub(crate) fn save(out: Option<&str>, remote_name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let path = output_path(out, remote_name);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    if out.is_some_and(|o| !out_is_dir(o)) {
        std::fs::write(&path, bytes)
            .with_context(|| format!("could not write {}", path.display()))?;
        return Ok(path);
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "attachment".into());
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    for k in 1..=1000 {
        let candidate = if k == 1 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem}-{k}{ext}"))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(bytes)
                    .with_context(|| format!("could not write {}", candidate.display()))?;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("could not write {}", candidate.display()));
            }
        }
    }
    bail!(
        "{} and 999 numbered copies already exist; pass --out <path>",
        path.display()
    )
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

/// The URL a token may be sent to, parsed once: the caller hands this same
/// [`reqwest::Url`] to `.get(…)`, so the host checked is the host connected to.
/// (A hand-rolled check once read `https://evil.example\@uploads.linear.app/`
/// as Linear's host while the WHATWG parser behind reqwest reads `\` as `/`
/// and connects to `evil.example`.)
///
/// Allowed: https, no user or password, default port, and a host that is
/// `domain` or a subdomain of it. A file URL comes out of an API response or
/// an issue, and attaching a token to whatever host it names would hand the
/// token to anyone who can post a link. `api_base`, the API the token is
/// already sent to (or a test's mock of it), allows its exact origin
/// (scheme, host and port).
pub(crate) fn token_url(url: &str, domain: &str, api_base: Option<&str>) -> Option<reqwest::Url> {
    let u = reqwest::Url::parse(url.trim()).ok()?;
    if let Some(base) = api_base.and_then(|b| reqwest::Url::parse(b).ok())
        && u.origin() == base.origin()
    {
        return Some(u);
    }
    let host = u.host_str()?;
    let ok = u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && u.port().is_none()
        && matches!(u.host(), Some(url::Host::Domain(_)))
        && (host == domain || host.ends_with(&format!(".{domain}")));
    ok.then_some(u)
}

/// Whether `url` is an https URL on exactly `host`, by the same parser the
/// request would use.
pub(crate) fn is_https_on(url: &str, host: &str) -> bool {
    reqwest::Url::parse(url.trim())
        .is_ok_and(|u| u.scheme() == "https" && u.host_str() == Some(host))
}

/// The largest file a service takes in one upload.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UploadLimit {
    pub service: &'static str,
    pub max: u64,
}

/// Slack's per-file limit (1 GB, `files.getUploadURLExternal`).
pub(crate) const SLACK_UPLOAD: UploadLimit = UploadLimit {
    service: "Slack",
    max: 1 << 30,
};

/// Linear's: `fileUpload` takes the size as a GraphQL `Int` (32-bit).
pub(crate) const LINEAR_UPLOAD: UploadLimit = UploadLimit {
    service: "Linear",
    max: i32::MAX as u64,
};

/// Read a local file for upload: its bytes, file name and content type.
/// The size is checked from metadata first, so a file over the service's
/// limit is refused before any of it is read into memory.
pub(crate) fn read_upload(path: &str, limit: UploadLimit) -> Result<(Vec<u8>, String, String)> {
    let p = Path::new(path);
    check_upload(path, limit)?;
    let name = p
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow::anyhow!("{path} has no file name"))?;
    let bytes = std::fs::read(p).with_context(|| format!("could not read {path}"))?;
    if bytes.is_empty() {
        bail!("{path} is empty; there is nothing to upload");
    }
    if bytes.len() as u64 > limit.max {
        bail!(
            "{path} grew past {}'s limit while being read",
            limit.service
        );
    }
    let mime = mime_for(&name).to_string();
    Ok((bytes, name, mime))
}

/// Whether a path can be uploaded, from its metadata alone: it exists, is a
/// file with a name, is not empty and is within the limit. For checking every
/// path before anything is sent, without reading any of them.
pub(crate) fn check_upload(path: &str, limit: UploadLimit) -> Result<()> {
    let p = Path::new(path);
    let meta = std::fs::metadata(p).with_context(|| format!("could not read {path}"))?;
    if !meta.is_file() {
        bail!("{path} is not a file");
    }
    if meta.len() > limit.max {
        bail!(
            "{path} is {}; {} takes files up to {}",
            human_bytes(meta.len()),
            limit.service,
            human_bytes(limit.max)
        );
    }
    if p.file_name().is_none() {
        bail!("{path} has no file name");
    }
    if meta.len() == 0 {
        bail!("{path} is empty; there is nothing to upload");
    }
    Ok(())
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
