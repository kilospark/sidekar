//! Shared credential flows for `sidekar repl credential add` and `/credential add|update`.

use anyhow::{Result, anyhow, bail};

const CREDENTIAL_ADD_USAGE: &str = "\
Usage: sidekar repl credential add <provider> [name]

  Second token is an optional nickname suffix: claude + work → stored credential 'claude-work'.

Providers:
  claude     Claude (Anthropic) — OAuth
  codex      Codex (OpenAI) — OAuth
  or         OpenRouter — API key
  oc         OpenCode — API key
  grok       Grok (xAI) — API key
  gem        Gemini (Google) — API key
  bedrock | brk Amazon Bedrock — IAM profile / credential chain → HTTPS SigV4
  oac <nickname> <url> [api_key]

Examples:
  sidekar repl credential add claude
  sidekar repl credential add claude work       → stored as 'claude-work'
  sidekar repl credential add or personal       → stored as 'or-personal'
  sidekar repl credential add oac local http://localhost:11434/v1";

/// CLI missing-arg help and REPL `/credential add` (no tokens).
pub fn credential_add_usage_message() -> &'static str {
    CREDENTIAL_ADD_USAGE
}

pub async fn perform_credential_add(tokens: &[String]) -> Result<String> {
    let provider = match tokens.first().map(|s| s.as_str()) {
        Some(n) => n,
        None => bail!("{}", CREDENTIAL_ADD_USAGE),
    };

    // oac is positional: oac <nickname> <url> [api_key]
    if provider == "oac" {
        let name = tokens.get(1).map(|s| s.as_str()).unwrap_or("oac");
        let base_url = tokens.get(2).map(|s| s.as_str());
        let api_key = tokens.get(3).map(|s| s.as_str());
        let creds =
            crate::providers::oauth::login_openai_compat(name, Some(name), base_url, api_key)
                .await?;
        return Ok(format!(
            "Logged in as '{name}' ({} at {}).",
            creds.name, creds.base_url
        ));
    }

    // Optional suffix: `credential add claude work` → nickname = "claude-work"
    let nickname: String = match tokens.get(1).map(|s| s.as_str()) {
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
