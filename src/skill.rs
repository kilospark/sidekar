//! Skill file installation for agent CLIs.
//!
//! Installs SKILL.md to the skills directory for each detected agent
//! (Claude Code, Codex, Gemini CLI, OpenCode, Pi).

use std::fs;
use std::path::{Path, PathBuf};

const SKILL_MD: &str = include_str!("../SKILL.md");

/// Return the embedded SKILL.md content.
pub fn skill_text() -> &'static str {
    SKILL_MD
}

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

/// Remove sidekar skill files and data from all known locations.
pub fn remove_skill() {
    println!();
    println!("Removing sidekar...");

    let mut any = false;
    let home = home_dir();

    // --- Skill directories ---
    for subdir in &[
        ".claude/skills/sidekar",
        ".claude/plugins/cache/sidekar",
        ".codex/skills/sidekar",
        ".gemini/skills/sidekar",
        ".pi/skills/sidekar",
    ] {
        let path = home.join(subdir);
        if path.is_dir() && fs::remove_dir_all(&path).is_ok() {
            any = true;
            println!("  Removed {}", path.display());
        }
    }

    let opencode_skill = xdg_config_dir().join("opencode/skills/sidekar");
    if opencode_skill.is_dir() && fs::remove_dir_all(&opencode_skill).is_ok() {
        any = true;
        println!("  Removed {}", opencode_skill.display());
    }

    // --- Data directory ---
    let data_dir = home.join(".sidekar");
    if data_dir.is_dir() && fs::remove_dir_all(&data_dir).is_ok() {
        any = true;
        println!("  Removed {}", data_dir.display());
    }

    println!();
    if any {
        println!("Done! sidekar has been uninstalled.");
    } else {
        println!("  Nothing to uninstall — no sidekar data found.");
    }
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
    if path.exists()
        && let Ok(existing) = fs::read_to_string(&path)
        && existing == SKILL_MD
    {
        println!("  {name}: up to date");
        return;
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
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    home_dir().join(".config")
}
