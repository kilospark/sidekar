//! Cron subsystem — schedule recurring sidekar tool calls.
//!
//! Runs as an in-process tokio task (like monitor). Jobs are persisted in the
//! broker SQLite database and restored on startup.

use crate::broker;
use crate::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Cron schedule parsing (minimal 5-field cron)
// ---------------------------------------------------------------------------

/// Parsed cron schedule: minute, hour, day-of-month, month, day-of-week.
/// Each field is a set of allowed values.
#[derive(Debug, Clone)]
struct CronSchedule {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days_of_month: Vec<u32>,
    months: Vec<u32>,
    days_of_week: Vec<u32>, // 0=Sun, 6=Sat
}

impl CronSchedule {
    fn parse(expr: &str) -> Result<Self> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            bail!(
                "Cron expression must have exactly 5 fields (minute hour dom month dow), got {}",
                fields.len()
            );
        }
        Ok(Self {
            minutes: parse_field(fields[0], 0, 59).context("Invalid minute field")?,
            hours: parse_field(fields[1], 0, 23).context("Invalid hour field")?,
            days_of_month: parse_field(fields[2], 1, 31).context("Invalid day-of-month field")?,
            months: parse_field(fields[3], 1, 12).context("Invalid month field")?,
            days_of_week: parse_field(fields[4], 0, 6).context("Invalid day-of-week field")?,
        })
    }

    /// Check if this schedule matches the given time components.
    fn matches(&self, min: u32, hour: u32, dom: u32, month: u32, dow: u32) -> bool {
        self.minutes.contains(&min)
            && self.hours.contains(&hour)
            && self.days_of_month.contains(&dom)
            && self.months.contains(&month)
            && self.days_of_week.contains(&dow)
    }
}

/// Parse a single cron field (e.g. "*/5", "1,15", "1-5", "*").
fn parse_field(field: &str, min: u32, max: u32) -> Result<Vec<u32>> {
    let mut values = Vec::new();

    for part in field.split(',') {
        let part = part.trim();
        if part == "*" {
            return Ok((min..=max).collect());
        }

        // */N — step
        if let Some(step_str) = part.strip_prefix("*/") {
            let step: u32 = step_str.parse().context("Invalid step value")?;
            if step == 0 {
                bail!("Step cannot be 0");
            }
            let mut v = min;
            while v <= max {
                values.push(v);
                v += step;
            }
            continue;
        }

        // N-M or N-M/S — range with optional step
        if part.contains('-') {
            let (range_part, step) = if part.contains('/') {
                let sp: Vec<&str> = part.splitn(2, '/').collect();
                (
                    sp[0],
                    sp[1].parse::<u32>().context("Invalid step in range")?,
                )
            } else {
                (part, 1u32)
            };
            let bounds: Vec<&str> = range_part.splitn(2, '-').collect();
            let lo: u32 = bounds[0].parse().context("Invalid range start")?;
            let hi: u32 = bounds[1].parse().context("Invalid range end")?;
            if lo > hi || lo < min || hi > max {
                bail!("Range {lo}-{hi} out of bounds ({min}-{max})");
            }
            let mut v = lo;
            while v <= hi {
                values.push(v);
                v += step;
            }
            continue;
        }

        // Single value
        let v: u32 = part.parse().context("Invalid number")?;
        if v < min || v > max {
            bail!("Value {v} out of bounds ({min}-{max})");
        }
        values.push(v);
    }

    if values.is_empty() {
        bail!("Empty field");
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

// ---------------------------------------------------------------------------
// Cron action types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum CronAction {
    Tool {
        tool: String,
        #[serde(default)]
        args: Value,
    },
    Batch {
        batch: Vec<Value>,
    },
}

// ---------------------------------------------------------------------------
// In-memory cron job
// ---------------------------------------------------------------------------

struct CronJob {
    id: String,
    name: Option<String>,
    schedule: CronSchedule,
    schedule_expr: String,
    action: CronAction,
    target: String,
    created_by: String,
    last_run_at: Option<u64>,
    running: Arc<AtomicBool>, // true while this job is executing (skip pileup)
}

// ---------------------------------------------------------------------------
// Global cron state
// ---------------------------------------------------------------------------

pub(crate) struct CronState {
    running: Arc<AtomicBool>,
    jobs: Arc<Mutex<Vec<CronJob>>>,
    task_handle: tokio::task::JoinHandle<()>,
}

impl Drop for CronState {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.task_handle.abort();
    }
}

static CRON: tokio::sync::OnceCell<Mutex<Option<CronState>>> = tokio::sync::OnceCell::const_new();

async fn cron_cell() -> &'static Mutex<Option<CronState>> {
    CRON.get_or_init(|| async { Mutex::new(None) }).await
}

// ---------------------------------------------------------------------------
// Cron context — minimal subset of AppContext for executing tools
// ---------------------------------------------------------------------------

/// Enough state to create an AppContext for cron tool execution.
#[derive(Clone)]
pub(crate) struct CronContext {
    pub cdp_port: u16,
    pub cdp_host: String,
    pub current_session_id: Option<String>,
    pub current_profile: String,
    pub headless: bool,
    pub agent_name: Option<String>,
}

impl CronContext {
    fn to_app_context(&self) -> Result<AppContext> {
        let mut ctx = AppContext::new()?;
        ctx.cdp_port = self.cdp_port;
        ctx.cdp_host = self.cdp_host.clone();
        ctx.current_session_id = self.current_session_id.clone();
        ctx.current_profile = self.current_profile.clone();
        ctx.headless = self.headless;
        ctx.isolated = true; // cron runs in isolated context
        Ok(ctx)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Update the cron context (e.g. after browser auto-launch sets cdp_port/session).
pub(crate) async fn update_cron_context(new_ctx: CronContext) {
    // The cron loop captures cron_ctx at start — we can't update it directly.
    // Instead, store a shared context that the loop reads on each execution.
    *SHARED_CRON_CTX.lock().await = Some(new_ctx);
}

static SHARED_CRON_CTX: tokio::sync::Mutex<Option<CronContext>> =
    tokio::sync::Mutex::const_new(None);

/// Start the cron background loop (idempotent — restarts if already running).
/// Loads persisted jobs from the broker on startup.
pub(crate) async fn start_cron_loop(cron_ctx: CronContext) {
    let cell = cron_cell().await;
    let mut guard = cell.lock().await;

    // Already running? Skip.
    if guard
        .as_ref()
        .is_some_and(|s| s.running.load(Ordering::Relaxed))
    {
        return;
    }

    let running = Arc::new(AtomicBool::new(true));
    let jobs = Arc::new(Mutex::new(Vec::new()));

    // Load persisted jobs
    if let Ok(records) = broker::list_cron_jobs(true) {
        let mut loaded = jobs.lock().await;
        for rec in records {
            if !job_belongs_to_agent(&rec.target, &rec.created_by, cron_ctx.agent_name.as_deref()) {
                continue;
            }
            match CronSchedule::parse(&rec.schedule) {
                Ok(sched) => {
                    let action: CronAction = match serde_json::from_str(&rec.action_json) {
                        Ok(a) => a,
                        Err(_) => {
                            continue;
                        }
                    };
                    loaded.push(CronJob {
                        id: rec.id,
                        name: rec.name,
                        schedule: sched,
                        schedule_expr: rec.schedule,
                        action,
                        target: normalize_loaded_target(&rec.target, &rec.created_by),
                        created_by: rec.created_by,
                        last_run_at: rec.last_run_at,
                        running: Arc::new(AtomicBool::new(false)),
                    });
                }
                Err(_) => {}
            }
        }
        // silently restored
    }

    let r = running.clone();
    let j = jobs.clone();
    let task_handle = tokio::spawn(cron_loop(r.clone(), j, cron_ctx));

    *guard = Some(CronState {
        running,
        jobs,
        task_handle,
    });
}

/// Create a new cron job. Returns the job ID.
pub(crate) async fn cmd_cron_create(
    ctx: &mut AppContext,
    schedule_expr: &str,
    action: &Value,
    target: &str,
    name: Option<&str>,
    created_by: &str,
) -> Result<String> {
    // Validate schedule
    let schedule = CronSchedule::parse(schedule_expr)?;

    // Enforce minimum interval: reject schedules that fire every second
    // (we check at minute granularity, so anything valid is >= 1 min)
    // But also reject "* * * * *" — every minute is ok but warn.

    // Validate action
    let action_parsed: CronAction = serde_json::from_value(action.clone())
        .context("Invalid action: must have 'tool'+'args' or 'batch' field")?;

    // Validate tool name if single tool
    if let CronAction::Tool { ref tool, .. } = action_parsed {
        // Block dangerous tools
        if matches!(
            tool.as_str(),
            "cron-create" | "cron-delete" | "kill" | "uninstall"
        ) {
            bail!("Tool '{tool}' cannot be used in cron actions");
        }
    }

    let effective_target = normalize_cron_target(target, created_by)?;

    // Check job limit (use broker as source of truth — works with or without cron loop)
    let max_jobs = crate::config::load_config().max_cron_jobs;
    let current_count = broker::list_cron_jobs(true)
        .map(|jobs| jobs.len())
        .unwrap_or(0);
    if current_count >= max_jobs {
        bail!(
            "Cron job limit reached ({max_jobs}). Delete a job first, or increase max_cron_jobs in config."
        );
    }

    // Generate ID
    let id = format!("{:08x}", rand::random::<u32>());

    // Persist to broker (works from any shell — the running PTY wrapper picks it up)
    let action_json = serde_json::to_string(&action_parsed)?;
    broker::create_cron_job(
        &id,
        name,
        schedule_expr,
        &action_json,
        &effective_target,
        created_by,
    )?;

    // Add to in-memory state if cron loop is running
    let cell = cron_cell().await;
    let guard = cell.lock().await;
    if let Some(state) = guard.as_ref() {
        state.jobs.lock().await.push(CronJob {
            id: id.clone(),
            name: name.map(String::from),
            schedule,
            schedule_expr: schedule_expr.to_string(),
            action: action_parsed,
            target: effective_target.clone(),
            created_by: created_by.to_string(),
            last_run_at: None,
            running: Arc::new(AtomicBool::new(false)),
        });
    }

    out!(ctx, "Cron job created: {id}");
    if let Some(n) = name {
        out!(ctx, "Name: {n}");
    }
    out!(ctx, "Schedule: {schedule_expr}");
    out!(ctx, "Target: {effective_target}");

    Ok(id)
}

/// List all active cron jobs.
pub(crate) async fn cmd_cron_list(ctx: &mut AppContext) -> Result<()> {
    let cell = cron_cell().await;
    let guard = cell.lock().await;

    match guard.as_ref() {
        Some(state) => {
            let jobs = state.jobs.lock().await;
            if jobs.is_empty() {
                out!(ctx, "No active cron jobs.");
                return Ok(());
            }
            for job in jobs.iter() {
                let name_str = job.name.as_deref().unwrap_or("(unnamed)");
                let last_run = job
                    .last_run_at
                    .map(|ts| format!("last run: {}s ago", epoch_now().saturating_sub(ts)))
                    .unwrap_or_else(|| "never run".to_string());
                let running_str = if job.running.load(Ordering::Relaxed) {
                    " [running]"
                } else {
                    ""
                };

                out!(
                    ctx,
                    "[{}] {} — schedule: {} — target: {} — owner: {} — {}{}",
                    job.id,
                    name_str,
                    job.schedule_expr,
                    job.target,
                    job.created_by,
                    last_run,
                    running_str
                );

                // Also show action summary
                match &job.action {
                    CronAction::Tool { tool, args } => {
                        let args_brief = if args.is_null() || args == &json!({}) {
                            String::new()
                        } else {
                            let s = serde_json::to_string(args).unwrap_or_default();
                            if s.len() > 80 {
                                format!(" {}", &s[..80])
                            } else {
                                format!(" {s}")
                            }
                        };
                        out!(ctx, "  action: {tool}{args_brief}");
                    }
                    CronAction::Batch { batch } => {
                        out!(ctx, "  action: batch ({} steps)", batch.len());
                    }
                }
            }
            // Also show stats from broker
            if let Ok(records) = broker::list_cron_jobs(true) {
                for rec in &records {
                    if rec.run_count > 0 || rec.error_count > 0 {
                        let err_str = if rec.error_count > 0 {
                            format!(", {} errors", rec.error_count)
                        } else {
                            String::new()
                        };
                        out!(
                            ctx,
                            "  [{}] stats: {} runs{}",
                            rec.id,
                            rec.run_count,
                            err_str
                        );
                        if let Some(ref e) = rec.last_error {
                            out!(ctx, "  [{}] last error: {}", rec.id, e);
                        }
                    }
                }
            }
        }
        None => {
            // Cron loop not started — check broker for persisted jobs
            if let Ok(records) = broker::list_cron_jobs(true) {
                if records.is_empty() {
                    out!(ctx, "No active cron jobs.");
                } else {
                    out!(
                        ctx,
                        "{} persisted cron job(s) (cron loop not yet started):",
                        records.len()
                    );
                    for rec in &records {
                        let name_str = rec.name.as_deref().unwrap_or("(unnamed)");
                        out!(
                            ctx,
                            "[{}] {} — schedule: {} — target: {} — owner: {} — {} runs",
                            rec.id,
                            name_str,
                            rec.schedule,
                            rec.target,
                            rec.created_by,
                            rec.run_count
                        );
                    }
                }
            } else {
                out!(ctx, "No active cron jobs.");
            }
        }
    }
    Ok(())
}

/// Delete a cron job by ID.
pub(crate) async fn cmd_cron_delete(ctx: &mut AppContext, job_id: &str) -> Result<()> {
    // Remove from in-memory state
    let cell = cron_cell().await;
    let guard = cell.lock().await;
    if let Some(state) = guard.as_ref() {
        let mut jobs = state.jobs.lock().await;
        let before = jobs.len();
        jobs.retain(|j| j.id != job_id);
        if jobs.len() == before {
            // Not found in memory — still try broker
        }
    }

    // Soft-delete in broker
    if broker::delete_cron_job(job_id)? {
        out!(ctx, "Cron job {job_id} deleted.");
    } else {
        bail!("Cron job '{job_id}' not found.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Background cron loop
// ---------------------------------------------------------------------------

async fn cron_loop(
    running: Arc<AtomicBool>,
    jobs: Arc<Mutex<Vec<CronJob>>>,
    cron_ctx: CronContext,
) {
    // cron loop started — no terminal output

    // Align to the next minute boundary so we check exactly on the minute
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

        // Reload jobs from broker to pick up externally-created jobs
        if let Ok(records) = broker::list_cron_jobs(true) {
            let mut mem_jobs = jobs.lock().await;
            for rec in records {
                if !job_belongs_to_agent(&rec.target, &rec.created_by, owner_name.as_deref()) {
                    continue;
                }
                if mem_jobs.iter().any(|j| j.id == rec.id) {
                    continue; // already loaded
                }
                match CronSchedule::parse(&rec.schedule) {
                    Ok(sched) => {
                        let action: CronAction = match serde_json::from_str(&rec.action_json) {
                            Ok(a) => a,
                            Err(_) => continue,
                        };
                        mem_jobs.push(CronJob {
                            id: rec.id,
                            name: rec.name,
                            schedule: sched,
                            schedule_expr: rec.schedule,
                            action,
                            target: normalize_loaded_target(&rec.target, &rec.created_by),
                            created_by: rec.created_by,
                            last_run_at: rec.last_run_at,
                            running: Arc::new(AtomicBool::new(false)),
                        });
                    }
                    Err(_) => continue,
                }
            }
            // Remove jobs that were deleted from broker
            let broker_ids: std::collections::HashSet<String> = broker::list_cron_jobs(true)
                .map(|recs| {
                    recs.into_iter()
                        .filter(|r| {
                            job_belongs_to_agent(&r.target, &r.created_by, owner_name.as_deref())
                        })
                        .map(|r| r.id)
                        .collect()
                })
                .unwrap_or_default();
            mem_jobs.retain(|j| broker_ids.contains(&j.id));
        }

        // Get current local time components
        let (min, hour, dom, month, dow) = local_time_components();

        // Check which jobs match
        let jobs_guard = jobs.lock().await;
        let matching: Vec<(String, CronAction, String, Arc<AtomicBool>)> = jobs_guard
            .iter()
            .filter(|j| j.schedule.matches(min, hour, dom, month, dow))
            .filter(|j| !j.running.load(Ordering::Relaxed)) // skip if still running
            .map(|j| {
                (
                    j.id.clone(),
                    j.action.clone(),
                    j.target.clone(),
                    j.running.clone(),
                )
            })
            .collect();
        drop(jobs_guard);

        // Execute matching jobs
        for (job_id, action, target, job_running) in matching {
            let ctx_clone = cron_ctx.clone();
            let jid = job_id.clone();
            job_running.store(true, Ordering::Relaxed);

            tokio::spawn(async move {
                let result = execute_cron_job(&ctx_clone, &jid, &action, &target).await;
                job_running.store(false, Ordering::Relaxed);

                // Update broker stats
                match &result {
                    Ok(_) => {
                        let _ = broker::update_cron_job_run(&jid, None);
                    }
                    Err(e) => {
                        let err_msg = format!("{e:#}");
                        let _ = broker::update_cron_job_run(&jid, Some(&err_msg));
                    }
                }
            });
        }

        // Update last_run_at in memory
        let mut jobs_guard = jobs.lock().await;
        let now = epoch_now();
        for job in jobs_guard.iter_mut() {
            if job.schedule.matches(min, hour, dom, month, dow) {
                job.last_run_at = Some(now);
            }
        }
    }

    // cron loop stopped
}

/// Execute a single cron job's action and deliver the result.
async fn execute_cron_job(
    fallback_ctx: &CronContext,
    job_id: &str,
    action: &CronAction,
    target: &str,
) -> Result<()> {
    // Use the latest shared context (updated after browser auto-launch), fall back to initial
    let cron_ctx = SHARED_CRON_CTX
        .lock()
        .await
        .clone()
        .unwrap_or_else(|| fallback_ctx.clone());

    // Set agent name so dispatched commands recover the PTY wrapper's bus identity
    // instead of registering a new throwaway agent.
    if let Some(ref name) = cron_ctx.agent_name {
        unsafe { std::env::set_var("SIDEKAR_AGENT_NAME", name) };
        // Also set channel so broker lookup succeeds
        if let Ok(Some(agent)) = crate::broker::find_agent(name, None) {
            if let Some(ref session) = agent.id.session {
                unsafe { std::env::set_var("SIDEKAR_CHANNEL", session) };
            }
        }
    }

    // Mark tool action so monitor doesn't double-notify
    super::monitor::mark_tool_action();

    let timeout = Duration::from_secs(crate::config::load_config().cdp_timeout_secs);

    let output = match action {
        CronAction::Tool { tool, args } => {
            // Build CLI args from the action args
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
    };

    // Deliver result via broker queue to the concrete owning/target agent.
    if !output.trim().is_empty() {
        let msg = format!("[from sidekar-cron]: [cron {job_id}]: {}", output.trim());
        let _ = crate::broker::enqueue_message("sidekar-cron", target, &msg);
    }

    Ok(())
}

fn normalize_cron_target(target: &str, created_by: &str) -> Result<String> {
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

fn normalize_loaded_target(target: &str, created_by: &str) -> String {
    let trimmed = target.trim();
    if trimmed.is_empty() || trimmed == "self" {
        created_by.to_string()
    } else {
        trimmed.to_string()
    }
}

fn job_belongs_to_agent(target: &str, created_by: &str, agent_name: Option<&str>) -> bool {
    let Some(agent_name) = agent_name else {
        return false;
    };
    normalize_loaded_target(target, created_by) == agent_name
}

/// Map cron action args (JSON object) to CLI arg vector for dispatch.
fn map_cron_action_args(tool: &str, args: &Value) -> Vec<String> {
    // Map action args to CLI args
    let mut cli_args = Vec::new();

    if let Some(obj) = args.as_object() {
        // Common pattern: each key-value becomes a positional or flag arg
        for (key, value) in obj {
            match key.as_str() {
                // URL-like args are positional
                "url" | "query" | "selector" | "target" | "text" | "key" | "expression" => {
                    if let Some(s) = value.as_str() {
                        cli_args.push(s.to_string());
                    }
                }
                // Boolean flags
                "full" if value.as_bool() == Some(true) => {
                    cli_args.push("--full".to_string());
                }
                // Numeric/string options
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
                // Action for monitor/storage/etc
                "action" => {
                    if let Some(s) = value.as_str() {
                        cli_args.push(s.to_string());
                    }
                }
                // Selector for type command (selector + text)
                _ => {
                    // Pass unknown args as --key=value
                    if let Some(s) = value.as_str() {
                        cli_args.push(format!("--{key}={s}"));
                    }
                }
            }
        }
    } else if let Some(s) = args.as_str() {
        cli_args.push(s.to_string());
    }

    // Tool-specific: navigate needs the URL as first positional
    if tool == "navigate" && cli_args.is_empty() {
        if let Some(url) = args.get("url").and_then(Value::as_str) {
            cli_args.push(url.to_string());
        }
    }

    cli_args
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Get local time components: (minute, hour, day_of_month, month, day_of_week).
/// day_of_week: 0=Sunday, 6=Saturday.
fn local_time_components() -> (u32, u32, u32, u32, u32) {
    // Use libc localtime to get local timezone components without pulling in chrono
    let now = epoch_now() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    (
        tm.tm_min as u32,
        tm.tm_hour as u32,
        tm.tm_mday as u32,
        (tm.tm_mon + 1) as u32, // tm_mon is 0-based
        tm.tm_wday as u32,      // 0=Sunday
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_star() {
        let vals = parse_field("*", 0, 59).unwrap();
        assert_eq!(vals.len(), 60);
        assert_eq!(vals[0], 0);
        assert_eq!(vals[59], 59);
    }

    #[test]
    fn parse_step() {
        let vals = parse_field("*/15", 0, 59).unwrap();
        assert_eq!(vals, vec![0, 15, 30, 45]);
    }

    #[test]
    fn parse_range() {
        let vals = parse_field("1-5", 0, 59).unwrap();
        assert_eq!(vals, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn parse_range_with_step() {
        let vals = parse_field("0-20/5", 0, 59).unwrap();
        assert_eq!(vals, vec![0, 5, 10, 15, 20]);
    }

    #[test]
    fn parse_list() {
        let vals = parse_field("1,5,10,15", 0, 59).unwrap();
        assert_eq!(vals, vec![1, 5, 10, 15]);
    }

    #[test]
    fn parse_single() {
        let vals = parse_field("30", 0, 59).unwrap();
        assert_eq!(vals, vec![30]);
    }

    #[test]
    fn parse_schedule_every_5_min() {
        let sched = CronSchedule::parse("*/5 * * * *").unwrap();
        assert!(sched.matches(0, 12, 15, 6, 3));
        assert!(sched.matches(5, 12, 15, 6, 3));
        assert!(!sched.matches(3, 12, 15, 6, 3));
    }

    #[test]
    fn parse_schedule_specific() {
        let sched = CronSchedule::parse("30 9 * * 1-5").unwrap();
        assert!(sched.matches(30, 9, 15, 6, 1)); // Monday
        assert!(sched.matches(30, 9, 15, 6, 5)); // Friday
        assert!(!sched.matches(30, 9, 15, 6, 0)); // Sunday
        assert!(!sched.matches(0, 9, 15, 6, 1)); // wrong minute
    }

    #[test]
    fn parse_invalid() {
        assert!(CronSchedule::parse("*/5 *").is_err()); // too few fields
        assert!(CronSchedule::parse("60 * * * *").is_err()); // minute out of range
        assert!(parse_field("*/0", 0, 59).is_err()); // step 0
    }

    #[test]
    fn normalize_self_target_to_creator() {
        assert_eq!(
            normalize_cron_target("self", "cheetah-sidekar-1").unwrap(),
            "cheetah-sidekar-1"
        );
        assert_eq!(
            normalize_loaded_target("self", "cheetah-sidekar-1"),
            "cheetah-sidekar-1"
        );
    }

    #[test]
    fn job_belongs_to_concrete_owner_only() {
        assert!(job_belongs_to_agent(
            "cheetah-sidekar-1",
            "cheetah-sidekar-1",
            Some("cheetah-sidekar-1")
        ));
        assert!(!job_belongs_to_agent(
            "cheetah-sidekar-1",
            "cheetah-sidekar-1",
            Some("otter-sidekar-1")
        ));
        assert!(job_belongs_to_agent(
            "self",
            "cheetah-sidekar-1",
            Some("cheetah-sidekar-1")
        ));
    }
}
