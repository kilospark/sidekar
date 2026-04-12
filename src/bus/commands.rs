use super::*;

// --- Delivery helpers ---

#[derive(Debug, Clone)]
struct DeliveryTarget {
    transport_name: &'static str,
    transport_target: String,
    output_label: String,
}

fn find_agent_on_channel(name_or_nick: &str, channel: &str) -> Option<BrokerAgent> {
    broker::list_agents(Some(channel))
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.id.name == name_or_nick || a.id.nick.as_deref() == Some(name_or_nick))
}

fn agents_on_channel(session: &str, exclude: &str) -> Vec<BrokerAgent> {
    broker::list_agents(Some(session))
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.id.name != exclude)
        .collect()
}

fn available_agents_str(channel: &str, exclude: &str) -> String {
    let agents = agents_on_channel(channel, exclude);
    if agents.is_empty() {
        "none".to_string()
    } else {
        agents
            .iter()
            .map(|a| a.id.display_name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn deliver_via(transport_name: &str, target: &str, message: &str, from: &str) -> Result<()> {
    let result = match transport_name {
        BROKER_TRANSPORT => crate::transport::Broker.deliver(target, message, from)?,
        RELAY_HTTP_TRANSPORT => crate::transport::RelayHttp.deliver(target, message, from)?,
        other => bail!("unknown transport: {other}"),
    };
    match result {
        crate::message::DeliveryResult::Delivered | crate::message::DeliveryResult::Queued => {
            Ok(())
        }
        crate::message::DeliveryResult::Failed(reason) => bail!("delivery failed: {reason}"),
    }
}

fn maybe_track_request(state: &SidekarBusState, envelope: &Envelope, delivery: &DeliveryTarget) {
    if !matches!(envelope.kind, MessageKind::Request | MessageKind::Handoff) {
        return;
    }
    if let Err(e) = broker::set_pending(envelope) {
        let _ = e;
        return;
    }
    let project = detect_project_name();
    if let Err(e) = broker::set_outbound_request(
        envelope,
        &envelope.from.display_name(),
        delivery.transport_name,
        &delivery.transport_target,
        state.channel(),
        Some(project.as_str()),
    ) {
        let _ = e;
    }
    let _ =
        broker::mark_agent_session_request(&envelope.from.name, &envelope.id, envelope.created_at);
}

fn cleanup_tracking(msg_id: &str) {
    let _ = broker::clear_pending(msg_id);
    let _ = broker::delete_outbound_request(msg_id);
}

fn resolve_reply(envelope: &Envelope, reply_to: Option<&str>) {
    if let Some(reply_id) = reply_to {
        let _ = broker::record_reply(reply_id, envelope);
    }
}

fn clear_local_pending_reply(reply_to: Option<&str>) {
    if let Some(reply_id) = reply_to {
        let _ = broker::resolve_reply(reply_id);
    }
}

fn cleanup_completed_exchange(
    self_name: &str,
    other_name: &str,
    channel: Option<&str>,
    keep_msg_id: Option<&str>,
) {
    let canonical_other = broker::find_agent(other_name, channel)
        .ok()
        .flatten()
        .map(|agent| agent.id.name)
        .unwrap_or_else(|| other_name.to_string());
    let _ = broker::clear_pending_between_agents(self_name, &canonical_other);
    let _ = broker::close_outbound_between_agents(self_name, &canonical_other, keep_msg_id);
}

fn relay_session_for_target(to: &str) -> Option<crate::transport::RelaySessionInfo> {
    crate::auth::auth_token()?;
    let sessions = crate::transport::fetch_relay_sessions().ok()?;
    sessions
        .into_iter()
        .find(|s| s.name == to || s.nickname.as_deref() == Some(to))
}

fn find_delivery_target(to: &str, channel: &str) -> Option<DeliveryTarget> {
    // Try same-channel first, then any agent
    if let Some(agent) = find_agent_on_channel(to, channel) {
        return Some(DeliveryTarget {
            transport_name: BROKER_TRANSPORT,
            transport_target: agent.id.name.clone(),
            output_label: format!("via broker (channel {})", channel),
        });
    }

    // Cross-channel fallback
    if let Ok(Some(agent)) = broker::find_agent(to, None) {
        return Some(DeliveryTarget {
            transport_name: BROKER_TRANSPORT,
            transport_target: agent.id.name.clone(),
            output_label: format!("via broker ({})", agent.id.pane.as_deref().unwrap_or("?")),
        });
    }

    // Remote relay: another machine / session for this user (device token + live tunnel on relay)
    if let Some(sess) = relay_session_for_target(to) {
        return Some(DeliveryTarget {
            transport_name: RELAY_HTTP_TRANSPORT,
            transport_target: sess.id.clone(),
            output_label: format!("via relay ({})", sess.hostname),
        });
    }

    None
}

fn send_directed_envelope(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    envelope: Envelope,
    reply_to: Option<&str>,
    verb: &str,
) -> Result<()> {
    let channel = match state.channel() {
        Some(c) => c.to_string(),
        None => bail!("Not registered on the bus. Relaunch your agent with: sidekar <agent-cli>"),
    };

    if matches!(envelope.to.as_str(), "@all" | "all") {
        bail!(
            "Broadcast targets are not supported. Use `sidekar bus who` and message a specific agent."
        );
    }

    let full_message = envelope.format_for_paste();
    let delivery = find_delivery_target(&envelope.to, &channel).ok_or_else(|| {
        let available = available_agents_str(&channel, &envelope.from.name);
        anyhow!(
            "Unknown agent \"{}\". Available on this channel: {available}. Use `sidekar bus who` to see all agents.",
            envelope.to
        )
    })?;

    maybe_track_request(state, &envelope, &delivery);

    let delivery_result: Result<()> = if delivery.transport_name == RELAY_HTTP_TRANSPORT {
        crate::transport::deliver_relay_envelope(&delivery.transport_target, &envelope).map(|_| ())
    } else {
        deliver_via(
            delivery.transport_name,
            &delivery.transport_target,
            &full_message,
            &envelope.from.name,
        )
    };

    if let Err(e) = delivery_result {
        cleanup_tracking(&envelope.id);
        bail!("Failed to reach {}: {e}", envelope.to);
    }

    if delivery.transport_name == RELAY_HTTP_TRANSPORT {
        clear_local_pending_reply(reply_to);
    } else {
        resolve_reply(&envelope, reply_to);
    }

    if matches!(envelope.kind, MessageKind::Request | MessageKind::Handoff) {
        out!(
            ctx,
            "{verb} to {} ({}). [msg_id: {}]",
            envelope.to,
            delivery.output_label,
            envelope.id
        );
    } else {
        out!(
            ctx,
            "{verb} to {} ({}).",
            envelope.to,
            delivery.output_label
        );
    }
    Ok(())
}

// --- Tool handlers ---

pub fn cmd_who(
    state: &SidekarBusState,
    ctx: &mut AppContext,
    show_all: bool,
    json_output: bool,
) -> Result<()> {
    let my_name = state.name().unwrap_or("unknown");

    let agents = if show_all {
        broker::list_agents(None).unwrap_or_default()
    } else {
        match state.channel() {
            Some(c) => broker::list_agents(Some(c)).unwrap_or_default(),
            None => broker::list_agents(None).unwrap_or_default(),
        }
    };

    if json_output {
        let entries: Vec<serde_json::Value> = agents
            .iter()
            .map(|a| {
                serde_json::json!({
                    "name": a.id.name,
                    "nick": a.id.nick,
                    "pane": a.id.pane,
                    "channel": a.id.session,
                    "cwd": a.cwd,
                    "is_you": a.id.name == my_name,
                })
            })
            .collect();
        out!(
            ctx,
            "{}",
            serde_json::to_string_pretty(&entries).unwrap_or_default()
        );
        return Ok(());
    }

    if agents.is_empty() {
        if show_all {
            out!(ctx, "0 agents on any channel.");
        } else {
            let scope = state.channel().unwrap_or("any channel");
            out!(ctx, "0 agents on \"{scope}\".");
        }
        return Ok(());
    }

    // Group agents by channel when showing all
    let format_agent = |a: &broker::BrokerAgent| -> String {
        let you = if a.id.name == my_name { " (you)" } else { "" };
        let nick =
            a.id.nick
                .as_deref()
                .map(|n| format!(" \"{n}\""))
                .unwrap_or_default();
        let pane = a.id.pane.as_deref().unwrap_or("?");
        let cwd = a
            .cwd
            .as_deref()
            .map(|c| format!(", cwd: {c}"))
            .unwrap_or_default();
        format!("- {}{}{} (pane {}{})", a.id.name, nick, you, pane, cwd)
    };

    if show_all {
        // Group by channel
        let mut by_channel: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for a in &agents {
            let ch = a.id.session.clone().unwrap_or_else(|| "unknown".into());
            by_channel.entry(ch).or_default().push(format_agent(a));
        }
        let mut out_lines = Vec::new();
        for (ch, lines) in &by_channel {
            out_lines.push(format!("Channel \"{ch}\":"));
            for l in lines {
                out_lines.push(l.clone());
            }
        }
        out!(ctx, "{}", out_lines.join("\n"));
    } else {
        let channel_label = state.channel().unwrap_or("all");
        let lines: Vec<String> = agents.iter().map(format_agent).collect();
        out!(ctx, "Channel \"{channel_label}\":\n{}", lines.join("\n"));
    }
    Ok(())
}

pub fn cmd_requests(
    state: &SidekarBusState,
    ctx: &mut AppContext,
    status: Option<&str>,
    limit: usize,
) -> Result<()> {
    let limit = if limit == 0 {
        DEFAULT_BUS_LIST_LIMIT
    } else {
        limit
    };
    let self_name = state.name().ok_or_else(|| {
        anyhow!("Not registered on the bus. Relaunch your agent with: sidekar <agent-cli>")
    })?;
    let requests = broker::list_outbound_requests_for_sender(self_name, status, limit)?;
    if requests.is_empty() {
        out!(ctx, "No outbound requests.");
        return Ok(());
    }

    out!(
        ctx,
        "msg_id\tstatus\tkind\tto\tcreated_at\tanswered_at\tpreview"
    );
    for request in requests {
        out!(
            ctx,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            request.msg_id,
            request.status,
            request.kind,
            request.recipient_name,
            request.created_at,
            request
                .answered_at
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            request.message_preview.replace('\n', " "),
        );
    }
    Ok(())
}

pub fn cmd_replies(
    state: &SidekarBusState,
    ctx: &mut AppContext,
    reply_to_msg_id: Option<&str>,
    limit: usize,
) -> Result<()> {
    let limit = if limit == 0 {
        DEFAULT_BUS_LIST_LIMIT
    } else {
        limit
    };
    let self_name = state.name().ok_or_else(|| {
        anyhow!("Not registered on the bus. Relaunch your agent with: sidekar <agent-cli>")
    })?;
    let replies = broker::list_bus_replies_for_sender(self_name, reply_to_msg_id, limit)?;
    if replies.is_empty() {
        out!(ctx, "No replies.");
        return Ok(());
    }

    out!(ctx, "reply_to\treply_id\tfrom\tkind\tcreated_at\tmessage");
    for reply in replies {
        out!(
            ctx,
            "{}\t{}\t{}\t{}\t{}\t{}",
            reply.reply_to_msg_id,
            reply.reply_msg_id,
            reply.sender_label,
            reply.kind,
            reply.created_at,
            reply.message.replace('\n', " "),
        );
    }
    Ok(())
}

pub fn cmd_show_request(state: &SidekarBusState, ctx: &mut AppContext, msg_id: &str) -> Result<()> {
    let self_name = state.name().ok_or_else(|| {
        anyhow!("Not registered on the bus. Relaunch your agent with: sidekar <agent-cli>")
    })?;
    let request =
        broker::outbound_request(msg_id)?.ok_or_else(|| anyhow!("Unknown request: {msg_id}"))?;
    if request.sender_name != self_name {
        bail!("Request {msg_id} does not belong to the current agent.");
    }

    out!(ctx, "msg_id: {}", request.msg_id);
    out!(ctx, "status: {}", request.status);
    out!(ctx, "kind: {}", request.kind);
    out!(ctx, "to: {}", request.recipient_name);
    out!(
        ctx,
        "channel: {}",
        request.channel.as_deref().unwrap_or("-")
    );
    out!(
        ctx,
        "project: {}",
        request.project.as_deref().unwrap_or("-")
    );
    out!(ctx, "created_at: {}", request.created_at);
    out!(
        ctx,
        "answered_at: {}",
        request
            .answered_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into())
    );
    out!(
        ctx,
        "timed_out_at: {}",
        request
            .timed_out_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into())
    );
    out!(ctx, "preview: {}", request.message_preview);

    let replies = broker::replies_for_request(msg_id)?;
    if replies.is_empty() {
        out!(ctx, "replies: none");
        return Ok(());
    }

    out!(ctx, "replies:");
    for reply in replies {
        out!(
            ctx,
            "- {} {} {} {}",
            reply.reply_msg_id,
            reply.kind,
            reply.sender_label,
            reply.message.replace('\n', " ")
        );
    }
    Ok(())
}

pub fn cmd_send_message(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    to: &str,
    message: &str,
    kind: &str,
    reply_to: Option<&str>,
) -> Result<()> {
    let from_id = state.agent_id();
    let msg_kind = MessageKind::from_str_lossy(kind);
    let mut envelope = Envelope::new(from_id, to, msg_kind, message);
    if let Some(rt) = reply_to {
        envelope.reply_to = Some(rt.to_string());
    }
    send_directed_envelope(state, ctx, envelope, reply_to, "Message sent")
}

pub fn cmd_signal_done(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    next: &str,
    summary: &str,
    request: &str,
    reply_to: Option<&str>,
) -> Result<()> {
    let from_id = state.agent_id();
    let self_name = from_id.name.clone();
    let channel = state.channel().map(str::to_string);
    let mut envelope = Envelope::new_handoff(from_id, next, summary, request);
    if let Some(rt) = reply_to {
        envelope.reply_to = Some(rt.to_string());
    }
    let keep_msg_id = envelope.id.clone();
    send_directed_envelope(state, ctx, envelope, reply_to, "Handed off")?;
    cleanup_completed_exchange(&self_name, next, channel.as_deref(), Some(&keep_msg_id));
    Ok(())
}

pub fn cmd_register(
    state: &mut SidekarBusState,
    ctx: &mut AppContext,
    custom_name: Option<&str>,
) -> Result<()> {
    state.do_register(custom_name);
    match state.name() {
        Some(name) => {
            out!(
                ctx,
                "Registered as \"{name}\" on channel \"{}\".",
                state.channel().unwrap_or("unknown")
            );
            Ok(())
        }
        None => bail!(
            "Registration failed — the agent bus requires a sidekar PTY wrapper.\n\n\
             To fix, launch your agent with: sidekar claude, sidekar codex, etc."
        ),
    }
}

pub fn cmd_unregister(state: &mut SidekarBusState, ctx: &mut AppContext) -> Result<()> {
    if state.identity.is_none() {
        bail!("Not registered.");
    }
    let old_name = state.name().unwrap_or("unknown").to_string();
    state.unregister();
    out!(ctx, "Unregistered \"{old_name}\". You are now off the bus.");
    Ok(())
}
