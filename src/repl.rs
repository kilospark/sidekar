use anyhow::Result;
use std::collections::hash_map::Entry;
use std::io::{self, BufRead, Write};

mod editor;
mod relay;
mod renderer;
mod slash;
mod system_prompt;
mod user_turn;

use self::editor::{
    ActivePromptSession, EscCancelWatcher, InputEvent, LineEditor, RawModeGuard,
    clear_transient_status, emit_shared_line, emit_transient_status, print_banner,
    read_input_or_bus,
};
use self::relay::{inject_bus_messages, start_relay, stop_relay};
use self::renderer::EventRenderer;
use self::slash::{
    SlashAction, SlashContext, apply_slash_result, build_provider, handle_slash_command,
    init_session,
};
use self::system_prompt::build_system_prompt;
use crate::broker;
use crate::message::AgentId;
use crate::providers::{self, ChatMessage, ContentBlock, Provider, Role, StreamEvent};
use crate::session;
use crate::tunnel::tunnel_println;

const REPL_INPUT_HISTORY_LIMIT: usize = 500;

fn repl_status_dim(msg: &str) {
    tunnel_println(&format!("\x1b[2m{msg}\x1b[0m"));
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
}

/// Entry point for the REPL.
pub async fn run_with_options(opts: ReplOptions) -> Result<()> {
    crate::runtime::init(opts.verbose);

    // Credential and model are optional — user can set them interactively.
    let mut cred_name: Option<String> = opts.credential;
    let mut model: Option<String> = opts.model.or_else(|| std::env::var("SIDEKAR_MODEL").ok());

    // Validate credential name if provided at startup
    if let Some(ref name) = cred_name {
        if providers::oauth::provider_type_for(name).is_none() {
            anyhow::bail!(
                "Unknown credential: '{name}'. Credential names must start with 'claude', 'codex', 'or', or 'oc'.\n\
                 Examples: claude, claude-1, codex, codex-work, or, or-personal, oc, oc-work\n\
                 Login with: sidekar repl login {name}"
            );
        }
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
    let system_prompt = build_system_prompt();
    let tool_defs = crate::agent::tools::definitions();

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

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

    // Single-prompt mode: fresh session, one turn, exit
    if let Some(input) = prompt {
        let Some(ref prov) = provider else {
            anyhow::bail!("Single-prompt mode requires -c <credential>");
        };
        let Some(ref mdl) = model else {
            anyhow::bail!("Single-prompt mode requires -m <model>");
        };
        let session_id = session::create_session(&cwd, mdl, "oneshot")?;
        let mut history: Vec<ChatMessage> = Vec::new();
        let user_msg = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: input }],
        };
        let _ = session::append_message(&session_id, &user_msg);
        history.push(user_msg);

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _cancel_watch = EscCancelWatcher::start(cancel.clone(), tunnel_input_fd);
        let renderer =
            std::sync::Arc::new(std::sync::Mutex::new(EventRenderer::new(cancel.clone())));
        let renderer_for_events = renderer.clone();
        let on_event: crate::agent::StreamCallback = Box::new(move |event: &StreamEvent| {
            if let Ok(mut guard) = renderer_for_events.lock() {
                guard.render(event);
            }
        });

        let pre_len = history.len();
        let run_result = crate::agent::run(
            prov,
            mdl,
            &system_prompt,
            &mut history,
            &tool_defs,
            on_event,
            Some(&cancel),
            Some(&session_id),
        )
        .await;
        if let Ok(mut guard) = renderer.lock() {
            guard.teardown();
        }

        if crate::runtime::verbose() && run_result.is_ok() {
            repl_status_dim("[turn complete]");
        }

        let did_compact = run_result?;
        if did_compact {
            let _ = session::replace_history(&session_id, &history);
        } else if pre_len < history.len() {
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
        }

        stop_relay(tunnel_tx.take());
        let _ = broker::unregister_agent(&bus_name);
        return Ok(());
    }

    let model_for_session = model.as_deref().unwrap_or("(not set)");

    // Default: fresh session. -r to resume.
    let (mut session_id, mut history) = match &opts.resume {
        Some(Some(sid)) => {
            // Resume specific session by ID (prefix match)
            match session::find_session_by_prefix(sid)? {
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
                    (s.id, hist)
                }
                None => {
                    anyhow::bail!("No session matching '{sid}'");
                }
            }
        }
        Some(None) => {
            // Interactive picker
            init_session(&cwd, model_for_session)?
        }
        None => {
            let id = session::create_session(&cwd, model_for_session, "repl")?;
            (id, Vec::new())
        }
    };

    print_banner(model.as_deref(), cred_name.as_deref());

    let scope_root = crate::scope::resolve_project_root(Some(&cwd));
    let scope_name = crate::scope::resolve_project_name(Some(&cwd));
    let mut line_editor = LineEditor::with_history(
        session::load_input_history(&scope_root, REPL_INPUT_HISTORY_LIMIT).unwrap_or_default(),
    );

    loop {
        let input = match read_input_or_bus(&bus_name, &mut line_editor, tunnel_input_fd) {
            InputEvent::User(s) => Some(s),
            InputEvent::Bus => None, // no user text — bus messages trigger the agent
            InputEvent::Eof => break,
        };

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
                    // Restore terminal to cooked mode for the subprocess
                    let _guard = RawModeGuard::enter_cooked();
                    let status = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(cmd)
                        .stdin(std::process::Stdio::inherit())
                        .stdout(std::process::Stdio::inherit())
                        .stderr(std::process::Stdio::inherit())
                        .status();
                    match status {
                        Ok(s) if !s.success() => {
                            tunnel_println(&format!(
                                "\x1b[2m[exit {}]\x1b[0m",
                                s.code().unwrap_or(-1)
                            ));
                        }
                        Err(e) => {
                            tunnel_println(&format!("\x1b[31mFailed to run command: {e}\x1b[0m"));
                        }
                        _ => {}
                    }
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
                )
                .await?
                {
                    SlashAction::Continue => continue,
                    SlashAction::Quit => break,
                }
            }
        }

        // Guard: need provider + model to run the agent
        let (Some(prov), Some(mdl)) = (&provider, &model) else {
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
            continue;
        };

        // Inject pending bus messages as steering
        let bus_injected = inject_bus_messages(&bus_name, &mut history, &session_id);
        if input.is_none() && bus_injected == 0 {
            continue;
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
        let on_event: crate::agent::StreamCallback = Box::new(move |event: &StreamEvent| {
            if let Ok(mut guard) = renderer_for_events.lock() {
                guard.render(event);
            }
        });

        let pre_len = history.len();
        let run_result = crate::agent::run(
            prov,
            mdl,
            &system_prompt,
            &mut history,
            &tool_defs,
            on_event,
            Some(&cancel),
            Some(&session_id),
        )
        .await;
        if let Ok(mut guard) = renderer.lock() {
            guard.teardown();
        }
        let (returned_editor, mut submitted) = active_prompt.finish();
        line_editor = returned_editor;
        line_editor.pending_followups.append(&mut submitted);

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
        if did_compact {
            let _ = session::replace_history(&session_id, &history);
        } else if run_ok {
            if had_staged_user {
                let _ = session::append_message(&session_id, &history[pre_len - 1]);
            }
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
        } else if pre_len < history.len() {
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
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
    tunnel_println(&format!("\n\x1b[2mResume: {resume_cmd}\x1b[0m"));

    stop_relay(tunnel_tx);
    let _ = broker::unregister_agent(&bus_name);
    Ok(())
}


