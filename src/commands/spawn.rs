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
                 [--model <model>] [--window] [--app <name>] [--log <path>] \
                 [--no-yolo] [--timeout <secs>]\n       \
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
    let mut timeout = REGISTER_TIMEOUT;
    let mut window = false;
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
            "--window" => window = true,
            _ if a.starts_with("--app") => {
                app = Some(take_value("--app")?);
                window = true;
            }
            _ if a.starts_with("--log") => log = Some(take_value("--log")?),
            _ if a.starts_with("--nick") => nick = Some(take_value("--nick")?),
            _ if a.starts_with("--cwd") => cwd = Some(take_value("--cwd")?),
            _ if a.starts_with("--model") => model = Some(take_value("--model")?),
            _ if a.starts_with("--timeout") => {
                let secs: u64 = take_value("--timeout")?
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--timeout expects whole seconds"))?;
                timeout = Duration::from_secs(secs);
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
    if !crate::pty::is_agent_command(&agent) {
        bail!("'{agent}' is not a PTY-wrappable agent; see `sidekar help` for the supported list");
    }
    if yolo && !crate::agent_cli::supports_yolo(&agent) {
        // Spawned agents have nobody to answer a prompt, so an agent that always
        // asks will sit there until it is stopped. Say so at launch rather than
        // letting it look like a hang.
        eprintln!(
            "\x1b[33m[sidekar]\x1b[0m {agent} has no unattended mode. It will stop at its \
             first approval prompt with nobody to answer it."
        );
    }

    let token = crate::message::gen_msg_id();
    let spawner = crate::bus::resolve_registered_agent_bus_name_for_current_process()
        .unwrap_or_else(|| "cli".to_string());

    let exe = std::env::current_exe()?;

    // The argv is the same either way; only who holds the terminal differs.
    let mut argv: Vec<String> = vec![agent.clone()];
    if yolo {
        argv.push("--yolo".into());
    }
    if let Some(ref m) = model {
        argv.push("--model".into());
        argv.push(m.clone());
    }
    if let Some(ref t) = task {
        argv.push(t.clone());
    }

    let pid = if window {
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
        None
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
        Some(child.spawn()?.id())
    };

    let started = Instant::now();
    loop {
        if let Ok(Some(found)) = crate::broker::agent_for_spawn_token(&token) {
            out!(ctx, "{}", found.id.name);
            return Ok(());
        }
        if started.elapsed() >= timeout {
            let where_to_look = match pid {
                Some(p) => format!("pid {p}; stop it with `kill {p}`"),
                None => "the window that just opened".to_string(),
            };
            bail!(
                "{agent} was launched ({where_to_look}) but never registered on the bus \
                 within {}s. Check it with `sidekar bus who`.",
                timeout.as_secs()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn cmd_spawn_list(ctx: &mut AppContext) -> Result<()> {
    let spawned = crate::broker::spawned_agents()?;
    if spawned.is_empty() {
        out!(ctx, "No agents spawned by sidekar.");
        return Ok(());
    }
    for (agent, spawner) in spawned {
        let pane = agent.id.pane.as_deref().unwrap_or("?");
        let alive = pid_of(pane).map(is_alive).unwrap_or(false);
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

    let pane = agent.id.pane.clone().unwrap_or_default();
    let Some(pid) = pid_of(&pane) else {
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

fn pid_of(pane: &str) -> Option<i32> {
    for prefix in ["pty-", "repl-", "cli-"] {
        if let Some(rest) = pane.strip_prefix(prefix)
            && let Ok(pid) = rest.parse::<i32>()
        {
            return Some(pid);
        }
    }
    None
}

fn is_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(test)]
mod tests;
