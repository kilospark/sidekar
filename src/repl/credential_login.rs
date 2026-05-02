//! Shared credential flows for `sidekar repl credential add` and `/credential add|update`.

use anyhow::{Context, Result, anyhow, bail};
use std::io::Write;

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
  oac <nickname> <url> [api_key|adc]

Examples:
  sidekar repl credential add claude
  sidekar repl credential add claude work       → stored as 'claude-work'
  sidekar repl credential add or personal       → stored as 'or-personal'
  sidekar repl credential add oac local http://localhost:11434/v1";

/// CLI missing-arg help and REPL `/credential add` (no tokens).
pub fn credential_add_usage_message() -> &'static str {
    CREDENTIAL_ADD_USAGE
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractiveOutput {
    Cli,
    Repl,
}

fn output_line(output: InteractiveOutput, text: &str) {
    match output {
        InteractiveOutput::Cli => eprintln!("{text}"),
        InteractiveOutput::Repl => crate::tunnel::tunnel_println(text),
    }
}

fn output_prompt(output: InteractiveOutput, text: &str) {
    match output {
        InteractiveOutput::Cli => {
            eprint!("{text}");
            let _ = std::io::stderr().flush();
        }
        InteractiveOutput::Repl => {
            print!("{text}");
            let _ = std::io::stdout().flush();
            crate::tunnel::tunnel_send(text.as_bytes().to_vec());
        }
    }
}

fn prompt_required(
    output: InteractiveOutput,
    label: &str,
    default: Option<&str>,
) -> Result<String> {
    match default {
        Some(default) => output_prompt(output, &format!("{label} [{default}]: ")),
        None => output_prompt(output, &format!("{label}: ")),
    }
    let mut value = String::new();
    std::io::stdin()
        .read_line(&mut value)
        .with_context(|| format!("failed to read {label}"))?;
    let value = value.trim();
    let value = if value.is_empty() {
        default.unwrap_or("")
    } else {
        value
    };
    if value.is_empty() {
        bail!("No {label} provided");
    }
    Ok(value.to_string())
}

fn prompt_optional(output: InteractiveOutput, label: &str) -> Result<Option<String>> {
    output_prompt(output, &format!("{label}: "));
    let mut value = String::new();
    std::io::stdin()
        .read_line(&mut value)
        .with_context(|| format!("failed to read {label}"))?;
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value.to_string()))
    }
}

fn open_browser_hint(url: &str) {
    let _ = crate::providers::oauth::open_browser_url(url);
}

pub async fn perform_credential_add(
    tokens: &[String],
    output: InteractiveOutput,
) -> Result<String> {
    let provider = match tokens.first().map(|s| s.as_str()) {
        Some(n) => n,
        None => bail!("{}", CREDENTIAL_ADD_USAGE),
    };

    // oac is positional: oac <nickname> <url> [api_key]
    if provider == "oac" {
        let name = tokens.get(1).map(|s| s.as_str()).unwrap_or("oac");
        let display_name = name.to_string();
        let base_url = match tokens.get(2).map(|s| s.as_str()) {
            Some(url) if !url.trim().is_empty() => url.trim().to_string(),
            _ => prompt_required(output, "Base URL", None)?,
        };
        let api_key = match tokens.get(3).map(|s| s.as_str()) {
            Some(key) if !key.trim().is_empty() => key.trim().to_string(),
            _ => prompt_required(
                output,
                "API key (adc = GCP Application Default Credentials)",
                None,
            )?,
        };
        let creds =
            if api_key.eq_ignore_ascii_case("adc") || api_key.eq_ignore_ascii_case("gcp-adc") {
                crate::providers::oauth::save_openai_compat_adc(name, &display_name, &base_url)?
            } else {
                crate::providers::oauth::save_openai_compat_credential(
                    name,
                    &display_name,
                    &base_url,
                    &api_key,
                )?
            };
        output_line(output, &format!("OpenAI-compat credential saved for '{name}'."));
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
            output_line(output, "No Anthropic credentials found. Starting OAuth login...");
            let login = crate::providers::oauth::begin_anthropic_login(Some(nickname)).await?;
            output_line(output, "");
            output_line(
                output,
                &format!("Opening browser for {} login...", login.provider_name),
            );
            output_line(
                output,
                &format!("If browser doesn't open, visit:\n{}\n", login.auth_url),
            );
            open_browser_hint(&login.auth_url);
            let _ = crate::providers::oauth::finish_anthropic_login(login).await?;
            output_line(output, "Logged in to Anthropic.");
            Ok(format!("Logged in as '{nickname}' (Claude OAuth)."))
        }
        "codex" => {
            output_line(output, "No Codex credentials found. Starting OAuth login...");
            let login = crate::providers::oauth::begin_codex_login(Some(nickname)).await?;
            output_line(output, "");
            output_line(
                output,
                &format!("Opening browser for {} login...", login.provider_name),
            );
            output_line(
                output,
                &format!("If browser doesn't open, visit:\n{}\n", login.auth_url),
            );
            open_browser_hint(&login.auth_url);
            let (_, account_id) = crate::providers::oauth::finish_codex_login(login).await?;
            output_line(output, "Logged in to Codex.");
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
            output_line(output, "No OpenRouter credentials found.");
            output_line(output, "Get an API key from https://openrouter.ai/keys");
            let key = prompt_required(output, "API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "openrouter",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "OpenRouter API key saved.");
            Ok(format!("Logged in as '{nickname}' (OpenRouter)."))
        }
        "opencode" => {
            output_line(output, "No OpenCode credentials found. Opening https://opencode.ai/auth ...");
            open_browser_hint("https://opencode.ai/auth");
            let key = prompt_required(output, "Paste API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "opencode",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "OpenCode API key saved.");
            Ok(format!("Logged in as '{nickname}' (OpenCode)."))
        }
        "opencode-go" => {
            output_line(
                output,
                "No OpenCode Go credentials found. Opening https://opencode.ai/auth ...",
            );
            open_browser_hint("https://opencode.ai/auth");
            let key = prompt_required(output, "Paste API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "opencode-go",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "OpenCode Go API key saved.");
            Ok(format!("Logged in as '{nickname}' (OpenCode Go)."))
        }
        "grok" => {
            output_line(output, "No Grok credentials found. Opening https://console.x.ai/ ...");
            open_browser_hint("https://console.x.ai/");
            let key = prompt_required(output, "API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "grok",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "Grok API key saved.");
            Ok(format!("Logged in as '{nickname}' (Grok)."))
        }
        "gemini" => {
            output_line(
                output,
                "No Gemini credentials found. Opening https://aistudio.google.com/apikey ...",
            );
            open_browser_hint("https://aistudio.google.com/apikey");
            let key = prompt_required(output, "API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "gemini",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "Gemini API key saved.");
            Ok(format!("Logged in as '{nickname}' (Gemini)."))
        }
        "bedrock" => {
            output_line(
                output,
                "Bedrock uses IAM via AWS SDK default chain (environment, ~/.aws/credentials, SSO, …).",
            );
            let region = prompt_required(output, "AWS region", Some("us-east-1"))?;
            let profile = prompt_optional(
                output,
                "AWS named profile (optional, Enter → default credential chain)",
            )?;
            crate::providers::oauth::save_bedrock_credential(
                nickname,
                &region,
                profile.as_deref(),
            )?;
            output_line(
                output,
                &format!(
                    "Saved Bedrock config to `{kv_key}`. Uses HTTPS + SigV4 (no aws-sdk-bedrock crates). IAM needs `bedrock:ListFoundationModels` (for `/model list`), `bedrock:ListInferenceProfiles` (recommended: resolve system inference profiles for Claude 4.x), and `bedrock:InvokeModelWithResponseStream`."
                ),
            );
            Ok(format!("Logged in as '{nickname}' (Amazon Bedrock)."))
        }
        _ => Err(anyhow!(
            "Unknown provider type for '{nickname}'.\nUse: claude, codex, or, oc, ocg, grok, gem, bedrock/brk, oac"
        )),
    }
}
