//! Localhost web UI for browsing `~/.sidekar/sidekar.sqlite3`.
//!
//! Served at `http://127.0.0.1:{http_port}/` on the daemon HTTP listener.
//! Binds localhost only; sensitive columns are masked by default.

use anyhow::Result;
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};
use std::collections::HashMap;

const UI_HTML: &str = include_str!("admin_ui.html");

#[derive(Clone, Copy)]
struct TableSpec {
    name: &'static str,
    group: &'static str,
    label: &'static str,
    order_by: &'static str,
}

const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "config",
        group: "secrets",
        label: "Config",
        order_by: "key ASC",
    },
    TableSpec {
        name: "kv_store",
        group: "secrets",
        label: "KV Store",
        order_by: "updated_at DESC",
    },
    TableSpec {
        name: "kv_history",
        group: "secrets",
        label: "KV History",
        order_by: "archived_at DESC",
    },
    TableSpec {
        name: "totp_secrets",
        group: "secrets",
        label: "TOTP",
        order_by: "service ASC, account ASC",
    },
    TableSpec {
        name: "encryption_meta",
        group: "secrets",
        label: "Encryption Meta",
        order_by: "key ASC",
    },
    TableSpec {
        name: "events",
        group: "logs",
        label: "Events",
        order_by: "id DESC",
    },
    TableSpec {
        name: "proxy_log",
        group: "logs",
        label: "Proxy Log",
        order_by: "id DESC",
    },
    TableSpec {
        name: "agents",
        group: "bus",
        label: "Agents",
        order_by: "last_seen_at DESC",
    },
    TableSpec {
        name: "bus_queue",
        group: "bus",
        label: "Bus Queue",
        order_by: "id DESC",
    },
    TableSpec {
        name: "pending_requests",
        group: "bus",
        label: "Pending Requests",
        order_by: "created_at DESC",
    },
    TableSpec {
        name: "outbound_requests",
        group: "bus",
        label: "Outbound Requests",
        order_by: "created_at DESC",
    },
    TableSpec {
        name: "bus_replies",
        group: "bus",
        label: "Bus Replies",
        order_by: "id DESC",
    },
    TableSpec {
        name: "agent_sessions",
        group: "bus",
        label: "Agent Sessions",
        order_by: "last_active_at DESC",
    },
    TableSpec {
        name: "cron_jobs",
        group: "jobs",
        label: "Cron Jobs",
        order_by: "created_at DESC",
    },
    TableSpec {
        name: "memory_events",
        group: "memory",
        label: "Memory Events",
        order_by: "updated_at DESC",
    },
    TableSpec {
        name: "memory_candidates",
        group: "memory",
        label: "Memory Candidates",
        order_by: "updated_at DESC",
    },
    TableSpec {
        name: "memory_events_usage",
        group: "memory",
        label: "Memory Usage",
        order_by: "created_at DESC",
    },
    TableSpec {
        name: "memory_journal_support",
        group: "memory",
        label: "Journal Support",
        order_by: "created_at DESC",
    },
    TableSpec {
        name: "memory_import_log",
        group: "memory",
        label: "Memory Import Log",
        order_by: "imported_at DESC",
    },
    TableSpec {
        name: "session_journals",
        group: "memory",
        label: "Session Journals",
        order_by: "created_at DESC",
    },
    TableSpec {
        name: "tasks",
        group: "tasks",
        label: "Tasks",
        order_by: "updated_at DESC",
    },
    TableSpec {
        name: "task_dependencies",
        group: "tasks",
        label: "Task Dependencies",
        order_by: "created_at DESC",
    },
    TableSpec {
        name: "repl_sessions",
        group: "repl",
        label: "REPL Sessions",
        order_by: "updated_at DESC",
    },
    TableSpec {
        name: "repl_entries",
        group: "repl",
        label: "REPL Entries",
        order_by: "created_at DESC",
    },
    TableSpec {
        name: "repl_input_history",
        group: "repl",
        label: "REPL Input History",
        order_by: "id DESC",
    },
];

const GROUP_LABELS: &[(&str, &str)] = &[
    ("secrets", "Secrets & config"),
    ("logs", "Logs"),
    ("bus", "Agent bus"),
    ("jobs", "Jobs"),
    ("memory", "Memory & journals"),
    ("tasks", "Tasks"),
    ("repl", "REPL"),
];

fn table_spec(name: &str) -> Option<&'static TableSpec> {
    TABLES.iter().find(|t| t.name == name)
}

fn parse_query_params(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            Some((k, v)) => {
                out.insert(
                    urlencoding::decode(k)
                        .unwrap_or_else(|_| k.into())
                        .into_owned(),
                    urlencoding::decode(v)
                        .unwrap_or_else(|_| v.into())
                        .into_owned(),
                );
            }
            None => {
                out.insert(pair.to_string(), String::new());
            }
        }
    }
    out
}

fn sql_value_to_json(v: SqlValue) -> Value {
    match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => json!(i),
        SqlValue::Real(f) => json!(f),
        SqlValue::Text(s) => Value::String(s),
        SqlValue::Blob(b) => json!({
            "blob": true,
            "bytes": b.len(),
        }),
    }
}

fn mask_string(s: &str) -> Value {
    json!({
        "masked": true,
        "len": s.len(),
    })
}

fn truncate_string(s: &str, max: usize) -> Value {
    if s.len() <= max {
        Value::String(s.to_string())
    } else {
        Value::String(format!(
            "{}… [truncated, {} bytes total]",
            &s[..max],
            s.len()
        ))
    }
}

fn config_value_redacted(key: &str, _value: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.starts_with("auth:")
        || k.contains("token")
        || k.contains("secret")
        || k.contains("password")
        || k.contains("credential")
}

fn redact_cell(table: &str, column: &str, row: &HashMap<String, Value>, raw: Value) -> Value {
    match (table, column) {
        ("kv_store", "value") | ("kv_history", "value") | ("totp_secrets", "secret") => {
            if let Value::String(s) = &raw {
                mask_string(s)
            } else {
                json!({"masked": true})
            }
        }
        ("encryption_meta", "value") => {
            if let Value::String(s) = &raw {
                mask_string(s)
            } else {
                json!({"masked": true})
            }
        }
        ("config", "value") => {
            let key = row.get("key").and_then(|v| v.as_str()).unwrap_or("");
            if let Value::String(s) = &raw {
                if config_value_redacted(key, s) {
                    mask_string(s)
                } else {
                    truncate_string(s, 500)
                }
            } else {
                raw
            }
        }
        ("proxy_log", "request_body") | ("proxy_log", "response_body") => raw,
        ("repl_entries", "content")
        | ("pending_requests", "envelope_json")
        | ("bus_replies", "envelope_json")
        | ("cron_jobs", "action_json")
        | ("session_journals", "structured_json")
        | ("memory_candidates", "detail_json") => {
            if let Value::String(s) = &raw {
                truncate_string(s, 800)
            } else {
                raw
            }
        }
        ("repl_input_history", "line") => {
            if let Value::String(s) = &raw {
                truncate_string(s, 200)
            } else {
                raw
            }
        }
        _ => {
            if let Value::String(s) = &raw {
                if s.len() > 1200 {
                    truncate_string(s, 1200)
                } else {
                    raw
                }
            } else {
                raw
            }
        }
    }
}

fn query_table_rows(
    conn: &Connection,
    spec: &TableSpec,
    limit: usize,
    offset: usize,
) -> Result<(i64, Vec<String>, Vec<HashMap<String, Value>>)> {
    let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {}", spec.name), [], |r| {
        r.get(0)
    })?;

    let sql = format!(
        "SELECT * FROM {} ORDER BY {} LIMIT ?1 OFFSET ?2",
        spec.name, spec.order_by
    );
    let mut stmt = conn.prepare(&sql)?;
    let col_count = stmt.column_count();
    let mut columns = Vec::with_capacity(col_count);
    for i in 0..col_count {
        columns.push(stmt.column_name(i)?.to_string());
    }

    let lim = limit.clamp(1, 200) as i64;
    let off = offset as i64;
    let rows_iter = stmt.query_map([lim, off], |row| {
        let mut map = HashMap::new();
        for (i, col) in columns.iter().enumerate() {
            let raw = sql_value_to_json(row.get::<_, SqlValue>(i)?);
            map.insert(col.clone(), raw);
        }
        Ok(map)
    })?;

    let mut rows = Vec::new();
    for row in rows_iter {
        let mut map = row?;
        let redacted = map
            .iter()
            .map(|(col, val)| (col.clone(), redact_cell(spec.name, col, &map, val.clone())))
            .collect::<HashMap<_, _>>();
        map = redacted;
        rows.push(map);
    }

    Ok((count, columns, rows))
}

fn pid_from_pane(pane: &str) -> Option<i32> {
    for prefix in ["pty-", "repl-", "cli-"] {
        if let Some(pid_str) = pane.strip_prefix(prefix) {
            return pid_str.parse().ok();
        }
    }
    None
}

fn process_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn agent_kind(agent: &crate::broker::BrokerAgent) -> &'static str {
    match agent.id.pane.as_deref() {
        Some(p) if p.starts_with("pty-") => "pty",
        Some(p) if p.starts_with("repl-") => "repl",
        Some(p) if p.starts_with("cli-") => "cli",
        _ => "other",
    }
}

fn agent_to_json(agent: &crate::broker::BrokerAgent) -> Value {
    let pid = agent.id.pane.as_deref().and_then(pid_from_pane);
    let alive = pid.map(process_alive).unwrap_or(false);
    json!({
        "name": agent.id.name,
        "nick": agent.id.nick,
        "kind": agent_kind(agent),
        "agent_type": agent.id.agent_type,
        "session": agent.id.session,
        "pane": agent.id.pane,
        "cwd": agent.cwd,
        "pid": pid,
        "alive": alive,
        "registered_at": agent.registered_at,
        "last_seen_at": agent.last_seen_at,
    })
}

fn agent_session_to_json(s: &crate::broker::AgentSessionRecord) -> Value {
    json!({
        "id": s.id,
        "agent_name": s.agent_name,
        "agent_type": s.agent_type,
        "nick": s.nick,
        "project": s.project,
        "channel": s.channel,
        "cwd": s.cwd,
        "started_at": s.started_at,
        "ended_at": s.ended_at,
        "last_active_at": s.last_active_at,
        "request_count": s.request_count,
        "reply_count": s.reply_count,
        "message_count": s.message_count,
    })
}

fn proxy_entry_summary(row: &crate::broker::ProxyLogRow) -> Value {
    json!({
        "id": row.id,
        "created_at": row.created_at,
        "status": row.status,
        "method": row.method,
        "path": row.path,
        "upstream_host": row.upstream_host,
        "response_status": row.response_status,
        "duration_ms": row.duration_ms,
        "request_size": row.request_body.len(),
        "response_size": row.response_body.len(),
    })
}

fn body_json(bytes: &[u8], max_chars: usize) -> Value {
    if let Ok(s) = std::str::from_utf8(bytes) {
        if s.len() > max_chars {
            json!({
                "encoding": "utf8",
                "truncated": true,
                "data": &s[..max_chars],
                "total_bytes": bytes.len(),
            })
        } else {
            json!({
                "encoding": "utf8",
                "truncated": false,
                "data": s,
                "total_bytes": bytes.len(),
            })
        }
    } else {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        if b64.len() > max_chars {
            json!({
                "encoding": "base64",
                "truncated": true,
                "data": &b64[..max_chars],
                "total_bytes": bytes.len(),
            })
        } else {
            json!({
                "encoding": "base64",
                "truncated": false,
                "data": b64,
                "total_bytes": bytes.len(),
            })
        }
    }
}

pub fn runtime_json(ext_status: Value) -> Result<Value> {
    let conn = crate::broker::open()?;
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let db_path = crate::broker::db_path();
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

    let logged_in = crate::auth::auth_token().is_some();
    let auth_created_at = crate::broker::auth_get("created_at");
    let encryption_configured: i64 =
        conn.query_row("SELECT COUNT(*) FROM encryption_meta", [], |r| r.get(0))?;

    let agents = crate::broker::list_agents(None).unwrap_or_default();
    let mut pty = Vec::new();
    let mut repl = Vec::new();
    let mut other = Vec::new();
    for agent in &agents {
        let alive = agent
            .id
            .pane
            .as_deref()
            .and_then(pid_from_pane)
            .map(process_alive)
            .unwrap_or(false);
        if !alive {
            continue;
        }
        match agent_kind(agent) {
            "pty" => pty.push(agent_to_json(agent)),
            "repl" => repl.push(agent_to_json(agent)),
            _ => other.push(agent_to_json(agent)),
        }
    }

    let agent_sessions = crate::broker::list_agent_sessions(true, None, 50)?
        .into_iter()
        .map(|s| agent_session_to_json(&s))
        .collect::<Vec<_>>();

    let repl_persisted_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM repl_sessions", [], |r| r.get(0))?;

    let (proxy_total, proxy_latest): (i64, Option<i64>) =
        conn.query_row("SELECT COUNT(*), MAX(created_at) FROM proxy_log", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;

    Ok(json!({
        "sidekar_version": env!("CARGO_PKG_VERSION"),
        "db_path": db_path.display().to_string(),
        "db_size_bytes": db_size,
        "schema_version": schema_version,
        "device": {
            "logged_in": logged_in,
            "auth_created_at": auth_created_at,
            "encryption_configured": encryption_configured > 0,
        },
        "extension": ext_status,
        "sessions": {
            "pty": pty,
            "repl": repl,
            "other": other,
            "agent_sessions_open": agent_sessions,
            "repl_persisted_count": repl_persisted_count,
        },
        "proxy": {
            "storage": "proxy_log table in sidekar.sqlite3",
            "note": "MITM runs inside REPL/PTY process (--proxy or /proxy on). Captured traffic is written here; daemon reads the shared DB.",
            "total_entries": proxy_total,
            "latest_at": proxy_latest,
        },
    }))
}

pub fn proxy_log_json(limit: usize, offset: usize) -> Result<Value> {
    let (total, rows) = crate::broker::proxy_log_page(limit, offset)?;
    let max_id = crate::broker::proxy_log_max_id()?;
    let page: Vec<Value> = rows.iter().map(proxy_entry_summary).collect();
    Ok(json!({
        "mode": "page",
        "total": total,
        "max_id": max_id,
        "limit": limit,
        "offset": offset,
        "items": page,
    }))
}

pub fn proxy_log_tail_json(since_id: i64, ids: &[i64], limit: usize) -> Result<Value> {
    let new_rows = if since_id > 0 {
        crate::broker::proxy_log_since(since_id, limit)?
    } else {
        Vec::new()
    };
    let updated = if ids.is_empty() {
        Vec::new()
    } else {
        crate::broker::proxy_log_by_ids(ids)?
    };
    let conn = crate::broker::open()?;
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM proxy_log", [], |r| r.get(0))?;
    let max_id = crate::broker::proxy_log_max_id()?;
    Ok(json!({
        "mode": "tail",
        "total": total,
        "max_id": max_id,
        "new": new_rows.iter().map(proxy_entry_summary).collect::<Vec<_>>(),
        "updated": updated.iter().map(proxy_entry_summary).collect::<Vec<_>>(),
    }))
}

pub fn proxy_show_json(id: i64) -> Result<Value> {
    let row = crate::broker::proxy_log_detail(id)?
        .ok_or_else(|| anyhow::anyhow!("no proxy log entry with id {id}"))?;
    Ok(json!({
        "id": row.id,
        "created_at": row.created_at,
        "status": row.status,
        "method": row.method,
        "path": row.path,
        "upstream_host": row.upstream_host,
        "response_status": row.response_status,
        "duration_ms": row.duration_ms,
        "request_headers": row.request_headers,
        "response_headers": row.response_headers,
        "request_body": body_json(&row.request_body, 500_000),
        "response_body": body_json(&row.response_body, 500_000),
    }))
}

pub fn overview_json(http_port: u16) -> Result<Value> {
    let conn = crate::broker::open()?;
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let db_path = crate::broker::db_path();
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "sidekar_version": env!("CARGO_PKG_VERSION"),
        "db_path": db_path.display().to_string(),
        "db_size_bytes": db_size,
        "schema_version": schema_version,
        "cli_logged_in": crate::auth::auth_token().is_some(),
        "http_port": http_port,
        "url": format!("http://127.0.0.1:{http_port}"),
    }))
}

pub fn tables_json() -> Result<Value> {
    let conn = crate::broker::open()?;
    let mut groups: HashMap<&str, Vec<Value>> = HashMap::new();
    for spec in TABLES {
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {}", spec.name), [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        groups.entry(spec.group).or_default().push(json!({
            "name": spec.name,
            "label": spec.label,
            "count": count,
        }));
    }

    let group_list: Vec<Value> = GROUP_LABELS
        .iter()
        .filter_map(|(id, label)| {
            let tables = groups.remove(*id)?;
            Some(json!({
                "id": id,
                "label": label,
                "tables": tables,
            }))
        })
        .collect();

    Ok(json!({ "groups": group_list }))
}

pub fn rows_json(table: &str, limit: usize, offset: usize) -> Result<Value> {
    let spec = table_spec(table).ok_or_else(|| anyhow::anyhow!("unknown table: {table}"))?;
    let conn = crate::broker::open()?;
    let (total, columns, rows) = query_table_rows(&conn, spec, limit, offset)?;
    let json_rows: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for col in &columns {
                if let Some(v) = row.get(col) {
                    obj.insert(col.clone(), v.clone());
                }
            }
            Value::Object(obj)
        })
        .collect();
    Ok(json!({
        "table": spec.name,
        "label": spec.label,
        "group": spec.group,
        "total": total,
        "limit": limit.clamp(1, 200),
        "offset": offset,
        "columns": columns,
        "rows": json_rows,
    }))
}

pub fn ui_html() -> &'static str {
    UI_HTML
}

fn is_ui_route(path: &str) -> bool {
    path == "/" || path.starts_with("/api/")
}

/// Handle web UI HTTP routes. Returns true if the request was handled.
pub async fn handle_admin_request(
    method: &str,
    path: &str,
    query: &str,
    http_port: u16,
    ext_status: Value,
    stream: &mut tokio::net::TcpStream,
) -> bool {
    if !is_ui_route(path) {
        return false;
    }

    if !method.eq_ignore_ascii_case("GET") {
        write_http_response(
            stream,
            405,
            "text/plain; charset=utf-8",
            "Method Not Allowed",
        )
        .await;
        return true;
    }

    match path {
        "/" => {
            write_http_response(stream, 200, "text/html; charset=utf-8", ui_html()).await;
        }
        "/api/runtime" => {
            let ext = ext_status.clone();
            match tokio::task::spawn_blocking(move || runtime_json(ext)).await {
                Ok(Ok(body)) => write_json_response(stream, 200, &body).await,
                Ok(Err(e)) => {
                    write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                }
                Err(e) => {
                    write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                }
            }
        }
        "/api/proxy/log" => {
            let params = parse_query_params(query);
            let limit = params
                .get("limit")
                .and_then(|s| s.parse().ok())
                .unwrap_or(50);
            if params.contains_key("since_id") || params.contains_key("ids") {
                let since_id = params
                    .get("since_id")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let ids: Vec<i64> = params
                    .get("ids")
                    .map(|s| s.split(',').filter_map(|p| p.trim().parse().ok()).collect())
                    .unwrap_or_default();
                match tokio::task::spawn_blocking(move || {
                    proxy_log_tail_json(since_id, &ids, limit)
                })
                .await
                {
                    Ok(Ok(body)) => write_json_response(stream, 200, &body).await,
                    Ok(Err(e)) => {
                        write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                    }
                    Err(e) => {
                        write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                    }
                }
            } else {
                let offset = params
                    .get("offset")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                match tokio::task::spawn_blocking(move || proxy_log_json(limit, offset)).await {
                    Ok(Ok(body)) => write_json_response(stream, 200, &body).await,
                    Ok(Err(e)) => {
                        write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                    }
                    Err(e) => {
                        write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                    }
                }
            }
        }
        "/api/proxy/show" => {
            let params = parse_query_params(query);
            let id = params
                .get("id")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            match tokio::task::spawn_blocking(move || proxy_show_json(id)).await {
                Ok(Ok(body)) => write_json_response(stream, 200, &body).await,
                Ok(Err(e)) => {
                    let status = if e.to_string().contains("no proxy log entry") {
                        404
                    } else {
                        500
                    };
                    write_json_response(stream, status, &json!({"error": format!("{e:#}")})).await
                }
                Err(e) => {
                    write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                }
            }
        }
        "/api/overview" => {
            match tokio::task::spawn_blocking(move || overview_json(http_port)).await {
                Ok(Ok(body)) => write_json_response(stream, 200, &body).await,
                Ok(Err(e)) => {
                    write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                }
                Err(e) => {
                    write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                }
            }
        }
        "/api/tables" => match tokio::task::spawn_blocking(tables_json).await {
            Ok(Ok(body)) => write_json_response(stream, 200, &body).await,
            Ok(Err(e)) => {
                write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
            }
            Err(e) => write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await,
        },
        "/api/rows" => {
            let params = parse_query_params(query);
            let table = params.get("table").cloned().unwrap_or_default();
            let limit = params
                .get("limit")
                .and_then(|s| s.parse().ok())
                .unwrap_or(50);
            let offset = params
                .get("offset")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let table_for_task = table.clone();
            match tokio::task::spawn_blocking(move || rows_json(&table_for_task, limit, offset))
                .await
            {
                Ok(Ok(body)) => write_json_response(stream, 200, &body).await,
                Ok(Err(e)) => {
                    let status = if e.to_string().contains("unknown table") {
                        404
                    } else {
                        500
                    };
                    write_json_response(stream, status, &json!({"error": format!("{e:#}")})).await
                }
                Err(e) => {
                    write_json_response(stream, 500, &json!({"error": format!("{e:#}")})).await
                }
            }
        }
        _ => write_http_response(stream, 404, "text/plain; charset=utf-8", "Not Found").await,
    }

    true
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) {
    use tokio::io::AsyncWriteExt;
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn write_json_response(stream: &mut tokio::net::TcpStream, status: u16, body: &Value) {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
    write_http_response(stream, status, "application/json; charset=utf-8", &text).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_allowlist_has_unique_names() {
        let mut seen = std::collections::HashSet::new();
        for t in TABLES {
            assert!(seen.insert(t.name), "duplicate table {}", t.name);
        }
    }

    #[test]
    fn config_redaction_detects_auth_keys() {
        assert!(config_value_redacted("auth:token", "abc"));
        assert!(config_value_redacted("oauth:refresh_token", "x"));
        assert!(!config_value_redacted("browser", "chrome"));
        assert!(!config_value_redacted("telemetry", "true"));
    }

    #[test]
    fn parse_query_params_decodes() {
        let m = parse_query_params("table=events&limit=10&offset=0");
        assert_eq!(m.get("table").map(String::as_str), Some("events"));
        assert_eq!(m.get("limit").map(String::as_str), Some("10"));
    }

    #[test]
    fn unknown_table_errors() {
        let err = rows_json("not_a_real_table;", 10, 0).unwrap_err();
        assert!(err.to_string().contains("unknown table"));
    }
}
