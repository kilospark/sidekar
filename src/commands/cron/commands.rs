use super::schedule::CronSchedule;
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[allow(clippy::too_many_arguments)]
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
    let schedule = CronSchedule::parse(schedule_expr)?;

    let action_parsed: CronAction = serde_json::from_value(action.clone())
        .context("Invalid action: must have 'tool', 'batch', 'command', or 'prompt' field")?;

    if let CronAction::Tool { ref tool, .. } = action_parsed
        && matches!(
            tool.as_str(),
            "cron-create" | "cron-delete" | "kill" | "uninstall"
        )
    {
        bail!("Tool '{tool}' cannot be used in cron actions");
    }
    if let CronAction::Bash { ref command } = action_parsed
        && command.trim().is_empty()
    {
        bail!("Bash command cannot be empty");
    }
    if crate::runtime::cron_depth() > 0 {
        bail!("Cannot create cron/loop jobs from within a cron action (prevents self-replication)");
    }
    if let CronAction::Prompt { ref prompt } = action_parsed
        && prompt.trim().is_empty()
    {
        bail!("Prompt text cannot be empty");
    }

    let effective_target = normalize_cron_target(target, created_by)?;

    let max_jobs = crate::config::load_config().max_cron_jobs;
    let current_count = broker::list_cron_jobs(true, crate::scope::ScopeView::All, "")
        .map(|jobs| jobs.len())
        .unwrap_or(0);
    if current_count >= max_jobs {
        bail!(
            "Cron job limit reached ({max_jobs}). Delete a job first, or increase max_cron_jobs in config."
        );
    }

    let id = format!("{:08x}", rand::random::<u32>());
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

    let cell = cron_cell().await;
    let guard = cell.lock().await;
    if let Some(state) = guard.as_ref() {
        state.jobs.lock().await.push(CronJob {
            id: id.clone(),
            schedule,
            action: action_parsed,
            target: effective_target.clone(),
            last_run_at: None,
            last_finished_at: None,
            once,
            loop_interval_secs,
            running: Arc::new(AtomicBool::new(false)),
        });
    }

    let create_out = CronCreateOutput {
        id: id.clone(),
        name: name.map(String::from),
        schedule: schedule_expr.to_string(),
        target: effective_target.clone(),
        once,
    };
    out!(ctx, "{}", crate::output::to_string(&create_out)?);

    Ok(id)
}

#[derive(serde::Serialize)]
struct CronCreateOutput {
    id: String,
    name: Option<String>,
    schedule: String,
    target: String,
    once: bool,
}

impl crate::output::CommandOutput for CronCreateOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Cron job created: {}", self.id)?;
        if let Some(n) = &self.name {
            writeln!(w, "Name: {n}")?;
        }
        writeln!(w, "Schedule: {}", self.schedule)?;
        writeln!(w, "Target: {}", self.target)?;
        if self.once {
            writeln!(w, "Mode: one-shot (auto-deletes after first run)")?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct CronListItem {
    id: String,
    name: Option<String>,
    schedule: String,
    target: String,
    owner: String,
    last_run_at: Option<u64>,
    last_run_secs_ago: Option<u64>,
    running: bool,
    action: Value,
    run_count: u64,
    error_count: u64,
    last_error: Option<String>,
}

#[derive(serde::Serialize)]
struct CronListOutput {
    items: Vec<CronListItem>,
    running: usize,
}

fn action_brief(action: &Value) -> String {
    if let Some(tool) = action.get("tool").and_then(|v| v.as_str()) {
        let args = action.get("args").cloned().unwrap_or(Value::Null);
        let args_brief = if args.is_null() || args == json!({}) {
            String::new()
        } else {
            let s = serde_json::to_string(&args).unwrap_or_default();
            if s.len() > 80 {
                format!(" {}", &s[..80])
            } else {
                format!(" {s}")
            }
        };
        return format!("{tool}{args_brief}");
    }
    if let Some(arr) = action.get("batch").and_then(|v| v.as_array()) {
        return format!("batch ({} steps)", arr.len());
    }
    if let Some(cmd) = action.get("command").and_then(|v| v.as_str()) {
        let brief = if cmd.len() > 80 { &cmd[..80] } else { cmd };
        return format!("bash `{brief}`");
    }
    if let Some(prompt) = action.get("prompt").and_then(|v| v.as_str()) {
        let brief = if prompt.len() > 80 {
            &prompt[..80]
        } else {
            prompt
        };
        return format!("prompt \"{brief}\"");
    }
    serde_json::to_string(action).unwrap_or_default()
}

impl crate::output::CommandOutput for CronListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "0 cron jobs.")?;
            return Ok(());
        }
        if self.running > 0 {
            writeln!(
                w,
                "{} cron jobs ({} running):",
                self.items.len(),
                self.running
            )?;
        } else {
            writeln!(w, "{} cron jobs:", self.items.len())?;
        }
        for it in &self.items {
            let last_run = it
                .last_run_secs_ago
                .map(|s| format!("last run: {s}s ago"))
                .unwrap_or_else(|| "never run".to_string());
            let running = if it.running { " [running]" } else { "" };
            writeln!(
                w,
                "[{}] {} — schedule: {} — target: {} — owner: {} — {}{}",
                it.id,
                it.name.as_deref().unwrap_or("(unnamed)"),
                it.schedule,
                it.target,
                it.owner,
                last_run,
                running
            )?;
            writeln!(w, "  action: {}", action_brief(&it.action))?;
            if it.run_count > 0 || it.error_count > 0 {
                let err = if it.error_count > 0 {
                    format!(", {} errors", it.error_count)
                } else {
                    String::new()
                };
                writeln!(w, "  [{}] stats: {} runs{}", it.id, it.run_count, err)?;
                if let Some(e) = &it.last_error {
                    writeln!(w, "  [{}] last error: {}", it.id, e)?;
                }
            }
        }
        Ok(())
    }
}

pub(crate) async fn cmd_cron_list(
    ctx: &mut AppContext,
    scope: crate::scope::ScopeView,
) -> Result<()> {
    let current_project = crate::scope::resolve_project_name(None);
    let persisted = broker::list_cron_jobs(true, scope, &current_project).unwrap_or_default();

    let running_ids: std::collections::HashSet<String> = {
        let cell = cron_cell().await;
        let guard = cell.lock().await;
        match guard.as_ref() {
            Some(state) => state
                .jobs
                .lock()
                .await
                .iter()
                .filter(|j| j.running.load(Ordering::Relaxed))
                .map(|j| j.id.clone())
                .collect(),
            None => std::collections::HashSet::new(),
        }
    };

    let now = epoch_now();
    let items: Vec<CronListItem> = persisted
        .into_iter()
        .map(|rec| {
            let action: Value = serde_json::from_str(&rec.action_json).unwrap_or(Value::Null);
            let secs_ago = rec.last_run_at.map(|ts| now.saturating_sub(ts));
            let running = running_ids.contains(&rec.id);
            CronListItem {
                id: rec.id,
                name: rec.name,
                schedule: rec.schedule,
                target: rec.target,
                owner: rec.created_by,
                last_run_at: rec.last_run_at,
                last_run_secs_ago: secs_ago,
                running,
                action,
                run_count: rec.run_count,
                error_count: rec.error_count,
                last_error: rec.last_error,
            }
        })
        .collect();

    let running = items.iter().filter(|i| i.running).count();
    let output = CronListOutput { items, running };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

#[derive(serde::Serialize)]
struct CronShowOutput {
    id: String,
    name: Option<String>,
    active: bool,
    once: bool,
    schedule: String,
    target: String,
    owner: String,
    created_at: u64,
    last_run_at: Option<u64>,
    run_count: u64,
    error_count: u64,
    last_error: Option<String>,
    action: Value,
}

impl crate::output::CommandOutput for CronShowOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "id: {}", self.id)?;
        writeln!(w, "name: {}", self.name.as_deref().unwrap_or("(unnamed)"))?;
        writeln!(w, "active: {}", if self.active { "yes" } else { "no" })?;
        writeln!(w, "once: {}", if self.once { "yes" } else { "no" })?;
        writeln!(w, "schedule: {}", self.schedule)?;
        writeln!(w, "target: {}", self.target)?;
        writeln!(w, "owner: {}", self.owner)?;
        writeln!(w, "created_at: {}", self.created_at)?;
        writeln!(
            w,
            "last_run_at: {}",
            self.last_run_at
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into())
        )?;
        writeln!(w, "run_count: {}", self.run_count)?;
        writeln!(w, "error_count: {}", self.error_count)?;
        writeln!(
            w,
            "last_error: {}",
            self.last_error.as_deref().unwrap_or("-")
        )?;
        let action_str = serde_json::to_string_pretty(&self.action).unwrap_or_default();
        writeln!(w, "action_json: {action_str}")?;
        Ok(())
    }
}

pub(crate) async fn cmd_cron_show(ctx: &mut AppContext, job_id: &str) -> Result<()> {
    let rec =
        broker::get_cron_job(job_id)?.ok_or_else(|| anyhow!("Cron job '{job_id}' not found."))?;
    let action: Value = serde_json::from_str(&rec.action_json)
        .context("failed to parse stored cron action JSON")?;
    let output = CronShowOutput {
        id: rec.id,
        name: rec.name,
        active: rec.active,
        once: rec.once,
        schedule: rec.schedule,
        target: rec.target,
        owner: rec.created_by,
        created_at: rec.created_at,
        last_run_at: rec.last_run_at,
        run_count: rec.run_count,
        error_count: rec.error_count,
        last_error: rec.last_error,
        action,
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_cron_delete(ctx: &mut AppContext, job_id: &str) -> Result<()> {
    let cell = cron_cell().await;
    let guard = cell.lock().await;
    if let Some(state) = guard.as_ref() {
        let mut jobs = state.jobs.lock().await;
        jobs.retain(|j| j.id != job_id);
    }

    if broker::delete_cron_job(job_id)? {
        let msg = format!("Cron job {job_id} deleted.");
        out!(
            ctx,
            "{}",
            crate::output::to_string(&crate::output::PlainOutput::new(msg))?
        );
    } else {
        bail!("Cron job '{job_id}' not found.");
    }
    Ok(())
}
