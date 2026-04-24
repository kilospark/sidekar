use super::*;

fn fresh_test_db_path() -> PathBuf {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    env::temp_dir().join(format!(
        "sidekar-broker-test-{}.sqlite3",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

fn with_test_db<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = crate::test_home_lock()
        .lock()
        .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
    let old_home = env::var_os("HOME");
    let temp_home = env::temp_dir().join(format!(
        "sidekar-broker-home-{}",
        fresh_test_db_path()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("tmp")
    ));
    fs::create_dir_all(&temp_home)?;
    // Safety: tests run in-process and this helper restores HOME before returning.
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
fn persists_pending_and_outbound() -> Result<()> {
    with_test_db(|| {
        let sender = AgentId {
            name: "sender".into(),
            nick: Some("borzoi".into()),
            session: Some("sess".into()),
            pane: Some("0:0.1".into()),
            agent_type: Some("sidekar".into()),
        };
        register_agent(&sender, Some("%1"))?;

        let envelope = Envelope::new_request(sender.clone(), "receiver", "hello");
        set_pending(&envelope)?;
        set_outbound_request(
            &envelope,
            &sender.display_name(),
            "broker",
            "receiver",
            sender.session.as_deref(),
            Some("/tmp/project"),
        )?;

        let pending = pending_for_agent("receiver")?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, envelope.id);

        let outbound = outbound_for_sender("sender")?;
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0].msg_id, envelope.id);
        assert_eq!(outbound[0].status, OUTBOUND_STATUS_OPEN);
        assert_eq!(outbound[0].kind, "request");

        let reply = Envelope::new_response(
            AgentId::new("receiver"),
            "sender",
            "done",
            envelope.id.clone(),
        );
        record_reply(&envelope.id, &reply)?;

        assert!(pending_for_agent("receiver")?.is_empty());
        assert!(outbound_for_sender("sender")?.is_empty());

        let stored = outbound_request(&envelope.id)?.context("missing outbound request")?;
        assert_eq!(stored.status, OUTBOUND_STATUS_ANSWERED);
        assert_eq!(stored.answered_at, Some(reply.created_at));

        let replies = replies_for_request(&envelope.id)?;
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].reply_msg_id, reply.id);
        assert_eq!(replies[0].message, "done");
        Ok(())
    })
}

#[test]
fn marks_outbound_timeouts_without_deleting_history() -> Result<()> {
    with_test_db(|| {
        let sender = AgentId {
            name: "sender".into(),
            nick: Some("borzoi".into()),
            session: Some("sess".into()),
            pane: Some("0:0.1".into()),
            agent_type: Some("sidekar".into()),
        };
        let envelope = Envelope::new_request(sender.clone(), "receiver", "hello");
        set_outbound_request(
            &envelope,
            &sender.display_name(),
            "broker",
            "receiver",
            sender.session.as_deref(),
            Some("/tmp/project"),
        )?;

        mark_outbound_timed_out(&envelope.id, envelope.created_at + 60)?;

        let open = outbound_for_sender("sender")?;
        assert!(open.is_empty());

        let timed_out =
            list_outbound_requests_for_sender("sender", Some(OUTBOUND_STATUS_TIMED_OUT), 10)?;
        assert_eq!(timed_out.len(), 1);
        assert_eq!(timed_out[0].msg_id, envelope.id);
        assert_eq!(timed_out[0].timed_out_at, Some(envelope.created_at + 60));
        Ok(())
    })
}

#[test]
fn persists_agent_sessions_and_updates_counters() -> Result<()> {
    with_test_db(|| {
        let started_at = 1_700_000_000u64;
        create_agent_session(
            "pty:123:1700000000",
            "sender",
            Some("claude"),
            Some("borzoi"),
            "/tmp/project",
            Some("/tmp/project"),
            Some("/tmp/project"),
            started_at,
        )?;

        let sender = AgentId {
            name: "sender".into(),
            nick: Some("borzoi".into()),
            session: Some("/tmp/project".into()),
            pane: Some("0:0.1".into()),
            agent_type: Some("sidekar".into()),
        };
        let envelope = Envelope::new_request(sender.clone(), "receiver", "hello");
        set_outbound_request(
            &envelope,
            &sender.display_name(),
            "broker",
            "receiver",
            sender.session.as_deref(),
            Some("/tmp/project"),
        )?;
        mark_agent_session_request(&sender.name, &envelope.id, envelope.created_at)?;

        let reply = Envelope::new_response(
            AgentId::new("receiver"),
            "sender",
            "done",
            envelope.id.clone(),
        );
        record_reply(&envelope.id, &reply)?;
        finish_agent_session("pty:123:1700000000", reply.created_at + 10)?;

        let sessions = list_agent_sessions(false, Some("/tmp/project"), 10)?;
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.agent_name, "sender");
        assert_eq!(session.agent_type.as_deref(), Some("claude"));
        assert_eq!(session.request_count, 1);
        assert_eq!(session.reply_count, 1);
        assert_eq!(session.message_count, 2);
        assert_eq!(
            session.last_request_msg_id.as_deref(),
            Some(envelope.id.as_str())
        );
        assert_eq!(
            session.last_reply_msg_id.as_deref(),
            Some(reply.id.as_str())
        );
        assert_eq!(session.ended_at, Some(reply.created_at + 10));

        let fetched = get_agent_session("pty:123:1700000000")?.context("missing session")?;
        assert_eq!(fetched.id, "pty:123:1700000000");
        Ok(())
    })
}

#[test]
fn agent_session_display_name_and_notes_are_persisted() -> Result<()> {
    with_test_db(|| {
        create_agent_session(
            "pty:321:1700000000",
            "sender",
            Some("codex"),
            Some("otter"),
            "/tmp/project",
            Some("/tmp/project"),
            Some("/tmp/project"),
            1_700_000_000,
        )?;

        assert!(set_agent_session_display_name(
            "pty:321:1700000000",
            Some("Review worker")
        )?);
        assert!(set_agent_session_notes(
            "pty:321:1700000000",
            Some("Owned the PR review thread")
        )?);

        let session = get_agent_session("pty:321:1700000000")?.context("missing session")?;
        assert_eq!(session.display_name.as_deref(), Some("Review worker"));
        assert_eq!(session.notes.as_deref(), Some("Owned the PR review thread"));

        assert!(set_agent_session_notes("pty:321:1700000000", None)?);
        let cleared = get_agent_session("pty:321:1700000000")?.context("missing session")?;
        assert_eq!(cleared.notes, None);
        Ok(())
    })
}

/// Schema smoke tests for the journaling tables. These protect
/// against future refactors silently dropping the migration or
/// changing a column's type — both are mistakes that would only
/// surface the first time a user enabled journaling, long after
/// the PR landed.
///
/// We deliberately don't call the store layer here (it doesn't
/// exist yet at this commit); we poke the tables with raw SQL so
/// the test stays valid even if later commits rearrange helpers.
#[test]
fn schema_migration_creates_session_journals() -> Result<()> {
    with_test_db(|| {
        init_db()?;
        let conn = open()?;

        // PRAGMA user_version should be current.
        let v: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        assert_eq!(v, SCHEMA_VERSION, "schema version not applied");

        // Seed a parent repl_sessions row so the FK is satisfiable.
        let sess_id = "test-session-001";
        conn.execute(
            "INSERT INTO repl_sessions (id, cwd, created_at, updated_at)
             VALUES (?1, '/tmp/t', 0.0, 0.0)",
            [sess_id],
        )?;

        // Full-column insert exercises every NOT NULL and default.
        conn.execute(
            "INSERT INTO session_journals (
                 session_id, project, created_at, from_entry_id,
                 to_entry_id, structured_json, headline, model_used,
                 cred_used
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                sess_id,
                "/tmp/t",
                1_700_000_000.0_f64,
                "e-1",
                "e-2",
                r#"{"summary":"x"}"#,
                "smoke headline",
                "test-model",
                "test-cred",
            ],
        )?;

        // Defaults: tokens_in/out=0, previous_id=NULL.
        let (tin, tout, prev): (i64, i64, Option<i64>) = conn.query_row(
            "SELECT tokens_in, tokens_out, previous_id
               FROM session_journals WHERE session_id = ?1",
            [sess_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!(tin, 0);
        assert_eq!(tout, 0);
        assert!(prev.is_none());

        Ok(())
    })
}

#[test]
fn schema_migration_creates_memory_journal_support() -> Result<()> {
    with_test_db(|| {
        init_db()?;
        let conn = open()?;

        // Seed a parent row in each referenced table so the composite
        // FK is satisfiable.
        conn.execute(
            "INSERT INTO repl_sessions (id, cwd, created_at, updated_at)
             VALUES ('s-1', '/tmp/t', 0.0, 0.0)",
            [],
        )?;
        conn.execute(
            "INSERT INTO session_journals (
                 session_id, project, created_at, from_entry_id,
                 to_entry_id, structured_json, headline, model_used,
                 cred_used
             ) VALUES ('s-1', '/tmp/t', 0.0, 'a', 'b', '{}', 'h', 'm', 'c')",
            [],
        )?;
        let journal_id: i64 = conn.query_row(
            "SELECT id FROM session_journals WHERE session_id = 's-1'",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO memory_events (
                 project, event_type, scope, summary, summary_norm,
                 confidence, created_at, updated_at
             ) VALUES ('/tmp/t', 'constraint', 'project',
                       'test', 'test', 0.4, 0, 0)",
            [],
        )?;
        let memory_id: i64 = conn.query_row(
            "SELECT id FROM memory_events WHERE summary = 'test'",
            [],
            |r| r.get(0),
        )?;

        conn.execute(
            "INSERT INTO memory_journal_support (memory_id, journal_id, created_at)
             VALUES (?1, ?2, 0.0)",
            rusqlite::params![memory_id, journal_id],
        )?;

        // Composite PK prevents duplicate links.
        let dup = conn.execute(
            "INSERT INTO memory_journal_support (memory_id, journal_id, created_at)
             VALUES (?1, ?2, 0.0)",
            rusqlite::params![memory_id, journal_id],
        );
        assert!(dup.is_err(), "composite PK should reject duplicate link");

        Ok(())
    })
}

#[test]
fn cancel_outbound_request_flips_open_to_cancelled_and_clears_pending() -> Result<()> {
    with_test_db(|| {
        let sender = AgentId {
            name: "sender".into(),
            nick: Some("borzoi".into()),
            session: Some("sess".into()),
            pane: Some("0:0.1".into()),
            agent_type: Some("sidekar".into()),
        };
        let envelope = Envelope::new_request(sender.clone(), "receiver", "hello");
        set_pending(&envelope)?;
        set_outbound_request(
            &envelope,
            &sender.display_name(),
            "broker",
            "receiver",
            sender.session.as_deref(),
            Some("/tmp/project"),
        )?;

        let updated = cancel_outbound_request(&envelope.id, envelope.created_at + 30)?;
        assert_eq!(updated, 1, "open request should be cancelled exactly once");

        // Row remains in history, but status flipped and pending cleared.
        assert!(
            outbound_for_sender("sender")?.is_empty(),
            "cancelled requests should not appear as open"
        );
        let stored = outbound_request(&envelope.id)?.context("missing outbound")?;
        assert_eq!(stored.status, OUTBOUND_STATUS_CANCELLED);
        assert_eq!(stored.closed_at, Some(envelope.created_at + 30));

        assert!(
            pending_for_agent("receiver")?.is_empty(),
            "pending row must be removed so nudger stops"
        );

        // Re-cancelling is a no-op (not open anymore).
        let again = cancel_outbound_request(&envelope.id, envelope.created_at + 60)?;
        assert_eq!(again, 0);
        Ok(())
    })
}

#[test]
fn cancel_all_outbound_for_sender_only_touches_open_rows() -> Result<()> {
    with_test_db(|| {
        let sender = AgentId {
            name: "sender".into(),
            nick: Some("borzoi".into()),
            session: Some("sess".into()),
            pane: Some("0:0.1".into()),
            agent_type: Some("sidekar".into()),
        };
        let other = AgentId {
            name: "other".into(),
            nick: None,
            session: Some("sess".into()),
            pane: Some("0:0.2".into()),
            agent_type: Some("sidekar".into()),
        };

        // Two open requests from `sender`, one answered, one from another agent.
        let r1 = Envelope::new_request(sender.clone(), "r1", "hi 1");
        let r2 = Envelope::new_request(sender.clone(), "r2", "hi 2");
        let r3 = Envelope::new_request(sender.clone(), "r3", "hi 3");
        let r4 = Envelope::new_request(other.clone(), "r4", "from other");
        for (env, owner) in [
            (&r1, &sender),
            (&r2, &sender),
            (&r3, &sender),
            (&r4, &other),
        ] {
            set_pending(env)?;
            set_outbound_request(
                env,
                &owner.display_name(),
                "broker",
                &env.to,
                owner.session.as_deref(),
                Some("/tmp/project"),
            )?;
        }

        // Answer r2 so it's no longer open.
        let reply = Envelope::new_response(
            AgentId::new("r2"),
            "sender",
            "ok",
            r2.id.clone(),
        );
        record_reply(&r2.id, &reply)?;

        let cancelled = cancel_all_outbound_for_sender("sender", r1.created_at + 5)?;
        let mut got: Vec<String> = cancelled.clone();
        got.sort();
        let mut want = vec![r1.id.clone(), r3.id.clone()];
        want.sort();
        assert_eq!(got, want, "only open rows owned by sender should flip");

        // Answered row untouched.
        let r2_stored = outbound_request(&r2.id)?.context("r2")?;
        assert_eq!(r2_stored.status, OUTBOUND_STATUS_ANSWERED);

        // Other agent's row untouched.
        let r4_stored = outbound_request(&r4.id)?.context("r4")?;
        assert_eq!(r4_stored.status, OUTBOUND_STATUS_OPEN);
        assert!(!pending_for_agent("r4")?.is_empty());

        // Pending rows for cancelled requests removed.
        assert!(pending_for_agent("r1")?.is_empty());
        assert!(pending_for_agent("r3")?.is_empty());

        // Second call with nothing open returns empty.
        let again = cancel_all_outbound_for_sender("sender", r1.created_at + 10)?;
        assert!(again.is_empty());

        Ok(())
    })
}
