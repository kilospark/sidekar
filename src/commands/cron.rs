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

/// Parse a human interval like "5m", "1h", "30s" into a cron expression.
/// Minimum granularity is 1 minute. Intervals < 1m are clamped to 1m.
/// Parse an interval string (e.g. "5m", "1h", "120s") into seconds.
pub(crate) fn interval_to_secs(interval: &str) -> Result<u64> {
    let interval = interval.trim().to_lowercase();
    let (num_str, unit) = if let Some(n) = interval.strip_suffix('m') {
        (n, 'm')
    } else if let Some(n) = interval.strip_suffix('h') {
        (n, 'h')
    } else if let Some(n) = interval.strip_suffix('s') {
        (n, 's')
    } else {
        (interval.as_str(), 'm')
    };
    let num: u64 = num_str.parse().context("Invalid interval number")?;
    if num == 0 {
        bail!("Interval must be > 0");
    }
    match unit {
        's' => Ok(num.max(60)), // minimum 60 seconds
        'm' => Ok(num * 60),
        'h' => Ok(num * 3600),
        _ => bail!("Unknown interval unit. Use s, m, or h"),
    }
}

#[cfg(test)]
fn interval_to_cron(interval: &str) -> Result<String> {
    let interval = interval.trim().to_lowercase();
    let (num_str, unit) = if let Some(n) = interval.strip_suffix('m') {
        (n, 'm')
    } else if let Some(n) = interval.strip_suffix('h') {
        (n, 'h')
    } else if let Some(n) = interval.strip_suffix('s') {
        (n, 's')
    } else {
        (interval.as_str(), 'm') // default to minutes
    };

    let num: u32 = num_str.parse().context("Invalid interval number")?;
    if num == 0 {
        bail!("Interval must be > 0");
    }

    match unit {
        's' => {
            let minutes = ((num + 59) / 60).max(1);
            if minutes >= 60 {
                Ok(format!("0 */{} * * *", minutes / 60))
            } else {
                Ok(format!("*/{minutes} * * * *"))
            }
        }
        'm' => {
            if num >= 60 {
                Ok(format!("0 */{} * * *", num / 60))
            } else {
                Ok(format!("*/{num} * * * *"))
            }
        }
        'h' => Ok(format!("0 */{num} * * *")),
        _ => bail!("Unknown interval unit. Use s, m, or h"),
    }
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
    Bash {
        command: String,
    },
    Prompt {
        prompt: String,
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
    last_finished_at: Option<u64>,
    once: bool,
    loop_interval_secs: Option<u64>, // if set, uses loop semantics (wait for completion + interval)
    running: Arc<AtomicBool>,
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

/// Convenience wrapper: start the cron loop with default CDP settings.
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
        ctx.isolated = true; // cron runs in isolated context
        ctx.agent_name = self.agent_name.clone();
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

    // Load persisted jobs scoped to this agent's project
    if let Ok(records) =
        broker::list_cron_jobs(true, crate::scope::ScopeView::Project, &cron_ctx.project)
    {
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
                        last_finished_at: None,
                        once: rec.once,
                        loop_interval_secs: rec.loop_interval_secs,
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
    let task_handle = tokio::spawn(cron_loop_supervisor(r.clone(), j, cron_ctx));

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
    once: bool,
    project: Option<&str>,
    loop_interval_secs: Option<u64>,
) -> Result<String> {
    // Validate schedule
    let schedule = CronSchedule::parse(schedule_expr)?;

    // Enforce minimum interval: reject schedules that fire every second
    // (we check at minute granularity, so anything valid is >= 1 min)
    // But also reject "* * * * *" — every minute is ok but warn.

    // Validate action
    let action_parsed: CronAction = serde_json::from_value(action.clone())
        .context("Invalid action: must have 'tool', 'batch', 'command', or 'prompt' field")?;

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
    if let CronAction::Bash { ref command } = action_parsed {
        if command.trim().is_empty() {
            bail!("Bash command cannot be empty");
        }
    }
    // Anti-replication: in-process guard for tool/batch dispatch plus inherited child env for
    // spawned shell commands. The hard cap at max_cron_jobs is the deeper safety net.
    if crate::runtime::cron_depth() > 0 {
        bail!("Cannot create cron/loop jobs from within a cron action (prevents self-replication)");
    }
    if let CronAction::Prompt { ref prompt } = action_parsed {
        if prompt.trim().is_empty() {
            bail!("Prompt text cannot be empty");
        }
    }

    let effective_target = normalize_cron_target(target, created_by)?;

    // Check job limit (use broker as source of truth — works with or without cron loop)
    let max_jobs = crate::config::load_config().max_cron_jobs;
    let current_count = broker::list_cron_jobs(true, crate::scope::ScopeView::All, "")
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
        once,
        project,
        loop_interval_secs,
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
            last_finished_at: None,
            once,
            loop_interval_secs,
            running: Arc::new(AtomicBool::new(false)),
        });
    }

    out!(ctx, "Cron job created: {id}");
    if let Some(n) = name {
        out!(ctx, "Name: {n}");
    }
    out!(ctx, "Schedule: {schedule_expr}");
    out!(ctx, "Target: {effective_target}");
    if once {
        out!(ctx, "Mode: one-shot (auto-deletes after first run)");
    }

    Ok(id)
}

/// List cron jobs scoped to the current project (default), global, or all.
pub(crate) async fn cmd_cron_list(
    ctx: &mut AppContext,
    scope: crate::scope::ScopeView,
) -> Result<()> {
    let current_project = crate::scope::resolve_project_name(None);
    let cell = cron_cell().await;
    let guard = cell.lock().await;

    match guard.as_ref() {
        Some(state) => {
            let jobs = state.jobs.lock().await;
            if jobs.is_empty() {
                out!(ctx, "0 cron jobs.");
                return Ok(());
            }
            let running = jobs
                .iter()
                .filter(|j| j.running.load(Ordering::Relaxed))
                .count();
            if running > 0 {
                out!(ctx, "{} cron jobs ({} running):", jobs.len(), running);
            } else {
                out!(ctx, "{} cron jobs:", jobs.len());
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
                    CronAction::Bash { command } => {
                        let brief = if command.len() > 80 {
                            &command[..80]
                        } else {
                            command
                        };
                        out!(ctx, "  action: bash `{brief}`");
                    }
                    CronAction::Prompt { prompt } => {
                        let brief = if prompt.len() > 80 {
                            &prompt[..80]
                        } else {
                            prompt
                        };
                        out!(ctx, "  action: prompt \"{brief}\"");
                    }
                }
            }
            // Also show stats from broker
            if let Ok(records) = broker::list_cron_jobs(true, scope, &current_project) {
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
            if let Ok(records) = broker::list_cron_jobs(true, scope, &current_project) {
                if records.is_empty() {
                    out!(ctx, "0 cron jobs.");
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
                out!(ctx, "0 cron jobs.");
            }
        }
    }
    Ok(())
}

/// Show one cron job in detail.
pub(crate) async fn cmd_cron_show(ctx: &mut AppContext, job_id: &str) -> Result<()> {
    let rec =
        broker::get_cron_job(job_id)?.ok_or_else(|| anyhow!("Cron job '{job_id}' not found."))?;
    let action_value: Value = serde_json::from_str(&rec.action_json)
        .context("failed to parse stored cron action JSON")?;

    out!(ctx, "id: {}", rec.id);
    out!(ctx, "name: {}", rec.name.as_deref().unwrap_or("(unnamed)"));
    out!(ctx, "active: {}", if rec.active { "yes" } else { "no" });
    out!(ctx, "once: {}", if rec.once { "yes" } else { "no" });
    out!(ctx, "schedule: {}", rec.schedule);
    out!(ctx, "target: {}", rec.target);
    out!(ctx, "owner: {}", rec.created_by);
    out!(ctx, "created_at: {}", rec.created_at);
    out!(
        ctx,
        "last_run_at: {}",
        rec.last_run_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into())
    );
    out!(ctx, "run_count: {}", rec.run_count);
    out!(ctx, "error_count: {}", rec.error_count);
    out!(
        ctx,
        "last_error: {}",
        rec.last_error.as_deref().unwrap_or("-")
    );
    out!(
        ctx,
        "action_json: {}",
        serde_json::to_string_pretty(&action_value).unwrap_or_else(|_| rec.action_json.clone())
    );
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
            Ok(()) => break, // normal exit (running set to false)
            Err(_) => {
                // task panicked — restart if not shutting down
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
        if let Ok(records) =
            broker::list_cron_jobs(true, crate::scope::ScopeView::Project, &cron_ctx.project)
        {
            let mut mem_jobs = jobs.lock().await;
            // Collect IDs of jobs that belong to this agent for pruning deleted ones
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
                            last_finished_at: None,
                            once: rec.once,
                            loop_interval_secs: rec.loop_interval_secs,
                            running: Arc::new(AtomicBool::new(false)),
                        });
                    }
                    Err(_) => continue,
                }
            }
            // Remove jobs that were deleted from broker
            mem_jobs.retain(|j| broker_ids.contains(&j.id));
        }

        // Get current local time components
        let (min, hour, dom, month, dow) = local_time_components();
        let now = epoch_now();

        // Collect jobs to execute
        let jobs_guard = jobs.lock().await;
        let mut to_execute: Vec<(String, CronAction, String, bool, Arc<AtomicBool>)> = Vec::new();

        for j in jobs_guard.iter() {
            if j.running.load(Ordering::Relaxed) {
                continue;
            }
            if let Some(interval) = j.loop_interval_secs {
                // Loop job: fire if interval has elapsed since last completion
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
            } else {
                // Cron job: fire on schedule match
                if j.schedule.matches(min, hour, dom, month, dow) {
                    to_execute.push((
                        j.id.clone(),
                        j.action.clone(),
                        j.target.clone(),
                        j.once,
                        j.running.clone(),
                    ));
                }
            }
        }
        drop(jobs_guard);

        // Collect one-shot job IDs for removal after execution
        let once_ids: Vec<String> = to_execute
            .iter()
            .filter(|(_, _, _, is_once, _)| *is_once)
            .map(|(id, _, _, _, _)| id.clone())
            .collect();

        // Execute matching jobs
        let jobs_for_update = jobs.clone();
        for (job_id, action, target, is_once, job_running) in to_execute {
            let ctx_clone = cron_ctx.clone();
            let jid = job_id.clone();
            let jobs_ref = jobs_for_update.clone();
            job_running.store(true, Ordering::Relaxed);

            tokio::spawn(async move {
                let result = execute_cron_job(&ctx_clone, &jid, &action, &target).await;
                job_running.store(false, Ordering::Relaxed);

                // Update last_finished_at for loop jobs
                let finished_at = epoch_now();
                {
                    let mut guard = jobs_ref.lock().await;
                    if let Some(job) = guard.iter_mut().find(|j| j.id == jid) {
                        job.last_finished_at = Some(finished_at);
                    }
                }

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

                // Auto-delete one-shot jobs after execution
                if is_once {
                    let _ = broker::delete_cron_job(&jid);
                }
            });
        }

        // Remove one-shot jobs from memory after they fire
        if !once_ids.is_empty() {
            let mut jobs_guard = jobs.lock().await;
            jobs_guard.retain(|j| !once_ids.contains(&j.id));
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

    let _cron_guard = crate::runtime::enter_cron_action();
    let inherited_agent_name = cron_ctx.agent_name.clone();
    let inherited_channel = inherited_agent_name.as_deref().and_then(|name| {
        crate::broker::find_agent(name, None)
            .ok()
            .flatten()
            .and_then(|agent| agent.id.session)
    });

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
        CronAction::Prompt { prompt } => {
            // Prompt action: the text IS the output, delivered raw to the agent's PTY.
            prompt.clone()
        }
    };

    // Deliver result via broker queue to the concrete owning/target agent.
    if !output.trim().is_empty() {
        let msg = match action {
            CronAction::Prompt { .. } => output.trim().to_string(),
            _ => format!("[from sidekar-cron]: [cron {job_id}]: {}", output.trim()),
        };
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
    let _ = target;
    created_by == agent_name
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
    fn interval_to_cron_minutes() {
        assert_eq!(interval_to_cron("5m").unwrap(), "*/5 * * * *");
        assert_eq!(interval_to_cron("1m").unwrap(), "*/1 * * * *");
        assert_eq!(interval_to_cron("30m").unwrap(), "*/30 * * * *");
    }

    #[test]
    fn interval_to_cron_hours() {
        assert_eq!(interval_to_cron("1h").unwrap(), "0 */1 * * *");
        assert_eq!(interval_to_cron("2h").unwrap(), "0 */2 * * *");
    }

    #[test]
    fn interval_to_cron_seconds_clamp() {
        assert_eq!(interval_to_cron("30s").unwrap(), "*/1 * * * *"); // clamped to 1m
        assert_eq!(interval_to_cron("120s").unwrap(), "*/2 * * * *");
    }

    #[test]
    fn interval_to_cron_large_minutes() {
        assert_eq!(interval_to_cron("120m").unwrap(), "0 */2 * * *");
    }

    #[test]
    fn interval_to_cron_default_unit() {
        assert_eq!(interval_to_cron("10").unwrap(), "*/10 * * * *"); // defaults to minutes
    }

    #[test]
    fn interval_to_cron_invalid() {
        assert!(interval_to_cron("0m").is_err());
        assert!(interval_to_cron("abc").is_err());
    }

    #[test]
    fn cron_action_serde_roundtrip() {
        let tool: CronAction = serde_json::from_str(r#"{"tool":"screenshot"}"#).unwrap();
        assert!(matches!(tool, CronAction::Tool { .. }));

        let bash: CronAction = serde_json::from_str(r#"{"command":"echo hello"}"#).unwrap();
        assert!(matches!(bash, CronAction::Bash { .. }));

        let prompt: CronAction = serde_json::from_str(r#"{"prompt":"check status"}"#).unwrap();
        assert!(matches!(prompt, CronAction::Prompt { .. }));

        let batch: CronAction = serde_json::from_str(r#"{"batch":[{"tool":"read"}]}"#).unwrap();
        assert!(matches!(batch, CronAction::Batch { .. }));
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
