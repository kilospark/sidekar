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
            if ok {
                // Mark delivered even when the claim was released underneath this
                // poller (reattach, daemon restart). The paste already happened;
                // recording it is what stops the row being handed out again.
                let _ = crate::broker::mark_message_delivered(id);
            } else if owned {
                let _ = crate::broker::release_queued_message(id);
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
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(NUDGE_LOOP_INTERVAL_SECS));
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
        // The recipient cannot answer what has not reached their pane. Nudging
        // here queues a second message behind the first and doubles the cost of
        // an already-stalled delivery.
        if crate::broker::envelope_awaiting_delivery(&request.msg_id).unwrap_or(false) {
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
        "relay_http" => {
            crate::transport::relay_recipient_should_defer_nudge(&request.transport_target)
        }
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

fn deliver_nudge(request: &crate::broker::OutboundRequestRecord, body: &str) -> anyhow::Result<()> {
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
            match crate::transport::RelayHttp.deliver(&request.transport_target, body, "sidekar")? {
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
                "created_at": msg.created_at,
                "interrupt": msg.envelope.as_ref().map(|e| e.interrupt).unwrap_or(false),
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
mod tests;
