//! Skill file installation for agent CLIs.
//!
//! Installs SKILL.md to the skills directory for each detected agent
//! (Claude Code, Codex, Gemini CLI, Grok, OpenCode, Pi).

use std::fs;
use std::path::{Path, PathBuf};

const SKILL_MD: &str = include_str!("../SKILL.md");

/// Return the embedded SKILL.md content.
pub fn skill_text() -> &'static str {
    SKILL_MD
}

/// Install sidekar skill file for detected agent CLIs.
///
/// With no `config_hint`, installs under each detected agent's effective config
/// directory (default home or env override such as `CLAUDE_CONFIG_DIR`).
///
/// With a `config_hint`, installs only to `{hint}/skills/sidekar/` — use for
/// alternate agent profiles (e.g. `sidekar install claude-work` → `~/.claude-work/`).
pub fn install_skill(config_hint: Option<&str>) {
    println!();
    println!("Installing sidekar skill...");

    let mut any = false;

    if let Some(hint) = config_hint {
        let root = resolve_config_hint(hint);
        let dir = root.join("skills/sidekar");
        any = true;
        install_skill_to(&dir, &format!("custom ({})", root.display()));
    } else {
        if crate::which_bin("claude").is_some() {
            any = true;
            let dir = claude_config_dir().join("skills/sidekar");
            install_skill_to(&dir, "Claude Code");
        }

        if crate::which_bin("codex").is_some() {
            any = true;
            let dir = codex_config_dir().join("skills/sidekar");
            install_skill_to(&dir, "Codex");
        }

        if crate::which_bin("gemini").is_some() {
            any = true;
            let dir = gemini_config_dir().join("skills/sidekar");
            install_skill_to(&dir, "Gemini CLI");
        }

        if crate::which_bin("grok").is_some() {
            any = true;
            let dir = grok_config_dir().join("skills/sidekar");
            install_skill_to(&dir, "Grok");
        }

        if crate::which_bin("opencode").is_some() {
            any = true;
            let dir = opencode_config_dir().join("skills/sidekar");
            install_skill_to(&dir, "OpenCode");
        }

        if crate::which_bin("pi").is_some() {
            any = true;
            let dir = pi_config_dir().join("skills/sidekar");
            install_skill_to(&dir, "Pi");
        }
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
        println!("  For Grok:         ~/.grok/skills/sidekar/SKILL.md");
        println!();
        println!(
            "  Alternate profile: sidekar install <folder>  (e.g. claude-work → ~/.claude-work/)"
        );
    }
}

/// Skill search roots used by the REPL `/skill` command — same dirs as install.
pub fn skill_search_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        claude_config_dir().join("skills"),
        codex_config_dir().join("skills"),
        gemini_config_dir().join("skills"),
        grok_config_dir().join("skills"),
        opencode_config_dir().join("skills"),
        pi_config_dir().join("skills"),
    ];
    roots.sort();
    roots.dedup();
    roots
}

/// Remove sidekar skill files and data from all known locations.
pub fn remove_skill() {
    println!();
    println!("Removing sidekar...");

    let mut any = false;

    for subdir in skill_search_roots()
        .into_iter()
        .map(|root| root.join("sidekar"))
        .chain([home_dir().join(".claude/plugins/cache/sidekar")])
    {
        if subdir.is_dir() && fs::remove_dir_all(&subdir).is_ok() {
            any = true;
            println!("  Removed {}", subdir.display());
        }
    }

    // --- Data directory ---
    let data_dir = home_dir().join(".sidekar");
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

fn claude_config_dir() -> PathBuf {
    env_config_dir("CLAUDE_CONFIG_DIR", home_dir().join(".claude"))
}

fn codex_config_dir() -> PathBuf {
    env_config_dir("CODEX_HOME", home_dir().join(".codex"))
}

fn gemini_config_dir() -> PathBuf {
    env_config_dir("GEMINI_CONFIG_DIR", home_dir().join(".gemini"))
}

fn grok_config_dir() -> PathBuf {
    env_config_dir("GROK_HOME", home_dir().join(".grok"))
}

fn opencode_config_dir() -> PathBuf {
    xdg_config_dir().join("opencode")
}

fn pi_config_dir() -> PathBuf {
    env_config_dir("PI_HOME", home_dir().join(".pi"))
}

fn env_config_dir(var: &str, default: PathBuf) -> PathBuf {
    std::env::var(var)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .map(expand_tilde_path)
        .unwrap_or(default)
}

/// Resolve a user-supplied config folder hint to an agent config root.
fn resolve_config_hint(hint: &str) -> PathBuf {
    expand_tilde_path(PathBuf::from(hint.trim()))
}

fn expand_tilde_path(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        home_dir()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home_dir().join(rest)
    } else if path.is_absolute() {
        path
    } else if s.starts_with('.') {
        home_dir().join(path)
    } else if s.contains('/') {
        home_dir().join(path)
    } else {
        home_dir().join(format!(".{s}"))
    }
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
        return expand_tilde_path(PathBuf::from(dir));
    }
    home_dir().join(".config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_path_folder_name_gets_dot_prefix() {
        let home = home_dir();
        assert_eq!(
            expand_tilde_path(PathBuf::from("claude-work")),
            home.join(".claude-work")
        );
    }

    #[test]
    fn expand_tilde_path_dot_folder_under_home() {
        let home = home_dir();
        assert_eq!(
            expand_tilde_path(PathBuf::from(".claude-work")),
            home.join(".claude-work")
        );
    }

    #[test]
    fn expand_tilde_path_absolute_unchanged() {
        assert_eq!(
            expand_tilde_path(PathBuf::from("/tmp/agent-config")),
            PathBuf::from("/tmp/agent-config")
        );
    }

    #[test]
    fn expand_tilde_path_tilde_slash() {
        let home = home_dir();
        assert_eq!(
            expand_tilde_path(PathBuf::from("~/profiles/work")),
            home.join("profiles/work")
        );
    }

    #[test]
    fn skill_search_roots_includes_grok_default() {
        let roots = skill_search_roots();
        assert!(roots.contains(&home_dir().join(".grok/skills")));
    }
}
