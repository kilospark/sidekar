use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::sync::OnceLock;

use crate::command_catalog;
use crate::providers::ToolDef;
use crate::rtk;

// Cap on tool output size returned to the model. Larger outputs are truncated
// with a footer explaining how to paginate (Read offset/limit) or refine (Grep
// pattern). 40 KB keeps a turn well under a single cache breakpoint's budget.
const MAX_TOOL_OUTPUT_BYTES: usize = 40_000;

/// The canonical SKILL.md shipped with sidekar, embedded at build time so the
/// REPL's Sidekar tool description can reuse the operating rules without
/// drifting from what external agents (Claude Code, Cursor, etc.) see.
const SKILL_MD: &str = include_str!("../../SKILL.md");

/// Build the Sidekar tool description once per process.
///
/// Combines:
///   1. A fixed intro explaining how to invoke the tool.
///   2. A compact `Group: cmd, cmd, ...` catalog generated from
///      `command_catalog` — the same source `sidekar help` uses.
///   3. The "Operating Rules" section from the embedded SKILL.md so the
///      rules stay in lockstep with what other agents see.
fn sidekar_tool_description() -> &'static str {
    static DESCRIPTION: OnceLock<String> = OnceLock::new();
    DESCRIPTION.get_or_init(|| {
        let catalog = command_catalog::render_tool_catalog();
        let rules = skill_operating_rules().unwrap_or("");
        let mut out = String::new();
        out.push_str(
            "Run the `sidekar` CLI directly — browser/page automation, desktop \
automation, agent memory/tasks/repo, KV store, scheduled jobs, sessions, \
device/account management, daemon/config, and extension control. Prefer \
this over `Bash` when calling sidekar so the invocation is explicit and \
cacheable. Pass the subcommand and its arguments verbatim in `args` (do \
NOT include `sidekar` itself). For exact flags and examples on a command, \
call with args=[\"help\",\"<command>\"].\n\n",
        );
        out.push_str("## Command catalog\n");
        out.push_str(catalog);
        if !rules.is_empty() {
            out.push('\n');
            out.push_str(rules);
        }
        out
    })
}

/// Extract the "Operating Rules" section from the embedded SKILL.md so the
/// REPL tool description and the external-agent skill file stay in sync.
/// Returns the section (heading + body) or None if the section is missing.
fn skill_operating_rules() -> Option<&'static str> {
    static RULES: OnceLock<Option<&'static str>> = OnceLock::new();
    *RULES.get_or_init(|| {
        let start = SKILL_MD.find("## Operating Rules")?;
        let tail = &SKILL_MD[start..];
        // Stop at the next top-level heading, or take the rest of the file.
        let end_rel = tail[2..].find("\n## ").map(|i| i + 2).unwrap_or(tail.len());
        Some(tail[..end_rel].trim_end())
    })
}

/// Return tool definitions for the LLM.
///
/// Seven tools total: Bash for arbitrary shell, five file/search primitives
/// (Read, Write, Edit, Glob, Grep) that mirror Claude Code's built-ins so the
/// model doesn't burn turns shelling `cat`/`sed`/`find`/`rg`, and a dedicated
/// Sidekar tool whose description embeds the full command catalog and
/// operating rules so no discovery round-trip is needed.
pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "Bash".into(),
            description: "Execute a bash command and return its combined stdout+stderr. \
Use this for system/terminal operations that require a real shell — building, \
running tests, git, network probes, and any command without a dedicated tool. \
Do NOT use Bash for file reads (use Read), file writes (use Write), file \
edits (use Edit), filename search (use Glob), content search (use Grep), or \
sidekar CLI calls (use Sidekar) — the dedicated tools are cheaper and safer. \
Output is piped through sidekar's `rtk` compactor for known commands (git, \
cargo, npm, etc.) to reduce token usage on noisy output."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute. Multiple commands can be chained with '&&' or ';'."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 120, max: 600)."
                    }
                },
                "required": ["command"]
            }),
        },
        ToolDef {
            name: "Read".into(),
            description: "Read a UTF-8 text file from disk and return its contents with \
1-indexed line numbers. Use this instead of `bash cat`/`head`/`tail`. \
Supports pagination: set `offset` to start at a specific line and `limit` to \
cap how many lines are returned — useful for large files. Binary files will \
be rejected. Paths may be absolute or relative to the current working directory."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read. Relative paths resolve from the current working directory."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "1-indexed line to start reading from (default: 1)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to return (default: 2000)."
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "Write".into(),
            description: "Write (or overwrite) a file with the given UTF-8 contents. \
Creates parent directories as needed. Use this to create new files; prefer \
Edit for modifying existing files so you don't clobber unrelated changes. \
Paths may be absolute or relative to the current working directory."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path of the file to write. Relative paths resolve from the current working directory."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full file contents to write."
                    }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDef {
            name: "Edit".into(),
            description: "Edit a file by replacing an exact string with a new string. \
Requires `old_string` to appear exactly once in the file (otherwise the edit \
is rejected). For multiple occurrences, include enough surrounding context to \
make `old_string` unique, or set `replace_all: true` to replace every \
occurrence. Use this for targeted changes instead of rewriting the whole file \
with Write."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path of the file to edit. Relative paths resolve from the current working directory."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to find and replace. Must match byte-for-byte including whitespace."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The text to replace `old_string` with. Must differ from `old_string`."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace every occurrence instead of requiring exactly one match (default: false)."
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
        ToolDef {
            name: "Glob".into(),
            description: "Find files by glob pattern (e.g. `src/**/*.rs`, `**/*.{ts,tsx}`). \
Walks the directory tree honoring .gitignore. Returns matching paths sorted \
by recency (most recently modified first). Use this instead of `bash find` or \
`bash ls` for filename search. Much faster than Grep when you only need to \
know which files exist."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern (supports `*`, `**`, `?`, `[...]`, `{a,b}`)."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search (default: current working directory). Relative paths resolve from the current working directory."
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolDef {
            name: "Grep".into(),
            description: "Search file contents for a regular expression. Walks the tree \
honoring .gitignore and returns matching lines prefixed with `path:line:`. \
Use this instead of `bash grep`/`bash rg` — it's already optimized and \
respects sidekar's output cap. Filter by file extension with `glob` \
(e.g. `*.rs`) or by directory with `path`. Returns at most 200 matches by \
default — refine the pattern or narrow the scope if you need more."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regular expression to search for (Rust `regex` crate syntax)."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (default: current working directory)."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Optional filename glob to restrict search (e.g. `*.rs`)."
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "Match case-insensitively (default: false)."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Max number of matching lines to return (default: 200)."
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolDef {
            name: "Sidekar".into(),
            description: sidekar_tool_description().to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Subcommand and arguments, e.g. [\"memory\", \"list\"], [\"kv\", \"get\", \"key\"], [\"help\", \"browser\"]."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 120)."
                    }
                },
                "required": ["args"]
            }),
        },
    ]
}

/// Execute a tool call and return the output string.
pub async fn execute(
    name: &str,
    arguments: &Value,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<String> {
    // Accept either the canonical PascalCase name or the lowercase variants
    // the model may emit if it remembers an older schema.
    match name {
        "Bash" | "bash" => exec_bash(arguments, cancel).await,
        "Read" | "read" => exec_read(arguments),
        "Write" | "write" => exec_write(arguments),
        "Edit" | "edit" => exec_edit(arguments),
        "Glob" | "glob" => exec_glob(arguments),
        "Grep" | "grep" => exec_grep(arguments),
        "Sidekar" | "sidekar" => exec_sidekar(arguments, cancel).await,
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
    let timeout_secs = args
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(120)
        .min(600);

    let mut command_proc = tokio::process::Command::new("bash");
    command_proc.arg("-c").arg(command);
    let output = match run_subprocess_cancellable(
        command_proc,
        cancel,
        std::time::Duration::from_secs(timeout_secs),
    )
    .await
    {
        Ok(o) => o,
        Err(CancellableError::Cancelled) => return Err(super::Cancelled.into()),
        Err(CancellableError::Timeout) => bail!("bash: command timed out after {timeout_secs}s"),
        Err(CancellableError::Spawn(e)) => return Err(e.context("bash: failed to spawn")),
        Err(CancellableError::Io(e)) => return Err(e.context("bash: io error")),
    };

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
    Ok(truncate_output(&final_result))
}

fn exec_read(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .context("read: missing 'path'")?;
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

    let bytes = std::fs::read(path).with_context(|| format!("read: cannot read {path}"))?;
    if bytes.iter().take(8000).any(|b| *b == 0) {
        bail!("read: {path} looks like a binary file (contains NUL bytes)");
    }
    let text = String::from_utf8_lossy(&bytes);

    let start = offset.saturating_sub(1);
    let mut out = String::new();
    for (i, line) in text.lines().enumerate().skip(start).take(limit) {
        out.push_str(&format!("{:>6}\t{}\n", i + 1, line));
    }
    if out.is_empty() {
        out.push_str("(empty or offset past end of file)");
    }
    Ok(truncate_output(&out))
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

    if let Some(parent) = std::path::Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("write: cannot create parent dir for {path}"))?;
    }
    std::fs::write(path, content).with_context(|| format!("write: cannot write {path}"))?;
    Ok(format!("Wrote {} bytes to {path}", content.len()))
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
    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if old_string == new_string {
        bail!("edit: old_string and new_string must differ");
    }

    let original =
        std::fs::read_to_string(path).with_context(|| format!("edit: cannot read {path}"))?;
    let count = original.matches(old_string).count();
    if count == 0 {
        bail!("edit: old_string not found in {path}");
    }
    if !replace_all && count > 1 {
        bail!(
            "edit: old_string appears {count} times in {path}; add more context or set replace_all=true"
        );
    }

    let updated = if replace_all {
        original.replace(old_string, new_string)
    } else {
        original.replacen(old_string, new_string, 1)
    };
    std::fs::write(path, &updated).with_context(|| format!("edit: cannot write {path}"))?;
    Ok(format!(
        "Replaced {} occurrence{} in {path}",
        if replace_all { count } else { 1 },
        if (if replace_all { count } else { 1 }) == 1 {
            ""
        } else {
            "s"
        }
    ))
}

fn exec_glob(args: &Value) -> Result<String> {
    use globset::{Glob, GlobSetBuilder};
    use ignore::WalkBuilder;

    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .context("glob: missing 'pattern'")?;
    let root = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    let set = {
        let mut b = GlobSetBuilder::new();
        b.add(Glob::new(pattern).with_context(|| format!("glob: invalid pattern `{pattern}`"))?);
        b.build()?
    };

    let mut hits: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in WalkBuilder::new(&root).hidden(false).build().flatten() {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&root)
            .unwrap_or(entry.path())
            .to_path_buf();
        if set.is_match(&rel) || set.is_match(entry.path()) {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            hits.push((entry.path().to_path_buf(), mtime));
        }
    }
    hits.sort_by(|a, b| b.1.cmp(&a.1));

    if hits.is_empty() {
        return Ok(format!("(no matches for `{pattern}`)"));
    }
    let mut out = String::new();
    for (p, _) in hits.iter().take(500) {
        out.push_str(&p.display().to_string());
        out.push('\n');
    }
    if hits.len() > 500 {
        out.push_str(&format!(
            "\n(showing 500 of {} matches; refine the pattern to narrow)\n",
            hits.len()
        ));
    }
    Ok(truncate_output(&out))
}

fn exec_grep(args: &Value) -> Result<String> {
    use globset::{Glob, GlobSetBuilder};
    use ignore::WalkBuilder;
    use regex::RegexBuilder;

    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .context("grep: missing 'pattern'")?;
    let case_insensitive = args
        .get("case_insensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(200) as usize;
    let root = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));

    let re = RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .with_context(|| format!("grep: invalid regex `{pattern}`"))?;

    let glob_filter = match args.get("glob").and_then(|v| v.as_str()) {
        Some(g) => {
            let mut b = GlobSetBuilder::new();
            b.add(Glob::new(g).with_context(|| format!("grep: invalid glob `{g}`"))?);
            Some(b.build()?)
        }
        None => None,
    };

    let mut out = String::new();
    let mut hits = 0usize;
    'outer: for entry in WalkBuilder::new(&root).build().flatten() {
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        if let Some(ref filter) = glob_filter {
            let name = entry.file_name();
            if !filter.is_match(name) && !filter.is_match(entry.path()) {
                continue;
            }
        }
        let text = match std::fs::read_to_string(entry.path()) {
            Ok(t) => t,
            Err(_) => continue, // binary or unreadable, skip silently
        };
        for (lineno, line) in text.lines().enumerate() {
            if re.is_match(line) {
                out.push_str(&format!(
                    "{}:{}:{}\n",
                    entry.path().display(),
                    lineno + 1,
                    line
                ));
                hits += 1;
                if hits >= max_results {
                    out.push_str(&format!(
                        "\n(hit max_results={max_results}; refine the pattern if you need more)\n"
                    ));
                    break 'outer;
                }
            }
        }
    }
    if out.is_empty() {
        return Ok(format!("(no matches for /{pattern}/)"));
    }
    Ok(truncate_output(&out))
}

async fn exec_sidekar(
    args: &Value,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<String> {
    let argv = args
        .get("args")
        .and_then(|v| v.as_array())
        .context("sidekar: missing 'args' array")?;
    let string_args: Vec<String> = argv
        .iter()
        .map(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| v.to_string())
        })
        .collect();
    let timeout_secs = args
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(120)
        .min(600);

    let sidekar_bin =
        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("sidekar"));
    let mut cmd = tokio::process::Command::new(sidekar_bin);
    cmd.args(&string_args);
    let output = match run_subprocess_cancellable(
        cmd,
        cancel,
        std::time::Duration::from_secs(timeout_secs),
    )
    .await
    {
        Ok(o) => o,
        Err(CancellableError::Cancelled) => return Err(super::Cancelled.into()),
        Err(CancellableError::Timeout) => {
            bail!("sidekar: command timed out after {timeout_secs}s")
        }
        Err(CancellableError::Spawn(e)) => {
            return Err(e.context("sidekar: failed to spawn (is `sidekar` on PATH?)"));
        }
        Err(CancellableError::Io(e)) => return Err(e.context("sidekar: io error")),
    };

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
    if exit_code != 0 {
        raw.push_str(&format!("\nExit code: {exit_code}"));
    }
    if raw.is_empty() {
        raw.push_str("(no output)");
    }
    Ok(truncate_output(&raw))
}

fn truncate_output(s: &str) -> String {
    if s.len() <= MAX_TOOL_OUTPUT_BYTES {
        return s.to_string();
    }
    let mut out = s[..MAX_TOOL_OUTPUT_BYTES].to_string();
    // Avoid splitting a UTF-8 codepoint mid-byte.
    while !out.is_char_boundary(out.len()) {
        out.pop();
    }
    out.push_str(&format!(
        "\n\n[truncated: output was {} bytes, showing first {MAX_TOOL_OUTPUT_BYTES}]",
        s.len()
    ));
    out
}

async fn wait_for_cancel(cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>) {
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Spawn a command and collect its output, with cooperative cancellation
/// that kills the **entire process tree**, not just the immediate child.
///
/// Why this exists: `tokio::process::Command::kill_on_drop(true)` only
/// SIGKILLs the direct child. `bash -c "cargo build"` spawns bash; bash
/// forks cargo. Dropping the future kills bash; cargo is reparented to
/// init and keeps running. From the user's perspective Esc/Ctrl+C
/// "didn't work" — the agent moved on but their machine is still busy.
///
/// Fix: put the child into its own process group via pre_exec
/// `setpgid(0, 0)`. On cancel, signal the group (`-pgid`) with SIGTERM
/// so every descendant receives it; wait up to 500ms for graceful exit;
/// escalate to SIGKILL if still alive. `kill_on_drop` stays on as a
/// safety net for the direct child.
///
/// Unix-only. Windows path would need Job Objects; out of scope until
/// sidekar ships a Windows build.
#[cfg(unix)]
async fn run_subprocess_cancellable(
    mut cmd: tokio::process::Command,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    timeout: std::time::Duration,
) -> Result<std::process::Output, CancellableError> {
    // tokio::process::Command has its own `pre_exec` (cfg(unix)) with the
    // same signature as std's CommandExt method — no trait import needed.
    unsafe {
        cmd.pre_exec(|| {
            // New process group, pgid == child pid. Ignore errors — worst
            // case we fall back to single-process kill via kill_on_drop.
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    cmd.kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| CancellableError::Spawn(anyhow::Error::new(e)))?;
    let pid = match child.id() {
        Some(p) => p as i32,
        None => {
            // Already-exited or detached — no pgid to target.
            return child
                .wait_with_output()
                .await
                .map_err(|e| CancellableError::Io(anyhow::Error::new(e)));
        }
    };

    let wait_future = child.wait_with_output();
    tokio::pin!(wait_future);

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    let cancel_fut = async {
        match cancel {
            Some(c) => wait_for_cancel(c).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(cancel_fut);

    tokio::select! {
        biased;
        _ = &mut cancel_fut => {
            // SIGTERM the entire process group. Negative pid targets the
            // group; since we called setpgid(0, 0) in pre_exec, pgid == pid.
            unsafe { libc::kill(-pid, libc::SIGTERM); }
            // Give it up to 500ms to clean up.
            let graceful = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                &mut wait_future,
            ).await;
            if graceful.is_err() {
                // Still alive — SIGKILL the group.
                unsafe { libc::kill(-pid, libc::SIGKILL); }
                // Drain so we don't leak a zombie; short timeout because
                // SIGKILL is synchronous at the kernel level.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    &mut wait_future,
                ).await;
            }
            Err(CancellableError::Cancelled)
        }
        _ = &mut deadline => {
            // Timeout: same tree-kill escalation.
            unsafe { libc::kill(-pid, libc::SIGTERM); }
            let graceful = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                &mut wait_future,
            ).await;
            if graceful.is_err() {
                unsafe { libc::kill(-pid, libc::SIGKILL); }
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    &mut wait_future,
                ).await;
            }
            Err(CancellableError::Timeout)
        }
        result = &mut wait_future => {
            result.map_err(|e| CancellableError::Io(anyhow::Error::new(e)))
        }
    }
}

#[cfg(not(unix))]
async fn run_subprocess_cancellable(
    mut cmd: tokio::process::Command,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    timeout: std::time::Duration,
) -> Result<std::process::Output, CancellableError> {
    cmd.kill_on_drop(true);
    let output_future = cmd.output();
    let cancel_fut = async {
        match cancel {
            Some(c) => wait_for_cancel(c).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = cancel_fut => Err(CancellableError::Cancelled),
        result = tokio::time::timeout(timeout, output_future) => {
            match result {
                Err(_) => Err(CancellableError::Timeout),
                Ok(Err(e)) => Err(CancellableError::Io(anyhow::Error::new(e))),
                Ok(Ok(o)) => Ok(o),
            }
        }
    }
}

#[derive(Debug)]
enum CancellableError {
    Cancelled,
    Timeout,
    Spawn(anyhow::Error),
    Io(anyhow::Error),
}

#[cfg(test)]
mod tests;
