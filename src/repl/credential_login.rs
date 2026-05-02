//! Shared credential login for `sidekar repl login` and `/credential add|login|update`.

use anyhow::{Result, anyhow, bail};

const LOGIN_USAGE: &str = "\
Usage: sidekar repl login <provider> [name]

Providers:
  claude     Claude (Anthropic) — OAuth
  codex      Codex (OpenAI) — OAuth
  or         OpenRouter — API key
  oc         OpenCode — API key
  grok       Grok (xAI) — API key
  gem        Gemini (Google) — API key
  bedrock | brk Amazon Bedrock — IAM profile / credential chain → HTTPS SigV4
  oac <name> <url> [api_key]

Examples:
  sidekar repl login claude            → stored as 'claude'
  sidekar repl login claude work       → stored as 'claude-work'
  sidekar repl login or personal       → stored as 'or-personal'
  sidekar repl login oac local http://localhost:11434/v1";

/// Same text as missing-arg help for CLI and REPL `/credential add`.
pub fn login_usage_message() -> &'static str {
    LOGIN_USAGE
}

/// Arguments match CLI `sidekar repl login …`: `args[0]` must be `"login"`.
pub async fn perform_login(args: &[String]) -> Result<String> {
    let provider = match args.get(1).map(|s| s.as_str()) {
        Some(n) => n,
        None => bail!("{}", LOGIN_USAGE),
    };

    // oac is positional: oac <name> <url> [api_key]
    if provider == "oac" {
        let name = args.get(2).map(String::as_str).unwrap_or("oac");
        let base_url = args.get(3).map(String::as_str);
        let api_key = args.get(4).map(String::as_str);
        let creds =
            crate::providers::oauth::login_openai_compat(name, Some(name), base_url, api_key)
                .await?;
        return Ok(format!(
            "Logged in as '{name}' ({} at {}).",
            creds.name, creds.base_url
        ));
    }

    // Optional name: `sidekar repl login claude work` → nickname = "claude-work"
    let nickname: String = match args.get(2).map(String::as_str) {
        Some(name) if !name.starts_with('-') => {
            let base = provider.trim_end_matches('-');
            format!("{base}-{name}")
        }
        _ => provider.to_string(),
    };
    let nickname = nickname.as_str();

    let provider_type =
        crate::providers::oauth::resolve_provider_type_for_login(nickname, provider).ok_or_else(
            || {
                anyhow!(
                    "Unknown provider: '{provider}'.\nUse: claude, codex, or, oc, ocg, grok, gem, bedrock/brk, oac"
                )
            },
        )?;

    let kv_key = crate::providers::oauth::kv_key_for(nickname);
    let _ = crate::broker::kv_delete(&kv_key);

    match provider_type {
        "anthropic" => {
            let _ = crate::providers::oauth::login_anthropic(Some(nickname)).await?;
            Ok(format!("Logged in as '{nickname}' (Claude OAuth)."))
        }
        "codex" => {
            let (_, account_id) = crate::providers::oauth::login_codex(Some(nickname)).await?;
            Ok(format!(
                "Logged in as '{nickname}' (Codex, account: {}).",
                if account_id.is_empty() {
                    "unknown"
                } else {
                    &account_id
                }
            ))
        }
        "openrouter" => {
            let _ = crate::providers::oauth::login_openrouter(Some(nickname)).await?;
            Ok(format!("Logged in as '{nickname}' (OpenRouter)."))
        }
        "opencode" => {
            let _ = crate::providers::oauth::login_opencode(Some(nickname)).await?;
            Ok(format!("Logged in as '{nickname}' (OpenCode)."))
        }
        "opencode-go" => {
            let _ = crate::providers::oauth::login_opencode_go(Some(nickname)).await?;
            Ok(format!("Logged in as '{nickname}' (OpenCode Go)."))
        }
        "grok" => {
            let _ = crate::providers::oauth::login_grok(Some(nickname)).await?;
            Ok(format!("Logged in as '{nickname}' (Grok)."))
        }
        "gemini" => {
            let _ = crate::providers::oauth::login_gemini(Some(nickname)).await?;
            Ok(format!("Logged in as '{nickname}' (Gemini)."))
        }
        "bedrock" => {
            crate::providers::oauth::login_bedrock(Some(nickname)).await?;
            Ok(format!("Logged in as '{nickname}' (Amazon Bedrock)."))
        }
        _ => Err(anyhow!(
            "Unknown provider type for '{nickname}'.\nUse: claude, codex, or, oc, ocg, grok, gem, bedrock/brk, oac"
        )),
    }
}
