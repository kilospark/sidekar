use crate::*;
use regex::Regex;
use std::sync::LazyLock;

mod rules;

pub use rules::Classification;
use rules::*;

static ENV_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:sudo\s+|env\s+|[A-Z_][A-Z0-9_]*=[^\s]*\s+)+").expect("invalid env regex")
});
static GIT_GLOBAL_OPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:(?:-C\s+\S+|-c\s+\S+|--git-dir(?:=\S+|\s+\S+)|--work-tree(?:=\S+|\s+\S+)|--no-pager)\s+)+",
    )
    .expect("invalid git option regex")
});
static ANSI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("invalid ansi regex"));

pub fn classify_command(command: &str) -> Classification {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Classification::Ignored;
    }

    let stripped = ENV_PREFIX.replace(trimmed, "");
    let mut normalized = stripped.trim().to_string();
    if let Some(rest) = normalized.strip_prefix("git ") {
        let rest = GIT_GLOBAL_OPT.replace(rest, "");
        normalized = format!("git {}", rest.trim());
    }

    for (idx, regex) in REWRITE_REGEXES.iter().enumerate() {
        if regex.is_match(&normalized) {
            let rule = &REWRITE_RULES[idx];
            return Classification::Supported {
                equivalent: rule.equivalent,
                category: rule.category,
                estimated_savings_pct: rule.estimated_savings_pct,
            };
        }
    }

    let base_command = normalized
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    if base_command.is_empty() {
        Classification::Ignored
    } else {
        Classification::Unsupported { base_command }
    }
}

pub fn compact_output(command: &str, output: &str) -> String {
    let normalized = command.trim();
    let mut current = output.to_string();

    if let Some(filter) = FILTERS
        .iter()
        .find(|filter| filter.pattern.is_match(normalized))
    {
        if filter.strip_ansi {
            current = ANSI_RE.replace_all(&current, "").to_string();
        }

        let mut lines: Vec<String> = current.lines().map(|line| line.to_string()).collect();
        if !filter.keep_lines_matching.is_empty() {
            lines.retain(|line| {
                filter
                    .keep_lines_matching
                    .iter()
                    .any(|regex| regex.is_match(line))
            });
        }
        if !filter.strip_lines_matching.is_empty() {
            lines.retain(|line| {
                !filter
                    .strip_lines_matching
                    .iter()
                    .any(|regex| regex.is_match(line))
            });
        }
        if filter.dedupe_repeats {
            lines = dedupe_repeated_lines(lines);
        }
        lines = normalize_blank_lines(lines);
        if let Some(max_lines) = filter.max_lines {
            let total = lines.len();
            if total > max_lines {
                let head = max_lines / 2;
                let tail = max_lines - head;
                let omitted = total - head - tail;
                let mut truncated = Vec::with_capacity(max_lines + 1);
                truncated.extend_from_slice(&lines[..head]);
                truncated.push(format!("... ({omitted} lines omitted) ..."));
                truncated.extend_from_slice(&lines[total - tail..]);
                lines = truncated;
            }
        }
        if lines.is_empty() {
            return filter.on_empty.unwrap_or_default().to_string();
        }
        return lines.join("\n");
    }

    generic_compact(output)
}

/// Max lines for unrecognized commands. Budget is split 50/50 between
/// head (first lines) and tail (last lines), with an omission marker
/// in between — matching the Codex middle-truncation approach.
const GENERIC_MAX_LINES: usize = 200;

fn generic_compact(output: &str) -> String {
    let stripped = ANSI_RE.replace_all(output, "").to_string();
    let lines: Vec<String> = stripped
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect();
    let lines = normalize_blank_lines(dedupe_repeated_lines(lines));
    let total = lines.len();
    if total <= GENERIC_MAX_LINES {
        return lines.join("\n");
    }
    let head = GENERIC_MAX_LINES / 2;
    let tail = GENERIC_MAX_LINES - head;
    let omitted = total - head - tail;
    let mut result = Vec::with_capacity(GENERIC_MAX_LINES + 1);
    result.extend_from_slice(&lines[..head]);
    result.push(format!("... ({omitted} lines omitted) ..."));
    result.extend_from_slice(&lines[total - tail..]);
    result.join("\n")
}

fn normalize_blank_lines(lines: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut last_blank = true;
    for line in lines {
        let is_blank = line.trim().is_empty();
        if is_blank {
            if last_blank {
                continue;
            }
            normalized.push(String::new());
        } else {
            normalized.push(line);
        }
        last_blank = is_blank;
    }
    while normalized.last().is_some_and(|line| line.is_empty()) {
        normalized.pop();
    }
    normalized
}

fn dedupe_repeated_lines(lines: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    let mut iter = lines.into_iter().peekable();
    while let Some(line) = iter.next() {
        let mut count = 1usize;
        while iter.peek().is_some_and(|next| next == &line) {
            let _ = iter.next();
            count += 1;
        }
        if count > 1 && !line.trim().is_empty() {
            result.push(format!("{line} [x{count}]"));
        } else {
            result.push(line);
        }
    }
    result
}

fn read_stdin() -> Result<String> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read stdin")?;
    Ok(input)
}

pub fn cmd_compact(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "classify" => {
            let command = args.get(1..).unwrap_or(&[]).join(" ");
            if command.trim().is_empty() {
                bail!("Usage: sidekar compact classify <command...>");
            }
            match classify_command(&command) {
                Classification::Supported {
                    equivalent,
                    category,
                    estimated_savings_pct,
                } => {
                    out!(ctx, "supported");
                    out!(ctx, "command: {command}");
                    out!(ctx, "equivalent: {equivalent}");
                    out!(ctx, "category: {category}");
                    out!(ctx, "estimated savings: {estimated_savings_pct}%");
                }
                Classification::Unsupported { base_command } => {
                    out!(ctx, "unsupported");
                    out!(ctx, "command: {command}");
                    out!(ctx, "base command: {base_command}");
                }
                Classification::Ignored => {
                    out!(ctx, "ignored");
                    out!(ctx, "command: {command}");
                }
            }
            Ok(())
        }
        "filter" => {
            let command = args.get(1..).unwrap_or(&[]).join(" ");
            if command.trim().is_empty() {
                bail!("Usage: sidekar compact filter <command...> < <output.txt>");
            }
            let input = read_stdin()?;
            let compacted = compact_output(&command, &input);
            out!(ctx, "{compacted}");
            Ok(())
        }
        "run" => {
            if args.len() < 2 {
                bail!("Usage: sidekar compact run <command> [args...]");
            }
            let command = &args[1];
            let child_args = &args[2..];
            let rendered = std::iter::once(command.as_str())
                .chain(child_args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            let output = Command::new(command)
                .args(child_args)
                .output()
                .with_context(|| format!("failed to run {rendered}"))?;
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            let compacted = compact_output(&rendered, &combined);
            if !compacted.is_empty() {
                out!(ctx, "{compacted}");
            }
            if !output.status.success() {
                if let Some(code) = output.status.code() {
                    out!(ctx, "[exit {code}]");
                } else {
                    out!(ctx, "[process terminated by signal]");
                }
            }
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar compact <classify|filter|run> ...\n\
             Examples:\n\
               sidekar compact classify git status\n\
               cargo test 2>&1 | sidekar compact filter cargo test\n\
               sidekar compact run cargo test"
        ),
    }
}

#[cfg(test)]
mod tests;
