use super::renderer::EventRenderer;
use super::*;

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
    LoadSkill(String),
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
    pub loaded_skills: &'a [String],
    /// Current conversation history. Used by /stats for live token
    /// accounting. Borrowed — never mutated by slash handling.
    pub history: &'a [providers::ChatMessage],
    /// Count of entries in the editor's input-line history (the
    /// up/down arrow buffer). Used by /stats to surface input-history
    /// growth; passed as a length rather than a slice to avoid moving
    /// the editor's state across the slash boundary.
    pub editor_input_history_len: usize,
    /// Per-session cumulative usage, touched on every Done event by
    /// the main on_event callback. Borrowed here (as an Arc+Mutex
    /// handle) so `/status` can produce a StatusView without copying.
    /// The mutex is intentionally separate from the renderer's mutex
    /// — see src/repl/turn_stats.rs for why.
    pub turn_stats: &'a std::sync::Arc<std::sync::Mutex<super::turn_stats::TurnStats>>,
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
        "/new" | "/reset" => match session::create_session(cwd, model, cred_name) {
            Ok(id) => SlashResult::SwitchSession(id),
            Err(e) => {
                broker::try_log_error("session", &format!("failed to create: {e}"), None);
                SlashResult::Continue
            }
        },
        "/session" => {
            // Fetch 20 rows annotated with message counts. We ask for
            // "only non-empty" so a directory with many abandoned
            // `/new` sessions doesn't push real sessions out of the
            // LIMIT. We still special-case the *current* session
            // below: it shows even when empty because the user may
            // have just created it and run /session to check state.
            // Log any DB failure so silent "No sessions found" doesn't
            // mask a real problem. Display-side still degrades
            // gracefully via the empty fallback.
            let non_empty = match session::list_sessions_with_counts(cwd, 20, true) {
                Ok(v) => v,
                Err(e) => {
                    broker::try_log_error(
                        "session",
                        &format!("failed to list: {e}"),
                        None,
                    );
                    Vec::new()
                }
            };
            // If the current session is empty it won't be in
            // `non_empty`; fetch it separately so we can still show
            // the "Current: …" header line.
            let current_info = if non_empty.iter().any(|s| s.session.id == *current_session) {
                None
            } else {
                session::list_sessions_with_counts(cwd, 20, false)
                    .ok()
                    .and_then(|all| {
                        all.into_iter().find(|s| s.session.id == *current_session)
                    })
            };
            let mut sessions = non_empty;
            // Truncate display to 10 rows after we know the current
            // session is accounted for — the extra headroom was for
            // the filter, not the display.
            sessions.truncate(10);

            if sessions.is_empty() && current_info.is_none() {
                tunnel_println("No sessions found.");
                SlashResult::Continue
            } else {
                // Print current session header. Either it's in the
                // filtered list (extract+remove so it doesn't show
                // twice), or it's the separately-fetched current_info
                // (empty session).
                let current_row = if let Some(idx) = sessions
                    .iter()
                    .position(|s| s.session.id == *current_session)
                {
                    Some(sessions.swap_remove(idx))
                } else {
                    current_info
                };
                // `now` is captured once and shared across every
                // relative-age format call, so entries printed in
                // the same listing get a consistent reference frame.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                if let Some(c) = current_row {
                    tunnel_println(&format!(
                        "Current: {} ({} msgs, {} ago, {})",
                        &c.session.id[..8],
                        c.messages,
                        session::format_relative_age(c.session.updated_at, now),
                        c.session.model
                    ));
                }
                if sessions.is_empty() {
                    tunnel_println("No other sessions.");
                    SlashResult::Continue
                } else {
                    tunnel_println("Pick session to switch:");
                    for (i, sc) in sessions.iter().enumerate() {
                        let s = &sc.session;
                        let name = s
                            .name
                            .as_deref()
                            .unwrap_or(&s.id[..s.id.len().min(8)]);
                        let cred = if s.provider.is_empty() {
                            "?"
                        } else {
                            s.provider.as_str()
                        };
                        let age =
                            session::format_relative_age(s.updated_at, now);
                        tunnel_println(&format!(
                            "  [{i}] {name} — {} msgs, {age} ago, {cred}/{}",
                            sc.messages, s.model
                        ));
                        // Second indented line: first 30 chars of
                        // the most recent user prompt. Rendered as
                        // dim-italic so the eye scans the metadata
                        // first. Skipped when no snippet exists (a
                        // tool-result-only session or, for the
                        // current-empty case, nothing has been sent).
                        if let Some(snip) = sc.last_prompt_snippet(30) {
                            tunnel_println(&format!(
                                "      \x1b[2m\"{snip}\"\x1b[0m"
                            ));
                        }
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
                            if let Some(sc) = sessions.get(idx) {
                                SlashResult::SwitchSession(sc.session.id.clone())
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
        "/skill" => {
            let parts: Vec<_> = input.split_whitespace().collect();
            match parts.get(1).copied() {
                None => {
                    if ctx.loaded_skills.is_empty() {
                        tunnel_println("No skills loaded this session.");
                        tunnel_println("\x1b[2mUse /skill list to see available, /skill <name> to load.\x1b[0m");
                    } else {
                        tunnel_println("Loaded skills (session-only):");
                        for name in ctx.loaded_skills {
                            tunnel_println(&format!("  {name}"));
                        }
                    }
                    SlashResult::Continue
                }
                Some("list") => {
                    let available = super::skills::list_skills();
                    if available.is_empty() {
                        tunnel_println("No skills found in standard agent skill dirs.");
                    } else {
                        tunnel_println("Available skills:");
                        for name in available {
                            let marker = if ctx.loaded_skills.iter().any(|s| s == &name) {
                                " (loaded)"
                            } else {
                                ""
                            };
                            tunnel_println(&format!("  {name}{marker}"));
                        }
                    }
                    SlashResult::Continue
                }
                Some(name) => SlashResult::LoadSkill(name.to_string()),
            }
        }
        "/stats" => {
            // /stats is a read-only snapshot: no provider call, no
            // history mutation. Safe to run at any time, including
            // during or right after a turn.
            let snap = super::stats::ResourceSnapshot::capture();
            let text = super::stats::format_stats(
                &snap,
                ctx.history,
                ctx.editor_input_history_len,
                model,
                cred_name,
                current_session,
            );
            tunnel_println(&text);
            SlashResult::Continue
        }
        "/status" => {
            // /status is user-facing: session age, cumulative
            // provider-reported token usage, context-window fill,
            // last response_id. All data is read-only; we briefly
            // acquire the turn_stats mutex for a snapshot and drop
            // it before rendering.
            //
            // Context-window size comes from the cached lookup — if
            // no turn has run on this model yet the window is shown
            // as "unknown" and the fill bar is suppressed. We don't
            // block on fetch_context_window because the REPL input
            // path must stay synchronous; users can run /status
            // again after the first turn completes.
            let snap_cum;
            let snap_last;
            let snap_turns;
            let snap_stop;
            let snap_rid;
            let snap_age;
            let snap_since;
            {
                let ts = ctx
                    .turn_stats
                    .lock()
                    .expect("turn_stats mutex poisoned");
                snap_cum = ts.cumulative.clone();
                snap_last = ts.last.clone();
                snap_turns = ts.turn_count;
                snap_stop = ts.last_stop_reason.clone();
                snap_rid = ts.last_response_id.clone();
                snap_age = ts.session_started_at.elapsed();
                snap_since = ts.last_turn_at.map(|t| t.elapsed());
            }
            let cw = providers::cached_context_window(model);
            let tokens_estimate =
                crate::agent::compaction::estimate_tokens_public(ctx.history);
            let view = super::status::StatusView {
                session_id: current_session,
                cwd,
                model,
                cred_name,
                context_window: cw,
                history_tokens_estimate: tokens_estimate,
                history_messages: ctx.history.len(),
                cumulative: &snap_cum,
                turn_count: snap_turns,
                last: snap_last.as_ref(),
                last_stop_reason: snap_stop.as_ref(),
                last_response_id: &snap_rid,
                session_age: snap_age,
                since_last_turn: snap_since,
            };
            let text = super::status::format_status(&view);
            tunnel_println(&text);
            SlashResult::Continue
        }
        "/help" => {
            tunnel_println("Slash commands:");
            tunnel_println("  /credential  — Show/set/list & select stored credentials");
            tunnel_println("  /model       — Show/set/list & select available models");
            tunnel_println("  /new         — Start fresh session");
            tunnel_println("  /session     — List and switch sessions");
            tunnel_println("  /skill       — Load a skill into the session system prompt");
            tunnel_println("  /compact     — Compact older session context now");
            tunnel_println("  /status      — Show session / model / token usage / context fill");
            tunnel_println("  /stats       — Show process diagnostics (RSS, CPU, threads)");
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
#[allow(clippy::too_many_arguments)]
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
    system_prompt: &mut String,
    loaded_skills: &mut Vec<String>,
    turn_stats: &std::sync::Arc<std::sync::Mutex<super::turn_stats::TurnStats>>,
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
            // Reset cumulative token tracking — the new session has
            // no turns associated with it. Without this /status would
            // carry over totals from the previous session, which
            // would be misleading and under-report the new one.
            if let Ok(mut ts) = turn_stats.lock() {
                *ts = super::turn_stats::TurnStats::new();
            }
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
                tunnel_println("\x1b[2mUse /credential <name> to set a credential first.\x1b[0m");
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
        SlashResult::LoadSkill(name) => {
            if loaded_skills.iter().any(|s| s == &name) {
                tunnel_println(&format!("Skill `{name}` already loaded."));
                return Ok(SlashAction::Continue);
            }
            let Some(path) = super::skills::find_skill(&name) else {
                tunnel_println(&format!(
                    "\x1b[31mSkill `{name}` not found.\x1b[0m Use /skill list to see available."
                ));
                return Ok(SlashAction::Continue);
            };
            match std::fs::read_to_string(&path) {
                Ok(body) => {
                    let body = body.trim();
                    system_prompt.push_str("\n## Skill: ");
                    system_prompt.push_str(&name);
                    system_prompt.push('\n');
                    system_prompt.push_str(body);
                    system_prompt.push('\n');
                    loaded_skills.push(name.clone());
                    tunnel_println(&format!(
                        "Loaded skill: \x1b[1m{name}\x1b[0m \x1b[2m({})\x1b[0m",
                        path.display()
                    ));
                }
                Err(e) => {
                    tunnel_println(&format!(
                        "\x1b[31mFailed to read skill `{name}`: {e}\x1b[0m"
                    ));
                }
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
            | "/stats"
            | "/status"
            | "/relay"
            | "/verbose"
            | "/skill"
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
    let models = providers::fetch_model_list_for_provider(prov).await;
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
        } else if let Ok(idx) = choice.parse::<usize>()
            && let Some(m) = models.get(idx)
        {
            tunnel_println(&format!(
                "\x1b[32mModel set: {} \x1b[0m({})",
                m.id, m.display_name
            ));
            return Some(m.id.clone());
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
    let renderer = std::sync::Arc::new(std::sync::Mutex::new(EventRenderer::new(cancel.clone())));
    let renderer_for_events = renderer.clone();
    let on_event: crate::agent::StreamCallback = Box::new(move |event: &StreamEvent| {
        if let Ok(mut guard) = renderer_for_events.lock() {
            guard.render(event);
        }
    });
    let changed = crate::agent::compaction::compact_now(prov, mdl, history, &on_event).await;
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
                "Unknown credential: '{cred_name}'. Names must start with 'claude', 'codex', 'or', 'oc', 'grok', 'gem', or 'oac'."
            )
        })?;
    let cred = Some(cred_name.to_string());
    match provider_type {
        "anthropic" => {
            let api_key = providers::oauth::get_anthropic_token(Some(cred_name)).await?;
            Ok(Provider::anthropic(api_key, cred))
        }
        "codex" => {
            let (api_key, account_id) = providers::oauth::get_codex_token(Some(cred_name)).await?;
            Ok(Provider::codex(api_key, account_id, cred))
        }
        "openrouter" => {
            let api_key = providers::oauth::get_openrouter_token(Some(cred_name)).await?;
            Ok(Provider::openrouter(api_key, cred))
        }
        "opencode" => {
            let api_key = providers::oauth::get_opencode_token(Some(cred_name)).await?;
            Ok(Provider::opencode(api_key, cred))
        }
        "grok" => {
            let api_key = providers::oauth::get_grok_token(Some(cred_name)).await?;
            Ok(Provider::grok(api_key, cred))
        }
        "gemini" => {
            let api_key = providers::oauth::get_gemini_token(Some(cred_name)).await?;
            Ok(Provider::gemini(api_key, cred))
        }
        "oac" => {
            let creds = providers::oauth::get_openai_compat_credentials(cred_name).await?;
            Ok(Provider::openai_compat(
                creds.api_key,
                creds.base_url,
                creds.name,
                cred,
            ))
        }
        _ => anyhow::bail!("Unknown provider type: {provider_type}"),
    }
}

