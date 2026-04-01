use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;

use crate::providers::ToolDef;

/// Return tool definitions for the LLM.
pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "bash".into(),
            description: "Execute a bash command and return its output. Use this for shell \
                commands, build tools, git, and any CLI tools available on the system."
                .into(),
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
        ToolDef {
            name: "read".into(),
            description: "Read a file's contents. Returns text with line numbers. \
                Supports offset and limit for large files."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative file path"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Starting line number (1-based, default: 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read (default: 2000)"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "write".into(),
            description: "Write content to a file. Creates the file if it doesn't exist, \
                or overwrites it if it does."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative file path"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write"
                    }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDef {
            name: "edit".into(),
            description: "Replace an exact string in a file with new content. The old_string \
                must match exactly (including whitespace). Use for surgical edits."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative file path"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to find and replace"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement string"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
        ToolDef {
            name: "glob".into(),
            description: "Find files matching a glob pattern. Returns matching file paths \
                sorted by modification time."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern (e.g., \"**/*.rs\", \"src/**/*.ts\")"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (default: current directory)"
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolDef {
            name: "grep".into(),
            description: "Search file contents using a regex pattern. Returns matching \
                file paths or matching lines with context."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search in (default: current directory)"
                    },
                    "include": {
                        "type": "string",
                        "description": "Glob to filter files (e.g., \"*.rs\")"
                    }
                },
                "required": ["pattern"]
            }),
        },
    ]
}

/// Execute a tool call and return the output string.
pub async fn execute(name: &str, arguments: &Value) -> Result<String> {
    match name {
        "bash" => exec_bash(arguments).await,
        "read" => exec_read(arguments),
        "write" => exec_write(arguments),
        "edit" => exec_edit(arguments),
        "glob" => exec_glob(arguments),
        "grep" => exec_grep(arguments).await,
        _ => bail!("Unknown tool: {name}"),
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

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

fn exec_read(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("read: missing 'path'")?;
    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as usize;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(2000) as usize;

    let resolved = resolve_path(path);
    let content =
        std::fs::read_to_string(&resolved).with_context(|| format!("read: {}", resolved))?;

    let lines: Vec<&str> = content.lines().collect();
    let start = offset.saturating_sub(1);
    if start >= lines.len() {
        return Ok(String::new());
    }
    let end = (start + limit).min(lines.len());

    let mut result = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        result.push_str(&format!("{}\t{}\n", start + i + 1, line));
    }

    if end < lines.len() {
        result.push_str(&format!(
            "\n({} more lines not shown)",
            lines.len() - end
        ));
    }

    Ok(result)
}

fn exec_write(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("write: missing 'path'")?;
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .context("write: missing 'content'")?;

    let resolved = resolve_path(path);

    // Create parent directories if needed
    if let Some(parent) = Path::new(&resolved).parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&resolved, content).with_context(|| format!("write: {}", resolved))?;
    Ok(format!("Wrote {} bytes to {}", content.len(), path))
}

fn exec_edit(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("edit: missing 'path'")?;
    let old_string = args
        .get("old_string")
        .and_then(|v| v.as_str())
        .context("edit: missing 'old_string'")?;
    let new_string = args
        .get("new_string")
        .and_then(|v| v.as_str())
        .context("edit: missing 'new_string'")?;

    let resolved = resolve_path(path);
    let content =
        std::fs::read_to_string(&resolved).with_context(|| format!("edit: {}", resolved))?;

    let count = content.matches(old_string).count();
    if count == 0 {
        bail!("edit: old_string not found in {path}");
    }
    if count > 1 {
        bail!("edit: old_string found {count} times in {path} (must be unique)");
    }

    let new_content = content.replacen(old_string, new_string, 1);
    std::fs::write(&resolved, &new_content)?;
    Ok(format!("Edited {path}"))
}

fn exec_glob(args: &Value) -> Result<String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .context("glob: missing 'pattern'")?;
    let base = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    let resolved_base = resolve_path(base);

    // Use the ignore crate's WalkBuilder for gitignore-aware globbing
    let glob = globset::GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .context("glob: invalid pattern")?
        .compile_matcher();

    let mut matches: Vec<String> = Vec::new();
    let walker = ignore::WalkBuilder::new(&resolved_base)
        .hidden(false)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_file() {
            let relative = path
                .strip_prefix(&resolved_base)
                .unwrap_or(path)
                .to_string_lossy();
            if glob.is_match(relative.as_ref()) {
                matches.push(relative.to_string());
            }
        }
    }

    matches.sort();
    if matches.is_empty() {
        Ok("No files matched.".into())
    } else {
        let total = matches.len();
        let shown: Vec<_> = matches.into_iter().take(200).collect();
        let mut result = shown.join("\n");
        if total > 200 {
            result.push_str(&format!("\n\n({} more files not shown)", total - 200));
        }
        Ok(result)
    }
}

async fn exec_grep(args: &Value) -> Result<String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .context("grep: missing 'pattern'")?;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    let include = args.get("include").and_then(|v| v.as_str());

    let resolved = resolve_path(path);

    let mut cmd_args = vec![
        "-rn".to_string(),
        "--color=never".to_string(),
        "-m".to_string(),
        "100".to_string(),
    ];
    if let Some(inc) = include {
        cmd_args.push("--include".to_string());
        cmd_args.push(inc.to_string());
    }
    cmd_args.push(pattern.to_string());
    cmd_args.push(resolved);

    let output = tokio::process::Command::new("grep")
        .args(&cmd_args)
        .output()
        .await
        .context("grep: failed to execute")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    match output.status.code().unwrap_or(-1) {
        0 => Ok(stdout.to_string()),
        1 => Ok("No matches found.".into()),
        code => {
            let detail = stderr.trim();
            if detail.is_empty() {
                bail!("grep failed with exit code {code}")
            } else {
                bail!("grep failed with exit code {code}: {detail}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_path(path: &str) -> String {
    if path.starts_with('/') || path.starts_with('~') {
        if path.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                return path.replacen('~', &home.to_string_lossy(), 1);
            }
        }
        path.to_string()
    } else {
        // Relative to cwd
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string());
        format!("{}/{}", cwd, path)
    }
}
