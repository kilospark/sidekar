use crate::*;
use rusqlite::{OptionalExtension, params};

mod db;
use db::*;

#[derive(Debug, Clone)]
struct TaskRow {
    id: i64,
    title: String,
    notes: Option<String>,
    scope: String,
    project: Option<String>,
    status: String,
    priority: i64,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
}

pub fn cmd_tasks(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "add" => cmd_tasks_add(ctx, &args[1..]),
        "list" => cmd_tasks_list(ctx, &args[1..]),
        "done" => cmd_tasks_done(ctx, &args[1..]),
        "reopen" => cmd_tasks_reopen(ctx, &args[1..]),
        "delete" => cmd_tasks_delete(ctx, &args[1..]),
        "show" => cmd_tasks_show(ctx, &args[1..]),
        "depend" => cmd_tasks_depend(ctx, &args[1..]),
        "undepend" => cmd_tasks_undepend(ctx, &args[1..]),
        "deps" => cmd_tasks_deps(ctx, &args[1..]),
        "" => cmd_tasks_list(ctx, args),
        other if other.starts_with('-') => cmd_tasks_list(ctx, args),
        other => bail!("Unknown tasks subcommand: {other}"),
    }
}

fn cmd_tasks_add(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let title = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .cloned()
        .context(
            "Usage: sidekar tasks add <title> [--notes=...] [--priority=N] [--scope=project|global] [--project=P]",
        )?;
    let notes = extract_optional_value(args, "--notes=");
    let priority = extract_optional_value(args, "--priority=")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let (scope, project) = parse_task_write_scope(args)?;
    let id = insert_task(
        &title,
        notes.as_deref(),
        priority,
        &scope,
        project.as_deref(),
    )?;
    let msg = format!("Stored task [{}].", id);
    out!(ctx, "{}", crate::output::to_string(&crate::output::PlainOutput::new(msg))?);
    Ok(())
}

#[derive(serde::Serialize)]
struct TaskOut {
    id: i64,
    title: String,
    status: String,
    priority: i64,
    scope: String,
    project: Option<String>,
    blocked_by: i64,
    #[serde(skip)]
    scope_suffix: String,
}

#[derive(serde::Serialize)]
struct TasksListOutput {
    items: Vec<TaskOut>,
}

impl crate::output::CommandOutput for TasksListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "0 tasks.")?;
            return Ok(());
        }
        let blocked = self.items.iter().filter(|t| t.blocked_by > 0).count();
        let ready = self.items.len() - blocked;
        writeln!(
            w,
            "{} tasks ({} ready, {} blocked):",
            self.items.len(),
            ready,
            blocked
        )?;
        for t in &self.items {
            let marker = if t.status == "done" { 'x' } else { ' ' };
            let state = if t.blocked_by > 0 {
                format!(" blocked-by={}", t.blocked_by)
            } else if t.status == "open" {
                " ready".to_string()
            } else {
                String::new()
            };
            writeln!(
                w,
                "[{}] [{}] p={} {}{}{}",
                t.id, marker, t.priority, t.title, t.scope_suffix, state
            )?;
        }
        Ok(())
    }
}

fn cmd_tasks_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let status = extract_optional_value(args, "--status=").unwrap_or_else(|| "open".to_string());
    if !matches!(status.as_str(), "open" | "done" | "all") {
        bail!("Invalid status: {status}. Valid: open, done, all");
    }
    let ready_only = args.iter().any(|arg| arg == "--ready");
    let blocked_only = args.iter().any(|arg| arg == "--blocked");
    if ready_only && blocked_only {
        bail!("Use either --ready or --blocked, not both");
    }
    let limit = extract_optional_value(args, "--limit=")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50);
    let (scope_view, project) = parse_task_view_scope(args)?;

    let conn = crate::broker::open_db()?;
    let rows = load_tasks(&conn, &status, limit, scope_view, project.as_deref())?;

    let items: Vec<TaskOut> = rows
        .into_iter()
        .filter_map(|row| {
            let unfinished = unfinished_dependency_count(&conn, row.id).unwrap_or(0);
            if ready_only && unfinished > 0 {
                return None;
            }
            if blocked_only && unfinished == 0 {
                return None;
            }
            let scope_suffix = render_scope_suffix(&row, scope_view, project.as_deref());
            Some(TaskOut {
                id: row.id,
                title: row.title,
                status: row.status,
                priority: row.priority,
                scope: row.scope,
                project: row.project,
                blocked_by: unfinished,
                scope_suffix,
            })
        })
        .collect();

    let output = TasksListOutput { items };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

fn cmd_tasks_done(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks done <id>")?;
    update_task_status(id, "done")?;
    let msg = format!("Completed task [{}].", id);
    out!(ctx, "{}", crate::output::to_string(&crate::output::PlainOutput::new(msg))?);
    Ok(())
}

fn cmd_tasks_reopen(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks reopen <id>")?;
    update_task_status(id, "open")?;
    let msg = format!("Reopened task [{}].", id);
    out!(ctx, "{}", crate::output::to_string(&crate::output::PlainOutput::new(msg))?);
    Ok(())
}

fn cmd_tasks_delete(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks delete <id>")?;
    let conn = crate::broker::open_db()?;
    let deleted = conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
    if deleted == 0 {
        bail!("Task [{}] not found.", id);
    }
    let msg = format!("Deleted task [{}].", id);
    out!(ctx, "{}", crate::output::to_string(&crate::output::PlainOutput::new(msg))?);
    Ok(())
}

#[derive(serde::Serialize)]
struct TaskDepOut {
    id: i64,
    title: String,
    status: String,
}

#[derive(serde::Serialize)]
struct TaskShowOutput {
    id: i64,
    title: String,
    scope: String,
    project: Option<String>,
    status: String,
    priority: i64,
    ready: bool,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
    notes: Option<String>,
    depends_on: Vec<TaskDepOut>,
    blocks: Vec<TaskDepOut>,
}

impl crate::output::CommandOutput for TaskShowOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "[{}] {}", self.id, self.title)?;
        writeln!(w, "scope: {}", self.scope)?;
        writeln!(w, "project: {}", self.project.as_deref().unwrap_or("-"))?;
        writeln!(w, "status: {}", self.status)?;
        writeln!(w, "priority: {}", self.priority)?;
        writeln!(w, "ready: {}", if self.ready { "yes" } else { "no" })?;
        writeln!(w, "created_at: {}", self.created_at)?;
        writeln!(w, "updated_at: {}", self.updated_at)?;
        writeln!(
            w,
            "completed_at: {}",
            self.completed_at
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string())
        )?;
        writeln!(w, "notes: {}", self.notes.as_deref().unwrap_or("-"))?;
        if self.depends_on.is_empty() {
            writeln!(w, "depends_on: -")?;
        } else {
            writeln!(w, "depends_on:")?;
            for dep in &self.depends_on {
                writeln!(w, "  - [{}] {} ({})", dep.id, dep.title, dep.status)?;
            }
        }
        if self.blocks.is_empty() {
            writeln!(w, "blocks: -")?;
        } else {
            writeln!(w, "blocks:")?;
            for dep in &self.blocks {
                writeln!(w, "  - [{}] {} ({})", dep.id, dep.title, dep.status)?;
            }
        }
        Ok(())
    }
}

fn task_row_to_dep(row: TaskRow) -> TaskDepOut {
    TaskDepOut {
        id: row.id,
        title: row.title,
        status: row.status,
    }
}

fn cmd_tasks_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks show <id>")?;
    let conn = crate::broker::open_db()?;
    let task = fetch_task(&conn, id)?.context("task not found")?;
    let depends_on = task_dependencies(&conn, id)?;
    let blocks = blocking_tasks(&conn, id)?;
    let unfinished = unfinished_dependency_count(&conn, id)?;
    let ready = task.status == "open" && unfinished == 0;

    let output = TaskShowOutput {
        id: task.id,
        title: task.title,
        scope: task.scope,
        project: task.project,
        status: task.status,
        priority: task.priority,
        ready,
        created_at: task.created_at,
        updated_at: task.updated_at,
        completed_at: task.completed_at,
        notes: task.notes,
        depends_on: depends_on.into_iter().map(task_row_to_dep).collect(),
        blocks: blocks.into_iter().map(task_row_to_dep).collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

fn cmd_tasks_depend(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar tasks depend <task_id> <depends_on_id>");
    }
    let task_id = parse_task_id(
        args.first(),
        "Usage: sidekar tasks depend <task_id> <depends_on_id>",
    )?;
    let depends_on_id = parse_task_id(
        args.get(1),
        "Usage: sidekar tasks depend <task_id> <depends_on_id>",
    )?;
    add_dependency(task_id, depends_on_id)?;
    out!(
        ctx,
        "Task [{}] now depends on [{}].",
        task_id,
        depends_on_id
    );
    Ok(())
}

fn cmd_tasks_undepend(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar tasks undepend <task_id> <depends_on_id>");
    }
    let task_id = parse_task_id(
        args.first(),
        "Usage: sidekar tasks undepend <task_id> <depends_on_id>",
    )?;
    let depends_on_id = parse_task_id(
        args.get(1),
        "Usage: sidekar tasks undepend <task_id> <depends_on_id>",
    )?;
    let conn = crate::broker::open_db()?;
    let deleted = conn.execute(
        "DELETE FROM task_dependencies WHERE task_id = ?1 AND depends_on_task_id = ?2",
        params![task_id, depends_on_id],
    )?;
    if deleted == 0 {
        bail!("Dependency [{}] -> [{}] not found.", task_id, depends_on_id);
    }
    out!(
        ctx,
        "Removed dependency [{}] -> [{}].",
        task_id,
        depends_on_id
    );
    Ok(())
}

#[derive(serde::Serialize)]
struct TaskDepsOutput {
    id: i64,
    depends_on: Vec<TaskDepOut>,
    blocks: Vec<TaskDepOut>,
}

impl crate::output::CommandOutput for TaskDepsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Task [{}] dependencies:", self.id)?;
        if self.depends_on.is_empty() {
            writeln!(w, "  depends_on: -")?;
        } else {
            for dep in &self.depends_on {
                writeln!(
                    w,
                    "  depends_on [{}] {} ({})",
                    dep.id, dep.title, dep.status
                )?;
            }
        }
        if self.blocks.is_empty() {
            writeln!(w, "  blocks: -")?;
        } else {
            for dep in &self.blocks {
                writeln!(
                    w,
                    "  blocks [{}] {} ({})",
                    dep.id, dep.title, dep.status
                )?;
            }
        }
        Ok(())
    }
}

fn cmd_tasks_deps(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks deps <id>")?;
    let conn = crate::broker::open_db()?;
    let depends_on = task_dependencies(&conn, id)?;
    let blocks = blocking_tasks(&conn, id)?;

    let output = TaskDepsOutput {
        id,
        depends_on: depends_on.into_iter().map(task_row_to_dep).collect(),
        blocks: blocks.into_iter().map(task_row_to_dep).collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

fn parse_task_id(value: Option<&String>, usage: &str) -> Result<i64> {
    value
        .ok_or_else(|| anyhow!(usage.to_string()))?
        .parse::<i64>()
        .context("task id must be numeric")
}

fn extract_optional_value(args: &[String], prefix: &str) -> Option<String> {
    args.iter()
        .find_map(|arg| arg.strip_prefix(prefix).map(ToOwned::to_owned))
}

fn parse_task_write_scope(args: &[String]) -> Result<(String, Option<String>)> {
    let scope = crate::scope::parse_stored_scope(
        &extract_optional_value(args, "--scope=")
            .unwrap_or_else(|| crate::scope::PROJECT_SCOPE.to_string()),
    )?
    .to_string();
    let project = if scope == crate::scope::PROJECT_SCOPE {
        Some(
            extract_optional_value(args, "--project=")
                .unwrap_or_else(|| crate::scope::resolve_project_name(None)),
        )
    } else {
        None
    };
    Ok((scope, project))
}

fn parse_task_view_scope(args: &[String]) -> Result<(crate::scope::ScopeView, Option<String>)> {
    let scope =
        crate::scope::ScopeView::parse(extract_optional_value(args, "--scope=").as_deref())?;
    let project = if scope == crate::scope::ScopeView::Project {
        Some(
            extract_optional_value(args, "--project=")
                .unwrap_or_else(|| crate::scope::resolve_project_name(None)),
        )
    } else {
        None
    };
    Ok((scope, project))
}

fn render_scope_suffix(
    row: &TaskRow,
    scope_view: crate::scope::ScopeView,
    current_project: Option<&str>,
) -> String {
    if row.scope == crate::scope::GLOBAL_SCOPE {
        " [global]".to_string()
    } else if scope_view == crate::scope::ScopeView::All
        || row.project.as_deref() != current_project
    {
        row.project
            .as_deref()
            .map(|project| format!(" [{project}]"))
            .unwrap_or_default()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests;
