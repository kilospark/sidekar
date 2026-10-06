use super::*;

/// HOME pointed at a fresh scratch dir, with nobody logged in. The current
/// user id is process-global, so it is reset on the way in and out; otherwise
/// one test's login would leak into the next.
fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    // Restores HOME and removes the directory even if `f` panics.
    let _home = crate::ScratchHome::new();
    crate::broker::clear_current_user_id();
    let result = f();
    crate::broker::clear_current_user_id();
    result
}

fn write(summary: &str) -> Result<()> {
    write_memory_event(
        "alpha", "convention", "project", summary, 0.8, &[], "explicit", "user",
    )?;
    Ok(())
}

/// (id, uid, sync_owner) of the row with this summary.
fn row(conn: &rusqlite::Connection, summary: &str) -> Result<(i64, String, Option<String>)> {
    Ok(conn.query_row(
        "SELECT id, uid, sync_owner FROM memory_events WHERE summary = ?1",
        [summary],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

/// (version, dirty, deleted) of the memory's sync state for `account`.
fn sync_state(
    conn: &rusqlite::Connection,
    account: &str,
    uid: &str,
) -> Result<Option<(i64, bool, bool)>> {
    Ok(conn
        .query_row(
            "SELECT version, dirty, deleted FROM sync_state
              WHERE user_id = ?1 AND kind = 'memory' AND record_id = ?2",
            params![account, uid],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, i64>(1)? != 0,
                    r.get::<_, i64>(2)? != 0,
                ))
            },
        )
        .optional()?)
}

fn sync_state_count(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM sync_state WHERE kind = 'memory'",
        [],
        |r| r.get(0),
    )?)
}

#[test]
fn a_write_while_logged_in_gets_a_uid_an_owner_and_a_pending_push() -> Result<()> {
    with_test_home(|| {
        crate::broker::set_current_user_id("acct-a".to_string());
        write("Use Readability.js before scraping")?;

        let conn = crate::broker::open_db()?;
        let (_, uid, owner) = row(&conn, "Use Readability.js before scraping")?;
        assert_eq!(uid.len(), 32, "a 128-bit hex uid");
        assert_eq!(owner.as_deref(), Some("acct-a"));
        assert_eq!(sync_state(&conn, "acct-a", &uid)?, Some((1, true, false)));
        Ok(())
    })
}

#[test]
fn a_write_while_logged_out_waits_unowned_until_an_account_claims_it() -> Result<()> {
    with_test_home(|| {
        write("Prefer ripgrep over grep")?;
        let conn = crate::broker::open_db()?;
        let (_, uid, owner) = row(&conn, "Prefer ripgrep over grep")?;
        assert_eq!(owner, None);
        assert_eq!(sync_state_count(&conn)?, 0, "nothing queued with nobody logged in");

        assert_eq!(claim_unowned(&conn, "acct-a")?, vec![uid.clone()]);
        assert_eq!(row(&conn, "Prefer ripgrep over grep")?.2.as_deref(), Some("acct-a"));
        assert!(claim_unowned(&conn, "acct-b")?.is_empty(), "already claimed");
        assert_eq!(owned_uids(&conn, "acct-a")?, vec![uid]);
        assert!(owned_uids(&conn, "acct-b")?.is_empty());
        Ok(())
    })
}

#[test]
fn a_change_uploads_to_the_rows_owner_not_whoever_is_logged_in() -> Result<()> {
    with_test_home(|| {
        crate::broker::set_current_user_id("acct-a".to_string());
        write("Run tests before release")?;
        let conn = crate::broker::open_db()?;
        let (id, uid, _) = row(&conn, "Run tests before release")?;

        // Logged into another account, the edit still belongs to acct-a.
        touch_as(&conn, id, Some("acct-b"))?;
        assert_eq!(sync_state(&conn, "acct-a", &uid)?, Some((2, true, false)));
        assert_eq!(sync_state(&conn, "acct-b", &uid)?, None, "never copied into acct-b");
        assert_eq!(row(&conn, "Run tests before release")?.2.as_deref(), Some("acct-a"));
        Ok(())
    })
}

#[test]
fn dedup_queues_a_push_only_when_confidence_actually_changes() -> Result<()> {
    with_test_home(|| {
        crate::broker::set_current_user_id("acct-a".to_string());
        let summary = "Pin the Rust toolchain";
        write_memory_event("alpha", "convention", "project", summary, 1.0, &[], "explicit", "user")?;
        let conn = crate::broker::open_db()?;
        let (_, uid, _) = row(&conn, summary)?;
        conn.execute("UPDATE sync_state SET dirty = 0 WHERE record_id = ?1", [&uid])?;

        // Already at 1.0: re-learning it changes nothing worth uploading.
        write_memory_event("alpha", "convention", "project", summary, 1.0, &[], "explicit", "user")?;
        assert_eq!(sync_state(&conn, "acct-a", &uid)?, Some((1, false, false)));
        Ok(())
    })
}

#[test]
fn deleting_a_synced_memory_leaves_a_tombstone_for_its_owner() -> Result<()> {
    with_test_home(|| {
        crate::broker::set_current_user_id("acct-a".to_string());
        write("Archive summaries at session end")?;
        let conn = crate::broker::open_db()?;
        let (id, uid, _) = row(&conn, "Archive summaries at session end")?;

        assert!(delete_memory(&conn, id)?);
        assert_eq!(sync_state(&conn, "acct-a", &uid)?, Some((2, true, true)));
        assert!(!delete_memory(&conn, id)?, "already gone");
        Ok(())
    })
}

#[test]
fn deleting_a_memory_that_never_synced_tells_nobody() -> Result<()> {
    with_test_home(|| {
        write("A local-only note")?;
        let conn = crate::broker::open_db()?;
        let (id, _, _) = row(&conn, "A local-only note")?;
        assert!(delete_memory(&conn, id)?);
        assert_eq!(sync_state_count(&conn)?, 0);
        Ok(())
    })
}

#[test]
fn a_payload_round_trips_through_apply_on_another_device() -> Result<()> {
    with_test_home(|| {
        crate::broker::set_current_user_id("acct-a".to_string());
        write_memory_event(
            "alpha",
            "decision",
            "project",
            "Ship memory sync behind the account key",
            0.9,
            &["sync".to_string()],
            "explicit",
            "user",
        )?;
        let conn = crate::broker::open_db()?;
        let summary = "Ship memory sync behind the account key";
        let (_, uid, _) = row(&conn, summary)?;
        let created_at: i64 =
            conn.query_row("SELECT created_at FROM memory_events WHERE uid = ?1", [&uid], |r| {
                r.get(0)
            })?;
        let payload = sync_payload(&conn, &uid)?.expect("payload");

        // The other device: no such row, then the pulled record arrives.
        conn.execute("DELETE FROM memory_events WHERE uid = ?1", [&uid])?;
        conn.execute("DELETE FROM sync_state", [])?;
        apply_synced(&conn, "acct-a", &uid, &payload)?;

        let (event_type, owner, tags, norm, hash, created): (
            String,
            Option<String>,
            String,
            String,
            String,
            i64,
        ) = conn.query_row(
            "SELECT event_type, sync_owner, tags_json, summary_norm, summary_hash, created_at
               FROM memory_events WHERE uid = ?1",
            [&uid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
        assert_eq!(event_type, "decision");
        assert_eq!(owner.as_deref(), Some("acct-a"));
        assert!(tags.contains("sync"));
        assert_eq!(norm, normalize_summary(summary), "derived columns are recomputed");
        assert_eq!(hash, summary_hash(summary));
        assert_eq!(created, created_at, "keeps when it was first written");
        assert_eq!(sync_state_count(&conn)?, 0, "applying a pulled record does not echo it back");

        let hits = search_events(
            "account key",
            crate::scope::ScopeView::Project,
            Some("alpha"),
            None,
            5,
        )?;
        assert_eq!(hits.len(), 1, "searchable on arrival");
        Ok(())
    })
}

#[test]
fn apply_updates_content_but_keeps_local_reinforcement_and_supersede_links() -> Result<()> {
    with_test_home(|| {
        crate::broker::set_current_user_id("acct-a".to_string());
        write("Original wording")?;
        let conn = crate::broker::open_db()?;
        let (_, uid, _) = row(&conn, "Original wording")?;
        conn.execute(
            "UPDATE memory_events SET reinforcement_count = 7, superseded_by = 999 WHERE uid = ?1",
            [&uid],
        )?;

        let mut payload: serde_json::Value =
            serde_json::from_str(&sync_payload(&conn, &uid)?.expect("payload"))?;
        payload["summary"] = "Revised wording".into();
        payload["confidence"] = 0.4.into();
        payload["reinforcement_count"] = 1.into();
        apply_synced(&conn, "acct-a", &uid, &payload.to_string())?;

        let (summary, confidence, reinforced, superseded_by): (String, f64, i64, Option<i64>) = conn
            .query_row(
                "SELECT summary, confidence, reinforcement_count, superseded_by
                   FROM memory_events WHERE uid = ?1",
                [&uid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
        assert_eq!(summary, "Revised wording");
        assert!((confidence - 0.4).abs() < 1e-9);
        assert_eq!(reinforced, 7, "local relevance signal kept");
        assert_eq!(superseded_by, Some(999), "local supersede link kept");
        Ok(())
    })
}

#[test]
fn a_pulled_record_never_overwrites_another_accounts_memory() -> Result<()> {
    with_test_home(|| {
        crate::broker::set_current_user_id("acct-b".to_string());
        write("Belongs to b")?;
        let conn = crate::broker::open_db()?;
        let (_, uid, _) = row(&conn, "Belongs to b")?;

        let mut payload: serde_json::Value =
            serde_json::from_str(&sync_payload(&conn, &uid)?.expect("payload"))?;
        payload["summary"] = "Rewritten by a".into();
        apply_synced(&conn, "acct-a", &uid, &payload.to_string())?;
        assert_eq!(row(&conn, "Belongs to b")?.2.as_deref(), Some("acct-b"));

        delete_synced(&conn, "acct-a", &uid)?;
        assert!(row(&conn, "Belongs to b").is_ok(), "a's tombstone can't delete b's memory");
        delete_synced(&conn, "acct-b", &uid)?;
        assert!(row(&conn, "Belongs to b").is_err(), "b's tombstone does");
        Ok(())
    })
}

#[test]
fn an_archive_is_queued_for_its_owner() -> Result<()> {
    with_test_home(|| {
        crate::broker::set_current_user_id("acct-a".to_string());
        let id = write_session_archive("alpha", "project", Some("Login debug"), "body", "muse", &[])?;
        let conn = crate::broker::open_db()?;
        let uid: String =
            conn.query_row("SELECT uid FROM memory_events WHERE id = ?1", [id], |r| r.get(0))?;
        assert_eq!(sync_state(&conn, "acct-a", &uid)?, Some((1, true, false)));
        Ok(())
    })
}

#[test]
fn opening_the_database_gives_any_uid_less_row_a_uid() -> Result<()> {
    with_test_home(|| {
        let conn = crate::broker::open_db()?;
        conn.execute(
            "INSERT INTO memory_events (project, event_type, scope, summary, summary_norm,
                                        created_at, updated_at)
             VALUES ('alpha', 'convention', 'project', 'no uid yet', 'no uid yet', 0, 0)",
            [],
        )?;
        let before: Option<String> = conn.query_row(
            "SELECT uid FROM memory_events WHERE summary = 'no uid yet'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(before, None);

        let conn = crate::broker::open_db()?;
        let after: Option<String> = conn.query_row(
            "SELECT uid FROM memory_events WHERE summary = 'no uid yet'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(after.map(|u| u.len()), Some(32));
        Ok(())
    })
}
