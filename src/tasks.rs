use crate::*;
use rusqlite::{OptionalExtension, params};

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
    out!(ctx, "Stored task [{}].", id);
    Ok(())
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

    let json_output = args.iter().any(|a| a == "--json");
    let conn = crate::broker::open_db()?;
    let rows = load_tasks(&conn, &status, limit, scope_view, project.as_deref())?;

    if json_output {
        let items: Vec<serde_json::Value> = rows
            .iter()
            .filter(|row| {
                let unfinished = unfinished_dependency_count(&conn, row.id).unwrap_or(0);
                if ready_only && unfinished > 0 {
                    return false;
                }
                if blocked_only && unfinished == 0 {
                    return false;
                }
                true
            })
            .map(|row| {
                let unfinished = unfinished_dependency_count(&conn, row.id).unwrap_or(0);
                serde_json::json!({
                    "id": row.id,
                    "title": row.title,
                    "status": row.status,
                    "priority": row.priority,
                    "scope": row.scope,
                    "project": row.project,
                    "blocked_by": unfinished,
                })
            })
            .collect();
        out!(
            ctx,
            "{}",
            serde_json::to_string_pretty(&items).unwrap_or_default()
        );
        return Ok(());
    }

    // Pre-filter rows for ready/blocked
    let filtered: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            let unfinished = unfinished_dependency_count(&conn, row.id).unwrap_or(0);
            if ready_only && unfinished > 0 {
                return false;
            }
            if blocked_only && unfinished == 0 {
                return false;
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        out!(ctx, "0 tasks.");
        return Ok(());
    }

    let blocked_count = filtered
        .iter()
        .filter(|r| unfinished_dependency_count(&conn, r.id).unwrap_or(0) > 0)
        .count();
    let ready_count = filtered.len() - blocked_count;
    out!(
        ctx,
        "{} tasks ({} ready, {} blocked):",
        filtered.len(),
        ready_count,
        blocked_count
    );

    for row in &filtered {
        let unfinished = unfinished_dependency_count(&conn, row.id)?;
        let marker = if row.status == "done" { 'x' } else { ' ' };
        let state = if unfinished > 0 {
            format!(" blocked-by={unfinished}")
        } else if row.status == "open" {
            " ready".to_string()
        } else {
            String::new()
        };
        let scope = render_scope_suffix(row, scope_view, project.as_deref());
        out!(
            ctx,
            "[{}] [{}] p={} {}{}{}",
            row.id,
            marker,
            row.priority,
            row.title,
            scope,
            state
        );
    }
    Ok(())
}

fn cmd_tasks_done(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks done <id>")?;
    update_task_status(id, "done")?;
    out!(ctx, "Completed task [{}].", id);
    Ok(())
}

fn cmd_tasks_reopen(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks reopen <id>")?;
    update_task_status(id, "open")?;
    out!(ctx, "Reopened task [{}].", id);
    Ok(())
}

fn cmd_tasks_delete(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks delete <id>")?;
    let conn = crate::broker::open_db()?;
    let deleted = conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
    if deleted == 0 {
        bail!("Task [{}] not found.", id);
    }
    out!(ctx, "Deleted task [{}].", id);
    Ok(())
}

fn cmd_tasks_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks show <id>")?;
    let conn = crate::broker::open_db()?;
    let task = fetch_task(&conn, id)?.context("task not found")?;
    let depends_on = task_dependencies(&conn, id)?;
    let blocks = blocking_tasks(&conn, id)?;
    let unfinished = unfinished_dependency_count(&conn, id)?;

    out!(ctx, "[{}] {}", task.id, task.title);
    out!(ctx, "scope: {}", task.scope);
    out!(
        ctx,
        "project: {}",
        task.project.unwrap_or_else(|| "-".to_string())
    );
    out!(ctx, "status: {}", task.status);
    out!(ctx, "priority: {}", task.priority);
    out!(
        ctx,
        "ready: {}",
        if task.status == "open" && unfinished == 0 {
            "yes"
        } else {
            "no"
        }
    );
    out!(ctx, "created_at: {}", task.created_at);
    out!(ctx, "updated_at: {}", task.updated_at);
    out!(
        ctx,
        "completed_at: {}",
        task.completed_at
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    out!(
        ctx,
        "notes: {}",
        task.notes.unwrap_or_else(|| "-".to_string())
    );

    if depends_on.is_empty() {
        out!(ctx, "depends_on: -");
    } else {
        out!(ctx, "depends_on:");
        for dep in depends_on {
            out!(ctx, "  - [{}] {} ({})", dep.id, dep.title, dep.status);
        }
    }

    if blocks.is_empty() {
        out!(ctx, "blocks: -");
    } else {
        out!(ctx, "blocks:");
        for dep in blocks {
            out!(ctx, "  - [{}] {} ({})", dep.id, dep.title, dep.status);
        }
    }
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

fn cmd_tasks_deps(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id = parse_task_id(args.first(), "Usage: sidekar tasks deps <id>")?;
    let conn = crate::broker::open_db()?;
    let depends_on = task_dependencies(&conn, id)?;
    let blocks = blocking_tasks(&conn, id)?;

    out!(ctx, "Task [{}] dependencies:", id);
    if depends_on.is_empty() {
        out!(ctx, "  depends_on: -");
    } else {
        for dep in depends_on {
            out!(
                ctx,
                "  depends_on [{}] {} ({})",
                dep.id,
                dep.title,
                dep.status
            );
        }
    }
    if blocks.is_empty() {
        out!(ctx, "  blocks: -");
    } else {
        for dep in blocks {
            out!(ctx, "  blocks [{}] {} ({})", dep.id, dep.title, dep.status);
        }
    }
    Ok(())
}

fn insert_task(
    title: &str,
    notes: Option<&str>,
    priority: i64,
    scope: &str,
    project: Option<&str>,
) -> Result<i64> {
    let conn = crate::broker::open_db()?;
    let now = now_epoch_ms();
    conn.execute(
        "INSERT INTO tasks (title, notes, scope, project, status, priority, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6, ?6)",
        params![
            title.trim(),
            notes.map(str::trim),
            scope,
            project,
            priority,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn update_task_status(id: i64, status: &str) -> Result<()> {
    let conn = crate::broker::open_db()?;
    ensure_task_exists(&conn, id)?;
    let now = now_epoch_ms();
    let completed_at = if status == "done" { Some(now) } else { None };
    conn.execute(
        "UPDATE tasks SET status = ?2, updated_at = ?3, completed_at = ?4 WHERE id = ?1",
        params![id, status, now, completed_at],
    )?;
    Ok(())
}

fn add_dependency(task_id: i64, depends_on_id: i64) -> Result<()> {
    let conn = crate::broker::open_db()?;
    let task = fetch_required_task(&conn, task_id)?;
    let depends_on = fetch_required_task(&conn, depends_on_id)?;
    if task_id == depends_on_id {
        bail!("A task cannot depend on itself.");
    }
    if task.scope != depends_on.scope || task.project != depends_on.project {
        bail!("Dependencies must stay within the same scope and project bucket.");
    }
    if introduces_cycle(&conn, task_id, depends_on_id)? {
        bail!(
            "Dependency would create a cycle: [{}] -> [{}].",
            task_id,
            depends_on_id
        );
    }
    conn.execute(
        "INSERT OR IGNORE INTO task_dependencies (task_id, depends_on_task_id, created_at)
         VALUES (?1, ?2, ?3)",
        params![task_id, depends_on_id, now_epoch_ms()],
    )?;
    conn.execute(
        "UPDATE tasks SET updated_at = ?2 WHERE id IN (?1, ?3)",
        params![task_id, now_epoch_ms(), depends_on_id],
    )?;
    Ok(())
}

fn introduces_cycle(conn: &rusqlite::Connection, task_id: i64, depends_on_id: i64) -> Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "WITH RECURSIVE path(id) AS (
                SELECT depends_on_task_id
                FROM task_dependencies
                WHERE task_id = ?1
                UNION
                SELECT td.depends_on_task_id
                FROM task_dependencies td
                JOIN path ON td.task_id = path.id
             )
             SELECT id
             FROM path
             WHERE id = ?2
             LIMIT 1",
            params![depends_on_id, task_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

fn ensure_task_exists(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    fetch_required_task(conn, id).map(|_| ())
}

fn fetch_required_task(conn: &rusqlite::Connection, id: i64) -> Result<TaskRow> {
    fetch_task(conn, id)?.ok_or_else(|| anyhow!("Task [{}] not found.", id))
}

fn fetch_task(conn: &rusqlite::Connection, id: i64) -> Result<Option<TaskRow>> {
    conn.query_row(
        "SELECT id, title, notes, scope, project, status, priority, created_at, updated_at, completed_at
         FROM tasks
         WHERE id = ?1",
        [id],
        |row| {
            Ok(TaskRow {
                id: row.get(0)?,
                title: row.get(1)?,
                notes: row.get(2)?,
                scope: row.get(3)?,
                project: row.get(4)?,
                status: row.get(5)?,
                priority: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
                completed_at: row.get(9)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn load_tasks(
    conn: &rusqlite::Connection,
    status: &str,
    limit: usize,
    scope_view: crate::scope::ScopeView,
    project: Option<&str>,
) -> Result<Vec<TaskRow>> {
    let status_clause = match status {
        "open" => "status = 'open'",
        "done" => "status = 'done'",
        "all" => "1=1",
        _ => bail!("Invalid status: {status}"),
    };
    let sql = match scope_view {
        crate::scope::ScopeView::Project => format!(
            "SELECT id, title, notes, scope, project, status, priority, created_at, updated_at, completed_at
             FROM tasks
             WHERE ({status_clause}) AND ((scope = 'project' AND project = ?1) OR scope = 'global')
             ORDER BY status ASC, priority DESC, created_at DESC
             LIMIT ?2"
        ),
        crate::scope::ScopeView::Global => format!(
            "SELECT id, title, notes, scope, project, status, priority, created_at, updated_at, completed_at
             FROM tasks
             WHERE ({status_clause}) AND scope = 'global'
             ORDER BY status ASC, priority DESC, created_at DESC
             LIMIT ?1"
        ),
        crate::scope::ScopeView::All => format!(
            "SELECT id, title, notes, scope, project, status, priority, created_at, updated_at, completed_at
             FROM tasks
             WHERE {status_clause}
             ORDER BY status ASC, priority DESC, created_at DESC
             LIMIT ?1"
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let mut map_row = |row: &rusqlite::Row<'_>| {
        Ok(TaskRow {
            id: row.get(0)?,
            title: row.get(1)?,
            notes: row.get(2)?,
            scope: row.get(3)?,
            project: row.get(4)?,
            status: row.get(5)?,
            priority: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            completed_at: row.get(9)?,
        })
    };
    let rows = match scope_view {
        crate::scope::ScopeView::Project => {
            stmt.query_map(params![project, limit as i64], &mut map_row)?
        }
        crate::scope::ScopeView::Global | crate::scope::ScopeView::All => {
            stmt.query_map(params![limit as i64], &mut map_row)?
        }
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn unfinished_dependency_count(conn: &rusqlite::Connection, task_id: i64) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*)
         FROM task_dependencies td
         JOIN tasks t ON t.id = td.depends_on_task_id
         WHERE td.task_id = ?1 AND t.status != 'done'",
        [task_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn task_dependencies(conn: &rusqlite::Connection, task_id: i64) -> Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.notes, t.scope, t.project, t.status, t.priority, t.created_at, t.updated_at, t.completed_at
         FROM task_dependencies td
         JOIN tasks t ON t.id = td.depends_on_task_id
         WHERE td.task_id = ?1
         ORDER BY t.status ASC, t.priority DESC, t.created_at DESC",
    )?;
    let rows = stmt.query_map([task_id], |row| {
        Ok(TaskRow {
            id: row.get(0)?,
            title: row.get(1)?,
            notes: row.get(2)?,
            scope: row.get(3)?,
            project: row.get(4)?,
            status: row.get(5)?,
            priority: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            completed_at: row.get(9)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn blocking_tasks(conn: &rusqlite::Connection, task_id: i64) -> Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.notes, t.scope, t.project, t.status, t.priority, t.created_at, t.updated_at, t.completed_at
         FROM task_dependencies td
         JOIN tasks t ON t.id = td.task_id
         WHERE td.depends_on_task_id = ?1
         ORDER BY t.status ASC, t.priority DESC, t.created_at DESC",
    )?;
    let rows = stmt.query_map([task_id], |row| {
        Ok(TaskRow {
            id: row.get(0)?,
            title: row.get(1)?,
            notes: row.get(2)?,
            scope: row.get(3)?,
            project: row.get(4)?,
            status: row.get(5)?,
            priority: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            completed_at: row.get(9)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
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
mod tests {
    use super::*;

    fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = crate::test_home_lock()
            .lock()
            .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
        let old_home = env::var_os("HOME");
        let temp_home = env::temp_dir().join(format!("sidekar-tasks-test-{}", now_epoch_ms()));
        fs::create_dir_all(&temp_home)?;
        unsafe { env::set_var("HOME", &temp_home) };
        let result = f();
        match old_home {
            Some(home) => unsafe { env::set_var("HOME", home) },
            None => unsafe { env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&temp_home);
        result
    }

    #[test]
    fn prevents_cycles() -> Result<()> {
        with_test_home(|| {
            let project = crate::scope::resolve_project_name(None);
            let a = insert_task("A", None, 0, crate::scope::PROJECT_SCOPE, Some(&project))?;
            let b = insert_task("B", None, 0, crate::scope::PROJECT_SCOPE, Some(&project))?;
            add_dependency(a, b)?;
            let err = add_dependency(b, a).expect_err("cycle should fail");
            assert!(err.to_string().contains("cycle"));
            Ok(())
        })
    }

    #[test]
    fn ready_list_hides_blocked_tasks() -> Result<()> {
        with_test_home(|| {
            let project = crate::scope::resolve_project_name(None);
            let a = insert_task("A", None, 0, crate::scope::PROJECT_SCOPE, Some(&project))?;
            let b = insert_task("B", None, 0, crate::scope::PROJECT_SCOPE, Some(&project))?;
            add_dependency(b, a)?;

            let mut ctx = AppContext::new()?;
            cmd_tasks(&mut ctx, &["list".into(), "--ready".into()])?;
            let output = ctx.drain_output();
            assert!(output.contains("[1]"));
            assert!(!output.contains("[2]"));

            update_task_status(a, "done")?;
            let mut ctx = AppContext::new()?;
            cmd_tasks(&mut ctx, &["list".into(), "--ready".into()])?;
            let output = ctx.drain_output();
            assert!(output.contains("[2]"));
            Ok(())
        })
    }

    #[test]
    fn project_list_includes_global_tasks_but_not_other_projects() -> Result<()> {
        with_test_home(|| {
            let current = crate::scope::resolve_project_name(None);
            let other = "other-project".to_string();
            let _project_task = insert_task(
                "project",
                None,
                0,
                crate::scope::PROJECT_SCOPE,
                Some(&current),
            )?;
            let _global_task = insert_task("global", None, 0, crate::scope::GLOBAL_SCOPE, None)?;
            let _other_task =
                insert_task("other", None, 0, crate::scope::PROJECT_SCOPE, Some(&other))?;

            let mut ctx = AppContext::new()?;
            cmd_tasks(&mut ctx, &["list".into()])?;
            let output = ctx.drain_output();
            assert!(output.contains("project"));
            assert!(output.contains("global [global]"));
            assert!(!output.contains("other"));
            Ok(())
        })
    }
}
