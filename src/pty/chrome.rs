/// Watch the per-agent session file for changes. When the child agent calls
/// `sidekar launch` or `sidekar connect`, this file is updated with the session ID.
/// We read the session state to get CDP port and update the cron context.
pub(crate) async fn watch_session_file(agent_name: String) {
    use tokio::time::{Duration, interval};

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".sidekar");
    let safe_name = crate::sanitize_for_filename(&agent_name);
    let last_session_file = data_dir.join(format!("last-session-{safe_name}"));

    let mut poll = interval(Duration::from_secs(2));
    let mut last_contents = String::new();

    loop {
        poll.tick().await;

        let contents = match std::fs::read_to_string(&last_session_file) {
            Ok(c) => c.trim().to_string(),
            Err(_) => continue,
        };

        if contents == last_contents || contents.is_empty() {
            continue;
        }
        last_contents = contents.clone();

        // Read session state to get CDP port
        let state_file = data_dir.join(format!("state-{contents}.json"));
        if let Ok(state_str) = std::fs::read_to_string(&state_file)
            && let Ok(state) = serde_json::from_str::<serde_json::Value>(&state_str)
        {
            let port = state.get("port").and_then(|v| v.as_u64()).unwrap_or(9222) as u16;
            let host = state
                .get("host")
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1")
                .to_string();
            let cron_ctx = crate::commands::cron::CronContext {
                cdp_port: port,
                cdp_host: host,
                current_session_id: Some(contents.clone()),
                current_profile: state
                    .get("profile")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default")
                    .to_string(),
                headless: false,
                agent_name: Some(agent_name.clone()),
                project: crate::scope::resolve_project_name(None),
            };
            crate::commands::cron::update_cron_context(cron_ctx).await;
            // silent — don't print to the pty terminal
        }
    }
}

/// Close Chrome tabs and windows owned by the child's last session.
pub(crate) async fn cleanup_chrome_session(agent_name: &str) {
    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".sidekar");
    let safe_name = crate::sanitize_for_filename(agent_name);
    let last_session_file = data_dir.join(format!("last-session-{safe_name}"));

    let session_id = match std::fs::read_to_string(&last_session_file) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return,
    };
    if session_id.is_empty() {
        return;
    }

    let state_file = data_dir.join(format!("state-{session_id}.json"));
    let state: crate::types::SessionState = match std::fs::read_to_string(&state_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(s) => s,
        None => return,
    };

    let port = state.port.unwrap_or(9222);
    let host = state.host.as_deref().unwrap_or("127.0.0.1");
    let base_url = format!("http://{host}:{port}");

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    // Close all tabs owned by this session
    for tab_id in &state.tabs {
        let _ = client
            .put(format!("{base_url}/json/close/{tab_id}"))
            .send()
            .await;
    }

    // Clean up session state file and per-agent session pointer
    let _ = std::fs::remove_file(&state_file);
    let _ = std::fs::remove_file(&last_session_file);
}
