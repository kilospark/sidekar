use crate::AppContext;
use crate::prompts;
use anyhow::{Result, bail};

#[derive(serde::Serialize)]
struct PromptEntryOut {
    key: String,
    description: String,
    chars: usize,
    edited: bool,
    drifted: bool,
}

#[derive(serde::Serialize)]
struct PromptListOutput {
    items: Vec<PromptEntryOut>,
}

impl crate::output::CommandOutput for PromptListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        let max_key = self.items.iter().map(|e| e.key.len()).max().unwrap_or(0);
        for entry in &self.items {
            let status = match (entry.edited, entry.drifted) {
                (false, _) => "default",
                (true, false) => "edited",
                (true, true) => "edited (default has changed)",
            };
            writeln!(
                w,
                "{:<width$}  {:>6} chars  {}",
                entry.key,
                entry.chars,
                status,
                width = max_key
            )?;
            writeln!(
                w,
                "{:<width$}  # {}",
                "",
                entry.description,
                width = max_key
            )?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct PromptGetOutput {
    key: String,
    value: String,
    edited: bool,
    drifted: bool,
}

impl crate::output::CommandOutput for PromptGetOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "{}", self.value)
    }
}

#[derive(serde::Serialize)]
struct PromptDiffOutput {
    key: String,
    differs: bool,
    stored: String,
    default: String,
}

impl crate::output::CommandOutput for PromptDiffOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if !self.differs {
            return writeln!(w, "{} matches the shipped default.", self.key);
        }
        for line in diff_lines(&self.default, &self.stored) {
            writeln!(w, "{line}")?;
        }
        Ok(())
    }
}

/// Line-level diff. Not a real LCS: prompts are edited as whole
/// paragraphs, so a set-membership comparison reads better than a
/// character-accurate patch and needs no dependency.
fn diff_lines(default: &str, stored: &str) -> Vec<String> {
    let default_lines: Vec<&str> = default.lines().collect();
    let stored_lines: Vec<&str> = stored.lines().collect();
    let mut out = Vec::new();
    for line in &default_lines {
        if !stored_lines.contains(line) {
            out.push(format!("- {line}"));
        }
    }
    for line in &stored_lines {
        if !default_lines.contains(line) {
            out.push(format!("+ {line}"));
        }
    }
    out
}

fn ensure_valid_prompt_key(key: &str) -> Result<()> {
    if prompts::builtin(key).is_some() {
        return Ok(());
    }
    let valid: Vec<&str> = prompts::BUILTIN_PROMPTS.iter().map(|p| p.key).collect();
    bail!("Unknown prompt \"{key}\". Valid keys: {}", valid.join(", "))
}

/// Resolve the replacement text for `set`, in precedence order:
/// `--file=path`, remaining positional args, then stdin when piped.
fn read_new_value(args: &[String]) -> Result<String> {
    if let Some(path) = args.iter().find_map(|a| a.strip_prefix("--file=")) {
        return std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read --file={path}: {e}"));
    }
    let positional: Vec<&str> = args
        .iter()
        .skip(2)
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    if !positional.is_empty() {
        return Ok(positional.join(" "));
    }
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        bail!("Usage: sidekar prompt set <key> <text|--file=path>  (or pipe the text on stdin)");
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        bail!("Refusing to store an empty prompt. Use `sidekar prompt reset <key>` instead.");
    }
    Ok(buf)
}

fn edit_in_editor(current: &str, key: &str) -> Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let path = std::env::temp_dir().join(format!(
        "sidekar-prompt-{}-{}.md",
        key.replace('.', "-"),
        std::process::id()
    ));
    std::fs::write(&path, current)?;
    let status = std::process::Command::new(&editor).arg(&path).status();
    let edited = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => bail!("{editor} exited with status {s}"),
        Err(e) => bail!("failed to launch {editor}: {e}"),
    }
    Ok(edited?)
}

pub(super) fn cmd_prompt(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" | "ls" => {
            let items = prompts::list()
                .into_iter()
                .map(|(builtin, row)| {
                    let edited = row.as_ref().map(|r| r.edited).unwrap_or(false);
                    let drifted = row.as_ref().map(prompts::is_drifted).unwrap_or(false);
                    let chars = row
                        .as_ref()
                        .map(|r| r.value.chars().count())
                        .unwrap_or_else(|| builtin.default.chars().count());
                    PromptEntryOut {
                        key: builtin.key.to_string(),
                        description: builtin.description.to_string(),
                        chars,
                        edited,
                        drifted,
                    }
                })
                .collect();
            out!(
                ctx,
                "{}",
                crate::output::to_string(&PromptListOutput { items })?
            );
            Ok(())
        }
        "get" | "show" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                bail!("Usage: sidekar prompt get <key>");
            }
            ensure_valid_prompt_key(key)?;
            let row = crate::broker::prompt_get(key).ok().flatten();
            let output = PromptGetOutput {
                key: key.to_string(),
                value: prompts::get(key),
                edited: row.as_ref().map(|r| r.edited).unwrap_or(false),
                drifted: row.as_ref().map(prompts::is_drifted).unwrap_or(false),
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
            Ok(())
        }
        "set" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                bail!("Usage: sidekar prompt set <key> <text|--file=path>");
            }
            ensure_valid_prompt_key(key)?;
            let value = read_new_value(args)?;
            if value.trim().is_empty() {
                bail!(
                    "Refusing to store an empty prompt. Use `sidekar prompt reset <key>` instead."
                );
            }
            prompts::set(key, &value)?;
            let msg = format!("Updated \"{key}\" ({} chars).", value.chars().count());
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::output::PlainOutput::new(msg))?
            );
            Ok(())
        }
        "edit" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                bail!("Usage: sidekar prompt edit <key>");
            }
            ensure_valid_prompt_key(key)?;
            let current = prompts::get(key);
            let edited = edit_in_editor(&current, key)?;
            if edited == current {
                out!(
                    ctx,
                    "{}",
                    crate::output::to_string(&crate::output::PlainOutput::new(format!(
                        "No change to \"{key}\"."
                    )))?
                );
                return Ok(());
            }
            if edited.trim().is_empty() {
                bail!(
                    "Refusing to store an empty prompt. Use `sidekar prompt reset <key>` instead."
                );
            }
            prompts::set(key, &edited)?;
            let msg = format!("Updated \"{key}\" ({} chars).", edited.chars().count());
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::output::PlainOutput::new(msg))?
            );
            Ok(())
        }
        "reset" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                bail!("Usage: sidekar prompt reset <key>");
            }
            ensure_valid_prompt_key(key)?;
            prompts::reset(key)?;
            let msg = format!("Reset \"{key}\" to the shipped default.");
            out!(
                ctx,
                "{}",
                crate::output::to_string(&crate::output::PlainOutput::new(msg))?
            );
            Ok(())
        }
        "diff" => {
            let key = args.get(1).map(String::as_str).unwrap_or("");
            if key.is_empty() {
                bail!("Usage: sidekar prompt diff <key>");
            }
            ensure_valid_prompt_key(key)?;
            let stored = prompts::get(key);
            let default = prompts::default_for(key).to_string();
            let output = PromptDiffOutput {
                key: key.to_string(),
                differs: stored.trim_end() != default.trim_end(),
                stored,
                default,
            };
            out!(ctx, "{}", crate::output::to_string(&output)?);
            Ok(())
        }
        other => bail!(
            "Unknown subcommand \"{other}\". Usage: sidekar prompt <list|get|set|edit|reset|diff> [key] [text]"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn known_keys_validate_and_unknown_keys_list_the_valid_set() {
        assert!(ensure_valid_prompt_key(prompts::KEY_PTY_STARTER).is_ok());
        let err = ensure_valid_prompt_key("nope").unwrap_err().to_string();
        assert!(err.contains("Unknown prompt \"nope\""));
        assert!(err.contains(prompts::KEY_REPL_SYSTEM));
    }

    #[test]
    fn set_reads_positional_text_joined_by_spaces() {
        let a = args(&["set", "pty.starter", "hello", "world"]);
        assert_eq!(read_new_value(&a).unwrap(), "hello world");
    }

    #[test]
    fn set_prefers_file_over_positional_text() {
        let path = std::env::temp_dir().join(format!("sidekar-prompt-test-{}", std::process::id()));
        std::fs::write(&path, "from file").unwrap();
        let a = args(&[
            "set",
            "pty.starter",
            "ignored",
            &format!("--file={}", path.display()),
        ]);
        assert_eq!(read_new_value(&a).unwrap(), "from file");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn diff_reports_added_and_removed_lines() {
        let out = diff_lines("one\ntwo\n", "one\nthree\n");
        assert_eq!(out, vec!["- two".to_string(), "+ three".to_string()]);
        assert!(diff_lines("same\n", "same\n").is_empty());
    }
}
