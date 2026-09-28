//! The process behind one session: the engine's pipes on one side, clients on
//! a unix socket on the other, and the turn bookkeeping in between.
//!
//! Engines do not number turns, so the host does. A turn a client sent is
//! `t<n>`; one the engine started on its own is `e<n>`. At most one sent turn
//! runs at a time; what arrives meanwhile is refused, queued, or interrupts,
//! as the sender asked.

use super::engine::{self, Engine, Input, Output, StartOptions};
use super::protocol::{Busy, Event, EventBody, Reply, Request};
use super::{ApprovalPolicy, Meta, Status};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

/// An approval nobody answers is denied after this long, so a session whose
/// caller went away does not sit blocked forever.
pub(crate) const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

/// Events kept in memory; older ones are read back from the event log when a
/// client asks for them. A long session would otherwise grow without bound.
pub(crate) const EVENTS_IN_MEMORY: usize = 2000;

/// How long `close` waits for the engine to exit on its own before killing it.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// A send waiting for the running turn to finish.
struct Queued {
    turn_id: String,
    text: String,
    source: &'static str,
}

struct PendingApproval {
    turn_id: String,
    input: Value,
    asked_at: Instant,
}

/// A client's request, and where to send the answer.
struct Call {
    request: Request,
    reply: oneshot::Sender<(Reply, Option<Stream>)>,
}

/// Events for a client that asked to follow: what it missed, then live ones.
struct Stream {
    backlog: Vec<Event>,
    live: mpsc::UnboundedReceiver<Event>,
}

/// Everything the host keeps about a running session.
pub(crate) struct State {
    engine: Box<dyn Engine>,
    policy: ApprovalPolicy,
    events: VecDeque<Event>,
    last_seq: u64,
    sink: Option<std::fs::File>,
    /// Where `sink` writes, for reading back what memory has let go of.
    log: Option<std::path::PathBuf>,
    subscribers: Vec<mpsc::UnboundedSender<Event>>,
    next_turn: u64,
    next_engine_turn: u64,
    /// The sent turn in progress.
    current: Option<String>,
    /// A turn the engine started itself, in progress.
    engine_turn: Option<String>,
    queue: VecDeque<Queued>,
    approvals: HashMap<String, PendingApproval>,
    engine_session_id: Option<String>,
    /// Turns that came from the bus, and the message each answers.
    bus_turns: HashMap<String, crate::message::Envelope>,
    /// Bus turns that finished, waiting for their answer to be sent.
    bus_answers: Vec<(crate::message::Envelope, String, bool)>,
    /// Lines for the engine's stdin, written by the caller of the state
    /// machine. Kept apart so the bookkeeping is testable without a process.
    outbox: Vec<String>,
    /// Refresh the engine's proxy environment from each send's caller.
    refresh_env: bool,
    /// The proxy environment the running engine was started with.
    engine_env: HashMap<String, String>,
    /// The environment the current request offers, until the request is
    /// accepted or refused. A refused send changes nothing.
    offered_env: Option<HashMap<String, String>>,
    /// A newer environment than the engine's, waiting for a moment when
    /// nothing is running. Restarting the engine mid-turn kills the turn,
    /// and the host would wait for its result forever.
    pending_env: Option<HashMap<String, String>>,
    /// A restart decided on: the outbox index where the new engine's input
    /// begins, and the environment to start it with. Taken by the loop.
    restart: Option<(usize, HashMap<String, String>)>,
}

impl State {
    pub(crate) fn new(engine: Box<dyn Engine>, policy: ApprovalPolicy, refresh_env: bool) -> Self {
        Self {
            engine,
            policy,
            events: VecDeque::new(),
            last_seq: 0,
            sink: None,
            log: None,
            subscribers: Vec::new(),
            next_turn: 0,
            next_engine_turn: 0,
            current: None,
            engine_turn: None,
            queue: VecDeque::new(),
            approvals: HashMap::new(),
            engine_session_id: None,
            bus_turns: HashMap::new(),
            bus_answers: Vec::new(),
            outbox: Vec::new(),
            refresh_env,
            engine_env: HashMap::new(),
            offered_env: None,
            pending_env: None,
            restart: None,
        }
    }

    pub(crate) fn seq(&self) -> u64 {
        self.last_seq
    }

    #[cfg(test)]
    pub(crate) fn events(&self) -> Vec<Event> {
        self.events.iter().cloned().collect()
    }

    /// A bus message becomes a turn, queued behind any running one: a sender
    /// on the bus cannot be told "busy" the way a socket client can.
    pub(crate) fn send_from_bus(&mut self, message: crate::message::Envelope) -> Reply {
        let text = bus_turn_text(&message);
        let reply = self.send_as(text, Busy::Queue, "bus");
        if let Some(turn_id) = reply.turn_id.clone() {
            self.bus_turns.insert(turn_id, message);
        }
        reply
    }

    pub(crate) fn take_bus_answers(&mut self) -> Vec<(crate::message::Envelope, String, bool)> {
        std::mem::take(&mut self.bus_answers)
    }

    pub(crate) fn take_outbox(&mut self) -> Vec<String> {
        std::mem::take(&mut self.outbox)
    }

    fn emit(&mut self, body: EventBody, raw: Option<Value>) {
        self.last_seq += 1;
        let event = Event {
            seq: self.last_seq,
            ts: crate::message::epoch_secs(),
            body,
            raw,
        };
        if let Some(file) = self.sink.as_mut() {
            use std::io::Write;
            let _ = writeln!(
                file,
                "{}",
                serde_json::to_string(&event).unwrap_or_default()
            );
        }
        self.subscribers.retain(|s| s.send(event.clone()).is_ok());
        self.events.push_back(event);
        while self.events.len() > EVENTS_IN_MEMORY {
            self.events.pop_front();
        }
    }

    /// Everything after `since`: from memory, with anything older than
    /// memory holds read back from the log.
    fn backlog(&self, since: u64) -> Vec<Event> {
        let oldest_held = self.events.front().map_or(self.last_seq + 1, |e| e.seq);
        let mut out = Vec::new();
        if since + 1 < oldest_held
            && let Some(log) = &self.log
        {
            out.extend(
                std::fs::read_to_string(log)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Event>(l).ok())
                    .filter(|e| e.seq > since && e.seq < oldest_held),
            );
        }
        out.extend(self.events.iter().filter(|e| e.seq > since).cloned());
        out
    }

    fn write(&mut self, input: Input) {
        let lines = self.engine.encode(&input);
        self.outbox.extend(lines);
    }

    fn start_turn(&mut self, turn_id: String, text: String, source: &str) {
        // The one safe moment to replace the engine: a turn of ours is about
        // to begin and nothing else is running. A turn the engine started
        // itself defers it to the next start.
        if self.engine_turn.is_none()
            && self.approvals.is_empty()
            && let Some(env) = self.pending_env.take()
        {
            self.restart = Some((self.outbox.len(), env));
        }
        self.current = Some(turn_id.clone());
        self.emit(
            EventBody::TurnStarted {
                turn_id,
                source: source.into(),
                text: Some(text.clone()),
            },
            None,
        );
        self.write(Input::User(text));
    }

    /// The turn engine output belongs to. Output with no turn running means
    /// the engine started one itself.
    fn attribute(&mut self) -> String {
        if let Some(t) = self.current.clone().or_else(|| self.engine_turn.clone()) {
            return t;
        }
        self.next_engine_turn += 1;
        let id = format!("e{}", self.next_engine_turn);
        self.engine_turn = Some(id.clone());
        self.emit(
            EventBody::TurnStarted {
                turn_id: id.clone(),
                source: "engine".into(),
                text: None,
            },
            None,
        );
        id
    }

    pub(crate) fn send(&mut self, text: String, busy: Busy) -> Reply {
        self.send_as(text, busy, "send")
    }

    /// A send's caller offers its proxy environment. Nothing changes unless
    /// the send is accepted (see `adopt_offered_env`).
    pub(crate) fn offer_env(&mut self, env: Option<HashMap<String, String>>) {
        self.offered_env = env
            .filter(|_| self.refresh_env)
            .map(super::only_proxy_vars)
            .filter(|e| !e.is_empty());
    }

    /// An accepted send's environment becomes the one wanted. The same as
    /// the engine's clears any older wish: the caller is what counts now.
    fn adopt_offered_env(&mut self) {
        if let Some(env) = self.offered_env.take() {
            self.pending_env = (env != self.engine_env).then_some(env);
        }
    }

    /// The restart the loop should do before writing the rest of the outbox.
    pub(crate) fn take_restart(&mut self) -> Option<(usize, HashMap<String, String>)> {
        self.restart.take()
    }

    /// The engine now runs with `env`.
    pub(crate) fn restarted(&mut self, env: HashMap<String, String>) {
        self.engine_env = env;
        self.emit(
            EventBody::EngineRestarted {
                reason: "proxy environment changed".into(),
            },
            None,
        );
    }

    /// The restart failed; the old engine carries on, and the next turn
    /// tries again.
    pub(crate) fn restart_failed(&mut self, env: HashMap<String, String>, error: String) {
        if self.pending_env.is_none() {
            self.pending_env = Some(env);
        }
        self.emit(EventBody::EngineRestartFailed { error }, None);
    }

    /// The engine's program and arguments, resuming its conversation.
    pub(crate) fn engine_command(&self, model: Option<String>) -> (String, Vec<String>) {
        self.engine.command(&StartOptions {
            model,
            resume: self.engine_session_id.clone(),
        })
    }

    fn send_as(&mut self, text: String, busy: Busy, source: &'static str) -> Reply {
        self.next_turn += 1;
        let turn_id = format!("t{}", self.next_turn);
        let running = self.current.clone();
        match (running, busy) {
            (None, _) => {
                self.adopt_offered_env();
                self.start_turn(turn_id.clone(), text, source)
            }
            (Some(t), Busy::Reject) => {
                self.offered_env = None;
                self.next_turn -= 1;
                return Reply::err(
                    "busy",
                    format!(
                        "turn {t} is still running. Wait for it (`session wait`), or send with \
                         --queue to run after it or --interrupt to stop it."
                    ),
                );
            }
            (Some(_), Busy::Queue) => {
                self.adopt_offered_env();
                self.queue.push_back(Queued {
                    turn_id: turn_id.clone(),
                    text,
                    source,
                })
            }
            (Some(_), Busy::Interrupt) => {
                self.adopt_offered_env();
                self.queue.push_front(Queued {
                    turn_id: turn_id.clone(),
                    text,
                    source,
                });
                self.write(Input::Interrupt);
            }
        }
        Reply {
            turn_id: Some(turn_id),
            ..Reply::ok()
        }
    }

    pub(crate) fn approve(
        &mut self,
        request_id: &str,
        allow: bool,
        message: Option<String>,
    ) -> Reply {
        let Some(pending) = self.approvals.remove(request_id) else {
            return Reply::err("unknown", format!("no pending approval {request_id}"));
        };
        self.resolve(
            request_id.to_string(),
            pending.input,
            allow,
            message,
            "client",
        );
        Reply::ok()
    }

    fn resolve(
        &mut self,
        request_id: String,
        input: Value,
        allow: bool,
        message: Option<String>,
        by: &str,
    ) {
        self.write(Input::Approval {
            request_id: request_id.clone(),
            allow,
            tool_input: input,
            message,
        });
        self.emit(
            EventBody::ApprovalResolved {
                request_id,
                allow,
                by: by.into(),
            },
            None,
        );
    }

    /// Deny every approval older than `timeout`.
    pub(crate) fn expire_approvals(&mut self, timeout: Duration) {
        let expired: Vec<String> = self
            .approvals
            .iter()
            .filter(|(_, p)| p.asked_at.elapsed() >= timeout)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(p) = self.approvals.remove(&id) {
                let msg = format!(
                    "No answer to this approval within {}; denied.",
                    super::cli::describe(timeout)
                );
                self.resolve(id, p.input, false, Some(msg), "timeout");
            }
        }
    }

    pub(crate) fn cancel(&mut self) -> Reply {
        if self.current.is_none() && self.engine_turn.is_none() {
            return Reply::err("idle", "no turn is running");
        }
        self.write(Input::Interrupt);
        Reply::ok()
    }

    pub(crate) fn status(&self) -> Value {
        serde_json::json!({
            "current": self.current,
            "engine_turn": self.engine_turn,
            "queued": self.queue.iter().map(|q| &q.turn_id).collect::<Vec<_>>(),
            "pending_approvals": self.approvals.iter().map(|(id, p)| {
                serde_json::json!({"request_id": id, "turn_id": p.turn_id})
            }).collect::<Vec<_>>(),
            "engine_session_id": self.engine_session_id,
            "seq": self.seq(),
        })
    }

    /// What one line of engine output means for the session.
    pub(crate) fn on_engine_line(&mut self, raw: Value) {
        let outputs = self.engine.decode(&raw);
        let mut raw = Some(raw);
        for output in outputs {
            self.on_output(output, &mut raw);
        }
    }

    fn on_output(&mut self, output: Output, raw: &mut Option<Value>) {
        match output {
            Output::Ready {
                engine_session_id,
                model,
            } => {
                // Claude repeats this at every turn; only a change is news.
                if self.engine_session_id.as_deref() != Some(&engine_session_id) {
                    self.engine_session_id = Some(engine_session_id.clone());
                    self.emit(
                        EventBody::EngineReady {
                            engine_session_id,
                            model,
                        },
                        None,
                    );
                }
            }
            Output::Text(text) => {
                let turn_id = self.attribute();
                self.emit(EventBody::Text { turn_id, text }, raw.take());
            }
            Output::ToolCall { id, name, input } => {
                let turn_id = self.attribute();
                self.emit(
                    EventBody::ToolCall {
                        turn_id,
                        id,
                        name,
                        input,
                    },
                    raw.take(),
                );
            }
            Output::ToolResult {
                id,
                is_error,
                content,
            } => {
                let turn_id = self.attribute();
                self.emit(
                    EventBody::ToolResult {
                        turn_id,
                        id,
                        is_error,
                        content,
                    },
                    raw.take(),
                );
            }
            Output::ApprovalRequested {
                request_id,
                tool,
                input,
            } => {
                let turn_id = self.attribute();
                self.emit(
                    EventBody::ApprovalRequested {
                        turn_id: turn_id.clone(),
                        request_id: request_id.clone(),
                        tool,
                        input: input.clone(),
                    },
                    raw.take(),
                );
                match self.policy {
                    ApprovalPolicy::Allow => self.resolve(request_id, input, true, None, "policy"),
                    ApprovalPolicy::Deny => self.resolve(
                        request_id,
                        input,
                        false,
                        Some("Denied by this session's approval policy.".into()),
                        "policy",
                    ),
                    ApprovalPolicy::Ask => {
                        self.approvals.insert(
                            request_id,
                            PendingApproval {
                                turn_id,
                                input,
                                asked_at: Instant::now(),
                            },
                        );
                    }
                }
            }
            Output::TurnDone {
                result,
                is_error,
                reason,
                engine_initiated,
            } => {
                let turn_id = if engine_initiated {
                    self.engine_turn
                        .take()
                        .unwrap_or_else(|| self.attribute_done())
                } else {
                    match self.current.take() {
                        Some(t) => t,
                        None => self
                            .engine_turn
                            .take()
                            .unwrap_or_else(|| self.attribute_done()),
                    }
                };
                // Approvals still open belong to a turn that is over.
                self.approvals.retain(|_, p| p.turn_id != turn_id);
                if let Some(message) = self.bus_turns.remove(&turn_id) {
                    self.bus_answers.push((message, result.clone(), is_error));
                }
                self.emit(
                    EventBody::TurnDone {
                        turn_id,
                        result,
                        is_error,
                        reason,
                    },
                    raw.take(),
                );
                if self.current.is_none()
                    && let Some(next) = self.queue.pop_front()
                {
                    self.start_turn(next.turn_id, next.text, next.source);
                }
            }
            Output::Other => {
                self.emit(EventBody::EngineEvent {}, raw.take());
            }
        }
    }

    /// A result for a turn nothing announced: open and close it as one.
    fn attribute_done(&mut self) -> String {
        let id = self.attribute();
        self.engine_turn = None;
        id
    }

    fn subscribe(&mut self, since: u64) -> Stream {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers.push(tx);
        Stream {
            backlog: self.backlog(since),
            live: rx,
        }
    }

    fn handle(&mut self, request: Request) -> (Reply, Option<Stream>) {
        match request {
            Request::Send {
                text,
                busy,
                follow,
                env,
            } => {
                let before = self.seq();
                let stream = follow.then(|| self.subscribe(before));
                self.offer_env(env);
                let mut reply = self.send(text, busy);
                self.offered_env = None;
                if !reply.ok {
                    return (reply, None);
                }
                reply.seq = Some(before);
                (reply, stream)
            }
            Request::Subscribe { since } => {
                let reply = Reply {
                    seq: Some(self.seq()),
                    ..Reply::ok()
                };
                (reply, Some(self.subscribe(since)))
            }
            Request::Approve {
                request_id,
                allow,
                message,
            } => (self.approve(&request_id, allow, message), None),
            Request::Cancel => (self.cancel(), None),
            Request::Status => (
                Reply {
                    status: Some(self.status()),
                    ..Reply::ok()
                },
                None,
            ),
            // Handled by the loop, which owns the process.
            Request::Close => (Reply::ok(), None),
        }
    }
}

/// What the engine is told when a bus message arrives: who it is from, and
/// that answering is automatic, so it does not go looking for a reply command.
pub(crate) fn bus_turn_text(message: &crate::message::Envelope) -> String {
    let from = message.from.display_name();
    let body = if message.message.is_empty() {
        message.request.clone().unwrap_or_default()
    } else {
        message.message.clone()
    };
    if message.requires_reply() {
        format!(
            "Message from {from}, another agent, over the sidekar bus. Your reply to this \
             message is sent back to them automatically; do not send it yourself.\n\n{body}"
        )
    } else {
        format!(
            "Note from {from}, another agent, over the sidekar bus. No reply is needed.\n\n{body}"
        )
    }
}

/// Sidekar's reminders about an unanswered request. A session answers every
/// request by finishing its turn, so a reminder would only start a pointless one.
const NUDGE_PREFIX: &str = "[sidekar] You have an unanswered request";

/// Take this session's bus mail and turn it into turns.
fn poll_bus(me: &str, state: &mut State) {
    let Ok(queued) = crate::broker::list_queued_messages(me) else {
        return;
    };
    for row in queued {
        let Ok(Some(row)) = crate::broker::claim_queued_message(row.id, me) else {
            continue;
        };
        let _ = crate::broker::mark_message_delivered(row.id);
        if row.body.starts_with(NUDGE_PREFIX) {
            continue;
        }
        let message = row.envelope.unwrap_or_else(|| {
            crate::message::Envelope::new_fyi(
                crate::message::AgentId::new(&row.sender),
                me,
                row.body.clone(),
            )
        });
        state.send_from_bus(message);
    }
}

/// Send a finished bus turn's result back as the reply to the message that
/// started it, recorded against its id so `bus await` finds it.
fn answer_on_bus(
    me: &crate::message::AgentId,
    message: &crate::message::Envelope,
    result: &str,
    is_error: bool,
) {
    if !message.requires_reply() {
        return;
    }
    let text = if is_error {
        format!("[turn failed] {result}")
    } else {
        result.to_string()
    };
    let reply = crate::message::Envelope::new_response(
        me.clone(),
        message.from.name.clone(),
        text,
        message.id.clone(),
    );
    let _ = crate::broker::enqueue_bus_message(
        &message.from.name,
        &me.name,
        &reply.format_for_paste(),
        true,
        Some(&reply),
    );
    let _ = crate::broker::record_reply(&message.id, &reply);
}

/// A running engine: the process and its pipes.
struct EngineProcess {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
}

/// Start the engine. With `proxy_env`, the engine's proxy variables are
/// exactly those — set when present, removed when absent, so a proxy the
/// caller no longer uses is not left behind from the host's own start.
fn spawn_engine(
    program: &str,
    args: &[String],
    cwd: &str,
    proxy_env: Option<&HashMap<String, String>>,
    log: &std::fs::File,
) -> Result<EngineProcess> {
    let mut command = tokio::process::Command::new(program);
    command.args(args).current_dir(cwd);
    if let Some(env) = proxy_env {
        for var in super::PROXY_ENV_VARS {
            match env.get(*var) {
                Some(value) => command.env(var, value),
                None => command.env_remove(var),
            };
        }
    }
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(log.try_clone()?))
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start {program}"))?;
    let stdin = child.stdin.take().context("engine stdin")?;
    let stdout = child.stdout.take().context("engine stdout")?;
    let lines = BufReader::new(stdout).lines();
    Ok(EngineProcess {
        child,
        stdin,
        lines,
    })
}

async fn write_lines(stdin: &mut tokio::process::ChildStdin, lines: &[String], closing: bool) {
    if closing {
        return;
    }
    for line in lines {
        let _ = stdin.write_all(line.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.flush().await;
    }
}

/// Run the session named `name`, whose meta the CLI has already written.
pub async fn run(name: &str) -> Result<()> {
    let mut meta = super::read_meta(name)?;
    meta.pid = std::process::id() as i32;
    super::ensure_private_dir(&super::dir_of(name))?;
    let socket = super::socket_path(name);
    let _ = std::fs::remove_file(&socket);

    let engine = engine::for_name(&meta.engine)?;
    let (program, args) = engine.command(&StartOptions {
        model: meta.model.clone(),
        resume: meta.engine_session_id.clone(),
    });
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(super::log_path(name))?;
    // The engine starts with the host's environment; when the session
    // refreshes its env, later sends carry fresher values.
    let initial_env = if meta.refresh_env {
        super::proxy_env()
    } else {
        HashMap::new()
    };
    let mut engine_proc = spawn_engine(&program, &args, &meta.cwd, None, &log)?;
    meta.engine_pid = engine_proc.child.id().map_or(0, |p| p as i32);

    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("failed to listen on {}", socket.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    }

    let mut state = State::new(engine, meta.approvals, meta.refresh_env);
    state.engine_session_id = meta.engine_session_id.clone();
    state.engine_env = initial_env;
    state.sink = Some(
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(super::events_path(name))?,
    );
    state.log = Some(super::events_path(name));
    state.emit(
        EventBody::SessionStarted {
            engine: meta.engine.clone(),
        },
        None,
    );
    meta.status = Status::Running;
    super::write_meta(&meta)?;

    // On the bus under the session's own name, so `bus send <name>` reaches
    // it. The pane carries the host pid, which is how the daemon's sweep
    // settles this session's mail if the host dies without leaving.
    let pane = format!("session-{}", meta.pid);
    let mut presence =
        crate::bus::presence::Presence::register(crate::bus::presence::Registration {
            name: meta.name.clone(),
            nick: meta.name.clone(),
            channel: meta.cwd.clone(),
            pane: pane.clone(),
            agent_type: "session",
            history: None,
        })
        .map_err(|e| eprintln!("[host] not on the bus: {e:#}"))
        .ok();
    let me = crate::message::AgentId {
        name: meta.name.clone(),
        nick: Some(meta.name.clone()),
        session: Some(meta.cwd.clone()),
        pane: Some(pane),
        agent_type: Some("session".into()),
    };
    let mut bus_tick = tokio::time::interval(Duration::from_millis(500));

    let (calls_tx, mut calls_rx) = mpsc::channel::<Call>(64);
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut closing: Option<Instant> = None;

    let exit_code = loop {
        tokio::select! {
            line = engine_proc.lines.next_line() => match line {
                Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                    Ok(raw) => {
                        state.on_engine_line(raw);
                        record_engine_id(&mut meta, &state);
                    }
                    Err(_) => eprintln!("[host] engine wrote a non-JSON line: {line}"),
                },
                // Output closed: the engine is exiting; the wait below says how.
                _ => {
                    let status = engine_proc.child.wait().await.ok();
                    break status.and_then(|s| s.code());
                }
            },
            accepted = listener.accept() => {
                if let Ok((conn, _)) = accepted {
                    tokio::spawn(serve(conn, calls_tx.clone()));
                }
            }
            Some(call) = calls_rx.recv() => {
                if matches!(call.request, Request::Close) {
                    let _ = call.reply.send((Reply::ok(), None));
                    closing.get_or_insert_with(Instant::now);
                    stop_engine(&meta);
                } else {
                    let answer = state.handle(call.request);
                    let _ = call.reply.send(answer);
                }
            }
            _ = bus_tick.tick(), if presence.is_some() && closing.is_none() => {
                poll_bus(&meta.name, &mut state);
            }
            _ = tick.tick() => {
                state.expire_approvals(APPROVAL_TIMEOUT);
                if closing.is_some_and(|t| t.elapsed() >= CLOSE_GRACE) {
                    let _ = engine_proc.child.start_kill();
                }
            }
            _ = sigterm.recv() => {
                closing.get_or_insert_with(Instant::now);
                stop_engine(&meta);
            }
        }
        for (message, result, is_error) in state.take_bus_answers() {
            answer_on_bus(&me, &message, &result, is_error);
        }
        let outbox = state.take_outbox();
        let restart = state.take_restart().filter(|_| closing.is_none());
        let split = restart
            .as_ref()
            .map_or(outbox.len(), |(at, _)| (*at).min(outbox.len()));
        write_lines(&mut engine_proc.stdin, &outbox[..split], closing.is_some()).await;
        if let Some((_, env)) = restart {
            let (program, args) = state.engine_command(meta.model.clone());
            match spawn_engine(&program, &args, &meta.cwd, Some(&env), &log) {
                Ok(fresh) => {
                    // Nothing is running on the old engine: it is stopped
                    // only at the start of a turn, before the turn's input.
                    let mut old = std::mem::replace(&mut engine_proc, fresh);
                    let _ = old.child.start_kill();
                    meta.engine_pid = engine_proc.child.id().map_or(0, |p| p as i32);
                    let _ = super::write_meta(&meta);
                    state.restarted(env);
                }
                Err(e) => {
                    eprintln!(
                        "[host] engine restart for fresh env failed: {e:#}; keeping the old engine"
                    );
                    state.restart_failed(env, format!("{e:#}"));
                }
            }
        }
        write_lines(&mut engine_proc.stdin, &outbox[split..], closing.is_some()).await;
    };

    state.emit(EventBody::SessionEnded { exit_code }, None);
    // Before the name is given up: whatever is still addressed to it is
    // settled and its senders told, not handed to the name's next owner.
    if let Some(p) = presence.as_mut() {
        p.leave();
    }
    meta.status = Status::Ended;
    meta.ended_at = Some(crate::message::epoch_secs());
    meta.exit_code = exit_code;
    super::write_meta(&meta)?;
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// Ask the engine to exit. Closing its input is not enough: Claude in
/// stream-json mode ignores end of input and keeps running. SIGTERM lets it
/// finish writing its transcript; the loop kills it if that takes too long.
fn stop_engine(meta: &Meta) {
    if meta.engine_pid > 0 {
        unsafe { libc::kill(meta.engine_pid, libc::SIGTERM) };
    }
}

/// Persist the engine's conversation id as soon as it is known, so a host
/// that dies can be resumed.
fn record_engine_id(meta: &mut Meta, state: &State) {
    if state.engine_session_id.is_some() && meta.engine_session_id != state.engine_session_id {
        meta.engine_session_id = state.engine_session_id.clone();
        let _ = super::write_meta(meta);
    }
}

/// One client connection: requests in, replies out, and events once it
/// follows. Only the user who owns the session may connect — a client can
/// approve tool calls, which is running code as that user.
async fn serve(conn: UnixStream, calls: mpsc::Sender<Call>) {
    let me = unsafe { libc::getuid() };
    if conn.peer_cred().map(|c| c.uid()).ok() != Some(me) {
        return;
    }
    let (read, mut write) = conn.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let request: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = send_line(&mut write, &Reply::err("bad_request", e.to_string())).await;
                continue;
            }
        };
        let (tx, rx) = oneshot::channel();
        if calls.send(Call { request, reply: tx }).await.is_err() {
            return;
        }
        let Ok((reply, stream)) = rx.await else {
            return;
        };
        if send_line(&mut write, &reply).await.is_err() {
            return;
        }
        if let Some(mut stream) = stream {
            for event in stream.backlog.drain(..) {
                if send_line(&mut write, &event).await.is_err() {
                    return;
                }
            }
            while let Some(event) = stream.live.recv().await {
                if send_line(&mut write, &event).await.is_err() {
                    return;
                }
            }
            return;
        }
    }
}

async fn send_line<T: serde::Serialize>(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    value: &T,
) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value).unwrap_or_default();
    line.push(b'\n');
    write.write_all(&line).await
}

#[cfg(test)]
mod tests;
