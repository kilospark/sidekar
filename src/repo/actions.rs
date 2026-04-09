use super::*;

#[derive(Debug)]
pub(super) struct RepoActionsListArgs {
    pub(super) target: Option<String>,
    pub(super) style: RepoStructuredStyle,
}

#[derive(Debug)]
pub(super) struct RepoActionsRunArgs {
    pub(super) action_id: String,
    pub(super) target: Option<String>,
    pub(super) style: RepoStructuredStyle,
    pub(super) timeout_secs: u64,
    pub(super) max_output_chars: usize,
    pub(super) include_output: bool,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub(super) struct ProjectAction {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) command: Vec<String>,
    pub(super) source: String,
    pub(super) description: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ProjectActionsSummary {
    pub(super) root: PathBuf,
    pub(super) actions: Vec<ProjectAction>,
}

#[derive(Debug, Serialize)]
pub(super) struct CommandRunSummary {
    pub(super) action_id: String,
    pub(super) headline: String,
    pub(super) exit_code: Option<i32>,
    pub(super) stdout_lines: usize,
    pub(super) stderr_lines: usize,
    pub(super) tail: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ProjectActionRunResult {
    pub(super) ok: bool,
    pub(super) root: PathBuf,
    pub(super) action: ProjectAction,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_sec: f64,
    pub(super) timed_out: bool,
    pub(super) summary: CommandRunSummary,
    pub(super) error: Option<String>,
    pub(super) stdout: Option<String>,
    pub(super) stderr: Option<String>,
}

pub(super) fn cmd_repo_actions(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("run") => cmd_repo_actions_run(ctx, &args[1..]),
        _ => cmd_repo_actions_list(ctx, args),
    }
}

fn cmd_repo_actions_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_actions_list_args(args)?;
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    let root = resolve_project_root(&cwd, opts.target.as_deref())?;
    let summary = ProjectActionsSummary {
        root: root.clone(),
        actions: discover_project_actions(&root)?,
    };
    match opts.style {
        RepoStructuredStyle::Json => write_output(ctx, &render_project_actions_json(&summary)?),
        RepoStructuredStyle::Plain => write_output(ctx, &render_project_actions_plain(&summary)),
    }
    Ok(())
}

fn cmd_repo_actions_run(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let opts = parse_repo_actions_run_args(args)?;
    let cwd = env::current_dir().context("failed to resolve current directory")?;
    let root = resolve_project_root(&cwd, opts.target.as_deref())?;
    let result = run_project_action(
        &root,
        &opts.action_id,
        opts.timeout_secs,
        opts.max_output_chars,
        opts.include_output,
    )?;
    match opts.style {
        RepoStructuredStyle::Json => write_output(ctx, &render_project_action_run_json(&result)?),
        RepoStructuredStyle::Plain => write_output(ctx, &render_project_action_run_plain(&result)),
    }
    if !result.ok {
        bail!("project action failed: {}", result.summary.headline);
    }
    Ok(())
}

fn parse_repo_actions_list_args(args: &[String]) -> Result<RepoActionsListArgs> {
    let mut target = None;
    let mut style = RepoStructuredStyle::Plain;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--style=") {
            style = RepoStructuredStyle::parse(value)?;
        } else if arg.starts_with("--") {
            bail!("Unknown flag: {arg}");
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            bail!("Usage: sidekar repo actions [path] [--style=json|plain]");
        }
    }

    Ok(RepoActionsListArgs { target, style })
}

fn parse_repo_actions_run_args(args: &[String]) -> Result<RepoActionsRunArgs> {
    let mut action_id = None;
    let mut target = None;
    let mut style = RepoStructuredStyle::Plain;
    let mut timeout_secs = DEFAULT_ACTION_TIMEOUT_SECS;
    let mut max_output_chars = DEFAULT_ACTION_MAX_OUTPUT_CHARS;
    let mut include_output = false;

    for arg in args {
        if let Some(value) = arg.strip_prefix("--style=") {
            style = RepoStructuredStyle::parse(value)?;
        } else if let Some(value) = arg.strip_prefix("--timeout=") {
            timeout_secs = value
                .parse::<u64>()
                .context("--timeout must be a positive integer")?;
        } else if let Some(value) = arg.strip_prefix("--max-output-chars=") {
            max_output_chars = value
                .parse::<usize>()
                .context("--max-output-chars must be a positive integer")?;
        } else if arg == "--include-output" {
            include_output = true;
        } else if arg.starts_with("--") {
            bail!("Unknown flag: {arg}");
        } else if action_id.is_none() {
            action_id = Some(arg.clone());
        } else if target.is_none() {
            target = Some(arg.clone());
        } else {
            bail!(
                "Usage: sidekar repo actions run <action-id> [path] [--timeout=N] [--max-output-chars=N] [--include-output] [--style=json|plain]"
            );
        }
    }

    Ok(RepoActionsRunArgs {
        action_id: action_id.context("Usage: sidekar repo actions run <action-id> [path] [--timeout=N] [--max-output-chars=N] [--include-output] [--style=json|plain]")?,
        target,
        style,
        timeout_secs,
        max_output_chars,
        include_output,
    })
}

pub(super) fn discover_project_actions(root: &Path) -> Result<Vec<ProjectAction>> {
    let mut actions = Vec::new();
    let mut seen = BTreeSet::new();

    let mut add_action =
        |id: &str, kind: &str, command: Vec<String>, source: &str, description: &str| {
            if seen.insert(id.to_string()) {
                actions.push(ProjectAction {
                    id: id.to_string(),
                    kind: kind.to_string(),
                    command,
                    source: source.to_string(),
                    description: description.to_string(),
                });
            }
        };

    let package_json = root.join("package.json");
    if package_json.exists() {
        if let Ok(raw) = fs::read_to_string(&package_json) {
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                if let Some(scripts) = value.get("scripts").and_then(Value::as_object) {
                    for script_name in scripts.keys().collect::<Vec<_>>() {
                        let kind = match script_name.as_str() {
                            "test" => "test",
                            "lint" => "lint",
                            "build" => "build",
                            "start" | "dev" => "run",
                            "typecheck" | "check" => "check",
                            _ => "custom",
                        };
                        add_action(
                            &format!("npm:{script_name}"),
                            kind,
                            vec![
                                "npm".to_string(),
                                "run".to_string(),
                                script_name.to_string(),
                            ],
                            "package.json",
                            &format!("Run npm script '{script_name}'."),
                        );
                    }
                }
            }
        }
    }

    let pyproject = root.join("pyproject.toml");
    if pyproject.exists() {
        let raw = fs::read_to_string(&pyproject).unwrap_or_default();
        let has_tests_dir = root.join("tests").is_dir();
        if raw.contains("[tool.pytest") || has_tests_dir {
            let mut command = vec!["pytest".to_string()];
            if has_tests_dir {
                command.push("tests/".to_string());
                command.push("-v".to_string());
            }
            add_action(
                "python:test",
                "test",
                command,
                "pyproject.toml",
                "Run the Python test suite with pytest.",
            );
        }
        if raw.contains("[tool.ruff") {
            let mut command = vec!["ruff".to_string(), "check".to_string()];
            if root.join("src").exists() {
                command.push("src/".to_string());
            }
            if root.join("tests").exists() {
                command.push("tests/".to_string());
            }
            if command.len() == 2 {
                command.push(".".to_string());
            }
            add_action(
                "python:lint",
                "lint",
                command,
                "pyproject.toml",
                "Run Ruff checks for the Python project.",
            );
        }
    }

    if root.join("Cargo.toml").exists() {
        add_action(
            "cargo:test",
            "test",
            vec!["cargo".to_string(), "test".to_string()],
            "Cargo.toml",
            "Run the Rust test suite.",
        );
        add_action(
            "cargo:check",
            "check",
            vec!["cargo".to_string(), "check".to_string()],
            "Cargo.toml",
            "Run cargo check.",
        );
        add_action(
            "cargo:build",
            "build",
            vec!["cargo".to_string(), "build".to_string()],
            "Cargo.toml",
            "Build the Rust project.",
        );
    }

    if root.join("go.mod").exists() {
        add_action(
            "go:test",
            "test",
            vec!["go".to_string(), "test".to_string(), "./...".to_string()],
            "go.mod",
            "Run Go tests.",
        );
        add_action(
            "go:build",
            "build",
            vec!["go".to_string(), "build".to_string(), "./...".to_string()],
            "go.mod",
            "Build Go packages.",
        );
    }

    for makefile_name in ["Makefile", "makefile", "GNUmakefile"] {
        let path = root.join(makefile_name);
        if !path.exists() {
            continue;
        }
        let contents = fs::read_to_string(&path).unwrap_or_default();
        let target_regex =
            Regex::new(r"(?m)^([A-Za-z0-9_.-]+)\s*:").expect("valid makefile target regex");
        let targets = target_regex
            .captures_iter(&contents)
            .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .filter(|target| !target.starts_with('.'))
            .collect::<BTreeSet<_>>();
        for (target, kind) in [
            ("test", "test"),
            ("lint", "lint"),
            ("build", "build"),
            ("run", "run"),
        ] {
            if targets.contains(target) {
                add_action(
                    &format!("make:{target}"),
                    kind,
                    vec!["make".to_string(), target.to_string()],
                    makefile_name,
                    &format!("Run make target '{target}'."),
                );
            }
        }
        break;
    }

    actions.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(actions)
}

fn run_project_action(
    root: &Path,
    action_id: &str,
    timeout_secs: u64,
    max_output_chars: usize,
    include_output: bool,
) -> Result<ProjectActionRunResult> {
    let actions = discover_project_actions(root)?;
    let action = actions
        .into_iter()
        .find(|candidate| candidate.id == action_id)
        .with_context(|| format!("unknown action '{action_id}'"))?;
    let start = Instant::now();
    let mut child = Command::new(&action.command[0])
        .args(&action.command[1..])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run action '{}'", action.id))?;
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if start.elapsed().as_secs() >= timeout_secs {
            let _ = child.kill();
            break true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to collect output for action '{}'", action.id))?;
    let duration_sec = start.elapsed().as_secs_f64();
    let stdout = truncate_output(&String::from_utf8_lossy(&output.stdout), max_output_chars);
    let stderr = truncate_output(&String::from_utf8_lossy(&output.stderr), max_output_chars);
    let summary = summarize_command_output(&action.id, &stdout, &stderr, output.status.code());
    Ok(ProjectActionRunResult {
        ok: output.status.success() && !timed_out,
        root: root.to_path_buf(),
        action,
        exit_code: output.status.code(),
        duration_sec,
        timed_out,
        summary,
        error: timed_out.then(|| format!("Action timed out after {timeout_secs}s.")),
        stdout: include_output.then_some(stdout),
        stderr: include_output.then_some(stderr),
    })
}

fn truncate_output(value: &str, max_output_chars: usize) -> String {
    if value.len() <= max_output_chars {
        return value.to_string();
    }
    let omitted = value.len().saturating_sub(max_output_chars);
    format!(
        "{}\n... [truncated {omitted} chars]",
        &value[..max_output_chars]
    )
}

fn summarize_command_output(
    action_id: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> CommandRunSummary {
    let stdout_lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let stderr_lines = stderr
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let headline = stdout_lines
        .last()
        .cloned()
        .or_else(|| stderr_lines.last().cloned())
        .unwrap_or_else(|| {
            if exit_code == Some(0) {
                "Command completed successfully.".to_string()
            } else {
                "Command failed.".to_string()
            }
        });
    let tail = stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .rev()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();

    CommandRunSummary {
        action_id: action_id.to_string(),
        headline,
        exit_code,
        stdout_lines: stdout_lines.len(),
        stderr_lines: stderr_lines.len(),
        tail,
    }
}

fn render_project_actions_plain(summary: &ProjectActionsSummary) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Project Actions: {}", summary.root.display());
    let _ = writeln!(out, "Actions: {}", summary.actions.len());
    if summary.actions.is_empty() {
        let _ = writeln!(out, "\nNo actions discovered.");
        return out;
    }
    for action in &summary.actions {
        let _ = writeln!(
            out,
            "\n- {} [{}] ({})",
            action.id, action.kind, action.source
        );
        let _ = writeln!(out, "  cmd: {}", action.command.join(" "));
        let _ = writeln!(out, "  {}", action.description);
    }
    out
}

fn render_project_actions_json(summary: &ProjectActionsSummary) -> Result<String> {
    serde_json::to_string_pretty(summary).context("failed to render project actions JSON")
}

fn render_project_action_run_plain(result: &ProjectActionRunResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Project Action: {}", result.action.id);
    let _ = writeln!(out, "Root: {}", result.root.display());
    let _ = writeln!(out, "Command: {}", result.action.command.join(" "));
    let _ = writeln!(out, "Exit Code: {:?}", result.exit_code);
    let _ = writeln!(out, "Duration: {:.3}s", result.duration_sec);
    let _ = writeln!(out, "Headline: {}", result.summary.headline);
    let _ = writeln!(
        out,
        "stdout_lines={} stderr_lines={}",
        result.summary.stdout_lines, result.summary.stderr_lines
    );
    if !result.summary.tail.is_empty() {
        let _ = writeln!(out, "Tail:");
        for line in &result.summary.tail {
            let _ = writeln!(out, "- {line}");
        }
    }
    if let Some(stdout) = &result.stdout {
        let _ = writeln!(out, "\nStdout\n{stdout}");
    }
    if let Some(stderr) = &result.stderr {
        let _ = writeln!(out, "\nStderr\n{stderr}");
    }
    out
}

fn render_project_action_run_json(result: &ProjectActionRunResult) -> Result<String> {
    serde_json::to_string_pretty(result).context("failed to render project action result JSON")
}
