//! `sidekar mcp`: a Model Context Protocol server over stdio.
//!
//! Why this exists alongside the skill. Agents that run a shell command read
//! `SKILL.md` and call the `sidekar` binary directly — they need no server, and
//! that path stays the default. The holdouts are GUI and sandboxed apps:
//! Claude Desktop and Cowork run skills in Anthropic's cloud sandbox, and Codex
//! sandboxes shell commands, so none of them can reach a local binary the way a
//! terminal agent does. For those, an MCP server is the only way in.
//!
//! The design is deliberately thin. One generic `sidekar` tool shells out to
//! the same CLI dispatch every other agent uses, so the tool surface can never
//! drift from the commands (the description is generated from the command
//! catalog). A second `bus_inbox` tool drains inter-agent messages: because the
//! server is one long-lived process, it registers on the bus and attaches to
//! the daemon like any PTY agent, so messages addressed to it are delivered
//! here rather than stranded in the queue. MCP cannot wake an idle
//! conversation, so each tool result notes how many messages are waiting.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(test)]
mod tests;

/// The MCP revision we implement. We echo the client's requested version when
/// it sends one, so a newer client negotiating down still works.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

/// How long a single tool call may run before it is killed. Kept under the
/// tool-call timeouts the clients impose (Codex defaults to 60s), so a runaway
/// `bus await` fails as a tool error rather than hanging the whole session.
const RUN_TIMEOUT_SECS: u64 = 110;

/// Commands the generic tool refuses. Each is interactive, long-running, or
/// changes the local setup — none fit a single request/response tool call, and
/// one (`uninstall`) is destructive. Everything else in the catalog is allowed.
fn command_blocked(cmd: &str) -> Option<&'static str> {
    match cmd {
        "repl" => Some("`repl` is an interactive session, not a single tool call"),
        "session" => Some("`session` runs an interactive agent, not a single tool call"),
        "daemon" => Some("the daemon is managed automatically; it is not a tool"),
        "mcp" => Some("`mcp` is this server; it cannot call itself"),
        "device" => Some("`device login` is interactive; run it in a terminal"),
        "install" | "uninstall" | "update" => {
            Some("install/uninstall/update change the local setup and are not exposed as tools")
        }
        _ => None,
    }
}

/// The `sidekar` tool's description, generated from the command catalog so it
/// lists exactly the commands the binary has — it can never fall out of date.
fn sidekar_tool_description() -> String {
    use std::collections::BTreeMap;
    let mut by_group: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for s in crate::command_catalog::command_specs() {
        if command_blocked(s.name).is_some() {
            continue;
        }
        by_group.entry(s.group.title()).or_default().push(s.name);
    }
    let mut desc = String::from(
        "Run a Sidekar CLI command. Pass argv as `args`, e.g. [\"bus\",\"send\",\"alice\",\"hi\"].\n\
         Sidekar adds inter-agent messaging, encrypted secrets (kv/totp/hotp), durable memory and \
         tasks, real-browser and macOS desktop automation, scheduled jobs, and repo tools.\n\n\
         Commands by group:\n",
    );
    for (group, mut names) in by_group {
        names.sort_unstable();
        desc.push_str(&format!("  {group}: {}\n", names.join(", ")));
    }
    desc.push_str(
        "\nRun [\"help\", \"<command>\"] for a command's full usage. New inter-agent messages are \
         noted at the end of each result; read them with the bus_inbox tool. Keep `bus await` \
         timeouts short, since a tool call is time-limited.",
    );
    desc
}

/// The two tools this server exposes.
fn tool_list() -> Value {
    json!([
        {
            "name": "sidekar",
            "description": sidekar_tool_description(),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "The sidekar command and its arguments, e.g. [\"bus\",\"who\"]."
                    }
                },
                "required": ["args"]
            }
        },
        {
            "name": "bus_inbox",
            "description": "Return and clear inter-agent bus messages delivered to this session \
                            since the last check. Messages arrive while you work; this is how you \
                            read them.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

fn ok_response(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result })
}

fn err_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": { "code": code, "message": message } })
}

/// A tool result: a single text block, flagged as an error or not.
fn tool_text(text: &str, is_error: bool) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": is_error })
}

/// The running server: its bus identity, the inbox the daemon-attach thread
/// fills, and the binary to shell out to for tool calls.
struct Server {
    bus_name: String,
    inbox: Arc<Mutex<Vec<String>>>,
    exe: PathBuf,
}

impl Server {
    fn drain_inbox(&self) -> Vec<String> {
        let mut guard = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *guard)
    }

    fn inbox_len(&self) -> usize {
        self.inbox.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Append a one-line note about waiting messages, since MCP cannot push
    /// them into the conversation on its own.
    fn with_inbox_note(&self, mut text: String) -> String {
        let n = self.inbox_len();
        if n > 0 {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            let plural = if n == 1 { "" } else { "s" };
            text.push_str(&format!(
                "\n[sidekar: {n} new bus message{plural} waiting — call bus_inbox to read]"
            ));
        }
        text
    }

    /// Run one `sidekar` command as a subprocess, returning its output as the
    /// tool text on success or an error string on failure. Validation runs
    /// before any spawn, so a blocked or unknown command never starts a child.
    async fn run_cli(&self, argv: &[String]) -> std::result::Result<String, String> {
        let Some(cmd) = argv.first() else {
            return Err("No command given. Pass args like [\"bus\",\"who\"].".to_string());
        };
        let canonical = crate::canonical_command_name(cmd).unwrap_or(cmd);
        // `help` is handled in main.rs before the command table, so it is not a
        // catalog command — but the tool description tells the model to run it,
        // so it must pass. Everything else is checked against the catalog.
        if canonical != "help" {
            if let Some(reason) = command_blocked(canonical) {
                return Err(format!("`{cmd}` is not available over MCP: {reason}."));
            }
            if !crate::is_known_command(canonical) {
                return Err(format!(
                    "Unknown sidekar command: {cmd}. Call tools/list, or run [\"help\"] for the command list."
                ));
            }
        }

        let mut command = tokio::process::Command::new(&self.exe);
        command
            .args(argv)
            .env("SIDEKAR_AGENT_NAME", &self.bus_name)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);

        let timeout = std::time::Duration::from_secs(RUN_TIMEOUT_SECS);
        let output = match tokio::time::timeout(timeout, command.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(format!("failed to run `sidekar {cmd}`: {e}")),
            Err(_) => {
                return Err(format!(
                    "`sidekar {cmd}` timed out after {RUN_TIMEOUT_SECS}s and was stopped."
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            let mut body = stdout.into_owned();
            let stderr = stderr.trim_end();
            if !stderr.is_empty() {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(stderr);
            }
            if body.trim().is_empty() {
                body = format!("`sidekar {cmd}` failed with no output.");
            }
            return Err(body);
        }
        // Some commands print only to stderr (progress, notes); keep it when
        // stdout is empty so the tool never returns a blank success.
        if stdout.trim().is_empty() && !stderr.trim().is_empty() {
            return Ok(stderr.into_owned());
        }
        Ok(stdout.into_owned())
    }
}

/// Dispatch one JSON-RPC request. Returns `None` for notifications (no `id`),
/// which take no response.
async fn handle_request(srv: &Server, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params");

    match method {
        "initialize" => {
            let version = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Some(ok_response(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "sidekar", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": crate::skill::skill_text(),
                }),
            ))
        }
        "tools/list" => Some(ok_response(id, json!({ "tools": tool_list() }))),
        "tools/call" => Some(handle_tool_call(srv, id, params).await),
        "ping" => Some(ok_response(id, json!({}))),
        // Notifications we accept and ignore.
        _ if id.is_none() => None,
        other => Some(err_response(id, -32601, &format!("method not found: {other}"))),
    }
}

async fn handle_tool_call(srv: &Server, id: Option<Value>, params: Option<&Value>) -> Value {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = params.and_then(|p| p.get("arguments"));

    match name {
        "bus_inbox" => {
            let msgs = srv.drain_inbox();
            let text = if msgs.is_empty() {
                "No new bus messages.".to_string()
            } else {
                msgs.join("\n\n")
            };
            ok_response(id, tool_text(&text, false))
        }
        "sidekar" => {
            let argv: Vec<String> = arguments
                .and_then(|a| a.get("args"))
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            match srv.run_cli(&argv).await {
                Ok(text) => ok_response(id, tool_text(&srv.with_inbox_note(text), false)),
                Err(text) => ok_response(id, tool_text(&text, true)),
            }
        }
        other => err_response(id, -32602, &format!("unknown tool: {other}")),
    }
}

/// Entry point for `sidekar mcp [subcommand]`.
pub async fn handle(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None => serve().await,
        Some("install") => install(),
        Some("status") => {
            print_status();
            Ok(())
        }
        Some(other) => {
            bail!("unknown `mcp` subcommand: {other}\nUsage: sidekar mcp [install|status]")
        }
    }
}

/// Register on the bus, start draining the inbox, then serve JSON-RPC on stdio
/// until the client closes stdin.
async fn serve() -> Result<()> {
    let project = crate::bus::detect_project_name();
    let nick = crate::bus::pick_nickname_for_project(Some(&project));
    let bus_name = crate::bus::presence::unique_name(&format!("sidekar-mcp-{project}"));
    let channel = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Held for the whole session; Drop leaves the bus on the way out. A crash
    // that skips Drop is swept by the daemon, which recognizes the `mcp-<pid>`
    // pane (see bus::presence::pid_of_pane).
    let _presence = crate::bus::presence::Presence::register(crate::bus::presence::Registration {
        name: bus_name.clone(),
        nick,
        channel,
        pane: format!("mcp-{}", std::process::id()),
        agent_type: "sidekar-mcp",
        history: None,
    })
    .ok();

    // SAFETY: set once at startup, before the stdio loop awaits or the inbox
    // thread spawns, so no task has observed the old value yet.
    unsafe { std::env::set_var("SIDEKAR_AGENT_NAME", &bus_name) };
    crate::runtime::set_agent_name(Some(bus_name.clone()));

    let inbox: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    spawn_inbox_reader(bus_name.clone(), inbox.clone());

    let srv = Server {
        bus_name,
        inbox,
        exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sidekar")),
    };

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break; // client closed stdin
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(req) => handle_request(&srv, &req).await,
            Err(e) => Some(err_response(None, -32700, &format!("parse error: {e}"))),
        };
        if let Some(response) = response {
            let mut out = serde_json::to_string(&response)?;
            out.push('\n');
            stdout.write_all(out.as_bytes()).await?;
            stdout.flush().await?;
        }
    }

    drop(_presence); // leave the bus before exiting
    Ok(())
}

/// Attach to the daemon under our bus name and buffer delivered messages. Runs
/// on its own thread with a blocking socket, like the PTY poller; reconnects if
/// the daemon restarts. The process exiting tears the thread down.
fn spawn_inbox_reader(bus_name: String, inbox: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        loop {
            let _ = attach_once(&bus_name, &inbox);
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

fn attach_once(bus_name: &str, inbox: &Arc<Mutex<Vec<String>>>) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    crate::daemon::ensure_running()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let stream = UnixStream::connect(crate::daemon::socket_path())?;
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{}", json!({ "type": "bus_attach", "agent": bus_name }))?;
    writer.flush()?;

    for line in BufReader::new(stream).lines() {
        let line = line?;
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if frame.get("type").and_then(Value::as_str) != Some("bus_message") {
            continue;
        }
        let sender = frame.get("sender").and_then(Value::as_str).unwrap_or("?");
        let body = frame.get("body").and_then(Value::as_str).unwrap_or("");
        inbox
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(format!("From {sender}:\n{body}"));
        if let Some(msg_id) = frame.get("id").and_then(Value::as_i64) {
            // Ack so the daemon marks it delivered and stops re-handing it out.
            let _ = writeln!(writer, "{}", json!({ "type": "bus_ack", "id": msg_id, "ok": true }));
            let _ = writer.flush();
        }
    }
    Ok(())
}

/// Register this server with the MCP-capable apps found on this machine.
fn install() -> Result<()> {
    let exe = std::env::current_exe()
        .context("cannot determine the sidekar binary path")?
        .to_string_lossy()
        .into_owned();

    println!();
    println!("Registering the sidekar MCP server...");
    let mut any = false;

    if crate::which_bin("codex").is_some() {
        any = true;
        // Codex stores MCP servers in ~/.codex/config.toml; let its own CLI
        // write the entry so the TOML stays valid.
        let status = std::process::Command::new("codex")
            .args(["mcp", "add", "sidekar", "--", &exe, "mcp"])
            .status();
        match status {
            Ok(s) if s.success() => println!("  Codex: registered (codex mcp add sidekar)"),
            Ok(s) => println!("  Codex: `codex mcp add` exited with {s}"),
            Err(e) => println!("  Codex: could not run codex: {e}"),
        }
    }

    if let Some(path) = claude_desktop_config_path() {
        any = true;
        match add_to_claude_desktop(&path, &exe) {
            Ok(true) => println!("  Claude Desktop: added to {}", path.display()),
            Ok(false) => println!("  Claude Desktop: already up to date"),
            Err(e) => println!("  Claude Desktop: {e:#}"),
        }
    }

    println!();
    if any {
        println!("Done. Restart the app to pick up the server.");
    } else {
        println!("No MCP-capable apps detected (Codex CLI or Claude Desktop).");
        print_status();
    }
    Ok(())
}

fn print_status() {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "sidekar".to_string());
    println!("sidekar mcp — Model Context Protocol server (stdio transport).");
    println!();
    println!("For apps that cannot run a shell command (Claude Desktop, Cowork, Codex).");
    println!("`sidekar mcp install` registers it automatically, or add it by hand with:");
    println!("  command: {exe}");
    println!("  args:    [\"mcp\"]");
}

/// `~/Library/Application Support/Claude/claude_desktop_config.json`, but only
/// when the Claude directory exists (i.e. the app is installed).
fn claude_desktop_config_path() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join("Library/Application Support/Claude");
    dir.is_dir()
        .then(|| dir.join("claude_desktop_config.json"))
}

/// Merge the sidekar entry into Claude Desktop's config, preserving every other
/// key. Returns whether anything changed.
fn add_to_claude_desktop(path: &Path, exe: &str) -> Result<bool> {
    let mut root: Value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(path)?)
            .context("existing claude_desktop_config.json is not valid JSON")?
    } else {
        json!({})
    };
    if !upsert_mcp_server(&mut root, "sidekar", exe) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&root)?)?;
    Ok(true)
}

/// Set `mcpServers.<name> = { command, args: ["mcp"] }` in a config object,
/// leaving all other fields untouched. Returns whether it changed anything.
fn upsert_mcp_server(root: &mut Value, name: &str, exe: &str) -> bool {
    if !root.is_object() {
        *root = json!({});
    }
    let obj = root.as_object_mut().expect("root is an object");
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    let servers = servers.as_object_mut().expect("mcpServers is an object");
    let desired = json!({ "command": exe, "args": ["mcp"] });
    if servers.get(name) == Some(&desired) {
        return false;
    }
    servers.insert(name.to_string(), desired);
    true
}
