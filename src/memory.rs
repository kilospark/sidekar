use crate::*;
use rusqlite::{OptionalExtension, params};

const MEMORY_TYPES: &[&str] = &[
    "decision",
    "convention",
    "constraint",
    "preference",
    "open-thread",
    "artifact-pointer",
];


const TAG_RULES: &[(&str, &[&str])] = &[
    (
        "testing",
        &[
            "test",
            "spec",
            "jest",
            "vitest",
            "mocha",
            "cypress",
            "playwright",
            "assert",
            "coverage",
        ],
    ),
    (
        "typescript",
        &["typescript", "tsconfig", "interface ", "generic<", "enum "],
    ),
    (
        "database",
        &[
            "sql",
            "postgres",
            "mysql",
            "sqlite",
            "mongodb",
            "redis",
            "migration",
            "schema",
            "orm",
            "drizzle",
            "prisma",
        ],
    ),
    (
        "api",
        &[
            "endpoint",
            "rest",
            "graphql",
            "grpc",
            "fetch(",
            "axios",
            "api route",
            "middleware",
        ],
    ),
    (
        "auth",
        &[
            "auth",
            "login",
            "session",
            "token",
            "jwt",
            "oauth",
            "password",
            "credential",
        ],
    ),
    (
        "deployment",
        &[
            "deploy",
            "docker",
            "kubernetes",
            "k8s",
            "github actions",
            "vercel",
            "aws",
            "gcp",
            "fly.io",
        ],
    ),
    (
        "workflow",
        &[
            "workflow",
            "process",
            "automation",
            "script",
            "hook",
            "lint",
            "format",
        ],
    ),
    (
        "security",
        &[
            "security",
            "xss",
            "injection",
            "sanitize",
            "encrypt",
            "secret",
            "credential",
        ],
    ),
    (
        "browser",
        &[
            "chrome",
            "browser",
            "tab",
            "dom",
            "selector",
            "cookie",
            "service worker",
            "cdp",
        ],
    ),
];

#[derive(Debug, Clone)]
struct MemoryEventRow {
    id: i64,
    project: String,
    event_type: String,
    scope: String,
    summary: String,
    confidence: f64,
    reinforcement_count: i64,
    tags: Vec<String>,
    superseded_by: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Clone)]
struct SearchResultRow {
    row: MemoryEventRow,
    score: f64,
}

pub fn cmd_memory(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "write" => cmd_memory_write(ctx, &args[1..]),
        "search" => cmd_memory_search(ctx, &args[1..]),
        "context" => cmd_memory_context(ctx, &args[1..]),
        "compact" => cmd_memory_compact(ctx, &args[1..]),
        "patterns" => cmd_memory_patterns(ctx, &args[1..]),
        "rate" => cmd_memory_rate(ctx, &args[1..]),
        "detail" => cmd_memory_detail(ctx, &args[1..]),
        "history" => cmd_memory_history(ctx, &args[1..]),
        "" => bail!(
            "Usage: sidekar memory <write|search|context|compact|patterns|rate|detail|history> ..."
        ),
        other => bail!("Unknown memory subcommand: {other}"),
    }
}

/// Compact brief appended to the REPL system prompt. Returns only real user-authored
/// memory events (decisions, constraints, conventions, preferences, open
/// threads, artifact pointers). Returns an empty string when there is no real
/// content — the starter skips the brief entirely in that case.
///
/// Deliberately excludes the title, "Last Session" summary, and session
/// snapshot that `build_context_text` renders. Those sections are populated
/// by self-generated session bookkeeping (every PTY launch records a
/// "session-start" intent event) and produce pure noise when the memory DB
/// has no user content.
pub fn startup_brief(limit: usize) -> Result<String> {
    const EVENT_TYPES: &[&str] = &[
        "constraint",
        "decision",
        "convention",
        "preference",
        "open-thread",
        "artifact-pointer",
    ];

    let project = crate::scope::resolve_project_name(None);
    let conn = crate::broker::open_db()?;
    let ranked = ranked_recent_events(
        &conn,
        crate::scope::ScopeView::Project,
        Some(&project),
        limit * 4,
    )?;
    let deduped = dedupe_rows_by_norm(ranked.into_iter().map(|item| item.row).collect());

    let mut sections: Vec<String> = Vec::new();
    for event_type in EVENT_TYPES {
        let items = deduped
            .iter()
            .filter(|row| row.event_type == *event_type)
            .take(limit)
            .collect::<Vec<_>>();
        if items.is_empty() {
            continue;
        }
        sections.push(format!("## {}", event_type_label(event_type)));
        for item in items {
            let scope = if item.scope == "global" {
                " [global]"
            } else {
                ""
            };
            sections.push(format!("- {}{}", item.summary, scope));
        }
    }

    if sections.is_empty() {
        return Ok(String::new());
    }

    let ids: Vec<i64> = deduped.iter().take(limit * 4).map(|row| row.id).collect();
    reinforce_events(ids)?;

    Ok(sections.join("\n"))
}

fn cmd_memory_write(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!(
            "Usage: sidekar memory write <decision|convention|constraint|preference|open-thread|artifact-pointer> <summary> [--project=P] [--scope=project|global] [--confidence=N] [--tags=a,b]"
        );
    }
    let event_type = normalize_event_type(&args[0])?;
    let summary = args[1].clone();
    let scope = crate::scope::parse_stored_scope(
        &extract_optional_value(args, "--scope=")
            .unwrap_or_else(|| crate::scope::PROJECT_SCOPE.to_string()),
    )?
    .to_string();
    let project = if scope == crate::scope::PROJECT_SCOPE {
        extract_optional_value(args, "--project=")
            .unwrap_or_else(|| crate::scope::resolve_project_name(None))
    } else {
        "global".to_string()
    };
    let confidence = extract_optional_value(args, "--confidence=")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.8);
    let tags = parse_csv_list(extract_optional_value(args, "--tags="));
    let message = write_memory_event(
        &project,
        &event_type,
        &scope,
        &summary,
        confidence,
        &tags,
        "explicit",
        "user",
    )?;
    out!(ctx, "{message}");
    Ok(())
}

fn cmd_memory_search(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let query = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .cloned()
        .context(
            "Usage: sidekar memory search <query> [--scope=project|global|all] [--project=P] [--type=T] [--limit=N]",
        )?;
    let scope_view =
        crate::scope::ScopeView::parse(extract_optional_value(args, "--scope=").as_deref())?;
    let project = if scope_view == crate::scope::ScopeView::Project {
        Some(
            extract_optional_value(args, "--project=")
                .unwrap_or_else(|| crate::scope::resolve_project_name(None)),
        )
    } else {
        extract_optional_value(args, "--project=")
    };
    let event_type = extract_optional_value(args, "--type=")
        .map(|value| normalize_event_type(&value))
        .transpose()?;
    let limit = extract_optional_value(args, "--limit=")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10);

    let results = search_events(
        &query,
        scope_view,
        project.as_deref(),
        event_type.as_deref(),
        limit,
    )?;
    if results.is_empty() {
        out!(ctx, "No memories found.");
        return Ok(());
    }
    reinforce_events(results.iter().map(|item| item.row.id))?;

    for item in results {
        let scope = if item.row.scope == "global" {
            " [global]"
        } else {
            ""
        };
        let tags = if item.row.tags.is_empty() {
            String::new()
        } else {
            format!(" tags={}", item.row.tags.join(","))
        };
        out!(
            ctx,
            "[{}] {} ({}, {:.2}){}{}",
            item.row.id,
            item.row.summary,
            item.row.event_type,
            item.score,
            scope,
            tags
        );
    }
    Ok(())
}

fn cmd_memory_context(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let scope_view =
        crate::scope::ScopeView::parse(extract_optional_value(args, "--scope=").as_deref())?;
    let project = if scope_view == crate::scope::ScopeView::Project {
        Some(
            extract_optional_value(args, "--project=")
                .unwrap_or_else(|| crate::scope::resolve_project_name(None)),
        )
    } else {
        extract_optional_value(args, "--project=")
    };
    let hint = extract_optional_value(args, "--hint=");
    let limit = extract_optional_value(args, "--limit=")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(3);
    let text = build_context_text(scope_view, project.as_deref(), hint.as_deref(), limit)?;
    if !text.is_empty() {
        out!(ctx, "{}", text);
    }
    Ok(())
}

fn cmd_memory_compact(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let project = extract_optional_value(args, "--project=");
    let event_type = extract_optional_value(args, "--type=")
        .map(|value| normalize_event_type(&value))
        .transpose()?;
    out!(
        ctx,
        "Compacted {} memory clusters.",
        compact_project(event_type.as_deref(), project.as_deref())?
    );
    Ok(())
}

fn cmd_memory_patterns(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let min_projects = extract_optional_value(args, "--min-projects=")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2);
    out!(
        ctx,
        "Promoted {} cross-project patterns.",
        detect_patterns(min_projects)?
    );
    Ok(())
}

fn cmd_memory_rate(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar memory rate <id> <helpful|wrong|outdated>");
    }
    let id: i64 = args[0].parse().context("memory id must be numeric")?;
    let rating = args[1].as_str();
    let conn = crate::broker::open_db()?;
    let old_confidence: f64 = conn.query_row(
        "SELECT confidence FROM memory_events WHERE id = ?1",
        [id],
        |row| row.get(0),
    )?;
    let (new_confidence, extra_tag) = match rating {
        "helpful" => ((old_confidence + 0.1).min(1.0), None),
        "wrong" => (0.2, Some("_user_rejected")),
        "outdated" => (0.3, Some("_outdated")),
        _ => bail!("Invalid rating: {rating}. Valid: helpful, wrong, outdated"),
    };
    conn.execute(
        "UPDATE memory_events SET confidence = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, new_confidence, now_epoch_ms()],
    )?;
    if let Some(tag) = extra_tag {
        let tags = event_tags(&conn, id)?;
        let mut merged = tags;
        if !merged.iter().any(|item| item == tag) {
            merged.push(tag.to_string());
        }
        conn.execute(
            "UPDATE memory_events SET tags_json = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&merged)?],
        )?;
    }
    insert_history(
        &conn,
        id,
        &format!("rated_{rating}"),
        None,
        None,
        Some(old_confidence),
        Some(new_confidence),
        serde_json::json!({}),
    )?;
    out!(
        ctx,
        "Rated memory [{}]: {:.2} -> {:.2}",
        id,
        old_confidence,
        new_confidence
    );
    Ok(())
}

fn cmd_memory_detail(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id: i64 = args
        .first()
        .context("Usage: sidekar memory detail <id>")?
        .parse()
        .context("memory id must be numeric")?;
    let conn = crate::broker::open_db()?;
    let row = conn.query_row(
        "SELECT id, summary, event_type, project, scope, confidence, trigger_kind, source_kind,
                tags_json, supersedes_json, superseded_by, reinforcement_count, last_reinforced_at,
                summary_hash, created_at, updated_at
         FROM memory_events WHERE id = ?1",
        [id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, Option<i64>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, i64>(14)?,
                row.get::<_, i64>(15)?,
            ))
        },
    )?;
    out!(ctx, "[{}] {}", row.0, row.1);
    out!(ctx, "type: {}", row.2);
    out!(ctx, "project: {}", row.3);
    out!(ctx, "scope: {}", row.4);
    out!(ctx, "confidence: {:.2}", row.5);
    out!(ctx, "trigger: {}", row.6);
    out!(ctx, "source: {}", row.7);
    out!(ctx, "tags: {}", row.8);
    out!(ctx, "supersedes: {}", row.9);
    out!(
        ctx,
        "superseded_by: {}",
        row.10
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    out!(ctx, "reinforcement_count: {}", row.11);
    out!(
        ctx,
        "last_reinforced_at: {}",
        row.12
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    out!(
        ctx,
        "summary_hash: {}",
        row.13.unwrap_or_else(|| "-".to_string())
    );
    out!(ctx, "created_at: {}", row.14);
    out!(ctx, "updated_at: {}", row.15);
    Ok(())
}

fn cmd_memory_history(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id: i64 = args
        .first()
        .context("Usage: sidekar memory history <id>")?
        .parse()
        .context("memory id must be numeric")?;
    let conn = crate::broker::open_db()?;
    let mut stmt = conn.prepare(
        "SELECT action, old_confidence, new_confidence, metadata_json, created_at
         FROM memory_event_history
         WHERE event_id = ?1
         ORDER BY id ASC",
    )?;
    let mut rows = stmt.query([id])?;
    let mut printed = false;
    while let Some(row) = rows.next()? {
        printed = true;
        let action: String = row.get(0)?;
        let old_confidence: Option<f64> = row.get(1)?;
        let new_confidence: Option<f64> = row.get(2)?;
        let metadata_json: Option<String> = row.get(3)?;
        let created_at: i64 = row.get(4)?;
        out!(
            ctx,
            "{} old={:?} new={:?} at={} {}",
            action,
            old_confidence,
            new_confidence,
            created_at,
            metadata_json.unwrap_or_default()
        );
    }
    if !printed {
        out!(ctx, "No history for memory [{}].", id);
    }
    Ok(())
}

fn write_memory_event(
    project: &str,
    event_type: &str,
    scope: &str,
    summary: &str,
    confidence: f64,
    user_tags: &[String],
    trigger_kind: &str,
    source_kind: &str,
) -> Result<String> {
    let conn = crate::broker::open_db()?;
    let now = now_epoch_ms();
    let summary_norm = normalize_summary(summary);
    let hash = summary_hash(summary);
    let tags_json = serde_json::to_string(&merge_tags(user_tags, &auto_tag(summary)))?;

    if let Some(existing_id) = exact_match_id(&conn, &hash, project, event_type, scope)? {
        let old_confidence: f64 = conn.query_row(
            "SELECT confidence FROM memory_events WHERE id = ?1",
            [existing_id],
            |row| row.get(0),
        )?;
        let new_confidence = (old_confidence + 0.05).min(1.0).max(confidence);
        conn.execute(
            "UPDATE memory_events
             SET confidence = ?2, reinforcement_count = reinforcement_count + 1,
                 last_reinforced_at = ?3, updated_at = ?3
             WHERE id = ?1",
            params![existing_id, new_confidence, now],
        )?;
        insert_history(
            &conn,
            existing_id,
            "deduplicated_hash",
            None,
            None,
            Some(old_confidence),
            Some(new_confidence),
            serde_json::json!({}),
        )?;
        return Ok(format!("Deduplicated existing memory [{}].", existing_id));
    }

    let (search_scope, search_project) = if scope == crate::scope::GLOBAL_SCOPE {
        (crate::scope::ScopeView::Global, None)
    } else {
        (crate::scope::ScopeView::Project, Some(project))
    };
    let near = search_events(summary, search_scope, search_project, Some(event_type), 3)?;
    for candidate in &near {
        if scope == "global" && candidate.row.scope != "global" {
            continue;
        }
        let overlap = word_overlap_ratio(summary, &candidate.row.summary);
        if overlap > 0.90 {
            let old_confidence = candidate.row.confidence;
            let new_confidence = (old_confidence + 0.05).min(1.0).max(confidence);
            conn.execute(
                "UPDATE memory_events
                 SET confidence = ?2, reinforcement_count = reinforcement_count + 1,
                     last_reinforced_at = ?3, updated_at = ?3
                 WHERE id = ?1",
                params![candidate.row.id, new_confidence, now],
            )?;
            insert_history(
                &conn,
                candidate.row.id,
                "deduplicated_fts",
                None,
                None,
                Some(old_confidence),
                Some(new_confidence),
                serde_json::json!({"score":candidate.score}),
            )?;
            return Ok(format!(
                "Deduplicated existing memory [{}].",
                candidate.row.id
            ));
        }
    }

    let mut supersedes = Vec::new();
    let mut superseded_candidate = None;
    if let Some(candidate) = near.iter().find(|item| {
        candidate_scope_match(&item.row, project, scope)
            && item.row.event_type == event_type
            && word_overlap_ratio(summary, &item.row.summary) > 0.72
    }) {
        supersedes.push(candidate.row.id);
        superseded_candidate = Some(candidate.row.clone());
    }

    conn.execute(
        "INSERT INTO memory_events (
            project, event_type, scope, summary, summary_norm, confidence, tags_json,
            supersedes_json, trigger_kind, source_kind, last_reinforced_at,
            reinforcement_count, summary_hash, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12, ?11, ?11)",
        params![
            project,
            event_type,
            scope,
            summary,
            summary_norm,
            confidence,
            tags_json,
            serde_json::to_string(&supersedes)?,
            trigger_kind,
            source_kind,
            now,
            hash
        ],
    )?;
    let event_id = conn.last_insert_rowid();

    if let Some(candidate) = superseded_candidate {
        conn.execute(
            "UPDATE memory_events SET superseded_by = ?2, updated_at = ?3 WHERE id = ?1",
            params![candidate.id, event_id, now],
        )?;
        insert_history(
            &conn,
            candidate.id,
            "superseded",
            Some(&candidate.summary),
            Some(summary),
            Some(candidate.confidence),
            Some(confidence),
            serde_json::json!({"superseded_by":event_id}),
        )?;
    }

    insert_history(
        &conn,
        event_id,
        "created",
        None,
        Some(summary),
        None,
        Some(confidence),
        serde_json::json!({}),
    )?;
    Ok(format!("Stored memory [{}].", event_id))
}

fn build_context_text(
    scope_view: crate::scope::ScopeView,
    project: Option<&str>,
    task_hint: Option<&str>,
    limit: usize,
) -> Result<String> {
    let conn = crate::broker::open_db()?;
    let title = match scope_view {
        crate::scope::ScopeView::Project => format!(
            "# Sidekar Memory: {}",
            project.unwrap_or(crate::scope::PROJECT_SCOPE)
        ),
        crate::scope::ScopeView::Global => "# Sidekar Memory: global".to_string(),
        crate::scope::ScopeView::All => "# Sidekar Memory: all".to_string(),
    };
    let mut sections = vec![title];

    let ranked = ranked_recent_events(&conn, scope_view, project, limit * 4)?;
    let deduped = dedupe_rows_by_norm(ranked.into_iter().map(|item| item.row).collect());
    let ids_to_reinforce: Vec<i64> = deduped.iter().take(limit * 4).map(|row| row.id).collect();
    reinforce_events(ids_to_reinforce)?;

    for event_type in [
        "constraint",
        "decision",
        "convention",
        "preference",
        "open-thread",
        "artifact-pointer",
    ] {
        let items = deduped
            .iter()
            .filter(|row| row.event_type == event_type)
            .take(limit)
            .collect::<Vec<_>>();
        if items.is_empty() {
            continue;
        }
        sections.push(format!("\n## {}", event_type_label(event_type)));
        for item in items {
            let scope = if item.scope == "global" {
                " [global]"
            } else {
                ""
            };
            sections.push(format!("- {}{}", item.summary, scope));
        }
    }

    if let Some(hint) = task_hint {
        let relevant = search_events(hint, scope_view, project, None, 5)?;
        if !relevant.is_empty() {
            sections.push("\n## Relevant To Current Task".to_string());
            for item in relevant.iter().take(5) {
                sections.push(format!("- [{:.2}] {}", item.score, item.row.summary));
            }
        }
    }

    // Only the title was pushed — no real content. Return empty so callers
    // can stay silent rather than emit a filler header.
    if sections.len() == 1 {
        return Ok(String::new());
    }

    Ok(sections.join("\n"))
}

fn search_events(
    query: &str,
    scope_view: crate::scope::ScopeView,
    project: Option<&str>,
    event_type: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchResultRow>> {
    let conn = crate::broker::open_db()?;
    let cleaned = sanitize_fts_query(query);
    if cleaned.is_empty() {
        return Ok(Vec::new());
    }

    let sql = match (scope_view, event_type) {
        (crate::scope::ScopeView::Project, Some(_)) => {
            "SELECT e.id, e.project, e.event_type, e.scope, e.summary, e.confidence,
                    e.reinforcement_count, e.tags_json, e.supersedes_json, e.superseded_by,
                    e.created_at, e.updated_at, bm25(memory_events_fts) AS rank
             FROM memory_events_fts
             JOIN memory_events e ON memory_events_fts.rowid = e.id
             WHERE memory_events_fts MATCH ?1
               AND (e.project = ?2 OR e.scope = 'global')
               AND e.event_type = ?3
               AND e.superseded_by IS NULL
             ORDER BY rank
             LIMIT ?4"
        }
        (crate::scope::ScopeView::Project, None) => {
            "SELECT e.id, e.project, e.event_type, e.scope, e.summary, e.confidence,
                    e.reinforcement_count, e.tags_json, e.supersedes_json, e.superseded_by,
                    e.created_at, e.updated_at, bm25(memory_events_fts) AS rank
             FROM memory_events_fts
             JOIN memory_events e ON memory_events_fts.rowid = e.id
             WHERE memory_events_fts MATCH ?1
               AND (e.project = ?2 OR e.scope = 'global')
               AND e.superseded_by IS NULL
             ORDER BY rank
             LIMIT ?3"
        }
        (crate::scope::ScopeView::Global, Some(_)) => {
            "SELECT e.id, e.project, e.event_type, e.scope, e.summary, e.confidence,
                    e.reinforcement_count, e.tags_json, e.supersedes_json, e.superseded_by,
                    e.created_at, e.updated_at, bm25(memory_events_fts) AS rank
             FROM memory_events_fts
             JOIN memory_events e ON memory_events_fts.rowid = e.id
             WHERE memory_events_fts MATCH ?1
               AND e.scope = 'global'
               AND e.event_type = ?2
               AND e.superseded_by IS NULL
             ORDER BY rank
             LIMIT ?3"
        }
        (crate::scope::ScopeView::Global, None) => {
            "SELECT e.id, e.project, e.event_type, e.scope, e.summary, e.confidence,
                    e.reinforcement_count, e.tags_json, e.supersedes_json, e.superseded_by,
                    e.created_at, e.updated_at, bm25(memory_events_fts) AS rank
             FROM memory_events_fts
             JOIN memory_events e ON memory_events_fts.rowid = e.id
             WHERE memory_events_fts MATCH ?1
               AND e.scope = 'global'
               AND e.superseded_by IS NULL
             ORDER BY rank
             LIMIT ?2"
        }
        (crate::scope::ScopeView::All, Some(_)) => {
            "SELECT e.id, e.project, e.event_type, e.scope, e.summary, e.confidence,
                    e.reinforcement_count, e.tags_json, e.supersedes_json, e.superseded_by,
                    e.created_at, e.updated_at, bm25(memory_events_fts) AS rank
             FROM memory_events_fts
             JOIN memory_events e ON memory_events_fts.rowid = e.id
             WHERE memory_events_fts MATCH ?1
               AND e.event_type = ?2
               AND e.superseded_by IS NULL
             ORDER BY rank
             LIMIT ?3"
        }
        (crate::scope::ScopeView::All, None) => {
            "SELECT e.id, e.project, e.event_type, e.scope, e.summary, e.confidence,
                    e.reinforcement_count, e.tags_json, e.supersedes_json, e.superseded_by,
                    e.created_at, e.updated_at, bm25(memory_events_fts) AS rank
             FROM memory_events_fts
             JOIN memory_events e ON memory_events_fts.rowid = e.id
             WHERE memory_events_fts MATCH ?1
               AND e.superseded_by IS NULL
             ORDER BY rank
             LIMIT ?2"
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let mut rows = match (scope_view, project, event_type) {
        (crate::scope::ScopeView::Project, Some(project), Some(event_type)) => {
            stmt.query(params![cleaned, project, event_type, limit as i64])?
        }
        (crate::scope::ScopeView::Project, Some(project), None) => {
            stmt.query(params![cleaned, project, limit as i64])?
        }
        (crate::scope::ScopeView::Global, _, Some(event_type))
        | (crate::scope::ScopeView::All, _, Some(event_type)) => {
            stmt.query(params![cleaned, event_type, limit as i64])?
        }
        (crate::scope::ScopeView::Global, _, None) | (crate::scope::ScopeView::All, _, None) => {
            stmt.query(params![cleaned, limit as i64])?
        }
        (crate::scope::ScopeView::Project, None, _) => {
            bail!("project scope queries require a project context")
        }
    };

    let mut results = Vec::new();
    while let Some(row) = rows.next()? {
        let event = MemoryEventRow {
            id: row.get(0)?,
            project: row.get(1)?,
            event_type: row.get(2)?,
            scope: row.get(3)?,
            summary: row.get(4)?,
            confidence: row.get(5)?,
            reinforcement_count: row.get(6)?,
            tags: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
            superseded_by: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        };
        let bm25_rank: f64 = row.get(12)?;
        results.push(SearchResultRow {
            score: score_search_result(&event, bm25_rank),
            row: event,
        });
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(results)
}

fn ranked_recent_events(
    conn: &rusqlite::Connection,
    scope_view: crate::scope::ScopeView,
    project: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchResultRow>> {
    let sql = match scope_view {
        crate::scope::ScopeView::Project => {
            "SELECT id, project, event_type, scope, summary, confidence, reinforcement_count,
                    tags_json, supersedes_json, superseded_by, created_at, updated_at
             FROM memory_events
             WHERE (project = ?1 OR scope = 'global') AND superseded_by IS NULL
             ORDER BY confidence DESC, reinforcement_count DESC, created_at DESC
             LIMIT ?2"
        }
        crate::scope::ScopeView::Global => {
            "SELECT id, project, event_type, scope, summary, confidence, reinforcement_count,
                    tags_json, supersedes_json, superseded_by, created_at, updated_at
             FROM memory_events
             WHERE scope = 'global' AND superseded_by IS NULL
             ORDER BY confidence DESC, reinforcement_count DESC, created_at DESC
             LIMIT ?1"
        }
        crate::scope::ScopeView::All => {
            "SELECT id, project, event_type, scope, summary, confidence, reinforcement_count,
                    tags_json, supersedes_json, superseded_by, created_at, updated_at
             FROM memory_events
             WHERE superseded_by IS NULL
             ORDER BY confidence DESC, reinforcement_count DESC, created_at DESC
             LIMIT ?1"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let mut rows = match scope_view {
        crate::scope::ScopeView::Project => stmt.query(params![project, limit as i64])?,
        crate::scope::ScopeView::Global | crate::scope::ScopeView::All => {
            stmt.query(params![limit as i64])?
        }
    };
    let mut results = Vec::new();
    while let Some(row) = rows.next()? {
        let event = MemoryEventRow {
            id: row.get(0)?,
            project: row.get(1)?,
            event_type: row.get(2)?,
            scope: row.get(3)?,
            summary: row.get(4)?,
            confidence: row.get(5)?,
            reinforcement_count: row.get(6)?,
            tags: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
            superseded_by: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        };
        results.push(SearchResultRow {
            score: recency_score(&event),
            row: event,
        });
    }
    Ok(results)
}

fn compact_project(event_type: Option<&str>, project: Option<&str>) -> Result<usize> {
    let conn = crate::broker::open_db()?;
    let project = project
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| crate::scope::resolve_project_name(None));
    let rows = find_active_events(&conn, Some(&project), event_type)?;
    let mut compacted = 0usize;

    for kind in event_type
        .map(|value| vec![value.to_string()])
        .unwrap_or_else(|| MEMORY_TYPES.iter().map(|value| value.to_string()).collect())
    {
        let items = rows
            .iter()
            .filter(|row| row.event_type == kind)
            .cloned()
            .collect::<Vec<_>>();
        for cluster in cluster_by_similarity(&items, 0.74) {
            if cluster.len() < 3 {
                continue;
            }
            let summary = format!(
                "Compacted {} pattern: {}",
                event_type_label(&kind).to_lowercase(),
                cluster
                    .iter()
                    .take(3)
                    .map(|row| row.summary.clone())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            let message = write_memory_event(
                &project,
                &kind,
                "project",
                &summary,
                0.78,
                &["compacted".to_string()],
                "passive",
                "system",
            )?;
            if message.starts_with("Stored memory [") {
                compacted += 1;
            }
        }
    }
    Ok(compacted)
}

fn detect_patterns(min_projects: usize) -> Result<usize> {
    let conn = crate::broker::open_db()?;
    let rows = find_active_events(&conn, None, None)?;
    let mut by_norm: HashMap<(String, String), Vec<MemoryEventRow>> = HashMap::new();
    for row in rows.into_iter().filter(|row| row.scope == "project") {
        by_norm
            .entry((row.event_type.clone(), normalize_summary(&row.summary)))
            .or_default()
            .push(row);
    }

    let mut promoted = 0usize;
    for ((event_type, _), rows) in by_norm {
        let distinct_projects: HashSet<String> =
            rows.iter().map(|row| row.project.clone()).collect();
        if distinct_projects.len() < min_projects {
            continue;
        }
        let exemplar = rows
            .iter()
            .max_by(|a, b| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .unwrap_or_else(|| rows[0].clone());
        let message = write_memory_event(
            &exemplar.project,
            &event_type,
            "global",
            &exemplar.summary,
            exemplar.confidence.max(0.82),
            &["pattern".to_string()],
            "passive",
            "system",
        )?;
        if message.starts_with("Stored memory [") {
            promoted += 1;
        }
    }
    Ok(promoted)
}

fn find_active_events(
    conn: &rusqlite::Connection,
    project: Option<&str>,
    event_type: Option<&str>,
) -> Result<Vec<MemoryEventRow>> {
    let sql = match (project, event_type) {
        (Some(_), Some(_)) => {
            "SELECT id, project, event_type, scope, summary, confidence, reinforcement_count,
                    tags_json, supersedes_json, superseded_by, created_at, updated_at
             FROM memory_events
             WHERE (project = ?1 OR scope = 'global') AND event_type = ?2 AND superseded_by IS NULL
             ORDER BY created_at DESC"
        }
        (Some(_), None) => {
            "SELECT id, project, event_type, scope, summary, confidence, reinforcement_count,
                    tags_json, supersedes_json, superseded_by, created_at, updated_at
             FROM memory_events
             WHERE (project = ?1 OR scope = 'global') AND superseded_by IS NULL
             ORDER BY created_at DESC"
        }
        (None, Some(_)) => {
            "SELECT id, project, event_type, scope, summary, confidence, reinforcement_count,
                    tags_json, supersedes_json, superseded_by, created_at, updated_at
             FROM memory_events
             WHERE event_type = ?1 AND superseded_by IS NULL
             ORDER BY created_at DESC"
        }
        (None, None) => {
            "SELECT id, project, event_type, scope, summary, confidence, reinforcement_count,
                    tags_json, supersedes_json, superseded_by, created_at, updated_at
             FROM memory_events
             WHERE superseded_by IS NULL
             ORDER BY created_at DESC"
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let mut rows = match (project, event_type) {
        (Some(project), Some(event_type)) => stmt.query(params![project, event_type])?,
        (Some(project), None) => stmt.query(params![project])?,
        (None, Some(event_type)) => stmt.query(params![event_type])?,
        (None, None) => stmt.query([])?,
    };

    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        result.push(MemoryEventRow {
            id: row.get(0)?,
            project: row.get(1)?,
            event_type: row.get(2)?,
            scope: row.get(3)?,
            summary: row.get(4)?,
            confidence: row.get(5)?,
            reinforcement_count: row.get(6)?,
            tags: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
            superseded_by: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        });
    }
    Ok(result)
}

fn exact_match_id(
    conn: &rusqlite::Connection,
    hash: &str,
    project: &str,
    event_type: &str,
    scope: &str,
) -> Result<Option<i64>> {
    let sql = if scope == "global" {
        "SELECT id
         FROM memory_events
         WHERE summary_hash = ?1 AND event_type = ?2 AND scope = 'global' AND superseded_by IS NULL
         LIMIT 1"
    } else {
        "SELECT id
         FROM memory_events
         WHERE summary_hash = ?1 AND event_type = ?2 AND superseded_by IS NULL
           AND (project = ?3 OR scope = 'global')
         LIMIT 1"
    };
    let params: &[&dyn rusqlite::ToSql] = if scope == "global" {
        &[&hash, &event_type]
    } else {
        &[&hash, &event_type, &project]
    };
    conn.query_row(sql, params, |row| row.get::<_, i64>(0))
        .optional()
        .map_err(Into::into)
}

fn reinforce_events<I>(ids: I) -> Result<()>
where
    I: IntoIterator<Item = i64>,
{
    let conn = crate::broker::open_db()?;
    let now = now_epoch_ms();
    for id in ids {
        conn.execute(
            "UPDATE memory_events
             SET reinforcement_count = reinforcement_count + 1, last_reinforced_at = ?2, updated_at = ?2
             WHERE id = ?1",
            params![id, now],
        )?;
    }
    Ok(())
}

fn insert_history(
    conn: &rusqlite::Connection,
    event_id: i64,
    action: &str,
    old_summary: Option<&str>,
    new_summary: Option<&str>,
    old_confidence: Option<f64>,
    new_confidence: Option<f64>,
    metadata: serde_json::Value,
) -> Result<()> {
    conn.execute(
        "INSERT INTO memory_event_history (
            event_id, action, old_summary, new_summary, old_confidence,
            new_confidence, metadata_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            event_id,
            action,
            old_summary,
            new_summary,
            old_confidence,
            new_confidence,
            serde_json::to_string(&metadata)?,
            now_epoch_ms()
        ],
    )?;
    Ok(())
}

fn event_tags(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    let tags = conn.query_row(
        "SELECT tags_json FROM memory_events WHERE id = ?1",
        [id],
        |row| row.get::<_, String>(0),
    )?;
    Ok(serde_json::from_str(&tags).unwrap_or_default())
}

fn extract_optional_value(args: &[String], prefix: &str) -> Option<String> {
    args.iter()
        .find_map(|arg| arg.strip_prefix(prefix).map(ToOwned::to_owned))
}

fn normalize_event_type(value: &str) -> Result<String> {
    let normalized = value.trim().replace('_', "-");
    if MEMORY_TYPES.iter().any(|item| *item == normalized) {
        Ok(normalized)
    } else {
        bail!(
            "Invalid memory type: {}. Valid: {}",
            value,
            MEMORY_TYPES.join(", ")
        )
    }
}

fn parse_csv_list(value: Option<String>) -> Vec<String> {
    value
        .map(|raw| {
            raw.split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn merge_tags(user_tags: &[String], auto_tags: &[String]) -> Vec<String> {
    let mut merged = Vec::new();
    for tag in user_tags.iter().chain(auto_tags.iter()) {
        if merged.len() >= 5 {
            break;
        }
        if !merged.iter().any(|existing| existing == tag) {
            merged.push(tag.clone());
        }
    }
    merged
}

fn auto_tag(summary: &str) -> Vec<String> {
    let lower = summary.to_lowercase();
    let mut tags = Vec::new();
    for &(tag, keywords) in TAG_RULES {
        if tags.len() >= 5 {
            break;
        }
        if keywords.iter().any(|kw| lower.contains(kw)) {
            tags.push(tag.to_string());
        }
    }
    tags
}

fn sanitize_fts_query(query: &str) -> String {
    normalize_summary(query)
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_summary(summary: &str) -> String {
    summary
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn summary_hash(summary: &str) -> String {
    let normalized = normalize_summary(summary);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in normalized.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn word_overlap_ratio(a: &str, b: &str) -> f64 {
    let a_words = significant_words(a);
    let b_words = significant_words(b);
    let min_size = a_words.len().min(b_words.len());
    if min_size == 0 {
        return 0.0;
    }
    let shared = a_words
        .iter()
        .filter(|word| b_words.contains(*word))
        .count();
    shared as f64 / min_size as f64
}

fn significant_words(text: &str) -> HashSet<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can",
        "to", "of", "in", "for", "on", "with", "at", "by", "from", "as", "into", "through",
        "during", "before", "after", "and", "but", "or", "nor", "not", "so", "yet", "both",
        "either", "neither", "each", "every", "all", "any", "few", "more", "most", "other", "some",
        "such", "no", "only", "own", "same", "than", "too", "very", "just", "because", "if",
        "when", "where", "how", "what", "which", "who", "whom", "this", "that", "these", "those",
        "it", "its", "use", "using", "used", "sidekar",
    ];
    let stop_words: HashSet<&str> = STOP_WORDS.iter().copied().collect();
    text.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|word| word.len() > 2 && !stop_words.contains(*word))
        .map(ToOwned::to_owned)
        .collect()
}

fn score_search_result(row: &MemoryEventRow, bm25_rank: f64) -> f64 {
    let fts_score = 1.0 / (1.0 + bm25_rank.abs());
    fts_score + recency_score(row)
}

fn recency_score(row: &MemoryEventRow) -> f64 {
    let age_days = (now_epoch_ms() - row.created_at).max(0) as f64 / 86_400_000.0;
    let recency_boost = if age_days < 7.0 {
        1.2
    } else if age_days < 30.0 {
        1.1
    } else {
        1.0
    };
    let stale_multiplier = match row.event_type.as_str() {
        "open-thread" if age_days > 14.0 => 0.6,
        "artifact-pointer" if age_days > 30.0 => 0.7,
        "decision" | "preference" if age_days > 180.0 => 0.9,
        _ => 1.0,
    };
    (0.5 * recency_boost * stale_multiplier)
        + (row.confidence * 0.3)
        + ((row.reinforcement_count as f64).min(5.0) * 0.03)
        + type_priority(&row.event_type)
}

fn type_priority(event_type: &str) -> f64 {
    match event_type {
        "constraint" => 0.15,
        "decision" => 0.12,
        "convention" => 0.10,
        "preference" => 0.08,
        "open-thread" => 0.05,
        "artifact-pointer" => 0.03,
        _ => 0.0,
    }
}

fn event_type_label(event_type: &str) -> &'static str {
    match event_type {
        "decision" => "Decisions",
        "convention" => "Conventions",
        "constraint" => "Constraints",
        "preference" => "Preferences",
        "open-thread" => "Open Threads",
        "artifact-pointer" => "Artifacts",
        _ => "Memories",
    }
}

fn cluster_by_similarity(rows: &[MemoryEventRow], threshold: f64) -> Vec<Vec<MemoryEventRow>> {
    let mut clusters = Vec::new();
    let mut used = HashSet::new();
    for row in rows {
        if used.contains(&row.id) {
            continue;
        }
        let mut cluster = vec![row.clone()];
        used.insert(row.id);
        for other in rows {
            if used.contains(&other.id) {
                continue;
            }
            if word_overlap_ratio(&row.summary, &other.summary) >= threshold {
                cluster.push(other.clone());
                used.insert(other.id);
            }
        }
        clusters.push(cluster);
    }
    clusters
}

fn dedupe_rows_by_norm(rows: Vec<MemoryEventRow>) -> Vec<MemoryEventRow> {
    let mut best: HashMap<(String, String), MemoryEventRow> = HashMap::new();
    for row in rows.into_iter().filter(|row| row.superseded_by.is_none()) {
        let key = (row.event_type.clone(), normalize_summary(&row.summary));
        match best.get(&key) {
            Some(existing) if existing.scope == "project" && row.scope == "global" => {}
            Some(existing) if row.scope == "project" && existing.scope == "global" => {
                best.insert(key, row);
            }
            Some(existing) if existing.updated_at >= row.updated_at => {}
            _ => {
                best.insert(key, row);
            }
        }
    }
    best.into_values().collect()
}

fn candidate_scope_match(row: &MemoryEventRow, project: &str, scope: &str) -> bool {
    if scope == "global" {
        row.scope == "global"
    } else {
        row.project == project || row.scope == "global"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;

        let old_home = env::var_os("HOME");
        let temp_home = env::temp_dir().join(format!("sidekar-memory-test-{}", now_epoch_ms()));
        fs::create_dir_all(&temp_home)?;

        // Safety: tests run under a process-global mutex and restore HOME before returning.
        unsafe { env::set_var("HOME", &temp_home) };

        let result = f();

        match old_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&temp_home);
        result
    }

    #[test]
    fn search_normalizes_punctuation_for_fts() -> Result<()> {
        with_test_home(|| {
            write_memory_event(
                "alpha",
                "convention",
                "project",
                "Use Readability.js before scraping article text",
                0.8,
                &[],
                "explicit",
                "user",
            )?;

            let results = search_events(
                "Readability.js",
                crate::scope::ScopeView::Project,
                Some("alpha"),
                None,
                5,
            )?;
            assert_eq!(results.len(), 1);
            assert_eq!(
                results[0].row.summary,
                "Use Readability.js before scraping article text"
            );
            Ok(())
        })
    }

    #[test]
    fn detect_patterns_promotes_global_memory() -> Result<()> {
        with_test_home(|| {
            write_memory_event(
                "alpha",
                "convention",
                "project",
                "Use Readability.js before scraping article text",
                0.8,
                &[],
                "explicit",
                "user",
            )?;
            write_memory_event(
                "beta",
                "convention",
                "project",
                "Use Readability.js before scraping article text",
                0.8,
                &[],
                "explicit",
                "user",
            )?;

            assert_eq!(detect_patterns(2)?, 1);

            let conn = crate::broker::open_db()?;
            let global_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_events
                 WHERE scope = 'global' AND event_type = 'convention'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(global_count, 1);

            let global_summary: String = conn.query_row(
                "SELECT summary FROM memory_events
                 WHERE scope = 'global' AND event_type = 'convention'
                 LIMIT 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(
                global_summary,
                "Use Readability.js before scraping article text"
            );
            Ok(())
        })
    }

}
