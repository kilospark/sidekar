use anyhow::Result;
use std::io::{self, BufRead, Read, Write};

use crate::broker;
use crate::message::AgentId;
use crate::providers::{self, ChatMessage, ContentBlock, Provider, Role, StreamEvent};
use crate::session;

const REPL_INPUT_HISTORY_LIMIT: usize = 500;

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
        broker::try_log_error("bus", &format!("registration failed: {e}"), None);
    }

    crate::bus::set_terminal_title(&format!("{nick} ({bus_name}) — sidekar repl"));

    // SAFETY: called once during serial startup, before spawning async tasks.
    unsafe { std::env::set_var("SIDEKAR_AGENT_NAME", &bus_name) };

    // Relay tunnel (web terminal access)
    let _ = rustls::crypto::ring::default_provider().install_default();
    let relay_policy = match opts.relay_override {
        Some(true) => crate::config::RelayMode::On,
        Some(false) => crate::config::RelayMode::Off,
        None => crate::config::relay_mode(),
    };
    let (tunnel_tx, _tunnel_rx): (Option<crate::tunnel::TunnelSender>, Option<crate::tunnel::TunnelReceiver>) = match relay_policy {
        crate::config::RelayMode::On => {
            if let Some(token) = crate::auth::auth_token() {
                let (cols, rows) = terminal_size().unwrap_or((80, 24));
                match crate::tunnel::connect(&token, &bus_name, "sidekar-repl", &cwd, &nick, cols, rows).await {
                    Ok((tx, rx)) => {
                        broker::try_log_event("debug", "relay", "connected", None);
                        (Some(tx), Some(rx))
                    }
                    Err(e) => {
                        broker::try_log_error("relay", &format!("{e:#}"), None);
                        (None, None)
                    }
                }
            } else {
                broker::try_log_error("relay", "skipped: no device token; run: sidekar login", None);
                (None, None)
            }
        }
        _ => (None, None),
    };

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
        let renderer = std::cell::RefCell::new(EventRenderer::new(cancel.clone(), tunnel_tx.clone()));
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

        if let Some(tx) = tunnel_tx {
            tx.shutdown();
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
                    broker::try_log_event("debug", "session", "resumed", Some(&format!("{} ({} messages)", &s.id[..s.id.len().min(8)], hist.len())));
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

    print_banner(&model, tunnel_tx.as_ref());

    let scope_root = crate::scope::resolve_project_root(Some(&cwd));
    let scope_name = crate::scope::resolve_project_name(Some(&cwd));
    let mut line_editor = LineEditor::with_history(
        session::load_input_history(&scope_root, REPL_INPUT_HISTORY_LIMIT).unwrap_or_default(),
    );

    loop {
        // Send prompt marker to tunnel so web viewers see it
        if let Some(ref tx) = tunnel_tx {
            tx.send_data(b"\n\x1b[36m\xe2\x80\xba\x1b[0m ".to_vec());
        }
        let input = match read_input_or_bus(&bus_name, &mut line_editor) {
            InputEvent::User(s) => Some(s),
            InputEvent::Bus => None, // no user text — bus messages trigger the agent
            InputEvent::Eof => break,
        };

        if let Some(ref text) = input {
            // Echo user input to tunnel
            if let Some(ref tx) = tunnel_tx {
                let mut data = text.as_bytes().to_vec();
                data.extend_from_slice(b"\r\n");
                tx.send_data(data);
            }
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }

            let _ = session::append_input_history(
                &scope_root,
                &scope_name,
                text,
                REPL_INPUT_HISTORY_LIMIT,
            );

            // Slash commands
            if let Some(result) = handle_slash_command(trimmed, &cwd, &model, &session_id) {
                match result {
                    SlashResult::Continue => continue,
                    SlashResult::Quit => break,
                    SlashResult::SwitchSession(new_id) => {
                        history = session::load_history(&new_id)?;
                        let count = history.len();
                        if count > 0 {
                            println!("\x1b[2mSwitched to session ({count} messages).\x1b[0m");
                        } else {
                            println!("New session started.");
                        }
                        session_id = new_id;
                        continue;
                    }
                    SlashResult::Compact => {
                        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                        let renderer = std::cell::RefCell::new(EventRenderer::new(cancel.clone(), tunnel_tx.clone()));
                        let on_event: crate::agent::StreamCallback =
                            Box::new(move |event: &StreamEvent| renderer.borrow_mut().render(event));

                        let changed = crate::agent::compaction::compact_now(
                            &provider,
                            &model,
                            &mut history,
                            &on_event,
                        )
                        .await;
                        if changed {
                            let _ = session::replace_history(&session_id, &history);
                            println!("\x1b[2m[session compacted]\x1b[0m");
                        } else {
                            println!("\x1b[2m[nothing to compact]\x1b[0m");
                        }
                        let _ = io::stdout().flush();
                        continue;
                    }
                }
            }
        }

        // Inject pending bus messages as steering
        inject_bus_messages(&bus_name, &mut history, &session_id, tunnel_tx.as_ref());

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
        let renderer = std::cell::RefCell::new(EventRenderer::new(cancel.clone(), tunnel_tx.clone()));
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
                history.truncate(pre_len);
                println!("\x1b[33m[cancelled]\x1b[0m");
                if let Some(ref tx) = tunnel_tx {
                    tx.send_data(b"\x1b[33m[cancelled]\x1b[0m\r\n".to_vec());
                }
                false
            }
            Err(e) => {
                let msg = format!("\x1b[31mError: {e:#}\x1b[0m");
                println!("{msg}");
                if let Some(ref tx) = tunnel_tx {
                    let mut d = msg.into_bytes();
                    d.extend_from_slice(b"\r\n");
                    tx.send_data(d);
                }
                broker::try_log_error("repl", &format!("{e:#}"), None);
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

    println!();
    if let Some(tx) = tunnel_tx {
        tx.shutdown();
    }
    let _ = broker::unregister_agent(&bus_name);
    Ok(())
}

fn init_session(cwd: &str, model: &str) -> Result<(String, Vec<ChatMessage>)> {
    match session::latest_session(cwd)? {
        Some(s) => {
            let hist = session::load_history(&s.id)?;
            if !hist.is_empty() {
                broker::try_log_event("debug", "session", "resuming", Some(&format!("{} messages", hist.len())));
            }
            Ok((s.id, hist))
        }
        None => {
            let id = session::create_session(cwd, model, "anthropic")?;
            Ok((id, Vec::new()))
        }
    }
}

fn terminal_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return None;
    }
    if ws.ws_col == 0 || ws.ws_row == 0 {
        return None;
    }
    Some((ws.ws_col, ws.ws_row))
}

// ---------------------------------------------------------------------------
// Stream event rendering
// ---------------------------------------------------------------------------

/// Stateful renderer with streaming markdown support, tool call display, and spinner.
struct EventRenderer {
    md: crate::md::MarkdownStream,
    tool_args: std::collections::HashMap<usize, (String, String)>,
    spinner: Option<Spinner>,
    partial_visible: bool,
    tunnel_tx: Option<crate::tunnel::TunnelSender>,
}

impl EventRenderer {
    fn new(_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>, tunnel_tx: Option<crate::tunnel::TunnelSender>) -> Self {
        Self {
            md: crate::md::MarkdownStream::new(),
            tool_args: std::collections::HashMap::new(),
            spinner: None,
            partial_visible: false,
            tunnel_tx,
        }
    }

    /// Write text to stdout and relay tunnel.
    fn emit(&self, text: &str) {
        print!("{text}");
        if let Some(ref tx) = self.tunnel_tx {
            tx.send_data(text.as_bytes().to_vec());
        }
    }

    /// Write text + newline to stdout and relay tunnel.
    fn emitln(&self, text: &str) {
        println!("{text}");
        if let Some(ref tx) = self.tunnel_tx {
            let mut data = text.as_bytes().to_vec();
            data.extend_from_slice(b"\r\n");
            tx.send_data(data);
        }
    }

    fn stop_spinner(&mut self) {
        if let Some(mut s) = self.spinner.take() {
            s.stop();
        }
    }

    fn flush_md_lines(&mut self) {
        let lines = self.md.commit_complete_lines();
        if lines.is_empty() {
            return;
        }
        self.clear_partial_preview();
        for line in lines {
            self.emitln(&line);
        }
        let _ = io::stdout().flush();
    }

    fn update_partial_preview(&mut self) {
        match self.md.preview_partial_line() {
            Some(line) => {
                self.emit(&format!("\r\x1b[K{line}"));
                let _ = io::stdout().flush();
                self.partial_visible = true;
            }
            None => self.clear_partial_preview(),
        }
    }

    fn clear_partial_preview(&mut self) {
        if self.partial_visible {
            self.emit("\r\x1b[K");
            let _ = io::stdout().flush();
            self.partial_visible = false;
        }
    }

    fn render(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Waiting => {
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label("thinking".to_string(), self.tunnel_tx.clone()));
            }
            StreamEvent::Compacting => {
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label("compacting context".to_string(), self.tunnel_tx.clone()));
            }
            StreamEvent::Idle => {
                self.stop_spinner();
            }
            StreamEvent::ToolExec { name } => {
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label(format!("running {name}"), self.tunnel_tx.clone()));
            }
            StreamEvent::TextDelta { delta } => {
                self.stop_spinner();
                self.md.push(delta);
                self.flush_md_lines();
                self.update_partial_preview();
            }
            StreamEvent::ThinkingDelta { .. } => {
                if self.spinner.is_none() {
                    self.spinner = Some(Spinner::start_with_label("thinking".to_string(), self.tunnel_tx.clone()));
                }
            }
            StreamEvent::ToolCallStart { index, name, .. } => {
                self.stop_spinner();
                self.clear_partial_preview();
                for line in self.md.finalize() {
                    self.emitln(&line);
                }
                let _ = io::stdout().flush();
                self.tool_args
                    .insert(*index, (name.clone(), String::new()));
                self.spinner = Some(Spinner::start_with_label(format!("preparing {name}"), self.tunnel_tx.clone()));
            }
            StreamEvent::ToolCallDelta { index, delta } => {
                if let Some((_, args)) = self.tool_args.get_mut(index) {
                    args.push_str(delta);
                }
                if self.spinner.is_none() {
                    self.spinner = Some(Spinner::start_with_label("preparing tool".to_string(), self.tunnel_tx.clone()));
                }
            }
            StreamEvent::ToolCallEnd { index } => {
                if let Some((name, args_json)) = self.tool_args.remove(index) {
                    let detail = extract_tool_summary(&name, &args_json);
                    self.emitln(&format!("\n\x1b[2m└─\x1b[0m \x1b[36m{name}\x1b[0m \x1b[2m{detail}\x1b[0m"));
                    let _ = io::stdout().flush();
                }
                // Restart spinner while tool executes and next API call happens
                self.stop_spinner();
                self.spinner = Some(Spinner::start_with_label("working".to_string(), self.tunnel_tx.clone()));
            }
            StreamEvent::Done { message } => {
                self.stop_spinner();
                self.clear_partial_preview();
                for line in self.md.finalize() {
                    self.emitln(&line);
                }
                self.emitln("");
                let _ = io::stdout().flush();
                let u = &message.usage;
                self.emitln(&format!(
                    "\x1b[2m[{} in / {} out tokens]\x1b[0m",
                    u.input_tokens, u.output_tokens
                ));
                let _ = io::stdout().flush();
            }
            StreamEvent::Error { message } => {
                self.stop_spinner();
                self.clear_partial_preview();
                self.emitln(&format!("\n\x1b[31mError: {message}\x1b[0m"));
                let _ = io::stdout().flush();
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
    fn start_with_label(label: String, tunnel_tx: Option<crate::tunnel::TunnelSender>) -> Self {
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let r = running.clone();
        let anim_idx = rand::random::<u32>() as usize % ANIMATIONS.len();
        let color_offset = rand::random::<u32>() as usize;
        let handle = std::thread::spawn(move || {
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
                let line = format!(
                    "\r{color}{}\x1b[0m \x1b[2m{:.1}s\x1b[0m{label_part}\x1b[K",
                    frames[i % frames.len()],
                    elapsed,
                );
                print!("{line}");
                if let Some(ref tx) = tunnel_tx {
                    tx.send_data(line.as_bytes().to_vec());
                }
                let _ = io::stdout().flush();
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            let clear = "\r\x1b[K";
            print!("{clear}");
            if let Some(ref tx) = tunnel_tx {
                tx.send_data(clear.as_bytes().to_vec());
            }
            let _ = io::stdout().flush();
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
         You have access to the `sidekar` CLI for browser/page interaction, data capture, \
         desktop automation, local agent memory/tasks/repo actions, scheduled jobs, \
         account/device/session management, encrypted secrets, daemon/config management, \
         and extension control. Run `sidekar skill` with the bash tool for the command catalog.\n\n\
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
    Compact,
}

fn handle_slash_command(
    input: &str,
    cwd: &str,
    model: &str,
    current_session: &str,
) -> Option<SlashResult> {
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
        "/sessions" => {
            match session::list_sessions(cwd, 10) {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        println!("No sessions found.");
                    } else {
                        println!("Sessions (most recent first):");
                        for s in &sessions {
                            let msgs = session::message_count(&s.id).unwrap_or(0);
                            let marker = if s.id == current_session { " *" } else { "" };
                            let name = s.name.as_deref().unwrap_or(&s.id[..s.id.len().min(8)]);
                            println!("  {name} — {msgs} msgs, {}{marker}", s.model);
                        }
                    }
                }
                Err(e) => broker::try_log_error("session", &format!("failed to list: {e}"), None),
            }
            SlashResult::Continue
        }
        "/resume" => {
            match session::list_sessions(cwd, 10) {
                Ok(sessions) => {
                    let sessions: Vec<_> = sessions
                        .into_iter()
                        .filter(|s| {
                            s.id == current_session
                                || session::message_count(&s.id).unwrap_or(0) > 0
                        })
                        .collect();
                    if sessions.len() <= 1 {
                        println!("No other sessions to resume.");
                        return Some(SlashResult::Continue);
                    }
                    println!("Pick a session:");
                    for (i, s) in sessions.iter().enumerate() {
                        let msgs = session::message_count(&s.id).unwrap_or(0);
                        let marker = if s.id == current_session {
                            " (current)"
                        } else {
                            ""
                        };
                        let name = s.name.as_deref().unwrap_or(&s.id[..s.id.len().min(8)]);
                        println!("  [{i}] {name} — {msgs} msgs{marker}");
                    }
                    print!("Enter number: ");
                    let _ = io::stdout().flush();
                    let mut line = String::new();
                    if io::stdin().lock().read_line(&mut line).is_ok()
                        && let Ok(idx) = line.trim().parse::<usize>()
                        && let Some(s) = sessions.get(idx)
                    {
                        return Some(SlashResult::SwitchSession(s.id.clone()));
                    }
                    println!("Invalid selection.");
                }
                Err(e) => broker::try_log_error("session", &format!("failed to list: {e}"), None),
            }
            SlashResult::Continue
        }
        "/model" => {
            println!("Current model: {model}");
            println!("\nList available models: sidekar repl models -c <credential>");
            SlashResult::Continue
        }
        "/compact" => SlashResult::Compact,
        "/help" => {
            println!("Slash commands:");
            println!("  /new      — Start fresh session");
            println!("  /sessions — List sessions for this directory");
            println!("  /resume   — Switch to a different session");
            println!("  /compact  — Compact older session context now");
            println!("  /model    — Show/change model");
            println!("  /quit     — Exit REPL");
            println!("  /help     — Show this help");
            println!();
            println!("Auth (run outside REPL):");
            println!("  sidekar repl login   — OAuth login");
            println!("  sidekar repl logout  — Remove credentials");
            SlashResult::Continue
        }
        _ => unreachable!("checked by is_known_slash_command"),
    };

    Some(result)
}

fn is_known_slash_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "/quit"
            | "/exit"
            | "/q"
            | "/new"
            | "/reset"
            | "/sessions"
            | "/resume"
            | "/model"
            | "/compact"
            | "/help"
    )
}

// ---------------------------------------------------------------------------
// Bus integration
// ---------------------------------------------------------------------------

fn inject_bus_messages(
    bus_name: &str,
    history: &mut Vec<ChatMessage>,
    session_id: &str,
    tunnel_tx: Option<&crate::tunnel::TunnelSender>,
) {
    if let Ok(messages) = broker::poll_messages(bus_name) {
        for msg in messages {
            let text = format!("[Bus message from {}]: {}", msg.sender, msg.body);
            let display = format!("\x1b[33m[bus] {} says: {}\x1b[0m", msg.sender, msg.body);
            println!("{display}");
            if let Some(tx) = tunnel_tx {
                let _ = tx.send_data(format!("{display}\r\n").into_bytes());
            }
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

struct RawModeGuard {
    saved: libc::termios,
    fd: i32,
}

impl RawModeGuard {
    fn enter() -> Result<Self> {
        let fd = libc::STDIN_FILENO;
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            anyhow::bail!("tcgetattr failed: {}", std::io::Error::last_os_error());
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            anyhow::bail!("tcsetattr failed: {}", std::io::Error::last_os_error());
        }
        Ok(Self { saved, fd })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
    }
}

enum LineEditResult {
    Continue,
    Submit(String),
    Eof,
}

struct LineEditor {
    buffer: String,
    cursor: usize,
    escape: Vec<u8>,
    utf8: Vec<u8>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: Option<String>,
}

impl LineEditor {
    fn with_history(history: Vec<String>) -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            escape: Vec::new(),
            utf8: Vec::new(),
            history,
            history_index: None,
            history_draft: None,
        }
    }

    fn redraw(&self) {
        print!("\r\x1b[K\x1b[36m›\x1b[0m {}", self.buffer);
        let trailing = self.buffer[self.cursor..].chars().count();
        if trailing > 0 {
            print!("\x1b[{}D", trailing);
        }
        let _ = io::stdout().flush();
    }

    fn clear_display(&self) {
        print!("\r\x1b[K");
        let _ = io::stdout().flush();
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.escape.clear();
        self.utf8.clear();
        self.history_index = None;
        self.history_draft = None;
    }

    fn feed_bytes(&mut self, bytes: &[u8]) -> LineEditResult {
        for &byte in bytes {
            let result = self.feed_byte(byte);
            if !matches!(result, LineEditResult::Continue) {
                return result;
            }
        }
        LineEditResult::Continue
    }

    fn feed_byte(&mut self, byte: u8) -> LineEditResult {
        if !self.escape.is_empty() {
            self.escape.push(byte);
            if self.try_handle_escape() {
                self.redraw();
            }
            return LineEditResult::Continue;
        }

        if !self.utf8.is_empty() {
            self.utf8.push(byte);
            match std::str::from_utf8(&self.utf8) {
                Ok(s) => {
                    if let Some(ch) = s.chars().next() {
                        self.insert_char(ch);
                        self.utf8.clear();
                        self.redraw();
                    }
                }
                Err(err) if err.error_len().is_none() => {}
                Err(_) => {
                    self.utf8.clear();
                }
            }
            return LineEditResult::Continue;
        }

        match byte {
            b'\r' | b'\n' => {
                print!("\r\n");
                let _ = io::stdout().flush();
                let submitted = self.buffer.clone();
                self.record_submission(&submitted);
                self.reset();
                LineEditResult::Submit(submitted)
            }
            0x04 => {
                if self.buffer.is_empty() {
                    print!("\r\n");
                    let _ = io::stdout().flush();
                    LineEditResult::Eof
                } else {
                    self.delete_at_cursor();
                    self.redraw();
                    LineEditResult::Continue
                }
            }
            0x7f | 0x08 => {
                self.backspace();
                self.redraw();
                LineEditResult::Continue
            }
            0x1b => {
                self.escape.push(byte);
                LineEditResult::Continue
            }
            byte if byte.is_ascii_control() => LineEditResult::Continue,
            byte if byte.is_ascii() => {
                self.insert_char(byte as char);
                self.redraw();
                LineEditResult::Continue
            }
            _ => {
                self.utf8.push(byte);
                LineEditResult::Continue
            }
        }
    }

    fn try_handle_escape(&mut self) -> bool {
        match self.escape.as_slice() {
            [0x1b, b'[', b'D'] => {
                self.move_left();
                self.escape.clear();
                true
            }
            [0x1b, b'[', b'C'] => {
                self.move_right();
                self.escape.clear();
                true
            }
            [0x1b, b'[', b'A'] => {
                self.history_prev();
                self.escape.clear();
                true
            }
            [0x1b, b'[', b'B'] => {
                self.history_next();
                self.escape.clear();
                true
            }
            [0x1b, b'[', b'H'] | [0x1b, b'O', b'H'] => {
                self.cursor = 0;
                self.escape.clear();
                true
            }
            [0x1b, b'[', b'F'] | [0x1b, b'O', b'F'] => {
                self.cursor = self.buffer.len();
                self.escape.clear();
                true
            }
            [0x1b, b'[', b'3', b'~'] => {
                self.delete_at_cursor();
                self.escape.clear();
                true
            }
            [0x1b] | [0x1b, b'['] | [0x1b, b'O'] | [0x1b, b'[', b'3'] => false,
            _ => {
                self.escape.clear();
                false
            }
        }
    }

    fn insert_char(&mut self, ch: char) {
        self.detach_history_nav();
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.buffer[..self.cursor]
            .char_indices()
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
    }

    fn move_right(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        if let Some(ch) = self.buffer[self.cursor..].chars().next() {
            self.cursor += ch.len_utf8();
        }
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.detach_history_nav();
        let prev = self.buffer[..self.cursor]
            .char_indices()
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
    }

    fn delete_at_cursor(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        self.detach_history_nav();
        if let Some(ch) = self.buffer[self.cursor..].chars().next() {
            let end = self.cursor + ch.len_utf8();
            self.buffer.drain(self.cursor..end);
        }
    }

    fn detach_history_nav(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.history_draft = None;
        }
    }

    fn record_submission(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|prev| prev == line) {
            return;
        }
        self.history.push(line.to_string());
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.history_draft = Some(self.buffer.clone());
                self.history_index = Some(self.history.len() - 1);
            }
            Some(0) => {}
            Some(idx) => {
                self.history_index = Some(idx - 1);
            }
        }
        self.load_history_selection();
    }

    fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(idx) if idx + 1 < self.history.len() => {
                self.history_index = Some(idx + 1);
                self.load_history_selection();
            }
            Some(_) => {
                self.history_index = None;
                self.buffer = self.history_draft.take().unwrap_or_default();
                self.cursor = self.buffer.len();
            }
        }
    }

    fn load_history_selection(&mut self) {
        if let Some(idx) = self.history_index {
            self.buffer = self.history[idx].clone();
            self.cursor = self.buffer.len();
        }
    }
}

/// Block for user input **or** a bus message, whichever comes first.
///
/// Uses non-blocking stdin polling (500 ms cycles) interleaved with
/// bus queue checks so that cron prompts are picked up without
/// requiring the user to press Enter.
fn read_input_or_bus(bus_name: &str, editor: &mut LineEditor) -> InputEvent {
    editor.redraw();

    let _raw_mode = match RawModeGuard::enter() {
        Ok(guard) => guard,
        Err(_) => {
            let mut line_buf = String::new();
            match io::stdin().lock().read_line(&mut line_buf) {
                Ok(0) => return InputEvent::Eof,
                Ok(_) => return InputEvent::User(line_buf.trim_end_matches('\n').to_string()),
                Err(_) => return InputEvent::Eof,
            }
        }
    };

    let mut buf = [0u8; 64];

    loop {
        // Check for pending bus messages (non-destructive peek)
        if broker::has_pending_messages(bus_name) {
            editor.clear_display();
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
                match io::stdin().read(&mut buf) {
                    Ok(0) => return InputEvent::Eof,
                    Ok(n) => match editor.feed_bytes(&buf[..n]) {
                        LineEditResult::Continue => {}
                        LineEditResult::Submit(line) => return InputEvent::User(line),
                        LineEditResult::Eof => return InputEvent::Eof,
                    },
                    Err(_) => return InputEvent::Eof,
                }
            } else if ready < 0 {
                // poll error (e.g. EINTR from signal) — just retry
                continue;
            }
        }
    }
}

fn print_banner(model: &str, tunnel_tx: Option<&crate::tunnel::TunnelSender>) {
    let version = env!("CARGO_PKG_VERSION");
    let line1 = format!("\x1b[1msidekar repl\x1b[0m \x1b[2mv{version}\x1b[0m");
    let line2 = format!("\x1b[36mmodel\x1b[0m {model}  \x1b[2m/help commands · /quit exit\x1b[0m\n");
    println!("{line1}");
    println!("{line2}");
    if let Some(tx) = tunnel_tx {
        let mut data = line1.into_bytes();
        data.extend_from_slice(b"\r\n");
        data.extend_from_slice(line2.as_bytes());
        data.extend_from_slice(b"\r\n");
        tx.send_data(data);
    }
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::{LineEditResult, LineEditor, SlashResult, handle_slash_command};

    #[test]
    fn absolute_path_is_not_treated_as_slash_command() {
        let result = handle_slash_command("/Users/karthik/image.png", ".", "model", "session");
        assert!(result.is_none());
    }

    #[test]
    fn known_slash_command_still_dispatches() {
        let result = handle_slash_command("/compact", ".", "model", "session");
        assert!(matches!(result, Some(SlashResult::Compact)));
    }

    #[test]
    fn slash_command_alias_still_dispatches() {
        let result = handle_slash_command("/q", ".", "model", "session");
        assert!(matches!(result, Some(SlashResult::Quit)));
    }

    #[test]
    fn line_editor_supports_left_right_insertion() {
        let mut editor = LineEditor::with_history(Vec::new());
        assert!(matches!(editor.feed_bytes(b"ac"), LineEditResult::Continue));
        assert!(matches!(editor.feed_bytes(&[0x1b, b'[', b'D']), LineEditResult::Continue));
        assert!(matches!(editor.feed_bytes(b"b"), LineEditResult::Continue));
        assert!(matches!(
            editor.feed_bytes(b"\r"),
            LineEditResult::Submit(line) if line == "abc"
        ));
    }

    #[test]
    fn line_editor_supports_delete_key() {
        let mut editor = LineEditor::with_history(Vec::new());
        assert!(matches!(editor.feed_bytes(b"abc"), LineEditResult::Continue));
        assert!(matches!(editor.feed_bytes(&[0x1b, b'[', b'D']), LineEditResult::Continue));
        assert!(matches!(editor.feed_bytes(&[0x1b, b'[', b'3', b'~']), LineEditResult::Continue));
        assert!(matches!(
            editor.feed_bytes(b"\r"),
            LineEditResult::Submit(line) if line == "ab"
        ));
    }

    #[test]
    fn line_editor_supports_history_up_down() {
        let mut editor = LineEditor::with_history(Vec::new());
        assert!(matches!(
            editor.feed_bytes(b"first\r"),
            LineEditResult::Submit(line) if line == "first"
        ));
        assert!(matches!(
            editor.feed_bytes(b"second\r"),
            LineEditResult::Submit(line) if line == "second"
        ));
        assert!(matches!(editor.feed_bytes(&[0x1b, b'[', b'A']), LineEditResult::Continue));
        assert_eq!(editor.buffer, "second");
        assert!(matches!(editor.feed_bytes(&[0x1b, b'[', b'A']), LineEditResult::Continue));
        assert_eq!(editor.buffer, "first");
        assert!(matches!(editor.feed_bytes(&[0x1b, b'[', b'B']), LineEditResult::Continue));
        assert_eq!(editor.buffer, "second");
    }

    #[test]
    fn line_editor_restores_draft_after_history_navigation() {
        let mut editor = LineEditor::with_history(Vec::new());
        assert!(matches!(
            editor.feed_bytes(b"saved\r"),
            LineEditResult::Submit(line) if line == "saved"
        ));
        assert!(matches!(editor.feed_bytes(b"draft"), LineEditResult::Continue));
        assert!(matches!(editor.feed_bytes(&[0x1b, b'[', b'A']), LineEditResult::Continue));
        assert_eq!(editor.buffer, "saved");
        assert!(matches!(editor.feed_bytes(&[0x1b, b'[', b'B']), LineEditResult::Continue));
        assert_eq!(editor.buffer, "draft");
    }
}
