use super::*;

pub fn cmd_memory(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "write" => cmd_memory_write(ctx, &args[1..]),
        "search" => cmd_memory_search(ctx, &args[1..]),
        "list" => cmd_memory_list(ctx, &args[1..]),
        "delete" => cmd_memory_delete(ctx, &args[1..]),
        "context" => cmd_memory_context(ctx, &args[1..]),
        "compact" => cmd_memory_compact(ctx, &args[1..]),
        "patterns" => cmd_memory_patterns(ctx, &args[1..]),
        "rate" => cmd_memory_rate(ctx, &args[1..]),
        "detail" => cmd_memory_detail(ctx, &args[1..]),
        "" => cmd_memory_list(ctx, args),
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
        out!(ctx, "0 memories matching '{}'.", query);
        return Ok(());
    }

    out!(ctx, "{} memories matching '{}':", results.len(), query);
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

fn cmd_memory_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
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
        .unwrap_or(20);

    let conn = crate::broker::open_db()?;

    let scope_clause = match scope_view {
        crate::scope::ScopeView::Project => "(project = ?1 OR scope = 'global')",
        crate::scope::ScopeView::Global => "scope = 'global'",
        crate::scope::ScopeView::All => "1=1",
    };
    let type_clause = if event_type.is_some() {
        "AND event_type = ?2"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, project, event_type, scope, summary, confidence, tags_json, created_at
         FROM memory_events
         WHERE superseded_by IS NULL AND {scope_clause} {type_clause}
         ORDER BY updated_at DESC
         LIMIT ?3"
    );

    let mut stmt = conn.prepare(&sql)?;
    let project_str = project.as_deref().unwrap_or("");
    let type_str = event_type.as_deref().unwrap_or("");
    let mut rows = stmt.query(params![project_str, type_str, limit as i64])?;

    // Collect all rows first for count header
    struct MemRow {
        id: i64,
        proj: String,
        etype: String,
        scope: String,
        summary: String,
        confidence: f64,
        tags: Vec<String>,
    }
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        items.push(MemRow {
            id: row.get(0)?,
            proj: row.get(1)?,
            etype: row.get(2)?,
            scope: row.get(3)?,
            summary: row.get(4)?,
            confidence: row.get(5)?,
            tags: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
        });
    }

    if items.is_empty() {
        out!(ctx, "0 memories.");
        return Ok(());
    }

    out!(ctx, "{} memories:", items.len());
    for item in &items {
        let scope_label = if item.scope == "global" {
            " [global]"
        } else {
            ""
        };
        let tags_label = if item.tags.is_empty() {
            String::new()
        } else {
            format!(" tags={}", item.tags.join(","))
        };
        out!(
            ctx,
            "[{}] {} ({}, {:.2}, {}){}{}",
            item.id,
            item.summary,
            item.etype,
            item.confidence,
            item.proj,
            scope_label,
            tags_label
        );
    }
    Ok(())
}

fn cmd_memory_delete(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id: i64 = args
        .first()
        .context("Usage: sidekar memory delete <id>")?
        .parse()
        .context("memory id must be numeric")?;

    let conn = crate::broker::open_db()?;

    // Verify it exists and get summary for the confirmation message.
    let summary: String = conn
        .query_row(
            "SELECT summary FROM memory_events WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .context(format!("No memory with id [{}].", id))?;

    conn.execute("DELETE FROM memory_events WHERE id = ?1", [id])?;

    out!(ctx, "Deleted memory [{}]: {}", id, summary);
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
