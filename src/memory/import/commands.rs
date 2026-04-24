//! CLI entrypoint for `sidekar memory import`. Parses flags,
//! runs the selected source detectors, drives extraction
//! (deterministic + optional LLM), writes the results.

use super::extract_llm;
use super::extract_structured;
use super::parse_sqlite;
use super::parse_transcripts::{self, SessionTranscript};
use super::sources::{self, DetectedFile};
use super::{ImportOptions, ScopeFilter, SourceReport, WriteStats, format_report_table};
use crate::providers::Provider;
use crate::*;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// How many characters to feed the LLM per session transcript.
/// Tail-weighted truncation; the recent half-hour of a session
/// carries more durable signal than the first prompt.
const TRANSCRIPT_MAX_CHARS: usize = 20_000;

pub async fn cmd_memory_import(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_args(args).map_err(|e| anyhow::anyhow!(e))?;
    let cwd = std::env::current_dir()?;
    let allow = sources::resolve_sources(&opts.sources);

    // Only resolve the LLM provider if at least one selected source
    // needs it. Running `--source=manifests` (the deterministic-only
    // path) should not require a credential.
    let needs_llm = !opts.no_llm && allow.iter().any(|s| source_needs_llm(s));
    let provider = if needs_llm {
        Some(resolve_provider(&opts).await.map_err(|e| {
            anyhow::anyhow!(
                "failed to resolve LLM credential — re-run with --no-llm or \
                 --credential=<name>: {e:#}"
            )
        })?)
    } else {
        None
    };
    let model = provider
        .as_ref()
        .map(|(_, m)| m.clone())
        .unwrap_or_default();

    // ---- Scan + extract phase -------------------------------------------
    let mut reports: Vec<SourceReport> = Vec::new();
    for src in &allow {
        let report = match *src {
            "manifests" => scan_manifests(&opts, &cwd),
            "claude" => scan_claude(&opts, &cwd, provider.as_ref(), &model).await,
            "codex" => scan_codex(&opts, provider.as_ref(), &model).await,
            "cursor" => scan_cursor(&opts, &cwd, provider.as_ref(), &model).await,
            "gemini" => scan_gemini(&opts, provider.as_ref(), &model).await,
            "opencode" => scan_opencode(&opts, provider.as_ref(), &model).await,
            "copilot" => scan_copilot(&opts, &cwd, provider.as_ref(), &model).await,
            "windsurf" => scan_windsurf(&opts, &cwd, provider.as_ref(), &model).await,
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
        for c in &r.candidates {
            if let Ok(hash) = file_hash(&c.source_file) {
                let _ = record_import(&c.source_kind, &c.source_file, &hash, &batch_id, &stats);
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

// ---- Provider resolution --------------------------------------------------

/// Pick the LLM credential + model for this invocation. Priority:
/// 1. `--credential=` + `--model=`.
/// 2. Whatever the REPL was last run with (via stored config).
/// 3. Hard-fail — we never guess a cheap model for an unknown
///    provider since that can run up a bill silently.
async fn resolve_provider(opts: &ImportOptions) -> Result<(Arc<Provider>, String)> {
    let cred = opts
        .credential
        .clone()
        .or_else(|| std::env::var("SIDEKAR_CREDENTIAL").ok())
        .or_else(|| {
            let v = crate::config::config_get("credential");
            if v.is_empty() { None } else { Some(v) }
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no credential configured. Pass --credential=<name>, set \
                 SIDEKAR_CREDENTIAL, or run --no-llm to skip LLM extraction."
            )
        })?;

    let provider = crate::repl::slash::build_provider(&cred).await?;

    let model = opts
        .model
        .clone()
        .or_else(|| std::env::var("SIDEKAR_MODEL").ok())
        .or_else(|| {
            let v = crate::config::config_get("model");
            if v.is_empty() { None } else { Some(v) }
        })
        .unwrap_or_else(|| default_model_for_provider(provider.provider_type()));

    Ok((Arc::new(provider), model))
}

/// Safe defaults when the user neither passed `--model=` nor has
/// a configured default. Chosen for low cost on short JSON-mode
/// extraction tasks.
/// Does a given source id use the LLM extractor? If every selected
/// source is purely deterministic (currently only `manifests` +
/// `opencode` once its SQLite parser lands), we can skip the
/// credential check entirely.
fn source_needs_llm(source_id: &str) -> bool {
    !matches!(source_id, "manifests")
}

fn default_model_for_provider(provider_type: &str) -> String {
    match provider_type {
        "anthropic" => "claude-haiku-4-5",
        "codex" => "gpt-4o-mini",
        "openrouter" => "anthropic/claude-haiku-4-5",
        "grok" => "grok-3-mini",
        "gemini" => "gemini-2.0-flash",
        "opencode" => "claude-haiku-4-5",
        _ => "gpt-4o-mini",
    }
    .to_string()
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

fn parse_duration(s: &str) -> Result<u64, String> {
    if s.is_empty() {
        return Err("empty duration".to_string());
    }
    let (num_str, suffix) = s.split_at(s.len() - 1);
    let (num, mult): (u64, u64) = match suffix {
        "s" => (num_str.parse().map_err(|_| format!("bad duration {s}"))?, 1),
        "m" => (num_str.parse().map_err(|_| format!("bad duration {s}"))?, 60),
        "h" => (num_str.parse().map_err(|_| format!("bad duration {s}"))?, 3600),
        "d" => (num_str.parse().map_err(|_| format!("bad duration {s}"))?, 86_400),
        "w" => (num_str.parse().map_err(|_| format!("bad duration {s}"))?, 604_800),
        _ => {
            let n: u64 = s.parse().map_err(|_| format!("bad duration {s}"))?;
            return Ok(n);
        }
    };
    Ok(num.saturating_mul(mult))
}

// ---- Scanners -------------------------------------------------------------

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
                Err(e) => report.errors.push(format!("{}: {e:#}", f.path.display())),
            },
            Err(e) => report
                .errors
                .push(format!("{}: read failed: {e}", f.path.display())),
        }
    }
    report
}

async fn scan_claude(
    opts: &ImportOptions,
    cwd: &Path,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) -> SourceReport {
    let mut report = SourceReport::new("claude");

    // Prefs: ~/.claude/CLAUDE.md (global), <cwd>/CLAUDE.md (project),
    // <cwd>/.claude/CLAUDE.md (project).
    for pref in sources::detect_claude_prefs(cwd) {
        report.files_seen += 1;
        let scope = if pref
            .path
            .to_string_lossy()
            .contains("/.claude/CLAUDE.md")
            && pref.path.to_string_lossy().starts_with(home_str().as_str())
        {
            crate::scope::GLOBAL_SCOPE
        } else {
            crate::scope::PROJECT_SCOPE
        };
        let project = resolve_project_for_path(&pref.path, opts, scope);
        run_text_llm(
            &mut report,
            &pref.path,
            "import:claude:md",
            &project,
            scope,
            provider,
            model,
        )
        .await;
    }

    // Sessions: ~/.claude/projects/<slug>/*.jsonl. Filter by --since
    // + --max-sessions, one LLM call per selected file.
    let sessions = sources::prune_recent(sources::detect_claude_sessions(), opts);
    for f in sessions {
        report.files_seen += 1;
        run_transcript_llm(
            &mut report,
            &f,
            "import:claude:session",
            opts,
            provider,
            model,
            TranscriptKind::Claude,
        )
        .await;
    }

    report
}

async fn scan_codex(
    opts: &ImportOptions,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) -> SourceReport {
    let mut report = SourceReport::new("codex");

    for pref in sources::detect_codex_prefs() {
        report.files_seen += 1;
        // Codex prefs are always global (stored under ~/.codex).
        run_text_llm(
            &mut report,
            &pref.path,
            "import:codex:md",
            "global",
            crate::scope::GLOBAL_SCOPE,
            provider,
            model,
        )
        .await;
    }

    let sessions = sources::prune_recent(sources::detect_codex_sessions(), opts);
    for f in sessions {
        report.files_seen += 1;
        run_transcript_llm(
            &mut report,
            &f,
            "import:codex:session",
            opts,
            provider,
            model,
            TranscriptKind::Codex,
        )
        .await;
    }
    report
}

async fn scan_cursor(
    opts: &ImportOptions,
    cwd: &Path,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) -> SourceReport {
    let mut report = SourceReport::new("cursor");
    // Rules are text — straight LLM path.
    for f in sources::detect_cursor_rules(cwd) {
        report.files_seen += 1;
        let scope = if f
            .path
            .to_string_lossy()
            .starts_with(home_str().as_str())
            && !f.path.starts_with(cwd)
        {
            crate::scope::GLOBAL_SCOPE
        } else {
            crate::scope::PROJECT_SCOPE
        };
        let project = resolve_project_for_path(&f.path, opts, scope);
        run_text_llm(
            &mut report,
            &f.path,
            "import:cursor:rules",
            &project,
            scope,
            provider,
            model,
        )
        .await;
    }

    // store.db sessions — real parser now. Each DB is one chat.
    let sessions = sources::prune_recent(sources::detect_cursor_sessions(), opts);
    for f in sessions {
        report.files_seen += 1;
        let transcript = match parse_sqlite::parse_cursor_store_db(&f.path) {
            Ok(t) => t,
            Err(e) => {
                report
                    .errors
                    .push(format!("{}: {e:#}", f.path.display()));
                continue;
            }
        };
        run_transcript_from(
            &mut report,
            &f.path,
            transcript,
            "import:cursor:session",
            opts,
            provider,
            model,
        )
        .await;
    }
    report
}

async fn scan_gemini(
    opts: &ImportOptions,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) -> SourceReport {
    let mut report = SourceReport::new("gemini");
    for pref in sources::detect_gemini_prefs() {
        report.files_seen += 1;
        run_text_llm(
            &mut report,
            &pref.path,
            "import:gemini:md",
            "global",
            crate::scope::GLOBAL_SCOPE,
            provider,
            model,
        )
        .await;
    }
    let sessions = sources::prune_recent(sources::detect_gemini_sessions(), opts);
    for f in sessions {
        report.files_seen += 1;
        run_transcript_llm(
            &mut report,
            &f,
            "import:gemini:session",
            opts,
            provider,
            model,
            TranscriptKind::Gemini,
        )
        .await;
    }
    report
}

async fn scan_opencode(
    opts: &ImportOptions,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) -> SourceReport {
    let mut report = SourceReport::new("opencode");
    let Some(db) = sources::detect_opencode_db() else {
        return report;
    };
    report.files_seen = 1;
    let transcripts = match parse_sqlite::parse_opencode_db(&db.path) {
        Ok(t) => t,
        Err(e) => {
            report.errors.push(format!("{}: {e:#}", db.path.display()));
            return report;
        }
    };

    // Respect --max-sessions as the number of sessions to import
    // (not the number of DB files — Opencode has one DB for all).
    let mut transcripts = transcripts;
    // Most-recent-first: sort by last turn count as a cheap proxy
    // since we don't carry session mtime. This still honors the
    // cap deterministically.
    transcripts.sort_by_key(|t| std::cmp::Reverse(t.turns.len()));
    if opts.max_sessions > 0 {
        transcripts.truncate(opts.max_sessions);
    }

    for transcript in transcripts {
        let source_path = transcript.source_path.clone();
        run_transcript_from(
            &mut report,
            &source_path,
            transcript,
            "import:opencode:session",
            opts,
            provider,
            model,
        )
        .await;
    }
    report
}

async fn scan_copilot(
    opts: &ImportOptions,
    cwd: &Path,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) -> SourceReport {
    let mut report = SourceReport::new("copilot");
    for f in sources::detect_copilot(cwd) {
        report.files_seen += 1;
        let project = resolve_project_for_path(&f.path, opts, crate::scope::PROJECT_SCOPE);
        run_text_llm(
            &mut report,
            &f.path,
            "import:copilot:md",
            &project,
            crate::scope::PROJECT_SCOPE,
            provider,
            model,
        )
        .await;
    }
    report
}

async fn scan_windsurf(
    opts: &ImportOptions,
    cwd: &Path,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) -> SourceReport {
    let mut report = SourceReport::new("windsurf");
    for f in sources::detect_windsurf(cwd) {
        report.files_seen += 1;
        let project = resolve_project_for_path(&f.path, opts, crate::scope::PROJECT_SCOPE);
        run_text_llm(
            &mut report,
            &f.path,
            "import:windsurf:md",
            &project,
            crate::scope::PROJECT_SCOPE,
            provider,
            model,
        )
        .await;
    }
    report
}

// ---- LLM runners ----------------------------------------------------------

async fn run_text_llm(
    report: &mut SourceReport,
    path: &Path,
    source_kind: &str,
    project: &str,
    scope: &str,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) {
    let (prov, _) = match provider {
        Some(p) => p,
        None => {
            report.errors.push(format!(
                "{}: skipped (--no-llm). Pass --credential=<name> to include.",
                path.display()
            ));
            return;
        }
    };
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            report.errors.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    if content.trim().is_empty() {
        return;
    }
    match extract_llm::extract_from_text(prov, model, source_kind, path, project, scope, &content)
        .await
    {
        Ok(cands) => report.candidates.extend(cands),
        Err(e) => report.errors.push(format!("{}: LLM: {e:#}", path.display())),
    }
}

#[derive(Copy, Clone)]
enum TranscriptKind {
    Claude,
    Codex,
    Gemini,
}

async fn run_transcript_llm(
    report: &mut SourceReport,
    f: &DetectedFile,
    source_kind: &str,
    opts: &ImportOptions,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
    kind: TranscriptKind,
) {
    let transcript = match kind {
        TranscriptKind::Claude => parse_transcripts::parse_claude_jsonl(&f.path),
        TranscriptKind::Codex => parse_transcripts::parse_codex_jsonl(&f.path),
        TranscriptKind::Gemini => parse_transcripts::parse_gemini_json(&f.path),
    };
    let transcript = match transcript {
        Ok(t) => t,
        Err(e) => {
            report
                .errors
                .push(format!("{}: parse: {e:#}", f.path.display()));
            return;
        }
    };
    run_transcript_from(report, &f.path, transcript, source_kind, opts, provider, model).await;
}

/// Shared tail of every transcript-based scanner. Once a
/// `SessionTranscript` is in hand (from JSONL, JSON, or SQLite),
/// resolving project + running the LLM is identical.
async fn run_transcript_from(
    report: &mut SourceReport,
    source_path: &Path,
    transcript: SessionTranscript,
    source_kind: &str,
    opts: &ImportOptions,
    provider: Option<&(Arc<Provider>, String)>,
    model: &str,
) {
    let (prov, _) = match provider {
        Some(p) => p,
        None => {
            report
                .errors
                .push(format!("{}: skipped (--no-llm).", source_path.display()));
            return;
        }
    };
    if transcript.turns.is_empty() {
        return;
    }

    let project = if let Some(cwd) = transcript.cwd.as_ref() {
        opts.project_override
            .clone()
            .unwrap_or_else(|| crate::scope::resolve_project_name(Some(&cwd.to_string_lossy())))
    } else if let Some(over) = opts.project_override.as_deref() {
        over.to_string()
    } else {
        report.errors.push(format!(
            "{}: no cwd recorded in transcript; pass --project=<name> to import anyway.",
            source_path.display()
        ));
        return;
    };

    let content = transcript.concatenated(TRANSCRIPT_MAX_CHARS);
    match extract_llm::extract_from_text(
        prov,
        model,
        source_kind,
        source_path,
        &project,
        crate::scope::PROJECT_SCOPE,
        &content,
    )
    .await
    {
        Ok(cands) => report.candidates.extend(cands),
        Err(e) => report
            .errors
            .push(format!("{}: LLM: {e:#}", source_path.display())),
    }
}

// ---- UX + persistence helpers --------------------------------------------

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
            out!(ctx, "  ... +{} more", r.candidates.len() - per_source);
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

fn home_str() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Pick a project name for a path. If the scope is global, returns
/// "global" (sidekar's canonical global bucket name). Otherwise
/// uses the user's override, or the path's nearest project root.
fn resolve_project_for_path(path: &Path, opts: &ImportOptions, scope: &str) -> String {
    if scope == crate::scope::GLOBAL_SCOPE {
        return "global".to_string();
    }
    if let Some(over) = opts.project_override.as_deref() {
        return over.to_string();
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    crate::scope::resolve_project_name(Some(&dir.to_string_lossy()))
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

    #[test]
    fn default_model_picks_cheap_for_known_providers() {
        assert!(default_model_for_provider("anthropic").contains("haiku"));
        assert!(default_model_for_provider("codex").contains("mini"));
        assert!(default_model_for_provider("gemini").contains("flash"));
    }

    #[test]
    fn resolve_project_for_path_global_shortcircuits() {
        let opts = ImportOptions {
            sources: vec![],
            project_override: Some("override".into()),
            scope_filter: ScopeFilter::All,
            since_secs: None,
            max_sessions: 5,
            no_llm: true,
            credential: None,
            model: None,
            dry_run: true,
            assume_yes: false,
            verbose: false,
        };
        assert_eq!(
            resolve_project_for_path(Path::new("/tmp/x"), &opts, crate::scope::GLOBAL_SCOPE),
            "global"
        );
    }

    #[test]
    fn resolve_project_for_path_honors_override() {
        let opts = ImportOptions {
            sources: vec![],
            project_override: Some("override".into()),
            scope_filter: ScopeFilter::All,
            since_secs: None,
            max_sessions: 5,
            no_llm: true,
            credential: None,
            model: None,
            dry_run: true,
            assume_yes: false,
            verbose: false,
        };
        assert_eq!(
            resolve_project_for_path(Path::new("/tmp/x"), &opts, crate::scope::PROJECT_SCOPE),
            "override"
        );
    }
}
