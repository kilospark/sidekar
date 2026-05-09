//! Shared credential flows for `sidekar repl credential add` and `/credential add|update`.

use anyhow::{Context, Result, anyhow, bail};
use std::io::Write;

const REPL_CREDENTIAL_HELP: &str = "\
Sidekar LLM credentials (each login is stored under KV key `oauth:<nickname>`).

Commands:
  sidekar repl credential add <provider> [nickname]   Add or refresh credentials
  sidekar repl credentials                             List saved credentials (nickname + provider)
  sidekar repl logout [nickname | all]                 Remove one credential or wipe all

How `credential add` works
  The first token after `add` is the **provider keyword** (what you are logging into).
  The optional second token is the **nickname** — it becomes the credential name / KV key.
  If you omit the nickname, the provider keyword itself is used as the key (e.g. only `gemini` → key `gemini`).
  Provider type is always saved in credential metadata; the nickname string does **not** imply the provider.

Providers (first argument to `credential add`):
  claude              Claude (Anthropic) — OAuth device flow
  codex               Codex (OpenAI) — OAuth device flow
  openrouter          OpenRouter — API key
  opencode-zen        OpenCode Zen — API key
  opencode-go         OpenCode Go — API key
  grok                Grok (xAI) — API key
  gemini              Gemini (Google) — API key
  cursor              Cursor — `CURSOR_API_KEY` → `api2.cursor.sh` (Connect / MitM-visible path); REPL stream still blocked on protobuf `AgentService.Run`
  bedrock             Amazon Bedrock — IAM / SigV4
  vertex              GCP Vertex AI (OpenAI-compat) — project id + region; Bearer via `gcloud`
  openai-compat       Generic OpenAI-compat — uses a positional form (see below)

OpenAI-compat (positional only)
  sidekar repl credential add openai-compat <nickname> <base_url> [api_key | adc]

Examples:
  sidekar repl credential add claude
  sidekar repl credential add claude work               → nickname `work`
  sidekar repl credential add vertex prod              → nickname `prod` (Vertex / GCP)
  sidekar repl credential add openrouter personal       → nickname `personal`
  sidekar repl credential add openai-compat local http://localhost:11434/v1";

/// Full credential CLI help (`sidekar repl credential`, `--help`, unknown subcommand, empty `/credential add`).
pub fn credential_add_usage_message() -> &'static str {
    REPL_CREDENTIAL_HELP
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
        InteractiveOutput::Repl => crate::tunnel::tunnel_print(text),
    }
}

fn relay_line_read(output: InteractiveOutput, relay_input_fd: Option<i32>) -> Result<String> {
    let tunnel_fd = match output {
        InteractiveOutput::Cli => None,
        InteractiveOutput::Repl => relay_input_fd,
    };
    super::editor::read_line_stdio_or_tunnel(tunnel_fd).map_err(|e| {
        if e.kind() == std::io::ErrorKind::Interrupted {
            anyhow!("Cancelled.")
        } else {
            anyhow!(e)
        }
    })
}

fn prompt_required(
    output: InteractiveOutput,
    relay_input_fd: Option<i32>,
    label: &str,
    default: Option<&str>,
) -> Result<String> {
    match default {
        Some(default) => output_prompt(output, &format!("{label} [{default}]: ")),
        None => output_prompt(output, &format!("{label}: ")),
    }
    let value = relay_line_read(output, relay_input_fd)
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

fn prompt_optional(
    output: InteractiveOutput,
    relay_input_fd: Option<i32>,
    label: &str,
) -> Result<Option<String>> {
    output_prompt(output, &format!("{label}: "));
    let value = relay_line_read(output, relay_input_fd)
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
    relay_input_fd: Option<i32>,
) -> Result<String> {
    let provider = match tokens.first().map(|s| s.as_str()) {
        Some(n) => n,
        None => bail!("{}", credential_add_usage_message()),
    };

    // openai-compat is positional: openai-compat <nickname> <url> [api_key]
    if provider == "openai-compat" {
        let name = tokens.get(1).map(|s| s.as_str()).unwrap_or("openai-compat");
        let display_name = name.to_string();
        let base_url = match tokens.get(2).map(|s| s.as_str()) {
            Some(url) if !url.trim().is_empty() => url.trim().to_string(),
            _ => prompt_required(output, relay_input_fd, "Base URL", None)?,
        };
        let api_key = match tokens.get(3).map(|s| s.as_str()) {
            Some(key) if !key.trim().is_empty() => key.trim().to_string(),
            _ => prompt_required(
                output,
                relay_input_fd,
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
        output_line(
            output,
            &format!("OpenAI-compat credential saved for '{name}'."),
        );
        return Ok(format!(
            "Logged in as '{name}' ({} at {}).",
            creds.name, creds.base_url
        ));
    }

    // Optional second token: bare nickname (`credential add openrouter personal` → `personal`).
    let nickname: String = match tokens.get(1).map(|s| s.as_str()) {
        Some(name) if !name.starts_with('-') => name.trim().to_string(),
        _ => provider.trim_end_matches('-').to_string(),
    };
    let nickname = nickname.as_str();

    let provider_type =
        crate::providers::oauth::resolve_provider_type_for_login(nickname, provider).ok_or_else(
            || {
                anyhow!(
                    "Unknown provider: '{provider}'.\nUse: claude, codex, openrouter, opencode-zen, opencode-go, grok, gemini, cursor, bedrock, vertex, openai-compat"
                )
            },
        )?;

    let kv_key = crate::providers::oauth::kv_key_for(nickname);
    let _ = crate::broker::kv_delete(&kv_key);

    match provider_type {
        "anthropic" => {
            output_line(
                output,
                "No Anthropic credentials found. Starting OAuth login...",
            );
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
            output_line(
                output,
                "No Codex credentials found. Starting OAuth login...",
            );
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
            let key = prompt_required(output, relay_input_fd, "API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "openrouter",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "OpenRouter API key saved.");
            Ok(format!("Logged in as '{nickname}' (OpenRouter)."))
        }
        "opencode-zen" => {
            output_line(
                output,
                "No OpenCode Zen credentials found. Opening https://opencode.ai/auth ...",
            );
            open_browser_hint("https://opencode.ai/auth");
            let key = prompt_required(output, relay_input_fd, "Paste API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "opencode-zen",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "OpenCode Zen API key saved.");
            Ok(format!("Logged in as '{nickname}' (OpenCode Zen)."))
        }
        "opencode-go" => {
            output_line(
                output,
                "No OpenCode Go credentials found. Opening https://opencode.ai/auth ...",
            );
            open_browser_hint("https://opencode.ai/auth");
            let key = prompt_required(output, relay_input_fd, "Paste API key", None)?;
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
            output_line(
                output,
                "No Grok credentials found. Opening https://console.x.ai/ ...",
            );
            open_browser_hint("https://console.x.ai/");
            let key = prompt_required(output, relay_input_fd, "API key", None)?;
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
            let key = prompt_required(output, relay_input_fd, "API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "gemini",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "Gemini API key saved.");
            Ok(format!("Logged in as '{nickname}' (Gemini)."))
        }
        "cursor" => {
            output_line(
                output,
                "Cursor uses your API key against the backend Sidekar already logs (default https://api2.cursor.sh). Full REPL streaming needs Rust protobuf for `agent.v1.AgentService/Run` — see src/providers/cursor.rs header.",
            );
            open_browser_hint("https://cursor.com/docs");
            let key = prompt_required(output, relay_input_fd, "Cursor API key", None)?;
            crate::providers::oauth::save_api_key_credential(
                &kv_key,
                "cursor",
                &key,
                serde_json::json!({}),
            )?;
            output_line(output, "Cursor API key saved.");
            Ok(format!("Logged in as '{nickname}' (Cursor API key)."))
        }
        "bedrock" => {
            output_line(
                output,
                "Bedrock uses IAM via AWS SDK default chain (environment, ~/.aws/credentials, SSO, …).",
            );
            let region = prompt_required(output, relay_input_fd, "AWS region", Some("us-east-1"))?;
            let profile = prompt_optional(
                output,
                relay_input_fd,
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
        "gcp" => {
            output_line(
                output,
                "Vertex OpenAI-compat uses `gcloud auth print-access-token` as Bearer. Run `gcloud auth login` if needed.",
            );
            let project = prompt_required(output, relay_input_fd, "GCP project id", None)?;
            let location = prompt_required(
                output,
                relay_input_fd,
                "Vertex location (region), e.g. us-central1 or global",
                Some("us-central1"),
            )?;
            crate::providers::oauth::save_gcp_vertex_credential(nickname, &project, &location)?;
            output_line(
                output,
                &format!(
                    "Saved GCP Vertex config to `{kv_key}` (OpenAI-compat base URL in metadata)."
                ),
            );
            Ok(format!(
                "Logged in as '{nickname}' (GCP Vertex, project {project}, {location})."
            ))
        }
        _ => Err(anyhow!(
            "Unknown provider type for '{nickname}'.\nUse: claude, codex, openrouter, opencode-zen, opencode-go, grok, gemini, cursor, bedrock, vertex, openai-compat"
        )),
    }
}
