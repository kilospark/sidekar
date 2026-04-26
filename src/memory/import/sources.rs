//! Source detection — figure out which local agent caches exist
//! and hand back a ranked list of files worth reading.
//!
//! Every function here is pure path-walking. Parsing is deferred
//! to dedicated modules (`extract_structured`, `parse_jsonl`, etc.)
//! so detection stays cheap and testable with a fixture directory.

use super::ImportOptions;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One file worth importing, with enough metadata for downstream
/// code to decide scope + project + recency.
#[derive(Debug, Clone)]
pub(super) struct DetectedFile {
    pub path: PathBuf,
    pub mtime_secs: u64,
    #[allow(dead_code)]
    pub size: u64,
}

/// Registry of every source the importer knows about. Each entry
/// is an identifier (must match the `--source=` CLI flag) plus a
/// human-readable label for reports.
pub(super) const SOURCE_IDS: &[&str] = &[
    "claude",
    "codex",
    "cursor",
    "gemini",
    "opencode",
    "copilot",
    "windsurf",
    "manifests",
];

pub(super) fn is_valid_source(id: &str) -> bool {
    SOURCE_IDS.contains(&id) || id == "all"
}

/// Resolve `--source=` into a concrete allowlist. Empty / `all`
/// means every known source.
pub(super) fn resolve_sources(selected: &[String]) -> Vec<&'static str> {
    if selected.is_empty() || selected.iter().any(|s| s == "all") {
        return SOURCE_IDS.to_vec();
    }
    SOURCE_IDS
        .iter()
        .filter(|id| selected.iter().any(|s| s == *id))
        .copied()
        .collect()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn detect_file(path: PathBuf) -> Option<DetectedFile> {
    if !path.is_file() {
        return None;
    }
    let mtime = mtime_secs(&path);
    let sz = size(&path);
    Some(DetectedFile {
        path,
        mtime_secs: mtime,
        size: sz,
    })
}

/// Walk a directory up to `max_depth`, yielding every file where
/// `accept` returns true. Directory errors are silently skipped —
/// we don't want a chmod-600 subtree to kill the whole source.
fn walk_dir<F>(root: &Path, max_depth: usize, mut accept: F) -> Vec<DetectedFile>
where
    F: FnMut(&Path) -> bool,
{
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = Vec::new();
    stack.push((root.to_path_buf(), 0));
    while let Some((dir, depth)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == ".DS_Store")
            {
                continue;
            }
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_dir() {
                if depth + 1 <= max_depth {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            if !accept(&path) {
                continue;
            }
            if let Some(df) = detect_file(path) {
                out.push(df);
            }
        }
    }
    out
}

/// Apply `--since=` and `--max-sessions=` to a list of detected
/// files, keeping the most recent N whose mtime is newer than the
/// cutoff. Used for transcript sources (Claude / Codex / Gemini /
/// Opencode) where we want recency, not the full archive.
pub(super) fn prune_recent(
    mut files: Vec<DetectedFile>,
    opts: &ImportOptions,
) -> Vec<DetectedFile> {
    if let Some(secs) = opts.since_secs {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let cutoff = now.saturating_sub(secs);
        files.retain(|f| f.mtime_secs >= cutoff);
    }
    files.sort_by(|a, b| b.mtime_secs.cmp(&a.mtime_secs));
    if opts.max_sessions > 0 {
        files.truncate(opts.max_sessions);
    }
    files
}

// ---- Per-source detectors -------------------------------------------------

/// Claude Code: `~/.claude/projects/<slug>/*.jsonl` transcripts
/// and `~/.claude/CLAUDE.md` / `<cwd>/CLAUDE.md` / `<cwd>/.claude/CLAUDE.md`
/// preference files.
pub(super) fn detect_claude_sessions() -> Vec<DetectedFile> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let root = home.join(".claude").join("projects");
    if !root.exists() {
        return Vec::new();
    }
    walk_dir(&root, 4, |p| {
        p.extension().and_then(|e| e.to_str()) == Some("jsonl")
    })
}

pub(super) fn detect_claude_prefs(cwd: &Path) -> Vec<DetectedFile> {
    let mut files = Vec::new();
    if let Some(home) = home() {
        let p = home.join(".claude").join("CLAUDE.md");
        if let Some(df) = detect_file(p) {
            files.push(df);
        }
    }
    let p = cwd.join(".claude").join("CLAUDE.md");
    if let Some(df) = detect_file(p) {
        files.push(df);
    }
    let p = cwd.join("CLAUDE.md");
    if let Some(df) = detect_file(p) {
        files.push(df);
    }
    files
}

/// Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` and
/// `~/.codex/AGENTS.md` + `~/.codex/memories/*`.
pub(super) fn detect_codex_sessions() -> Vec<DetectedFile> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let root = home.join(".codex").join("sessions");
    if !root.exists() {
        return Vec::new();
    }
    walk_dir(&root, 4, |p| {
        p.extension().and_then(|e| e.to_str()) == Some("jsonl")
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
    })
}

pub(super) fn detect_codex_prefs() -> Vec<DetectedFile> {
    let mut files = Vec::new();
    let Some(home) = home() else {
        return files;
    };
    let agents = home.join(".codex").join("AGENTS.md");
    if let Some(df) = detect_file(agents) {
        files.push(df);
    }
    let memories = home.join(".codex").join("memories");
    if memories.exists() {
        files.extend(walk_dir(&memories, 2, |p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("md") | Some("txt")
            )
        }));
    }
    files
}

/// Cursor chat DBs: `~/.cursor/chats/<workspace>/<chat-id>/store.db`.
pub(super) fn detect_cursor_sessions() -> Vec<DetectedFile> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let root = home.join(".cursor").join("chats");
    if !root.exists() {
        return Vec::new();
    }
    walk_dir(&root, 4, |p| {
        p.file_name().and_then(|n| n.to_str()) == Some("store.db")
    })
}

pub(super) fn detect_cursor_rules(cwd: &Path) -> Vec<DetectedFile> {
    let mut files = Vec::new();
    let p = cwd.join(".cursorrules");
    if let Some(df) = detect_file(p) {
        files.push(df);
    }
    let rules_dir = cwd.join(".cursor").join("rules");
    if rules_dir.exists() {
        files.extend(walk_dir(&rules_dir, 2, |_| true));
    }
    if let Some(home) = home() {
        let global_rules = home.join(".cursor").join("rules");
        if global_rules.exists() {
            files.extend(walk_dir(&global_rules, 2, |_| true));
        }
    }
    files
}

/// Gemini CLI sessions. On-disk they live under
/// `~/.gemini/tmp/<project>/chats/session-*.json`. Global prefs
/// and skills live elsewhere in `~/.gemini`.
pub(super) fn detect_gemini_sessions() -> Vec<DetectedFile> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let root = home.join(".gemini").join("tmp");
    if !root.exists() {
        return Vec::new();
    }
    walk_dir(&root, 4, |p| {
        p.parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some("chats")
            && p.extension().and_then(|e| e.to_str()) == Some("json")
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-"))
    })
}

pub(super) fn detect_gemini_prefs() -> Vec<DetectedFile> {
    let mut files = Vec::new();
    let Some(home) = home() else {
        return files;
    };
    let gemini = home.join(".gemini");
    for name in ["GEMINI.md", "settings.json"] {
        if let Some(df) = detect_file(gemini.join(name)) {
            files.push(df);
        }
    }
    let skills = gemini.join("skills");
    if skills.exists() {
        files.extend(walk_dir(&skills, 2, |p| {
            p.file_name().and_then(|n| n.to_str()) == Some("SKILL.md")
        }));
    }
    files
}

/// Opencode: single SQLite DB at
/// `~/.local/share/opencode/opencode.db`. We only report the file
/// here; the parser opens it and iterates `session`/`message`.
pub(super) fn detect_opencode_db() -> Option<DetectedFile> {
    let home = home()?;
    let p = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    detect_file(p)
}

pub(super) fn detect_copilot(cwd: &Path) -> Vec<DetectedFile> {
    let p = cwd.join(".github").join("copilot-instructions.md");
    detect_file(p).into_iter().collect()
}

pub(super) fn detect_windsurf(cwd: &Path) -> Vec<DetectedFile> {
    let p = cwd.join(".windsurfrules");
    detect_file(p).into_iter().collect()
}

/// Manifests / config files we know how to extract deterministically.
pub(super) fn detect_manifests(cwd: &Path) -> Vec<DetectedFile> {
    let candidates: &[&str] = &[
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "requirements.txt",
        "tsconfig.json",
        "README.md",
        "CONTRIBUTING.md",
    ];
    let mut files = Vec::new();
    for name in candidates {
        if let Some(df) = detect_file(cwd.join(name)) {
            files.push(df);
        }
    }
    for docs_dir in ["docs/adr", "docs/decisions"] {
        let d = cwd.join(docs_dir);
        if d.exists() {
            files.extend(walk_dir(&d, 2, |p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("md") | Some("markdown")
                )
            }));
        }
    }
    // Workflows — just detect presence, extractor counts files.
    let wf = cwd.join(".github").join("workflows");
    if wf.exists() {
        files.extend(walk_dir(&wf, 1, |p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("yml") | Some("yaml")
            )
        }));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_sources_empty_means_all() {
        assert_eq!(resolve_sources(&[]).len(), SOURCE_IDS.len());
    }

    #[test]
    fn resolve_sources_all_keyword() {
        assert_eq!(
            resolve_sources(&["all".to_string()]).len(),
            SOURCE_IDS.len()
        );
    }

    #[test]
    fn resolve_sources_filters_unknown() {
        let got = resolve_sources(&["claude".to_string(), "bogus".to_string()]);
        assert_eq!(got, vec!["claude"]);
    }

    #[test]
    fn is_valid_source_accepts_known() {
        assert!(is_valid_source("claude"));
        assert!(is_valid_source("all"));
        assert!(!is_valid_source("bogus"));
    }
}
