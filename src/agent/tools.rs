use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::providers::ToolDef;

/// Return tool definitions for the LLM.
pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
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
        },
    ]
}

/// Execute a tool call and return the output string.
pub async fn execute(name: &str, arguments: &Value) -> Result<String> {
    match name {
        "bash" | "Bash" => exec_bash(arguments).await,
        _ => bail!("Unknown tool: {name}"),
    }
}

async fn exec_bash(args: &Value) -> Result<String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .context("bash: missing 'command'")?;
    let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(120);

    // Execute the command
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new("bash")
            .arg("-c")
            .arg(command)
            .output(),
    )
    .await
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

    // Pipe through sidekar compact filter for token-efficient output.
    // Compacts known tools (git, cargo, npm, cat, grep, etc.).
    // Unknown commands pass through unchanged.
    let result = if !raw.is_empty() {
        compact_output(command, &raw).await.unwrap_or(raw)
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

/// Pipe raw output through `sidekar compact filter <command>` for token savings.
async fn compact_output(command: &str, raw: &str) -> Option<String> {
    let mut child = tokio::process::Command::new("sidekar")
        .arg("compact")
        .arg("filter")
        .args(command.split_whitespace())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    use tokio::io::AsyncWriteExt;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(raw.as_bytes()).await;
        drop(stdin);
    }

    let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .ok()?
        .ok()?;

    if output.status.success() {
        let compacted = String::from_utf8_lossy(&output.stdout).to_string();
        if !compacted.is_empty() {
            return Some(compacted);
        }
    }

    None
}
