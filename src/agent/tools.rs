use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::providers::ToolDef;
use crate::rtk;

/// Return tool definitions for the LLM.
pub fn definitions() -> Vec<ToolDef> {
    vec![ToolDef {
        name: "bash".into(),
        description: "Execute a bash command and return its output.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120)"
                }
            },
            "required": ["command"]
        }),
    }]
}

/// Execute a tool call and return the output string.
pub async fn execute(
    name: &str,
    arguments: &Value,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<String> {
    match name {
        "bash" | "Bash" => exec_bash(arguments, cancel).await,
        _ => bail!("Unknown tool: {name}"),
    }
}

async fn exec_bash(
    args: &Value,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .context("bash: missing 'command'")?;
    let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(120);

    let mut command_proc = tokio::process::Command::new("bash");
    command_proc.kill_on_drop(true).arg("-c").arg(command);
    let output_future = command_proc.output();

    let output = match cancel {
        Some(cancel) => {
            tokio::select! {
                _ = wait_for_cancel(cancel) => return Err(super::Cancelled.into()),
                result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), output_future) => {
                    result
                }
            }
        }
        None => tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), output_future).await,
    }
    .context("bash: command timed out")?
    .context("bash: failed to execute")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = output.status.code().unwrap_or(-1);

    let mut raw = String::new();
    if !stdout.is_empty() {
        raw.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !raw.is_empty() {
            raw.push('\n');
        }
        raw.push_str(&stderr);
    }

    // Pipe through rtk compact filter for token-efficient output.
    // Compacts known tools (git, cargo, npm, cat, grep, etc.).
    // Unknown commands pass through unchanged.
    let result = if !raw.is_empty() {
        rtk::compact_output(command, &raw)
    } else {
        raw
    };

    let mut final_result = result;
    if exit_code != 0 {
        final_result.push_str(&format!("\nExit code: {exit_code}"));
    }
    if final_result.is_empty() {
        final_result.push_str("(no output)");
    }
    Ok(final_result)
}

async fn wait_for_cancel(cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>) {
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests;
