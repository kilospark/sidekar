use super::*;

#[derive(Debug, Clone)]
pub struct CronJobRecord {
    pub id: String,
    pub name: Option<String>,
    pub schedule: String,
    pub action_json: String,
    pub target: String,
    pub created_by: String,
    pub created_at: u64,
    pub last_run_at: Option<u64>,
    pub run_count: u64,
    pub error_count: u64,
    pub last_error: Option<String>,
    pub active: bool,
    pub once: bool,
    pub project: Option<String>,
    pub loop_interval_secs: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
pub fn create_cron_job(
    id: &str,
    name: Option<&str>,
    schedule: &str,
    action_json: &str,
    target: &str,
    created_by: &str,
    once: bool,
    project: Option<&str>,
    loop_interval_secs: Option<u64>,
) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    conn.execute(
        "INSERT INTO cron_jobs (id, name, schedule, action_json, target, created_by, created_at, active, once, project, loop_interval_secs)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?10)",
        params![id, name, schedule, action_json, target, created_by, now, once as i64, project, loop_interval_secs.map(|v| v as i64)],
    )?;
    Ok(())
}

pub fn list_cron_jobs(
    active_only: bool,
    scope: crate::scope::ScopeView,
    current_project: &str,
) -> Result<Vec<CronJobRecord>> {
    let conn = open()?;
    let (where_clause, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = {
        let mut clauses = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if active_only {
            clauses.push("active = 1".to_string());
        }

        match scope {
            crate::scope::ScopeView::Project => {
                clauses.push(format!(
                    "(project = ?{} OR project IS NULL)",
                    params.len() + 1
                ));
                params.push(Box::new(current_project.to_string()));
            }
            crate::scope::ScopeView::Global => {
                clauses.push("project = 'global'".to_string());
            }
            crate::scope::ScopeView::All => {} // no filter
        }

        let where_str = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        (where_str, params)
    };

    let sql = format!(
        "SELECT id, name, schedule, action_json, target, created_by, created_at,
                last_run_at, run_count, error_count, last_error, active, once, project, loop_interval_secs
         FROM cron_jobs{where_clause} ORDER BY created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params_vec.iter()))?;
    let mut jobs = Vec::new();
    while let Some(row) = rows.next()? {
        jobs.push(row_to_cron_job(row)?);
    }
    Ok(jobs)
}

pub fn get_cron_job(id: &str) -> Result<Option<CronJobRecord>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT id, name, schedule, action_json, target, created_by, created_at,
                last_run_at, run_count, error_count, last_error, active, once, project, loop_interval_secs
         FROM cron_jobs WHERE id = ?1 LIMIT 1",
    )?;
    stmt.query_row(params![id], row_to_cron_job)
        .optional()
        .map_err(Into::into)
}

pub fn delete_cron_job(id: &str) -> Result<bool> {
    let conn = open()?;
    let rows = conn.execute(
        "UPDATE cron_jobs SET active = 0 WHERE id = ?1 AND active = 1",
        params![id],
    )?;
    Ok(rows > 0)
}

pub fn update_cron_job_run(id: &str, error: Option<&str>) -> Result<()> {
    let conn = open()?;
    let now = crate::message::epoch_secs() as i64;
    if let Some(err_msg) = error {
        conn.execute(
            "UPDATE cron_jobs SET last_run_at = ?2, run_count = run_count + 1,
             error_count = error_count + 1, last_error = ?3 WHERE id = ?1",
            params![id, now, err_msg],
        )?;
    } else {
        conn.execute(
            "UPDATE cron_jobs SET last_run_at = ?2, run_count = run_count + 1,
             last_error = NULL WHERE id = ?1",
            params![id, now],
        )?;
    }
    Ok(())
}

fn row_to_cron_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<CronJobRecord> {
    Ok(CronJobRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        schedule: row.get(2)?,
        action_json: row.get(3)?,
        target: row.get(4)?,
        created_by: row.get(5)?,
        created_at: row.get::<_, i64>(6)? as u64,
        last_run_at: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        run_count: row.get::<_, i64>(8)? as u64,
        error_count: row.get::<_, i64>(9)? as u64,
        last_error: row.get(10)?,
        active: row.get::<_, i64>(11)? != 0,
        once: row
            .get::<_, Option<i64>>(12)
            .unwrap_or(Some(0))
            .unwrap_or(0)
            != 0,
        project: row.get::<_, Option<String>>(13).unwrap_or(None),
        loop_interval_secs: row
            .get::<_, Option<i64>>(14)
            .unwrap_or(None)
            .map(|v| v as u64),
    })
}
