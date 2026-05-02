use anyhow::Result;
use std::collections::hash_map::Entry;
use std::io::{self, BufRead, Write};

mod editor;
mod event_forward;
mod relay;
mod renderer;
mod shell_escape;
mod skills;
// pub(crate) so external modules (e.g. `commands/journal.rs` which
// powers the `sidekar journal` CLI) can reach the store and parse
// layers. Visibility of individual items is still controlled per-
// module; the outer walls are the access gate.
pub(crate) mod journal;
mod ratelimit;
pub(crate) mod slash;
mod spinner;
mod stats;
mod status;
mod system_prompt;
mod turn_stats;
mod user_turn;

use self::editor::{
    ActivePromptSession, EscCancelWatcher, InputEvent, LineEditor, clear_transient_status,
    emit_shared_line, emit_shared_output, emit_transient_status, print_banner, read_input_or_bus,
};
use self::relay::{inject_bus_messages, start_relay, stop_relay};
use self::renderer::EventRenderer;
use self::slash::{
    SlashAction, SlashContext, apply_slash_result, build_provider, handle_slash_command,
};
use self::system_prompt::build_system_prompt_with_project;
use crate::broker;
use crate::message::AgentId;
use crate::providers::{self, ChatMessage, ContentBlock, Provider, Role, StreamEvent};
use crate::session;
use crate::tunnel::tunnel_println;

const REPL_INPUT_HISTORY_LIMIT: usize = 500;

/// Resolve a credential nickname into a [`Provider`] (shared by REPL and `repl models`).
pub async fn provider_from_credential(cred_name: &str) -> anyhow::Result<Provider> {
    build_provider(cred_name).await
}

fn repl_status_dim(msg: &str) {
    tunnel_println(&format!("\x1b[2m{msg}\x1b[0m"));
}

async fn maybe_run_final_journal(ctx: Option<&self::journal::task::Context>) {
    let Some(ctx) = ctx else {
        return;
    };

    match self::journal::task::run_once(ctx).await {
        self::journal::task::Outcome::Persisted { id, .. } => {
            if crate::runtime::verbose() {
                repl_status_dim(&format!("[final journal #{id} written]"));
            }
        }
        self::journal::task::Outcome::Failed(e) => {
            crate::broker::try_log_error("journal", &format!("final flush failed: {e:#}"), None);
        }
        self::journal::task::Outcome::SkippedJournalOff
        | self::journal::task::Outcome::SkippedOverBudget { .. }
        | self::journal::task::Outcome::SkippedEmptySlice
        | self::journal::task::Outcome::SkippedLowSignal { .. } => {}
    }
}

/// Resolve a session from the resume option, creating a fresh one if needed.
/// `credential` is stored on new sessions so `/sessions` can show which
/// credential authored them.
fn resolve_session(
    cwd: &str,
    model: &str,
    credential: &str,
    resume: Option<&Option<String>>,
) -> Result<(String, Vec<ChatMessage>)> {
    match resume {
        Some(Some(sid)) => match session::find_session_by_prefix(sid)? {
            Some(s) => {
                let hist = session::load_history(&s.id)?;
                broker::try_log_event(
                    "debug",
                    "session",
                    "resumed",
                    Some(&format!(
                        "{} ({} messages)",
                        &s.id[..s.id.len().min(8)],
                        hist.len()
                    )),
                );
                Ok((s.id, hist))
            }
            None => anyhow::bail!("No session matching '{sid}'"),
        },
        Some(None) => match session::latest_session(cwd)? {
            Some(s) => {
                let hist = session::load_history(&s.id)?;
                Ok((s.id, hist))
            }
            None => {
                let id = session::create_session(cwd, model, credential)?;
                Ok((id, Vec::new()))
            }
        },
        None => {
            let id = session::create_session(cwd, model, credential)?;
            Ok((id, Vec::new()))
        }
    }
}

/// REPL options parsed from CLI flags.
pub struct ReplOptions {
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub credential: Option<String>,
    pub verbose: bool,
    /// None = fresh session, Some(None) = interactive picker, Some(Some(id)) = resume specific session
    pub resume: Option<Option<String>>,
    pub relay_override: Option<bool>,
    pub proxy_override: Option<bool>,
    /// Tri-state CLI override for background session journaling.
    /// `Some(true)` = `--journal`, `Some(false)` = `--no-journal`,
    /// `None` = fall through to env / config / built-in default.
    /// Parsed in `main/repl_cmd.rs::handle_run` alongside the
    /// verbose/relay/proxy flags.
    pub journal_override: Option<bool>,
}

/// Entry point for the REPL.
pub async fn run_with_options(opts: ReplOptions) -> Result<()> {
    crate::runtime::init_with_journal(opts.verbose, opts.journal_override);

    // Eagerly initialize the broker schema so the first hot-path call (the
    // bus poller in `read_input_or_bus`) doesn't pay for it. Then kick off
    // an opportunistic VACUUM in the background — long-running REPL users
    // who never run `sidekar daemon` would otherwise accumulate hundreds of
    // megabytes of freed pages without ever reclaiming them.
    if let Err(e) = crate::broker::init_db() {
        crate::broker::try_log_error("broker", &format!("init_db failed: {e:#}"), None);
    }
    std::thread::spawn(|| {
        if let Ok(true) = crate::broker::maybe_vacuum(0.30) {
            crate::broker::try_log_event("info", "broker", "vacuumed bloated db", None);
        }
    });

    // Start the MITM proxy for in-process streaming provider calls if requested.
    // Mirrors PTY proxy semantics: explicit --proxy / --no-proxy wins, otherwise
    // falls back to the SIDEKAR_PROXY env var. The CA PEM is loaded and handed
    // to `providers::attach_shared_mitm_proxy`, which `build_streaming_client`
    // reads on every provider request.
    let proxy_enabled = match opts.proxy_override {
        Some(v) => v,
        None => std::env::var("SIDEKAR_PROXY").is_ok(),
    };
    if proxy_enabled {
        match crate::proxy::start(opts.verbose).await {
            Ok((port, ca_path)) => match std::fs::read(&ca_path) {
                Ok(ca_pem) => {
                    providers::attach_shared_mitm_proxy(port, ca_pem, ca_path);
                    repl_status_dim(&format!(
                        "MITM proxy attached on 127.0.0.1:{port} (payloads in `sidekar proxy log`)"
                    ));
                }
                Err(e) => {
                    crate::broker::try_log_error(
                        "proxy",
                        &format!("failed to read CA at {}: {e:#}", ca_path.display()),
                        None,
                    );
                }
            },
            Err(e) => {
                crate::broker::try_log_error("proxy", &format!("failed to start: {e:#}"), None);
            }
        }
    }

    // Credential and model are optional — user can set them interactively.
    let mut cred_name: Option<String> = opts.credential;
    let mut model: Option<String> = opts.model.or_else(|| std::env::var("SIDEKAR_MODEL").ok());

    // Validate credential name if provided at startup
    if let Some(ref name) = cred_name
        && providers::oauth::resolve_provider_type_for_credential(name).is_none()
    {
        anyhow::bail!(
            "Unknown credential: '{name}'. Use a nicknamed key (e.g. claude-work) or default stem (anthropic, codex, gem, oac-…).\n\
                 Examples: claude, claude-1, codex, codex-work, or-personal, anthropic, gem-work, oac-lab\n\
                 Login with: sidekar repl login {name}"
        );
    }

    // Build provider if credential is available
    let mut provider: Option<Provider> = match cred_name.as_deref() {
        Some(name) => {
            repl_status_dim(&format!("Loading credential `{name}`…"));
            Some(build_provider(name).await?)
        }
        None => None,
    };

    let prompt = opts.prompt;

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    // Compute project name up front so `build_system_prompt_with_project`
    // can inject any prior-session journals for this project. Same
    // identifier the journaling task uses when inserting rows, so
    // the lookup hits the right bucket.
    let scope_project = crate::scope::resolve_project_name(Some(&cwd));
    let mut system_prompt = build_system_prompt_with_project(Some(&scope_project));
    let mut loaded_skills: Vec<String> = Vec::new();
    let tool_defs = crate::agent::tools::definitions();

    // Register on the bus (same pattern as PTY/bus do_register)
    let project = crate::bus::detect_project_name();
    let nick = crate::bus::pick_nickname_for_project(Some(&project));
    let pane_id = format!("repl-{}", std::process::id());

    let existing_names: std::collections::HashSet<String> = broker::list_agents(None)
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.id.name)
        .collect();
    let mut n = 1u32;
    let bus_name = loop {
        let candidate = format!("sidekar-repl-{project}-{n}");
        if !existing_names.contains(&candidate) {
            break candidate;
        }
        n += 1;
    };

    let identity = AgentId {
        name: bus_name.clone(),
        nick: Some(nick.clone()),
        session: Some(cwd.clone()),
        pane: Some(pane_id.clone()),
        agent_type: Some("sidekar-repl".to_string()),
    };

    if let Err(e) = broker::register_agent(&identity, Some(&pane_id)) {
        broker::try_log_error("bus", &format!("registration failed: {e}"), None);
    }

    crate::bus::set_terminal_title(&format!("{nick} - sidekar repl"));

    // SAFETY: called once during serial startup, before spawning async tasks.
    unsafe { std::env::set_var("SIDEKAR_AGENT_NAME", &bus_name) };
    crate::runtime::set_agent_name(Some(bus_name.clone()));

    // Relay tunnel (web terminal access)
    let _ = rustls::crypto::ring::default_provider().install_default();
    let relay_policy = match opts.relay_override {
        Some(true) => crate::config::RelayMode::On,
        Some(false) => crate::config::RelayMode::Off,
        None => crate::config::relay_mode(),
    };
    let (mut tunnel_tx, mut tunnel_input_fd) = if relay_policy == crate::config::RelayMode::On {
        start_relay(&bus_name, &cwd, &nick).await
    } else {
        (None, None)
    };

    let cron_project = crate::scope::resolve_project_name(None);
    crate::commands::cron::start_default_cron_loop(bus_name.clone(), cron_project).await;

    // Nudge unanswered outbound bus requests on the same schedule as PTY mode.
    crate::poller::start_nudger(bus_name.clone());

    // Single-prompt mode: one turn, exit. Honors -r/--resume-session so the
    // prompt is appended to an existing session's history.
    if let Some(input) = prompt {
        let Some(ref prov) = provider else {
            anyhow::bail!("Single-prompt mode requires -c <credential>");
        };
        let Some(ref mdl) = model else {
            anyhow::bail!("Single-prompt mode requires -m <model>");
        };
        let cred_tag = cred_name.as_deref().unwrap_or("");
        let (session_id, mut history) = resolve_session(&cwd, mdl, cred_tag, opts.resume.as_ref())?;
        let prompt_text = input.clone();
        let user_msg = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: input }],
        };
        let user_entry_id = session::append_message(&session_id, &user_msg).ok();
        history.push(user_msg);

        let relevant_memory = crate::memory::relevant_brief(&scope_project, &prompt_text, 5)
            .unwrap_or(crate::memory::RelevantMemoryBrief {
                text: String::new(),
                ids: Vec::new(),
            });
        let mut turn_system_prompt = system_prompt.clone();
        if !relevant_memory.text.trim().is_empty() {
            turn_system_prompt.push('\n');
            turn_system_prompt.push_str(&relevant_memory.text);
            turn_system_prompt.push('\n');
        }

        let mut prev_resp_id: Option<String> = None;
        let mut cached_ws: Option<providers::codex::CachedWs> = None;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _cancel_watch = EscCancelWatcher::start(cancel.clone(), tunnel_input_fd);
        let renderer =
            std::sync::Arc::new(std::sync::Mutex::new(EventRenderer::new(cancel.clone())));
        let renderer_for_events = renderer.clone();
        // Forwarder is lock-free (atomic state), shared by Arc so the
        // per-event callback doesn't acquire a second mutex. Hot-path
        // reason: `TextDelta` fires ~50-80/s during a streaming
        // response and the callback runs synchronously on the main
        // task; every extra `Mutex::lock()` here competes with the
        // STDIN worker thread's editor lock and adds to typing lag.
        let forwarder = std::sync::Arc::new(self::event_forward::EventForwarder::new());
        let forwarder_for_events = forwarder.clone();
        let on_event: crate::agent::StreamCallback = Box::new(move |event: &StreamEvent| {
            if let Ok(mut guard) = renderer_for_events.lock() {
                guard.render(event);
            }
            forwarder_for_events.forward(event);
        });

        let pre_len = history.len();
        let run_result = crate::agent::run(
            prov,
            mdl,
            &turn_system_prompt,
            &mut history,
            &tool_defs,
            on_event,
            Some(&cancel),
            Some(&session_id),
            &mut prev_resp_id,
            &mut cached_ws,
        )
        .await;
        if let Ok(mut guard) = renderer.lock() {
            guard.teardown();
        }

        if crate::runtime::verbose() && run_result.is_ok() {
            repl_status_dim("[turn complete]");
        }

        let run_ok = run_result.is_ok();
        let did_compact = run_result?;
        if did_compact {
            let _ = session::replace_history(&session_id, &history);
        } else if pre_len < history.len() {
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
        }
        if run_ok {
            let _ = crate::memory::accept_selected_memories(
                &relevant_memory.ids,
                &session_id,
                user_entry_id.as_deref(),
                &prompt_text,
            );
        }

        let journal_ctx = self::journal::task::Context {
            provider: std::sync::Arc::new(prov.clone()),
            session_id: session_id.clone(),
            project: scope_project.clone(),
            model: mdl.clone(),
            cred_name: cred_name.clone().unwrap_or_default(),
        };
        maybe_run_final_journal(Some(&journal_ctx)).await;

        stop_relay(tunnel_tx.take());
        crate::poller::shutdown_poller();
        let _ = broker::unregister_agent(&bus_name);
        return Ok(());
    }

    let model_for_session = model.as_deref().unwrap_or("(not set)");
    let cred_tag = cred_name.as_deref().unwrap_or("");

    // Default: fresh session. -r to resume.
    let (mut session_id, mut history) =
        resolve_session(&cwd, model_for_session, cred_tag, opts.resume.as_ref())?;

    print_banner(model.as_deref(), cred_name.as_deref());

    let scope_root = crate::scope::resolve_project_root(Some(&cwd));
    // Reuse the project name already resolved above for system-
    // prompt injection — calling `resolve_project_name` twice
    // would return the same value but wastes a scope walk.
    let scope_name = scope_project.clone();
    let mut line_editor = LineEditor::with_history(
        session::load_input_history(&scope_root, REPL_INPUT_HISTORY_LIMIT).unwrap_or_default(),
    );

    // Stateful chaining: persists the codex response ID across turns so
    // subsequent calls can use previous_response_id for delta-only input.
    let mut prev_resp_id: Option<String> = None;
    // Persistent WS connection for Codex provider — reused across turns so
    // the server can correlate requests and cache prompt prefixes.
    let mut cached_ws: Option<providers::codex::CachedWs> = None;

    // Cumulative token-usage accumulator, surfaced via `/status`. Lives
    // for the full REPL session so cumulative counts reflect every
    // turn (not just the most recent). Wrapped in Arc<Mutex<..>> so
    // the per-turn on_event callback can update it without moving
    // ownership out of the REPL loop.
    //
    // On session switch (`/new`, `/session`) we reset this — the
    // switch handlers below replace it via `TurnStats::new()`.
    let turn_stats = std::sync::Arc::new(std::sync::Mutex::new(self::turn_stats::TurnStats::new()));

    // Idle tracker for the background journaling subsystem.
    // - Armed at StreamEvent::Done in the event callback below.
    // - Disarmed right before we block on input reading, and again
    //   after the read returns (any input, bus message, or EOF).
    // - Polled by the background journaling task (step 7) which
    //   fires an LLM summarization when the gap exceeds threshold.
    //
    // Always instantiated even when runtime::journal() is off — it's
    // cheap (two Options + a mutex), and keeping it present means
    // `/journal on` mid-session starts journaling on the very next
    // idle window without re-threading anything.
    let idle_tracker = std::sync::Arc::new(self::journal::IdleTracker::new());

    // Handle for the background journaling polling task. Lazily
    // spawned on the first turn — at that point we know provider
    // and model are populated, which the journaler needs. We
    // capture them at spawn time; a later `/model` or `/credential`
    // switch does NOT re-spawn. Rationale:
    //   - The original provider/credential is still valid after a
    //     switch; journaling with "the old pair" is fine.
    //   - Re-spawning on every switch is a lot of plumbing for a
    //     minor fidelity win; `/new` creates a fresh REPL loop
    //     (via SwitchSession) which spawns a fresh task with the
    //     then-current values.
    // On REPL exit the handle is aborted to stop the poll loop.
    let mut journal_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut journal_ctx: Option<self::journal::task::Context> = None;

    loop {
        // Disarm before we block on stdin — an about-to-type user
        // is "active," even before the first keystroke. This also
        // closes the window between a Done and a follow-up command:
        // the main loop re-enters read_input_or_bus right after the
        // agent returns, so the journaling task only fires if the
        // user genuinely went idle, not merely "between turns."
        idle_tracker.disarm();

        let input = match read_input_or_bus(&bus_name, &mut line_editor, tunnel_input_fd) {
            InputEvent::User(s) => Some(s),
            InputEvent::Bus => None, // no user text — bus messages trigger the agent
            InputEvent::Eof => break,
        };

        // Defensive second disarm: if the background task fired
        // between the block above and whatever branch below does
        // the actual agent.run call, that's fine — record_fired()
        // already suppresses retries. But a user could also type
        // *during* a journaling pass (tokio task runs concurrently
        // with the input reader on separate threads); disarming
        // here makes the intent explicit.
        idle_tracker.disarm();

        let mut staged_user_content: Option<Vec<ContentBlock>> = None;

        if let Some(ref sub) = input {
            let trimmed = sub.text.trim();
            if trimmed.is_empty() {
                continue;
            }

            let mut content = match user_turn::build_user_turn_content(&sub.text, &sub.image_paths)
            {
                Ok(c) if !c.is_empty() => c,
                Ok(_) => continue,
                Err(e) => {
                    tunnel_println(&format!("\x1b[31m{e:#}\x1b[0m"));
                    continue;
                }
            };
            if let Err(e) = user_turn::finalize_multimodal_for_api(&mut content) {
                tunnel_println(&format!("\x1b[31m{e:#}\x1b[0m"));
                continue;
            }
            staged_user_content = Some(content);

            // Record to both in-memory (for up-arrow) and SQLite (for next session)
            line_editor.push_history(&sub.text);
            let _ = session::append_input_history(
                &scope_root,
                &scope_name,
                &sub.text,
                REPL_INPUT_HISTORY_LIMIT,
            );

            // Shell escape: "! cmd" runs cmd in a subprocess
            if let Some(cmd) = trimmed.strip_prefix('!') {
                let cmd = cmd.trim();
                if cmd.is_empty() {
                    tunnel_println("\x1b[2mUsage: ! <command>\x1b[0m");
                } else {
                    shell_escape::run(cmd);
                }
                continue;
            }

            // Slash commands
            let slash_ctx = SlashContext {
                input: trimmed,
                cwd: &cwd,
                model: model.as_deref().unwrap_or("(not set)"),
                session_id: &session_id,
                cred_name: cred_name.as_deref().unwrap_or("(none)"),
                loaded_skills: &loaded_skills,
                history: &history,
                editor_input_history_len: line_editor.input_history_len(),
                turn_stats: &turn_stats,
            };
            if let Some(result) = handle_slash_command(&slash_ctx) {
                match apply_slash_result(
                    result,
                    &mut provider,
                    &mut cred_name,
                    &mut model,
                    &mut history,
                    &mut session_id,
                    &mut tunnel_tx,
                    &mut tunnel_input_fd,
                    &bus_name,
                    &cwd,
                    &nick,
                    &mut cached_ws,
                    &mut system_prompt,
                    &mut loaded_skills,
                    &turn_stats,
                )
                .await?
                {
                    SlashAction::Continue => continue,
                    SlashAction::Quit => break,
                }
            }
        }

        // Guard: need provider + model to run the agent.
        // Bus wakeups reuse this loop often; printing here would flood the terminal
        // (hint includes `/model <name>`) while the credential is OK but `-m`/model unset.
        if provider.is_none() || model.is_none() {
            if staged_user_content.is_some() {
                let mut missing = Vec::new();
                if provider.is_none() {
                    missing.push("/credential <name>");
                }
                if model.is_none() {
                    missing.push("/model <name>");
                }
                tunnel_println(&format!(
                    "\x1b[33mSet {} before sending messages.\x1b[0m",
                    missing.join(" and ")
                ));
            }
            continue;
        }

        // Inject pending bus messages as steering
        let bus_injected = inject_bus_messages(&bus_name, &mut history, &session_id);
        if input.is_none() && bus_injected == 0 {
            continue;
        }

        // Re-resolve the provider before each turn so OAuth tokens near expiry
        // get refreshed via the stored refresh_token (see providers::oauth).
        // `Provider::Anthropic { api_key, .. }` is a snapshot, so without this
        // step long-idle sessions 401 until the user re-picks the credential.
        if let Some(ref name) = cred_name {
            match build_provider(name).await {
                Ok(p) => provider = Some(p),
                Err(e) => {
                    tunnel_println(&format!(
                        "\x1b[31mCredential `{name}` failed to resolve: {e:#}\x1b[0m"
                    ));
                    continue;
                }
            }
        }

        let prov = provider.as_ref().expect("guarded above");
        let mdl = model.as_ref().expect("guarded above");

        // Lazy one-time spawn of the journaling polling task. Now
        // that `prov`/`mdl`/`cred_name` are all definitely Some,
        // we can build a self::journal::task::Context and start
        // the poll loop. Subsequent turns take the already-Some
        // branch as a no-op.
        //
        // Cloning the Provider Arc (via provider.clone() -> cheap,
        // Provider is Arc-friendly internally but the outer type
        // isn't Arc yet — wrap). Rather than touch that structure
        // here, we `Arc::new(prov.clone())` which pays a one-shot
        // clone cost at spawn. Given this runs once per session,
        // it's well worth the code simplicity.
        if journal_task.is_none() {
            let ctx = self::journal::task::Context {
                provider: std::sync::Arc::new(prov.clone()),
                session_id: session_id.clone(),
                project: scope_name.clone(),
                model: mdl.clone(),
                cred_name: cred_name.clone().unwrap_or_default(),
            };
            journal_ctx = Some(ctx.clone());
            journal_task = Some(self::journal::task::spawn_polling_loop(
                ctx,
                idle_tracker.clone(),
            ));
        }

        // Add user message (if any) — persisted only after a successful agent turn (see below).
        let mut had_staged_user = false;
        if let Some(content) = staged_user_content {
            let user_msg = ChatMessage {
                role: Role::User,
                content,
            };
            history.push(user_msg);
            had_staged_user = true;
        }

        let selection_hint = input.as_ref().map(|sub| sub.text.clone());
        let relevant_memory = selection_hint
            .as_deref()
            .and_then(|hint| crate::memory::relevant_brief(&scope_name, hint, 5).ok())
            .unwrap_or(crate::memory::RelevantMemoryBrief {
                text: String::new(),
                ids: Vec::new(),
            });
        let mut turn_system_prompt = system_prompt.clone();
        if !relevant_memory.text.trim().is_empty() {
            turn_system_prompt.push('\n');
            turn_system_prompt.push_str(&relevant_memory.text);
            turn_system_prompt.push('\n');
        }

        // Run agent loop
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let active_prompt = ActivePromptSession::start(
            std::mem::take(&mut line_editor),
            cancel.clone(),
            tunnel_input_fd,
        );
        let renderer =
            std::sync::Arc::new(std::sync::Mutex::new(EventRenderer::new(cancel.clone())));
        let renderer_for_events = renderer.clone();
        // Forwarder is lock-free (atomic state), shared by Arc so the
        // per-event callback doesn't acquire a second mutex. Hot-path
        // reason: `TextDelta` fires ~50-80/s during a streaming
        // response and the callback runs synchronously on the main
        // task; every extra `Mutex::lock()` here competes with the
        // STDIN worker thread's editor lock and adds to typing lag.
        let forwarder = std::sync::Arc::new(self::event_forward::EventForwarder::new());
        let forwarder_for_events = forwarder.clone();
        // Clone the Arc once per turn so the closure can accumulate
        // Usage from each Done event. The TurnStats mutex is held
        // briefly (a few field writes) and is *not* the renderer's
        // mutex — cumulative recording never blocks token rendering.
        let ts_for_events = turn_stats.clone();
        // Clone for the event callback; the journaling background
        // task (step 7) will hold another clone and poll
        // `should_fire()`.
        let idle_for_events = idle_tracker.clone();
        let on_event: crate::agent::StreamCallback = Box::new(move |event: &StreamEvent| {
            if let Ok(mut guard) = renderer_for_events.lock() {
                guard.render(event);
            }
            if let StreamEvent::Done { message } = event {
                if let Ok(mut ts) = ts_for_events.lock() {
                    ts.record(message);
                }
                // Arm the idle tracker — the agent just finished an
                // assistant turn. If the user doesn't type for
                // SIDEKAR_JOURNAL_IDLE_SECS after this, step 7's
                // background task will fire a journal pass.
                //
                // `arm()` is a no-op if we're already armed (tool
                // loops can Done→Waiting→Done without real idleness
                // in between; we want "since the last thing we
                // actually said," not "since the loop last yielded").
                //
                // Also armed regardless of runtime::journal() state:
                // a mid-session flip to `/journal on` should start
                // working immediately without needing a fresh Done.
                idle_for_events.arm();
            }
            forwarder_for_events.forward(event);
        });

        let pre_len = history.len();
        let run_result = crate::agent::run(
            prov,
            mdl,
            &turn_system_prompt,
            &mut history,
            &tool_defs,
            on_event,
            Some(&cancel),
            Some(&session_id),
            &mut prev_resp_id,
            &mut cached_ws,
        )
        .await;
        if let Ok(mut guard) = renderer.lock() {
            guard.teardown();
        }
        let returned_editor = active_prompt.finish();
        line_editor = returned_editor;

        let run_ok = run_result.is_ok();
        if run_ok {
            crate::agent::images::strip_user_image_blobs_from_history(&mut history);
        }

        if crate::runtime::verbose() && run_ok {
            repl_status_dim("[turn complete]");
        }

        let did_compact = match run_result {
            Ok(c) => c,
            Err(e) if e.is::<crate::agent::Cancelled>() => {
                history.truncate(pre_len);
                tunnel_println("\x1b[33m[cancelled]\x1b[0m");
                false
            }
            Err(e) => {
                if !crate::agent::take_error_displayed() {
                    tunnel_println(&format!("\x1b[31mError: {e:#}\x1b[0m"));
                    broker::try_log_error("repl", &format!("{e:#}"), None);
                }
                false
            }
        };

        // Persist: full replace only after compaction; otherwise append this turn (user only after success).
        let mut user_entry_id: Option<String> = None;
        if did_compact {
            let _ = session::replace_history(&session_id, &history);
        } else if run_ok {
            if had_staged_user {
                user_entry_id = session::append_message(&session_id, &history[pre_len - 1]).ok();
            }
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
        } else if pre_len < history.len() {
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
        }

        if run_ok && let Some(hint) = selection_hint.as_deref() {
            let _ = crate::memory::accept_selected_memories(
                &relevant_memory.ids,
                &session_id,
                user_entry_id.as_deref(),
                hint,
            );
        }

        // Auto-submit follow-ups queued during this turn (merged into one message).
        // Skipped on cancel/error so the user isn't dragged into another turn they didn't ask for.
        if run_ok {
            line_editor.drain_pending_followups_as_submit();
        }
    }

    // Show resume command
    let mut resume_cmd = String::from("sidekar repl");
    if let Some(ref c) = cred_name {
        resume_cmd.push_str(&format!(" -c {c}"));
    }
    if let Some(ref m) = model {
        resume_cmd.push_str(&format!(" -m {m}"));
    }
    resume_cmd.push_str(&format!(" -r {session_id}"));
    tunnel_println(&format!("\n\x1b[2m{resume_cmd}\x1b[0m"));

    // Stop the journaling polling task, if it was spawned. Abort
    // is immediate; any in-flight LLM call is dropped. Not calling
    // `record_fired()` here because the task is going away — no
    // future poll will run against this tracker.
    if let Some(handle) = journal_task.take() {
        handle.abort();
        let _ = handle.await;
    }
    maybe_run_final_journal(journal_ctx.as_ref()).await;

    stop_relay(tunnel_tx);
    crate::poller::shutdown_poller();
    let _ = broker::unregister_agent(&bus_name);

    // ExecSession cleanup: kill any still-running PTY sessions.
    #[cfg(unix)]
    {
        let mgr = crate::agent::tools::exec_session_manager();
        mgr.terminate_all().await;
    }

    Ok(())
}
