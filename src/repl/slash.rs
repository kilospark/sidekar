use super::*;
use super::renderer::EventRenderer;

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

pub(super) enum SlashResult {
    Continue,
    Quit,
    SwitchSession(String),
    /// Requires provider + model.
    NeedProvider(SlashAsync),
    SetCredential(String),
    SetModel(String),
    RelayOn,
    RelayOff,
}

/// Async slash commands that require an active provider.
pub(super) enum SlashAsync {
    Compact,
    InteractiveSelectModel,
}

pub(super) struct SlashContext<'a> {
    pub input: &'a str,
    pub cwd: &'a str,
    pub model: &'a str,
    pub session_id: &'a str,
    pub cred_name: &'a str,
}

pub(super) fn handle_slash_command(ctx: &SlashContext<'_>) -> Option<SlashResult> {
    let input = ctx.input;
    let cwd = ctx.cwd;
    let model = ctx.model;
    let current_session = ctx.session_id;
    let cred_name = ctx.cred_name;
    let cmd = input.split_whitespace().next().unwrap_or("");

    if !is_known_slash_command(cmd) {
        return None;
    }

    let result = match cmd {
        "/quit" | "/exit" | "/q" => SlashResult::Quit,
        "/new" | "/reset" => match session::create_session(cwd, model, "anthropic") {
            Ok(id) => SlashResult::SwitchSession(id),
            Err(e) => {
                broker::try_log_error("session", &format!("failed to create: {e}"), None);
                SlashResult::Continue
            }
        },
        "/session" => match session::list_sessions(cwd, 10) {
            Ok(mut sessions) => {
                if sessions.is_empty() {
                    tunnel_println("No sessions found.");
                    SlashResult::Continue
                } else {
                    if let Some(current_idx) =
                        sessions.iter().position(|s| s.id == *current_session)
                    {
                        let current = sessions.swap_remove(current_idx);
                        tunnel_println(&format!(
                            "Current: {} ({} msgs, {})",
                            &current.id[..8],
                            session::message_count(&current.id).unwrap_or(0),
                            current.model
                        ));
                    }
                    if sessions.is_empty() {
                        tunnel_println("No other sessions.");
                        SlashResult::Continue
                    } else {
                        tunnel_println("Pick session to switch:");
                        for (i, s) in sessions.iter().enumerate() {
                            let msgs = session::message_count(&s.id).unwrap_or(0);
                            let name = s.name.as_deref().unwrap_or(&s.id[..s.id.len().min(8)]);
                            tunnel_println(&format!("  [{i}] {name} — {msgs} msgs, {}", s.model));
                        }
                        print!("Enter number or Enter: ");
                        let _ = io::stdout().flush();
                        let mut line = String::new();
                        if io::stdin().lock().read_line(&mut line).is_ok() {
                            let choice = line.trim();
                            if choice.is_empty() {
                                tunnel_println("\x1b[2mStaying current.\x1b[0m");
                                SlashResult::Continue
                            } else if let Ok(idx) = choice.parse::<usize>() {
                                if let Some(s) = sessions.get(idx) {
                                    SlashResult::SwitchSession(s.id.clone())
                                } else {
                                    tunnel_println("Invalid.");
                                    SlashResult::Continue
                                }
                            } else {
                                tunnel_println("Invalid.");
                                SlashResult::Continue
                            }
                        } else {
                            SlashResult::Continue
                        }
                    }
                }
            }
            Err(e) => {
                broker::try_log_error("session", &format!("failed to list: {e}"), None);
                SlashResult::Continue
            }
        },
        "/model" => {
            let parts: Vec<_> = input.split_whitespace().collect();
            let arg = parts.get(1).copied();
            match arg {
                Some("list") => SlashResult::NeedProvider(SlashAsync::InteractiveSelectModel),
                Some(name) => SlashResult::SetModel(name.to_string()),
                None => {
                    if model != "(not set)" {
                        tunnel_println(&format!("Current model: {model}"));
                    } else {
                        tunnel_println("No model set.");
                    }
                    tunnel_println("\x1b[2mUse /model list to select.\x1b[0m");
                    SlashResult::Continue
                }
            }
        }
        "/credential" => {
            let parts: Vec<_> = input.split_whitespace().collect();
            let arg = parts.get(1).copied();
            match arg {
                Some("list") => {
                    let creds = providers::oauth::list_credentials();
                    if creds.is_empty() {
                        tunnel_println("No credentials stored. Use: sidekar repl login <nickname>");
                        SlashResult::Continue
                    } else {
                        let current = cred_name.to_string();
                        tunnel_println("Stored credentials (pick to switch):");
                        for (i, (name, provider)) in creds.iter().enumerate() {
                            let marker = if *name == current { " (current)" } else { "" };
                            let email = providers::oauth::credential_email(name)
                                .map(|e| format!(" <{e}>"))
                                .unwrap_or_default();
                            tunnel_println(&format!("  [{i}] {name} ({provider}){email}{marker}"));
                        }
                        print!("Enter number or Enter: ");
                        let _ = io::stdout().flush();
                        let mut line = String::new();
                        if io::stdin().lock().read_line(&mut line).is_ok() {
                            let choice = line.trim();
                            if choice.is_empty() {
                                tunnel_println("\x1b[2mStaying current.\x1b[0m");
                                SlashResult::Continue
                            } else if let Ok(idx) = choice.parse::<usize>() {
                                if let Some((name, _)) = creds.get(idx) {
                                    SlashResult::SetCredential(name.clone())
                                } else {
                                    tunnel_println("Invalid.");
                                    SlashResult::Continue
                                }
                            } else {
                                tunnel_println("Invalid.");
                                SlashResult::Continue
                            }
                        } else {
                            SlashResult::Continue
                        }
                    }
                }
                Some("delete") => {
                    let creds = providers::oauth::list_credentials();
                    if creds.is_empty() {
                        tunnel_println("No credentials stored.");
                        SlashResult::Continue
                    } else {
                        tunnel_println("Delete which credential?");
                        for (i, (name, provider)) in creds.iter().enumerate() {
                            let email = providers::oauth::credential_email(name)
                                .map(|e| format!(" <{e}>"))
                                .unwrap_or_default();
                            tunnel_println(&format!("  [{i}] {name} ({provider}){email}"));
                        }
                        print!("Enter number or Enter to cancel: ");
                        let _ = io::stdout().flush();
                        let mut line = String::new();
                        if io::stdin().lock().read_line(&mut line).is_ok() {
                            let choice = line.trim();
                            if let Ok(idx) = choice.parse::<usize>() {
                                if let Some((name, _)) = creds.get(idx) {
                                    let kv_key = providers::oauth::kv_key_for(name);
                                    match crate::broker::kv_delete(&kv_key) {
                                        Ok(_) => {
                                            tunnel_println(&format!("Deleted credential '{name}'."))
                                        }
                                        Err(e) => tunnel_println(&format!(
                                            "Failed to delete credential '{name}': {e}"
                                        )),
                                    }
                                } else {
                                    tunnel_println("Invalid.");
                                }
                            } else if !choice.is_empty() {
                                tunnel_println("Invalid.");
                            }
                        }
                        SlashResult::Continue
                    }
                }
                Some(name) => SlashResult::SetCredential(name.to_string()),
                None => {
                    if cred_name != "(none)" {
                        tunnel_println(&format!("Current credential: {cred_name}"));
                    } else {
                        tunnel_println("No credential set.");
                    }
                    tunnel_println("\x1b[2mUse /credential list | delete to select.\x1b[0m");
                    SlashResult::Continue
                }
            }
        }
        "/compact" => SlashResult::NeedProvider(SlashAsync::Compact),
        "/relay" => {
            let arg = input.split_whitespace().nth(1).unwrap_or("");
            match arg {
                "on" | "true" | "1" => SlashResult::RelayOn,
                "off" | "false" | "0" => SlashResult::RelayOff,
                "" => {
                    let state = if crate::tunnel::has_output_tunnel() {
                        "on"
                    } else {
                        "off"
                    };
                    tunnel_println(&format!("Relay: {state}"));
                    SlashResult::Continue
                }
                _ => {
                    tunnel_println("Usage: /relay [on|off]");
                    SlashResult::Continue
                }
            }
        }
        "/verbose" => {
            let arg = input.split_whitespace().nth(1).unwrap_or("");
            match arg {
                "on" | "true" | "1" => {
                    crate::runtime::set_verbose(true);
                    tunnel_println("Verbose mode: on");
                }
                "off" | "false" | "0" => {
                    crate::runtime::set_verbose(false);
                    tunnel_println("Verbose mode: off");
                }
                "" => {
                    let state = if crate::runtime::verbose() {
                        "on"
                    } else {
                        "off"
                    };
                    tunnel_println(&format!("Verbose mode: {state}"));
                }
                _ => {
                    tunnel_println("Usage: /verbose [on|off]");
                }
            }
            SlashResult::Continue
        }
        "/help" => {
            tunnel_println("Slash commands:");
            tunnel_println("  /credential  — Show/set/list & select stored credentials");
            tunnel_println("  /model       — Show/set/list & select available models");
            tunnel_println("  /new         — Start fresh session");
            tunnel_println("  /session     — List and switch sessions");
            tunnel_println("  /compact     — Compact older session context now");
            tunnel_println("  /relay       — Toggle web terminal relay (on/off)");
            tunnel_println(
                "  /verbose     — Verbose API logging + `[turn complete]` after each run (on/off)",
            );
            tunnel_println("  /quit        — Exit REPL");
            tunnel_println("  /help        — Show this help");
            tunnel_println("");
            tunnel_println("Shell:");
            tunnel_println("  ! <command>  — Run a shell command without leaving the REPL");
            tunnel_println("");
            tunnel_println("Auth (run outside REPL):");
            tunnel_println("  sidekar repl login   — OAuth login");
            tunnel_println("  sidekar repl logout  — Remove credentials");
            SlashResult::Continue
        }
        _ => unreachable!("checked by is_known_slash_command"),
    };

    Some(result)
}

/// Control flow result from applying a slash command to REPL state.
pub(super) enum SlashAction {
    /// Continue to next iteration of the main loop.
    Continue,
    /// Break out of the main loop.
    Quit,
}

/// Apply a `SlashResult` to mutable REPL state. Returns control flow instruction.
pub(super) async fn apply_slash_result(
    result: SlashResult,
    provider: &mut Option<Provider>,
    cred_name: &mut Option<String>,
    model: &mut Option<String>,
    history: &mut Vec<ChatMessage>,
    session_id: &mut String,
    tunnel_tx: &mut Option<crate::tunnel::TunnelSender>,
    tunnel_input_fd: &mut Option<i32>,
    bus_name: &str,
    cwd: &str,
    nick: &str,
    cached_ws: &mut Option<crate::providers::codex::CachedWs>,
) -> Result<SlashAction> {
    match result {
        SlashResult::Continue => {}
        SlashResult::Quit => return Ok(SlashAction::Quit),
        SlashResult::SwitchSession(new_id) => {
            *history = session::load_history(&new_id)?;
            let count = history.len();
            if count > 0 {
                tunnel_println(&format!(
                    "\x1b[2mSwitched to session ({count} messages).\x1b[0m"
                ));
            } else {
                tunnel_println("New session started.");
            }
            *session_id = new_id;
        }
        SlashResult::NeedProvider(action) => {
            let Some(prov) = provider.as_ref() else {
                tunnel_println("\x1b[33mSet a credential first: /credential <name>\x1b[0m");
                return Ok(SlashAction::Continue);
            };
            match action {
                SlashAsync::Compact => {
                    let Some(mdl) = model.as_deref() else {
                        tunnel_println("\x1b[33mSet a model first: /model <name>\x1b[0m");
                        return Ok(SlashAction::Continue);
                    };
                    run_compact(prov, mdl, history, session_id).await;
                }
                SlashAsync::InteractiveSelectModel => {
                    let cn = cred_name.as_deref().unwrap_or("?");
                    if let Some(selected) =
                        interactive_select_model(prov, cn, model.as_deref()).await
                    {
                        *model = Some(selected);
                    }
                }
            }
        }
        SlashResult::SetCredential(name) => {
            repl_status_dim(&format!("Resolving credential `{name}`…"));
            match build_provider(&name).await {
                Ok(prov) => {
                    let pt = prov.provider_type().to_string();
                    *provider = Some(prov);
                    *cred_name = Some(name.clone());
                    // Invalidate cached WS — old connection has stale auth
                    *cached_ws = None;
                    let email_info = providers::oauth::credential_email(&name)
                        .map(|e| format!(" <{e}>"))
                        .unwrap_or_default();
                    tunnel_println(&format!(
                        "Credential set: \x1b[1m{name}\x1b[0m ({pt}){email_info}"
                    ));
                    if model.is_none() {
                        tunnel_println("\x1b[2mUse /model list to select a model.\x1b[0m");
                    }
                }
                Err(e) => {
                    tunnel_println(&format!("\x1b[31mFailed to set credential: {e:#}\x1b[0m"));
                }
            }
        }
        SlashResult::SetModel(name) => {
            *model = Some(name.clone());
            tunnel_println(&format!("Model set: \x1b[1m{name}\x1b[0m"));
            if provider.is_none() {
                tunnel_println(
                    "\x1b[2mUse /credential <name> to set a credential first.\x1b[0m",
                );
            }
        }
        SlashResult::RelayOn => {
            if tunnel_tx.is_some() {
                tunnel_println("Relay is already on.");
            } else {
                let (tx, fd) = start_relay(bus_name, cwd, nick).await;
                if tx.is_some() {
                    *tunnel_tx = tx;
                    *tunnel_input_fd = fd;
                    tunnel_println("Relay: \x1b[32mon\x1b[0m");
                } else {
                    tunnel_println(
                        "\x1b[31mFailed to start relay. Are you logged in? (sidekar device login)\x1b[0m",
                    );
                }
            }
        }
        SlashResult::RelayOff => {
            if tunnel_tx.is_none() {
                tunnel_println("Relay is already off.");
            } else {
                stop_relay(tunnel_tx.take());
                *tunnel_input_fd = None;
                tunnel_println("Relay: \x1b[31moff\x1b[0m");
            }
        }
    }
    Ok(SlashAction::Continue)
}

pub(super) fn is_known_slash_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "/quit"
            | "/exit"
            | "/q"
            | "/new"
            | "/reset"
            | "/session"
            | "/credential"
            | "/model"
            | "/compact"
            | "/relay"
            | "/verbose"
            | "/help"
    )
}

/// Run interactive model selection. Returns `Some(model_id)` if a new model was picked.
pub(super) async fn interactive_select_model(
    prov: &Provider,
    cred_name: &str,
    current_model: Option<&str>,
) -> Option<String> {
    let pt = prov.provider_type();
    tunnel_println(&format!(
        "Fetching models for \x1b[1m{cred_name}\x1b[0m ({pt})..."
    ));
    let models = providers::fetch_model_list(pt, prov.api_key()).await;
    if models.is_empty() {
        tunnel_println("No models found.");
        return None;
    }
    let current = current_model.unwrap_or_default();
    tunnel_println("\nAvailable models (pick one to set):");
    for (i, m) in models.iter().enumerate() {
        let ctx = if m.context_window > 0 {
            format!("{}k ctx", m.context_window / 1000)
        } else {
            String::new()
        };
        let marker = if m.id == current { " (current)" } else { "" };
        tunnel_println(&format!(
            "  [{i}] \x1b[36m{}\x1b[0m  \x1b[2m{}{}{marker}\x1b[0m",
            m.id,
            m.display_name,
            if ctx.is_empty() {
                String::new()
            } else {
                format!(", {ctx}")
            }
        ));
    }
    print!("Enter number (or Enter to keep current): ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).is_ok() {
        let choice = line.trim();
        if choice.is_empty() {
            if !current.is_empty() {
                tunnel_println("\x1b[2mKeeping current model.\x1b[0m");
            }
            return None;
        } else if let Ok(idx) = choice.parse::<usize>() {
            if let Some(m) = models.get(idx) {
                tunnel_println(&format!(
                    "\x1b[32mModel set: {} \x1b[0m({})",
                    m.id, m.display_name
                ));
                return Some(m.id.clone());
            }
        }
        tunnel_println("Invalid selection.");
    }
    None
}

/// Run compaction. Returns true if history was compacted.
pub(super) async fn run_compact(
    prov: &Provider,
    mdl: &str,
    history: &mut Vec<ChatMessage>,
    session_id: &str,
) {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let renderer = std::sync::Arc::new(std::sync::Mutex::new(
        EventRenderer::new(cancel.clone()),
    ));
    let renderer_for_events = renderer.clone();
    let on_event: crate::agent::StreamCallback =
        Box::new(move |event: &StreamEvent| {
            if let Ok(mut guard) = renderer_for_events.lock() {
                guard.render(event);
            }
        });
    let changed = crate::agent::compaction::compact_now(
        prov,
        mdl,
        history,
        &on_event,
    )
    .await;
    if let Ok(mut guard) = renderer.lock() {
        guard.teardown();
    }
    if changed {
        let _ = session::replace_history(session_id, history);
        tunnel_println("\x1b[2m[session compacted]\x1b[0m");
    } else {
        tunnel_println("\x1b[2m[nothing to compact]\x1b[0m");
    }
    let _ = io::stdout().flush();
}

pub(super) async fn build_provider(cred_name: &str) -> Result<Provider> {
    let provider_type = providers::oauth::provider_type_for(cred_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown credential: '{cred_name}'. Names must start with 'claude', 'codex', 'or', or 'oc'."
            )
        })?;
    match provider_type {
        "anthropic" => {
            let api_key = providers::oauth::get_anthropic_token(Some(cred_name)).await?;
            Ok(Provider::anthropic(api_key))
        }
        "codex" => {
            let (api_key, account_id) = providers::oauth::get_codex_token(Some(cred_name)).await?;
            Ok(Provider::codex(api_key, account_id))
        }
        "openrouter" => {
            let api_key = providers::oauth::get_openrouter_token(Some(cred_name)).await?;
            Ok(Provider::openrouter(api_key))
        }
        "opencode" => {
            let api_key = providers::oauth::get_opencode_token(Some(cred_name)).await?;
            Ok(Provider::opencode(api_key))
        }
        _ => anyhow::bail!("Unknown provider type: {provider_type}"),
    }
}

pub(super) fn init_session(cwd: &str, model: &str) -> Result<(String, Vec<ChatMessage>)> {
    match session::latest_session(cwd)? {
        Some(s) => {
            let hist = session::load_history(&s.id)?;
            if !hist.is_empty() {
                broker::try_log_event(
                    "debug",
                    "session",
                    "resuming",
                    Some(&format!("{} messages", hist.len())),
                );
            }
            Ok((s.id, hist))
        }
        None => {
            let id = session::create_session(cwd, model, "anthropic")?;
            Ok((id, Vec::new()))
        }
    }
}
