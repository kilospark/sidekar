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
    pub resume: bool,
}

/// Entry point for the REPL.
pub async fn run_with_options(opts: ReplOptions) -> Result<()> {
    providers::set_verbose(opts.verbose || std::env::var("SIDEKAR_VERBOSE").is_ok());

    let cred = opts.credential.as_deref();

    // Validate credential name if provided
    if let Some(name) = cred {
        if providers::oauth::provider_type_for(name).is_none() {
            anyhow::bail!(
                "Unknown credential: '{name}'. Credential names must start with 'claude', 'codex', or 'or'.\n\
                 Examples: claude, claude-1, codex, codex-work, or, or-personal\n\
                 Login with: sidekar repl login {name}"
            );
        }
    }

    // Infer default model from credential or env
    let default_model = match cred.and_then(providers::oauth::provider_type_for) {
        Some("codex") => Some("gpt-5.1-codex-mini"),
        Some("openrouter") => Some("x-ai/grok-3"),
        Some("anthropic") => Some(providers::default_model()),
        _ => None,
    };

    let model = opts
        .model
        .or_else(|| std::env::var("SIDEKAR_MODEL").ok())
        .or_else(|| default_model.map(String::from));

    let model = match model {
        Some(m) => m,
        None => {
            anyhow::bail!(
                "No model specified. Use one of:\n\
                 \n\
                 \x1b[1mOption 1:\x1b[0m Pass a credential:  sidekar repl -r claude\n\
                 \x1b[1mOption 2:\x1b[0m Pass a model:       sidekar repl -m grok-3\n\
                 \x1b[1mOption 3:\x1b[0m Set env var:        SIDEKAR_MODEL=grok-3\n\
                 \n\
                 Login first:  sidekar repl login <claude|codex|or>"
            );
        }
    };

    // Validate model name
    let provider_kind = providers::model_info(&model)
        .map(|m| m.provider)
        .ok_or_else(|| {
            let available = providers::MODELS
                .iter()
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
            providers::ProviderKind::OpenRouter => "openrouter",
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
        providers::ProviderKind::OpenRouter => {
            let api_key = providers::oauth::get_openrouter_token(cred).await?;
            Provider::openrouter(api_key)
        }
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
        eprintln!("\x1b[2m[bus registration failed: {e}]\x1b[0m");
    }

    crate::bus::set_terminal_title(&format!("{nick} ({bus_name}) — sidekar repl"));

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

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let renderer = std::cell::RefCell::new(EventRenderer::new(cancel.clone()));
        let on_event: crate::agent::StreamCallback =
            Box::new(move |event: &StreamEvent| renderer.borrow_mut().render(event));

        let pre_len = history.len();
        crate::agent::run(
            &provider,
            &model,
            &system_prompt,
            &mut history,
            &tool_defs,
            on_event,
            Some(&cancel),
        )
        .await?;

        if pre_len <= history.len() {
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
        }

        let _ = broker::unregister_agent(&bus_name);
        return Ok(());
    }

    // Default: fresh session. --resume to continue a previous one.
    let (mut session_id, mut history) = if opts.resume {
        init_session(&cwd, &model)?
    } else {
        let id = session::create_session(&cwd, &model, "repl")?;
        (id, Vec::new())
    };

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
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let renderer = std::cell::RefCell::new(EventRenderer::new(cancel.clone()));
        let on_event: crate::agent::StreamCallback =
            Box::new(move |event: &StreamEvent| renderer.borrow_mut().render(event));

        let pre_len = history.len();
        match crate::agent::run(
            &provider,
            &model,
            &system_prompt,
            &mut history,
            &tool_defs,
            on_event,
            Some(&cancel),
        )
        .await
        {
            Ok(()) => {}
            Err(e) if e.is::<crate::agent::Cancelled>() => {
                // Truncate any partial messages added during cancelled run
                history.truncate(pre_len);
                eprintln!("\x1b[33m[cancelled]\x1b[0m");
            }
            Err(e) => {
                eprintln!("\x1b[31mError: {e:#}\x1b[0m");
            }
        }

        // Persist new messages from the agent loop
        if pre_len <= history.len() {
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
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

/// Stateful renderer with streaming markdown support, tool call display, and spinner.
struct EventRenderer {
    md: crate::md::MarkdownStream,
    tool_args: std::collections::HashMap<usize, (String, String)>,
    spinner: Option<Spinner>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl EventRenderer {
    fn new(cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            md: crate::md::MarkdownStream::new(),
            tool_args: std::collections::HashMap::new(),
            spinner: None,
            cancel,
        }
    }

    fn stop_spinner(&mut self) {
        if let Some(mut s) = self.spinner.take() {
            s.stop();
        }
    }

    fn start_spinner(&mut self) {
        self.stop_spinner();
        self.spinner = Some(Spinner::start_with_cancel(self.cancel.clone()));
    }

    fn flush_md_lines(&mut self) {
        for line in self.md.commit_complete_lines() {
            println!("{line}");
        }
    }

    fn render(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Waiting => {
                self.start_spinner();
            }
            StreamEvent::ToolExec { name } => {
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label(
                    self.cancel.clone(),
                    format!("running {name}"),
                ));
            }
            StreamEvent::TextDelta { delta } => {
                self.stop_spinner();
                self.md.push(delta);
                self.flush_md_lines();
            }
            StreamEvent::ThinkingDelta { .. } => {
                self.stop_spinner();
            }
            StreamEvent::ToolCallStart { index, name, .. } => {
                self.stop_spinner();
                for line in self.md.finalize() {
                    println!("{line}");
                }
                self.tool_args
                    .insert(*index, (name.clone(), String::new()));
            }
            StreamEvent::ToolCallDelta { index, delta } => {
                if let Some((_, args)) = self.tool_args.get_mut(index) {
                    args.push_str(delta);
                }
            }
            StreamEvent::ToolCallEnd { index } => {
                if let Some((name, args_json)) = self.tool_args.remove(index) {
                    let detail = extract_tool_summary(&name, &args_json);
                    eprintln!("\n\x1b[2m> {name}: {detail}\x1b[0m");
                }
                // Restart spinner while tool executes and next API call happens
                self.start_spinner();
            }
            StreamEvent::Done { message } => {
                self.stop_spinner();
                for line in self.md.finalize() {
                    println!("{line}");
                }
                println!();
                let u = &message.usage;
                eprintln!(
                    "\x1b[2m[{} in / {} out tokens]\x1b[0m",
                    u.input_tokens, u.output_tokens
                );
            }
            StreamEvent::Error { message } => {
                self.stop_spinner();
                eprintln!("\n\x1b[31mError: {message}\x1b[0m");
            }
        }
    }
}

struct Spinner {
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

const ANIMATIONS: &[&[&str]] = &[
    &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    &["░", "▒", "▓", "█", "▓", "▒"],
    &["←", "↖", "↑", "↗", "→", "↘", "↓", "↙"],
    &["▖", "▘", "▝", "▗"],
    &["∙", "○", "●", "○"],
    &["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"],
    &["◐", "◓", "◑", "◒"],
    &["▹▹▹", "▸▹▹", "▹▸▹", "▹▹▸"],
    &["⊶", "⊷"],
    &["◇", "◈", "◆", "◈"],
];

impl Spinner {
    fn start_with_cancel(
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self::start_with_label(cancel, String::new())
    }

    fn start_with_label(
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        label: String,
    ) -> Self {
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let r = running.clone();
        let anim_idx = rand::random::<u32>() as usize % ANIMATIONS.len();
        let color_offset = rand::random::<u32>() as usize;
        let handle = std::thread::spawn(move || {
            let orig_termios = enter_raw_mode();
            let frames = ANIMATIONS[anim_idx];
            let colors = [
                "\x1b[33m", // yellow
                "\x1b[36m", // cyan
                "\x1b[35m", // magenta
                "\x1b[32m", // green
                "\x1b[34m", // blue
                "\x1b[31m", // red
            ];
            let started = std::time::Instant::now();
            let label_part = if label.is_empty() {
                String::new()
            } else {
                format!(" \x1b[36m{label}\x1b[0m")
            };
            let mut i = 0;
            while r.load(std::sync::atomic::Ordering::Relaxed) {
                let elapsed = started.elapsed().as_secs_f32();
                let color = colors[(i + color_offset) % colors.len()];
                eprint!(
                    "\r{color}{}\x1b[0m \x1b[2m{:.1}s\x1b[0m{label_part} \x1b[2m(esc to cancel)\x1b[0m\x1b[K",
                    frames[i % frames.len()],
                    elapsed,
                );
                let _ = io::stderr().flush();
                i += 1;

                if poll_stdin_byte(80) == Some(0x1b) {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    r.store(false, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
            }
            eprint!("\r\x1b[K");
            let _ = io::stderr().flush();
            if let Some(t) = orig_termios {
                restore_termios(t);
            }
        });
        Self {
            running,
            handle: Some(handle),
        }
    }

    fn stop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn enter_raw_mode() -> Option<libc::termios> {
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut termios) != 0 {
            return None;
        }
        let orig = termios;
        // Disable canonical mode and echo
        termios.c_lflag &= !(libc::ICANON | libc::ECHO);
        // Set minimum chars to 0, timeout to 0 (non-blocking)
        termios.c_cc[libc::VMIN] = 0;
        termios.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(0, libc::TCSANOW, &termios) != 0 {
            return None;
        }
        Some(orig)
    }
}

fn restore_termios(termios: libc::termios) {
    unsafe {
        libc::tcsetattr(0, libc::TCSANOW, &termios);
    }
}

/// Poll stdin for a single byte with a timeout in milliseconds.
/// Returns the byte if available, None if timeout or error.
fn poll_stdin_byte(timeout_ms: i32) -> Option<u8> {
    unsafe {
        let mut fds = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = libc::poll(&mut fds, 1, timeout_ms);
        if ready > 0 && (fds.revents & libc::POLLIN) != 0 {
            let mut buf = [0u8; 1];
            let n = libc::read(0, buf.as_mut_ptr() as *mut libc::c_void, 1);
            if n == 1 {
                return Some(buf[0]);
            }
        }
    }
    None
}

fn extract_tool_summary(name: &str, args_json: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(args_json).unwrap_or_default();
    match name {
        "bash" | "Bash" => args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or(args_json)
            .to_string(),
        _ => {
            // For other tools, show first string field or truncated args
            if let Some(obj) = args.as_object() {
                if let Some((_, v)) = obj.iter().next() {
                    if let Some(s) = v.as_str() {
                        return truncate_display(s, 120);
                    }
                }
            }
            truncate_display(args_json, 120)
        }
    }
}

fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..s.floor_char_boundary(max)])
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
    let months = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
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
        "/new" | "/reset" => match session::create_session(cwd, model, "anthropic") {
            Ok(id) => SlashResult::SwitchSession(id),
            Err(e) => {
                eprintln!("Failed to create session: {e}");
                SlashResult::Continue
            }
        },
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
                    // Filter out empty sessions (except current)
                    let sessions: Vec<_> = sessions
                        .into_iter()
                        .filter(|s| {
                            s.id == current_session
                                || session::message_count(&s.id).unwrap_or(0) > 0
                        })
                        .collect();
                    if sessions.len() <= 1 {
                        eprintln!("No other sessions to resume.");
                        return SlashResult::Continue;
                    }
                    eprintln!("Pick a session:");
                    for (i, s) in sessions.iter().enumerate() {
                        let msgs = session::message_count(&s.id).unwrap_or(0);
                        let marker = if s.id == current_session {
                            " (current)"
                        } else {
                            ""
                        };
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

            let has_claude = broker::kv_get(providers::oauth::KV_KEY_ANTHROPIC)
                .ok()
                .flatten()
                .is_some()
                || std::env::var("ANTHROPIC_API_KEY").is_ok();

            eprintln!(
                "Claude (Anthropic) {}:",
                if has_claude {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[2mnot logged in\x1b[0m"
                }
            );
            for m in providers::MODELS
                .iter()
                .filter(|m| m.provider == providers::ProviderKind::Anthropic)
            {
                eprintln!("  {} ({})", m.id, m.display_name);
            }

            let has_openrouter = broker::kv_get(providers::oauth::KV_KEY_OPENROUTER)
                .ok()
                .flatten()
                .is_some()
                || std::env::var("OPENROUTER_API_KEY").is_ok();

            eprintln!(
                "\nOpenRouter {}:",
                if has_openrouter {
                    "\x1b[32m✓\x1b[0m"
                } else {
                    "\x1b[2mnot logged in\x1b[0m"
                }
            );
            for m in providers::MODELS
                .iter()
                .filter(|m| m.provider == providers::ProviderKind::OpenRouter)
            {
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

