use super::*;
use crate::message::AgentId;

const UID: &str = "acct-1";
const KEY: [u8; 32] = [9u8; 32];

/// A scratch database, logged out of nothing but holding the account key, as
/// the daemon does mid-round.
fn with_db<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let _home = crate::ScratchHome::new();
    clear_encryption_key();
    set_encryption_key(KEY.to_vec());
    let conn = open()?;
    let out = f(&conn);
    clear_encryption_key();
    out
}

fn register(name: &str, pane: &str) -> AgentId {
    let id = AgentId {
        name: name.into(),
        nick: Some(format!("{name}-nick")),
        session: Some("/src/app".into()),
        pane: Some(pane.into()),
        agent_type: Some("claude".into()),
    };
    register_agent(&id, Some(pane)).unwrap();
    id
}

fn dirty_rows(conn: &Connection, kind: &str) -> Vec<(String, bool)> {
    let mut stmt = conn
        .prepare("SELECT record_id, deleted FROM sync_state WHERE kind = ?1 AND dirty = 1 ORDER BY record_id")
        .unwrap();
    stmt.query_map(params![kind], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn clear_dirty(conn: &Connection) {
    conn.execute("UPDATE sync_state SET dirty = 0", []).unwrap();
}

/// What a pushed record looks like to a pulling machine.
fn sealed(conn: &Connection, kind: &str, record_id: &str) -> String {
    let plain = sync_payload(conn, kind, record_id).unwrap();
    crate::broker::encryption::sync_encrypt(&KEY, &plain).unwrap()
}

#[test]
fn only_long_lived_registrations_are_published() {
    assert!(publishes(Some("pty-123")));
    assert!(publishes(Some("session-9")));
    assert!(
        !publishes(Some("cli-123")),
        "one command, gone before anyone could address it"
    );
    assert!(!publishes(None));
}

#[test]
fn presence_follows_the_agents_registered_here() -> Result<()> {
    with_db(|conn| {
        register("claude-app-1", "pty-111");
        register("cli-app-1", "cli-222");

        reconcile_presence(conn, UID, "devA")?;
        assert_eq!(
            dirty_rows(conn, KIND_AGENT),
            [(agent_record_id("devA", "claude-app-1"), false)],
            "the agent is published; the one-shot command is not"
        );

        clear_dirty(conn);
        reconcile_presence(conn, UID, "devA")?;
        assert!(dirty_rows(conn, KIND_AGENT).is_empty(), "nothing due yet");

        // A heartbeat is due once HEARTBEAT_SECS have passed.
        conn.execute(
            "UPDATE sync_state SET updated_at = updated_at - ?1",
            params![HEARTBEAT_SECS],
        )?;
        reconcile_presence(conn, UID, "devA")?;
        assert_eq!(dirty_rows(conn, KIND_AGENT).len(), 1, "republished");

        clear_dirty(conn);
        unregister_agent("claude-app-1")?;
        reconcile_presence(conn, UID, "devA")?;
        assert_eq!(
            dirty_rows(conn, KIND_AGENT),
            [(agent_record_id("devA", "claude-app-1"), true)],
            "an agent that left is tombstoned"
        );
        Ok(())
    })
}

#[test]
fn another_machines_agents_are_listed_until_they_go_quiet() -> Result<()> {
    with_db(|conn| {
        register("claude-app-1", "pty-111");
        let rid = agent_record_id("devA", "claude-app-1");
        let ciphertext = sealed(conn, KIND_AGENT, &rid);

        // Pulled by machine B.
        assert!(apply_record(
            conn,
            UID,
            "devB",
            KIND_AGENT,
            &rid,
            &ciphertext,
            1,
            false
        )?);
        let agents = live_remote_agents(conn, UID)?;
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "claude-app-1");
        assert_eq!(agents[0].device_id, "devA");
        assert_eq!(agents[0].nick.as_deref(), Some("claude-app-1-nick"));

        // Machine A ignores its own record when it comes back.
        conn.execute("DELETE FROM remote_agents", [])?;
        assert!(!apply_record(
            conn,
            UID,
            "devA",
            KIND_AGENT,
            &rid,
            &ciphertext,
            1,
            false
        )?);
        assert!(live_remote_agents(conn, UID)?.is_empty());

        // An older version never overwrites a newer one.
        apply_record(conn, UID, "devB", KIND_AGENT, &rid, &ciphertext, 5, false)?;
        assert!(!apply_record(
            conn,
            UID,
            "devB",
            KIND_AGENT,
            &rid,
            &ciphertext,
            4,
            false
        )?);

        // Silent past the TTL: gone from the list, without a tombstone.
        conn.execute(
            "UPDATE remote_agents SET published_at = published_at - ?1",
            params![PRESENCE_TTL_SECS + 1],
        )?;
        assert!(live_remote_agents(conn, UID)?.is_empty());

        // A tombstone removes it outright.
        conn.execute(
            "UPDATE remote_agents SET published_at = ?1",
            params![crate::message::epoch_secs() as i64],
        )?;
        assert!(apply_record(
            conn, UID, "devB", KIND_AGENT, &rid, "", 6, true
        )?);
        assert!(live_remote_agents(conn, UID)?.is_empty());
        Ok(())
    })
}

fn insert_remote(conn: &Connection, device: &str, host: &str, name: &str, nick: &str) {
    conn.execute(
        "INSERT INTO remote_agents (record_id, user_id, device_id, hostname, name, nick, published_at, version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
        params![
            agent_record_id(device, name),
            UID,
            device,
            host,
            name,
            nick,
            crate::message::epoch_secs() as i64
        ],
    )
    .unwrap();
}

#[test]
fn a_remote_agent_is_found_by_name_nick_or_name_at_host() -> Result<()> {
    with_db(|conn| {
        insert_remote(conn, "devA", "studio.local", "claude-app-1", "otter");
        assert_eq!(
            find_remote_agent(conn, UID, "claude-app-1")?
                .unwrap()
                .device_id,
            "devA"
        );
        assert_eq!(
            find_remote_agent(conn, UID, "otter")?.unwrap().device_id,
            "devA"
        );
        assert!(find_remote_agent(conn, UID, "nobody")?.is_none());

        // The same project on two machines: the host picks one.
        insert_remote(conn, "devC", "laptop", "claude-app-1", "heron");
        let err = find_remote_agent(conn, UID, "claude-app-1")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("claude-app-1@studio.local") && err.contains("claude-app-1@laptop"),
            "{err}"
        );
        assert_eq!(
            find_remote_agent(conn, UID, "claude-app-1@laptop")?
                .unwrap()
                .device_id,
            "devC"
        );
        assert_eq!(
            find_remote_agent(conn, UID, "claude-app-1@studio")?
                .unwrap()
                .device_id,
            "devA"
        );
        Ok(())
    })
}

#[test]
fn a_message_reaches_its_recipient_once_and_is_tombstoned() -> Result<()> {
    with_db(|conn| {
        register("claude-app-1", "pty-111");
        let asker = AgentId::new("codex-other-1");
        let request = Envelope::new_request(asker, "claude-app-1", "review the diff");
        let id = queue_remote_message(
            conn,
            UID,
            "devB",
            "devA",
            "claude-app-1",
            "codex-other-1",
            "[from codex] review the diff",
            Some(&request),
        )?;
        assert_eq!(id, request.id, "named by the message id");
        let ciphertext = sealed(conn, KIND_BUS, &id);
        conn.execute("DELETE FROM sync_state", [])?; // as machine A, which never queued it

        // Not for machine C.
        assert!(!apply_record(
            conn,
            UID,
            "devC",
            KIND_BUS,
            &id,
            &ciphertext,
            1,
            false
        )?);
        assert!(list_queued_messages("claude-app-1")?.is_empty());

        assert!(apply_record(
            conn,
            UID,
            "devA",
            KIND_BUS,
            &id,
            &ciphertext,
            1,
            false
        )?);
        let queued = list_queued_messages("claude-app-1")?;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].body, "[from codex] review the diff");
        assert!(
            pending_message(&id)?.is_some(),
            "a request waits for its answer"
        );
        assert_eq!(
            origin_device(conn, &id)?.as_deref(),
            Some("devB"),
            "and remembers who asked"
        );
        assert_eq!(
            dirty_rows(conn, KIND_BUS),
            [(id.clone(), true)],
            "tombstoned for push"
        );

        // The same record pulled again is not delivered twice.
        assert!(!apply_record(
            conn,
            UID,
            "devA",
            KIND_BUS,
            &id,
            &ciphertext,
            1,
            false
        )?);
        assert_eq!(list_queued_messages("claude-app-1")?.len(), 1);
        Ok(())
    })
}

#[test]
fn an_answer_is_recorded_for_bus_await_even_with_its_asker_gone() -> Result<()> {
    with_db(|conn| {
        let asker = AgentId::new("cli-app-9");
        let request = Envelope::new_request(asker, "claude-app-1", "review the diff");
        let answer = Envelope::new_response(
            AgentId::new("claude-app-1"),
            "cli-app-9",
            "looks fine",
            request.id.clone(),
        );
        let id = queue_remote_message(
            conn,
            UID,
            "devA",
            "devB",
            "cli-app-9",
            "claude-app-1",
            "looks fine",
            Some(&answer),
        )?;
        let ciphertext = sealed(conn, KIND_BUS, &id);
        conn.execute("DELETE FROM sync_state", [])?;

        // The one-shot asker is not registered on machine B any more.
        apply_record(conn, UID, "devB", KIND_BUS, &id, &ciphertext, 1, false)?;
        let replies = replies_for_request(&request.id)?;
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].message, "looks fine");
        assert!(
            list_queued_messages("cli-app-9")?.is_empty(),
            "nothing queued under a name nobody holds"
        );
        Ok(())
    })
}

#[test]
fn a_pushed_message_leaves_the_outbox_and_old_bookkeeping_is_pruned() -> Result<()> {
    with_db(|conn| {
        let id = queue_remote_message(conn, UID, "devB", "devA", "x", "y", "hi", None)?;
        pushed(conn, KIND_BUS, &id)?;
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM bus_outbox", [], |r| r.get(0))?;
        assert_eq!(left, 0);

        // A message that could not be pushed for a week is given up on.
        let stuck = queue_remote_message(conn, UID, "devB", "devA", "x", "y", "hi", None)?;
        conn.execute(
            "UPDATE bus_outbox SET created_at = created_at - ?1",
            params![KEEP_SECS + 1],
        )?;
        insert_remote(conn, "devA", "studio", "old-agent", "n");
        conn.execute(
            "UPDATE remote_agents SET published_at = published_at - 90000",
            [],
        )?;
        prune(conn)?;
        assert!(dirty_rows(conn, KIND_BUS).iter().all(|(r, _)| *r != stuck));
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM bus_outbox", [], |r| r.get(0))?;
        assert_eq!(rows, 0);
        let agents: i64 = conn.query_row("SELECT COUNT(*) FROM remote_agents", [], |r| r.get(0))?;
        assert_eq!(agents, 0);
        Ok(())
    })
}

#[test]
fn two_pulls_racing_on_one_message_deliver_it_once() -> Result<()> {
    with_db(|conn| {
        register("claude-app-1", "pty-111");
        let request = Envelope::new_request(AgentId::new("codex-1"), "claude-app-1", "hi");
        let id = queue_remote_message(
            conn,
            UID,
            "devB",
            "devA",
            "claude-app-1",
            "codex-1",
            "hi",
            Some(&request),
        )?;
        let ciphertext = sealed(conn, KIND_BUS, &id);
        conn.execute("DELETE FROM sync_state", [])?;
        conn.execute("DELETE FROM bus_outbox", [])?;

        // As the daemon's round and a `bus await` would: separate connections,
        // at the same moment.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let racers: Vec<_> = (0..4)
            .map(|_| {
                let (id, ciphertext, barrier) = (id.clone(), ciphertext.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let conn = open().unwrap();
                    barrier.wait();
                    apply_record(&conn, UID, "devA", KIND_BUS, &id, &ciphertext, 1, false).unwrap()
                })
            })
            .collect();
        let delivered = racers
            .into_iter()
            .map(|r| r.join().unwrap_or(false))
            .filter(|d| *d)
            .count();
        assert_eq!(delivered, 1, "exactly one puller claims it");
        assert_eq!(list_queued_messages("claude-app-1")?.len(), 1);
        assert_eq!(dirty_rows(conn, KIND_BUS), [(id, true)]);
        Ok(())
    })
}

#[test]
fn a_message_that_fails_to_deliver_is_released_for_a_later_pull() -> Result<()> {
    with_db(|conn| {
        let mut payload: MessagePayload = serde_json::from_str(&{
            queue_remote_message(conn, UID, "devB", "devA", "x", "y", "hi", None)?;
            conn.query_row("SELECT payload FROM bus_outbox", [], |r| {
                r.get::<_, String>(0)
            })?
        })?;
        payload.envelope_json = Some("not an envelope".into());
        let ciphertext =
            crate::broker::encryption::sync_encrypt(&KEY, &serde_json::to_string(&payload)?)?;
        conn.execute("DELETE FROM sync_state", [])?;

        assert!(apply_record(conn, UID, "devA", KIND_BUS, "m1", &ciphertext, 1, false).is_err());
        assert!(
            dirty_rows(conn, KIND_BUS).is_empty(),
            "the claim is let go, not kept"
        );
        Ok(())
    })
}

#[test]
fn a_sender_clock_running_ahead_does_not_keep_its_agents_listed() -> Result<()> {
    with_db(|conn| {
        register("claude-app-1", "pty-111");
        let rid = agent_record_id("devA", "claude-app-1");
        let mut a: AgentPayload = serde_json::from_str(&sync_payload(conn, KIND_AGENT, &rid)?)?;
        a.published_at += 3600; // an hour fast
        let ciphertext =
            crate::broker::encryption::sync_encrypt(&KEY, &serde_json::to_string(&a)?)?;
        apply_record(conn, UID, "devB", KIND_AGENT, &rid, &ciphertext, 1, false)?;
        let stored: i64 =
            conn.query_row("SELECT published_at FROM remote_agents", [], |r| r.get(0))?;
        assert!(
            stored <= crate::message::epoch_secs() as i64,
            "taken as now, not an hour ahead"
        );
        Ok(())
    })
}

#[test]
fn machines_sharing_a_host_name_are_told_apart_by_device_id() -> Result<()> {
    with_db(|conn| {
        insert_remote(
            conn,
            "Qx7kPa2m+/abc",
            "MacBook-Pro.local",
            "claude-app-1",
            "a",
        );
        insert_remote(
            conn,
            "Zr4tLm9w+/def",
            "MacBook-Pro.local",
            "claude-app-1",
            "b",
        );
        let err = find_remote_agent(conn, UID, "claude-app-1@MacBook-Pro")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("claude-app-1@Qx7kPa2m") && err.contains("claude-app-1@Zr4tLm9w"),
            "{err}"
        );
        let found = find_remote_agent(conn, UID, "claude-app-1@Zr4tLm9w")?.unwrap();
        assert_eq!(found.device_id, "Zr4tLm9w+/def");
        Ok(())
    })
}
