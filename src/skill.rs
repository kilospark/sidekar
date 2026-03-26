//! Skill file installation for agent CLIs.
//!
//! Installs SKILL.md to the skills directory for each detected agent
//! (Claude Code, Codex, Gemini CLI, OpenCode, Pi).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use serde_json;

const SKILL_MD: &str = include_str!("../SKILL.md");

/// Install sidekar skill file for all detected agent CLIs.
pub fn install_skill() {
    println!();
    println!("Installing sidekar skill...");

    let mut any = false;

    if crate::which_bin("claude").is_some() {
        any = true;
        let dir = home_dir().join(".claude/skills/sidekar");
        install_skill_to(&dir, "Claude Code");
    }

    if crate::which_bin("codex").is_some() {
        any = true;
        let dir = home_dir().join(".codex/skills/sidekar");
        install_skill_to(&dir, "Codex");
    }

    if crate::which_bin("gemini").is_some() {
        any = true;
        let dir = home_dir().join(".gemini/skills/sidekar");
        install_skill_to(&dir, "Gemini CLI");
    }

    if crate::which_bin("opencode").is_some() {
        any = true;
        let dir = xdg_config_dir().join("opencode/skills/sidekar");
        install_skill_to(&dir, "OpenCode");
    }

    if crate::which_bin("pi").is_some() {
        any = true;
        let dir = home_dir().join(".pi/skills/sidekar");
        install_skill_to(&dir, "Pi");
    }

    println!();
    if any {
        println!("Done! The sidekar skill is now available in your agent.");
    } else {
        println!("  No supported agents detected.");
        println!("  Manually copy SKILL.md to your agent's skills directory.");
        println!();
        println!("  For Claude Code:  ~/.claude/skills/sidekar/SKILL.md");
        println!("  For Codex:        ~/.codex/skills/sidekar/SKILL.md");
    }
}

/// Remove sidekar skill files, legacy MCP configs, and data from all known locations.
pub fn remove_skill() {
    println!();
    println!("Removing sidekar...");

    let mut any = false;
    let home = home_dir();
    let is_macos = cfg!(target_os = "macos");

    // --- Skill directories ---
    for subdir in &[
        ".claude/skills/sidekar",
        ".claude/plugins/cache/sidekar",
        ".codex/skills/sidekar",
        ".gemini/skills/sidekar",
        ".pi/skills/sidekar",
        ".agents/skills/sidekar", // legacy
    ] {
        let path = home.join(subdir);
        if path.is_dir() {
            if fs::remove_dir_all(&path).is_ok() {
                any = true;
                println!("  Removed {}", path.display());
            }
        }
    }

    let opencode_skill = xdg_config_dir().join("opencode/skills/sidekar");
    if opencode_skill.is_dir() {
        if fs::remove_dir_all(&opencode_skill).is_ok() {
            any = true;
            println!("  Removed {}", opencode_skill.display());
        }
    }

    // --- Legacy MCP client configurations ---

    // CLI-based clients
    if crate::which_bin("claude").is_some() && run_silent(&["claude", "mcp", "get", "sidekar"]) {
        if run_silent(&["claude", "mcp", "remove", "-s", "user", "sidekar"]) {
            any = true;
            println!("  Claude Code MCP: removed");
        }
    }
    if crate::which_bin("codex").is_some() && run_grep(&["codex", "mcp", "list"], "sidekar") {
        if run_silent(&["codex", "mcp", "remove", "sidekar"]) {
            any = true;
            println!("  Codex MCP: removed");
        }
    }
    if crate::which_bin("gemini").is_some() && run_grep(&["gemini", "mcp", "list"], "sidekar") {
        if run_silent(&["gemini", "mcp", "remove", "-s", "user", "sidekar"]) {
            any = true;
            println!("  Gemini CLI MCP: removed");
        }
    }

    // JSON config file clients (Claude Desktop, Cursor, Windsurf, ChatGPT, Cline, Copilot)
    let mut config_files: Vec<(&str, PathBuf)> = Vec::new();

    if is_macos {
        config_files.push((
            "Claude Desktop",
            home.join("Library/Application Support/Claude/claude_desktop_config.json"),
        ));
        config_files.push((
            "ChatGPT Desktop",
            home.join("Library/Application Support/ChatGPT/mcp.json"),
        ));
        config_files.push((
            "Cline (VSCode)",
            home.join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json"),
        ));
        config_files.push((
            "Cline (Cursor)",
            home.join("Library/Application Support/Cursor/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json"),
        ));
    } else {
        config_files.push((
            "Claude Desktop",
            xdg_config_dir().join("Claude/claude_desktop_config.json"),
        ));
        config_files.push((
            "ChatGPT Desktop",
            xdg_config_dir().join("chatgpt/mcp.json"),
        ));
        config_files.push((
            "Cline (VSCode)",
            xdg_config_dir().join("Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json"),
        ));
        config_files.push((
            "Cline (Cursor)",
            xdg_config_dir().join("Cursor/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json"),
        ));
    }
    config_files.push(("Cursor", home.join(".cursor/mcp.json")));
    config_files.push(("Windsurf", home.join(".codeium/windsurf/mcp_config.json")));
    config_files.push(("Copilot CLI", home.join(".copilot/mcp-config.json")));

    for (name, path) in &config_files {
        if let Some(msg) = remove_mcp_from_json(path) {
            any = true;
            println!("  {name} MCP: {msg}");
        }
    }

    // OpenCode MCP config
    let opencode_config = xdg_config_dir().join("opencode/config.json");
    if let Some(msg) = remove_mcp_from_json(&opencode_config) {
        any = true;
        println!("  OpenCode MCP: {msg}");
    }

    // --- Data directory ---
    let data_dir = home.join(".sidekar");
    if data_dir.is_dir() {
        if fs::remove_dir_all(&data_dir).is_ok() {
            any = true;
            println!("  Removed {}", data_dir.display());
        }
    }

    println!();
    if any {
        println!("Done! sidekar has been uninstalled.");
    } else {
        println!("  Nothing to uninstall — no sidekar data found.");
    }
}

/// Remove "sidekar" entry from an mcpServers or mcp JSON config file.
/// Returns Some(status) if modified, None if file doesn't exist or has no sidekar entry.
fn remove_mcp_from_json(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    if !raw.contains("\"sidekar\"") {
        return None;
    }

    let mut data: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let mut removed = false;

    if let Some(obj) = data.as_object_mut() {
        // Check both "mcpServers" and "mcp" keys
        for key in &["mcpServers", "mcp"] {
            if let Some(section) = obj.get_mut(*key) {
                if let Some(m) = section.as_object_mut() {
                    if m.remove("sidekar").is_some() {
                        removed = true;
                    }
                }
            }
        }
    }

    if !removed {
        return None;
    }

    let serialized = serde_json::to_string_pretty(&data).ok()?;
    fs::write(path, format!("{serialized}\n")).ok()?;
    Some("removed".into())
}

fn run_silent(args: &[&str]) -> bool {
    if args.is_empty() {
        return false;
    }
    Command::new(args[0])
        .args(&args[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_grep(args: &[&str], pattern: &str) -> bool {
    if args.is_empty() {
        return false;
    }
    Command::new(args[0])
        .args(&args[1..])
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(pattern))
        .unwrap_or(false)
}

/// Print the embedded SKILL.md to stdout (for agents to read).
pub fn print_skill() {
    print!("{SKILL_MD}");
}

fn install_skill_to(dir: &Path, name: &str) {
    if let Err(e) = fs::create_dir_all(dir) {
        println!("  {name}: failed to create directory: {e}");
        return;
    }
    let path = dir.join("SKILL.md");
    if path.exists() {
        if let Ok(existing) = fs::read_to_string(&path) {
            if existing == SKILL_MD {
                println!("  {name}: up to date");
                return;
            }
        }
    }
    match fs::write(&path, SKILL_MD) {
        Ok(()) => println!("  {name}: installed → {}", path.display()),
        Err(e) => println!("  {name}: failed to write: {e}"),
    }
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn xdg_config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home_dir().join(".config")
}

