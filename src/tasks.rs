use crate::*;
use rusqlite::{OptionalExtension, params};

#[derive(Debug, Clone)]
struct TaskRow {
    id: i64,
    title: String,
    notes: Option<String>,
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
        "" => bail!(
            "Usage: sidekar tasks <add|list|done|reopen|delete|show|depend|undepend|deps> ..."
        ),
        other => bail!("Unknown tasks subcommand: {other}"),
    }
}

fn cmd_tasks_add(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let title = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .cloned()
        .context("Usage: sidekar tasks add <title> [--notes=...] [--priority=N]")?;
    let notes = extract_optional_value(args, "--notes=");
    let priority = extract_optional_value(args, "--priority=")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let id = insert_task(&title, notes.as_deref(), priority)?;
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

    let conn = crate::broker::open_db()?;
    let rows = load_tasks(&conn, &status, limit)?;
    let mut printed = false;
    for row in rows {
        let unfinished = unfinished_dependency_count(&conn, row.id)?;
        if ready_only && unfinished > 0 {
            continue;
        }
        if blocked_only && unfinished == 0 {
            continue;
        }
        printed = true;
        let marker = if row.status == "done" { 'x' } else { ' ' };
        let state = if unfinished > 0 {
            format!(" blocked-by={unfinished}")
        } else if row.status == "open" {
            " ready".to_string()
        } else {
            String::new()
        };
        out!(
            ctx,
            "[{}] [{}] p={} {}{}",
            row.id,
            marker,
            row.priority,
            row.title,
            state
        );
    }
    if !printed {
        out!(ctx, "No tasks found.");
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

fn insert_task(title: &str, notes: Option<&str>, priority: i64) -> Result<i64> {
    let conn = crate::broker::open_db()?;
    let now = now_epoch_ms();
    conn.execute(
        "INSERT INTO tasks (title, notes, status, priority, created_at, updated_at)
         VALUES (?1, ?2, 'open', ?3, ?4, ?4)",
        params![title.trim(), notes.map(str::trim), priority, now],
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
    ensure_task_exists(&conn, task_id)?;
    ensure_task_exists(&conn, depends_on_id)?;
    if task_id == depends_on_id {
        bail!("A task cannot depend on itself.");
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
    let exists: Option<i64> = conn
        .query_row("SELECT id FROM tasks WHERE id = ?1", [id], |row| row.get(0))
        .optional()?;
    if exists.is_none() {
        bail!("Task [{}] not found.", id);
    }
    Ok(())
}

fn fetch_task(conn: &rusqlite::Connection, id: i64) -> Result<Option<TaskRow>> {
    conn.query_row(
        "SELECT id, title, notes, status, priority, created_at, updated_at, completed_at
         FROM tasks
         WHERE id = ?1",
        [id],
        |row| {
            Ok(TaskRow {
                id: row.get(0)?,
                title: row.get(1)?,
                notes: row.get(2)?,
                status: row.get(3)?,
                priority: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                completed_at: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn load_tasks(conn: &rusqlite::Connection, status: &str, limit: usize) -> Result<Vec<TaskRow>> {
    let sql = match status {
        "open" => {
            "SELECT id, title, notes, status, priority, created_at, updated_at, completed_at
             FROM tasks
             WHERE status = 'open'
             ORDER BY priority DESC, created_at DESC
             LIMIT ?1"
        }
        "done" => {
            "SELECT id, title, notes, status, priority, created_at, updated_at, completed_at
             FROM tasks
             WHERE status = 'done'
             ORDER BY completed_at DESC, created_at DESC
             LIMIT ?1"
        }
        "all" => {
            "SELECT id, title, notes, status, priority, created_at, updated_at, completed_at
             FROM tasks
             ORDER BY status ASC, priority DESC, created_at DESC
             LIMIT ?1"
        }
        _ => bail!("Invalid status: {status}"),
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([limit as i64], |row| {
        Ok(TaskRow {
            id: row.get(0)?,
            title: row.get(1)?,
            notes: row.get(2)?,
            status: row.get(3)?,
            priority: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            completed_at: row.get(7)?,
        })
    })?;
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
        "SELECT t.id, t.title, t.notes, t.status, t.priority, t.created_at, t.updated_at, t.completed_at
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
            status: row.get(3)?,
            priority: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            completed_at: row.get(7)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn blocking_tasks(conn: &rusqlite::Connection, task_id: i64) -> Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.notes, t.status, t.priority, t.created_at, t.updated_at, t.completed_at
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
            status: row.get(3)?,
            priority: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            completed_at: row.get(7)?,
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
            let a = insert_task("A", None, 0)?;
            let b = insert_task("B", None, 0)?;
            add_dependency(a, b)?;
            let err = add_dependency(b, a).expect_err("cycle should fail");
            assert!(err.to_string().contains("cycle"));
            Ok(())
        })
    }

    #[test]
    fn ready_list_hides_blocked_tasks() -> Result<()> {
        with_test_home(|| {
            let a = insert_task("A", None, 0)?;
            let b = insert_task("B", None, 0)?;
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
}
