//! CLI entrypoint for `sidekar memory import`. Parses flags,
//! runs the selected source detectors, drives extraction,
//! writes the results.

use super::extract_structured;
use super::sources;
use super::{ImportOptions, ScopeFilter, SourceReport, WriteStats, format_report_table};
use crate::*;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn cmd_memory_import(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(msg) => bail!("{msg}"),
    };
    let cwd = std::env::current_dir()?;
    let allow = sources::resolve_sources(&opts.sources);
    let mut reports: Vec<SourceReport> = Vec::new();

    // ---- Scan phase -----------------------------------------------------
    for src in &allow {
        let report = match *src {
            "manifests" => scan_manifests(&opts, &cwd),
            "claude" => scan_claude(&opts, &cwd),
            "codex" => scan_codex(&opts, &cwd),
            "cursor" => scan_cursor(&opts, &cwd),
            "gemini" => scan_gemini(&opts, &cwd),
            "opencode" => scan_opencode(&opts, &cwd),
            "copilot" => scan_copilot(&opts, &cwd),
            "windsurf" => scan_windsurf(&opts, &cwd),
            _ => continue,
        };
        reports.push(report);
    }

    // Apply scope filter uniformly.
    for r in &mut reports {
        r.candidates.retain(|c| opts.keep_scope(&c.scope));
    }

    // ---- Report / confirm ----------------------------------------------
    out!(ctx, "{}", format_report_table(&reports));
    for r in &reports {
        for e in &r.errors {
            out!(ctx, "  [{}] {}", r.label, e);
        }
    }

    if opts.dry_run {
        out!(ctx, "\n[dry-run] no memories written.");
        preview_candidates(ctx, &reports, 5);
        return Ok(());
    }

    let total_candidates: usize = reports.iter().map(|r| r.candidates.len()).sum();
    if total_candidates == 0 {
        out!(ctx, "Nothing to import.");
        return Ok(());
    }
    if !opts.assume_yes && !confirm_interactive(total_candidates)? {
        out!(ctx, "Aborted.");
        return Ok(());
    }

    // ---- Write phase ----------------------------------------------------
    let batch_id = new_batch_id();
    let mut stats = WriteStats::default();
    for r in &reports {
        for c in &r.candidates {
            match crate::memory::write_memory_event(
                &c.project,
                &c.event_type,
                &c.scope,
                &c.summary,
                c.confidence,
                &c.tags,
                "passive",
                &c.source_kind,
            ) {
                Ok(msg) => {
                    if msg.starts_with("Deduplicated") {
                        stats.deduped += 1;
                    } else {
                        stats.written += 1;
                    }
                }
                Err(e) => stats.errors.push(format!("{}: {:#}", c.source_file.display(), e)),
            }
        }
        // Log files we touched so a re-run can short-circuit unchanged
        // ones. One row per (source_kind, path); duplicates get REPLACEd.
        for c in &r.candidates {
            if let Ok(hash) = file_hash(&c.source_file) {
                let _ = record_import(
                    &c.source_kind,
                    &c.source_file,
                    &hash,
                    &batch_id,
                    &stats,
                );
            }
        }
    }

    out!(
        ctx,
        "\n{} written, {} deduped, {} errors (batch {}).",
        stats.written,
        stats.deduped,
        stats.errors.len(),
        batch_id
    );
    for e in &stats.errors {
        out!(ctx, "  error: {e}");
    }
    Ok(())
}

// ---- Flag parsing ---------------------------------------------------------

fn parse_args(args: &[String]) -> Result<ImportOptions, String> {
    let mut opts = ImportOptions {
        sources: Vec::new(),
        project_override: None,
        scope_filter: ScopeFilter::All,
        since_secs: None,
        max_sessions: 5,
        no_llm: false,
        credential: None,
        model: None,
        dry_run: false,
        assume_yes: false,
        verbose: false,
    };
    for a in args {
        if let Some(v) = a.strip_prefix("--source=") {
            for s in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if !sources::is_valid_source(s) {
                    return Err(format!(
                        "Unknown source '{s}'. Valid: claude, codex, cursor, gemini, opencode, copilot, windsurf, manifests, all."
                    ));
                }
                opts.sources.push(s.to_string());
            }
        } else if let Some(v) = a.strip_prefix("--project=") {
            opts.project_override = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--scope=") {
            opts.scope_filter = match v {
                "project" => ScopeFilter::Project,
                "global" => ScopeFilter::Global,
                "all" => ScopeFilter::All,
                other => return Err(format!("Invalid --scope={other}. Use project|global|all.")),
            };
        } else if let Some(v) = a.strip_prefix("--since=") {
            opts.since_secs = Some(parse_duration(v)?);
        } else if let Some(v) = a.strip_prefix("--max-sessions=") {
            opts.max_sessions = v
                .parse::<usize>()
                .map_err(|_| format!("Invalid --max-sessions={v}"))?;
        } else if a == "--no-llm" {
            opts.no_llm = true;
        } else if let Some(v) = a.strip_prefix("--credential=") {
            opts.credential = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--model=") {
            opts.model = Some(v.to_string());
        } else if a == "--dry-run" {
            opts.dry_run = true;
        } else if a == "--yes" || a == "-y" {
            opts.assume_yes = true;
        } else if a == "--verbose" || a == "-v" {
            opts.verbose = true;
        } else if a == "--help" || a == "-h" {
            return Err(usage_message());
        } else {
            return Err(format!("Unknown flag: {a}. {}", usage_message()));
        }
    }
    Ok(opts)
}

fn usage_message() -> String {
    "Usage: sidekar memory import [--source=<list>] [--project=<name>] \
     [--scope=project|global|all] [--since=<30d>] [--max-sessions=N] \
     [--no-llm] [--credential=<name>] [--model=<id>] [--dry-run] [--yes] [--verbose]"
        .to_string()
}

/// Parse "30d" / "12h" / "6w" / raw seconds. Returns seconds.
fn parse_duration(s: &str) -> Result<u64, String> {
    if s.is_empty() {
        return Err("empty duration".to_string());
    }
    let (num_str, suffix) = s.split_at(s.len() - 1);
    let (num, mult): (u64, u64) = match suffix {
        "s" => (num_str.parse().map_err(|_| format!("bad duration {s}"))?, 1),
        "m" => (num_str.parse().map_err(|_| format!("bad duration {s}"))?, 60),
        "h" => (
            num_str.parse().map_err(|_| format!("bad duration {s}"))?,
            3600,
        ),
        "d" => (
            num_str.parse().map_err(|_| format!("bad duration {s}"))?,
            86_400,
        ),
        "w" => (
            num_str.parse().map_err(|_| format!("bad duration {s}"))?,
            604_800,
        ),
        _ => {
            let n: u64 = s.parse().map_err(|_| format!("bad duration {s}"))?;
            return Ok(n);
        }
    };
    Ok(num.saturating_mul(mult))
}

// ---- Per-source scanners --------------------------------------------------

fn scan_manifests(opts: &ImportOptions, cwd: &Path) -> SourceReport {
    let mut report = SourceReport::new("manifests");
    let project = opts
        .project_override
        .clone()
        .unwrap_or_else(|| crate::scope::resolve_project_name(Some(&cwd.to_string_lossy())));
    let files = sources::detect_manifests(cwd);
    report.files_seen = files.len();
    for f in files {
        match fs::read_to_string(&f.path) {
            Ok(content) => match extract_structured::extract(&f.path, &content, &project) {
                Ok(cands) => report.candidates.extend(cands),
                Err(e) => report
                    .errors
                    .push(format!("{}: {e:#}", f.path.display())),
            },
            Err(e) => report
                .errors
                .push(format!("{}: read failed: {e}", f.path.display())),
        }
    }
    report
}

fn scan_claude(_opts: &ImportOptions, cwd: &Path) -> SourceReport {
    // Prefs only in v1 of the Claude scanner. JSONL transcripts
    // require the LLM extractor; see TODO in next step.
    let mut report = SourceReport::new("claude");
    let prefs = sources::detect_claude_prefs(cwd);
    report.files_seen = prefs.len();
    // Sessions are detected so the report accurately shows what's
    // available, but they're only processable with LLM — add a
    // note instead of silently ignoring them.
    let sessions = sources::detect_claude_sessions();
    if !sessions.is_empty() {
        report.errors.push(format!(
            "{} Claude session transcript(s) detected; LLM extraction not yet wired — rerun after LLM path lands.",
            sessions.len()
        ));
    }
    for f in prefs {
        report.errors.push(format!(
            "{}: LLM extraction not yet wired — rerun after LLM path lands.",
            f.path.display()
        ));
    }
    report
}

fn scan_codex(_opts: &ImportOptions, _cwd: &Path) -> SourceReport {
    let mut report = SourceReport::new("codex");
    let prefs = sources::detect_codex_prefs();
    let sessions = sources::detect_codex_sessions();
    report.files_seen = prefs.len() + sessions.len();
    if !prefs.is_empty() || !sessions.is_empty() {
        report.errors.push(format!(
            "{} pref / {} session file(s) detected; LLM extraction not yet wired.",
            prefs.len(),
            sessions.len()
        ));
    }
    report
}

fn scan_cursor(_opts: &ImportOptions, cwd: &Path) -> SourceReport {
    let mut report = SourceReport::new("cursor");
    let rules = sources::detect_cursor_rules(cwd);
    let sessions = sources::detect_cursor_sessions();
    report.files_seen = rules.len() + sessions.len();
    if !rules.is_empty() || !sessions.is_empty() {
        report.errors.push(format!(
            "{} rule / {} chat DB(s) detected; LLM + store.db extraction not yet wired.",
            rules.len(),
            sessions.len()
        ));
    }
    report
}

fn scan_gemini(_opts: &ImportOptions, _cwd: &Path) -> SourceReport {
    let mut report = SourceReport::new("gemini");
    let prefs = sources::detect_gemini_prefs();
    let sessions = sources::detect_gemini_sessions();
    report.files_seen = prefs.len() + sessions.len();
    if !prefs.is_empty() || !sessions.is_empty() {
        report.errors.push(format!(
            "{} pref / {} session file(s) detected; LLM extraction not yet wired.",
            prefs.len(),
            sessions.len()
        ));
    }
    report
}

fn scan_opencode(_opts: &ImportOptions, _cwd: &Path) -> SourceReport {
    let mut report = SourceReport::new("opencode");
    if let Some(db) = sources::detect_opencode_db() {
        report.files_seen = 1;
        report.errors.push(format!(
            "{}: opencode.db SQLite parser not yet wired.",
            db.path.display()
        ));
    }
    report
}

fn scan_copilot(_opts: &ImportOptions, cwd: &Path) -> SourceReport {
    let mut report = SourceReport::new("copilot");
    let files = sources::detect_copilot(cwd);
    report.files_seen = files.len();
    for f in files {
        report.errors.push(format!(
            "{}: LLM extraction not yet wired.",
            f.path.display()
        ));
    }
    report
}

fn scan_windsurf(_opts: &ImportOptions, cwd: &Path) -> SourceReport {
    let mut report = SourceReport::new("windsurf");
    let files = sources::detect_windsurf(cwd);
    report.files_seen = files.len();
    for f in files {
        report.errors.push(format!(
            "{}: LLM extraction not yet wired.",
            f.path.display()
        ));
    }
    report
}

// ---- UX helpers -----------------------------------------------------------

fn preview_candidates(ctx: &mut AppContext, reports: &[SourceReport], per_source: usize) {
    out!(ctx, "\nPreview (first {per_source} per source):");
    for r in reports {
        if r.candidates.is_empty() {
            continue;
        }
        out!(ctx, "\n[{}]", r.label);
        for c in r.candidates.iter().take(per_source) {
            let preview: String = c.summary.chars().take(140).collect();
            out!(
                ctx,
                "  ({} {} {:.2}) {}",
                c.event_type,
                c.scope,
                c.confidence,
                preview
            );
        }
        if r.candidates.len() > per_source {
            out!(
                ctx,
                "  ... +{} more",
                r.candidates.len() - per_source
            );
        }
    }
}

fn confirm_interactive(count: usize) -> Result<bool> {
    use std::io::{BufRead, Write};
    eprint!("Write {count} memories? [y/N] ");
    std::io::stderr().flush().ok();
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).ok();
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

fn new_batch_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 6 hex chars from a hash of nanos gives a stable sortable tag.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(now.to_be_bytes());
    h.update(nanos.to_be_bytes());
    let digest = h.finalize();
    format!("imp-{now}-{:x}{:x}{:x}", digest[0], digest[1], digest[2])
}

fn file_hash(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn record_import(
    source_kind: &str,
    path: &PathBuf,
    content_hash: &str,
    batch_id: &str,
    stats: &WriteStats,
) -> Result<()> {
    let conn = crate::broker::open_db()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO memory_import_log
            (source_kind, file_path, content_hash, batch_id, events_created, events_deduped, imported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(source_kind, file_path) DO UPDATE SET
            content_hash = excluded.content_hash,
            batch_id = excluded.batch_id,
            events_created = excluded.events_created,
            events_deduped = excluded.events_deduped,
            imported_at = excluded.imported_at",
        params![
            source_kind,
            path.to_string_lossy(),
            content_hash,
            batch_id,
            stats.written as i64,
            stats.deduped as i64,
            now
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_supports_common_suffixes() {
        assert_eq!(parse_duration("30s").unwrap(), 30);
        assert_eq!(parse_duration("5m").unwrap(), 300);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert_eq!(parse_duration("14d").unwrap(), 14 * 86400);
        assert_eq!(parse_duration("2w").unwrap(), 2 * 604800);
        assert_eq!(parse_duration("90").unwrap(), 90);
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn parse_args_defaults_are_sane() {
        let opts = parse_args(&[]).unwrap();
        assert_eq!(opts.scope_filter, ScopeFilter::All);
        assert_eq!(opts.max_sessions, 5);
        assert!(!opts.dry_run);
        assert!(!opts.no_llm);
        assert!(!opts.assume_yes);
    }

    #[test]
    fn parse_args_source_validation() {
        let err = parse_args(&["--source=bogus".to_string()]).unwrap_err();
        assert!(err.contains("Unknown source"));
    }

    #[test]
    fn parse_args_scope_validation() {
        assert!(parse_args(&["--scope=weird".to_string()]).is_err());
        assert_eq!(
            parse_args(&["--scope=project".to_string()])
                .unwrap()
                .scope_filter,
            ScopeFilter::Project
        );
    }

    #[test]
    fn parse_args_handles_multiple_sources() {
        let opts = parse_args(&["--source=manifests,claude".to_string()]).unwrap();
        assert_eq!(opts.sources, vec!["manifests", "claude"]);
    }
}
