use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use super::*;
use crate::repl::journal::parse::StructuredJournal;

const DEFAULT_LIST_LIMIT: usize = 20;

#[derive(Debug, Clone)]
struct CandidateInput {
    event_type: String,
    scope: String,
    summary: String,
    confidence: f64,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MemoryCandidateRow {
    id: i64,
    project: String,
    session_id: String,
    journal_id: i64,
    event_type: String,
    scope: String,
    summary: String,
    summary_norm: String,
    confidence: f64,
    status: String,
    source_kind: String,
    trigger_kind: String,
    related_memory_id: Option<i64>,
    support_count: i64,
    created_at: f64,
    updated_at: f64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CandidateProcessOutcome {
    pub extracted: usize,
    pub inserted: usize,
    pub updated: usize,
    pub auto_promoted: usize,
    pub reinforced: usize,
    pub resolved: usize,
    pub contradicted: usize,
    pub memory_ids: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CandidateMemoryAction {
    memory_id: i64,
    contradicted: usize,
}

#[derive(serde::Serialize)]
struct MemoryCandidateItem {
    id: i64,
    summary: String,
    event_type: String,
    status: String,
    support_count: i64,
    confidence: f64,
    project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    related_memory_id: Option<i64>,
}

#[derive(serde::Serialize)]
struct MemoryCandidateListOutput {
    items: Vec<MemoryCandidateItem>,
}

impl crate::output::CommandOutput for MemoryCandidateListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "0 candidate memories.")?;
            return Ok(());
        }
        writeln!(w, "{} candidate memories:", self.items.len())?;
        for item in &self.items {
            let memory = item
                .related_memory_id
                .map(|id| format!(" memory={id}"))
                .unwrap_or_default();
            writeln!(
                w,
                "[{}] {} ({}, {}, support={}, {:.2}, {}){}",
                item.id,
                item.summary,
                item.event_type,
                item.status,
                item.support_count,
                item.confidence,
                item.project,
                memory
            )?;
        }
        Ok(())
    }
}

pub(super) fn cmd_memory_candidates(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "" | "list" => {
            let rest = if matches!(args.first().map(String::as_str), Some("list")) {
                &args[1..]
            } else {
                args
            };
            cmd_memory_candidates_list(ctx, rest)
        }
        "promote" => cmd_memory_candidates_promote(ctx, &args[1..]),
        "reject" => cmd_memory_candidates_reject(ctx, &args[1..]),
        other => bail!("Unknown memory candidates subcommand: {other}"),
    }
}

fn cmd_memory_candidates_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let project = extract_optional_value(args, "--project=")
        .unwrap_or_else(|| crate::scope::resolve_project_name(None));
    let status = extract_optional_value(args, "--status=").unwrap_or_else(|| "all".to_string());
    let event_type = extract_optional_value(args, "--type=")
        .map(|value| normalize_event_type(&value))
        .transpose()?;
    let limit = extract_optional_value(args, "--limit=")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LIST_LIMIT);

    let items = list_candidates(&project, status.as_str(), event_type.as_deref(), limit)?;
    out!(
        ctx,
        "{}",
        crate::output::to_string(&MemoryCandidateListOutput { items })?
    );
    Ok(())
}

fn cmd_memory_candidates_promote(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id: i64 = args
        .first()
        .context("Usage: sidekar memory candidates promote <id>")?
        .parse()
        .context("candidate id must be numeric")?;
    let candidate = load_candidate(id)?.context(format!("No memory candidate [{}].", id))?;
    let action = promote_candidate(&candidate, None, None, None)?
        .context(format!("Unable to promote memory candidate [{}].", id))?;
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(format!(
            "Promoted candidate [{}] -> memory [{}].",
            id, action.memory_id
        )))?
    );
    Ok(())
}

fn cmd_memory_candidates_reject(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id: i64 = args
        .first()
        .context("Usage: sidekar memory candidates reject <id>")?
        .parse()
        .context("candidate id must be numeric")?;
    let candidate = load_candidate(id)?.context(format!("No memory candidate [{}].", id))?;
    if candidate.status == "promoted" {
        bail!(
            "Candidate [{}] already promoted to memory [{}]. Rate/delete memory separately.",
            id,
            candidate
                .related_memory_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "?".to_string())
        );
    }
    let conn = crate::broker::open_db()?;
    conn.execute(
        "UPDATE memory_candidates SET status = 'rejected', updated_at = ?2 WHERE id = ?1",
        params![id, now_epoch_ms()],
    )?;
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(format!(
            "Rejected candidate [{}].",
            id
        )))?
    );
    Ok(())
}

pub(crate) fn process_journal_candidates(
    project: &str,
    session_id: &str,
    journal_id: i64,
    journal: &StructuredJournal,
) -> Result<CandidateProcessOutcome> {
    let conn = crate::broker::open_db()?;
    let mut outcome = CandidateProcessOutcome::default();
    let accepted_detail = success_detail_json(journal);
    outcome.resolved = apply_resolution_signals(&conn, project, session_id, journal_id, journal)?;
    for candidate in extract_candidates(journal) {
        outcome.extracted += 1;
        let (row, inserted) = upsert_candidate(&conn, project, session_id, journal_id, &candidate)?;
        if inserted {
            outcome.inserted += 1;
        } else {
            outcome.updated += 1;
        }

        if row.status == "promoted" {
            if let Some(action) = reinforce_promoted_candidate(
                &row,
                Some(session_id),
                Some(journal_id),
                accepted_detail.as_deref(),
            )? {
                outcome.reinforced += 1;
                outcome.contradicted += action.contradicted;
                if !outcome.memory_ids.contains(&action.memory_id) {
                    outcome.memory_ids.push(action.memory_id);
                }
            }
            continue;
        }
        if row.status == "rejected" || row.status == "superseded" {
            continue;
        }
        if row.support_count >= auto_promote_threshold(&row.event_type) {
            if let Some(action) = promote_candidate(
                &row,
                Some(session_id),
                Some(journal_id),
                accepted_detail.as_deref(),
            )? {
                outcome.auto_promoted += 1;
                outcome.contradicted += action.contradicted;
                if !outcome.memory_ids.contains(&action.memory_id) {
                    outcome.memory_ids.push(action.memory_id);
                }
            }
        }
    }
    Ok(outcome)
}

fn extract_candidates(journal: &StructuredJournal) -> Vec<CandidateInput> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for value in &journal.constraints {
        push_candidate(
            &mut out,
            &mut seen,
            "constraint",
            value,
            candidate_confidence("constraint"),
        );
    }
    for value in &journal.decisions {
        push_candidate(
            &mut out,
            &mut seen,
            "decision",
            value,
            candidate_confidence("decision"),
        );
    }
    for value in journal
        .blocked
        .iter()
        .chain(journal.pending_user_asks.iter())
    {
        push_candidate(
            &mut out,
            &mut seen,
            "open-thread",
            value,
            candidate_confidence("open-thread"),
        );
    }
    for path in &journal.relevant_files {
        let summary = format!("Relevant file: {}", path.trim());
        push_candidate(
            &mut out,
            &mut seen,
            "artifact-pointer",
            &summary,
            candidate_confidence("artifact-pointer"),
        );
    }

    out
}

fn push_candidate(
    out: &mut Vec<CandidateInput>,
    seen: &mut HashSet<(String, String)>,
    event_type: &str,
    raw_summary: &str,
    confidence: f64,
) {
    let Some(summary) = normalize_candidate_summary(raw_summary) else {
        return;
    };
    let summary_norm = normalize_summary(&summary);
    if summary_norm.len() < 4 {
        return;
    }
    let key = (event_type.to_string(), summary_norm);
    if !seen.insert(key) {
        return;
    }
    out.push(CandidateInput {
        event_type: event_type.to_string(),
        scope: crate::scope::PROJECT_SCOPE.to_string(),
        summary,
        confidence,
    });
}

fn normalize_candidate_summary(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "[blocked]" {
        return None;
    }
    let summary = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}

fn candidate_confidence(event_type: &str) -> f64 {
    match event_type {
        "constraint" => 0.70,
        "decision" => 0.66,
        "open-thread" => 0.58,
        "artifact-pointer" => 0.52,
        _ => 0.50,
    }
}

fn auto_promote_threshold(event_type: &str) -> i64 {
    match event_type {
        "constraint" | "decision" | "open-thread" => 2,
        "artifact-pointer" => 2,
        _ => 3,
    }
}

fn list_candidates(
    project: &str,
    status: &str,
    event_type: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryCandidateItem>> {
    let conn = crate::broker::open_db()?;
    match (status, event_type) {
        ("all", Some(event_type)) => {
            let mut stmt = conn.prepare(
                "SELECT id, project, summary, event_type, status, support_count, confidence, related_memory_id
                   FROM memory_candidates
                  WHERE project = ?1 AND event_type = ?2
                  ORDER BY updated_at DESC
                  LIMIT ?3",
            )?;
            collect_candidate_items(&mut stmt.query(params![project, event_type, limit as i64])?)
        }
        ("all", None) => {
            let mut stmt = conn.prepare(
                "SELECT id, project, summary, event_type, status, support_count, confidence, related_memory_id
                   FROM memory_candidates
                  WHERE project = ?1
                  ORDER BY updated_at DESC
                  LIMIT ?2",
            )?;
            collect_candidate_items(&mut stmt.query(params![project, limit as i64])?)
        }
        ("new" | "promoted" | "rejected" | "superseded", Some(event_type)) => {
            let mut stmt = conn.prepare(
                "SELECT id, project, summary, event_type, status, support_count, confidence, related_memory_id
                   FROM memory_candidates
                  WHERE project = ?1 AND status = ?2 AND event_type = ?3
                  ORDER BY updated_at DESC
                  LIMIT ?4",
            )?;
            collect_candidate_items(&mut stmt.query(params![
                project,
                status,
                event_type,
                limit as i64
            ])?)
        }
        ("new" | "promoted" | "rejected" | "superseded", None) => {
            let mut stmt = conn.prepare(
                "SELECT id, project, summary, event_type, status, support_count, confidence, related_memory_id
                   FROM memory_candidates
                  WHERE project = ?1 AND status = ?2
                  ORDER BY updated_at DESC
                  LIMIT ?3",
            )?;
            collect_candidate_items(&mut stmt.query(params![project, status, limit as i64])?)
        }
        (other, _) => {
            bail!(
                "Invalid candidate status: {other}. Valid: new, promoted, rejected, superseded, all"
            )
        }
    }
}

fn collect_candidate_items(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<MemoryCandidateItem>> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(MemoryCandidateItem {
            id: row.get(0)?,
            project: row.get(1)?,
            summary: row.get(2)?,
            event_type: row.get(3)?,
            status: row.get(4)?,
            support_count: row.get(5)?,
            confidence: row.get(6)?,
            related_memory_id: row.get(7)?,
        });
    }
    Ok(out)
}

fn upsert_candidate(
    conn: &Connection,
    project: &str,
    session_id: &str,
    journal_id: i64,
    candidate: &CandidateInput,
) -> Result<(MemoryCandidateRow, bool)> {
    let now = now_epoch_ms();
    let summary_norm = normalize_summary(&candidate.summary);
    let existing = conn
        .query_row(
            "SELECT id, project, session_id, journal_id, event_type, scope, summary, summary_norm,
                    confidence, status, source_kind, trigger_kind, related_memory_id,
                    support_count, created_at, updated_at
               FROM memory_candidates
              WHERE project = ?1 AND event_type = ?2 AND scope = ?3 AND summary_norm = ?4",
            params![project, candidate.event_type, candidate.scope, summary_norm],
            row_to_candidate,
        )
        .optional()?;

    if let Some(row) = existing {
        let confidence = row.confidence.max(candidate.confidence);
        conn.execute(
            "UPDATE memory_candidates
                SET session_id = ?2,
                    journal_id = ?3,
                    summary = ?4,
                    confidence = ?5,
                    support_count = support_count + 1,
                    updated_at = ?6
              WHERE id = ?1",
            params![
                row.id,
                session_id,
                journal_id,
                candidate.summary,
                confidence,
                now
            ],
        )?;
        return Ok((
            load_candidate_from_conn(conn, row.id)?.context(format!(
                "missing memory candidate [{}] after update",
                row.id
            ))?,
            false,
        ));
    }

    conn.execute(
        "INSERT INTO memory_candidates (
            project, session_id, journal_id, event_type, scope, summary, summary_norm,
            confidence, status, source_kind, trigger_kind, related_memory_id,
            support_count, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'new', 'journal', 'passive', NULL, 1, ?9, ?9)",
        params![
            project,
            session_id,
            journal_id,
            candidate.event_type,
            candidate.scope,
            candidate.summary,
            summary_norm,
            candidate.confidence,
            now
        ],
    )?;
    let id = conn.last_insert_rowid();
    Ok((
        load_candidate_from_conn(conn, id)?
            .context(format!("missing memory candidate [{}] after insert", id))?,
        true,
    ))
}

fn load_candidate(id: i64) -> Result<Option<MemoryCandidateRow>> {
    let conn = crate::broker::open_db()?;
    load_candidate_from_conn(&conn, id)
}

fn load_candidate_from_conn(conn: &Connection, id: i64) -> Result<Option<MemoryCandidateRow>> {
    Ok(conn
        .query_row(
            "SELECT id, project, session_id, journal_id, event_type, scope, summary, summary_norm,
                    confidence, status, source_kind, trigger_kind, related_memory_id,
                    support_count, created_at, updated_at
               FROM memory_candidates
              WHERE id = ?1",
            [id],
            row_to_candidate,
        )
        .optional()?)
}

fn row_to_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryCandidateRow> {
    Ok(MemoryCandidateRow {
        id: row.get(0)?,
        project: row.get(1)?,
        session_id: row.get(2)?,
        journal_id: row.get(3)?,
        event_type: row.get(4)?,
        scope: row.get(5)?,
        summary: row.get(6)?,
        summary_norm: row.get(7)?,
        confidence: row.get(8)?,
        status: row.get(9)?,
        source_kind: row.get(10)?,
        trigger_kind: row.get(11)?,
        related_memory_id: row.get(12)?,
        support_count: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn promote_candidate(
    candidate: &MemoryCandidateRow,
    session_id: Option<&str>,
    journal_id: Option<i64>,
    accepted_detail: Option<&str>,
) -> Result<Option<CandidateMemoryAction>> {
    let message = write_memory_event(
        &candidate.project,
        &candidate.event_type,
        &candidate.scope,
        &candidate.summary,
        candidate.confidence,
        &["from-journal-candidate".to_string()],
        "passive",
        "journal-candidate",
    )?;
    let memory_id = parse_memory_id_from_msg(&message);
    if let Some(id) = memory_id {
        let conn = crate::broker::open_db()?;
        conn.execute(
            "UPDATE memory_candidates
                SET status = 'promoted', related_memory_id = ?2, updated_at = ?3
              WHERE id = ?1",
            params![candidate.id, id, now_epoch_ms()],
        )?;
        let _ = crate::repl::journal::store::link_memory_to_journal(id, candidate.journal_id);
        let contradicted = audit_supersession(candidate, id, session_id, journal_id)?;
        if let Some(detail) = accepted_detail {
            log_memory_usage(id, session_id, journal_id, None, "accepted", Some(detail))?;
        }
        return Ok(Some(CandidateMemoryAction {
            memory_id: id,
            contradicted,
        }));
    }
    Ok(None)
}

fn reinforce_promoted_candidate(
    candidate: &MemoryCandidateRow,
    session_id: Option<&str>,
    journal_id: Option<i64>,
    accepted_detail: Option<&str>,
) -> Result<Option<CandidateMemoryAction>> {
    let Some(existing_id) = candidate.related_memory_id else {
        return promote_candidate(candidate, session_id, journal_id, accepted_detail);
    };
    let message = write_memory_event(
        &candidate.project,
        &candidate.event_type,
        &candidate.scope,
        &candidate.summary,
        candidate.confidence,
        &["from-journal-candidate".to_string()],
        "passive",
        "journal-candidate",
    )?;
    let memory_id = parse_memory_id_from_msg(&message).unwrap_or(existing_id);
    let conn = crate::broker::open_db()?;
    conn.execute(
        "UPDATE memory_candidates SET related_memory_id = ?2, updated_at = ?3 WHERE id = ?1",
        params![candidate.id, memory_id, now_epoch_ms()],
    )?;
    let _ = crate::repl::journal::store::link_memory_to_journal(memory_id, candidate.journal_id);
    log_memory_usage(
        memory_id,
        session_id,
        journal_id,
        None,
        "reinforced",
        Some(
            &serde_json::json!({
                "candidate_id": candidate.id,
                "support_count": candidate.support_count,
                "event_type": candidate.event_type,
            })
            .to_string(),
        ),
    )?;
    let contradicted = audit_supersession(candidate, memory_id, session_id, journal_id)?;
    if let Some(detail) = accepted_detail {
        log_memory_usage(
            memory_id,
            session_id,
            journal_id,
            None,
            "accepted",
            Some(detail),
        )?;
    }
    Ok(Some(CandidateMemoryAction {
        memory_id,
        contradicted,
    }))
}

fn audit_supersession(
    candidate: &MemoryCandidateRow,
    new_memory_id: i64,
    session_id: Option<&str>,
    journal_id: Option<i64>,
) -> Result<usize> {
    let conn = crate::broker::open_db()?;
    let mut superseded = superseded_memory_ids(new_memory_id)?;
    if superseded.is_empty() {
        for row in find_active_events(&conn, Some(&candidate.project), Some(&candidate.event_type))?
        {
            if row.id == new_memory_id {
                continue;
            }
            if !candidate_scope_match(&row, &candidate.project, &candidate.scope) {
                continue;
            }
            if word_overlap_ratio(&candidate.summary, &row.summary) <= 0.72 {
                continue;
            }
            conn.execute(
                "UPDATE memory_events SET superseded_by = ?2, updated_at = ?3 WHERE id = ?1",
                params![row.id, new_memory_id, now_epoch_ms()],
            )?;
            superseded.push(row.id);
        }
    }
    if superseded.is_empty() {
        return Ok(0);
    }
    let detail = serde_json::json!({ "superseded_by": new_memory_id }).to_string();
    for old_id in &superseded {
        log_memory_usage(
            *old_id,
            session_id,
            journal_id,
            None,
            "contradicted",
            Some(&detail),
        )?;
        conn.execute(
            "UPDATE memory_candidates
                SET status = 'superseded', updated_at = ?2
              WHERE related_memory_id = ?1 AND status != 'superseded'",
            params![old_id, now_epoch_ms()],
        )?;
    }
    Ok(superseded.len())
}

fn apply_resolution_signals(
    conn: &Connection,
    project: &str,
    session_id: &str,
    journal_id: i64,
    journal: &StructuredJournal,
) -> Result<usize> {
    let topics = resolution_topics(journal);
    if topics.is_empty() {
        return Ok(0);
    }

    let mut stmt = conn.prepare(
        "SELECT id, project, session_id, journal_id, event_type, scope, summary, summary_norm,
                confidence, status, source_kind, trigger_kind, related_memory_id,
                support_count, created_at, updated_at
           FROM memory_candidates
          WHERE project = ?1
            AND event_type = 'open-thread'
            AND status IN ('new', 'promoted')",
    )?;
    let rows = stmt.query_map([project], row_to_candidate)?;
    let mut resolved = 0usize;
    for row in rows {
        let candidate = row?;
        if !topics
            .iter()
            .any(|topic| is_resolution_match(topic, &candidate.summary))
        {
            continue;
        }
        conn.execute(
            "UPDATE memory_candidates
                SET status = 'superseded', updated_at = ?2
              WHERE id = ?1",
            params![candidate.id, now_epoch_ms()],
        )?;
        if let Some(memory_id) = candidate.related_memory_id {
            mark_memory_resolved(memory_id)?;
            log_memory_usage(
                memory_id,
                Some(session_id),
                Some(journal_id),
                None,
                "resolved",
                Some(
                    &serde_json::json!({
                        "resolved_by": &topics,
                        "candidate_id": candidate.id,
                    })
                    .to_string(),
                ),
            )?;
        }
        resolved += 1;
    }
    Ok(resolved)
}

fn resolution_topics(journal: &StructuredJournal) -> Vec<String> {
    journal
        .completed
        .iter()
        .chain(journal.resolved_questions.iter())
        .filter_map(|item| normalize_candidate_summary(item))
        .collect()
}

fn is_resolution_match(topic: &str, candidate_summary: &str) -> bool {
    word_overlap_ratio(topic, candidate_summary) > 0.72
}

fn success_detail_json(journal: &StructuredJournal) -> Option<String> {
    if journal.completed.is_empty() && journal.resolved_questions.is_empty() {
        return None;
    }
    Some(
        serde_json::json!({
            "completed": journal.completed,
            "resolved_questions": journal.resolved_questions,
        })
        .to_string(),
    )
}

fn parse_memory_id_from_msg(msg: &str) -> Option<i64> {
    let start = msg.find('[')?;
    let end = msg.find(']')?;
    if end <= start {
        return None;
    }
    msg[start + 1..end].parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;
    use crate::repl::journal::store::JournalInsert;

    fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = match crate::test_home_lock().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let old_home = env::var_os("HOME");
        let temp_home =
            env::temp_dir().join(format!("sidekar-memory-candidates-test-{}", now_epoch_ms()));
        fs::create_dir_all(&temp_home)?;

        unsafe { env::set_var("HOME", &temp_home) };

        let result = f();

        match old_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&temp_home);
        result
    }

    fn count_memories(project: &str, event_type: &str) -> Result<i64> {
        let conn = crate::broker::open_db()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM memory_events
              WHERE project = ?1 AND event_type = ?2 AND superseded_by IS NULL",
            params![project, event_type],
            |row| row.get(0),
        )?)
    }

    fn seed_session(session_id: &str, project: &str) -> Result<()> {
        let conn = crate::broker::open_db()?;
        conn.execute(
            "INSERT INTO repl_sessions (id, cwd, created_at, updated_at)
             VALUES (?1, ?2, 0.0, 0.0)",
            params![session_id, project],
        )?;
        Ok(())
    }

    fn seed_journal(session_id: &str, project: &str, at: f64) -> Result<i64> {
        let structured = serde_json::to_string(&StructuredJournal::default())?;
        let insert = JournalInsert {
            session_id,
            project,
            from_entry_id: "e-a",
            to_entry_id: "e-b",
            structured_json: &structured,
            headline: "h",
            previous_id: None,
            model_used: "m",
            cred_used: "c",
            tokens_in: 0,
            tokens_out: 0,
            created_at: Some(at),
        };
        crate::repl::journal::store::insert_journal(&insert)
    }

    #[test]
    fn process_journal_extracts_candidates_once_per_unique_value() -> Result<()> {
        with_test_home(|| {
            crate::broker::init_db()?;
            seed_session("sess", "proj")?;
            let journal_id = seed_journal("sess", "proj", 1_000.0)?;
            let journal = StructuredJournal {
                constraints: vec!["use cargo test --lib".into(), "use cargo test --lib".into()],
                decisions: vec!["keep sqlite in WAL mode".into()],
                blocked: vec!["need AWS account confirmation".into()],
                pending_user_asks: vec!["need AWS account confirmation".into()],
                relevant_files: vec!["src/providers/bedrock.rs".into()],
                ..Default::default()
            };

            let outcome = process_journal_candidates("proj", "sess", journal_id, &journal)?;
            assert_eq!(outcome.extracted, 4);
            assert_eq!(outcome.inserted, 4);
            assert_eq!(outcome.auto_promoted, 0);

            let conn = crate::broker::open_db()?;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_candidates WHERE project = 'proj'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(count, 4);
            Ok(())
        })
    }

    #[test]
    fn repeated_candidates_auto_promote_to_memory() -> Result<()> {
        with_test_home(|| {
            crate::broker::init_db()?;
            seed_session("sess-1", "proj")?;
            seed_session("sess-2", "proj")?;
            let journal_id_1 = seed_journal("sess-1", "proj", 1_000.0)?;
            let journal_id_2 = seed_journal("sess-2", "proj", 2_000.0)?;
            let journal = StructuredJournal {
                constraints: vec!["never push to main".into()],
                relevant_files: vec!["src/repl.rs".into()],
                ..Default::default()
            };

            let first = process_journal_candidates("proj", "sess-1", journal_id_1, &journal)?;
            assert_eq!(first.auto_promoted, 0);
            let second = process_journal_candidates("proj", "sess-2", journal_id_2, &journal)?;
            assert_eq!(second.auto_promoted, 2);
            assert_eq!(count_memories("proj", "constraint")?, 1);
            assert_eq!(count_memories("proj", "artifact-pointer")?, 1);

            let conn = crate::broker::open_db()?;
            let statuses: Vec<(String, Option<i64>)> = conn
                .prepare(
                    "SELECT status, related_memory_id
                       FROM memory_candidates
                      WHERE project = 'proj'
                      ORDER BY event_type",
                )?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            assert_eq!(
                statuses,
                vec![
                    ("promoted".to_string(), Some(2)),
                    ("promoted".to_string(), Some(1)),
                ]
            );
            Ok(())
        })
    }

    #[test]
    fn rejected_candidates_do_not_auto_promote() -> Result<()> {
        with_test_home(|| {
            crate::broker::init_db()?;
            seed_session("sess-1", "proj")?;
            seed_session("sess-2", "proj")?;
            let journal_id_1 = seed_journal("sess-1", "proj", 1_000.0)?;
            let journal_id_2 = seed_journal("sess-2", "proj", 2_000.0)?;
            let journal = StructuredJournal {
                blocked: vec!["waiting on legal review".into()],
                ..Default::default()
            };
            process_journal_candidates("proj", "sess-1", journal_id_1, &journal)?;

            let conn = crate::broker::open_db()?;
            let id: i64 = conn.query_row(
                "SELECT id FROM memory_candidates WHERE project = 'proj' LIMIT 1",
                [],
                |row| row.get(0),
            )?;
            conn.execute(
                "UPDATE memory_candidates SET status = 'rejected' WHERE id = ?1",
                [id],
            )?;

            let second = process_journal_candidates("proj", "sess-2", journal_id_2, &journal)?;
            assert_eq!(second.auto_promoted, 0);
            assert_eq!(count_memories("proj", "open-thread")?, 0);
            let status: String = conn.query_row(
                "SELECT status FROM memory_candidates WHERE id = ?1",
                [id],
                |row| row.get(0),
            )?;
            assert_eq!(status, "rejected");
            Ok(())
        })
    }

    #[test]
    fn conflicting_candidates_supersede_old_memory_and_log_contradiction() -> Result<()> {
        with_test_home(|| {
            crate::broker::init_db()?;
            for session in ["sess-1", "sess-2", "sess-3", "sess-4"] {
                seed_session(session, "proj")?;
            }
            let j1 = seed_journal("sess-1", "proj", 1_000.0)?;
            let j2 = seed_journal("sess-2", "proj", 2_000.0)?;
            let j3 = seed_journal("sess-3", "proj", 3_000.0)?;
            let j4 = seed_journal("sess-4", "proj", 4_000.0)?;

            let sqlite = StructuredJournal {
                decisions: vec!["use sqlite backing engine for metadata store".into()],
                ..Default::default()
            };
            let postgres = StructuredJournal {
                decisions: vec!["use postgres backing engine for metadata store".into()],
                ..Default::default()
            };

            process_journal_candidates("proj", "sess-1", j1, &sqlite)?;
            let second = process_journal_candidates("proj", "sess-2", j2, &sqlite)?;
            assert_eq!(second.auto_promoted, 1);
            let fourth = process_journal_candidates("proj", "sess-3", j3, &postgres)?;
            assert_eq!(fourth.auto_promoted, 0);
            let fifth = process_journal_candidates("proj", "sess-4", j4, &postgres)?;
            assert_eq!(fifth.auto_promoted, 1);
            assert_eq!(fifth.contradicted, 1);

            let conn = crate::broker::open_db()?;
            let old_memory: (i64, Option<i64>) = conn.query_row(
                "SELECT id, superseded_by FROM memory_events
                  WHERE summary = 'use sqlite backing engine for metadata store'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert!(old_memory.1.is_some(), "old memory should be superseded");

            let old_candidate_status: String = conn.query_row(
                "SELECT status FROM memory_candidates
                  WHERE summary = 'use sqlite backing engine for metadata store'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(old_candidate_status, "superseded");

            let usage = recent_memory_usage(old_memory.0, 5)?;
            assert!(
                usage.iter().any(|row| row.usage_kind == "contradicted"),
                "expected contradicted usage log"
            );
            assert_eq!(count_memories("proj", "decision")?, 1);
            Ok(())
        })
    }

    #[test]
    fn resolved_open_threads_are_hidden_and_logged() -> Result<()> {
        with_test_home(|| {
            crate::broker::init_db()?;
            for session in ["sess-1", "sess-2", "sess-3"] {
                seed_session(session, "proj")?;
            }
            let j1 = seed_journal("sess-1", "proj", 1_000.0)?;
            let j2 = seed_journal("sess-2", "proj", 2_000.0)?;
            let j3 = seed_journal("sess-3", "proj", 3_000.0)?;

            let blocked = StructuredJournal {
                blocked: vec!["waiting on legal review".into()],
                ..Default::default()
            };
            process_journal_candidates("proj", "sess-1", j1, &blocked)?;
            let promoted = process_journal_candidates("proj", "sess-2", j2, &blocked)?;
            assert_eq!(promoted.auto_promoted, 1);

            let resolved_journal = StructuredJournal {
                completed: vec!["resolved waiting on legal review".into()],
                ..Default::default()
            };
            let resolved = process_journal_candidates("proj", "sess-3", j3, &resolved_journal)?;
            assert_eq!(resolved.resolved, 1);

            let conn = crate::broker::open_db()?;
            let memory: (i64, String) = conn.query_row(
                "SELECT id, tags_json FROM memory_events
                  WHERE summary = 'waiting on legal review'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert!(memory.1.contains("_resolved"));

            let candidate_status: String = conn.query_row(
                "SELECT status FROM memory_candidates
                  WHERE summary = 'waiting on legal review'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(candidate_status, "superseded");

            let search = search_events(
                "legal review",
                crate::scope::ScopeView::Project,
                Some("proj"),
                Some("open-thread"),
                5,
            )?;
            assert!(
                search.is_empty(),
                "resolved thread should be hidden from active search"
            );

            let usage = recent_memory_usage(memory.0, 5)?;
            assert!(
                usage.iter().any(|row| row.usage_kind == "resolved"),
                "expected resolved usage log"
            );
            Ok(())
        })
    }
}
