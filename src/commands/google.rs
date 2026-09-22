//! `sidekar google`, `gmail`, `drive` and `calendar`.

use crate::AppContext;
use crate::google;
use anyhow::{Result, bail};

pub async fn cmd_google(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        Some("login") => {
            let token_key = flag(rest, "--token").ok_or_else(|| {
                anyhow::anyhow!(
                    "login needs --token <KV_KEY>, the key the refresh token will be stored under"
                )
            })?;
            let client_id_key = flag(rest, "--client-id")
                .ok_or_else(|| anyhow::anyhow!("login needs --client-id <KV_KEY>"))?;
            let client_secret_key = flag(rest, "--client-secret")
                .ok_or_else(|| anyhow::anyhow!("login needs --client-secret <KV_KEY>"))?;
            let open_browser = !rest
                .iter()
                .any(|a| a == "--no-browser" || a == "--print-url");
            let who = google::auth::login(
                &token_key,
                &client_id_key,
                &client_secret_key,
                flag(rest, "--account").as_deref(),
                open_browser,
            )
            .await?;
            if who.is_empty() {
                out!(ctx, "Stored a token under {token_key}.");
            } else {
                out!(ctx, "Stored a token for {who} under {token_key}.");
            }
            Ok(())
        }
        Some("list") | Some("accounts") => {
            let tokens = google::auth::tokens()?;
            if tokens.is_empty() {
                out!(ctx, "No Google tokens stored.");
                return Ok(());
            }
            let default = google::auth::default_token_key()?;
            for t in tokens {
                let marker = if Some(&t.key) == default.as_ref() {
                    "*"
                } else {
                    " "
                };
                // Which client minted it matters: one account can be reachable
                // through one client and refused by another.
                out!(
                    ctx,
                    "{marker} {}\t{}\tclient={}",
                    t.key,
                    if t.account.is_empty() {
                        "(unknown account)"
                    } else {
                        &t.account
                    },
                    t.client_id_key
                );
            }
            Ok(())
        }
        Some("use") => {
            let key = positional(rest)
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar google use <KV_KEY>"))?;
            let known = google::auth::tokens()?;
            if !known.iter().any(|t| t.key == key) {
                bail!("no Google token stored under {key}; `sidekar google list` shows them");
            }
            google::auth::set_default_token_key(&key)?;
            out!(ctx, "Default token is now {key}.");
            Ok(())
        }
        Some("status") => {
            let tokens = google::auth::tokens()?;
            if tokens.is_empty() {
                out!(
                    ctx,
                    "No Google tokens stored. Run `sidekar google login --help`."
                );
                return Ok(());
            }
            let active = google::auth::resolve_token(flag(rest, "--token").as_deref())?;
            out!(
                ctx,
                "Active: {} ({})",
                active.key,
                if active.account.is_empty() {
                    "unknown account"
                } else {
                    &active.account
                }
            );
            out!(ctx, "Client: {}", active.client_id_key);
            out!(ctx, "Stored: {}", tokens.len());
            out!(ctx, "Scopes: {}", google::auth::SCOPES.join(" "));
            Ok(())
        }
        Some("provision") => {
            let project = flag(rest, "--project")
                .ok_or_else(|| anyhow::anyhow!("provision needs --project <GCP_PROJECT_ID>"))?;
            let account = flag(rest, "--account")
                .ok_or_else(|| anyhow::anyhow!("provision needs --account <email>"))?;
            let token_key = flag(rest, "--token")
                .ok_or_else(|| anyhow::anyhow!("provision needs --token <KV_KEY>"))?;
            let id_key =
                flag(rest, "--client-id").unwrap_or_else(|| format!("{token_key}_CLIENT_ID"));
            let secret_key = flag(rest, "--client-secret")
                .unwrap_or_else(|| format!("{token_key}_CLIENT_SECRET"));
            let app_name = flag(rest, "--app-name").unwrap_or_else(|| "Sidekar".to_string());

            use crate::google::provision as pv;
            let mut log: Vec<String> = Vec::new();
            let tab = pv::open_tab(&pv::consent_url(&project))?;

            let result = (|| -> Result<(String, String)> {
                let mut say = |line: &str| log.push(line.to_string());
                say("consent screen");
                pv::check_consent_screen(&tab, &project, &mut say)?;
                say("apis");
                pv::ensure_apis(&tab, &project, &mut say)?;
                say("oauth client");
                pv::create_client(&tab, &project, &app_name)
            })();
            // Always close the tab this run opened, success or not.
            pv::close_tab(&tab);

            for line in &log {
                out!(ctx, "{line}");
            }
            let (client_id, client_secret) = result?;

            let tags = ["google".to_string(), "oauth".to_string()];
            crate::broker::kv_set(&id_key, &client_id, Some(&tags))?;
            crate::broker::kv_set(&secret_key, &client_secret, Some(&tags))?;
            out!(ctx, "  stored {id_key} and {secret_key}");
            out!(
                ctx,
                "\nNow authorize as {account}:\n  sidekar google login --token {token_key} \
                 --client-id {id_key} --client-secret {secret_key} --account {account}\n\
                 If Google refuses the account, add it under Test users at {}",
                pv::audience_url(&project)
            );
            Ok(())
        }
        Some("setup") => {
            let project = flag(rest, "--project").unwrap_or_else(|| "<PROJECT_ID>".into());
            let token_key = flag(rest, "--token").unwrap_or_else(|| "GOOGLE_MY_TOKEN".into());
            let id_key = flag(rest, "--client-id").unwrap_or_else(|| "GOOGLE_MY_CLIENT_ID".into());
            let secret_key =
                flag(rest, "--client-secret").unwrap_or_else(|| "GOOGLE_MY_CLIENT_SECRET".into());
            let account = flag(rest, "--account").unwrap_or_else(|| "<your-email>".into());
            out!(
                ctx,
                "{}",
                setup_walkthrough(&project, &account, &token_key, &id_key, &secret_key)
            );
            Ok(())
        }
        Some("doctor") | Some("check") => {
            let stored = google::auth::tokens()?;
            out!(ctx, "Stored tokens: {}", stored.len());
            if stored.is_empty() {
                out!(ctx, "  none — run `sidekar google login --help`");
                return Ok(());
            }
            let token = google::auth::resolve_token(flag(rest, "--token").as_deref())?;
            out!(
                ctx,
                "Checking {} ({})",
                token.key,
                if token.account.is_empty() {
                    "unknown account"
                } else {
                    &token.account
                }
            );
            for key in [&token.client_id_key, &token.client_secret_key] {
                let present = crate::broker::kv_get(key)?.is_some();
                out!(
                    ctx,
                    "  {} {}",
                    if present { "ok  " } else { "MISSING" },
                    key
                );
            }
            match google::auth::access_token_for(&token).await {
                Ok(_) => out!(ctx, "  ok   token refreshes"),
                Err(e) => {
                    out!(ctx, "  FAIL token does not refresh\n{e}");
                    return Ok(());
                }
            }
            for (name, result) in google::probe(&token).await {
                match result {
                    Ok(()) => out!(ctx, "  ok   {name}"),
                    Err(e) => out!(
                        ctx,
                        "  FAIL {name}: {}",
                        e.to_string().lines().next().unwrap_or("unreachable")
                    ),
                }
            }
            Ok(())
        }
        Some("logout") => {
            let t = google::auth::resolve_token(flag(rest, "--token").as_deref())?;
            google::auth::forget(&t.key)?;
            out!(
                ctx,
                "Removed {}. The grant still exists at myaccount.google.com/permissions \
                 until you revoke it there.",
                t.key
            );
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar google <login|list|use|status|logout> …\n  \
             login --token <KV_KEY> --client-id <KV_KEY> --client-secret <KV_KEY>\n        \
                   [--account <email>] [--no-browser]\n  \
             list                     stored tokens; * marks the default\n  \
             use <KV_KEY>             make it the default\n  \
             status [--token <KV_KEY>]\n  \
             doctor [--token <KV_KEY>]   check keys, refresh, and all five APIs\n  \
             provision --project <ID> --account <email> --token <KV_KEY>\n        \
                   drive the console: check consent, enable APIs, create the client\n  \
             setup --project <ID> --account <email>   print the steps instead\n  \
             logout [--token <KV_KEY>]\n\n\
             You name the keys; sidekar imposes no convention. Every gmail/drive/calendar/\n\
             sheets/docs command takes --token <KV_KEY> to pick an account."
        ),
    }
}

pub async fn cmd_gmail(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let token = google::auth::resolve_token(flag(rest, "--token").as_deref())?;
    match sub {
        "search" => {
            let limit = flag_usize(rest, "--limit").unwrap_or(10);
            let query = positional(rest).join(" ");
            if query.is_empty() {
                bail!("Usage: sidekar gmail search <query> [--limit N]  (Gmail query syntax)");
            }
            let found = google::gmail::search(&token, &query, limit).await?;
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
            out!(ctx, "{}", google::gmail::read(&token, &id).await?);
            Ok(())
        }
        "send" => {
            let to = flag(rest, "--to").ok_or_else(|| anyhow::anyhow!("gmail send needs --to"))?;
            let subject = flag(rest, "--subject").unwrap_or_default();
            let body = flag(rest, "--body").unwrap_or_default();
            if body.is_empty() {
                bail!("gmail send needs --body (use --body \"$(cat file)\" for long text)");
            }
            let id = google::gmail::send(&token, &to, &subject, &body).await?;
            out!(ctx, "Sent to {to} (id {id}).");
            Ok(())
        }
        "draft" => cmd_gmail_draft(ctx, &token, rest).await,
        "labels" => {
            for l in google::gmail::labels(&token).await? {
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
            google::gmail::modify(&token, &id, &add, &remove).await?;
            out!(ctx, "Updated {id}.");
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar gmail <search|read|send|draft|labels|modify> …\n  \
             search <query> [--limit N]      Gmail query syntax: from:, is:unread, newer_than:2d\n  \
             read <id>\n  \
             send --to <addr> --subject <s> --body <text>\n  \
             draft <create|list|show|update|send|rm> …   compose without sending\n  \
             labels\n  \
             modify <id> [--add LABEL] [--remove LABEL]   (UNREAD is a label)"
        ),
    }
}

/// `sidekar gmail draft …` — compose mail a human sends.
///
/// Split out from `cmd_gmail` because it is a verb with its own verbs; folding
/// six more arms into that match would bury the four that send mail directly.
async fn cmd_gmail_draft(
    ctx: &mut AppContext,
    token: &google::auth::TokenRef,
    args: &[String],
) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let id_arg = |usage: &str| -> Result<String> {
        positional(rest)
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{usage}"))
    };

    match sub {
        "create" => {
            let to = flag(rest, "--to")
                .ok_or_else(|| anyhow::anyhow!("gmail draft create needs --to"))?;
            let subject = flag(rest, "--subject").unwrap_or_default();
            let body = flag(rest, "--body").unwrap_or_default();
            if body.is_empty() {
                bail!("gmail draft create needs --body (use --body \"$(cat file)\" for long text)");
            }
            let id = google::gmail::draft_create(token, &to, &subject, &body).await?;
            out!(ctx, "Drafted to {to} (draft {id}). Nothing has been sent.");
            Ok(())
        }
        "list" | "" => {
            let limit = flag_usize(rest, "--limit").unwrap_or(10);
            let drafts = google::gmail::draft_list(token, limit).await?;
            if drafts.is_empty() {
                out!(ctx, "No drafts.");
            }
            for d in drafts {
                out!(ctx, "{}\t{}\t{}", d.id, d.to, d.subject);
            }
            Ok(())
        }
        "show" | "read" => {
            let id = id_arg("Usage: sidekar gmail draft show <draft-id>")?;
            out!(ctx, "{}", google::gmail::draft_show(token, &id).await?);
            Ok(())
        }
        "update" => {
            let id = id_arg(
                "Usage: sidekar gmail draft update <draft-id> --to <addr> --subject <s> --body <text>",
            )?;
            // Gmail replaces the whole draft, so a partial update would blank
            // whatever was left out. Demanding all three is the honest spelling
            // of what the API does.
            let (to, subject, body) = match (
                flag(rest, "--to"),
                flag(rest, "--subject"),
                flag(rest, "--body"),
            ) {
                (Some(t), Some(s), Some(b)) => (t, s, b),
                _ => bail!(
                    "gmail draft update rewrites the whole draft, so it needs --to, --subject \
                     and --body together. `sidekar gmail draft show {id}` prints the current text."
                ),
            };
            let new_id = google::gmail::draft_update(token, &id, &to, &subject, &body).await?;
            out!(ctx, "Updated draft {new_id}. Nothing has been sent.");
            Ok(())
        }
        "send" => {
            let id = id_arg("Usage: sidekar gmail draft send <draft-id>")?;
            let msg = google::gmail::draft_send(token, &id).await?;
            out!(ctx, "Sent draft {id} (message {msg}).");
            Ok(())
        }
        "rm" | "delete" => {
            let id = id_arg("Usage: sidekar gmail draft rm <draft-id>")?;
            google::gmail::draft_delete(token, &id).await?;
            out!(ctx, "Deleted draft {id}.");
            Ok(())
        }
        other => bail!(
            "Unknown draft subcommand '{other}'.\n  \
             create --to <addr> --subject <s> --body <text>\n  \
             list [--limit N]\n  \
             show <draft-id>\n  \
             update <draft-id> --to <addr> --subject <s> --body <text>\n  \
             send <draft-id>\n  \
             rm <draft-id>"
        ),
    }
}

pub async fn cmd_drive(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let token = google::auth::resolve_token(flag(rest, "--token").as_deref())?;
    let pos = positional(rest);
    match sub {
        "ls" | "search" => {
            let limit = flag_usize(rest, "--limit").unwrap_or(25);
            // --parent wins over a text query: listing a folder is the common
            // case and should not require Drive query grammar.
            let query = match flag(rest, "--parent") {
                Some(p) => format!("parent:{p}"),
                None => positional(rest).join(" "),
            };
            let entries = google::drive::list(&token, &query, limit).await?;
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
            let file = google::drive::download(&token, &id).await?;
            match flag(rest, "--out") {
                Some(path) => {
                    // Bytes straight to disk. A PDF or .docx routed through a
                    // String arrives inflated and unopenable.
                    std::fs::write(&path, &file.bytes)?;
                    out!(ctx, "Wrote {} bytes to {path}", file.bytes.len());
                }
                None => {
                    if google::drive::looks_like_text(&file.bytes) {
                        out!(ctx, "{}", String::from_utf8_lossy(&file.bytes));
                    } else {
                        bail!(
                            "{} is binary ({} bytes). Use --out <path> to save it; printing it \
                             would corrupt both the file and your terminal.",
                            file.name,
                            file.bytes.len()
                        );
                    }
                }
            }
            Ok(())
        }
        "put" => {
            let path = positional(rest).first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar drive put <path> [--name n] [--folder id]")
            })?;
            let id = google::drive::put(
                &token,
                &path,
                flag(rest, "--name").as_deref(),
                flag(rest, "--folder").as_deref(),
            )
            .await?;
            out!(ctx, "Uploaded as {id}.");
            Ok(())
        }
        "mkdir" => {
            let name = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar drive mkdir <name> [--parent <id>]")
            })?;
            let id = google::drive::mkdir(&token, &name, flag(rest, "--parent").as_deref()).await?;
            out!(ctx, "{id}");
            Ok(())
        }
        "mv" | "move" => {
            let id = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Usage: sidekar drive mv <file-id> --to <folder-id> [--from <folder-id>]"
                )
            })?;
            let to = flag(rest, "--to")
                .ok_or_else(|| anyhow::anyhow!("drive mv needs --to <folder-id> (or 'root')"))?;
            google::drive::move_to(&token, &id, &to, flag(rest, "--from").as_deref()).await?;
            out!(ctx, "Moved {id} to {to}.");
            Ok(())
        }
        "rm" | "delete" => {
            let id = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar drive rm <file-id> [--permanent]")
            })?;
            let permanent = rest.iter().any(|a| a == "--permanent");
            google::drive::remove(&token, &id, permanent).await?;
            if permanent {
                out!(ctx, "Permanently deleted {id}.");
            } else {
                out!(ctx, "Moved {id} to trash (recoverable for 30 days).");
            }
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar drive <ls|get|put|mkdir|mv|rm> …\n  \
             ls [query] [--parent <id>] [--limit N]   bare words match names; --parent lists a folder\n  \
             mkdir <name> [--parent <id>]  prints the new folder id\n  \
             mv <file-id> --to <folder-id> [--from <folder-id>]   one call, no data transfer\n  \
             get <file-id> [--out path]    Docs export as text, Sheets as CSV\n  \
             put <path> [--name n] [--folder id]   omit --folder for My Drive root\n  \
             rm <file-id> [--permanent]    trashes by default; --permanent has no undo"
        ),
    }
}

pub async fn cmd_calendar(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let token = google::auth::resolve_token(flag(rest, "--token").as_deref())?;
    let cal = flag(rest, "--calendar").unwrap_or_else(|| "primary".into());
    match sub {
        "list" | "" => {
            let days = flag_usize(rest, "--days").unwrap_or(7) as u32;
            let limit = flag_usize(rest, "--limit").unwrap_or(25);
            let events = google::calendar::list(&token, &cal, days, limit).await?;
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
            let id =
                google::calendar::create(&token, &cal, &summary, &start, &end, &attendees).await?;
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

pub async fn cmd_sheets(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let token = google::auth::resolve_token(flag(rest, "--token").as_deref())?;
    let pos = positional(rest);
    match sub {
        "info" => {
            let id = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar sheets info <id>"))?;
            let (title, tabs) = google::sheets::info(&token, &id).await?;
            out!(ctx, "{title}");
            for t in tabs {
                out!(ctx, "  {t}");
            }
            Ok(())
        }
        "get" => {
            let id = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar sheets get <id> <range>"))?;
            let range = pos.get(1).cloned().unwrap_or_else(|| "A1:Z1000".into());
            let rows = google::sheets::squared(google::sheets::get(&token, &id, &range).await?);
            if rows.is_empty() {
                out!(ctx, "{range} is empty.");
            }
            for r in rows {
                out!(ctx, "{}", r.join("\t"));
            }
            Ok(())
        }
        "set" | "append" => {
            let id = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar sheets {sub} <id> <range> --values \"a,b|c,d\"")
            })?;
            let range = pos.get(1).cloned().unwrap_or_else(|| "A1".into());
            let raw = flag(rest, "--values").ok_or_else(|| {
                anyhow::anyhow!("needs --values \"a,b|c,d\"  (| separates rows, , separates cells)")
            })?;
            let values = parse_grid(&raw);
            let n = if sub == "set" {
                google::sheets::set(&token, &id, &range, &values).await?
            } else {
                google::sheets::append(&token, &id, &range, &values).await?
            };
            out!(ctx, "{n} cells updated.");
            Ok(())
        }
        "create" => {
            let title = pos.first().cloned().unwrap_or_else(|| "Untitled".into());
            out!(ctx, "{}", google::sheets::create(&token, &title).await?);
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar sheets <info|get|set|append|create> …\n  \
             info <id>\n  \
             get <id> [range]                     range is A1 notation, default A1:Z1000\n  \
             set <id> <range> --values \"a,b|c,d\"   | separates rows, , separates cells\n  \
             append <id> <range> --values \"…\"\n  \
             create <title>"
        ),
    }
}

pub async fn cmd_docs(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let token = google::auth::resolve_token(flag(rest, "--token").as_deref())?;
    let pos = positional(rest);
    match sub {
        "get" | "read" => {
            let id = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar docs get <id>"))?;
            let text = google::docs::get_text(&token, &id).await?;
            match flag(rest, "--out") {
                Some(path) => {
                    std::fs::write(&path, text.as_bytes())?;
                    out!(ctx, "Wrote {} bytes to {path}.", text.len());
                }
                None => out!(ctx, "{text}"),
            }
            Ok(())
        }
        "title" => {
            let id = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar docs title <id>"))?;
            out!(ctx, "{}", google::docs::title(&token, &id).await?);
            Ok(())
        }
        "create" => {
            let title = pos.first().cloned().unwrap_or_else(|| "Untitled".into());
            out!(ctx, "{}", google::docs::create(&token, &title).await?);
            Ok(())
        }
        "append" => {
            let id = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar docs append <id> --text \"…\""))?;
            let text =
                flag(rest, "--text").ok_or_else(|| anyhow::anyhow!("docs append needs --text"))?;
            google::docs::append(&token, &id, &text).await?;
            out!(ctx, "Appended {} characters.", text.len());
            Ok(())
        }
        "replace" => {
            let id = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar docs replace <id> --find x --with y")
            })?;
            let find =
                flag(rest, "--find").ok_or_else(|| anyhow::anyhow!("docs replace needs --find"))?;
            let with = flag(rest, "--with").unwrap_or_default();
            let n = google::docs::replace(&token, &id, &find, &with).await?;
            out!(ctx, "{n} occurrences replaced.");
            Ok(())
        }
        _ => bail!(
            "Usage: sidekar docs <get|title|create|append|replace> …\n  \
             get <id> [--out path]\n  \
             title <id>\n  \
             create <title>\n  \
             append <id> --text \"…\"\n  \
             replace <id> --find x --with y"
        ),
    }
}

/// The console steps for standing up an individual OAuth client.
///
/// Printed rather than automated. The console's markup changes under us — two
/// of the flows I drove by hand today had already moved — and a walkthrough that
/// silently clicks the wrong thing is worse than one that tells you what to
/// click. Every URL is filled in for the project, so there is no hunting.
pub(crate) fn setup_walkthrough(
    project: &str,
    account: &str,
    token_key: &str,
    id_key: &str,
    secret_key: &str,
) -> String {
    format!(
        "Standing up your own Google OAuth client for {account}\n\
         Project: {project}\n\n\
         1. Consent screen — External, then add yourself as a test user.\n   \
            https://console.cloud.google.com/auth/overview?project={project}\n   \
            External + Testing needs no Google verification and accepts up to 100\n   \
            named test users with every scope, including Gmail and full Drive.\n   \
            The cost is that refresh tokens expire after 7 days; publishing the\n   \
            app removes that, but publishing with Gmail or full Drive requires\n   \
            verification and a CASA security assessment.\n\n   \
            Add {account} under Audience > Test users, or it will be refused.\n\n\
         2. Enable the five APIs. Fastest, if gcloud is signed in:\n   \
            gcloud services enable gmail.googleapis.com drive.googleapis.com \\\n     \
              calendar-json.googleapis.com sheets.googleapis.com docs.googleapis.com \\\n     \
              --project {project}\n   \
            Otherwise enable each at:\n   \
            https://console.cloud.google.com/apis/library?project={project}\n\n\
         3. Create the client — Application type: Desktop app.\n   \
            https://console.cloud.google.com/auth/clients/create?project={project}\n   \
            Desktop app accepts a loopback redirect, so nothing has to be\n   \
            registered and `google login` can pick its own port.\n\n   \
            COPY THE SECRET BEFORE CLOSING THE DIALOG. Google shows it once and\n   \
            will not show it again; a lost secret means creating another one.\n\n\
         4. Store both, under whatever key names you like:\n   \
            sidekar kv set {id_key} '<client id>' --tag=google,oauth\n   \
            sidekar kv set {secret_key} '<client secret>' --tag=google,oauth\n\n\
         5. Authorize:\n   \
            sidekar google login --token {token_key} \\\n     \
              --client-id {id_key} --client-secret {secret_key} --account {account}\n\n\
         6. Confirm:\n   \
            sidekar google doctor --token {token_key}\n\n\
         Re-run step 5 when the 7-day expiry bites; sidekar will tell you when\n\
         that is what happened."
    )
}

/// `a,b|c,d` into rows of cells. Pipes separate rows, commas separate cells.
pub(crate) fn parse_grid(raw: &str) -> Vec<Vec<String>> {
    raw.split('|')
        .map(|row| row.split(',').map(|c| c.trim().to_string()).collect())
        .collect()
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
