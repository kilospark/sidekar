use super::*;

// ---------------------------------------------------------------------------
// Hygiene report: read-only audit of memory quality issues
// ---------------------------------------------------------------------------

/// A single issue found during hygiene audit.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct HygieneIssue {
    pub kind: &'static str,
    pub ids: Vec<i64>,
    pub detail: String,
}

/// Full hygiene report.
#[derive(Debug, serde::Serialize)]
pub(super) struct HygieneReport {
    pub total_active: usize,
    pub total_superseded: usize,
    pub issues: Vec<HygieneIssue>,
}

impl crate::output::CommandOutput for HygieneReport {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(
            w,
            "{} active memories, {} superseded.",
            self.total_active, self.total_superseded
        )?;
        if self.issues.is_empty() {
            writeln!(w, "No issues found.")?;
            return Ok(());
        }
        writeln!(w, "{} issues:\n", self.issues.len())?;
        for (i, issue) in self.issues.iter().enumerate() {
            let ids = issue
                .ids
                .iter()
                .map(|id| format!("[{}]", id))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(w, "{}. {} {}: {}", i + 1, issue.kind, ids, issue.detail)?;
        }
        Ok(())
    }
}

/// Run a read-only hygiene audit over all active memories.
pub(super) fn run_hygiene(project: Option<&str>) -> Result<HygieneReport> {
    let conn = crate::broker::open_db()?;

    // Count active vs superseded
    let total_active: usize = conn.query_row(
        "SELECT COUNT(*) FROM memory_events WHERE superseded_by IS NULL",
        [],
        |row| row.get(0),
    )?;
    let total_superseded: usize = conn.query_row(
        "SELECT COUNT(*) FROM memory_events WHERE superseded_by IS NOT NULL",
        [],
        |row| row.get(0),
    )?;

    let active = find_active_events(&conn, project, None)?;
    let mut issues = Vec::new();

    // 1. Duplicate clusters: groups with high word overlap
    find_duplicates(&active, &mut issues);

    // 2. Low confidence memories (user-rated as wrong/outdated)
    find_low_confidence(&active, &mut issues);

    // 3. Stale entries: open-thread or artifact-pointer older than thresholds
    find_stale(&active, &mut issues);

    // 4. Short summaries (< 20 chars) — likely too vague to be useful
    find_short_summaries(&active, &mut issues);

    // 5. Orphaned supersession chains (supersedes something that doesn't exist)
    find_orphaned_chains(&conn, &active, &mut issues);

    Ok(HygieneReport {
        total_active,
        total_superseded,
        issues,
    })
}

fn find_duplicates(rows: &[MemoryEventRow], issues: &mut Vec<HygieneIssue>) {
    for event_type in MEMORY_TYPES {
        let typed: Vec<MemoryEventRow> = rows
            .iter()
            .filter(|r| r.event_type == *event_type)
            .cloned()
            .collect();
        if typed.len() < 2 {
            continue;
        }
        for cluster in cluster_by_similarity(&typed, 0.72) {
            if cluster.len() < 2 {
                continue;
            }
            let ids: Vec<i64> = cluster.iter().map(|r| r.id).collect();
            let summaries: Vec<String> = cluster
                .iter()
                .map(|r| format!("[{}] \"{}\"", r.id, truncate_str(&r.summary, 60)))
                .collect();
            issues.push(HygieneIssue {
                kind: "duplicate",
                ids,
                detail: format!(
                    "{} similar {} memories: {}",
                    cluster.len(),
                    event_type,
                    summaries.join("; ")
                ),
            });
        }
    }
}

fn find_low_confidence(rows: &[MemoryEventRow], issues: &mut Vec<HygieneIssue>) {
    for row in rows {
        if row.confidence <= 0.3 {
            issues.push(HygieneIssue {
                kind: "low-confidence",
                ids: vec![row.id],
                detail: format!(
                    "confidence {:.2} ({}): \"{}\"",
                    row.confidence,
                    row.event_type,
                    truncate_str(&row.summary, 80)
                ),
            });
        }
    }
}

fn find_stale(rows: &[MemoryEventRow], issues: &mut Vec<HygieneIssue>) {
    let now = now_epoch_ms();
    for row in rows {
        let age_days = (now - row.updated_at).max(0) as f64 / 86_400_000.0;
        let threshold = match row.event_type.as_str() {
            "open-thread" => 14.0,
            "artifact-pointer" => 60.0,
            _ => continue,
        };
        if age_days > threshold {
            issues.push(HygieneIssue {
                kind: "stale",
                ids: vec![row.id],
                detail: format!(
                    "{} unchanged for {:.0} days: \"{}\"",
                    row.event_type,
                    age_days,
                    truncate_str(&row.summary, 80)
                ),
            });
        }
    }
}

fn find_short_summaries(rows: &[MemoryEventRow], issues: &mut Vec<HygieneIssue>) {
    for row in rows {
        if row.summary.len() < 20 {
            issues.push(HygieneIssue {
                kind: "too-short",
                ids: vec![row.id],
                detail: format!(
                    "{} has only {} chars: \"{}\"",
                    row.event_type,
                    row.summary.len(),
                    row.summary
                ),
            });
        }
    }
}

fn find_orphaned_chains(
    conn: &rusqlite::Connection,
    active: &[MemoryEventRow],
    issues: &mut Vec<HygieneIssue>,
) {
    for row in active {
        let supersedes_json: Option<String> = conn
            .query_row(
                "SELECT supersedes_json FROM memory_events WHERE id = ?1",
                [row.id],
                |r| r.get(0),
            )
            .ok();
        let supersedes: Vec<i64> = supersedes_json
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        for old_id in &supersedes {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_events WHERE id = ?1",
                    [old_id],
                    |r| r.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);
            if !exists {
                issues.push(HygieneIssue {
                    kind: "orphaned-chain",
                    ids: vec![row.id],
                    detail: format!(
                        "supersedes deleted memory [{}]: \"{}\"",
                        old_id,
                        truncate_str(&row.summary, 80)
                    ),
                });
            }
        }
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}
