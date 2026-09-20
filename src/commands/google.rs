//! `sidekar google`, `gmail`, `drive` and `calendar`.

use crate::AppContext;
use crate::google;
use crate::out;
use anyhow::{Result, bail};

pub async fn cmd_google(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("login") => {
            let account = google::auth::login().await?;
            if account.is_empty() {
                out!(ctx, "Signed in to Google.");
            } else {
                out!(ctx, "Signed in to Google as {account}.");
            }
            Ok(())
        }
        Some("status") => {
            if !google::auth::is_logged_in() {
                out!(ctx, "Not signed in. Run `sidekar google login`.");
                return Ok(());
            }
            let who = google::auth::logged_in_account()?.unwrap_or_else(|| "(unknown)".into());
            out!(ctx, "Signed in as {who}");
            out!(ctx, "Scopes: {}", google::auth::SCOPES.join(" "));
            Ok(())
        }
        Some("logout") => {
            google::auth::forget()?;
            out!(
                ctx,
                "Forgot the stored token. The grant still exists at \
                 myaccount.google.com/permissions until you revoke it there."
            );
            Ok(())
        }
        _ => bail!("Usage: sidekar google <login|status|logout>"),
    }
}

pub async fn cmd_gmail(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    match sub {
        "search" => {
            let limit = flag_usize(rest, "--limit").unwrap_or(10);
            let query = positional(rest).join(" ");
            if query.is_empty() {
                bail!("Usage: sidekar gmail search <query> [--limit N]  (Gmail query syntax)");
            }
            let found = google::gmail::search(&query, limit).await?;
            if found.is_empty() {
                out!(ctx, "No messages match {query}.");
            }
            for m in found {
                out!(ctx, "{}\t{}\t{}\t{}", m.id, m.date, m.from, m.subject);
                if !m.snippet.is_empty() {
                    out!(ctx, "    {}", m.snippet);
                }
            }
            Ok(())
        }
        "read" => {
            let id = positional(rest)
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar gmail read <message-id>"))?;
            out!(ctx, "{}", google::gmail::read(&id).await?);
            Ok(())
        }
        "send" => {
            let to = flag(rest, "--to").ok_or_else(|| anyhow::anyhow!("gmail send needs --to"))?;
            let subject = flag(rest, "--subject").unwrap_or_default();
            let body = flag(rest, "--body").unwrap_or_default();
            if body.is_empty() {
                bail!("gmail send needs --body (use --body \"$(cat file)\" for long text)");
            }
            let id = google::gmail::send(&to, &subject, &body).await?;
            out!(ctx, "Sent to {to} (id {id}).");
            Ok(())
        }
        "labels" => {
            for l in google::gmail::labels().await? {
                out!(ctx, "{l}");
            }
            Ok(())
        }
        "modify" => {
            let id = positional(rest).first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar gmail modify <id> [--add L] [--remove L]")
            })?;
            let add: Vec<String> = flag(rest, "--add").into_iter().collect();
            let remove: Vec<String> = flag(rest, "--remove").into_iter().collect();
            google::gmail::modify(&id, &add, &remove).await?;
            out!(ctx, "Updated {id}.");
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar gmail <search|read|send|labels|modify> …\n  \
             search <query> [--limit N]      Gmail query syntax: from:, is:unread, newer_than:2d\n  \
             read <id>\n  \
             send --to <addr> --subject <s> --body <text>\n  \
             labels\n  \
             modify <id> [--add LABEL] [--remove LABEL]   (UNREAD is a label)"
        ),
    }
}

pub async fn cmd_drive(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    match sub {
        "ls" | "search" => {
            let limit = flag_usize(rest, "--limit").unwrap_or(25);
            let query = positional(rest).join(" ");
            let entries = google::drive::list(&query, limit).await?;
            if entries.is_empty() {
                out!(ctx, "Nothing found.");
            }
            for e in entries {
                out!(
                    ctx,
                    "{}\t{}\t{}\t{}",
                    e.id,
                    google::drive::human_size(e.size.as_deref()),
                    e.modified,
                    e.name
                );
                let _ = &e.mime;
            }
            Ok(())
        }
        "get" => {
            let id = positional(rest).first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar drive get <file-id> [--out path]")
            })?;
            let text = google::drive::get_text(&id).await?;
            match flag(rest, "--out") {
                Some(path) => {
                    std::fs::write(&path, text.as_bytes())?;
                    out!(ctx, "Wrote {} bytes to {path}.", text.len());
                }
                None => out!(ctx, "{text}"),
            }
            Ok(())
        }
        "put" => {
            let path = positional(rest).first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar drive put <path> [--name n] [--folder id]")
            })?;
            let id = google::drive::put(
                &path,
                flag(rest, "--name").as_deref(),
                flag(rest, "--folder").as_deref(),
            )
            .await?;
            out!(ctx, "Uploaded as {id}.");
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar drive <ls|get|put> …\n  \
             ls [query] [--limit N]        bare words match names; Drive query syntax also works\n  \
             get <file-id> [--out path]    Docs export as text, Sheets as CSV\n  \
             put <path> [--name n] [--folder id]"
        ),
    }
}

pub async fn cmd_calendar(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let cal = flag(rest, "--calendar").unwrap_or_else(|| "primary".into());
    match sub {
        "list" | "" => {
            let days = flag_usize(rest, "--days").unwrap_or(7) as u32;
            let limit = flag_usize(rest, "--limit").unwrap_or(25);
            let events = google::calendar::list(&cal, days, limit).await?;
            if events.is_empty() {
                out!(ctx, "Nothing scheduled in the next {days} days.");
            }
            for e in events {
                out!(
                    ctx,
                    "{}\t{} → {}\t{} ({} attendees)",
                    e.id,
                    e.start,
                    e.end,
                    e.summary,
                    e.attendees
                );
            }
            Ok(())
        }
        "create" => {
            let summary = flag(rest, "--summary")
                .ok_or_else(|| anyhow::anyhow!("calendar create needs --summary"))?;
            let start = flag(rest, "--start")
                .ok_or_else(|| anyhow::anyhow!("calendar create needs --start"))?;
            let end = flag(rest, "--end")
                .ok_or_else(|| anyhow::anyhow!("calendar create needs --end"))?;
            let attendees: Vec<String> = flag(rest, "--attendees")
                .map(|a| a.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let id = google::calendar::create(&cal, &summary, &start, &end, &attendees).await?;
            out!(ctx, "Created {id}.");
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar calendar <list|create> …\n  \
             list [--days N] [--limit N] [--calendar id]\n  \
             create --summary <s> --start <t> --end <t> [--attendees a,b] [--calendar id]\n  \
             times are RFC3339 (2026-09-20T14:00:00-04:00) or a bare date for all-day"
        ),
    }
}

/// `--name value` or `--name=value`.
fn flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&prefix) {
            return Some(v.to_string());
        }
        if a == name {
            return args.get(i + 1).cloned();
        }
    }
    None
}

fn flag_usize(args: &[String], name: &str) -> Option<usize> {
    flag(args, name).and_then(|v| v.parse().ok())
}

/// Arguments that are neither a flag nor a flag's value.
fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            // `--flag=value` carries its value; `--flag value` eats the next one.
            skip_next = !a.contains('=');
            continue;
        }
        out.push(a.clone());
    }
    out
}

#[cfg(test)]
mod tests;
