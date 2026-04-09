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
