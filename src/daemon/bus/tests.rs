use super::*;
use anyhow::{Result, anyhow};
use rand::RngCore;
use std::{env, ffi::OsString, fs, path::PathBuf, sync::MutexGuard, time::Duration};

fn temp_home() -> PathBuf {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    env::temp_dir().join(format!(
        "sidekar-daemon-bus-test-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

struct HomeGuard {
    _lock: MutexGuard<'static, ()>,
    old_home: Option<OsString>,
    home: PathBuf,
}

impl HomeGuard {
    fn new() -> Result<Self> {
        let lock = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
        let old_home = env::var_os("HOME");
        let home = temp_home();
        fs::create_dir_all(&home)?;
        unsafe { env::set_var("HOME", &home) };
        Ok(Self {
            _lock: lock,
            old_home,
            home,
        })
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.old_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

#[test]
fn stale_detach_does_not_remove_newer_client() {
    let (tx1, _rx1) = mpsc::unbounded_channel();
    let (tx2, _rx2) = mpsc::unbounded_channel();
    let mut bus = BusState::default();
    let old_id = bus.attach("agent".to_string(), tx1);
    let new_id = bus.attach("agent".to_string(), tx2);

    bus.detach("agent", old_id);
    assert_eq!(
        bus.clients.get("agent").map(|client| client.id),
        Some(new_id)
    );

    bus.detach("agent", new_id);
    assert!(!bus.clients.contains_key("agent"));
}

#[tokio::test(flavor = "current_thread")]
async fn deliver_once_sends_all_bus_message_types_and_releases_on_detach() -> Result<()> {
    let _home = HomeGuard::new()?;
    let request = crate::message::Envelope::new_request(
        crate::message::AgentId::new("sender"),
        "receiver",
        "need data",
    );
    let response = crate::message::Envelope::new_response(
        crate::message::AgentId::new("sender"),
        "receiver",
        "done",
        request.id.clone(),
    );
    let fyi = crate::message::Envelope::new_fyi(
        crate::message::AgentId::new("sender"),
        "receiver",
        "closed.",
    );
    crate::broker::enqueue_bus_message(
        "receiver",
        "sender",
        "[request from sender]: need data",
        false,
        Some(&request),
    )?;
    crate::broker::enqueue_bus_message(
        "receiver",
        "sender",
        "[response from sender]: done",
        false,
        Some(&response),
    )?;
    crate::broker::enqueue_bus_message(
        "receiver",
        "sender",
        "[fyi from sender]: closed.",
        false,
        Some(&fyi),
    )?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let bus_state: SharedBusState = Arc::new(Mutex::new(BusState::default()));
    let client_id = bus_state.lock().await.attach("receiver".to_string(), tx);
    deliver_once(&bus_state).await;

    let mut bodies = Vec::new();
    let mut interrupts = Vec::new();
    for _ in 0..3 {
        let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for daemon bus frame")
            .expect("daemon bus sender closed");
        interrupts.push(
            frame
                .get("interrupt")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        );
        bodies.push(
            frame
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
    }
    assert_eq!(
        bodies,
        vec![
            "[request from sender]: need data",
            "[response from sender]: done",
            "[fyi from sender]: closed.",
        ]
    );
    assert_eq!(interrupts, vec![false, false, false]);
    assert!(crate::broker::list_queued_messages("receiver")?.is_empty());

    {
        let bus = bus_state.lock().await;
        assert_eq!(bus.in_flight.len(), 3);
    }
    bus_state.lock().await.detach("receiver", client_id);
    assert_eq!(crate::broker::list_queued_messages("receiver")?.len(), 3);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn deliver_once_marks_interrupt_frame_from_envelope() -> Result<()> {
    let _home = HomeGuard::new()?;
    let mut envelope = crate::message::Envelope::new_fyi(
        crate::message::AgentId::new("sender"),
        "receiver",
        "now",
    );
    envelope.interrupt = true;
    crate::broker::enqueue_bus_message(
        "receiver",
        "sender",
        "[fyi from sender]: now",
        false,
        Some(&envelope),
    )?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let bus_state: SharedBusState = Arc::new(Mutex::new(BusState::default()));
    let _client_id = bus_state.lock().await.attach("receiver".to_string(), tx);
    deliver_once(&bus_state).await;

    let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for daemon bus frame")
        .expect("daemon bus sender closed");
    assert_eq!(frame.get("interrupt").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn bus_attach_socket_delivers_and_ack_deletes_queue_row() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let _home = HomeGuard::new()?;
    let (server, mut client) = tokio::net::UnixStream::pair()?;
    let state = Arc::new(Mutex::new(super::super::DaemonState::new()));
    let bus_state = state.lock().await.bus_state.clone();
    let server_task = tokio::spawn(super::super::handle_connection(server, state));

    client
        .write_all(b"{\"type\":\"bus_attach\",\"agent\":\"receiver\"}\n")
        .await?;
    client.flush().await?;
    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let attached = serde_json::from_str::<Value>(line.trim())?;
    assert_eq!(
        attached.get("type").and_then(Value::as_str),
        Some("bus_attached")
    );
    assert_eq!(attached.get("ok").and_then(Value::as_bool), Some(true));

    crate::broker::enqueue_bus_message(
        "receiver",
        "sender",
        "[response from sender]: ok",
        false,
        None,
    )?;
    deliver_once(&bus_state).await;

    line.clear();
    tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut line)).await??;
    let frame = serde_json::from_str::<Value>(line.trim())?;
    assert_eq!(
        frame.get("type").and_then(Value::as_str),
        Some("bus_message")
    );
    assert_eq!(
        frame.get("body").and_then(Value::as_str),
        Some("[response from sender]: ok")
    );
    let id = frame.get("id").and_then(Value::as_i64).expect("message id");

    let client = reader.get_mut();
    let ack = serde_json::json!({"type": "bus_ack", "id": id, "ok": true});
    client
        .write_all(serde_json::to_string(&ack)?.as_bytes())
        .await?;
    client.write_all(b"\n").await?;
    client.flush().await?;

    for _ in 0..20 {
        if crate::broker::list_queued_messages("receiver")?.is_empty() {
            server_task.abort();
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server_task.abort();
    anyhow::bail!("acked bus message remained queued");
}

#[tokio::test(flavor = "current_thread")]
async fn deliver_once_delivers_queued_nudge_messages() -> Result<()> {
    let _home = HomeGuard::new()?;
    crate::broker::enqueue_bus_message(
        "receiver",
        "sidekar",
        "[sidekar] You have an unanswered request from sender. Reply using bus send or bus done with --reply-to=abc-123",
        false,
        None,
    )?;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let bus_state: SharedBusState = Arc::new(Mutex::new(BusState::default()));
    let client_id = bus_state.lock().await.attach("receiver".to_string(), tx);
    deliver_once(&bus_state).await;

    let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for nudge")
        .expect("daemon bus sender closed");
    assert_eq!(
        frame.get("body").and_then(Value::as_str),
        Some(
            "[sidekar] You have an unanswered request from sender. Reply using bus send or bus done with --reply-to=abc-123"
        )
    );
    assert_eq!(bus_state.lock().await.in_flight.len(), 1);
    bus_state.lock().await.detach("receiver", client_id);
    assert_eq!(crate::broker::list_queued_messages("receiver")?.len(), 1);
    Ok(())
}

#[test]
fn nudge_once_enqueues_due_idle_outbound() -> Result<()> {
    let _home = HomeGuard::new()?;
    crate::broker::register_agent(&crate::message::AgentId::new("receiver"), None)?;
    let mut envelope = crate::message::Envelope::new_request(
        crate::message::AgentId::new("sender"),
        "receiver",
        "need update",
    );
    envelope.created_at = crate::message::epoch_secs().saturating_sub(61);
    crate::broker::set_outbound_request(
        &envelope,
        "sender",
        "broker",
        "receiver",
        Some("need update"),
        None,
    )?;

    nudge_once();

    let queued = crate::broker::list_queued_messages("receiver")?;
    assert_eq!(queued.len(), 1);
    assert!(queued[0].body.contains("--reply-to="));
    assert_eq!(
        crate::broker::outbound_request(&envelope.id)?
            .expect("outbound")
            .nudge_count,
        1
    );
    Ok(())
}

#[test]
fn nudge_once_skips_busy_recipient_without_claiming() -> Result<()> {
    let _home = HomeGuard::new()?;
    crate::broker::register_agent(&crate::message::AgentId::new("receiver"), None)?;
    let now = crate::message::epoch_secs();
    crate::broker::update_agent_activity(
        "receiver",
        crate::activity::ActivityState::AgentWorking,
        now,
    )?;
    let mut envelope = crate::message::Envelope::new_request(
        crate::message::AgentId::new("sender"),
        "receiver",
        "need update",
    );
    envelope.created_at = now.saturating_sub(61);
    crate::broker::set_outbound_request(
        &envelope,
        "sender",
        "broker",
        "receiver",
        Some("need update"),
        None,
    )?;

    nudge_once();

    assert!(crate::broker::list_queued_messages("receiver")?.is_empty());
    assert_eq!(
        crate::broker::outbound_request(&envelope.id)?
            .expect("outbound")
            .nudge_count,
        0
    );
    Ok(())
}
