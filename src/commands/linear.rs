//! `sidekar linear`: sign-in and the GraphQL API commands.
//!
//! Account verbs (`setup`, `login`, `add`, `accounts`, `use`, `status`,
//! `doctor`, `logout`) sit beside the API verbs, as in `sidekar slack`. Every
//! API verb takes `--token <KV_KEY>`.

use super::google::{
    flag, flag_usize, flags_all, one_of, positional_with_switches, reject_unknown_flags,
};
use crate::AppContext;
use crate::linear::{Linear, api, auth};
use crate::timefmt::{self, Zone};
use anyhow::{Result, bail};

const SWITCHES: &[&str] = &[
    "--all",
    "--unassign",
    "--no-browser",
    "--print-url",
    "--unread",
    "--archived",
    "--print",
];

const USAGE: &str = "Usage: sidekar linear <command> …\n\
  Account:\n  \
  setup [--port N]                          how to get an API key or create an OAuth app\n  \
  add --token <KV_KEY>                      adopt a personal API key already in kv\n  \
  login --token <KV_KEY> --client-id <KV_KEY> --client-secret <KV_KEY> [--port N] [--no-browser]\n  \
  accounts                                  stored tokens; * marks the default\n  \
  workspaces                                each stored token's workspace, checked live\n  \
  use <KV_KEY>                              make it the default\n  \
  status | doctor [--token <KV_KEY>]\n  \
  logout [--token <KV_KEY>]\n\
  Read:\n  \
  issues [text] [--team K] [--state S] [--assignee A] [--project P] [--label L]\n         \
         [--priority P] [--all] [--limit N]   open issues unless --all or --state\n  \
  mine [--state S] [--team K] [--all] [--limit N]   assigned to you\n  \
  issue <ID>                                description, sub-issues, comments\n  \
  history <ID> [--limit N]                  who changed what, oldest first\n  \
  attachments <ID>                          linked attachments + files uploaded into the text\n  \
  download <ID> [name] [--out <path|dir/>] [--print] | <ID> --all [--out dir] | <upload-url>\n  \
  activity [--team K] [--project P] [--since 7d] [--limit N]   issues updated + comments made\n  \
  inbox [--unread] [--archived] [--limit N] your notifications, newest first\n  \
  teams | states [--team K] | labels [--team K] | users [filter]\n  \
  projects [filter] [--team K] [--limit N]\n  \
  cycles [--team K] [--all] [--limit N]     current, upcoming and previous unless --all\n\
  Write:\n  \
  create --title T [--team K] [FIELDS] [--attach <path>]…   files go in the description\n  \
  update <ID> [FIELDS] [--unassign] [--add-label L] [--remove-label L]\n  \
  comment <ID> --body <text>|--body-file <path> [--attach <path>]…\n  \
  upload <ID> <path>… [--title T]           attach files to the issue\n  \
  link <ID> <url> [--title T]               attach a URL (GitHub, Slack, Figma… shown richly)\n  \
  inbox read|unread|archive <NOTIFICATION_ID>… | inbox read --all\n  \
  FIELDS: --title T  --description D|--description-file P  --state S  --assignee A\n          \
          --priority urgent|high|medium|low|none|0-4  --labels a,b (replaces)\n          \
          --project P  --cycle current|next|N  --parent ID  --due YYYY-MM-DD  --estimate N\n          \
          (update: --project/--cycle/--parent none clears)\n\n\
  <ID> is an identifier (ENG-123) or a uuid. A (assignee) is me, none, an email, or a name.\n\
  S (state) is a state name (\"In Review\") or type (backlog, unstarted, started, completed, canceled).\n\
  Times are ISO 8601 UTC (2026-09-14T03:36:49Z); --local shows this machine's zone\n\
  with its offset (2026-09-13T23:36:49-04:00). Dates (--due, project dates) stay dates.";

/// Flags every create/update accepts.
const FIELD_FLAGS: &[&str] = &[
    "--title",
    "--description",
    "--description-file",
    "--state",
    "--assignee",
    "--priority",
    "--labels",
    "--project",
    "--cycle",
    "--parent",
    "--due",
    "--estimate",
];

pub async fn cmd_linear(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    // `--local` goes with every command, like `--token`: it only changes how
    // times are shown.
    let (zone, args) = Zone::from_args(args);
    let args = args.as_slice();
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let pos = positional_with_switches(rest, SWITCHES);
    let has = |s: &str| rest.iter().any(|a| a == s);

    match sub {
        "setup" => {
            reject_unknown_flags(rest, &["--port", "--client-id", "--client-secret"])?;
            let port = port_flag(rest)?;
            let token_key = flag(rest, "--token").unwrap_or_else(|| "LINEAR_MY_TOKEN".into());
            let id_key = flag(rest, "--client-id").unwrap_or_else(|| "LINEAR_MY_CLIENT_ID".into());
            let secret_key =
                flag(rest, "--client-secret").unwrap_or_else(|| "LINEAR_MY_CLIENT_SECRET".into());
            out!(
                ctx,
                "{}",
                setup_walkthrough(port, &token_key, &id_key, &secret_key)
            );
            Ok(())
        }
        "login" => {
            reject_unknown_flags(
                rest,
                &[
                    "--client-id",
                    "--client-secret",
                    "--port",
                    "--no-browser",
                    "--print-url",
                ],
            )?;
            let token_key = flag(rest, "--token").ok_or_else(|| {
                anyhow::anyhow!(
                    "login needs --token <KV_KEY>, the key the token will be stored under"
                )
            })?;
            let client_id_key = flag(rest, "--client-id")
                .ok_or_else(|| anyhow::anyhow!("login needs --client-id <KV_KEY>"))?;
            let client_secret_key = flag(rest, "--client-secret")
                .ok_or_else(|| anyhow::anyhow!("login needs --client-secret <KV_KEY>"))?;
            let me = auth::login(auth::LoginOptions {
                token_key: &token_key,
                client_id_key: &client_id_key,
                client_secret_key: &client_secret_key,
                port: port_flag(rest)?,
                open_browser: !has("--no-browser") && !has("--print-url"),
            })
            .await?;
            out!(
                ctx,
                "Stored a token for {} ({}) under {token_key}.",
                me.email,
                me.org
            );
            Ok(())
        }
        "add" => {
            reject_unknown_flags(rest, &[])?;
            let token_key = flag(rest, "--token").ok_or_else(|| {
                anyhow::anyhow!(
                    "add needs --token <KV_KEY>, the kv key already holding the API key.\n  \
                     sidekar kv set LINEAR_TOKEN 'lin_api_…' && sidekar linear add --token LINEAR_TOKEN"
                )
            })?;
            let (method, me) = auth::add(&token_key).await?;
            out!(
                ctx,
                "{token_key} ({}) acts as {} in {}.",
                method.as_str(),
                me.email,
                me.org
            );
            Ok(())
        }
        "accounts" => {
            reject_unknown_flags(rest, &[])?;
            let tokens = auth::tokens()?;
            if tokens.is_empty() {
                out!(
                    ctx,
                    "No Linear tokens stored. `sidekar linear setup` shows how."
                );
                return Ok(());
            }
            let default = auth::default_token_key()?;
            for t in tokens {
                let marker = if Some(&t.key) == default.as_ref() {
                    "*"
                } else {
                    " "
                };
                out!(
                    ctx,
                    "{marker} {}\t{}\t{}\t{}",
                    t.key,
                    t.method.as_str(),
                    or_unknown(&t.org),
                    or_unknown(&t.account)
                );
            }
            Ok(())
        }
        "use" => {
            reject_unknown_flags(rest, &[])?;
            let key = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar linear use <KV_KEY>"))?;
            if !auth::tokens()?.iter().any(|t| t.key == key) {
                bail!("no Linear token stored under {key}; `sidekar linear accounts` lists them");
            }
            auth::set_default_token_key(&key)?;
            out!(ctx, "Default Linear token is now {key}.");
            Ok(())
        }
        "status" => {
            reject_unknown_flags(rest, &[])?;
            let t = auth::resolve_token(flag(rest, "--token").as_deref())?;
            out!(ctx, "Active: {} ({})", t.key, t.method.as_str());
            out!(ctx, "Workspace: {}", or_unknown(&t.org));
            out!(ctx, "Acts as: {}", or_unknown(&t.account));
            out!(ctx, "Stored: {}", auth::tokens()?.len());
            Ok(())
        }
        "workspaces" | "orgs" => {
            reject_unknown_flags(rest, &[])?;
            let tokens = auth::tokens()?;
            if tokens.is_empty() {
                out!(
                    ctx,
                    "No Linear tokens stored. `sidekar linear setup` shows how."
                );
                return Ok(());
            }
            let default = auth::default_token_key()?;
            for t in tokens {
                let marker = if Some(&t.key) == default.as_ref() {
                    "*"
                } else {
                    " "
                };
                let live = match Linear::connect(&t).await {
                    Ok(l) => api::organization(&l).await,
                    Err(e) => Err(e),
                };
                match live {
                    Ok(o) => out!(
                        ctx,
                        "{marker} {}\t{}\thttps://linear.app/{}\t{} users\tas {} <{}>",
                        t.key,
                        o.name,
                        o.url_key,
                        o.users,
                        o.viewer,
                        o.viewer_email
                    ),
                    Err(e) => out!(
                        ctx,
                        "{marker} {}\t{}\tunreachable: {}",
                        t.key,
                        or_unknown(&t.org),
                        e.to_string().lines().next().unwrap_or("")
                    ),
                }
            }
            Ok(())
        }
        "doctor" | "check" => {
            reject_unknown_flags(rest, &[])?;
            doctor(ctx, flag(rest, "--token").as_deref()).await
        }
        "logout" => {
            reject_unknown_flags(rest, &[])?;
            let t = auth::resolve_token(flag(rest, "--token").as_deref())?;
            auth::forget(&t.key)?;
            out!(
                ctx,
                "Removed {}. The key or app authorization stays valid in Linear until revoked \
                 under Settings → Security & access.",
                t.key
            );
            Ok(())
        }
        "issues" | "search" | "mine" | "issue" | "show" | "read" | "view" | "create" | "update"
        | "edit" | "comment" | "teams" | "states" | "labels" | "users" | "projects" | "cycles"
        | "history" | "activity" | "inbox" | "notifications" | "attachments" | "files"
        | "download" | "upload" | "attach" | "link" => {
            let token = auth::resolve_token(flag(rest, "--token").as_deref())?;
            let linear = Linear::connect(&token).await?;
            api_command(ctx, &linear, sub, rest, &pos, zone).await
        }
        _ => bail!("{USAGE}"),
    }
}

async fn api_command(
    ctx: &mut AppContext,
    linear: &Linear,
    sub: &str,
    rest: &[String],
    pos: &[String],
    zone: Zone,
) -> Result<()> {
    let has = |s: &str| rest.iter().any(|a| a == s);
    match sub {
        "issues" | "search" | "mine" => {
            let mut known = vec![
                "--team",
                "--state",
                "--all",
                "--limit",
                "--project",
                "--label",
                "--priority",
            ];
            if sub != "mine" {
                known.push("--assignee");
            }
            reject_unknown_flags(rest, &known)?;
            let q = api::IssueQuery {
                text: Some(pos.join(" ")).filter(|t| !t.trim().is_empty()),
                team: flag(rest, "--team"),
                state: flag(rest, "--state"),
                assignee: if sub == "mine" {
                    Some("me".into())
                } else {
                    flag(rest, "--assignee")
                },
                project: flag(rest, "--project"),
                label: flag(rest, "--label"),
                priority: flag(rest, "--priority")
                    .map(|p| api::parse_priority(&p))
                    .transpose()?,
                include_closed: has("--all"),
                updated_since: None,
                limit: flag_usize(rest, "--limit").unwrap_or(25),
            };
            let found = api::issues(linear, &q).await?;
            if found.items.is_empty() {
                out!(ctx, "No issues match.");
            }
            let more = found.more;
            for i in found.items {
                out!(
                    ctx,
                    "{}\t{}\t{}\t{}\t{}",
                    i.identifier,
                    i.state,
                    i.priority,
                    if i.assignee.is_empty() {
                        "-"
                    } else {
                        &i.assignee
                    },
                    i.title
                );
            }
            if more {
                out!(ctx, "{}", more_note(q.limit, "issues"));
            }
            Ok(())
        }
        "issue" | "show" | "read" | "view" => {
            reject_unknown_flags(rest, &[])?;
            let id = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar linear issue <ENG-123>"))?;
            out!(
                ctx,
                "{}",
                api::render_issue(&api::issue(linear, &id).await?, zone)
            );
            Ok(())
        }
        "create" => {
            let mut known = FIELD_FLAGS.to_vec();
            known.extend(["--team", "--attach"]);
            reject_unknown_flags(rest, &known)?;
            let mut changes = changes_from(rest)?;
            if changes.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
                bail!("an issue needs --title");
            }
            let attach = flags_all(rest, "--attach");
            if !attach.is_empty() {
                for p in &attach {
                    crate::attachments::check_upload(p, crate::attachments::LINEAR_UPLOAD)?;
                }
                let md = api::upload_for_markdown(linear, &attach).await?;
                changes.description = Some(api::with_files(changes.description.as_deref(), &md));
            }
            let team = api::resolve_team(linear, flag(rest, "--team").as_deref()).await?;
            let (id, url) = api::create(linear, &team, &changes).await?;
            out!(ctx, "Created {id}.\n{url}");
            Ok(())
        }
        "update" | "edit" => {
            let mut known = FIELD_FLAGS.to_vec();
            known.extend(["--unassign", "--add-label", "--remove-label"]);
            reject_unknown_flags(rest, &known)?;
            let id = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Usage: sidekar linear update <ENG-123> [--state S] [--assignee A] …"
                )
            })?;
            let changes = changes_from(rest)?;
            if changes.is_empty() {
                bail!("nothing to change; `sidekar linear` lists the fields update takes");
            }
            if changes.unassign && changes.assignee.is_some() {
                bail!("pass --assignee or --unassign, not both");
            }
            let (ident, url) = api::update(linear, &id, &changes).await?;
            out!(ctx, "Updated {ident}.\n{url}");
            Ok(())
        }
        "comment" => {
            reject_unknown_flags(rest, &["--body", "--body-file", "--attach"])?;
            let id = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Usage: sidekar linear comment <ENG-123> --body <text>|--body-file <path> [--attach <path>]…"
                )
            })?;
            let text = one_of(rest, "--body", "--body-file")?.filter(|b| !b.trim().is_empty());
            let attach = flags_all(rest, "--attach");
            if text.is_none() && attach.is_empty() {
                bail!("linear comment needs --body <text>, --body-file <path>, or --attach <path>");
            }
            for p in &attach {
                crate::attachments::check_upload(p, crate::attachments::LINEAR_UPLOAD)?;
            }
            let md = if attach.is_empty() {
                String::new()
            } else {
                api::upload_for_markdown(linear, &attach).await?
            };
            let body = api::with_files(text.as_deref(), &md);
            let url = api::comment(linear, &id, &body).await?;
            out!(ctx, "Commented on {id}.\n{url}");
            Ok(())
        }
        "teams" => {
            reject_unknown_flags(rest, &[])?;
            for t in api::teams(linear).await? {
                let mut tags = vec![format!("{} issues", t.issue_count)];
                if t.private {
                    tags.push("private".into());
                }
                if t.cycles_enabled {
                    tags.push("cycles".into());
                }
                out!(
                    ctx,
                    "{}\t{}\t{}\t{}{}",
                    t.key,
                    t.name,
                    tags.join(", "),
                    t.id,
                    if t.description.is_empty() {
                        String::new()
                    } else {
                        format!("\t{}", api::first_line(&t.description, 100))
                    }
                );
            }
            Ok(())
        }
        "states" => {
            reject_unknown_flags(rest, &["--team"])?;
            let team = api::resolve_team(linear, flag(rest, "--team").as_deref()).await?;
            for s in api::states(linear, &team.id).await? {
                out!(ctx, "{}\t{}", s.name, s.kind);
            }
            Ok(())
        }
        "labels" => {
            reject_unknown_flags(rest, &["--team"])?;
            let team = match flag(rest, "--team") {
                Some(k) => Some(api::resolve_team(linear, Some(&k)).await?),
                None => None,
            };
            for l in api::labels(linear, team.as_ref().map(|t| t.id.as_str())).await? {
                if l.is_group {
                    continue;
                }
                out!(
                    ctx,
                    "{}\t{}",
                    l.name,
                    if l.team.is_empty() {
                        "workspace"
                    } else {
                        &l.team
                    }
                );
            }
            Ok(())
        }
        "users" => {
            reject_unknown_flags(rest, &["--limit", "--all"])?;
            let filter = pos.join(" ").to_lowercase();
            let limit = flag_usize(rest, "--limit").unwrap_or(250);
            let all = has("--all");
            let keep = |u: &api::Person| {
                (all || u.active)
                    && (filter.is_empty()
                        || format!("{} {} {}", u.name, u.display_name, u.email)
                            .to_lowercase()
                            .contains(&filter))
            };
            let found = api::users(linear, limit, Some(&keep)).await?;
            if found.items.is_empty() {
                out!(ctx, "No users match.");
            }
            for u in &found.items {
                out!(ctx, "{}\t{}\t{}", u.display_name, u.name, u.email);
            }
            if found.more {
                out!(ctx, "{}", more_note(limit, "users"));
            }
            Ok(())
        }
        "projects" => {
            reject_unknown_flags(rest, &["--team", "--limit"])?;
            let name = Some(pos.join(" ")).filter(|n| !n.trim().is_empty());
            let limit = flag_usize(rest, "--limit").unwrap_or(50);
            let found = api::projects(
                linear,
                flag(rest, "--team").as_deref(),
                name.as_deref(),
                limit,
            )
            .await?;
            if found.items.is_empty() {
                out!(ctx, "No projects match.");
            }
            let dash = |x: &str| {
                if x.is_empty() {
                    "-".to_string()
                } else {
                    x.to_string()
                }
            };
            for p in &found.items {
                out!(
                    ctx,
                    "{}\t{}\t{:.0}%\t{} → {}\t{}\tlead {}\tteams {}\t{}",
                    p.name,
                    dash(&p.status),
                    p.progress * 100.0,
                    dash(&p.start),
                    dash(&p.target),
                    dash(&p.health),
                    dash(&p.lead),
                    dash(&p.teams.join(",")),
                    p.url
                );
            }
            if found.more {
                out!(ctx, "{}", more_note(limit, "projects"));
            }
            Ok(())
        }
        "history" => {
            reject_unknown_flags(rest, &["--limit"])?;
            let id = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar linear history <ENG-123>"))?;
            let (title, entries) =
                api::history(linear, &id, flag_usize(rest, "--limit").unwrap_or(50)).await?;
            out!(ctx, "{title}");
            if entries.is_empty() {
                out!(ctx, "No history.");
            }
            for h in entries {
                out!(
                    ctx,
                    "{}\t{}\t{}",
                    timefmt::from_iso(&h.at, zone),
                    if h.actor.is_empty() {
                        "Linear"
                    } else {
                        &h.actor
                    },
                    h.change
                );
            }
            Ok(())
        }
        "activity" => {
            reject_unknown_flags(rest, &["--team", "--project", "--since", "--limit"])?;
            let since_text = flag(rest, "--since").unwrap_or_else(|| "7d".into());
            let since = api::since(&since_text)?;
            let limit = flag_usize(rest, "--limit").unwrap_or(25);
            let a = api::activity(
                linear,
                flag(rest, "--team").as_deref(),
                flag(rest, "--project").as_deref(),
                &since,
                limit,
            )
            .await?;
            // One timeline, newest first.
            let mut lines: Vec<(String, String)> = a
                .issues
                .iter()
                .map(|i| {
                    (
                        i.updated.clone(),
                        format!(
                            "{}\tupdated\t{}\t{}\t{}\t{}",
                            timefmt::from_iso(&i.updated, zone),
                            i.identifier,
                            i.state,
                            if i.assignee.is_empty() {
                                "-"
                            } else {
                                &i.assignee
                            },
                            i.title
                        ),
                    )
                })
                .collect();
            lines.extend(a.comments.iter().map(|c| {
                (
                    c.created.clone(),
                    format!(
                        "{}\tcomment\t{}\t{}\t{}",
                        timefmt::from_iso(&c.created, zone),
                        c.issue,
                        if c.author.is_empty() { "-" } else { &c.author },
                        c.body
                    ),
                )
            }));
            lines.sort_by(|x, y| y.0.cmp(&x.0));
            if lines.is_empty() {
                out!(ctx, "Nothing changed since {since_text}.");
            }
            for (_, l) in lines {
                out!(ctx, "{l}");
            }
            if a.more {
                out!(ctx, "{}", more_note(limit, "issues or comments"));
            }
            Ok(())
        }
        "inbox" | "notifications" => {
            let verb = pos.first().map(String::as_str);
            match verb {
                Some("read" | "unread" | "archive") => {
                    reject_unknown_flags(rest, &["--all"])?;
                    let verb = verb.unwrap_or_default();
                    let mut ids: Vec<String> = pos[1..].to_vec();
                    if has("--all") {
                        if verb != "read" {
                            bail!("--all goes with `inbox read` only");
                        }
                        let (_, unread) =
                            api::notifications(linear, true, false, usize::MAX).await?;
                        ids.extend(unread.items.into_iter().map(|n| n.id));
                    } else if ids.is_empty() {
                        bail!(
                            "Usage: sidekar linear inbox {verb} <NOTIFICATION_ID>…  \
                             (ids are the first column of `sidekar linear inbox`)"
                        );
                    }
                    if ids.is_empty() {
                        out!(ctx, "No unread notifications.");
                        return Ok(());
                    }
                    let report = inbox_apply(linear, verb, &ids).await;
                    out!(ctx, "{}", report.summary(verb));
                    if !report.failed.is_empty() {
                        bail!(
                            "{} of {} notification(s) failed",
                            report.failed.len(),
                            ids.len()
                        );
                    }
                    Ok(())
                }
                Some(other) => bail!("unknown inbox action {other}; use read, unread or archive"),
                None => {
                    reject_unknown_flags(rest, &["--unread", "--archived", "--limit"])?;
                    let limit = flag_usize(rest, "--limit").unwrap_or(25);
                    let (unread, found) =
                        api::notifications(linear, has("--unread"), has("--archived"), limit)
                            .await?;
                    out!(ctx, "{unread} unread.");
                    for n in &found.items {
                        out!(ctx, "{}", api_inbox_line(n, zone));
                    }
                    if found.more {
                        out!(ctx, "{}", more_note(limit, "notifications"));
                    }
                    Ok(())
                }
            }
        }
        "attachments" | "files" => {
            reject_unknown_flags(rest, &[])?;
            let id = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar linear attachments <ENG-123>"))?;
            let issue = api::issue(linear, &id).await?;
            let files = api::embedded_files(&issue, zone);
            if issue.attachments.is_empty() && files.is_empty() {
                out!(ctx, "No attachments or uploaded files on {id}.");
            }
            for a in &issue.attachments {
                out!(ctx, "attachment\t{}", a.line());
            }
            for f in &files {
                out!(ctx, "file\t{}\t{}\t{}", f.name, f.place, f.url);
            }
            Ok(())
        }
        "download" => {
            reject_unknown_flags(rest, &["--out", "--all", "--print"])?;
            let target = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Usage: sidekar linear download <ENG-123> [name] [--out <path|dir/>] [--print]\n  \
                     or: sidekar linear download <ENG-123> --all [--out <dir>]\n  \
                     or: sidekar linear download <https://uploads.linear.app/…> [--out <path>]"
                )
            })?;
            let out_flag = flag(rest, "--out");
            let print = has("--print");
            if target.starts_with("http") {
                let name = target.rsplit('/').next().unwrap_or("file").to_string();
                return fetch_one(ctx, linear, &target, &name, out_flag.as_deref(), print).await;
            }
            let issue = api::issue(linear, &target).await?;
            let files = api::downloadable_files(&issue, zone);
            if files.is_empty() {
                bail!(
                    "{target} has no uploaded files; `sidekar linear attachments {target}` lists \
                     its links"
                );
            }
            if has("--all") {
                if print {
                    bail!("--print shows one file; name it instead of --all");
                }
                let dir = out_flag.unwrap_or_else(|| ".".into());
                let dir = if dir.ends_with('/') {
                    dir
                } else {
                    format!("{dir}/")
                };
                // Pasted screenshots are all "image.png"; keep every one.
                let names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
                for (f, name) in files.iter().zip(crate::attachments::distinct_names(&names)) {
                    fetch_one(ctx, linear, &f.url, &name, Some(&dir), false).await?;
                }
                return Ok(());
            }
            let f = match pos.get(1) {
                Some(wanted) => files
                    .iter()
                    .find(|f| f.name == *wanted || f.url == *wanted)
                    .or_else(|| files.iter().find(|f| f.url.contains(wanted.as_str()))),
                None if files.len() == 1 => files.first(),
                None => None,
            }
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "which file? Name one, or pass --all. Available: {}",
                    files
                        .iter()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            fetch_one(ctx, linear, &f.url, &f.name, out_flag.as_deref(), print).await
        }
        "upload" | "attach" => {
            reject_unknown_flags(rest, &["--title"])?;
            let id = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar linear upload <ENG-123> <path>… [--title T]")
            })?;
            let paths = &pos[1..];
            if paths.is_empty() {
                bail!("linear upload needs at least one file path after the issue");
            }
            let title = flag(rest, "--title");
            if title.is_some() && paths.len() > 1 {
                bail!("--title names one file; upload several without it");
            }
            // Fail on a bad path before anything is uploaded.
            for p in paths {
                crate::attachments::check_upload(p, crate::attachments::LINEAR_UPLOAD)?;
            }
            for p in paths {
                let url = api::attach_file(linear, &id, p, title.as_deref()).await?;
                out!(ctx, "Attached {p} to {id}.\n{url}");
            }
            Ok(())
        }
        "link" => {
            reject_unknown_flags(rest, &["--title"])?;
            let (id, url) = match (pos.first(), pos.get(1)) {
                (Some(i), Some(u)) => (i.clone(), u.clone()),
                _ => bail!("Usage: sidekar linear link <ENG-123> <url> [--title T]"),
            };
            let a = api::link_url(linear, &id, &url, flag(rest, "--title").as_deref()).await?;
            out!(ctx, "Linked to {id}: {}", a.line());
            Ok(())
        }
        "cycles" => {
            reject_unknown_flags(rest, &["--team", "--all", "--limit"])?;
            let limit = flag_usize(rest, "--limit").unwrap_or(25);
            let found =
                api::cycles(linear, flag(rest, "--team").as_deref(), has("--all"), limit).await?;
            if found.items.is_empty() {
                out!(ctx, "No cycles.");
            }
            let more = found.more;
            for c in found.items {
                out!(
                    ctx,
                    "{}\t{}\t{}\t{} → {}\t{:.0}%{}",
                    c.team,
                    c.number,
                    c.status,
                    timefmt::from_iso(&c.starts, zone),
                    timefmt::from_iso(&c.ends, zone),
                    c.progress * 100.0,
                    if c.name.is_empty() {
                        String::new()
                    } else {
                        format!("\t{}", c.name)
                    }
                );
            }
            if more {
                out!(ctx, "{}", more_note(limit, "cycles"));
            }
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}

/// What an inbox action did to each notification.
#[derive(Debug, Default)]
pub(crate) struct InboxReport {
    pub done: usize,
    /// Id and why, for each that failed.
    pub failed: Vec<(String, String)>,
}

impl InboxReport {
    pub(crate) fn summary(&self, verb: &str) -> String {
        let done = match verb {
            "read" => "Marked read",
            "unread" => "Marked unread",
            _ => "Archived",
        };
        let mut out = format!("{done}: {} notification(s).", self.done);
        if !self.failed.is_empty() {
            out.push_str(&format!(" Failed: {}.", self.failed.len()));
            for (id, why) in &self.failed {
                out.push_str(&format!("\n  {id}\t{why}"));
            }
        }
        out
    }
}

/// Apply `read`, `unread` or `archive` to every id, going on past a failure
/// (one deleted notification must not leave the rest of the inbox as it was),
/// and say which failed.
pub(crate) async fn inbox_apply(linear: &Linear, verb: &str, ids: &[String]) -> InboxReport {
    let mut report = InboxReport::default();
    for id in ids {
        let r = match verb {
            "archive" => api::archive_notification(linear, id).await,
            _ => api::mark_notification(linear, id, verb == "read").await,
        };
        match r {
            Ok(()) => report.done += 1,
            Err(e) => report.failed.push((
                id.clone(),
                e.to_string().lines().next().unwrap_or_default().to_string(),
            )),
        }
    }
    report
}

/// The line under a list cut at `--limit`, so a page is never taken for all.
pub(crate) fn more_note(limit: usize, what: &str) -> String {
    format!("(showing the first {limit} {what}; there are more. Raise --limit to see them.)")
}

/// One inbox row: id, unread marker, when, type, who, what it is about.
pub(crate) fn api_inbox_line(n: &api::Notification, zone: Zone) -> String {
    let about = if !n.issue.is_empty() {
        format!("{} {}", n.issue, n.issue_title)
    } else if !n.project.is_empty() {
        format!("project {}", n.project)
    } else {
        n.title.clone()
    };
    let mut line = format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        n.id,
        if n.archived {
            "archived"
        } else if n.read {
            "read"
        } else {
            "UNREAD"
        },
        timefmt::from_iso(&n.created, zone),
        n.kind,
        if n.actor.is_empty() { "-" } else { &n.actor },
        about
    );
    if !n.comment.is_empty() {
        line.push_str(&format!("\t“{}”", n.comment));
    } else if !n.subtitle.is_empty() && n.issue.is_empty() {
        line.push_str(&format!("\t{}", n.subtitle));
    }
    line
}

/// Download one Linear-hosted file to disk, or print it if it is text.
async fn fetch_one(
    ctx: &mut AppContext,
    linear: &Linear,
    url: &str,
    name: &str,
    out: Option<&str>,
    print: bool,
) -> Result<()> {
    let (bytes, _) = linear.download(url).await?;
    if print {
        out!(ctx, "{}", crate::attachments::printable(name, &bytes)?);
    } else {
        let path = crate::attachments::save(out, name, &bytes)?;
        out!(ctx, "Wrote {} ({} bytes).", path.display(), bytes.len());
    }
    Ok(())
}

/// Read the create/update field flags into a [`api::Changes`].
pub(crate) fn changes_from(rest: &[String]) -> Result<api::Changes> {
    let list = |v: String| -> Vec<String> {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    Ok(api::Changes {
        title: flag(rest, "--title"),
        description: one_of(rest, "--description", "--description-file")?,
        state: flag(rest, "--state"),
        assignee: flag(rest, "--assignee"),
        unassign: rest.iter().any(|a| a == "--unassign"),
        priority: flag(rest, "--priority")
            .map(|p| api::parse_priority(&p))
            .transpose()?,
        labels: flag(rest, "--labels").map(list),
        add_labels: flags_all(rest, "--add-label")
            .into_iter()
            .flat_map(list)
            .collect(),
        remove_labels: flags_all(rest, "--remove-label")
            .into_iter()
            .flat_map(list)
            .collect(),
        project: flag(rest, "--project"),
        cycle: flag(rest, "--cycle"),
        parent: flag(rest, "--parent"),
        due: flag(rest, "--due"),
        estimate: flag(rest, "--estimate")
            .map(|e| {
                e.parse::<i64>()
                    .map_err(|_| anyhow::anyhow!("--estimate must be a whole number, got {e}"))
            })
            .transpose()?,
    })
}

async fn doctor(ctx: &mut AppContext, requested: Option<&str>) -> Result<()> {
    let stored = auth::tokens()?;
    out!(ctx, "Stored tokens: {}", stored.len());
    if stored.is_empty() {
        out!(ctx, "  none — run `sidekar linear setup`");
        return Ok(());
    }
    let token = auth::resolve_token(requested)?;
    out!(ctx, "Checking {} ({})", token.key, token.method.as_str());
    for key in [&token.client_id_key, &token.client_secret_key]
        .into_iter()
        .flatten()
    {
        let state = match crate::broker::kv_lookup(key)? {
            Some(Ok(_)) => "ok  ".to_string(),
            Some(Err(unreadable)) => format!("UNREADABLE ({})", unreadable.reason),
            None => "MISSING".to_string(),
        };
        out!(ctx, "  {state} {key}");
    }
    let linear = match Linear::connect(&token).await {
        Ok(l) => l,
        Err(e) => {
            out!(ctx, "  FAIL token\n{e}");
            return Ok(());
        }
    };
    match api::viewer(&linear).await {
        Ok(me) => out!(
            ctx,
            "  ok   viewer: {} <{}> in {}",
            me.name,
            me.email,
            me.org
        ),
        Err(e) => {
            out!(ctx, "  FAIL viewer: {e}");
            return Ok(());
        }
    }
    match api::teams(&linear).await {
        Ok(t) => out!(
            ctx,
            "  ok   teams: {}",
            t.iter()
                .map(|t| t.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(e) => out!(
            ctx,
            "  FAIL teams: {}",
            e.to_string().lines().next().unwrap_or("")
        ),
    }
    let q = api::IssueQuery {
        assignee: Some("me".into()),
        limit: 1,
        ..Default::default()
    };
    match api::issues(&linear, &q).await {
        Ok(_) => out!(ctx, "  ok   issues"),
        Err(e) => out!(
            ctx,
            "  FAIL issues: {}",
            e.to_string().lines().next().unwrap_or("")
        ),
    }
    Ok(())
}

fn port_flag(args: &[String]) -> Result<u16> {
    match flag(args, "--port") {
        None => Ok(auth::DEFAULT_PORT),
        Some(p) => p
            .parse()
            .map_err(|_| anyhow::anyhow!("--port must be a number, got {p}")),
    }
}

fn or_unknown(s: &str) -> &str {
    if s.is_empty() { "(unknown)" } else { s }
}

pub(crate) fn setup_walkthrough(
    port: u16,
    token_key: &str,
    id_key: &str,
    secret_key: &str,
) -> String {
    let redirect = auth::redirect_uri(port);
    format!(
        "Connecting Linear to sidekar — two ways.\n\n\
         A. Personal API key (simplest; acts as you; does not expire)\n   \
            1. Linear → Settings → Security & access → Personal API keys → New key\n      \
               https://linear.app/settings/account/security\n      \
               Give it Read and Write (or full access) and the teams you want.\n   \
            2. sidekar kv set {token_key} 'lin_api_…' --tag=linear\n   \
            3. sidekar linear add --token {token_key}\n\n\
         B. OAuth app (for a shared workspace app, or where API keys are disabled)\n   \
            1. Linear → Settings → API → OAuth applications → New\n      \
               https://linear.app/settings/api/applications/new\n      \
               Callback URL: {redirect}\n   \
            2. Store the Client ID and Client Secret:\n      \
               sidekar kv set {id_key} '<client id>' --tag=linear,oauth\n      \
               sidekar kv set {secret_key} '<client secret>' --tag=linear,oauth\n   \
            3. sidekar linear login --token {token_key} --client-id {id_key} --client-secret {secret_key}\n      \
               Scopes requested: {scopes}. Tokens last 24h and refresh on their own.\n\n\
         Then: sidekar linear doctor --token {token_key}\n\n\
         Another port? Pass --port N here and to login; the callback becomes\n\
         http://localhost:N/callback and must be registered on the app exactly.",
        scopes = auth::SCOPES,
    )
}

#[cfg(test)]
mod tests;
