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
    /// None = fresh session, Some(None) = interactive picker, Some(Some(id)) = resume specific session
    pub resume: Option<Option<String>>,
}

/// Entry point for the REPL.
pub async fn run_with_options(opts: ReplOptions) -> Result<()> {
    providers::set_verbose(opts.verbose || std::env::var("SIDEKAR_VERBOSE").is_ok());

    let cred = opts.credential.as_deref();

    // Validate credential name if provided
    if let Some(name) = cred
        && providers::oauth::provider_type_for(name).is_none()
    {
        anyhow::bail!(
            "Unknown credential: '{name}'. Credential names must start with 'claude', 'codex', or 'or'.\n\
             Examples: claude, claude-1, codex, codex-work, or, or-personal\n\
             Login with: sidekar repl login {name}"
        );
    }

    // Require credential — provider is derived from it
    let cred_name = match cred {
        Some(name) => name,
        None => {
            anyhow::bail!(
                "No credential specified. Use -c to provide one.\n\
                 \n\
                 Example: sidekar repl -c claude -m claude-sonnet-4-20250514\n\
                 \n\
                 Login first:  sidekar repl login <claude|codex|or>"
            );
        }
    };

    let provider_type = providers::oauth::provider_type_for(cred_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown credential: '{cred_name}'. Names must start with 'claude', 'codex', or 'or'."
            )
        })?;

    let model = opts
        .model
        .or_else(|| std::env::var("SIDEKAR_MODEL").ok());

    let model = match model {
        Some(m) => m,
        None => {
            anyhow::bail!(
                "No model specified. Use -m to provide one.\n\
                 \n\
                 Example: sidekar repl -c {cred_name} -m <model>\n\
                 \n\
                 List models: sidekar repl models -c {cred_name}"
            );
        }
    };

    let provider = match provider_type {
        "anthropic" => {
            let api_key = providers::oauth::get_anthropic_token(Some(cred_name)).await?;
            Provider::anthropic(api_key)
        }
        "codex" => {
            let (api_key, account_id) = providers::oauth::get_codex_token(Some(cred_name)).await?;
            Provider::codex(api_key, account_id)
        }
        "openrouter" => {
            let api_key = providers::oauth::get_openrouter_token(Some(cred_name)).await?;
            Provider::openrouter(api_key)
        }
        _ => {
            anyhow::bail!("Unknown provider type: {provider_type}");
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

    // SAFETY: called once during serial startup, before spawning async tasks.
    unsafe { std::env::set_var("SIDEKAR_AGENT_NAME", &bus_name) };

    let cron_project = crate::scope::resolve_project_name(None);
    crate::commands::cron::start_default_cron_loop(bus_name.clone(), cron_project).await;

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
        let did_compact = crate::agent::run(
            &provider,
            &model,
            &system_prompt,
            &mut history,
            &tool_defs,
            on_event,
            Some(&cancel),
        )
        .await?;

        if did_compact {
            let _ = session::replace_history(&session_id, &history);
        } else if pre_len < history.len() {
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
        }

        let _ = broker::unregister_agent(&bus_name);
        return Ok(());
    }

    // Default: fresh session. -r to resume.
    let (mut session_id, mut history) = match &opts.resume {
        Some(Some(sid)) => {
            // Resume specific session by ID (prefix match)
            match session::find_session_by_prefix(sid)? {
                Some(s) => {
                    let hist = session::load_history(&s.id)?;
                    eprintln!(
                        "\x1b[2mResumed session {} ({} messages).\x1b[0m",
                        &s.id[..s.id.len().min(8)],
                        hist.len()
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
            init_session(&cwd, &model)?
        }
        None => {
            let id = session::create_session(&cwd, &model, "repl")?;
            (id, Vec::new())
        }
    };

    print_banner(&model);

    loop {
        let input = match read_input_or_bus(&bus_name) {
            InputEvent::User(s) => Some(s),
            InputEvent::Bus => None, // no user text — bus messages trigger the agent
            InputEvent::Eof => break,
        };

        if let Some(ref text) = input {
            let trimmed = text.trim();
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
        }

        // Inject pending bus messages as steering
        inject_bus_messages(&bus_name, &mut history, &session_id);

        // Add user message (if any)
        if let Some(text) = input {
            let user_msg = ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text { text }],
            };
            let _ = session::append_message(&session_id, &user_msg);
            history.push(user_msg);
        }

        // Run agent loop
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let renderer = std::cell::RefCell::new(EventRenderer::new(cancel.clone()));
        let on_event: crate::agent::StreamCallback =
            Box::new(move |event: &StreamEvent| renderer.borrow_mut().render(event));

        let pre_len = history.len();
        let did_compact = match crate::agent::run(
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
            Ok(c) => c,
            Err(e) if e.is::<crate::agent::Cancelled>() => {
                // Truncate any partial messages added during cancelled run
                history.truncate(pre_len);
                eprintln!("\x1b[33m[cancelled]\x1b[0m");
                false
            }
            Err(e) => {
                eprintln!("\x1b[31mError: {e:#}\x1b[0m");
                false
            }
        };

        // Persist: full replace only after compaction, otherwise just append new messages
        if did_compact {
            let _ = session::replace_history(&session_id, &history);
        } else if pre_len < history.len() {
            for msg in &history[pre_len..] {
                let _ = session::append_message(&session_id, msg);
            }
        }
    }

    eprintln!();
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

    fn flush_md_lines(&mut self) {
        for line in self.md.commit_complete_lines() {
            println!("{line}");
        }
    }

    fn render(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Waiting => {
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label(
                    self.cancel.clone(),
                    "thinking".to_string(),
                ));
            }
            StreamEvent::Compacting => {
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label(
                    self.cancel.clone(),
                    "compacting context".to_string(),
                ));
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
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label(
                    self.cancel.clone(),
                    "working".to_string(),
                ));
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
        "bash" | "Bash" => {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or(args_json);
            // Show only the first line to keep multi-line scripts readable
            let first_line = cmd.lines().next().unwrap_or(cmd);
            truncate_display(first_line, 120)
        }
        _ => {
            // For other tools, show first string field or truncated args
            if let Some(obj) = args.as_object()
                && let Some((_, v)) = obj.iter().next()
                && let Some(s) = v.as_str()
            {
                return truncate_display(s, 120);
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

    let mut prompt = format!(
        "You are a capable coding and automation assistant.\n\
         You have a bash tool for running shell commands.\n\
         You have access to `sidekar` CLI tools for browser automation, desktop automation, \
         web research, agent coordination, secrets management, and more. \
         Call the `sidekar_skill` tool to see what's available.\n\n\
         ## Guidelines\n\
         - Be concise. Lead with the answer, not the reasoning.\n\
         - Do not guess file contents — read them first.\n\
         - Show file paths when referencing code.\n\
         - When you learn a durable fact (decision, constraint, convention, preference), \
         store it with `sidekar memory write` so it persists across sessions.\n\n\
         ## Environment\n\
         - Working directory: {cwd}\n\
         - Date: {today}\n"
    );

    // Inject project + global memory context (decisions, constraints, conventions, etc.)
    if let Ok(brief) = crate::memory::startup_brief(5) {
        let brief = brief.trim();
        if !brief.is_empty() {
            prompt.push_str("\n## Memory\n");
            prompt.push_str(brief);
            prompt.push('\n');
        }
    }

    prompt
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
                    if io::stdin().lock().read_line(&mut line).is_ok()
                        && let Ok(idx) = line.trim().parse::<usize>()
                        && let Some(s) = sessions.get(idx)
                    {
                        return SlashResult::SwitchSession(s.id.clone());
                    }
                    eprintln!("Invalid selection.");
                }
                Err(e) => eprintln!("Failed to list sessions: {e}"),
            }
            SlashResult::Continue
        }
        "/model" => {
            eprintln!("Current model: {model}");
            eprintln!("\nList available models: sidekar repl models -c <credential>");
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

/// What `read_input_or_bus` returned.
enum InputEvent {
    /// User typed a line.
    User(String),
    /// One or more bus messages arrived while idle.
    Bus,
    /// EOF / error.
    Eof,
}

/// Block for user input **or** a bus message, whichever comes first.
///
/// Uses non-blocking stdin polling (500 ms cycles) interleaved with
/// bus queue checks so that cron prompts are picked up without
/// requiring the user to press Enter.
fn read_input_or_bus(bus_name: &str) -> InputEvent {
    print!("\n\x1b[1m> \x1b[0m");
    let _ = io::stdout().flush();

    let mut line_buf = String::new();

    loop {
        // Check for pending bus messages (non-destructive peek)
        if broker::has_pending_messages(bus_name) {
            return InputEvent::Bus;
        }

        // Poll stdin with a short timeout so we cycle back to bus check
        unsafe {
            let mut fds = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = libc::poll(&mut fds, 1, 500); // 500 ms
            if ready > 0 && (fds.revents & libc::POLLIN) != 0 {
                // Data available on stdin — read a full line
                match io::stdin().lock().read_line(&mut line_buf) {
                    Ok(0) => return InputEvent::Eof,
                    Ok(_) => {
                        return InputEvent::User(
                            line_buf.trim_end_matches('\n').to_string(),
                        );
                    }
                    Err(_) => return InputEvent::Eof,
                }
            } else if ready < 0 {
                // poll error (e.g. EINTR from signal) — just retry
                continue;
            }
        }
    }
}

fn print_banner(model: &str) {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("\x1b[1msidekar repl\x1b[0m v{version} — {model}");
    eprintln!("Type /help for commands, /quit to exit\n");
}

