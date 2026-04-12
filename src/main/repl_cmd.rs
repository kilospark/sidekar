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
    let nickname = match args.get(1).map(|s| s.as_str()) {
        Some(n) => n,
        None => {
            eprintln!("Usage: sidekar repl login <provider>");
            eprintln!();
            eprintln!("Providers:");
            eprintln!("  claude     Claude (Anthropic) — OAuth");
            eprintln!("  codex      Codex (OpenAI) — OAuth");
            eprintln!("  or         OpenRouter — API key");
            eprintln!("  oc         OpenCode — API key");
            eprintln!();
            eprintln!("Named credentials: claude-work, codex-2, or-personal, oc-work, etc.");
            std::process::exit(1);
        }
    };
    let provider_type =
        sidekar::providers::oauth::provider_type_for(nickname).unwrap_or_else(|| {
            if nickname == "anthropic" {
                "anthropic"
            } else if nickname == "codex" || nickname == "openai" {
                "codex"
            } else if nickname == "openrouter" {
                "openrouter"
            } else if nickname == "opencode" {
                "opencode"
            } else {
                eprintln!("Unknown provider: '{nickname}'.");
                eprintln!("Use claude-<name> for Claude, codex-<name> for Codex, or-<name> for OpenRouter, or oc-<name> for OpenCode.");
                std::process::exit(1);
            }
        });
    // Clear existing creds for this nickname before login
    let kv_key = sidekar::providers::oauth::kv_key_for(nickname);
    let _ = sidekar::broker::kv_delete(&kv_key);
    match provider_type {
        "anthropic" => {
            let token = sidekar::providers::oauth::login_anthropic(Some(nickname)).await?;
            if token.contains("sk-ant-oat") {
                println!("Logged in as '{nickname}' (Claude OAuth).");
            } else {
                println!("Using API key from environment for '{nickname}'.");
            }
        }
        "codex" => {
            let (_, account_id) = sidekar::providers::oauth::login_codex(Some(nickname)).await?;
            println!(
                "Logged in as '{nickname}' (Codex, account: {}).",
                if account_id.is_empty() {
                    "unknown"
                } else {
                    &account_id
                }
            );
        }
        "openrouter" => {
            let _ = sidekar::providers::oauth::get_openrouter_token(Some(nickname)).await?;
            println!("Logged in as '{nickname}' (OpenRouter).");
        }
        "opencode" => {
            let _ = sidekar::providers::oauth::get_opencode_token(Some(nickname)).await?;
            println!("Logged in as '{nickname}' (OpenCode).");
        }
        _ => {
            eprintln!("Unknown provider type for nickname '{nickname}'.");
            eprintln!(
                "Use claude-<name> for Claude, codex-<name> for Codex, or-<name> for OpenRouter, or oc-<name> for OpenCode."
            );
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
        println!("All OAuth credentials removed.");
    } else {
        let kv_key = sidekar::providers::oauth::kv_key_for(nickname);
        let _ = sidekar::broker::kv_delete(&kv_key);
        println!("Credentials for '{nickname}' removed.");
    }
    Ok(())
}

fn handle_credentials() -> Result<()> {
    let creds = sidekar::providers::oauth::list_credentials();
    if creds.is_empty() {
        println!("No stored credentials. Use: sidekar repl login <nickname>");
    } else {
        println!("Stored credentials:");
        for (name, provider) in &creds {
            println!("  {name} ({provider})");
        }
    }
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
        _ => anyhow::bail!("Unknown provider"),
    };
    let api_key = match api_key {
        Ok(k) => k,
        Err(e) => {
            eprintln!("Failed to get credentials for '{cred}': {e}");
            std::process::exit(1);
        }
    };
    let models = sidekar::providers::fetch_model_list(provider_type, &api_key).await;
    if models.is_empty() {
        println!("No models found.");
    } else {
        println!("Models for \x1b[1m{cred}\x1b[0m ({provider_type}):\n");
        for m in &models {
            let ctx = if m.context_window > 0 {
                format!("{}k ctx", m.context_window / 1000)
            } else {
                String::new()
            };
            println!(
                "  \x1b[36m{}\x1b[0m  \x1b[2m{}{}\x1b[0m",
                m.id,
                m.display_name,
                if ctx.is_empty() {
                    String::new()
                } else {
                    format!(", {ctx}")
                }
            );
        }
        println!("\n\x1b[2m{} models\x1b[0m", models.len());
    }
    Ok(())
}

fn handle_sessions(args: &[String]) -> Result<()> {
    // Prune empty sessions from disk
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
    if sessions.is_empty() {
        println!("No sessions found.");
    } else {
        println!("Sessions (most recent first):\n");
        for s in &sessions {
            let msgs = sidekar::session::message_count(&s.id).unwrap_or(0);
            let name = s.name.as_deref().unwrap_or(&s.id[..s.id.len().min(8)]);
            let model = if s.model.is_empty() { "?" } else { &s.model };
            let age = super::format_age(s.updated_at);
            if all {
                let dir = s.cwd.rsplit('/').next().unwrap_or(&s.cwd);
                println!(
                    "  \x1b[36m{name}\x1b[0m  {msgs} msgs, {model}, {age}  \x1b[2m{dir}\x1b[0m",
                );
            } else {
                println!("  \x1b[36m{name}\x1b[0m  {msgs} msgs, {model}, {age}",);
            }
        }
    }
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
    })
    .await
}
