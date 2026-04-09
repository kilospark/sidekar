use super::*;

pub(super) fn write_memory_event(
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
    }

    Ok(format!("Stored memory [{}].", event_id))
}

pub(super) fn build_context_text(
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

pub(super) fn search_events(
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

pub(super) fn ranked_recent_events(
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

pub(super) fn compact_project(event_type: Option<&str>, project: Option<&str>) -> Result<usize> {
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

pub(super) fn detect_patterns(min_projects: usize) -> Result<usize> {
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

pub(super) fn exact_match_id(
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

pub(super) fn reinforce_events<I>(ids: I) -> Result<()>
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

pub(super) fn event_tags(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    let tags = conn.query_row(
        "SELECT tags_json FROM memory_events WHERE id = ?1",
        [id],
        |row| row.get::<_, String>(0),
    )?;
    Ok(serde_json::from_str(&tags).unwrap_or_default())
}

pub(super) fn find_active_events(
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
