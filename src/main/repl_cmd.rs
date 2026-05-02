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
        "login" => {
            anyhow::bail!(
                "`sidekar repl login` was removed. Use:\n  sidekar repl credential add <provider> [name]\n\n{}",
                sidekar::repl::credential_login::credential_add_usage_message()
            );
        }
        "credential" => handle_credential(args).await,
        "logout" => handle_logout(args),
        "credentials" => handle_credentials(),
        "models" => handle_models(args).await,
        "sessions" => handle_sessions(args),
        "transcript" => handle_transcript(&args[1..]),
        "ws-test" => handle_ws_test(args).await,
        _ => handle_run(args, relay_override, proxy_override).await,
    }
}

async fn run_repl_credential_add(provider_and_suffix: &[String]) -> Result<()> {
    let msg = sidekar::repl::credential_login::perform_credential_add(
        provider_and_suffix,
        sidekar::repl::credential_login::InteractiveOutput::Cli,
    )
    .await?;
    sidekar::output::emit(&sidekar::output::PlainOutput::new(msg))?;
    Ok(())
}

async fn handle_credential(args: &[String]) -> Result<()> {
    match args.get(1).map(|s| s.as_str()) {
        None | Some("-h") | Some("--help") | Some("help") => {
            eprintln!(
                "{}",
                sidekar::repl::credential_login::credential_add_usage_message()
            );
            Ok(())
        }
        Some("add") => {
            let tokens: Vec<String> = args.iter().skip(2).cloned().collect();
            run_repl_credential_add(&tokens).await
        }
        Some(other) => {
            anyhow::bail!(
                "Unknown subcommand '{other}'.\n{}",
                sidekar::repl::credential_login::credential_add_usage_message()
            );
        }
    }
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
                "No stored credentials. Use: sidekar repl credential add <provider> [name]"
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
    let prov = match sidekar::repl::provider_from_credential(&cred).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to get credentials for '{cred}': {e}");
            std::process::exit(1);
        }
    };
    let provider_type = prov.provider_type();
    let models = match sidekar::providers::fetch_model_list_for_provider(&prov).await {
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
            bedrock_foundation_model_arn: m.bedrock_foundation_model_arn.clone(),
            bedrock_inference_profile_refs: m.bedrock_inference_profile_refs.clone(),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    bedrock_foundation_model_arn: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bedrock_inference_profile_refs: Vec<String>,
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
            if sidekar::providers::is_verbose() {
                if let Some(ref fm) = m.bedrock_foundation_model_arn {
                    writeln!(w, "      \x1b[2mfoundation-model ARN: {fm}\x1b[0m")?;
                }
                for (i, pr) in m.bedrock_inference_profile_refs.iter().enumerate() {
                    writeln!(w, "      \x1b[2minference profile [{}]: {pr}\x1b[0m", i + 1)?;
                }
            }
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

const TRANSCRIPT_CLI_HELP: &str = "\
Usage: sidekar repl transcript <list|undo|prune-after> … [options]

  list [--session=<prefix>] [--full] [--limit N]
      Print persisted transcript rows (default: last 250 messages unless --full).
  undo [--session=<prefix>] [N]
      Drop last N user turns (default 1). Clears matching session journals.
  prune-after [--session=<prefix>] <id_prefix|@index>
      Delete messages strictly after the entry (@index matches transcript list).

  --session=<prefix>   Session id prefix (default: latest session for cwd)
";

fn transcript_parse_session_flag(args: &[String]) -> Result<(Option<String>, Vec<String>)> {
    let mut session = None::<String>;
    let mut rest = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let a = &args[i];
        if let Some(p) = a.strip_prefix("--session=") {
            session = Some(p.to_string());
            i += 1;
        } else if a == "--session" && i + 1 < args.len() {
            session = Some(args[i + 1].clone());
            i += 2;
        } else {
            rest.push(a.clone());
            i += 1;
        }
    }
    Ok((session, rest))
}

fn transcript_resolve_session_id(prefix: Option<&str>) -> Result<String> {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());
    match prefix {
        Some(p) => sidekar::session::find_session_by_prefix(p)?
            .map(|s| s.id)
            .ok_or_else(|| anyhow::anyhow!("No repl session matches prefix `{p}`")),
        None => sidekar::session::latest_session(&cwd)?
            .map(|s| s.id)
            .ok_or_else(|| anyhow::anyhow!(
                "No repl session for this directory. Pass --session=<id_prefix> or run `sidekar repl` here first."
            )),
    }
}

fn repl_sid_short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn handle_transcript(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    let tail = args.get(1..).unwrap_or(&[]);
    match sub {
        "help" | "-h" | "--help" | "" => {
            sidekar::output::emit(&sidekar::output::PlainOutput::new(
                TRANSCRIPT_CLI_HELP.to_string(),
            ))?;
            Ok(())
        }
        "list" => transcript_cli_list(tail),
        "undo" => transcript_cli_undo(tail),
        "prune-after" => transcript_cli_prune_after(tail),
        other => bail!(
            "Unknown transcript subcommand `{other}`.\n{}",
            TRANSCRIPT_CLI_HELP
        ),
    }
}

fn transcript_cli_list(tail: &[String]) -> Result<()> {
    const CAP: usize = 250;
    let (sess_pfx, rest) = transcript_parse_session_flag(tail)?;
    let sid = transcript_resolve_session_id(sess_pfx.as_deref())?;
    let mut full = false;
    let mut limit: Option<usize> = None;
    let mut i = 0usize;
    while i < rest.len() {
        match rest[i].as_str() {
            "--full" => {
                full = true;
                i += 1;
            }
            "--limit" => {
                let n = rest
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--limit requires a number"))?
                    .parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("--limit must be a positive integer"))?;
                if n == 0 {
                    bail!("--limit must be at least 1");
                }
                limit = Some(n);
                i += 2;
            }
            flag => bail!("Unknown flag `{flag}` for transcript list"),
        }
    }

    let rows_full = sidekar::session::list_message_entries(&sid)?;
    let (slice, start_idx, note): (&[sidekar::session::MessageEntrySummary], usize, Option<String>) =
        if full {
            (&rows_full, 0, None)
        } else if let Some(n) = limit {
            let take = n.min(rows_full.len());
            let start = rows_full.len().saturating_sub(take);
            (
                &rows_full[start..],
                start,
                Some(format!("Last {take} of {} messages.", rows_full.len())),
            )
        } else if rows_full.len() <= CAP {
            (&rows_full, 0, None)
        } else {
            let start = rows_full.len() - CAP;
            (
                &rows_full[start..],
                start,
                Some(format!(
                    "Showing last {CAP} of {} messages (use --full for all).",
                    rows_full.len()
                )),
            )
        };

    let mut lines = Vec::<String>::new();
    lines.push(format!(
        "Transcript session={} ({} messages total):",
        repl_sid_short(&sid),
        rows_full.len()
    ));
    if let Some(ref n) = note {
        lines.push(format!("({n})"));
    }
    for (j, r) in slice.iter().enumerate() {
        let idx = start_idx + j;
        lines.push(format!("  [{idx}] {:9} {}", r.role, r.id));
        lines.push(format!("      {}", r.preview));
    }
    sidekar::output::emit(&sidekar::output::PlainOutput::new(lines.join("\n")))?;
    Ok(())
}

fn transcript_cli_undo(tail: &[String]) -> Result<()> {
    let (sess_pfx, rest) = transcript_parse_session_flag(tail)?;
    let sid = transcript_resolve_session_id(sess_pfx.as_deref())?;
    let n = match rest.len() {
        0 => 1usize,
        1 => rest[0]
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("undo count must be an integer"))?,
        _ => bail!("extra arguments — usage: transcript undo [--session=…] [N]"),
    };
    if n < 1 {
        bail!("N must be at least 1");
    }
    let deleted = sidekar::session::undo_message_turns(&sid, n)?;
    if deleted > 0 {
        sidekar::repl::sync_transcript_mutation_side_effects(&sid)?;
    }
    sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
        "Undo session={}: removed {deleted} message row(s) (~{n} user turn(s)).",
        repl_sid_short(&sid)
    )))?;
    Ok(())
}

fn transcript_cli_prune_after(tail: &[String]) -> Result<()> {
    let (sess_pfx, rest) = transcript_parse_session_flag(tail)?;
    let sid = transcript_resolve_session_id(sess_pfx.as_deref())?;
    let tok = rest
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing target — pass id_prefix or @index"))?;
    if rest.len() > 1 {
        bail!("extra arguments — usage: transcript prune-after [--session=…] <id_prefix|@index>");
    }
    let keep_id = if let Some(restidx) = tok.strip_prefix('@') {
        let idx = restidx
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("invalid index after @"))?;
        let rows = sidekar::session::list_message_entries(&sid)?;
        rows.get(idx)
            .map(|r| r.id.clone())
            .ok_or_else(|| anyhow::anyhow!("no message at index [{idx}]"))?
    } else {
        sidekar::session::resolve_message_entry_id_prefix(&sid, tok)?
    };
    let deleted = sidekar::session::truncate_messages_after_entry(&sid, &keep_id)?;
    if deleted > 0 {
        sidekar::repl::sync_transcript_mutation_side_effects(&sid)?;
    }
    sidekar::output::emit(&sidekar::output::PlainOutput::new(format!(
        "Prune-after session={}: deleted {deleted} newer row(s); kept through `{keep_id}`.",
        repl_sid_short(&sid)
    )))?;
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
