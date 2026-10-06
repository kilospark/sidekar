//! Launch a second agent, wait for it to reach the bus, and hand back its name.
//!
//! The name an agent registers under is chosen inside the PTY wrapper and cannot
//! be predicted from outside, so spawn plants a token in the child's environment
//! and waits for a registration carrying it. That token is also what makes
//! `spawn list` and `stop` honest about which panes sidekar owns.

mod terminal;

use crate::AppContext;
use anyhow::{Result, bail};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for a spawned agent to register before giving up on it.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

pub async fn cmd_spawn(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            bail!(
                "Usage: sidekar spawn <agent> [task] [--nick <name>] [--cwd <dir>] \
                 [--model <model>] [--window] [--app <name>] [--log <path>] [--pty] \
                 [--no-yolo] [--relay|--no-relay] [--proxy|--no-proxy] [--wait] \
                 [--timeout <duration>]\n       \
                 sidekar spawn list"
            )
        }
        Some("list") | Some("--list") => return cmd_spawn_list(ctx),
        _ => {}
    }

    let mut agent = String::new();
    let mut task: Option<String> = None;
    let mut nick: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut model: Option<String> = None;
    let mut yolo = true;
    let mut relay: Option<bool> = None;
    let mut proxy: Option<bool> = None;
    let mut timeout: Option<Duration> = None;
    let mut wait = false;
    let mut window = false;
    let mut pty = false;
    let mut app: Option<String> = None;
    let mut log: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        let mut take_value = |label: &str| -> Result<String> {
            if let Some(v) = a.split_once('=').map(|(_, v)| v.to_string()) {
                return Ok(v);
            }
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{label} needs a value"))
        };
        match a {
            "--no-yolo" => yolo = false,
            "--yolo" | "--auto-approve" => yolo = true,
            "--relay" => relay = Some(true),
            "--no-relay" => relay = Some(false),
            "--proxy" => proxy = Some(true),
            "--no-proxy" => proxy = Some(false),
            "--window" => window = true,
            "--pty" => pty = true,
            "--wait" => wait = true,
            _ if a.starts_with("--app") => {
                app = Some(take_value("--app")?);
                window = true;
            }
            _ if a.starts_with("--log") => log = Some(take_value("--log")?),
            _ if a.starts_with("--nick") => nick = Some(take_value("--nick")?),
            _ if a.starts_with("--cwd") => cwd = Some(take_value("--cwd")?),
            _ if a.starts_with("--model") => model = Some(take_value("--model")?),
            _ if a.starts_with("--timeout") => {
                timeout = Some(crate::bus::await_reply::parse_duration(&take_value(
                    "--timeout",
                )?)?);
            }
            _ if a.starts_with('-') => bail!("unknown option for spawn: {a}"),
            _ if agent.is_empty() => agent = a.to_string(),
            _ if task.is_none() => task = Some(a.to_string()),
            _ => bail!("spawn takes one agent and one task; quote the task if it has spaces"),
        }
        i += 1;
    }

    if agent.is_empty() {
        bail!("Usage: sidekar spawn <agent> [task] — run `sidekar spawn --help` for options");
    }
    if wait && task.is_none() {
        bail!(
            "--wait waits for the answer to a task; give one: sidekar spawn {agent} \"<task>\" --wait"
        );
    }
    if !crate::pty::is_agent_command(&agent) {
        bail!("'{agent}' is not a PTY-wrappable agent; see `sidekar help` for the supported list");
    }
    // Where the engine has one, the agent runs as a session (#17): a turn in
    // and a result out, over the engine's own protocol. Nothing is typed into
    // a screen, so there is no reply footer to skip and no idle to guess. The
    // terminal wrapper stays for what needs a terminal: a window to watch, a
    // transcript of the screen, a relay tunnel, the API proxy, or --pty.
    let as_session = !pty
        && !window
        && log.is_none()
        && relay != Some(true)
        && proxy != Some(true)
        && crate::hosted::engine::for_name(&agent).is_ok();
    if as_session && let Some(ref n) = nick {
        // A session's nick is its bus name, and registering a name takes it
        // from whoever has it.
        if crate::broker::find_agent(n, None)?.is_some() {
            bail!("'{n}' is already on the bus (`sidekar bus who`); pick another --nick");
        }
    }
    if !as_session && yolo && !crate::agent_cli::supports_yolo(&agent) {
        // Spawned agents have nobody to answer a prompt, so an agent that always
        // asks will sit there until it is stopped. Say so at launch rather than
        // letting it look like a hang.
        eprintln!(
            "\x1b[33m[sidekar]\x1b[0m {agent} has no unattended mode. It will stop at its \
             first approval prompt with nobody to answer it."
        );
    }

    // Without --wait the timeout is how long to wait for the agent to come up;
    // with it, how long to wait for the answer.
    let register_timeout = match (wait, timeout) {
        (false, Some(t)) => t,
        (true, Some(t)) => t.min(REGISTER_TIMEOUT),
        (_, None) => REGISTER_TIMEOUT,
    };
    let answer_timeout = timeout.unwrap_or(crate::bus::await_reply::DEFAULT_AWAIT);

    let token = crate::message::gen_msg_id();
    // Somewhere on the bus for the answer to go. An agent spawning a helper
    // already has one. A plain shell does not, so for --wait it gets one for
    // as long as it waits; without --wait there is nobody to answer to.
    let mut transient: Option<crate::bus::presence::Presence> = None;
    let spawner = match crate::bus::resolve_registered_agent_bus_name_for_current_process() {
        Some(name) => Some(name),
        None if wait => {
            let p = transient_identity()?;
            let name = p.name().to_string();
            transient = Some(p);
            Some(name)
        }
        None => None,
    };

    // The task still goes in on the command line — typing it into the agent's
    // terminal raced its startup — but it now carries a request id and the
    // command to answer it with, the same footer every bus request has, so
    // the answer is recorded against the task instead of only pasted.
    let request = match (&spawner, &task) {
        (Some(from), Some(t)) => {
            let from_id = crate::broker::find_agent(from, None)?
                .map(|a| a.id)
                .unwrap_or_else(|| crate::message::AgentId::new(from));
            Some(crate::message::Envelope::new_request(
                from_id,
                "",
                t.clone(),
            ))
        }
        _ => None,
    };
    // A session answers each request itself, so only the terminal needs the
    // footer.
    if let Some(ref req) = request
        && !as_session
    {
        task = Some(with_reply_footer(&req.message, &req.from.name, &req.id));
    }
    let spawner = spawner.unwrap_or_else(|| "cli".to_string());

    if let Some(refusal) = spawn_limit_refusal(
        &running_spawned()?,
        &spawner,
        crate::config::get_usize("max_spawned_per_agent"),
        crate::config::get_usize("max_spawned"),
    ) {
        bail!("{refusal}");
    }

    let exe = std::env::current_exe()?;

    // The argv is the same either way; only who holds the terminal differs.
    let wrapper = WrapperFlags { yolo, relay, proxy };
    let argv = child_argv(&agent, &wrapper, model.as_deref(), task.as_deref());

    let where_to_look = if as_session {
        let approvals = if yolo {
            crate::hosted::ApprovalPolicy::Allow
        } else {
            crate::hosted::ApprovalPolicy::Ask
        };
        let name = crate::hosted::cli::start_session(
            crate::hosted::cli::NewSession {
                engine: agent.clone(),
                cwd: cwd.clone(),
                model: model.clone(),
                approvals,
                refresh_env: false,
                name: nick.clone(),
            },
            &[
                ("SIDEKAR_SPAWNED_BY", spawner.as_str()),
                ("SIDEKAR_SPAWN_TOKEN", token.as_str()),
            ],
        )
        .await?;
        if !yolo {
            eprintln!(
                "[sidekar] approvals are on: `sidekar session events {name}` shows what is \
                 waiting, `sidekar session approve {name} <id> allow|deny` answers it."
            );
        }
        format!("session {name}; stop it with `sidekar session stop {name}`")
    } else if window {
        let chosen = match app.as_deref() {
            Some(name) => terminal::TerminalApp::parse(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown terminal app '{name}'; try terminal, iterm, ghostty, wezterm, \
                     kitty or alacritty"
                )
            })?,
            // Match the spawner's own terminal, which is the whole point of
            // reading TERM_PROGRAM rather than picking a default.
            None => terminal::detect().ok_or_else(|| {
                anyhow::anyhow!(
                    "--window could not tell which terminal this session is running under \
                     (TERM_PROGRAM is unset or not a windowed terminal). Name one with --app."
                )
            })?,
        };

        // A new window is a fresh shell: it inherits nothing from here, so the
        // token and the working directory have to travel inside the command.
        // Always cd. A new window starts in the user's home directory, and an
        // agent's bus name and repo context both come from where it is running,
        // so without this a windowed spawn lands somewhere the caller did not ask
        // for while the background path inherits the caller's directory.
        let start_dir = match cwd {
            Some(ref d) => d.clone(),
            None => std::env::current_dir()?.to_string_lossy().to_string(),
        };
        let mut line = format!("cd {} && ", terminal::shell_quote(&start_dir));
        line.push_str(&format!(
            "SIDEKAR_SPAWNED_BY={} SIDEKAR_SPAWN_TOKEN={} ",
            terminal::shell_quote(&spawner),
            terminal::shell_quote(&token)
        ));
        if let Some(ref n) = nick {
            line.push_str(&format!("SIDEKAR_NICK={} ", terminal::shell_quote(n)));
        }
        line.push_str(&format!(
            "exec {}",
            terminal::shell_quote(&exe.to_string_lossy())
        ));
        for a in &argv {
            line.push(' ');
            line.push_str(&terminal::shell_quote(a));
        }
        if let Some(ref l) = log {
            line = terminal::with_transcript(&line, l);
        }
        terminal::open_window(chosen, &line)?;
        "the window that just opened".to_string()
    } else {
        let mut child = Command::new(&exe);
        child.args(&argv);
        if let Some(ref d) = cwd {
            child.current_dir(d);
        }
        // Without a window there is nowhere for the agent's screen to go, so it
        // goes to the transcript when one was asked for and is dropped otherwise.
        let sink = match log {
            Some(ref path) => {
                let f = std::fs::File::create(path)?;
                let dup = f.try_clone()?;
                child.stdout(Stdio::from(f)).stderr(Stdio::from(dup));
                true
            }
            None => {
                child.stdout(Stdio::null()).stderr(Stdio::null());
                false
            }
        };
        let _ = sink;
        child
            .env("SIDEKAR_SPAWNED_BY", &spawner)
            .env("SIDEKAR_SPAWN_TOKEN", &token)
            .stdin(Stdio::null());
        if let Some(ref n) = nick {
            child.env("SIDEKAR_NICK", n);
        }

        // Own session and process group: the spawned agent outlives this command
        // and must not take a Ctrl-C aimed at the caller's terminal.
        unsafe {
            use std::os::unix::process::CommandExt;
            child.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let pid = child.spawn()?.id();
        format!("pid {pid}; stop it with `kill {pid}`")
    };

    let started = Instant::now();
    let found = loop {
        if let Ok(Some(found)) = crate::broker::agent_for_spawn_token(&token) {
            break found;
        }
        if started.elapsed() >= register_timeout {
            bail!(
                "{agent} was launched ({where_to_look}) but never registered on the bus \
                 within {}s. Check it with `sidekar bus who`.",
                register_timeout.as_secs()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    let name = found.id.name.clone();

    let Some(mut request) = request else {
        if as_session && let Some(task) = task {
            // From a plain shell there is nobody on the bus for an answer to
            // go to, so the task goes in as a turn and `session wait` collects
            // its result.
            let turn = crate::hosted::cli::send_turn(&name, task).await?;
            eprintln!(
                "[sidekar] task sent as turn {turn}; `sidekar session wait {name}` waits for \
                 the answer."
            );
        }
        out!(ctx, "{name}");
        return Ok(());
    };
    request.to = name.clone();
    track_request(&request, &found);
    if as_session {
        // The session takes its bus mail as turns and answers each request
        // itself, recorded against the request's id.
        crate::broker::enqueue_bus_message(
            &name,
            &request.from.name,
            &request.message,
            true,
            Some(&request),
        )?;
    }

    if !wait {
        eprintln!(
            "[sidekar] task sent as request {id}; `sidekar bus await {id}` waits for the answer.",
            id = request.id
        );
        out!(ctx, "{name}");
        return Ok(());
    }

    eprintln!(
        "[sidekar] {name} is working on it (request {}). Waiting up to {}.",
        request.id,
        crate::bus::await_reply::describe(answer_timeout)
    );
    let recipient = crate::bus::await_reply::Recipient {
        name: name.clone(),
        pane: found.id.pane.clone(),
    };
    let outcome =
        crate::bus::await_reply::await_reply(&request.id, Some(recipient), answer_timeout).await?;
    if transient.is_some() && matches!(outcome, crate::bus::await_reply::AwaitOutcome::TimedOut) {
        // Nobody will be at this address to take a late answer, so withdraw the
        // request rather than leave the agent reminded to answer it.
        let _ = crate::broker::cancel_outbound_request(&request.id, crate::message::epoch_secs());
        bail!(
            "No answer from {name} within {}. It is still running; stop it with \
             `sidekar stop {name}`.",
            crate::bus::await_reply::describe(answer_timeout)
        );
    }
    let answer = crate::bus::await_reply::answer_or_exit(&request.id, outcome, answer_timeout)?;
    drop(transient);
    // The agent stays up for follow-ups; say where, off stdout so the answer
    // is all that `$(...)` captures.
    eprintln!("[sidekar] answered by {name}; it is still running (`sidekar stop {name}`).");
    out!(ctx, "{answer}");
    Ok(())
}

/// Spawned agents still running, as `(name, spawner)`. A registration whose
/// process has died is not one: it waits for the sweep, and must not hold a
/// place under the limit until then.
fn running_spawned() -> Result<Vec<(String, String)>> {
    Ok(crate::broker::spawned_agents()?
        .into_iter()
        .filter(|(agent, _)| {
            agent
                .id
                .pane
                .as_deref()
                .and_then(crate::bus::presence::pid_of_pane)
                .is_some_and(crate::bus::presence::process_alive)
        })
        .map(|(agent, spawner)| (agent.id.name, spawner))
        .collect())
}

/// Why `spawner` may not spawn another agent while `running` are up, if it
/// may not. A limit of 0 is no limit.
fn spawn_limit_refusal(
    running: &[(String, String)],
    spawner: &str,
    per_spawner: usize,
    total: usize,
) -> Option<String> {
    let mine: Vec<&str> = running
        .iter()
        .filter(|(_, by)| by == spawner)
        .map(|(name, _)| name.as_str())
        .collect();
    let (what, key, limit, names) = if per_spawner > 0 && mine.len() >= per_spawner {
        (
            format!(
                "{spawner} already has {} spawned agents running",
                mine.len()
            ),
            "max_spawned_per_agent",
            per_spawner,
            mine,
        )
    } else if total > 0 && running.len() >= total {
        (
            format!("{} spawned agents are already running", running.len()),
            "max_spawned",
            total,
            running.iter().map(|(name, _)| name.as_str()).collect(),
        )
    } else {
        return None;
    };
    Some(format!(
        "Not spawning: {what} ({key} is {limit}): {}.\n\
         Stop one with `sidekar stop <name>` (`sidekar spawn list` shows them all), \
         or raise the limit with `sidekar config set {key} <n>` (0 means no limit).",
        names.join(", ")
    ))
}

/// The flags `sidekar <agent>` takes for itself, which spawn passes on.
struct WrapperFlags {
    yolo: bool,
    /// `--relay` or `--no-relay`; neither leaves it to the `relay` setting.
    relay: Option<bool>,
    /// `--proxy` or `--no-proxy`; neither leaves it to SIDEKAR_PROXY.
    proxy: Option<bool>,
}

/// The arguments spawn runs `sidekar` with: the agent, the wrapper's flags,
/// then the model and task for the agent.
fn child_argv(
    agent: &str,
    wrapper: &WrapperFlags,
    model: Option<&str>,
    task: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![agent.to_string()];
    if wrapper.yolo {
        argv.push("--yolo".into());
    }
    if let Some(on) = wrapper.relay {
        argv.push(if on { "--relay" } else { "--no-relay" }.into());
    }
    if let Some(on) = wrapper.proxy {
        argv.push(if on { "--proxy" } else { "--no-proxy" }.into());
    }
    if let Some(m) = model {
        argv.push("--model".into());
        argv.push(m.to_string());
    }
    if let Some(t) = task {
        argv.push(t.to_string());
    }
    argv
}

/// The task, plus how to answer it.
fn with_reply_footer(task: &str, reply_to: &str, msg_id: &str) -> String {
    format!(
        "{task}\n\nWhen you have the answer, send it back with:\n\
         sidekar bus send {reply_to} \"<your answer>\" --reply-to={msg_id}\n\
         (for a long answer, write it to a file and pass --file=<path> instead of the quoted text)"
    )
}

/// Record the task as an open request to the agent now running it, as `bus
/// send` does for a request: so its answer is linked to it, the agent is
/// reminded if it goes quiet without answering, and the request is closed as
/// `recipient_gone` if the agent leaves first.
fn track_request(request: &crate::message::Envelope, agent: &crate::broker::BrokerAgent) {
    let _ = crate::broker::set_pending(request);
    let project = crate::bus::detect_project_name();
    let _ = crate::broker::set_outbound_request(
        request,
        &request.from.display_name(),
        "broker",
        &agent.id.name,
        request.from.session.as_deref(),
        Some(project.as_str()),
    );
    let _ = crate::broker::mark_agent_session_request(
        &request.from.name,
        &request.id,
        request.created_at,
    );
}

/// A bus address for a plain shell for the length of one `spawn --wait`.
///
/// Registered under a `cli-<pid>` pane so the daemon's dead-agent sweep
/// removes it if this process is killed mid-wait.
fn transient_identity() -> Result<crate::bus::presence::Presence> {
    let project = crate::bus::detect_project_name();
    let name = crate::bus::presence::unique_name(&format!("cli-{project}"));
    crate::bus::presence::Presence::register(crate::bus::presence::Registration {
        nick: name.clone(),
        name,
        channel: crate::pty::detect_channel(),
        pane: format!("cli-{}", std::process::id()),
        agent_type: "sidekar",
        history: None,
    })
}

fn cmd_spawn_list(ctx: &mut AppContext) -> Result<()> {
    let spawned = crate::broker::spawned_agents()?;
    if spawned.is_empty() {
        out!(ctx, "No agents spawned by sidekar.");
        return Ok(());
    }
    for (agent, spawner) in spawned {
        let pane = agent.id.pane.as_deref().unwrap_or("?");
        let alive = crate::bus::presence::pid_of_pane(pane)
            .map(crate::bus::presence::process_alive)
            .unwrap_or(false);
        out!(
            ctx,
            "{}\t{}\tby {}\t{}",
            agent.id.name,
            pane,
            spawner,
            if alive { "running" } else { "dead" }
        );
    }
    Ok(())
}

pub fn cmd_stop(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let force = args.iter().any(|a| a == "--force");
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow::anyhow!("Usage: sidekar stop <agent-name> [--force]"))?;

    let Some(agent) = crate::broker::find_agent(name, None)? else {
        bail!("no agent named '{name}' on the bus; `sidekar bus who` lists them");
    };
    let spawner = crate::broker::spawner_of(&agent.id.name)?;
    if spawner.is_none() && !force {
        bail!(
            "'{}' was not started by sidekar spawn, so stopping it would kill a pane someone \
             else is using. Pass --force if you meant it.",
            agent.id.name
        );
    }

    if let Some(me) = crate::bus::resolve_registered_agent_bus_name_for_current_process() {
        let _ = crate::broker::cancel_requests_before_stopping(&me, &agent.id.name);
    }

    let pane = agent.id.pane.clone().unwrap_or_default();
    let Some(pid) = crate::bus::presence::pid_of_pane(&pane) else {
        crate::broker::unregister_agent(&agent.id.name)?;
        out!(
            ctx,
            "Unregistered {} (no live process found).",
            agent.id.name
        );
        return Ok(());
    };

    // SIGTERM, not SIGKILL: the wrapper unregisters and releases its bus claims
    // on the way out, so queued mail is handed back rather than stranded.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    out!(ctx, "Stopped {} (pid {}).", agent.id.name, pid);
    Ok(())
}

#[cfg(test)]
mod tests;
