use crate::*;

pub fn print_command_help(command: &str) {
    if let Some(replacement) = removed_command_replacement(command) {
        println!("Command '{command}' was removed.\n\nUse: sidekar {replacement}");
        return;
    }

    let command = canonical_command_name(command).unwrap_or(command);
    let help = command_help_text(command).or_else(|| command_spec_fallback(command));
    let Some(help) = help else {
        println!("Unknown command: {command}\n\nRun 'sidekar help' for a list of all commands.");
        return;
    };
    let text = colorize_command_help(&help);
    println!("{}", crate::runtime::maybe_strip_ansi(&text));
}

fn command_spec_fallback(command: &str) -> Option<String> {
    let spec = crate::command_catalog::command_spec(command)?;
    let usage = if spec.usage.is_empty() {
        format!("sidekar {}", spec.name)
    } else {
        format!("sidekar {} {}", spec.name, spec.usage)
    };
    Some(format!("{usage}\n\n  {}", spec.summary))
}

fn command_help_text(command: &str) -> Option<String> {
    crate::help_text::command_help_text(command).map(str::to_owned)
}

fn colorize_command_help(help: &str) -> String {
    const BOLD: &str = "\x1b[1m";
    const DIM: &str = "\x1b[2m";
    const CYAN: &str = "\x1b[36m";
    const GREEN: &str = "\x1b[32m";
    const YELLOW: &str = "\x1b[33m";
    const RST: &str = "\x1b[0m";

    let mut out = String::new();
    let mut in_examples = false;

    for (i, line) in help.lines().enumerate() {
        if i == 0 {
            if let Some(rest) = line.strip_prefix("sidekar ") {
                let (cmd, args) = match rest.find(|c: char| c == ' ' || c == '<' || c == '[') {
                    Some(pos) => (&rest[..pos], &rest[pos..]),
                    None => (rest, ""),
                };
                out.push_str(&format!("{BOLD}sidekar {cmd}{RST}{DIM}{args}{RST}\n"));
            } else {
                out.push_str(&format!("{BOLD}{line}{RST}\n"));
            }
            continue;
        }

        let trimmed = line.trim();

        if trimmed.ends_with(':')
            && !trimmed.starts_with("sidekar")
            && !trimmed.starts_with("--")
            && !trimmed.starts_with('-')
            && !trimmed.contains("  ")
        {
            in_examples = trimmed == "Examples:" || trimmed == "Example:";
            out.push_str(&format!(
                "{}{YELLOW}{BOLD}{trimmed}{RST}\n",
                &line[..line.len() - trimmed.len()]
            ));
            continue;
        }

        if in_examples && trimmed.starts_with("sidekar ") {
            out.push_str(&format!(
                "{}{CYAN}{trimmed}{RST}\n",
                &line[..line.len() - trimmed.len()]
            ));
            continue;
        }

        if trimmed.starts_with("Example: sidekar ") || trimmed.starts_with("Example:  sidekar ") {
            let rest = trimmed.strip_prefix("Example:").unwrap().trim();
            out.push_str(&format!(
                "{}{YELLOW}{BOLD}Example:{RST} {CYAN}{rest}{RST}\n",
                &line[..line.len() - trimmed.len()]
            ));
            continue;
        }

        if trimmed.starts_with("--")
            || (trimmed.starts_with('-')
                && trimmed.len() > 1
                && trimmed.as_bytes()[1].is_ascii_alphabetic())
        {
            if let Some(pos) = trimmed.find("  ") {
                let flag = &trimmed[..pos];
                let desc = trimmed[pos..].trim();
                out.push_str(&format!(
                    "{}{GREEN}{flag}{RST}  {DIM}{desc}{RST}\n",
                    &line[..line.len() - trimmed.len()]
                ));
            } else {
                out.push_str(&format!(
                    "{}{GREEN}{trimmed}{RST}\n",
                    &line[..line.len() - trimmed.len()]
                ));
            }
            continue;
        }

        if !trimmed.is_empty() && !trimmed.starts_with("sidekar") && !in_examples {
            if let Some(pos) = trimmed.find("  ") {
                let left = &trimmed[..pos];
                let right = trimmed[pos..].trim();
                if !left.is_empty() && left.len() < 40 && !left.contains('.') && !right.is_empty() {
                    out.push_str(&format!(
                        "{}{CYAN}{left}{RST}  {DIM}{right}{RST}\n",
                        &line[..line.len() - trimmed.len()]
                    ));
                    continue;
                }
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    if out.ends_with('\n') {
        out.pop();
    }
    out
}

pub fn print_help() {
    let text = crate::cli::render_help(env!("CARGO_PKG_VERSION"));
    println!("{}", crate::runtime::maybe_strip_ansi(&text));
}

#[cfg(test)]
mod tests;
