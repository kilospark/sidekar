use super::*;

/// Run `f` with HOME at a scratch directory, restored and removed afterwards
/// even if `f` panics.
fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _home = crate::ScratchHome::new();
    f()
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
fn session_archives_are_verbatim_and_never_deduped() -> Result<()> {
    with_test_home(|| {
        // write_memory_event would merge two identical rows; archives must not.
        let a = write_session_archive(
            "alpha",
            "project",
            Some("Login debug"),
            "fixed the remembered-device MFA cookie",
            "muse",
            &[],
        )?;
        let b = write_session_archive(
            "alpha",
            "project",
            Some("Login debug"),
            "fixed the remembered-device MFA cookie",
            "muse",
            &[],
        )?;
        assert_ne!(a, b, "identical archives are distinct rows, not merged");
        Ok(())
    })
}

#[test]
fn a_session_archive_is_searchable_by_body_and_title() -> Result<()> {
    with_test_home(|| {
        write_session_archive(
            "alpha",
            "project",
            Some("Readability pipeline"),
            "we switched to the mozilla parser for articles",
            "grok",
            &["research".to_string()],
        )?;
        let by_body = search_events(
            "mozilla parser",
            crate::scope::ScopeView::Project,
            Some("alpha"),
            None,
            5,
        )?;
        assert_eq!(by_body.len(), 1, "found by a body term");
        assert_eq!(by_body[0].row.event_type, "session");

        // The title leads the stored text, so it is indexed too.
        let by_title = search_events(
            "Readability pipeline",
            crate::scope::ScopeView::Project,
            Some("alpha"),
            None,
            5,
        )?;
        assert_eq!(by_title.len(), 1, "found by a title term");
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

#[test]
fn hygiene_finds_duplicates() -> Result<()> {
    with_test_home(|| {
        write_memory_event(
            "proj",
            "convention",
            "project",
            "Always use snake_case for function names in Python modules",
            0.8,
            &[],
            "explicit",
            "user",
        )?;
        write_memory_event(
            "proj",
            "convention",
            "project",
            "Always use snake_case for function names in Python source files",
            0.8,
            &[],
            "explicit",
            "user",
        )?;

        let report = run_hygiene(Some("proj"))?;
        assert!(report.total_active >= 2);
        let dups: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.kind == "duplicate")
            .collect();
        assert!(!dups.is_empty(), "should find duplicate cluster");
        assert!(dups[0].ids.len() >= 2);
        Ok(())
    })
}

#[test]
fn hygiene_finds_low_confidence() -> Result<()> {
    with_test_home(|| {
        write_memory_event(
            "proj",
            "decision",
            "project",
            "Use PostgreSQL for the main database backend",
            0.2,
            &[],
            "explicit",
            "user",
        )?;

        let report = run_hygiene(Some("proj"))?;
        let low: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.kind == "low-confidence")
            .collect();
        assert!(!low.is_empty(), "should flag low confidence memory");
        Ok(())
    })
}

#[test]
fn hygiene_finds_short_summaries() -> Result<()> {
    with_test_home(|| {
        write_memory_event(
            "proj",
            "convention",
            "project",
            "use ruff",
            0.8,
            &[],
            "explicit",
            "user",
        )?;

        let report = run_hygiene(Some("proj"))?;
        let short: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.kind == "too-short")
            .collect();
        assert!(!short.is_empty(), "should flag short summary");
        Ok(())
    })
}

#[test]
fn hygiene_clean_db_has_no_issues() -> Result<()> {
    with_test_home(|| {
        write_memory_event(
            "proj",
            "convention",
            "project",
            "Use Readability.js before scraping article text from web pages",
            0.8,
            &[],
            "explicit",
            "user",
        )?;

        let report = run_hygiene(Some("proj"))?;
        assert_eq!(report.total_active, 1);
        assert!(
            report.issues.is_empty(),
            "clean DB should have no issues, got: {:?}",
            report.issues
        );
        Ok(())
    })
}

#[test]
fn relevant_brief_selects_matching_memories_and_logs_usage() -> Result<()> {
    with_test_home(|| {
        crate::broker::init_db()?;
        let conn = crate::broker::open_db()?;
        conn.execute(
            "INSERT INTO repl_sessions (id, cwd, created_at, updated_at)
             VALUES ('s-brief', '/tmp/proj', 0.0, 0.0)",
            [],
        )?;

        write_memory_event(
            "proj",
            "artifact-pointer",
            "project",
            "Relevant file: src/providers/bedrock.rs",
            0.7,
            &[],
            "passive",
            "journal-candidate",
        )?;
        write_memory_event(
            "proj",
            "constraint",
            "project",
            "Never guess AWS response shape",
            0.8,
            &[],
            "explicit",
            "user",
        )?;

        let brief = relevant_brief("proj", "check src/providers/bedrock.rs streaming", 5)?;
        assert!(
            brief
                .text
                .contains("Relevant file: src/providers/bedrock.rs")
        );
        assert!(!brief.ids.is_empty());

        accept_selected_memories(&brief.ids, "s-brief", Some("e-1"), "bedrock.rs streaming")?;

        let usage_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_events_usage WHERE usage_kind IN ('selected', 'accepted')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(usage_count, (brief.ids.len() * 2) as i64);
        let reinforced: (i64, f64) = conn.query_row(
            "SELECT reinforcement_count, confidence FROM memory_events
              WHERE summary = 'Relevant file: src/providers/bedrock.rs'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert!(reinforced.0 >= 1);
        assert!(reinforced.1 > 0.7);
        Ok(())
    })
}
