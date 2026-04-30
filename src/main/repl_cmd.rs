use sidekar::*;

/// Handle `sidekar repl <subcommand>` dispatch.
///
/// Returns `Ok(())` on success — the caller should `return` immediately after.
pub async fn handle(
    args: &[String],
    relay_override: Option<bool>,
    proxy_override: Option<bool>,
) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "login" => handle_login(args).await,
        "logout" => handle_logout(args),
        "credentials" => handle_credentials(),
        "models" => handle_models(args).await,
        "sessions" => handle_sessions(args),
        "ws-test" => handle_ws_test(args).await,
        _ => handle_run(args, relay_override, proxy_override).await,
    }
}

async fn handle_login(args: &[String]) -> Result<()> {
    let provider = match args.get(1).map(|s| s.as_str()) {
        Some(n) => n,
        None => {
            eprintln!("Usage: sidekar repl login <provider> [name]");
            eprintln!();
            eprintln!("Providers:");
            eprintln!("  claude     Claude (Anthropic) — OAuth");
            eprintln!("  codex      Codex (OpenAI) — OAuth");
            eprintln!("  or         OpenRouter — API key");
            eprintln!("  oc         OpenCode — API key");
            eprintln!("  grok       Grok (xAI) — API key");
            eprintln!("  gem        Gemini (Google) — API key");
            eprintln!("  oac <name> <url> [api_key]");
            eprintln!();
            eprintln!("Examples:");
            eprintln!("  sidekar repl login claude            → stored as 'claude'");
            eprintln!("  sidekar repl login claude work       → stored as 'claude-work'");
            eprintln!("  sidekar repl login or personal       → stored as 'or-personal'");
            eprintln!("  sidekar repl login oac local http://localhost:11434/v1");
            std::process::exit(1);
        }
    };

    // oac is positional: oac <name> <url> [api_key]
    if provider == "oac" {
        let name = args.get(2).map(String::as_str).unwrap_or("oac");
        let base_url = args.get(3).map(String::as_str);
        let api_key = args.get(4).map(String::as_str);
        let creds =
            sidekar::providers::oauth::login_openai_compat(name, Some(name), base_url, api_key)
                .await?;
        sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
            "Logged in as '{name}' ({} at {}).",
            creds.name, creds.base_url
        )))?;
        return Ok(());
    }

    // Optional name: `sidekar repl login claude work` → nickname = "claude-work"
    // If no name given, nickname = provider as-is (e.g. "claude", "or-personal").
    let nickname: String = match args.get(2).map(String::as_str) {
        Some(name) if !name.starts_with('-') => {
            // Avoid double-hyphen: "claude-work work" → "claude-work" not "claude-work-work"
            let base = provider.trim_end_matches('-');
            format!("{base}-{name}")
        }
        _ => provider.to_string(),
    };
    let nickname = nickname.as_str();

    let provider_type =
        sidekar::providers::oauth::provider_type_for(nickname).unwrap_or_else(|| {
            // Fall back on the bare provider keyword
            match provider {
                "claude" | "anthropic" => "anthropic",
                "codex" | "openai" => "codex",
                "or" | "openrouter" => "openrouter",
                "oc" | "opencode" => "opencode",
                "ocg" | "opencode-go" => "opencode-go",
                "grok" => "grok",
                "gem" | "gemini" => "gemini",
                _ => {
                    eprintln!("Unknown provider: '{provider}'.");
                    eprintln!("Use: claude, codex, or, oc, ocg, grok, gem, oac");
                    std::process::exit(1);
                }
            }
        });

    // Clear existing creds for this nickname before login
    let kv_key = sidekar::providers::oauth::kv_key_for(nickname);
    let _ = sidekar::broker::kv_delete(&kv_key);

    match provider_type {
        "anthropic" => {
            let _ = sidekar::providers::oauth::login_anthropic(Some(nickname)).await?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
                "Logged in as '{nickname}' (Claude OAuth)."
            )))?;
        }
        "codex" => {
            let (_, account_id) = sidekar::providers::oauth::login_codex(Some(nickname)).await?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
                "Logged in as '{nickname}' (Codex, account: {}).",
                if account_id.is_empty() {
                    "unknown"
                } else {
                    &account_id
                }
            )))?;
        }
        "openrouter" => {
            let _ = sidekar::providers::oauth::login_openrouter(Some(nickname)).await?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
                "Logged in as '{nickname}' (OpenRouter)."
            )))?;
        }
        "opencode" => {
            let _ = sidekar::providers::oauth::login_opencode(Some(nickname)).await?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
                "Logged in as '{nickname}' (OpenCode)."
            )))?;
        }
        "opencode-go" => {
            let _ = sidekar::providers::oauth::login_opencode_go(Some(nickname)).await?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
                "Logged in as '{nickname}' (OpenCode Go)."
            )))?;
        }
        "grok" => {
            let _ = sidekar::providers::oauth::login_grok(Some(nickname)).await?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
                "Logged in as '{nickname}' (Grok)."
            )))?;
        }
        "gemini" => {
            let _ = sidekar::providers::oauth::login_gemini(Some(nickname)).await?;
            sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
                "Logged in as '{nickname}' (Gemini)."
            )))?;
        }
        _ => {
            eprintln!("Unknown provider type for '{nickname}'.");
            eprintln!("Use: claude, codex, or, oc, ocg, grok, gem, oac");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn handle_logout(args: &[String]) -> Result<()> {
    let nickname = args.get(1).map(|s| s.as_str()).unwrap_or("all");
    if nickname == "all" {
        let creds = sidekar::providers::oauth::list_credentials();
        for (name, _) in &creds {
            let _ = sidekar::broker::kv_delete(&sidekar::providers::oauth::kv_key_for(name));
        }
        sidekar::output::emit(&sidekar::output::PlainOutput::new(
            "All OAuth credentials removed.",
        ))?;
    } else {
        let kv_key = sidekar::providers::oauth::kv_key_for(nickname);
        let _ = sidekar::broker::kv_delete(&kv_key);
        sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
            "Credentials for '{nickname}' removed."
        )))?;
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct CredentialEntry {
    name: String,
    provider: String,
}

#[derive(serde::Serialize)]
struct CredentialsListOutput {
    credentials: Vec<CredentialEntry>,
}

impl sidekar::output::CommandOutput for CredentialsListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.credentials.is_empty() {
            writeln!(
                w,
                "No stored credentials. Use: sidekar repl login <nickname>"
            )?;
        } else {
            writeln!(w, "Stored credentials:")?;
            for c in &self.credentials {
                writeln!(w, "  {} ({})", c.name, c.provider)?;
            }
        }
        Ok(())
    }
}

fn handle_credentials() -> Result<()> {
    let creds = sidekar::providers::oauth::list_credentials();
    let credentials = creds
        .into_iter()
        .map(|(name, provider)| CredentialEntry { name, provider })
        .collect();
    sidekar::output::emit(&CredentialsListOutput { credentials })?;
    Ok(())
}

async fn handle_models(args: &[String]) -> Result<()> {
    let mut credential: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        if matches!(args[i].as_str(), "-c" | "--credential") && i + 1 < args.len() {
            credential = Some(args[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }
    let cred = match credential {
        Some(c) => c,
        None => {
            eprintln!("Usage: sidekar repl models -c <credential>");
            eprintln!();
            eprintln!("Example: sidekar repl models -c claude");
            std::process::exit(1);
        }
    };
    let provider_type = sidekar::providers::oauth::provider_type_for(&cred).unwrap_or_else(|| {
        if cred == "anthropic" {
            "anthropic"
        } else if cred == "codex" || cred == "openai" {
            "codex"
        } else if cred == "openrouter" {
            "openrouter"
        } else if cred == "opencode" {
            "opencode"
        } else if cred == "opencode-go" {
            "opencode-go"
        } else if cred == "grok" {
            "grok"
        } else {
            eprintln!("Unknown provider for '{cred}'.");
            std::process::exit(1);
        }
    });
    // Get token silently (don't trigger login)
    let api_key = match provider_type {
        "anthropic" => sidekar::providers::oauth::get_anthropic_token(Some(&cred)).await,
        "codex" => sidekar::providers::oauth::get_codex_token(Some(&cred))
            .await
            .map(|(t, _)| t),
        "openrouter" => sidekar::providers::oauth::get_openrouter_token(Some(&cred)).await,
        "opencode" => sidekar::providers::oauth::get_opencode_token(Some(&cred)).await,
        "opencode-go" => sidekar::providers::oauth::get_opencode_go_token(Some(&cred)).await,
        "grok" => sidekar::providers::oauth::get_grok_token(Some(&cred)).await,
        "oac" => sidekar::providers::oauth::get_openai_compat_credentials(&cred)
            .await
            .map(|c| c.api_key),
        _ => anyhow::bail!("Unknown provider"),
    };
    let api_key = match api_key {
        Ok(k) => k,
        Err(e) => {
            eprintln!("Failed to get credentials for '{cred}': {e}");
            std::process::exit(1);
        }
    };
    let models = if provider_type == "oac" {
        let creds = sidekar::providers::oauth::get_openai_compat_credentials(&cred).await?;
        sidekar::providers::fetch_openai_compat_model_list(&creds.api_key, &creds.base_url).await
    } else {
        sidekar::providers::fetch_model_list(provider_type, &api_key).await
    };
    let models = match models {
        Ok(m) => m,
        Err(err) => {
            eprintln!("Error listing models for '{cred}' ({provider_type}): {err}");
            std::process::exit(1);
        }
    };
    let items: Vec<ModelEntry> = models
        .iter()
        .map(|m| ModelEntry {
            id: m.id.clone(),
            display_name: m.display_name.clone(),
            context_window: m.context_window,
        })
        .collect();
    sidekar::output::emit(&ModelsListOutput {
        credential: cred.clone(),
        provider_type: provider_type.to_string(),
        count: items.len(),
        models: items,
    })?;
    Ok(())
}

#[derive(serde::Serialize)]
struct ModelEntry {
    id: String,
    display_name: String,
    context_window: u32,
}

#[derive(serde::Serialize)]
struct ModelsListOutput {
    credential: String,
    provider_type: String,
    count: usize,
    models: Vec<ModelEntry>,
}

impl sidekar::output::CommandOutput for ModelsListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.models.is_empty() {
            writeln!(w, "No models found.")?;
            return Ok(());
        }
        writeln!(
            w,
            "Models for \x1b[1m{}\x1b[0m ({}):\n",
            self.credential, self.provider_type
        )?;
        for m in &self.models {
            let ctx = if m.context_window > 0 {
                format!(", {}k ctx", m.context_window / 1000)
            } else {
                String::new()
            };
            writeln!(
                w,
                "  \x1b[36m{}\x1b[0m  \x1b[2m{}{}\x1b[0m",
                m.id, m.display_name, ctx
            )?;
        }
        writeln!(w, "\n\x1b[2m{} models\x1b[0m", self.count)?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ReplSessionOut {
    id: String,
    name: Option<String>,
    credential: String,
    model: String,
    message_count: usize,
    updated_at: f64,
    cwd: String,
}

#[derive(serde::Serialize)]
struct ReplSessionsOutput {
    show_cwd: bool,
    items: Vec<ReplSessionOut>,
}

impl sidekar::output::CommandOutput for ReplSessionsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No sessions found.")?;
            return Ok(());
        }
        writeln!(w, "Sessions (most recent first):\n")?;
        for s in &self.items {
            let name = s.name.as_deref().unwrap_or(&s.id[..s.id.len().min(8)]);
            let age = super::format_age(s.updated_at);
            if self.show_cwd {
                let dir = s.cwd.rsplit('/').next().unwrap_or(&s.cwd);
                writeln!(
                    w,
                    "  \x1b[36m{name}\x1b[0m  {} msgs, {}/{}, {age}  \x1b[2m{dir}\x1b[0m",
                    s.message_count, s.credential, s.model
                )?;
            } else {
                writeln!(
                    w,
                    "  \x1b[36m{name}\x1b[0m  {} msgs, {}/{}, {age}",
                    s.message_count, s.credential, s.model
                )?;
            }
        }
        Ok(())
    }
}

fn handle_sessions(args: &[String]) -> Result<()> {
    let pruned = sidekar::session::prune_empty_sessions().unwrap_or(0);
    if pruned > 0 {
        eprintln!("\x1b[2mPruned {pruned} empty sessions.\x1b[0m");
    }

    let all = args.iter().any(|a| a == "--all");
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let sessions = if all {
        sidekar::session::list_all_sessions(20)?
    } else {
        sidekar::session::list_sessions(&cwd, 20)?
    };
    let items = sessions
        .into_iter()
        .map(|s| {
            let msgs = sidekar::session::message_count(&s.id).unwrap_or(0);
            let model = if s.model.is_empty() {
                "?".to_string()
            } else {
                s.model.clone()
            };
            let credential = if s.provider.is_empty() {
                "?".to_string()
            } else {
                s.provider.clone()
            };
            ReplSessionOut {
                id: s.id,
                name: s.name,
                credential,
                model,
                message_count: msgs,
                updated_at: s.updated_at,
                cwd: s.cwd,
            }
        })
        .collect();
    sidekar::output::emit(&ReplSessionsOutput {
        show_cwd: all,
        items,
    })?;
    Ok(())
}

async fn handle_ws_test(args: &[String]) -> Result<()> {
    use sidekar::providers::{StreamEvent, codex};

    let mut credential = "codex".to_string();
    let mut prompt = "Say hello in exactly 5 words.".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-c" | "--credential" if i + 1 < args.len() => {
                credential = args[i + 1].clone();
                i += 2;
            }
            _ => {
                prompt = args[i..].join(" ");
                break;
            }
        }
    }

    // Get codex credentials
    let (api_key, account_id) =
        sidekar::providers::oauth::get_codex_token(Some(&credential)).await?;

    let base_url = "https://chatgpt.com/backend-api";
    let model = "gpt-5.4";

    let messages = vec![sidekar::providers::ChatMessage {
        role: sidekar::providers::Role::User,
        content: vec![sidekar::providers::ContentBlock::Text {
            text: prompt.clone(),
        }],
    }];

    eprintln!("\x1b[2mConnecting WS to {base_url} ...\x1b[0m");
    eprintln!("\x1b[2mPrompt: {prompt}\x1b[0m");
    eprintln!();

    let ws_config = sidekar::providers::StreamConfig {
        use_websocket: true,
        ..sidekar::providers::StreamConfig::default()
    };
    let (mut rx, _reclaim) = codex::stream_ws(
        &api_key,
        &account_id,
        base_url,
        model,
        "You are a helpful assistant. Be concise.",
        &messages,
        &[],
        Some("ws-test-session"),
        None,
        &ws_config,
        None,
    )
    .await?;

    let mut full_text = String::new();
    while let Some(event) = rx.recv().await {
        match &event {
            StreamEvent::TextDelta { delta } => {
                eprint!("{delta}");
                full_text.push_str(delta);
            }
            StreamEvent::Done { message } => {
                eprintln!();
                eprintln!();
                let u = &message.usage;
                eprintln!("\x1b[2m--- Usage ---\x1b[0m");
                eprintln!(
                    "  input: {} (cached: {}, cache_write: {})",
                    u.input_tokens, u.cache_read_tokens, u.cache_write_tokens
                );
                eprintln!("  output: {}", u.output_tokens);
                eprintln!("  model: {}", message.model);
                eprintln!("  response_id: {}", message.response_id);
                eprintln!("  stop: {:?}", message.stop_reason);
            }
            StreamEvent::Error { message } => {
                eprintln!("\n\x1b[31mError: {message}\x1b[0m");
            }
            _ => {}
        }
    }

    Ok(())
}

async fn handle_run(
    args: &[String],
    relay_override: Option<bool>,
    proxy_override: Option<bool>,
) -> Result<()> {
    let mut prompt: Option<String> = None;
    let mut model: Option<String> = None;
    let mut credential: Option<String> = None;
    let mut verbose = false;
    let mut resume: Option<Option<String>> = None;
    // Tri-state matches relay_override / proxy_override convention:
    // None = leave runtime default untouched, Some(true) = --journal,
    // Some(false) = --no-journal. The paired-flag style beats a
    // single `--journal=VALUE` because it mirrors --proxy/--no-proxy
    // that already exist upstream and keeps tab-completion obvious.
    let mut journal_override: Option<bool> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" if i + 1 < args.len() => {
                prompt = Some(args[i + 1].clone());
                i += 2;
            }
            "-m" if i + 1 < args.len() => {
                model = Some(args[i + 1].clone());
                i += 2;
            }
            "-c" | "--credential" if i + 1 < args.len() => {
                credential = Some(args[i + 1].clone());
                i += 2;
            }
            "--verbose" | "-v" => {
                verbose = true;
                i += 1;
            }
            "--journal" => {
                journal_override = Some(true);
                i += 1;
            }
            "--no-journal" => {
                journal_override = Some(false);
                i += 1;
            }
            "--resume" => {
                resume = Some(None);
                i += 1;
            }
            "-r" | "--resume-session" if i + 1 < args.len() => {
                resume = Some(Some(args[i + 1].clone()));
                i += 2;
            }
            "-r" | "--resume-session" => {
                resume = Some(None);
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    sidekar::repl::run_with_options(sidekar::repl::ReplOptions {
        prompt,
        model,
        credential,
        verbose,
        resume,
        relay_override,
        proxy_override,
        journal_override,
    })
    .await
}
