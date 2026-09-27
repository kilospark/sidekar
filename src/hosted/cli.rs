//! `sidekar session …`: start hosted sessions and drive them.

use super::protocol::{Busy, Event, EventBody, Reply, Request};
use super::{ApprovalPolicy, Meta, Status};
use crate::utils::ExitWith;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Exit statuses a caller branches on. 0 is a finished turn.
pub const EXIT_TIMEOUT: i32 = 1;
pub const EXIT_ENDED: i32 = 2;
pub const EXIT_APPROVAL: i32 = 3;
pub const EXIT_BUSY: i32 = 4;
pub const EXIT_TURN_FAILED: i32 = 5;

const DEFAULT_WAIT: Duration = Duration::from_secs(600);
const START_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) const USAGE: &str = "\
sidekar session start <engine> [--cwd <dir>] [--model <m>] [--approvals ask|allow|deny] [--name <n>]
sidekar session send <name> <text|--file=path> [--wait] [--timeout <d>] [--queue|--interrupt]
sidekar session wait <name> [--turn <id>] [--timeout <d>]
sidekar session approve <name> <request_id> allow|deny [--message <why>]
sidekar session cancel <name>
sidekar session events <name> [--since <seq>] [--follow]
sidekar session status <name>
sidekar session list
sidekar session stop <name>
sidekar session resume <name>";

pub(crate) fn describe(d: Duration) -> String {
    crate::bus::await_reply::describe(d)
}

pub async fn handle(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match sub {
        "start" => start(rest).await,
        "resume" => resume(rest).await,
        "send" => send(rest).await,
        "wait" => wait(rest).await,
        "approve" => approve(rest).await,
        "cancel" => simple(rest, Request::Cancel, "cancel").await,
        "events" => events(rest).await,
        "status" => status(rest).await,
        "list" | "ls" => list(),
        "stop" => stop(rest).await,
        "__host" => super::host::run(rest.first().context("__host needs a name")?).await,
        "" | "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown session command {other:?}\n{USAGE}"),
    }
}

/// Flags and positionals, with `--flag value` and `--flag=value` both accepted.
struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Self> {
        let mut out = Self {
            positional: Vec::new(),
            flags: Vec::new(),
        };
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if let Some(flag) = a.strip_prefix("--") {
                let (name, inline) = match flag.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (flag.to_string(), None),
                };
                if with_value.contains(&name.as_str()) {
                    let value = match inline {
                        Some(v) => v,
                        None => {
                            i += 1;
                            args.get(i)
                                .cloned()
                                .with_context(|| format!("--{name} needs a value"))?
                        }
                    };
                    out.flags.push((name, Some(value)));
                } else if switches.contains(&name.as_str()) && inline.is_none() {
                    out.flags.push((name, None));
                } else {
                    bail!("unknown option --{name}\n{USAGE}");
                }
            } else {
                out.positional.push(a.clone());
            }
            i += 1;
        }
        Ok(out)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }

    fn timeout(&self) -> Result<Duration> {
        match self.value("timeout") {
            Some(v) => crate::bus::await_reply::parse_duration(v),
            None => Ok(DEFAULT_WAIT),
        }
    }

    fn name(&self) -> Result<&str> {
        self.positional
            .first()
            .map(String::as_str)
            .with_context(|| format!("which session? (`sidekar session list`)\n{USAGE}"))
    }
}

async fn start(args: &[String]) -> Result<()> {
    let a = Args::parse(
        args,
        &["cwd", "model", "approvals", "name"],
        &["refresh-env"],
    )?;
    let engine = a
        .positional
        .first()
        .context("start which engine? e.g. `sidekar session start claude`")?
        .clone();
    super::engine::for_name(&engine)?;
    let approvals = match a.value("approvals") {
        Some(v) => ApprovalPolicy::parse(v)?,
        None => ApprovalPolicy::Ask,
    };
    let cwd = match a.value("cwd") {
        Some(d) => std::fs::canonicalize(d)
            .with_context(|| format!("--cwd {d}"))?
            .to_string_lossy()
            .to_string(),
        None => std::env::current_dir()?.to_string_lossy().to_string(),
    };
    super::ensure_private_dir(&super::root())?;
    let name = match a.value("name") {
        Some(n) => {
            if n.is_empty() || n.contains('/') || n.starts_with('.') {
                bail!("--name {n:?}: use letters, digits and dashes");
            }
            if let Ok(m) = super::read_meta(n)
                && m.status != Status::Ended
                && super::host_alive(&m)
            {
                bail!("a session named {n} is already running");
            }
            n.to_string()
        }
        None => super::free_name(&engine),
    };
    super::ensure_private_dir(&super::dir_of(&name))?;
    let meta = Meta {
        name: name.clone(),
        engine,
        cwd,
        model: a.value("model").map(String::from),
        approvals,
        refresh_env: a.has("refresh-env"),
        status: Status::Starting,
        pid: 0,
        engine_pid: 0,
        engine_session_id: None,
        created_at: crate::message::epoch_secs(),
        ended_at: None,
        exit_code: None,
    };
    super::write_meta(&meta)?;
    launch_host(&name).await?;
    println!("{name}");
    Ok(())
}

/// Start a new host for a session whose host is gone, continuing the
/// engine's conversation from where it was.
async fn resume(args: &[String]) -> Result<()> {
    let a = Args::parse(args, &[], &[])?;
    let name = a.name()?;
    let mut meta = super::read_meta(name)?;
    if super::host_alive(&meta) {
        bail!("{name} is still running");
    }
    // A killed host's engine can still be running. Resuming beside it would
    // put two engines on one conversation, so it is stopped first.
    super::reap(&mut meta)?;
    if meta.engine_session_id.is_none() {
        bail!("{name} ended before its engine reported a conversation id; nothing to resume");
    }
    meta.status = Status::Starting;
    meta.pid = 0;
    meta.engine_pid = 0;
    meta.ended_at = None;
    meta.exit_code = None;
    super::write_meta(&meta)?;
    launch_host(name).await?;
    println!("{name}");
    Ok(())
}

async fn launch_host(name: &str) -> Result<()> {
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(super::log_path(name))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["session", "__host", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log));
    // Its own session: the host outlives this command and must not take a
    // Ctrl-C or hangup aimed at the caller's terminal.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("failed to start the session host")?;

    let started = Instant::now();
    loop {
        let meta = super::read_meta(name)?;
        if meta.status == Status::Running
            && UnixStream::connect(super::socket_path(name)).await.is_ok()
        {
            // Reap the launcher's child entry; the host itself keeps running.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            bail!(
                "the session host exited ({status}) before it was ready:\n{}",
                log_tail(name)
            );
        }
        if started.elapsed() >= START_TIMEOUT {
            let _ = child.kill();
            bail!(
                "the session host did not come up within {}:\n{}",
                describe(START_TIMEOUT),
                log_tail(name)
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn log_tail(name: &str) -> String {
    let log = std::fs::read_to_string(super::log_path(name)).unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

/// A connection to a running session's host.
struct Client {
    lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    write: tokio::net::unix::OwnedWriteHalf,
}

impl Client {
    async fn connect(name: &str) -> Result<Self> {
        let meta = super::read_meta(name)?;
        if meta.status == Status::Ended || !super::host_alive(&meta) {
            let mut meta = meta;
            let _ = super::reap(&mut meta);
            return Err(ExitWith::new(
                EXIT_ENDED,
                format!(
                    "session {name} has ended{}.{}",
                    meta.exit_code
                        .map(|c| format!(" (engine exit {c})"))
                        .unwrap_or_default(),
                    if meta.engine_session_id.is_some() {
                        format!(" `sidekar session resume {name}` continues it.")
                    } else {
                        String::new()
                    }
                ),
            )
            .into());
        }
        let stream = UnixStream::connect(super::socket_path(name))
            .await
            .with_context(|| format!("cannot reach session {name}"))?;
        let (read, write) = stream.into_split();
        Ok(Self {
            lines: BufReader::new(read).lines(),
            write,
        })
    }

    async fn call(&mut self, request: &Request) -> Result<Reply> {
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        self.write.write_all(&line).await?;
        let reply = self
            .lines
            .next_line()
            .await?
            .context("the session host closed the connection")?;
        Ok(serde_json::from_str(&reply)?)
    }

    async fn next_event(&mut self) -> Result<Option<Event>> {
        match self.lines.next_line().await? {
            Some(line) => Ok(Some(serde_json::from_str(&line)?)),
            None => Ok(None),
        }
    }
}

fn refused(reply: Reply) -> anyhow::Error {
    let message = reply.error.unwrap_or_else(|| "refused".into());
    match reply.code.as_deref() {
        Some("busy") => ExitWith::new(EXIT_BUSY, message).into(),
        _ => anyhow::anyhow!(message),
    }
}

/// The proxy environment this process runs with, for handing to a session
/// whose engine needs fresh network credentials on each turn.
fn caller_proxy_env() -> HashMap<String, String> {
    const VARS: &[&str] = &[
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
        "no_proxy",
        "NO_PROXY",
        "NODE_EXTRA_CA_CERTS",
    ];
    VARS.iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect()
}

async fn send(args: &[String]) -> Result<()> {
    let a = Args::parse(args, &["timeout", "file"], &["wait", "queue", "interrupt"])?;
    let name = a.name()?.to_string();
    let text = match a.value("file") {
        Some(path) => std::fs::read_to_string(path).with_context(|| format!("--file {path}"))?,
        None => a.positional[1..].join(" "),
    };
    if text.trim().is_empty() {
        bail!("nothing to send\n{USAGE}");
    }
    let busy = match (a.has("queue"), a.has("interrupt")) {
        (true, true) => bail!("--queue and --interrupt are opposites; pick one"),
        (true, false) => Busy::Queue,
        (false, true) => Busy::Interrupt,
        _ => Busy::Reject,
    };
    let wait = a.has("wait");
    let timeout = a.timeout()?;
    let meta = super::read_meta(&name)?;
    let policy = meta.approvals;
    // When the session refreshes its env, hand the host this process's
    // proxy environment so the engine's network keeps working.
    let env = meta
        .refresh_env
        .then(caller_proxy_env)
        .filter(|m| !m.is_empty());

    let mut client = Client::connect(&name).await?;
    let reply = client
        .call(&Request::Send {
            text,
            busy,
            follow: wait,
            env,
        })
        .await?;
    if !reply.ok {
        return Err(refused(reply));
    }
    let turn_id = reply.turn_id.clone().unwrap_or_default();
    if !wait {
        println!("{turn_id}");
        return Ok(());
    }
    // Everything after the send is new, so there is no backlog to settle.
    follow_turn(&mut client, &name, &turn_id, policy, timeout, 0).await
}

async fn wait(args: &[String]) -> Result<()> {
    let a = Args::parse(args, &["turn", "timeout"], &[])?;
    let name = a.name()?.to_string();
    let timeout = a.timeout()?;
    let policy = super::read_meta(&name)?.approvals;
    let mut client = Client::connect(&name).await?;
    let reply = client.call(&Request::Subscribe { since: 0 }).await?;
    if !reply.ok {
        return Err(refused(reply));
    }
    let backlog_end = reply.seq.unwrap_or(0);
    let target = match a.value("turn") {
        Some(t) => t.to_string(),
        None => latest_sent_turn(&name)
            .with_context(|| format!("nothing has been sent to {name} yet"))?,
    };
    follow_turn(&mut client, &name, &target, policy, timeout, backlog_end).await
}

/// The most recent turn a client sent, from the session's event log.
fn latest_sent_turn(name: &str) -> Option<String> {
    read_event_log(name)
        .into_iter()
        .rev()
        .find_map(|e| match e.body {
            EventBody::TurnStarted {
                turn_id, source, ..
            } if source != "engine" => Some(turn_id),
            _ => None,
        })
}

fn read_event_log(name: &str) -> Vec<Event> {
    std::fs::read_to_string(super::events_path(name))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// What a caller waiting on one turn should do next.
#[derive(Debug, PartialEq)]
pub(crate) enum Waited {
    Done {
        result: String,
        is_error: bool,
    },
    /// An approval under `ask` is waiting on the caller.
    NeedsApproval(serde_json::Value),
    Ended,
}

/// Fold events into the outcome for `target`, or `None` to keep waiting.
///
/// `settled` is false while replaying events that happened before the caller
/// arrived: an approval raised and then resolved in the past is not news, so
/// unresolved ones are only judged once the replay is over.
#[derive(Default)]
pub(crate) struct TurnWatch {
    pending: Vec<serde_json::Value>,
}

impl TurnWatch {
    pub(crate) fn observe(
        &mut self,
        event: &Event,
        target: &str,
        policy: ApprovalPolicy,
        settled: bool,
    ) -> Option<Waited> {
        match &event.body {
            EventBody::TurnDone {
                turn_id,
                result,
                is_error,
                ..
            } if turn_id == target => Some(Waited::Done {
                result: result.clone(),
                is_error: *is_error,
            }),
            EventBody::ApprovalRequested {
                turn_id,
                request_id,
                tool,
                input,
            } if turn_id == target && policy == ApprovalPolicy::Ask => {
                let request = serde_json::json!({
                    "request_id": request_id, "turn_id": turn_id, "tool": tool, "input": input,
                });
                if settled {
                    return Some(Waited::NeedsApproval(request));
                }
                self.pending.push(request);
                None
            }
            EventBody::ApprovalResolved { request_id, .. } => {
                self.pending
                    .retain(|p| p["request_id"].as_str() != Some(request_id.as_str()));
                None
            }
            EventBody::SessionEnded { .. } => Some(Waited::Ended),
            _ => None,
        }
    }

    /// After the replay: an approval still open is waiting on the caller.
    pub(crate) fn unresolved(&self) -> Option<Waited> {
        self.pending.first().cloned().map(Waited::NeedsApproval)
    }
}

async fn follow_turn(
    client: &mut Client,
    name: &str,
    target: &str,
    policy: ApprovalPolicy,
    timeout: Duration,
    backlog_end: u64,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut watch = TurnWatch::default();
    let mut settled = backlog_end == 0;
    loop {
        let next = tokio::time::timeout_at(deadline, client.next_event()).await;
        let event = match next {
            Err(_) => {
                return Err(ExitWith::new(
                    EXIT_TIMEOUT,
                    format!(
                        "turn {target} still running after {}. `sidekar session wait {name} --turn {target}` keeps waiting.",
                        describe(timeout)
                    ),
                )
                .into());
            }
            Ok(Ok(Some(e))) => e,
            Ok(Ok(None)) => {
                return Err(ExitWith::new(
                    EXIT_ENDED,
                    format!("session {name} closed the connection"),
                )
                .into());
            }
            Ok(Err(e)) => return Err(e),
        };
        let outcome = watch.observe(&event, target, policy, settled);
        if !settled && event.seq >= backlog_end {
            settled = true;
            if outcome.is_none()
                && let Some(w) = watch.unresolved()
            {
                return finish(name, target, w);
            }
        }
        if let Some(w) = outcome {
            return finish(name, target, w);
        }
    }
}

fn finish(name: &str, target: &str, waited: Waited) -> Result<()> {
    match waited {
        Waited::Done {
            result,
            is_error: false,
        } => {
            println!("{result}");
            Ok(())
        }
        Waited::Done {
            result,
            is_error: true,
        } => Err(ExitWith::new(EXIT_TURN_FAILED, format!("turn {target} failed: {result}")).into()),
        Waited::NeedsApproval(request) => {
            println!("{request}");
            Err(ExitWith::new(
                EXIT_APPROVAL,
                format!(
                    "turn {target} is waiting for approval. Answer with `sidekar session approve {name} {} allow|deny`, then `sidekar session wait {name} --turn {target}`.",
                    request["request_id"].as_str().unwrap_or("?")
                ),
            )
            .into())
        }
        Waited::Ended => Err(ExitWith::new(
            EXIT_ENDED,
            format!("session {name} ended before turn {target} finished"),
        )
        .into()),
    }
}

async fn approve(args: &[String]) -> Result<()> {
    let a = Args::parse(args, &["message"], &[])?;
    let name = a.name()?;
    let (Some(request_id), Some(decision)) = (a.positional.get(1), a.positional.get(2)) else {
        bail!("Usage: sidekar session approve <name> <request_id> allow|deny [--message <why>]");
    };
    let allow = match decision.as_str() {
        "allow" | "yes" => true,
        "deny" | "no" => false,
        other => bail!("allow or deny, not {other:?}"),
    };
    let mut client = Client::connect(name).await?;
    let reply = client
        .call(&Request::Approve {
            request_id: request_id.clone(),
            allow,
            message: a.value("message").map(String::from),
        })
        .await?;
    if !reply.ok {
        return Err(refused(reply));
    }
    Ok(())
}

async fn simple(args: &[String], request: Request, what: &str) -> Result<()> {
    let a = Args::parse(args, &[], &[])?;
    let name = a.name()?;
    let mut client = Client::connect(name).await?;
    let reply = client.call(&request).await?;
    if !reply.ok {
        return Err(refused(reply).context(format!("{what} {name}")));
    }
    Ok(())
}

async fn events(args: &[String]) -> Result<()> {
    let a = Args::parse(args, &["since"], &["follow"])?;
    let name = a.name()?;
    let since: u64 = match a.value("since") {
        Some(v) => v.parse().context("--since takes an event seq")?,
        None => 0,
    };
    if !a.has("follow") {
        // From the log, so it works after the session has ended too.
        super::read_meta(name)?;
        for event in read_event_log(name).into_iter().filter(|e| e.seq > since) {
            println!("{}", serde_json::to_string(&event)?);
        }
        return Ok(());
    }
    let mut client = Client::connect(name).await?;
    let reply = client.call(&Request::Subscribe { since }).await?;
    if !reply.ok {
        return Err(refused(reply));
    }
    while let Some(event) = client.next_event().await? {
        println!("{}", serde_json::to_string(&event)?);
        if matches!(event.body, EventBody::SessionEnded { .. }) {
            break;
        }
    }
    Ok(())
}

async fn status(args: &[String]) -> Result<()> {
    let a = Args::parse(args, &[], &[])?;
    let name = a.name()?;
    let mut meta = super::read_meta(name)?;
    let _ = super::reap(&mut meta);
    let mut out = serde_json::to_value(&meta)?;
    if meta.status != Status::Ended
        && let Ok(mut client) = Client::connect(name).await
        && let Ok(reply) = client.call(&Request::Status).await
        && let Some(s) = reply.status
    {
        out["live"] = s;
    }
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn list() -> Result<()> {
    let mut sessions = super::list();
    if sessions.is_empty() {
        println!("No sessions. Start one with `sidekar session start claude`.");
        return Ok(());
    }
    for meta in sessions.iter_mut() {
        let _ = super::reap(meta);
        println!(
            "{}\t{}\t{}\t{}",
            meta.name,
            meta.engine,
            match meta.status {
                Status::Starting => "starting",
                Status::Running => "running",
                Status::Ended => "ended",
            },
            meta.cwd
        );
    }
    Ok(())
}

async fn stop(args: &[String]) -> Result<()> {
    let a = Args::parse(args, &[], &[])?;
    let name = a.name()?;
    let meta = super::read_meta(name)?;
    if meta.status == Status::Ended || !super::host_alive(&meta) {
        let mut meta = meta;
        let _ = super::reap(&mut meta);
        println!("{name} had already ended.");
        return Ok(());
    }
    if let Ok(mut client) = Client::connect(name).await {
        let _ = client.call(&Request::Close).await;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !super::host_alive(&super::read_meta(name)?) {
            println!("Stopped {name}.");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    unsafe { libc::kill(meta.pid, libc::SIGKILL) };
    let mut meta = super::read_meta(name)?;
    let _ = super::reap(&mut meta);
    println!("Stopped {name} (killed; it did not exit on its own).");
    Ok(())
}
