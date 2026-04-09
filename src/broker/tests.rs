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
