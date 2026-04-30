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
    /// Attach local MITM (same path as `--proxy`) for capturing API traffic into `proxy_log`.
    ProxyOn,
    ProxyOff,
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

/// Parsed line from a numbered REPL menu (`/session`, `/credential`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StdinMenuIndex {
    EofOrReadError,
    Blank,
    Index(usize),
    NotANumber,
}

fn read_stdin_menu_index(prompt: &str) -> StdinMenuIndex {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).is_err() {
        return StdinMenuIndex::EofOrReadError;
    }
    let choice = line.trim();
    if choice.is_empty() {
        return StdinMenuIndex::Blank;
    }
    match choice.parse::<usize>() {
        Ok(i) => StdinMenuIndex::Index(i),
        Err(_) => StdinMenuIndex::NotANumber,
    }
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
        "/new" | "/reset" => {
            // Terminate any live ExecSession processes before
            // switching conversations. The design doc
            // (context/unified-exec.md §Resolved design decisions)
            // spells this out: /new is a conversation reset, and
            // keeping a rogue `npm run dev` or similar alive
            // across resets would port-conflict and be surprising.
            //
            // Best-effort: terminate_all signals every session and
            // clears the registry; we don't wait for reapers to
            // finish (the reader threads wind down on their own).
            #[cfg(unix)]
            {
                let mgr = crate::agent::tools::exec_session_manager().clone();
                tokio::spawn(async move {
                    mgr.terminate_all().await;
                });
            }
            match session::create_session(cwd, model, cred_name) {
                Ok(id) => SlashResult::SwitchSession(id),
                Err(e) => {
                    broker::try_log_error("session", &format!("failed to create: {e}"), None);
                    SlashResult::Continue
                }
            }
        }
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
                    broker::try_log_error("session", &format!("failed to list: {e}"), None);
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
                    .and_then(|all| all.into_iter().find(|s| s.session.id == *current_session))
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
                        let name = s.name.as_deref().unwrap_or(&s.id[..s.id.len().min(8)]);
                        let cred = if s.provider.is_empty() {
                            "?"
                        } else {
                            s.provider.as_str()
                        };
                        let age = session::format_relative_age(s.updated_at, now);
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
                            tunnel_println(&format!("      \x1b[2m\"{snip}\"\x1b[0m"));
                        }
                    }
                    match read_stdin_menu_index("Enter number or Enter: ") {
                        StdinMenuIndex::Blank => {
                            tunnel_println("\x1b[2mStaying current.\x1b[0m");
                            SlashResult::Continue
                        }
                        StdinMenuIndex::Index(idx) => {
                            if let Some(sc) = sessions.get(idx) {
                                SlashResult::SwitchSession(sc.session.id.clone())
                            } else {
                                tunnel_println("Invalid.");
                                SlashResult::Continue
                            }
                        }
                        StdinMenuIndex::NotANumber => {
                            tunnel_println("Invalid.");
                            SlashResult::Continue
                        }
                        StdinMenuIndex::EofOrReadError => SlashResult::Continue,
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
                        match read_stdin_menu_index("Enter number or Enter: ") {
                            StdinMenuIndex::Blank => {
                                tunnel_println("\x1b[2mStaying current.\x1b[0m");
                                SlashResult::Continue
                            }
                            StdinMenuIndex::Index(idx) => {
                                if let Some((name, _)) = creds.get(idx) {
                                    SlashResult::SetCredential(name.clone())
                                } else {
                                    tunnel_println("Invalid.");
                                    SlashResult::Continue
                                }
                            }
                            StdinMenuIndex::NotANumber => {
                                tunnel_println("Invalid.");
                                SlashResult::Continue
                            }
                            StdinMenuIndex::EofOrReadError => SlashResult::Continue,
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
                        match read_stdin_menu_index("Enter number or Enter to cancel: ") {
                            StdinMenuIndex::Index(idx) => {
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
                            }
                            StdinMenuIndex::NotANumber => {
                                tunnel_println("Invalid.");
                            }
                            StdinMenuIndex::Blank | StdinMenuIndex::EofOrReadError => {}
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
        "/proxy" => {
            let arg = input.split_whitespace().nth(1).unwrap_or("");
            match arg {
                "on" | "true" | "1" => SlashResult::ProxyOn,
                "off" | "false" | "0" => SlashResult::ProxyOff,
                "" => match providers::shared_mitm_proxy_port() {
                    Some(p) => {
                        tunnel_println(&format!(
                            "MITM proxy: \x1b[32mon\x1b[0m → 127.0.0.1:{p} (`sidekar proxy log`)"
                        ));
                        SlashResult::Continue
                    }
                    None => {
                        tunnel_println("MITM proxy: off");
                        SlashResult::Continue
                    }
                },
                _ => {
                    tunnel_println("Usage: /proxy [on|off]");
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
        "/journal" => {
            // Slash interface to the journaling subsystem.
            //
            // Subcommands:
            //   /journal                    show on/off state + cmd help
            //   /journal on|off|...         toggle session-level state
            //   /journal list [N]           last N journals for this project
            //                               (default 10, max 50)
            //   /journal show <id>          full structured view of a single row
            //   /journal now                force an immediate journaling pass
            //                               (bypasses the idle threshold)
            //
            // Precedence of the toggle is documented in the /journal
            // on|off branch; same chain (--journal > env > slash >
            // config > default-on) used everywhere else in the code.
            let parts: Vec<&str> = input.split_whitespace().collect();
            let sub = parts.get(1).copied().unwrap_or("");

            match sub {
                "" => {
                    let state = if crate::runtime::journal() {
                        "on"
                    } else {
                        "off"
                    };
                    tunnel_println(&format!("Journal: {state}"));
                    tunnel_println(
                        "\x1b[2mSubcommands: on|off | list [N] | show <id> | now\x1b[0m",
                    );
                    tunnel_println(
                        "\x1b[2mPersist across launches: sidekar config set journal true|false\x1b[0m",
                    );
                }
                "list" => {
                    let n = parts
                        .get(2)
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(10)
                        .min(50);
                    let project = crate::scope::resolve_project_name(Some(cwd));
                    match crate::repl::journal::store::recent_for_project(&project, n) {
                        Ok(rows) if rows.is_empty() => {
                            tunnel_println("No journals yet for this project.");
                        }
                        Ok(rows) => {
                            // Two-column rendering: id + age + headline.
                            // Age is relative to now for scan-ability; id
                            // is the numeric key used by `show`.
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs_f64())
                                .unwrap_or(0.0);
                            for r in rows {
                                let age = crate::session::format_relative_age(r.created_at, now);
                                let head = if r.headline.is_empty() {
                                    "(no headline)"
                                } else {
                                    r.headline.as_str()
                                };
                                tunnel_println(&format!("  [{id:>4}] {age:>8}  {head}", id = r.id));
                            }
                        }
                        Err(e) => {
                            tunnel_println(&format!("\x1b[31m/journal list failed: {e:#}\x1b[0m"));
                        }
                    }
                }
                "show" => {
                    // `/journal show <id>` — render the full 12-section
                    // structured view of a single journal row. id is the
                    // numeric key from `/journal list`. Rendering reuses
                    // the inject module's render path (single-row slice)
                    // so the format matches what the model sees on resume.
                    let id_arg = parts.get(2).copied().unwrap_or("");
                    let Ok(id) = id_arg.parse::<i64>() else {
                        tunnel_println("Usage: /journal show <id>");
                        return Some(SlashResult::Continue);
                    };
                    match crate::repl::journal::store::get_by_id(id) {
                        Ok(Some(row)) => {
                            tunnel_println(&render_journal_show(&row));
                        }
                        Ok(None) => {
                            tunnel_println(&format!("No journal with id {id}."));
                        }
                        Err(e) => {
                            tunnel_println(&format!("\x1b[31m/journal show failed: {e:#}\x1b[0m"));
                        }
                    }
                }
                "now" => {
                    // Force an immediate journaling pass, bypassing the
                    // idle-threshold wait. Same run_once entry point the
                    // background loop uses. Runs on the current tokio
                    // runtime; blocks the slash handler until complete
                    // so the user sees the outcome inline.
                    //
                    // Building the Context requires Provider, which we
                    // don't have a cheap handle to from the slash layer.
                    // Punt with a helpful pointer — `sidekar journal
                    // now` (CLI) reconstructs the provider and can do
                    // this without live REPL state.
                    tunnel_println(
                        "/journal now runs automatically on idle. \
                         To force from outside the REPL, use \
                         `sidekar journal now`.",
                    );
                }
                other => {
                    if let Some(parsed) = crate::runtime::parse_bool_arg(other) {
                        crate::runtime::set_journal(parsed);
                        tunnel_println(&format!("Journal: {}", if parsed { "on" } else { "off" }));
                    } else {
                        tunnel_println("Usage: /journal [on|off | list [N] | show <id> | now]");
                    }
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
                        tunnel_println(
                            "\x1b[2mUse /skill list to see available, /skill <name> to load.\x1b[0m",
                        );
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
                let ts = ctx.turn_stats.lock().expect("turn_stats mutex poisoned");
                snap_cum = ts.cumulative.clone();
                snap_last = ts.last.clone();
                snap_turns = ts.turn_count;
                snap_stop = ts.last_stop_reason.clone();
                snap_rid = ts.last_response_id.clone();
                snap_age = ts.session_started_at.elapsed();
                snap_since = ts.last_turn_at.map(|t| t.elapsed());
            }
            let cw = providers::cached_context_window(model);
            let tokens_estimate = crate::agent::compaction::estimate_tokens_public(ctx.history);
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
                journal_on: crate::runtime::journal(),
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
                "  /proxy       — Toggle MITM capture for streaming API (`sidekar proxy log`)",
            );
            tunnel_println("  /journal     — Toggle background session journaling (on/off)");
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
        SlashResult::ProxyOn => {
            if providers::shared_mitm_proxy_port().is_some() {
                tunnel_println("MITM proxy is already on.");
            } else {
                match crate::proxy::start(crate::runtime::verbose()).await {
                    Ok((port, ca_path)) => match std::fs::read(&ca_path) {
                        Ok(ca_pem) => {
                            providers::attach_shared_mitm_proxy(port, ca_pem, ca_path);
                            tunnel_println(&format!(
                                "MITM proxy: \x1b[32mon\x1b[0m → 127.0.0.1:{port} (`sidekar proxy log`)"
                            ));
                        }
                        Err(e) => tunnel_println(&format!(
                            "\x1b[31mFailed to read ephemeral CA PEM: {e:#}\x1b[0m"
                        )),
                    },
                    Err(e) => tunnel_println(&format!(
                        "\x1b[31mFailed to start MITM proxy: {e:#}\x1b[0m"
                    )),
                }
            }
        }
        SlashResult::ProxyOff => {
            if providers::shared_mitm_proxy_port().is_none() {
                tunnel_println("MITM proxy is already off.");
            } else {
                providers::detach_shared_mitm_proxy();
                tunnel_println(
                    "MITM proxy: \x1b[31moff\x1b[0m (clients built after this use direct TLS)",
                );
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

/// Render a single journal row for `/journal show`. Mirrors the
/// structure of the resume-injection block (same field ordering,
/// same skip-empty-field rule), but styled for interactive
/// reading rather than model consumption — headers are ANSI-
/// formatted, the framing directive is replaced with a terse
/// metadata line.
fn render_journal_show(row: &crate::repl::journal::store::JournalRow) -> String {
    use std::fmt::Write;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let age = crate::session::format_relative_age(row.created_at, now);

    let outcome = crate::repl::journal::parse::parse_response(&row.structured_json);
    let j = outcome.journal;

    let mut out = String::with_capacity(1024);
    let _ = writeln!(
        out,
        "\x1b[1mJournal [{id}]\x1b[0m  {age}  session={sid}",
        id = row.id,
        sid = &row.session_id[..row.session_id.len().min(8)],
    );
    let _ = writeln!(
        out,
        "\x1b[2mmodel={m} cred={c} tokens_in={ti} tokens_out={to}\x1b[0m",
        m = row.model_used,
        c = row.cred_used,
        ti = row.tokens_in,
        to = row.tokens_out,
    );
    if outcome.was_degraded {
        let _ = writeln!(
            out,
            "\x1b[33m(parse was degraded: {r})\x1b[0m",
            r = outcome.reason
        );
    }

    // Reuse the field rendering logic locally (inject's helpers
    // are private to that module). Kept simple here — duplication
    // of ~20 lines beats widening inject.rs's API surface for a
    // single non-prompt consumer.
    let emit_str = |out: &mut String, label: &str, value: &str| {
        let v = value.trim();
        if !v.is_empty() {
            let _ = writeln!(out, "\x1b[1m{label}:\x1b[0m {v}");
        }
    };
    let emit_list = |out: &mut String, label: &str, vs: &[String]| {
        let any = vs.iter().any(|v| !v.trim().is_empty());
        if !any {
            return;
        }
        let _ = writeln!(out, "\x1b[1m{label}:\x1b[0m");
        for v in vs {
            let v = v.trim();
            if !v.is_empty() {
                let _ = writeln!(out, "  - {v}");
            }
        }
    };

    emit_str(&mut out, "Active task", &j.active_task);
    emit_str(&mut out, "Goal", &j.goal);
    emit_list(&mut out, "Constraints", &j.constraints);
    emit_list(&mut out, "Completed", &j.completed);
    emit_str(&mut out, "Active state", &j.active_state);
    emit_list(&mut out, "In progress", &j.in_progress);
    emit_list(&mut out, "Blocked", &j.blocked);
    emit_list(&mut out, "Decisions", &j.decisions);
    emit_list(&mut out, "Resolved questions", &j.resolved_questions);
    emit_list(&mut out, "Pending user asks", &j.pending_user_asks);
    emit_list(&mut out, "Relevant files", &j.relevant_files);
    emit_str(&mut out, "Critical context", &j.critical_context);
    out
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
            | "/journal"
            | "/relay"
            | "/proxy"
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
    let models = match providers::fetch_model_list_for_provider(prov).await {
        Ok(m) => m,
        Err(err) => {
            tunnel_println(&format!("\x1b[31mError listing models: {err}\x1b[0m"));
            tunnel_println("Type a model name directly (or Enter to cancel):");
            print!("> ");
            let _ = io::stdout().flush();
            let mut line = String::new();
            if io::stdin().lock().read_line(&mut line).is_ok() {
                let name = line.trim();
                if !name.is_empty() {
                    tunnel_println(&format!("\x1b[32mModel set: {name}\x1b[0m"));
                    return Some(name.to_string());
                }
            }
            return None;
        }
    };
    if models.is_empty() {
        tunnel_println("No models returned by provider.");
        tunnel_println("Type a model name directly (or Enter to cancel):");
        print!("> ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line).is_ok() {
            let name = line.trim();
            if !name.is_empty() {
                tunnel_println(&format!("\x1b[32mModel set: {name}\x1b[0m"));
                return Some(name.to_string());
            }
        }
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
            tunnel_println("Invalid index.");
        } else {
            // Not a number — treat as a model name typed directly.
            tunnel_println(&format!("\x1b[32mModel set: {choice}\x1b[0m"));
            return Some(choice.to_string());
        }
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

pub async fn build_provider(cred_name: &str) -> Result<Provider> {
    let provider_type =
        providers::oauth::resolve_provider_type_for_credential(cred_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown credential '{cred_name}'. Expected a nicknamed key (e.g. claude-work) or default stem (anthropic, codex, gem, oac-…); see `sidekar repl login --help`."
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
        "opencode-go" => {
            let api_key = providers::oauth::get_opencode_go_token(Some(cred_name)).await?;
            Ok(Provider::opencode_go(api_key, cred))
        }
        "grok" => {
            let api_key = providers::oauth::get_grok_token(Some(cred_name)).await?;
            Ok(Provider::grok(api_key, cred))
        }
        "gemini" => {
            let api_key = providers::oauth::get_gemini_token(Some(cred_name)).await?;
            Ok(Provider::gemini(api_key, cred))
        }
        "bedrock" => {
            let b = providers::oauth::load_bedrock_stored(cred_name)?;
            Ok(Provider::bedrock(b.region, b.aws_profile))
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
