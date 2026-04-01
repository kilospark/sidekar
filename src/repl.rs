use anyhow::Result;
use std::io::{self, BufRead, Write};

use crate::broker;
use crate::message::AgentId;
use crate::providers::{self, ChatMessage, ContentBlock, Provider, Role, StreamEvent};
use crate::session;

/// REPL options parsed from CLI flags.
pub struct ReplOptions {
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub credential: Option<String>,
    pub verbose: bool,
}

/// Entry point for the REPL.
pub async fn run_with_options(opts: ReplOptions) -> Result<()> {
    providers::set_verbose(opts.verbose || std::env::var("SIDEKAR_VERBOSE").is_ok());

    let cred = opts.credential.as_deref();

    // Validate credential name if provided
    if let Some(name) = cred {
        if providers::oauth::provider_type_for(name).is_none() {
            anyhow::bail!(
                "Unknown credential: '{name}'. Credential names must start with 'claude' or 'codex'.\n\
                 Examples: claude, claude-1, codex, codex-work\n\
                 Login with: sidekar repl login {name}"
            );
        }
    }

    // Infer default model from credential provider
    let default_model = match cred.and_then(providers::oauth::provider_type_for) {
        Some("codex") => "gpt-5.1-codex-mini",
        Some("anthropic") => providers::default_model(),
        _ => providers::default_model(),
    };

    let model = opts.model
        .or_else(|| std::env::var("SIDEKAR_MODEL").ok())
        .unwrap_or_else(|| default_model.to_string());

    // Validate model name
    let provider_kind = providers::model_info(&model)
        .map(|m| m.provider)
        .ok_or_else(|| {
            let available = providers::MODELS.iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!("Unknown model: '{model}'. Available: {available}")
        })?;

    // Validate credential and model match the same provider
    if let Some(name) = cred {
        let cred_provider = providers::oauth::provider_type_for(name).unwrap_or("");
        let model_provider = match provider_kind {
            providers::ProviderKind::Anthropic => "anthropic",
            providers::ProviderKind::Codex => "codex",
        };
        if cred_provider != model_provider {
            anyhow::bail!(
                "Mismatch: credential '{name}' is {cred_provider} but model '{model}' is {model_provider}.\n\
                 Use a matching model or credential."
            );
        }
    }

    let provider = match provider_kind {
        providers::ProviderKind::Anthropic => {
            let api_key = providers::oauth::get_anthropic_token(cred).await?;
            Provider::anthropic(api_key)
        }
        providers::ProviderKind::Codex => {
            let (api_key, account_id) = providers::oauth::get_codex_token(cred).await?;
            Provider::codex(api_key, account_id)
        }
    };
    let prompt = opts.prompt;
    let system_prompt = build_system_prompt();
    let tool_defs = crate::agent::tools::definitions();

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    // Register on the bus
    let bus_name = format!("sidekar-repl-{}", std::process::id());
    let identity = AgentId {
        name: bus_name.clone(),
        nick: Some("self".to_string()),
        session: Some(cwd.clone()),
        pane: None,
        agent_type: Some("sidekar".to_string()),
    };
    let pane_id = format!("repl-{}", std::process::id());
    if let Err(e) = broker::register_agent(&identity, Some(&pane_id)) {
        eprintln!("\x1b[2m[bus registration failed: {e}]\x1b[0m");
    }

    // Single-prompt mode: fresh session, one turn, exit
    if let Some(input) = prompt {
        let session_id = session::create_session(&cwd, &model, "oneshot")?;
        let mut history: Vec<ChatMessage> = Vec::new();
        let user_msg = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: input }],
        };
        let _ = session::append_message(&session_id, &user_msg);
        history.push(user_msg);

        let on_event: crate::agent::StreamCallback =
            Box::new(|event: &StreamEvent| render_event(event));

        let pre_len = history.len();
        crate::agent::run(&provider, &model, &system_prompt, &mut history, &tool_defs, on_event).await?;

        for msg in &history[pre_len..] {
            let _ = session::append_message(&session_id, msg);
        }

        let _ = broker::unregister_agent(&bus_name);
        return Ok(());
    }

    // Interactive mode: resume or create session
    let (mut session_id, mut history) = init_session(&cwd, &model)?;

    print_banner(&model);

    loop {
        let input = match read_input() {
            Some(text) => text,
            None => break,
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Slash commands
        if trimmed.starts_with('/') {
            match handle_slash_command(trimmed, &cwd, &model, &session_id) {
                SlashResult::Continue => continue,
                SlashResult::Quit => break,
                SlashResult::SwitchSession(new_id) => {
                    history = session::load_history(&new_id)?;
                    let count = history.len();
                    if count > 0 {
                        eprintln!("\x1b[2mSwitched to session ({count} messages).\x1b[0m");
                    } else {
                        eprintln!("New session started.");
                    }
                    session_id = new_id;
                    continue;
                }
                SlashResult::NotFound => {
                    eprintln!("Unknown command: {trimmed}");
                    eprintln!("Available: /new /resume /sessions /model /quit /help");
                    continue;
                }
            }
        }

        // Inject pending bus messages as steering
        inject_bus_messages(&bus_name, &mut history, &session_id);

        // Add user message
        let user_msg = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: input.clone(),
            }],
        };
        let _ = session::append_message(&session_id, &user_msg);
        history.push(user_msg);

        // Run agent loop
        let on_event: crate::agent::StreamCallback =
            Box::new(|event: &StreamEvent| render_event(event));

        let pre_len = history.len();
        if let Err(e) = crate::agent::run(
            &provider,
            &model,
            &system_prompt,
            &mut history,
            &tool_defs,
            on_event,
        )
        .await
        {
            eprintln!("\x1b[31mError: {e:#}\x1b[0m");
        }

        // Persist new messages from the agent loop
        for msg in &history[pre_len..] {
            let _ = session::append_message(&session_id, msg);
        }
    }

    let _ = broker::unregister_agent(&bus_name);
    Ok(())
}

fn init_session(cwd: &str, model: &str) -> Result<(String, Vec<ChatMessage>)> {
    match session::latest_session(cwd)? {
        Some(s) => {
            let hist = session::load_history(&s.id)?;
            if !hist.is_empty() {
                eprintln!(
                    "\x1b[2mResuming session ({} messages). /new to start fresh.\x1b[0m",
                    hist.len()
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

// ---------------------------------------------------------------------------
// Stream event rendering
// ---------------------------------------------------------------------------

fn render_event(event: &StreamEvent) {
    match event {
        StreamEvent::TextDelta { delta } => {
            print!("{delta}");
            let _ = io::stdout().flush();
        }
        StreamEvent::ThinkingDelta { .. } => {}
        StreamEvent::ToolCallStart { name, .. } => {
            eprintln!("\n\x1b[2m> {name}\x1b[0m");
        }
        StreamEvent::ToolCallEnd { .. } | StreamEvent::ToolCallDelta { .. } => {}
        StreamEvent::Done { message } => {
            println!();
            let u = &message.usage;
            eprintln!(
                "\x1b[2m[{} in / {} out tokens]\x1b[0m",
                u.input_tokens, u.output_tokens
            );
        }
        StreamEvent::Error { message } => {
            eprintln!("\n\x1b[31mError: {message}\x1b[0m");
        }
    }
}

// ---------------------------------------------------------------------------
// System prompt
// ---------------------------------------------------------------------------

fn build_system_prompt() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let today = chrono_lite_today();

    format!(
        "You are a capable coding and automation assistant.\n\
         You have a bash tool for running shell commands.\n\n\
         ## Guidelines\n\
         - Be concise. Lead with the answer, not the reasoning.\n\
         - Do not guess file contents — read them first.\n\
         - Show file paths when referencing code.\n\n\
         ## Environment\n\
         - Working directory: {cwd}\n\
         - Date: {today}\n"
    )
}

fn load_context_files(cwd: &str) -> String {
    let names = ["AGENTS.md", "CLAUDE.md"];
    let mut result = String::new();
    let mut dir = std::path::PathBuf::from(cwd);
    let mut depth = 0;
    loop {
        for name in &names {
            let path = dir.join(name);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if !content.is_empty() {
                    if !result.is_empty() {
                        result.push_str("\n---\n\n");
                    }
                    result.push_str(&format!("Contents of {}:\n\n{}", path.display(), content.trim()));
                }
            }
        }
        if !dir.pop() || depth > 5 {
            break;
        }
        depth += 1;
    }
    result
}

fn chrono_lite_today() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let mut y = 1970i64;
    let mut remaining = days as i64;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let months = [31, if is_leap(y) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 1;
    for &days_in_month in &months {
        if remaining < days_in_month {
            break;
        }
        remaining -= days_in_month;
        m += 1;
    }
    format!("{y}-{m:02}-{:02}", remaining + 1)
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

enum SlashResult {
    Continue,
    Quit,
    SwitchSession(String),
    NotFound,
}

fn handle_slash_command(input: &str, cwd: &str, model: &str, current_session: &str) -> SlashResult {
    let cmd = input.split_whitespace().next().unwrap_or("");

    match cmd {
        "/quit" | "/exit" | "/q" => SlashResult::Quit,
        "/new" | "/reset" => {
            match session::create_session(cwd, model, "anthropic") {
                Ok(id) => SlashResult::SwitchSession(id),
                Err(e) => {
                    eprintln!("Failed to create session: {e}");
                    SlashResult::Continue
                }
            }
        }
        "/sessions" => {
            match session::list_sessions(cwd, 10) {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        eprintln!("No sessions found.");
                    } else {
                        eprintln!("Sessions (most recent first):");
                        for s in &sessions {
                            let msgs = session::message_count(&s.id).unwrap_or(0);
                            let marker = if s.id == current_session { " *" } else { "" };
                            let name = s.name.as_deref().unwrap_or(&s.id[..s.id.len().min(8)]);
                            eprintln!("  {name} — {msgs} msgs, {}{marker}", s.model);
                        }
                    }
                }
                Err(e) => eprintln!("Failed to list sessions: {e}"),
            }
            SlashResult::Continue
        }
        "/resume" => {
            match session::list_sessions(cwd, 10) {
                Ok(sessions) => {
                    if sessions.len() <= 1 {
                        eprintln!("No other sessions to resume.");
                        return SlashResult::Continue;
                    }
                    eprintln!("Pick a session:");
                    for (i, s) in sessions.iter().enumerate() {
                        let msgs = session::message_count(&s.id).unwrap_or(0);
                        let marker = if s.id == current_session { " (current)" } else { "" };
                        let name = s.name.as_deref().unwrap_or(&s.id[..s.id.len().min(8)]);
                        eprintln!("  [{i}] {name} — {msgs} msgs{marker}");
                    }
                    eprint!("Enter number: ");
                    let _ = io::stderr().flush();
                    let mut line = String::new();
                    if io::stdin().lock().read_line(&mut line).is_ok() {
                        if let Ok(idx) = line.trim().parse::<usize>() {
                            if let Some(s) = sessions.get(idx) {
                                return SlashResult::SwitchSession(s.id.clone());
                            }
                        }
                    }
                    eprintln!("Invalid selection.");
                }
                Err(e) => eprintln!("Failed to list sessions: {e}"),
            }
            SlashResult::Continue
        }
        "/model" => {
            eprintln!(
                "Current model: {}",
                std::env::var("SIDEKAR_MODEL")
                    .unwrap_or_else(|_| providers::default_model().to_string())
            );
            eprintln!("Set with: SIDEKAR_MODEL=<model-id>\n");

            let has_claude = broker::kv_get(providers::oauth::KV_KEY_ANTHROPIC).ok().flatten().is_some()
                || std::env::var("ANTHROPIC_API_KEY").is_ok();

            eprintln!("Claude (Anthropic) {}:", if has_claude { "\x1b[32m✓\x1b[0m" } else { "\x1b[2mnot logged in\x1b[0m" });
            for m in providers::MODELS.iter().filter(|m| m.provider == providers::ProviderKind::Anthropic) {
                eprintln!("  {} ({})", m.id, m.display_name);
            }
            SlashResult::Continue
        }
        "/help" => {
            eprintln!("Slash commands:");
            eprintln!("  /new      — Start fresh session");
            eprintln!("  /sessions — List sessions for this directory");
            eprintln!("  /resume   — Switch to a different session");
            eprintln!("  /model    — Show/change model");
            eprintln!("  /quit     — Exit REPL");
            eprintln!("  /help     — Show this help");
            eprintln!();
            eprintln!("Auth (run outside REPL):");
            eprintln!("  sidekar repl login   — OAuth login");
            eprintln!("  sidekar repl logout  — Remove credentials");
            SlashResult::Continue
        }
        _ => SlashResult::NotFound,
    }
}

// ---------------------------------------------------------------------------
// Bus integration
// ---------------------------------------------------------------------------

fn inject_bus_messages(bus_name: &str, history: &mut Vec<ChatMessage>, session_id: &str) {
    if let Ok(messages) = broker::poll_messages(bus_name) {
        for msg in messages {
            let text = format!("[Bus message from {}]: {}", msg.sender, msg.body);
            eprintln!("\x1b[33m[bus] {} says: {}\x1b[0m", msg.sender, msg.body);
            let steering = ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text }],
            };
            let _ = session::append_message(session_id, &steering);
            history.push(steering);
        }
    }
}

// ---------------------------------------------------------------------------
// Input / Output
// ---------------------------------------------------------------------------

fn read_input() -> Option<String> {
    print!("\n\x1b[1m> \x1b[0m");
    let _ = io::stdout().flush();

    let mut input = String::new();
    match io::stdin().lock().read_line(&mut input) {
        Ok(0) => None,
        Ok(_) => Some(input.trim_end_matches('\n').to_string()),
        Err(_) => None,
    }
}

fn print_banner(model: &str) {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("\x1b[1msidekar repl\x1b[0m v{version} — {model}");
    eprintln!("Type /help for commands, /quit to exit\n");
}
