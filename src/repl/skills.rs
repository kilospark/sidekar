//! Skill loading for the REPL `/skill` slash command.
//!
//! Skills are Anthropic-style agent skills: a directory containing `SKILL.md`.
//! We look in the same per-agent skill dirs that `sidekar install` writes to,
//! so any skill already installed for Claude Code, Codex, Gemini, pi, or
//! OpenCode is instantly loadable here.

use std::path::PathBuf;

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

fn xdg_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|h| h.join(".config"))
}

fn skill_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".claude/skills"));
        roots.push(home.join(".codex/skills"));
        roots.push(home.join(".gemini/skills"));
        roots.push(home.join(".pi/skills"));
    }
    if let Some(cfg) = xdg_config_dir() {
        roots.push(cfg.join("opencode/skills"));
    }
    roots
}

/// Resolve a skill name to `<root>/<name>/SKILL.md` in the first root that has it.
pub(super) fn find_skill(name: &str) -> Option<PathBuf> {
    for root in skill_roots() {
        let candidate = root.join(name).join("SKILL.md");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Return sorted unique skill names found across all roots.
pub(super) fn list_skills() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for root in skill_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("SKILL.md").is_file()
                && let Some(name) = path.file_name().and_then(|s| s.to_str())
            {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}
