use crate::*;
use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    Supported {
        equivalent: &'static str,
        category: &'static str,
        estimated_savings_pct: u8,
    },
    Unsupported {
        base_command: String,
    },
    Ignored,
}

struct RewriteRule {
    pattern: &'static str,
    equivalent: &'static str,
    category: &'static str,
    estimated_savings_pct: u8,
}

struct OutputFilterDef {
    pattern: &'static str,
    strip_ansi: bool,
    strip_lines_matching: &'static [&'static str],
    keep_lines_matching: &'static [&'static str],
    max_lines: Option<usize>,
    on_empty: Option<&'static str>,
    dedupe_repeats: bool,
}

struct OutputFilter {
    pattern: Regex,
    strip_ansi: bool,
    strip_lines_matching: Vec<Regex>,
    keep_lines_matching: Vec<Regex>,
    max_lines: Option<usize>,
    on_empty: Option<&'static str>,
    dedupe_repeats: bool,
}

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

const REWRITE_RULES: &[RewriteRule] = &[
    RewriteRule {
        pattern: r"^git\s+(?:-[Cc]\s+\S+\s+)*(status|log|diff|show|add|commit|push|pull)",
        equivalent: "compact git",
        category: "Git",
        estimated_savings_pct: 75,
    },
    RewriteRule {
        pattern: r"^cargo\s+(build|check|clippy|test)",
        equivalent: "compact cargo",
        category: "Cargo",
        estimated_savings_pct: 85,
    },
    RewriteRule {
        pattern: r"^(python\s+-m\s+)?pytest(\s|$)",
        equivalent: "compact pytest",
        category: "Tests",
        estimated_savings_pct: 90,
    },
    RewriteRule {
        pattern: r"^(pnpm\s+|npm\s+(run\s+)?)test(\s|$)|^(npx\s+|pnpm\s+)?(vitest|jest)(\s|$)",
        equivalent: "compact test",
        category: "Tests",
        estimated_savings_pct: 90,
    },
    RewriteRule {
        pattern: r"^(cat|head|tail)\s+",
        equivalent: "compact read",
        category: "Files",
        estimated_savings_pct: 60,
    },
    RewriteRule {
        pattern: r"^(rg|grep)\s+",
        equivalent: "compact grep",
        category: "Files",
        estimated_savings_pct: 75,
    },
    RewriteRule {
        pattern: r"^ls(\s|$)|^find\s+",
        equivalent: "compact files",
        category: "Files",
        estimated_savings_pct: 65,
    },
    RewriteRule {
        pattern: r"^docker\s+(ps|logs|compose\s+logs)",
        equivalent: "compact docker",
        category: "Infra",
        estimated_savings_pct: 80,
    },
    RewriteRule {
        pattern: r"^kubectl\s+(get|logs|describe)",
        equivalent: "compact kubectl",
        category: "Infra",
        estimated_savings_pct: 80,
    },
    RewriteRule {
        pattern: r"^curl\s+",
        equivalent: "compact curl",
        category: "Network",
        estimated_savings_pct: 60,
    },
];

static REWRITE_REGEXES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    REWRITE_RULES
        .iter()
        .map(|rule| Regex::new(rule.pattern).expect("invalid compact rule regex"))
        .collect()
});

const FILTER_DEFS: &[OutputFilterDef] = &[
    OutputFilterDef {
        pattern: r"^git\s+(?:-[Cc]\s+\S+\s+)*status\b",
        strip_ansi: true,
        strip_lines_matching: &[
            r"^On branch ",
            r"^Your branch is ",
            r"^nothing to commit, working tree clean$",
            r"^Changes to be committed:$",
            r"^Changes not staged for commit:$",
            r"^Untracked files:$",
            r"^\s+\(use ",
            r"^no changes added to commit",
        ],
        keep_lines_matching: &[
            r"^\s*(modified:|deleted:|new file:|renamed:|both modified:|both added:|\?\?)",
            r"^\s+\S.*$",
        ],
        max_lines: Some(40),
        on_empty: Some("git status: clean"),
        dedupe_repeats: false,
    },
    OutputFilterDef {
        pattern: r"^cargo\s+(build|check|clippy|test)\b",
        strip_ansi: true,
        strip_lines_matching: &[
            r"^Compiling ",
            r"^Checking ",
            r"^Finished ",
            r"^Running ",
            r"^Blocking waiting for file lock",
        ],
        keep_lines_matching: &[
            r"^error(\[.+\])?:",
            r"^warning:",
            r"^test result:",
            r"^failures:",
            r"^---- ",
            r"^FAILED$",
            r"^error: test failed",
        ],
        max_lines: Some(60),
        on_empty: Some("cargo: ok"),
        dedupe_repeats: false,
    },
    OutputFilterDef {
        pattern: r"^(python\s+-m\s+)?pytest(\s|$)",
        strip_ansi: true,
        strip_lines_matching: &[
            r"^=+ test session starts =+$",
            r"^platform ",
            r"^rootdir:",
            r"^plugins:",
            r"^collected \d+ items?$",
            r"^\s*$",
        ],
        keep_lines_matching: &[
            r"^=+ FAILURES =+$",
            r"^=+ ERRORS =+$",
            r"^FAILED ",
            r"^ERROR ",
            r"^short test summary info",
            r"^=+ .* in [0-9.]+s =+$",
        ],
        max_lines: Some(60),
        on_empty: Some("pytest: ok"),
        dedupe_repeats: false,
    },
    OutputFilterDef {
        pattern: r"^(pnpm\s+|npm\s+(run\s+)?)test(\s|$)|^(npx\s+|pnpm\s+)?(vitest|jest)(\s|$)",
        strip_ansi: true,
        strip_lines_matching: &[
            r"^> ",
            r"^ RUN ",
            r"^ PASS ",
            r"^ ✓ ",
            r"^ Test Files ",
            r"^ Duration ",
        ],
        keep_lines_matching: &[
            r"^ FAIL ",
            r"^ ❯ ",
            r"^× ",
            r"^stderr ",
            r"^stdout ",
            r"^Tests?\s+",
            r"^Snapshots?\s+",
            r"^Time:\s+",
        ],
        max_lines: Some(60),
        on_empty: Some("tests: ok"),
        dedupe_repeats: false,
    },
    OutputFilterDef {
        pattern: r"^docker\s+(logs|compose\s+logs)\b|^kubectl\s+logs\b",
        strip_ansi: true,
        strip_lines_matching: &[],
        keep_lines_matching: &[],
        max_lines: Some(80),
        on_empty: None,
        dedupe_repeats: true,
    },
];

static FILTERS: LazyLock<Vec<OutputFilter>> = LazyLock::new(|| {
    FILTER_DEFS
        .iter()
        .map(|def| OutputFilter {
            pattern: Regex::new(def.pattern).expect("invalid output filter regex"),
            strip_ansi: def.strip_ansi,
            strip_lines_matching: def
                .strip_lines_matching
                .iter()
                .map(|pattern| Regex::new(pattern).expect("invalid strip regex"))
                .collect(),
            keep_lines_matching: def
                .keep_lines_matching
                .iter()
                .map(|pattern| Regex::new(pattern).expect("invalid keep regex"))
                .collect(),
            max_lines: def.max_lines,
            on_empty: def.on_empty,
            dedupe_repeats: def.dedupe_repeats,
        })
        .collect()
});

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
            if lines.len() > max_lines {
                lines.truncate(max_lines);
                lines.push(format!("... (truncated to {} lines)", max_lines));
            }
        }
        if lines.is_empty() {
            return filter.on_empty.unwrap_or_default().to_string();
        }
        return lines.join("\n");
    }

    generic_compact(output)
}

fn generic_compact(output: &str) -> String {
    let stripped = ANSI_RE.replace_all(output, "").to_string();
    let lines: Vec<String> = stripped
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect();
    let mut lines = normalize_blank_lines(dedupe_repeated_lines(lines));
    if lines.len() > 80 {
        lines.truncate(80);
        lines.push("... (truncated to 80 lines)".to_string());
    }
    lines.join("\n")
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
mod tests {
    use super::*;

    #[test]
    fn classifies_git_status() {
        let result = classify_command("git status");
        assert!(matches!(
            result,
            Classification::Supported {
                category: "Git",
                ..
            }
        ));
    }

    #[test]
    fn compacts_git_status_output() {
        let raw = "\
On branch main
Your branch is up to date with 'origin/main'.

Changes not staged for commit:
  (use \"git add <file>...\" to update what will be committed)
  modified:   src/main.rs

Untracked files:
  (use \"git add <file>...\" to include in what will be committed)
  new-file.rs
";
        let compacted = compact_output("git status", raw);
        assert!(compacted.contains("modified:   src/main.rs"));
        assert!(compacted.contains("new-file.rs"));
        assert!(!compacted.contains("On branch"));
    }
}
