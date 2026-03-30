use crate::message::DeliveryResult;
use crate::transport::{self, Transport};
use crate::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Mutex;

/// Global timestamp of the last sidekar tool call (epoch ms).
/// Used for source attribution — title changes within 5s of a tool call are skipped.
static LAST_TOOL_ACTION_MS: AtomicU64 = AtomicU64::new(0);

/// Update the last tool action timestamp (call from dispatch).
pub fn mark_tool_action() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    LAST_TOOL_ACTION_MS.store(now, Ordering::Relaxed);
}

fn last_tool_action_ms() -> u64 {
    LAST_TOOL_ACTION_MS.load(Ordering::Relaxed)
}

/// How the monitor delivers notifications — transport + target.
pub(crate) struct Delivery {
    transport: Box<dyn Transport>,
    target: String,
}

/// Monitor state shared between the background task and tool calls.
pub struct MonitorState {
    running: Arc<AtomicBool>,
    watched_tabs: Vec<String>,
    event_count: Arc<AtomicU64>,
    last_event_ms: Arc<AtomicU64>,
    error_count: Arc<AtomicU64>,
    last_error: Arc<Mutex<Option<String>>>,
    task_handle: tokio::task::JoinHandle<()>,
}

impl Drop for MonitorState {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.task_handle.abort();
    }
}

/// Global monitor state — one per session.
static MONITOR: tokio::sync::OnceCell<Mutex<Option<MonitorState>>> =
    tokio::sync::OnceCell::const_new();

async fn monitor_cell() -> &'static Mutex<Option<MonitorState>> {
    MONITOR.get_or_init(|| async { Mutex::new(None) }).await
}

/// Deliver a notification message using the chosen transport.
pub(crate) fn deliver_notification(delivery: &Delivery, message: &str) -> Result<()> {
    let formatted = format!("[from sidekar-monitor]: {message}");
    match delivery
        .transport
        .deliver(&delivery.target, &formatted, "sidekar-monitor")?
    {
        DeliveryResult::Delivered | DeliveryResult::Queued => Ok(()),
        DeliveryResult::Failed(reason) => bail!("delivery failed: {reason}"),
    }
}

/// Resolve the delivery transport for monitor notifications.
/// Uses broker queue for delivery.
pub(crate) fn resolve_delivery() -> Result<Delivery> {
    // Deliver via broker queue to our own agent
    if let Ok(agents) = crate::broker::list_agents(None) {
        let my_pid = std::process::id().to_string();
        let my_pane = format!("pty-{my_pid}");
        for agent in &agents {
            if agent.pane_unique_id.as_deref() == Some(&my_pane) {
                return Ok(Delivery {
                    transport: Box::new(transport::Broker),
                    target: agent.id.name.clone(),
                });
            }
        }
        // Fall back to first registered agent
        if let Some(agent) = agents.first() {
            return Ok(Delivery {
                transport: Box::new(transport::Broker),
                target: agent.id.name.clone(),
            });
        }
    }

    bail!(
        "monitor: cannot find a delivery target. \
         Run inside a sidekar PTY wrapper (sidekar claude, sidekar codex, etc.)."
    )
}

/// Start the monitor background task.
async fn start_monitor(ctx: &mut AppContext, tab_ids: Vec<String>) -> Result<()> {
    let cell = monitor_cell().await;
    let mut guard = cell.lock().await;

    // Stop existing monitor if running
    if let Some(old) = guard.take() {
        old.running.store(false, Ordering::Relaxed);
        old.task_handle.abort();
        // stopped previous monitor
    }

    // Resolve tab IDs to CDP target IDs
    let debug_tabs = get_debug_tabs(ctx).await?;
    let state = ctx.load_session_state()?;

    let mut cdp_target_ids: Vec<String> = Vec::new();
    let mut watched_names: Vec<String> = Vec::new();

    if tab_ids.len() == 1 && tab_ids[0] == "all" {
        for tab_id in &state.tabs {
            if let Some(tab) = debug_tabs.iter().find(|t| t.id == *tab_id) {
                cdp_target_ids.push(tab.id.clone());
                watched_names.push(tab.title.as_deref().unwrap_or("untitled").to_string());
            }
        }
    } else {
        for id_str in &tab_ids {
            if let Ok(idx) = id_str.parse::<usize>() {
                if let Some(tab_cdp_id) = state.tabs.get(idx) {
                    if let Some(tab) = debug_tabs.iter().find(|t| t.id == *tab_cdp_id) {
                        cdp_target_ids.push(tab.id.clone());
                        watched_names.push(tab.title.as_deref().unwrap_or("untitled").to_string());
                    }
                }
            } else if debug_tabs.iter().any(|t| t.id == *id_str) {
                cdp_target_ids.push(id_str.clone());
                watched_names.push(id_str.clone());
            }
        }
    }

    if cdp_target_ids.is_empty() {
        bail!("No valid tabs to monitor. Use tab IDs from `tabs` command, or \"all\".");
    }

    let delivery = resolve_delivery()?;

    let running = Arc::new(AtomicBool::new(true));
    let event_count = Arc::new(AtomicU64::new(0));
    let last_event_ms = Arc::new(AtomicU64::new(0));
    let error_count = Arc::new(AtomicU64::new(0));
    let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let ws_url = {
        let body = http_get_text(ctx, "/json/version").await?;
        let version_info: serde_json::Value = serde_json::from_str(&body)?;
        version_info
            .get("webSocketDebuggerUrl")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("No browser-level webSocketDebuggerUrl"))?
            .to_string()
    };

    let r = running.clone();
    let ec = event_count.clone();
    let le = last_event_ms.clone();
    let erc = error_count.clone();
    let lerr = last_error.clone();
    let targets = cdp_target_ids.clone();

    let task_handle = tokio::spawn(async move {
        let _ = run_title_watcher(r, targets, ws_url, delivery, ec, le, erc, lerr).await;
    });

    *guard = Some(MonitorState {
        running,
        watched_tabs: cdp_target_ids,
        event_count,
        last_event_ms,
        error_count,
        last_error,
        task_handle,
    });

    out!(
        ctx,
        "Monitor started. Watching {} tab(s): {}",
        watched_names.len(),
        watched_names.join(", ")
    );
    Ok(())
}

/// JavaScript injected into each watched tab to observe favicon changes.
/// Reports via `__sidekarFaviconChanged` binding with JSON: {"old":"...","new":"..."}
const FAVICON_OBSERVER_JS: &str = r#"
(() => {
    if (window.__sidekarFaviconObserver) return;
    function getFaviconHref() {
        const el = document.querySelector('link[rel~="icon"]');
        return el ? el.href : '';
    }
    let lastHref = getFaviconHref();

    const observer = new MutationObserver(() => {
        const cur = getFaviconHref();
        if (cur !== lastHref) {
            const old = lastHref;
            lastHref = cur;
            try { __sidekarFaviconChanged(JSON.stringify({old, new: cur})); } catch(e) {}
        }
    });

    // Observe the <head> for child additions/removals and attribute changes on link[rel~=icon]
    const head = document.head || document.documentElement;
    observer.observe(head, { childList: true, subtree: true, attributes: true, attributeFilter: ['href', 'rel'] });
    window.__sidekarFaviconObserver = observer;
})();
"#;

/// Attach to a target and inject the favicon MutationObserver.
/// Returns the CDP session ID for the target, or None on failure.
async fn inject_favicon_observer(cdp: &mut CdpClient, target_id: &str) -> Option<String> {
    // Attach to target with flatten=true so events arrive on the browser connection
    let result = cdp
        .send(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
        )
        .await;
    let session_id = match result {
        Ok(v) => v.get("sessionId").and_then(Value::as_str)?.to_string(),
        Err(e) => {
            let _ = e;
            return None;
        }
    };

    // Add the JS binding (fires Runtime.bindingCalled when page calls it)
    if let Err(e) = cdp
        .send_to_session(
            "Runtime.addBinding",
            json!({"name": "__sidekarFaviconChanged"}),
            &session_id,
        )
        .await
    {
        let _ = e;
    }

    // Enable Runtime domain so bindingCalled events are delivered
    if let Err(e) = cdp
        .send_to_session("Runtime.enable", json!({}), &session_id)
        .await
    {
        let _ = e;
    }

    // Enable Page domain for frameNavigated events (re-inject after navigation)
    if let Err(e) = cdp
        .send_to_session("Page.enable", json!({}), &session_id)
        .await
    {
        let _ = e;
    }

    // Inject the observer script
    if let Err(e) = cdp
        .send_to_session(
            "Runtime.evaluate",
            json!({"expression": FAVICON_OBSERVER_JS}),
            &session_id,
        )
        .await
    {
        let _ = e;
    }

    // favicon observer injected
    Some(session_id)
}

/// The background CDP watcher for title and favicon changes.
async fn run_title_watcher(
    running: Arc<AtomicBool>,
    watched_targets: Vec<String>,
    ws_url: String,
    delivery: Delivery,
    event_count: Arc<AtomicU64>,
    last_event_ms: Arc<AtomicU64>,
    error_count: Arc<AtomicU64>,
    last_error: Arc<Mutex<Option<String>>>,
) -> Result<()> {
    let mut cdp = CdpClient::connect(&ws_url).await?;

    cdp.send("Target.setDiscoverTargets", json!({"discover": true}))
        .await?;

    // Attach to each watched target and inject favicon observer.
    // Maps session_id -> target_id for routing bindingCalled events.
    let mut session_to_target: HashMap<String, String> = HashMap::new();
    for target_id in &watched_targets {
        if let Some(session_id) = inject_favicon_observer(&mut cdp, target_id).await {
            session_to_target.insert(session_id, target_id.clone());
        }
    }

    // CDP watcher connected

    // known: target_id -> (title, url, favicon_href)
    let mut known: HashMap<String, (String, String, String)> = HashMap::new();
    let debounce_ms: u64 = 3000;
    // pending: target_id -> (kind, old_val, new_val, title, url, deadline)
    let mut pending_events: HashMap<String, (String, String, String, String, String, Instant)> =
        HashMap::new();

    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }

        // Find the earliest deadline across all pending events
        let wait_dur = if let Some(earliest) = pending_events.values().map(|v| v.5).min() {
            let now = Instant::now();
            if now >= earliest {
                Duration::from_millis(0)
            } else {
                earliest - now
            }
        } else {
            Duration::from_secs(5)
        };

        // Flush any pending events whose debounce has expired
        let now_instant = Instant::now();
        let expired: Vec<String> = pending_events
            .iter()
            .filter(|(_, v)| now_instant >= v.5)
            .map(|(k, _)| k.clone())
            .collect();

        for target_id in expired {
            if let Some((kind, old_val, new_val, title, url, _)) = pending_events.remove(&target_id)
            {
                // Source attribution: skip if within 5s of a tool call
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let last_action = last_tool_action_ms();
                if last_action > 0 && now_ms.saturating_sub(last_action) <= 5000 {
                    // skipping agent-initiated change
                    continue;
                }

                let message = if kind == "favicon" {
                    format!(
                        "Tab favicon changed on \"{}\"\n  old: {}\n  new: {}\nURL: {}",
                        title, old_val, new_val, url
                    )
                } else {
                    format!(
                        "Tab title changed: \"{}\" -> \"{}\"\nURL: {}",
                        old_val, new_val, url
                    )
                };
                // delivering notification

                match deliver_notification(&delivery, &message) {
                    Ok(()) => {
                        event_count.fetch_add(1, Ordering::Relaxed);
                        last_event_ms.store(now_ms, Ordering::Relaxed);
                    }
                    Err(e) => {
                        let _ = &e;
                        error_count.fetch_add(1, Ordering::Relaxed);
                        *last_error.lock().await = Some(format!("{e}"));
                    }
                }
            }
        }

        let event = match cdp.next_event(wait_dur).await {
            Ok(Some(ev)) => ev,
            Ok(None) => continue,
            Err(e) => {
                let _ = e;
                break;
            }
        };

        let method = event
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        match method {
            "Target.targetInfoChanged" => {
                let target_info = match event.pointer("/params/targetInfo") {
                    Some(info) => info,
                    None => continue,
                };

                let target_type = target_info
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if target_type != "page" {
                    continue;
                }

                let target_id = target_info
                    .get("targetId")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();

                if !watched_targets.contains(&target_id) {
                    continue;
                }

                let title = target_info
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let url = target_info
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();

                if url.starts_with("chrome://") || url.starts_with("chrome-extension://") {
                    continue;
                }

                let (old_title, old_url, old_fav) =
                    known.get(&target_id).cloned().unwrap_or_default();

                known.insert(target_id.clone(), (title.clone(), url.clone(), old_fav));

                if title != old_title && !old_title.is_empty() {
                    pending_events.insert(
                        target_id.clone(),
                        (
                            "title".to_string(),
                            old_title,
                            title.clone(),
                            title,
                            url.clone(),
                            Instant::now() + Duration::from_millis(debounce_ms),
                        ),
                    );
                }

                // If URL changed, the page navigated — re-inject favicon observer
                if url != old_url && !old_url.is_empty() {
                    if let Some(session_id) = session_to_target
                        .iter()
                        .find(|(_, tid)| **tid == target_id)
                        .map(|(sid, _)| sid.clone())
                    {
                        // URL changed, re-injecting favicon observer
                        // Small delay to let the page load
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        let _ = cdp
                            .send_to_session(
                                "Runtime.evaluate",
                                json!({"expression": FAVICON_OBSERVER_JS}),
                                &session_id,
                            )
                            .await;
                    }
                }
            }

            "Runtime.bindingCalled" => {
                let name = event
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name != "__sidekarFaviconChanged" {
                    continue;
                }

                // Route this event to the right target via sessionId
                let session_id = event
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let target_id = match session_to_target.get(session_id) {
                    Some(tid) => tid.clone(),
                    None => continue,
                };

                let payload_str = event
                    .pointer("/params/payload")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let payload: Value = match serde_json::from_str(payload_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let old_fav = payload
                    .get("old")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let new_fav = payload
                    .get("new")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();

                // Update known state
                let (title, url) = if let Some((t, u, _)) = known.get(&target_id) {
                    (t.clone(), u.clone())
                } else {
                    ("(unknown)".to_string(), String::new())
                };
                if let Some(entry) = known.get_mut(&target_id) {
                    entry.2 = new_fav.clone();
                }

                // favicon changed

                // Use a separate debounce key so favicon and title don't clobber each other
                let key = format!("{target_id}:favicon");
                pending_events.insert(
                    key,
                    (
                        "favicon".to_string(),
                        old_fav,
                        new_fav,
                        title,
                        url,
                        Instant::now() + Duration::from_millis(debounce_ms),
                    ),
                );
            }

            "Page.frameNavigated" => {
                // Re-inject favicon observer after navigation
                let session_id = event
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Some(_target_id) = session_to_target.get(&session_id) {
                    // Only re-inject for top-level frame navigations
                    let is_top = event
                        .pointer("/params/frame/parentId")
                        .and_then(Value::as_str)
                        .is_none();
                    if is_top {
                        // page navigated, re-injecting
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        let _ = cdp
                            .send_to_session(
                                "Runtime.evaluate",
                                json!({"expression": FAVICON_OBSERVER_JS}),
                                &session_id,
                            )
                            .await;
                    }
                }
            }

            _ => continue,
        }
    }

    Ok(())
}

/// Handle the `monitor` tool command.
pub async fn cmd_monitor(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("status");

    match sub {
        "start" => {
            let tab_ids: Vec<String> = args
                .iter()
                .skip(1)
                .filter(|a| !a.starts_with("--"))
                .cloned()
                .collect();

            if tab_ids.is_empty() {
                bail!("Usage: monitor start <tab_id|tab_id2|all>");
            }

            start_monitor(ctx, tab_ids).await
        }
        "stop" => {
            let cell = monitor_cell().await;
            let mut guard = cell.lock().await;
            if let Some(mon) = guard.take() {
                mon.running.store(false, Ordering::Relaxed);
                mon.task_handle.abort();
                out!(ctx, "Monitor stopped.");
            } else {
                out!(ctx, "No monitor is running.");
            }
            Ok(())
        }
        "status" => {
            let cell = monitor_cell().await;
            let guard = cell.lock().await;
            match guard.as_ref() {
                Some(mon) => {
                    let running = mon.running.load(Ordering::Relaxed);
                    let events = mon.event_count.load(Ordering::Relaxed);
                    let last_ms = mon.last_event_ms.load(Ordering::Relaxed);
                    let errors = mon.error_count.load(Ordering::Relaxed);
                    let last_err = mon.last_error.lock().await;

                    let last_event_ago = if last_ms > 0 {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        format!("{}s ago", (now - last_ms) / 1000)
                    } else {
                        "never".to_string()
                    };

                    out!(
                        ctx,
                        "Monitor: {}",
                        if running { "running" } else { "stopped" }
                    );
                    out!(ctx, "Watching: {} tab(s)", mon.watched_tabs.len());
                    out!(ctx, "Events delivered: {events}");
                    out!(ctx, "Last event: {last_event_ago}");
                    out!(ctx, "Delivery errors: {errors}");
                    if let Some(err) = last_err.as_ref() {
                        out!(ctx, "Last error: {err}");
                    }
                }
                None => {
                    out!(ctx, "No monitor is running.");
                }
            }
            Ok(())
        }
        _ => bail!("Usage: monitor <start|stop|status>"),
    }
}
