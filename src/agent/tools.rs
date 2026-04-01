use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::providers::ToolDef;

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
    let timeout_secs = args
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(120);

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

    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("STDERR:\n");
        result.push_str(&stderr);
    }
    if exit_code != 0 {
        result.push_str(&format!("\nExit code: {exit_code}"));
    }
    if result.is_empty() {
        result.push_str("(no output)");
    }
    Ok(result)
}
