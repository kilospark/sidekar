//! Memory promoter: turn repeated journal constraints/decisions
//! into `memory_events` rows so they survive even if the user
//! disables journaling or the journal rows are pruned.
//!
//! Rationale: the journaling subsystem is a short-term recall
//! aid — capped, paginated, injected into the system prompt.
//! Durable preferences ("always use cargo test --lib"),
//! constraints ("never push to main"), and decisions ("we're
//! using the 12-section template") deserve a first-class home
//! in the `memory_events` table where the existing /memory
//! tooling can search, compact, and surface them across the
//! project.
//!
//! Trigger: when the same normalized entry appears in >= N
//! distinct journals for the current project, promote it as
//! a single memory_events row with low confidence. Subsequent
//! reinforcements via `write_memory_event`'s dedup path bump
//! confidence automatically.
//!
//! Linkage: every time we promote, we insert a row into
//! `memory_journal_support` via `store::link_memory_to_journal`
//! pairing the new memory id with each supporting journal id.
//! This is the provenance chain — operators can ask "what
//! journals backed this memory?" or "what memories did this
//! journal contribute to?".
//!
//! Scope: project-local. Cross-project generalization is
//! already handled by `memory::detect_patterns`. Running the
//! promoter here only within-project keeps the surface narrow
//! — the global promotion layer can pick up from there if the
//! same pattern surfaces in multiple projects.
//!
//! This module is pure-ish: the DB reads go through existing
//! helpers, writes go through `memory::write_memory_event`. No
//! new SQL invented here.

use std::collections::HashMap;

use anyhow::Result;

use crate::memory;
use crate::repl::journal::parse::{self, StructuredJournal};
use crate::repl::journal::store;

/// How many distinct journals must reference the same normalized
/// constraint before it's promoted to `memory_events`. Tuned
/// conservatively — two is too permissive (picks up momentary
/// phrasings), four is too strict (stable constraints never
/// surface). Three is where hermes settled empirically.
const PROMOTE_THRESHOLD: usize = 3;

/// Confidence we stamp on newly-promoted memories. Low enough
/// that direct-authored `sidekar memory write` entries (default
/// ~0.75) always outrank a promotion. Reinforcement via repeat
/// promotion will bump this up naturally through the
/// `write_memory_event` dedup path.
const PROMOTE_CONFIDENCE: f64 = 0.60;

/// How many journals to scan for promotion candidates. Cap so a
/// project with thousands of journals doesn't produce a huge
/// in-memory bucket. Newest-first — older journals are the most
/// likely to be stale.
const SCAN_WINDOW_JOURNALS: usize = 50;

/// Outcome bundle. Callers (the background task, or `/journal
/// promote` in step 10) can log per-kind counts to surface
/// what changed this pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PromoteOutcome {
    pub constraints_promoted: usize,
    pub decisions_promoted: usize,
    /// Journals scanned. For observability; 0 means "no journals
    /// for this project yet."
    pub journals_scanned: usize,
    /// Memory ids created this pass. Useful for linking + testing.
    pub new_memory_ids: Vec<i64>,
}

/// Run one promotion pass for a project. Reads the last
/// `SCAN_WINDOW_JOURNALS` journals, buckets their constraints
/// and decisions by normalized form, promotes any bucket that
/// reaches `PROMOTE_THRESHOLD` distinct journals.
///
/// Idempotent in the sense that `write_memory_event` deduplicates
/// by `summary_hash` — running this twice on the same journal set
/// won't create duplicate memories; subsequent runs reinforce.
///
/// Failures inside the loop are logged and the pass continues —
/// one bad row shouldn't abort the promotion of the rest.
pub(crate) fn run_for_project(project: &str) -> Result<PromoteOutcome> {
    let rows = store::recent_for_project(project, SCAN_WINDOW_JOURNALS)?;
    let mut outcome = PromoteOutcome {
        journals_scanned: rows.len(),
        ..Default::default()
    };
    if rows.is_empty() {
        return Ok(outcome);
    }

    // Parse each stored journal once. Store (journal_id, Parsed).
    // Degraded journals are skipped for promotion — they're
    // unreliable signal.
    let parsed: Vec<(i64, StructuredJournal)> = rows
        .iter()
        .filter_map(|row| {
            let outcome = parse::parse_response(&row.structured_json);
            if outcome.was_degraded {
                return None;
            }
            Some((row.id, outcome.journal))
        })
        .collect();

    outcome.constraints_promoted = promote_field(
        project,
        "constraint",
        &parsed,
        |j| j.constraints.as_slice(),
        &mut outcome.new_memory_ids,
    );
    outcome.decisions_promoted = promote_field(
        project,
        "decision",
        &parsed,
        |j| j.decisions.as_slice(),
        &mut outcome.new_memory_ids,
    );

    Ok(outcome)
}

/// Bucket a specific field across parsed journals by normalized
/// form, promote any bucket at or over the threshold. Returns
/// number of buckets that resulted in a fresh `memory_events`
/// insert (not counting dedup reinforcements).
fn promote_field<F>(
    project: &str,
    event_type: &str,
    parsed: &[(i64, StructuredJournal)],
    extract: F,
    new_ids: &mut Vec<i64>,
) -> usize
where
    F: Fn(&StructuredJournal) -> &[String],
{
    // Normalized string -> (exemplar, supporting journal ids).
    // Using the normalized form as the key means "use cargo test
    // --lib" and "Use Cargo test --lib  " bucket together.
    let mut buckets: HashMap<String, Bucket> = HashMap::new();
    for (journal_id, journal) in parsed {
        for item in extract(journal) {
            let norm = normalize(item);
            if norm.len() < 3 {
                // Ignore ultra-short items — they're usually
                // punctuation or noise ("."), never a real
                // constraint.
                continue;
            }
            let b = buckets.entry(norm).or_insert_with(|| Bucket {
                exemplar: item.clone(),
                journal_ids: Vec::new(),
            });
            // Only count a journal once per bucket even if the
            // same constraint appears twice in its list (LLMs
            // sometimes repeat themselves). Dedup via linear
            // scan; bucket lists are small.
            if !b.journal_ids.contains(journal_id) {
                b.journal_ids.push(*journal_id);
            }
        }
    }

    let mut promoted = 0;
    for (_norm, b) in buckets {
        if b.journal_ids.len() < PROMOTE_THRESHOLD {
            continue;
        }
        match memory::write_memory_event(
            project,
            event_type,
            "project",
            &b.exemplar,
            PROMOTE_CONFIDENCE,
            &["from-journal".to_string()],
            "passive",
            "journal",
        ) {
            Ok(msg) => {
                // write_memory_event returns either "Stored
                // memory [N]." or "Deduplicated existing memory
                // [N].". Extract the id from either.
                if let Some(id) = parse_memory_id_from_msg(&msg) {
                    // Link every supporting journal to the
                    // memory regardless of whether it was newly
                    // stored or a dedup hit — the dedup path
                    // still represents 'these journals support
                    // this memory.'
                    for jid in &b.journal_ids {
                        let _ = store::link_memory_to_journal(id, *jid);
                    }
                    if msg.starts_with("Stored memory [") {
                        promoted += 1;
                        new_ids.push(id);
                    }
                }
            }
            Err(e) => {
                crate::broker::try_log_error(
                    "journal",
                    &format!(
                        "promote {event_type}: write_memory_event failed: {e:#}"
                    ),
                    None,
                );
            }
        }
    }
    promoted
}

struct Bucket {
    /// First-seen original text; used verbatim as the memory
    /// summary. The normalized key decides bucket membership,
    /// but humans read the exemplar.
    exemplar: String,
    /// Ids of journals that contributed to this bucket. Distinct.
    journal_ids: Vec<i64>,
}

/// Cheap, deterministic normalizer: lowercase, trim, collapse
/// internal whitespace runs to single spaces. Strip trailing
/// sentence punctuation since that often drifts between journal
/// runs ("use cargo test --lib" vs "use cargo test --lib.").
///
/// Deliberately NOT importing `memory::normalize_summary` because
/// it's pub(super) — reaching across module privacy would couple
/// us too tightly. The normalizer here is a subset of what memory
/// does, which is fine: worst case, two entries that would have
/// collided under memory's stricter rules get promoted as separate
/// memories; `memory::write_memory_event` then collapses them via
/// its own word-overlap check. Redundancy, not contradiction.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = true; // swallow leading whitespace
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_ws = false;
        }
    }
    // Strip trailing whitespace + trailing sentence punctuation.
    while let Some(c) = out.chars().last() {
        if c == ' ' || c == '.' || c == ',' || c == ';' || c == '!' {
            out.pop();
        } else {
            break;
        }
    }
    out
}

/// Extract the numeric id from write_memory_event's human-readable
/// return string. Returns None on format drift. Used only for
/// back-linking into memory_journal_support — a None here means
/// the memory exists but we lose the linkage record, not a
/// correctness hazard.
fn parse_memory_id_from_msg(msg: &str) -> Option<i64> {
    // Both formats: "Stored memory [N]." and "Deduplicated existing memory [N]."
    let start = msg.find('[')?;
    let end = msg.find(']')?;
    if end <= start {
        return None;
    }
    msg[start + 1..end].parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- normalize ------------------------------------------

    #[test]
    fn normalize_collapses_case_and_whitespace() {
        assert_eq!(normalize("  Use   Cargo TEST --lib  "), "use cargo test --lib");
    }

    #[test]
    fn normalize_strips_trailing_punctuation() {
        assert_eq!(normalize("never push to main."), "never push to main");
        assert_eq!(normalize("use foo,"), "use foo");
        assert_eq!(normalize("do the thing!"), "do the thing");
    }

    #[test]
    fn normalize_preserves_internal_punctuation() {
        // Hyphens and slashes inside the text matter (cli flags).
        assert_eq!(normalize("--no-merge --rebase"), "--no-merge --rebase");
    }

    #[test]
    fn normalize_empty_or_whitespace() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   "), "");
    }

    // ---- parse_memory_id_from_msg ----------------------------

    #[test]
    fn parse_id_from_stored_message() {
        assert_eq!(parse_memory_id_from_msg("Stored memory [42]."), Some(42));
    }

    #[test]
    fn parse_id_from_dedup_message() {
        assert_eq!(
            parse_memory_id_from_msg("Deduplicated existing memory [7]."),
            Some(7)
        );
    }

    #[test]
    fn parse_id_returns_none_on_garbage() {
        assert_eq!(parse_memory_id_from_msg("hello"), None);
        assert_eq!(parse_memory_id_from_msg("Stored memory []"), None);
        assert_eq!(parse_memory_id_from_msg("[abc]"), None);
    }

    // ---- bucket math via run_for_project ---------------------
    //
    // Full DB-integration tests for the end-to-end promotion
    // path — seeded journals, assert memory_events row count and
    // memory_journal_support link count.

    use crate::broker;
    use rusqlite::params;

    fn with_test_db<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow::anyhow!("home lock poisoned"))?;
        let old_home = std::env::var_os("HOME");
        let temp = std::env::temp_dir().join(format!(
            "sidekar-promote-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp)?;
        // Safety: test-only, restored on exit.
        unsafe {
            std::env::set_var("HOME", &temp);
        }
        broker::init_db()?;
        let result = f();
        match old_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&temp);
        result
    }

    fn seed_session(id: &str, cwd: &str) -> Result<()> {
        let conn = broker::open()?;
        conn.execute(
            "INSERT INTO repl_sessions (id, cwd, created_at, updated_at)
             VALUES (?1, ?2, 0.0, 0.0)",
            params![id, cwd],
        )?;
        Ok(())
    }

    fn seed_journal(
        session_id: &str,
        project: &str,
        structured: &StructuredJournal,
        at: f64,
    ) -> Result<i64> {
        let sj = serde_json::to_string(structured)?;
        let insert = store::JournalInsert {
            session_id,
            project,
            from_entry_id: "e-a",
            to_entry_id: "e-b",
            structured_json: &sj,
            headline: "h",
            previous_id: None,
            model_used: "m",
            cred_used: "c",
            tokens_in: 0,
            tokens_out: 0,
            created_at: Some(at),
        };
        store::insert_journal(&insert)
    }

    fn count_memories(project: &str, event_type: &str) -> Result<i64> {
        let conn = broker::open()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM memory_events
              WHERE project = ?1 AND event_type = ?2 AND superseded_by IS NULL",
            params![project, event_type],
            |r| r.get(0),
        )?)
    }

    fn count_links(memory_id: i64) -> Result<i64> {
        let conn = broker::open()?;
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM memory_journal_support WHERE memory_id = ?1",
            [memory_id],
            |r| r.get(0),
        )?)
    }

    #[test]
    fn promotes_constraint_after_threshold_reached() -> Result<()> {
        with_test_db(|| {
            seed_session("s-p-1", "/tmp/proj")?;
            let j = StructuredJournal {
                constraints: vec!["use cargo test --lib".into()],
                ..Default::default()
            };
            // Three journals with the same constraint => promote.
            let j1 = seed_journal("s-p-1", "/tmp/proj", &j, 1_000.0)?;
            let j2 = seed_journal("s-p-1", "/tmp/proj", &j, 2_000.0)?;
            let j3 = seed_journal("s-p-1", "/tmp/proj", &j, 3_000.0)?;

            let out = run_for_project("/tmp/proj")?;
            assert_eq!(out.journals_scanned, 3);
            assert_eq!(out.constraints_promoted, 1);
            assert_eq!(out.new_memory_ids.len(), 1);
            assert_eq!(count_memories("/tmp/proj", "constraint")?, 1);

            // All three journals linked to the promoted memory.
            let mid = out.new_memory_ids[0];
            assert_eq!(count_links(mid)?, 3);

            // The link edges reference the exact journal ids.
            let conn = broker::open()?;
            let mut stmt = conn.prepare(
                "SELECT journal_id FROM memory_journal_support WHERE memory_id = ?1 ORDER BY journal_id",
            )?;
            let ids: Vec<i64> = stmt
                .query_map([mid], |r| r.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            assert_eq!(ids, vec![j1, j2, j3]);
            Ok(())
        })
    }

    #[test]
    fn below_threshold_does_not_promote() -> Result<()> {
        with_test_db(|| {
            seed_session("s-p-2", "/tmp/proj")?;
            let j = StructuredJournal {
                constraints: vec!["some rare rule".into()],
                ..Default::default()
            };
            // Only two occurrences — below the 3-journal threshold.
            seed_journal("s-p-2", "/tmp/proj", &j, 1_000.0)?;
            seed_journal("s-p-2", "/tmp/proj", &j, 2_000.0)?;

            let out = run_for_project("/tmp/proj")?;
            assert_eq!(out.journals_scanned, 2);
            assert_eq!(out.constraints_promoted, 0);
            assert_eq!(count_memories("/tmp/proj", "constraint")?, 0);
            Ok(())
        })
    }

    #[test]
    fn case_and_whitespace_variants_bucket_together() -> Result<()> {
        with_test_db(|| {
            seed_session("s-p-3", "/tmp/proj")?;
            let variants = [
                "Use Cargo test --lib",
                "  use CARGO TEST --lib  ",
                "use  cargo  test  --lib.",
            ];
            for (i, v) in variants.iter().enumerate() {
                let j = StructuredJournal {
                    constraints: vec![v.to_string()],
                    ..Default::default()
                };
                seed_journal("s-p-3", "/tmp/proj", &j, 1_000.0 + (i as f64))?;
            }

            let out = run_for_project("/tmp/proj")?;
            assert_eq!(out.constraints_promoted, 1);
            Ok(())
        })
    }

    #[test]
    fn multiple_fields_promote_independently() -> Result<()> {
        with_test_db(|| {
            seed_session("s-p-4", "/tmp/proj")?;
            let j = StructuredJournal {
                constraints: vec!["use cargo test --lib".into()],
                decisions: vec!["picked 12-section template".into()],
                ..Default::default()
            };
            for i in 0..3 {
                seed_journal("s-p-4", "/tmp/proj", &j, (1_000 + i) as f64)?;
            }
            let out = run_for_project("/tmp/proj")?;
            assert_eq!(out.constraints_promoted, 1);
            assert_eq!(out.decisions_promoted, 1);
            Ok(())
        })
    }

    #[test]
    fn degraded_journals_are_ignored() -> Result<()> {
        with_test_db(|| {
            seed_session("s-p-5", "/tmp/proj")?;
            // Two good + one garbage. Below the good-only
            // threshold so no promotion.
            let j = StructuredJournal {
                constraints: vec!["rare constraint".into()],
                ..Default::default()
            };
            seed_journal("s-p-5", "/tmp/proj", &j, 1_000.0)?;
            seed_journal("s-p-5", "/tmp/proj", &j, 2_000.0)?;

            // Seed a row with invalid structured_json directly.
            let insert = store::JournalInsert {
                session_id: "s-p-5",
                project: "/tmp/proj",
                from_entry_id: "x",
                to_entry_id: "y",
                structured_json: "not json",
                headline: "bad",
                previous_id: None,
                model_used: "m",
                cred_used: "c",
                tokens_in: 0,
                tokens_out: 0,
                created_at: Some(3_000.0),
            };
            store::insert_journal(&insert)?;

            let out = run_for_project("/tmp/proj")?;
            assert_eq!(out.journals_scanned, 3);
            // Still below threshold because degraded doesn't count.
            assert_eq!(out.constraints_promoted, 0);
            Ok(())
        })
    }

    #[test]
    fn empty_project_promotes_nothing() -> Result<()> {
        with_test_db(|| {
            let out = run_for_project("/tmp/empty-proj")?;
            assert_eq!(out.journals_scanned, 0);
            assert_eq!(out.constraints_promoted, 0);
            assert_eq!(out.decisions_promoted, 0);
            Ok(())
        })
    }

    #[test]
    fn ultra_short_items_ignored() -> Result<()> {
        // Single-char "." noise shouldn't bucket into a promotable
        // entry no matter how many times it appears.
        with_test_db(|| {
            seed_session("s-p-6", "/tmp/proj")?;
            let j = StructuredJournal {
                constraints: vec![".".into(), "-".into(), "x".into()],
                ..Default::default()
            };
            for i in 0..5 {
                seed_journal("s-p-6", "/tmp/proj", &j, (1_000 + i) as f64)?;
            }
            let out = run_for_project("/tmp/proj")?;
            assert_eq!(out.constraints_promoted, 0);
            Ok(())
        })
    }
}
