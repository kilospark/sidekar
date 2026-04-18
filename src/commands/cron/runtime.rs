use super::schedule::CronSchedule;
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub(super) enum CronAction {
    Tool {
        tool: String,
        #[serde(default)]
        args: Value,
    },
    Batch {
        batch: Vec<Value>,
    },
    Bash {
        command: String,
    },
    Prompt {
        prompt: String,
    },
}

pub(super) struct CronJob {
    pub(super) id: String,
    pub(super) schedule: CronSchedule,
    pub(super) action: CronAction,
    pub(super) target: String,
    pub(super) last_run_at: Option<u64>,
    pub(super) last_finished_at: Option<u64>,
    pub(super) once: bool,
    pub(super) loop_interval_secs: Option<u64>,
    pub(super) running: Arc<AtomicBool>,
}

pub(crate) struct CronState {
    pub(super) running: Arc<AtomicBool>,
    pub(super) jobs: Arc<Mutex<Vec<CronJob>>>,
    pub(super) task_handle: tokio::task::JoinHandle<()>,
}

impl Drop for CronState {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.task_handle.abort();
    }
}

static CRON: tokio::sync::OnceCell<Mutex<Option<CronState>>> = tokio::sync::OnceCell::const_new();
static SHARED_CRON_CTX: tokio::sync::Mutex<Option<CronContext>> =
    tokio::sync::Mutex::const_new(None);

pub(super) async fn cron_cell() -> &'static Mutex<Option<CronState>> {
    CRON.get_or_init(|| async { Mutex::new(None) }).await
}

pub(crate) async fn start_default_cron_loop(agent_name: String, project: String) {
    let ctx = CronContext {
        cdp_port: crate::DEFAULT_CDP_PORT,
        cdp_host: crate::DEFAULT_CDP_HOST.to_string(),
        current_session_id: None,
        current_profile: "default".to_string(),
        headless: false,
        agent_name: Some(agent_name),
        project,
    };
    start_cron_loop(ctx).await;
}

/// Enough state to create an AppContext for cron tool execution.
#[derive(Clone)]
pub(crate) struct CronContext {
    pub cdp_port: u16,
    pub cdp_host: String,
    pub current_session_id: Option<String>,
    pub current_profile: String,
    pub headless: bool,
    pub agent_name: Option<String>,
    pub project: String,
}

impl CronContext {
    fn to_app_context(&self) -> Result<AppContext> {
        let mut ctx = AppContext::new()?;
        ctx.cdp_port = self.cdp_port;
        ctx.cdp_host = self.cdp_host.clone();
        ctx.current_session_id = self.current_session_id.clone();
        ctx.current_profile = self.current_profile.clone();
        ctx.headless = self.headless;
        ctx.isolated = true;
        ctx.agent_name = self.agent_name.clone();
        Ok(ctx)
    }
}

/// Update the cron context (e.g. after browser auto-launch sets cdp_port/session).
pub(crate) async fn update_cron_context(new_ctx: CronContext) {
    *SHARED_CRON_CTX.lock().await = Some(new_ctx);
}

/// Start the cron background loop (idempotent — restarts if already running).
/// Loads persisted jobs from the broker on startup.
pub(crate) async fn start_cron_loop(cron_ctx: CronContext) {
    let cell = cron_cell().await;
    let mut guard = cell.lock().await;

    if guard
        .as_ref()
        .is_some_and(|s| s.running.load(Ordering::Relaxed))
    {
        return;
    }

    let running = Arc::new(AtomicBool::new(true));
    let jobs = Arc::new(Mutex::new(Vec::new()));

    if let Ok(records) =
        broker::list_cron_jobs(true, crate::scope::ScopeView::Project, &cron_ctx.project)
    {
        let mut loaded = jobs.lock().await;
        for rec in records {
            if !job_belongs_to_agent(&rec.target, &rec.created_by, cron_ctx.agent_name.as_deref()) {
                continue;
            }
            if let Ok(sched) = CronSchedule::parse(&rec.schedule) {
                let action: CronAction = match serde_json::from_str(&rec.action_json) {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                loaded.push(CronJob {
                    id: rec.id,
                    schedule: sched,
                    action,
                    target: normalize_loaded_target(&rec.target, &rec.created_by),
                    last_run_at: rec.last_run_at,
                    last_finished_at: None,
                    once: rec.once,
                    loop_interval_secs: rec.loop_interval_secs,
                    running: Arc::new(AtomicBool::new(false)),
                });
            }
        }
    }

    let r = running.clone();
    let j = jobs.clone();
    let task_handle = tokio::spawn(cron_loop_supervisor(r.clone(), j, cron_ctx));

    *guard = Some(CronState {
        running,
        jobs,
        task_handle,
    });
}

/// Supervisor that restarts the cron loop if it panics.
async fn cron_loop_supervisor(
    running: Arc<AtomicBool>,
    jobs: Arc<Mutex<Vec<CronJob>>>,
    cron_ctx: CronContext,
) {
    loop {
        let r = running.clone();
        let j = jobs.clone();
        let ctx = cron_ctx.clone();

        match tokio::spawn(cron_loop(r, j, ctx)).await {
            Ok(()) => break,
            Err(_) => {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                if !running.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
}

async fn cron_loop(
    running: Arc<AtomicBool>,
    jobs: Arc<Mutex<Vec<CronJob>>>,
    cron_ctx: CronContext,
) {
    let now_secs = epoch_now();
    let secs_into_minute = now_secs % 60;
    let wait_until_next_min = if secs_into_minute == 0 {
        60
    } else {
        60 - secs_into_minute
    };
    tokio::time::sleep(Duration::from_secs(wait_until_next_min)).await;

    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let owner_name = cron_ctx.agent_name.clone();

    loop {
        interval.tick().await;

        if !running.load(Ordering::Relaxed) {
            break;
        }

        if let Ok(records) =
            broker::list_cron_jobs(true, crate::scope::ScopeView::Project, &cron_ctx.project)
        {
            let mut mem_jobs = jobs.lock().await;
            let mut broker_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for rec in &records {
                if !job_belongs_to_agent(&rec.target, &rec.created_by, owner_name.as_deref()) {
                    continue;
                }
                broker_ids.insert(rec.id.clone());
            }

            for rec in records {
                if !job_belongs_to_agent(&rec.target, &rec.created_by, owner_name.as_deref()) {
                    continue;
                }
                if mem_jobs.iter().any(|j| j.id == rec.id) {
                    continue;
                }
                match CronSchedule::parse(&rec.schedule) {
                    Ok(sched) => {
                        let action: CronAction = match serde_json::from_str(&rec.action_json) {
                            Ok(a) => a,
                            Err(_) => continue,
                        };
                        mem_jobs.push(CronJob {
                            id: rec.id,
                            schedule: sched,
                            action,
                            target: normalize_loaded_target(&rec.target, &rec.created_by),
                            last_run_at: rec.last_run_at,
                            last_finished_at: None,
                            once: rec.once,
                            loop_interval_secs: rec.loop_interval_secs,
                            running: Arc::new(AtomicBool::new(false)),
                        });
                    }
                    Err(_) => continue,
                }
            }
            mem_jobs.retain(|j| broker_ids.contains(&j.id));
        }

        let (min, hour, dom, month, dow) = local_time_components();
        let now = epoch_now();

        let jobs_guard = jobs.lock().await;
        let mut to_execute: Vec<(String, CronAction, String, bool, Arc<AtomicBool>)> = Vec::new();

        for j in jobs_guard.iter() {
            if j.running.load(Ordering::Relaxed) {
                continue;
            }
            if let Some(interval) = j.loop_interval_secs {
                let since = j.last_finished_at.unwrap_or(0);
                if now.saturating_sub(since) >= interval {
                    to_execute.push((
                        j.id.clone(),
                        j.action.clone(),
                        j.target.clone(),
                        j.once,
                        j.running.clone(),
                    ));
                }
            } else if j.schedule.matches(min, hour, dom, month, dow) {
                to_execute.push((
                    j.id.clone(),
                    j.action.clone(),
                    j.target.clone(),
                    j.once,
                    j.running.clone(),
                ));
            }
        }
        drop(jobs_guard);

        let once_ids: Vec<String> = to_execute
            .iter()
            .filter(|(_, _, _, is_once, _)| *is_once)
            .map(|(id, _, _, _, _)| id.clone())
            .collect();

        let jobs_for_update = jobs.clone();
        for (job_id, action, target, is_once, job_running) in to_execute {
            let ctx_clone = cron_ctx.clone();
            let jid = job_id.clone();
            let jobs_ref = jobs_for_update.clone();
            job_running.store(true, Ordering::Relaxed);

            tokio::spawn(async move {
                let result = execute_cron_job(&ctx_clone, &jid, &action, &target).await;
                job_running.store(false, Ordering::Relaxed);

                let finished_at = epoch_now();
                {
                    let mut guard = jobs_ref.lock().await;
                    if let Some(job) = guard.iter_mut().find(|j| j.id == jid) {
                        job.last_finished_at = Some(finished_at);
                    }
                }

                match &result {
                    Ok(_) => {
                        let _ = broker::update_cron_job_run(&jid, None);
                    }
                    Err(e) => {
                        let err_msg = format!("{e:#}");
                        let _ = broker::update_cron_job_run(&jid, Some(&err_msg));
                    }
                }

                if is_once {
                    let _ = broker::delete_cron_job(&jid);
                }
            });
        }

        if !once_ids.is_empty() {
            let mut jobs_guard = jobs.lock().await;
            jobs_guard.retain(|j| !once_ids.contains(&j.id));
        }

        let mut jobs_guard = jobs.lock().await;
        let now = epoch_now();
        for job in jobs_guard.iter_mut() {
            if job.schedule.matches(min, hour, dom, month, dow) {
                job.last_run_at = Some(now);
            }
        }
    }
}

async fn execute_cron_job(
    fallback_ctx: &CronContext,
    job_id: &str,
    action: &CronAction,
    target: &str,
) -> Result<()> {
    let cron_ctx = SHARED_CRON_CTX
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| fallback_ctx.clone());

    let _cron_guard = crate::runtime::enter_cron_action();
    let inherited_agent_name = cron_ctx.agent_name.clone();
    let inherited_channel = inherited_agent_name.as_deref().and_then(|name| {
        crate::broker::find_agent(name, None)
            .ok()
            .flatten()
            .and_then(|agent| agent.id.session)
    });

    crate::commands::monitor::mark_tool_action();

    let timeout = Duration::from_secs(crate::config::load_config().cdp_timeout_secs);

    let output = match action {
        CronAction::Tool { tool, args } => {
            let cli_args = map_cron_action_args(tool, args);
            let mut ctx = cron_ctx.to_app_context()?;
            ctx.isolated = true;
            let result = tokio::time::timeout(
                timeout,
                crate::commands::dispatch(&mut ctx, tool, &cli_args),
            )
            .await;
            match result {
                Ok(Ok(())) => ctx.drain_output(),
                Ok(Err(e)) => return Err(e),
                Err(_) => bail!("Cron job timed out after {}s", timeout.as_secs()),
            }
        }
        CronAction::Batch { batch } => {
            let batch_input = json!({ "actions": batch });
            let batch_json = serde_json::to_string(&batch_input)?;
            let mut ctx = cron_ctx.to_app_context()?;
            ctx.isolated = true;
            let result = tokio::time::timeout(
                timeout,
                crate::commands::dispatch(&mut ctx, "batch", &[batch_json]),
            )
            .await;
            match result {
                Ok(Ok(())) => ctx.drain_output(),
                Ok(Err(e)) => return Err(e),
                Err(_) => bail!("Cron batch timed out after {}s", timeout.as_secs()),
            }
        }
        CronAction::Bash { command } => {
            let mut bash = tokio::process::Command::new("sh");
            bash.arg("-c").arg(command);
            bash.env("SIDEKAR_CRON_DEPTH", "1");
            if let Some(ref name) = inherited_agent_name {
                bash.env("SIDEKAR_AGENT_NAME", name);
            }
            if let Some(ref channel) = inherited_channel {
                bash.env("SIDEKAR_CHANNEL", channel);
            }
            let result = tokio::time::timeout(timeout, bash.output()).await;
            match result {
                Ok(Ok(output)) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let mut combined = stdout.to_string();
                    if !stderr.is_empty() {
                        if !combined.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str("[stderr]: ");
                        combined.push_str(&stderr);
                    }
                    if !output.status.success() {
                        combined.push_str(&format!(
                            "\n[exit code: {}]",
                            output.status.code().unwrap_or(-1)
                        ));
                    }
                    combined
                }
                Ok(Err(e)) => return Err(anyhow::anyhow!("Failed to run bash command: {e}")),
                Err(_) => bail!("Cron bash command timed out after {}s", timeout.as_secs()),
            }
        }
        CronAction::Prompt { prompt } => prompt.clone(),
    };

    if !output.trim().is_empty() {
        let msg = match action {
            CronAction::Prompt { .. } => output.trim().to_string(),
            _ => format!("[from sidekar-cron]: [cron {job_id}]: {}", output.trim()),
        };
        let _ = crate::broker::enqueue_message("sidekar-cron", target, &msg);
    }

    Ok(())
}

pub(super) fn normalize_cron_target(target: &str, created_by: &str) -> Result<String> {
    let trimmed = target.trim();
    if trimmed.is_empty() || trimmed == "self" {
        if created_by == "cli" {
            bail!(
                "Cron target `self` requires a Sidekar agent context. Run inside `sidekar <agent>` or pass --target=<agent-name>."
            );
        }
        return Ok(created_by.to_string());
    }

    if let Ok(Some(agent)) = crate::broker::find_agent(trimmed, None) {
        return Ok(agent.id.name);
    }

    Ok(trimmed.to_string())
}

pub(super) fn normalize_loaded_target(target: &str, created_by: &str) -> String {
    let trimmed = target.trim();
    if trimmed.is_empty() || trimmed == "self" {
        created_by.to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn job_belongs_to_agent(
    target: &str,
    created_by: &str,
    agent_name: Option<&str>,
) -> bool {
    let Some(agent_name) = agent_name else {
        return false;
    };
    let _ = target;
    created_by == agent_name
}

/// Map cron action args (JSON object) to CLI arg vector for dispatch.
fn map_cron_action_args(tool: &str, args: &Value) -> Vec<String> {
    let mut cli_args = Vec::new();

    if let Some(obj) = args.as_object() {
        for (key, value) in obj {
            match key.as_str() {
                "url" | "query" | "selector" | "target" | "text" | "key" | "expression" => {
                    if let Some(s) = value.as_str() {
                        cli_args.push(s.to_string());
                    }
                }
                "full" if value.as_bool() == Some(true) => {
                    cli_args.push("--full".to_string());
                }
                "max_tokens" => {
                    if let Some(n) = value.as_u64() {
                        cli_args.push(format!("--tokens={n}"));
                    }
                }
                "format" => {
                    if let Some(s) = value.as_str() {
                        cli_args.push(format!("--format={s}"));
                    }
                }
                "output" => {
                    if let Some(s) = value.as_str() {
                        cli_args.push(format!("--output={s}"));
                    }
                }
                "action" => {
                    if let Some(s) = value.as_str() {
                        cli_args.push(s.to_string());
                    }
                }
                _ => {
                    if let Some(s) = value.as_str() {
                        cli_args.push(format!("--{key}={s}"));
                    }
                }
            }
        }
    } else if let Some(s) = args.as_str() {
        cli_args.push(s.to_string());
    }

    if tool == "navigate"
        && cli_args.is_empty()
        && let Some(url) = args.get("url").and_then(Value::as_str)
    {
        cli_args.push(url.to_string());
    }

    cli_args
}

pub(super) fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Get local time components: (minute, hour, day_of_month, month, day_of_week).
/// day_of_week: 0=Sunday, 6=Saturday.
fn local_time_components() -> (u32, u32, u32, u32, u32) {
    let now = epoch_now() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    (
        tm.tm_min as u32,
        tm.tm_hour as u32,
        tm.tm_mday as u32,
        (tm.tm_mon + 1) as u32,
        tm.tm_wday as u32,
    )
}
