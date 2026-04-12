use super::*;

pub(super) fn insert_task(
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

pub(super) fn update_task_status(id: i64, status: &str) -> Result<()> {
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

pub(super) fn add_dependency(task_id: i64, depends_on_id: i64) -> Result<()> {
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

pub(super) fn fetch_required_task(conn: &rusqlite::Connection, id: i64) -> Result<TaskRow> {
    fetch_task(conn, id)?.ok_or_else(|| anyhow!("Task [{}] not found.", id))
}

pub(super) fn fetch_task(conn: &rusqlite::Connection, id: i64) -> Result<Option<TaskRow>> {
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

pub(super) fn load_tasks(
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

pub(super) fn unfinished_dependency_count(
    conn: &rusqlite::Connection,
    task_id: i64,
) -> Result<i64> {
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

pub(super) fn task_dependencies(conn: &rusqlite::Connection, task_id: i64) -> Result<Vec<TaskRow>> {
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

pub(super) fn blocking_tasks(conn: &rusqlite::Connection, task_id: i64) -> Result<Vec<TaskRow>> {
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
