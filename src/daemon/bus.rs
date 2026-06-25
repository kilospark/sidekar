use super::*;
use tokio::sync::mpsc;

const NUDGE_BACKOFF_SECS: [u64; 5] = [60, 120, 300, 600, 900];
const NUDGE_LOOP_INTERVAL_SECS: u64 = 30;

#[derive(Default)]
pub(super) struct BusState {
    clients: std::collections::HashMap<String, BusClient>,
    in_flight: std::collections::HashMap<i64, String>,
    next_client_id: u64,
}

pub(super) type SharedBusState = Arc<Mutex<BusState>>;

struct BusClient {
    id: u64,
    tx: mpsc::UnboundedSender<Value>,
}

impl BusState {
    fn attach(&mut self, agent: String, tx: mpsc::UnboundedSender<Value>) -> u64 {
        self.next_client_id = self.next_client_id.wrapping_add(1);
        let id = self.next_client_id;
        self.release_agent_claims(&agent);
        self.clients.insert(agent, BusClient { id, tx });
        id
    }

    fn detach(&mut self, agent: &str, id: u64) {
        if !matches!(self.clients.get(agent), Some(client) if client.id == id) {
            return;
        }
        self.clients.remove(agent);
        self.release_agent_claims(agent);
    }

    fn release_agent_claims(&mut self, agent: &str) {
        let ids: Vec<i64> = self
            .in_flight
            .iter()
            .filter_map(|(id, owner)| (owner == agent).then_some(*id))
            .collect();
        for id in ids {
            self.in_flight.remove(&id);
            let _ = crate::broker::release_queued_message(id);
        }
    }

    pub(super) fn status_json(&self) -> Value {
        let clients: Vec<String> = self.clients.keys().cloned().collect();
        json!({
            "clients": clients,
            "in_flight": self.in_flight.len(),
        })
    }
}

pub(super) async fn handle_bus_client(
    first: Value,
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    state: Arc<Mutex<DaemonState>>,
) {
    let agent = first
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if agent.is_empty() {
        let _ = writer
            .write_all(b"{\"type\":\"bus_attached\",\"ok\":false,\"error\":\"missing agent\"}\n")
            .await;
        let _ = writer.flush().await;
        return;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let bus_state = {
        let s = state.lock().await;
        s.bus_state.clone()
    };
    let client_id = {
        let mut bus = bus_state.lock().await;
        bus.attach(agent.clone(), tx)
    };
    if writer
        .write_all(b"{\"type\":\"bus_attached\",\"ok\":true}\n")
        .await
        .is_err()
    {
        bus_state.lock().await.detach(&agent, client_id);
        return;
    }
    let _ = writer.flush().await;

    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let mut out = match serde_json::to_string(&frame) {
                Ok(s) => s,
                Err(_) => continue,
            };
            out.push('\n');
            if writer.write_all(out.as_bytes()).await.is_err() {
                break;
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
    });

    let mut line = String::new();
    loop {
        match super::read_line_limited(&mut reader, &mut line, super::MAX_LINE_LEN).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let parsed = serde_json::from_str::<Value>(line.trim());
        line.clear();
        let Ok(frame) = parsed else {
            continue;
        };
        if frame.get("type").and_then(|v| v.as_str()) == Some("bus_ack") {
            let Some(id) = frame.get("id").and_then(|v| v.as_i64()) else {
                continue;
            };
            let ok = frame.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let owned = {
                let mut bus = bus_state.lock().await;
                matches!(bus.in_flight.remove(&id), Some(owner) if owner == agent)
            };
            if owned {
                if ok {
                    let _ = crate::broker::delete_queued_message(id);
                } else {
                    let _ = crate::broker::release_queued_message(id);
                }
            }
        }
    }

    writer_task.abort();
    bus_state.lock().await.detach(&agent, client_id);
}

pub(super) async fn bus_delivery_loop(bus_state: SharedBusState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
    interval.tick().await;
    loop {
        interval.tick().await;
        deliver_once(&bus_state).await;
    }
}

pub(super) async fn bus_nudge_loop() {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        NUDGE_LOOP_INTERVAL_SECS,
    ));
    interval.tick().await;
    loop {
        interval.tick().await;
        nudge_once();
    }
}

fn nudge_once() {
    let now = crate::message::epoch_secs();
    let Ok(requests) = crate::broker::all_open_outbound_requests() else {
        return;
    };
    for request in requests {
        if !nudge_due(&request, now) {
            continue;
        }
        if recipient_should_defer_nudge(&request) {
            continue;
        }
        let Ok(true) = crate::broker::outbound_nudgeable(&request.msg_id) else {
            continue;
        };
        let Ok(true) = crate::broker::try_increment_nudge_count(&request.msg_id, now) else {
            continue;
        };
        let body = nudge_body(&request);
        if let Err(e) = deliver_nudge(&request, &body) {
            let _ = crate::broker::revert_nudge_claim(&request.msg_id, now);
            crate::broker::try_log_error(
                "daemon-bus",
                "failed to deliver outbound nudge",
                Some(&format!("{}: {e:#}", request.msg_id)),
            );
        }
    }
}

fn nudge_due(request: &crate::broker::OutboundRequestRecord, now: u64) -> bool {
    let index = request.nudge_count as usize;
    let Some(delay) = NUDGE_BACKOFF_SECS.get(index).copied() else {
        return false;
    };
    let base = request.last_nudged_at.unwrap_or(request.created_at);
    now.saturating_sub(base) >= delay
}

fn recipient_should_defer_nudge(request: &crate::broker::OutboundRequestRecord) -> bool {
    match request.transport_name.as_str() {
        "broker" => crate::broker::get_agent_activity(&request.transport_target)
            .ok()
            .flatten()
            .map(|snap| snap.should_defer_nudge())
            .unwrap_or(false),
        "relay_http" => crate::transport::relay_recipient_should_defer_nudge(
            &request.transport_target,
        ),
        _ => false,
    }
}

fn nudge_body(request: &crate::broker::OutboundRequestRecord) -> String {
    let preview = request.message_preview.trim();
    let preview = if preview.is_empty() {
        "request pending"
    } else {
        preview
    };
    format!(
        "[sidekar] You have an unanswered request from {}. Reply using bus send or bus done with --reply-to={}. Request: {}",
        request.sender_name, request.msg_id, preview
    )
}

fn deliver_nudge(
    request: &crate::broker::OutboundRequestRecord,
    body: &str,
) -> anyhow::Result<()> {
    match request.transport_name.as_str() {
        "broker" => {
            crate::broker::enqueue_bus_message(
                &request.transport_target,
                "sidekar",
                body,
                true,
                None,
            )?;
            Ok(())
        }
        "relay_http" => {
            use crate::transport::Transport;
            match crate::transport::RelayHttp.deliver(
                &request.transport_target,
                body,
                "sidekar",
            )? {
                crate::message::DeliveryResult::Delivered => Ok(()),
                crate::message::DeliveryResult::Queued => Ok(()),
                crate::message::DeliveryResult::Failed(reason) => anyhow::bail!(reason),
            }
        }
        other => anyhow::bail!("unsupported transport {other}"),
    }
}

async fn deliver_once(bus_state: &SharedBusState) {
    let clients: Vec<(String, u64, mpsc::UnboundedSender<Value>)> = {
        let bus = bus_state.lock().await;
        bus.clients
            .iter()
            .map(|(agent, client)| (agent.clone(), client.id, client.tx.clone()))
            .collect()
    };

    for (agent, client_id, tx) in clients {
        let Ok(messages) = crate::broker::list_queued_messages(&agent) else {
            continue;
        };
        for msg in messages {
            let Ok(Some(msg)) = crate::broker::claim_queued_message(msg.id, &agent) else {
                continue;
            };

            if let Some(ref envelope) = msg.envelope
                && !envelope.requires_reply()
            {
                let _ = crate::broker::dismiss_terminal_ack_request(&envelope.id);
            }
            {
                let mut bus = bus_state.lock().await;
                if !matches!(bus.clients.get(&agent), Some(client) if client.id == client_id) {
                    let _ = crate::broker::release_queued_message(msg.id);
                    continue;
                }
                bus.in_flight.insert(msg.id, agent.clone());
            }

            let frame = json!({
                "type": "bus_message",
                "id": msg.id,
                "sender": msg.sender,
                "recipient": msg.recipient,
                "body": msg.body,
            });
            if tx.send(frame).is_err() {
                let mut bus = bus_state.lock().await;
                bus.in_flight.remove(&msg.id);
                bus.detach(&agent, client_id);
                let _ = crate::broker::release_queued_message(msg.id);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
        for _ in 0..3 {
            let frame = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("timed out waiting for daemon bus frame")
                .expect("daemon bus sender closed");
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
            Some("[sidekar] You have an unanswered request from sender. Reply using bus send or bus done with --reply-to=abc-123")
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
}
