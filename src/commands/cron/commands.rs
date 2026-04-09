use super::schedule::CronSchedule;
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

    if let CronAction::Tool { ref tool, .. } = action_parsed {
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
    if crate::runtime::cron_depth() > 0 {
        bail!("Cannot create cron/loop jobs from within a cron action (prevents self-replication)");
    }
    if let CronAction::Prompt { ref prompt } = action_parsed {
        if prompt.trim().is_empty() {
            bail!("Prompt text cannot be empty");
        }
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

pub(crate) async fn cmd_cron_delete(ctx: &mut AppContext, job_id: &str) -> Result<()> {
    let cell = cron_cell().await;
    let guard = cell.lock().await;
    if let Some(state) = guard.as_ref() {
        let mut jobs = state.jobs.lock().await;
        jobs.retain(|j| j.id != job_id);
    }

    if broker::delete_cron_job(job_id)? {
        out!(ctx, "Cron job {job_id} deleted.");
    } else {
        bail!("Cron job '{job_id}' not found.");
    }
    Ok(())
}
