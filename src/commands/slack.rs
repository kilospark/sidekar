//! `sidekar slack`: sign-in and the Web API commands.
//!
//! One command rather than Google's split (`google` for accounts, `gmail` and
//! friends for the APIs) because there is one API behind it. Account verbs
//! (`login`, `add`, `accounts`, `use`, `status`, `doctor`, `setup`, `logout`)
//! sit beside the API verbs, and every API verb takes `--token <KV_KEY>`.

use super::google::{
    flag, flag_usize, flags_all, one_of, positional_with_switches, reject_unknown_flags,
};
use crate::AppContext;
use crate::slack::{self, Slack, api, auth};
use anyhow::{Result, bail};

/// Flags that take no value, anywhere under `slack`.
const SWITCHES: &[&str] = &[
    "--bot",
    "--pkce",
    "--no-browser",
    "--print-url",
    "--broadcast",
    "--member",
    "--all",
    "--print",
    "--existing-dm",
];

const USAGE: &str = "Usage: sidekar slack <command> …\n\
  Account:\n  \
  setup [--port N] [--app-name NAME]        the app to create, with a ready manifest\n  \
  login --token <KV_KEY> --client-id <KV_KEY> --client-secret <KV_KEY>\n        \
        [--bot] [--team <T…>] [--port N] [--no-browser] [--pkce]   OAuth; user token unless --bot\n  \
  add --token <KV_KEY>                      adopt an xoxp-/xoxb- token already in kv\n  \
  accounts                                  stored tokens; * marks the default\n  \
  use <KV_KEY>                              make it the default\n  \
  status | doctor [--token <KV_KEY>]        who it is / check token and scopes\n  \
  logout [--token <KV_KEY>]\n\
  Read:\n  \
  channels [filter] [--types public,private,dm,group-dm|all] [--member] [--limit N]\n  \
  read <channel> [--limit N] [--thread <ts>]   history, oldest first; --thread for replies\n  \
  search <query> [--limit N]                Slack syntax: from:@x in:#y after:2026-01-01\n  \
  users [filter] [--all] [--limit N]\n  \
  user <person>\n  \
  bookmarks <channel>                       the channel's bookmarks bar\n  \
  file <file-id|file-link>                  name, type, size, owner, where shared\n  \
  download <file-id|file-link> [--out <path|dir/>] [--print]   --print: text files to stdout\n\
  Write:\n  \
  send <channel> TEXT [--thread <ts>] [--broadcast] [--attach <path>]…\n  \
  dm <person> TEXT [--attach <path>]…\n  \
  upload <channel|person|link> <path>… [--text T] [--thread <ts>] [--title T]\n  \
  draft <channel|person|link> TEXT [--thread <ts>] [--existing-dm]   into your Slack Drafts; NOT sent\n  \
        a person drafts into your DM with them, opening one if there is none\n  \
        (Slack shows them nothing until a message is sent); --existing-dm refuses instead\n  \
  TEXT is --text <t> or --text-file <path>; optional when --attach is given.\n  \
  Messages list their files as [file F… name (type, size)].\n\n\
  <channel> is an id, #name, a message link, or a person (@handle, email, U…) for their DM.\n\
  <person> is a user id, an email, or a handle / display name / real name.\n\
  Every command takes --token <KV_KEY> to pick a workspace.";

pub async fn cmd_slack(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    let pos = positional_with_switches(rest, SWITCHES);
    let has = |s: &str| rest.iter().any(|a| a == s);

    match sub {
        "setup" => {
            reject_unknown_flags(
                rest,
                &["--port", "--app-name", "--client-id", "--client-secret"],
            )?;
            let port = port_flag(rest)?;
            let app = flag(rest, "--app-name").unwrap_or_else(|| "Sidekar".into());
            let token_key = flag(rest, "--token").unwrap_or_else(|| "SLACK_MY_TOKEN".into());
            let id_key = flag(rest, "--client-id").unwrap_or_else(|| "SLACK_MY_CLIENT_ID".into());
            let secret_key =
                flag(rest, "--client-secret").unwrap_or_else(|| "SLACK_MY_CLIENT_SECRET".into());
            out!(
                ctx,
                "{}",
                setup_walkthrough(&app, port, &token_key, &id_key, &secret_key)
            );
            Ok(())
        }
        "login" => {
            reject_unknown_flags(
                rest,
                &[
                    "--client-id",
                    "--client-secret",
                    "--bot",
                    "--team",
                    "--port",
                    "--no-browser",
                    "--print-url",
                    "--pkce",
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
            let kind = if has("--bot") {
                auth::Kind::Bot
            } else {
                auth::Kind::User
            };
            let team = flag(rest, "--team");
            let who = auth::login(auth::LoginOptions {
                token_key: &token_key,
                client_id_key: &client_id_key,
                client_secret_key: &client_secret_key,
                kind,
                team: team.as_deref(),
                port: port_flag(rest)?,
                open_browser: !has("--no-browser") && !has("--print-url"),
                pkce: has("--pkce"),
            })
            .await?;
            out!(
                ctx,
                "Stored a {} token for {} in {} under {token_key}.",
                kind.as_str(),
                who.user,
                who.team
            );
            Ok(())
        }
        "add" => {
            reject_unknown_flags(rest, &[])?;
            let token_key = flag(rest, "--token").ok_or_else(|| {
                anyhow::anyhow!(
                    "add needs --token <KV_KEY>, the kv key already holding the token.\n  \
                     sidekar kv set SLACK_TOKEN 'xoxp-…' && sidekar slack add --token SLACK_TOKEN"
                )
            })?;
            let (kind, who) = auth::add(&token_key).await?;
            out!(
                ctx,
                "{token_key} is a {} token for {} in {}.",
                kind.as_str(),
                who.user,
                who.team
            );
            Ok(())
        }
        "accounts" => {
            reject_unknown_flags(rest, &[])?;
            let tokens = auth::tokens()?;
            if tokens.is_empty() {
                out!(
                    ctx,
                    "No Slack tokens stored. `sidekar slack setup` shows how."
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
                    t.kind.as_str(),
                    or_unknown(&t.team),
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
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar slack use <KV_KEY>"))?;
            if !auth::tokens()?.iter().any(|t| t.key == key) {
                bail!("no Slack token stored under {key}; `sidekar slack accounts` lists them");
            }
            auth::set_default_token_key(&key)?;
            out!(ctx, "Default Slack token is now {key}.");
            Ok(())
        }
        "status" => {
            reject_unknown_flags(rest, &[])?;
            let t = auth::resolve_token(flag(rest, "--token").as_deref())?;
            out!(ctx, "Active: {} ({} token)", t.key, t.kind.as_str());
            out!(ctx, "Workspace: {} {}", or_unknown(&t.team), t.url);
            out!(ctx, "Acts as: {}", or_unknown(&t.account));
            out!(ctx, "Stored: {}", auth::tokens()?.len());
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
                "Removed {}. The app stays installed in {} until removed there \
                 (workspace settings > Manage apps).",
                t.key,
                or_unknown(&t.team)
            );
            Ok(())
        }
        "channels" | "read" | "history" | "thread" | "search" | "send" | "post" | "reply"
        | "dm" | "users" | "user" | "draft" | "bookmarks" | "file" | "download" | "upload" => {
            let token = auth::resolve_token(flag(rest, "--token").as_deref())?;
            let slack = Slack::connect(&token).await?;
            api_command(ctx, &slack, &token, sub, rest, &pos).await
        }
        _ => bail!("{USAGE}"),
    }
}

async fn api_command(
    ctx: &mut AppContext,
    slack: &Slack,
    token: &auth::TokenRef,
    sub: &str,
    rest: &[String],
    pos: &[String],
) -> Result<()> {
    match sub {
        "channels" => {
            reject_unknown_flags(rest, &["--types", "--member", "--limit"])?;
            let types = api::conversation_types(
                &flag(rest, "--types").unwrap_or_else(|| "public,private".into()),
            )?;
            let limit = flag_usize(rest, "--limit").unwrap_or(1000);
            let filter = pos.join(" ").trim_start_matches('#').to_lowercase();
            let member_only = rest.iter().any(|a| a == "--member");
            let list = if member_only {
                api::my_channels(slack, &types, limit).await?
            } else {
                api::channels(slack, &types, limit).await?
            };
            let dm_peers: Vec<String> = list.iter().filter_map(|c| c.user.clone()).collect();
            let names = api::names_for(slack, &dm_peers).await;
            let mut shown = 0;
            for c in &list {
                let label = match &c.user {
                    Some(u) => format!("@{}", names.get(u).unwrap_or(u)),
                    None => format!("#{}", c.name),
                };
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                shown += 1;
                out!(
                    ctx,
                    "{}\t{}\t{}{}\t{}\t{}",
                    c.id,
                    label,
                    c.kind,
                    if c.is_member { "" } else { " (not joined)" },
                    c.members
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "-".into()),
                    c.topic.replace('\n', " ")
                );
            }
            if shown == 0 {
                out!(ctx, "No conversations match.");
            }
            Ok(())
        }
        "read" | "history" | "thread" => {
            reject_unknown_flags(rest, &["--limit", "--thread"])?;
            let target = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Usage: sidekar slack read <channel|message-link> [--thread <ts>] [--limit N]"
                )
            })?;
            let limit = flag_usize(rest, "--limit").unwrap_or(30);
            // A message link opens that message's thread: the reply's own
            // thread when it is one, else the thread it would start.
            let link = api::parse_permalink(&target);
            let thread = flag(rest, "--thread")
                .or_else(|| pos.get(1).cloned().filter(|_| sub == "thread"))
                .or_else(|| {
                    link.as_ref()
                        .map(|(_, ts, t)| t.clone().unwrap_or(ts.clone()))
                });
            let channel = api::resolve_channel(slack, &target).await?;
            let msgs = match &thread {
                Some(ts) => api::replies(slack, &channel, ts, limit).await?,
                None => api::history(slack, &channel, limit, None).await?,
            };
            if msgs.is_empty() {
                out!(ctx, "No messages.");
                return Ok(());
            }
            let names = api::names_for(slack, &api::user_ids_in(&msgs)).await;
            for m in &msgs {
                let author = if !m.user.is_empty() {
                    names.get(&m.user).cloned().unwrap_or(m.user.clone())
                } else if !m.username.is_empty() {
                    m.username.clone()
                } else {
                    "-".to_string()
                };
                let text = api::render_text(&m.text, &names);
                let mut lines = text.lines();
                out!(
                    ctx,
                    "{}\t{}\t{}\t{}",
                    m.ts,
                    api::ts_to_date(&m.ts),
                    author,
                    lines.next().unwrap_or("")
                );
                for l in lines {
                    out!(ctx, "    {l}");
                }
                for f in &m.files {
                    out!(ctx, "    [file {}]", f.summary());
                }
                if thread.is_none() && m.reply_count > 0 {
                    out!(
                        ctx,
                        "    [{} repl{} — sidekar slack read {channel} --thread {}]",
                        m.reply_count,
                        if m.reply_count == 1 { "y" } else { "ies" },
                        m.ts
                    );
                }
            }
            Ok(())
        }
        "search" => {
            reject_unknown_flags(rest, &["--limit"])?;
            if token.kind == auth::Kind::Bot {
                bail!(
                    "Slack search needs a user token; {} is a bot token. Run `sidekar slack \
                     login` without --bot, or `slack add` an xoxp- token.",
                    token.key
                );
            }
            let query = pos.join(" ");
            if query.is_empty() {
                bail!(
                    "Usage: sidekar slack search <query> [--limit N]  (from:@x in:#y after:2026-01-01)"
                );
            }
            let limit = flag_usize(rest, "--limit").unwrap_or(20);
            if let Some(note) = search_cap_note(limit) {
                eprintln!("{note}");
            }
            let found = api::search(slack, &query, limit).await?;
            if found.is_empty() {
                out!(ctx, "No messages match {query}.");
                return Ok(());
            }
            let ids: Vec<String> = found
                .iter()
                .flat_map(|m| {
                    std::iter::once(m.user.clone()).chain(api::mentioned_user_ids(&m.text))
                })
                .collect();
            let names = api::names_for(slack, &ids).await;
            for m in found {
                let author = names.get(&m.user).cloned().unwrap_or_else(|| {
                    if m.username.is_empty() {
                        m.user.clone()
                    } else {
                        m.username.clone()
                    }
                });
                let where_ =
                    if m.channel_name.is_empty() || api::looks_like_user_id(&m.channel_name) {
                        m.channel_id.clone()
                    } else {
                        format!("#{}", m.channel_name)
                    };
                out!(
                    ctx,
                    "{}\t{}\t{}\t{}",
                    api::ts_to_date(&m.ts),
                    where_,
                    author,
                    api::render_text(&m.text, &names).replace('\n', " ")
                );
                if !m.permalink.is_empty() {
                    out!(ctx, "    {}", m.permalink);
                }
            }
            Ok(())
        }
        "send" | "post" | "reply" | "dm" => {
            let known: &[&str] = if sub == "dm" {
                &["--text", "--text-file", "--attach"]
            } else {
                &[
                    "--text",
                    "--text-file",
                    "--thread",
                    "--broadcast",
                    "--attach",
                ]
            };
            reject_unknown_flags(rest, known)?;
            let target = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Usage: sidekar slack {sub} <{}> --text <t>|--text-file <path>{}",
                    if sub == "dm" { "person" } else { "channel" },
                    if sub == "dm" {
                        ""
                    } else {
                        " [--thread <ts>] [--broadcast]"
                    }
                )
            })?;
            let attach = flags_all(rest, "--attach");
            let text = one_of(rest, "--text", "--text-file")?.filter(|t| !t.trim().is_empty());
            if text.is_none() && attach.is_empty() {
                bail!("slack {sub} needs --text <t>, --text-file <path>, or --attach <path>");
            }
            // Read the files before anything is posted, so a typo in a path
            // does not leave half a message behind.
            let files = uploads_from(&attach, None)?;
            let channel = if sub == "dm" {
                let user = api::resolve_user(slack, &target).await?;
                api::open_dm(slack, &[user.id]).await?
            } else {
                api::resolve_channel(slack, &target).await?
            };
            // Replying to a message link lands in that message's thread.
            let thread = flag(rest, "--thread")
                .or_else(|| api::parse_permalink(&target).map(|(_, ts, t)| t.unwrap_or(ts)));
            if sub == "reply" && thread.is_none() {
                bail!("slack reply needs --thread <ts> or a message link");
            }
            let broadcast = rest.iter().any(|a| a == "--broadcast");
            if !files.is_empty() {
                if broadcast {
                    bail!("Slack cannot broadcast a file share to the channel; drop --broadcast");
                }
                return share(
                    ctx,
                    slack,
                    &channel,
                    files,
                    text.as_deref(),
                    thread.as_deref(),
                )
                .await;
            }
            let text = text.unwrap_or_default();
            let (ch, ts) = api::post(slack, &channel, &text, thread.as_deref(), broadcast).await?;
            out!(
                ctx,
                "Posted to {ch}{} (ts {ts}).",
                if thread.is_some() { " in thread" } else { "" }
            );
            if let Ok(link) = api::permalink(slack, &ch, &ts).await
                && !link.is_empty()
            {
                out!(ctx, "{link}");
            }
            Ok(())
        }
        "file" => {
            reject_unknown_flags(rest, &[])?;
            let id = file_arg(pos, "file")?;
            let f = api::file_info(slack, &id).await?;
            let names = api::names_for(slack, std::slice::from_ref(&f.user)).await;
            out!(ctx, "{}\t{}", f.id, f.display_name());
            if !f.title.is_empty() && f.title != f.name {
                out!(ctx, "title:    {}", f.title);
            }
            out!(
                ctx,
                "type:     {}{}",
                if f.mimetype.is_empty() {
                    "-"
                } else {
                    &f.mimetype
                },
                if f.filetype.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", f.filetype)
                }
            );
            out!(ctx, "size:     {}", crate::attachments::human_bytes(f.size));
            if !f.mode.is_empty() {
                out!(ctx, "mode:     {}", f.mode);
            }
            if !f.user.is_empty() {
                out!(
                    ctx,
                    "by:       {}",
                    names.get(&f.user).cloned().unwrap_or(f.user.clone())
                );
            }
            if f.created > 0 {
                out!(ctx, "created:  {}", api::ts_to_date(&f.created.to_string()));
            }
            if !f.channels.is_empty() {
                out!(ctx, "shared in: {}", f.channels.join(", "));
            }
            if !f.external_url.is_empty() {
                out!(ctx, "external: {}", f.external_url);
            }
            if !f.permalink.is_empty() {
                out!(ctx, "{}", f.permalink);
            }
            Ok(())
        }
        "download" => {
            reject_unknown_flags(rest, &["--out", "--print"])?;
            let id = file_arg(pos, "download")?;
            let f = api::file_info(slack, &id).await?;
            let bytes = api::file_download(slack, &f).await?;
            if rest.iter().any(|a| a == "--print") {
                out!(
                    ctx,
                    "{}",
                    crate::attachments::printable(f.display_name(), &bytes)?
                );
            } else {
                let path = crate::attachments::save(
                    flag(rest, "--out").as_deref(),
                    f.display_name(),
                    &bytes,
                )?;
                out!(ctx, "Wrote {} ({} bytes).", path.display(), bytes.len());
            }
            Ok(())
        }
        "upload" => {
            reject_unknown_flags(rest, &["--text", "--text-file", "--thread", "--title"])?;
            let target = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Usage: sidekar slack upload <channel|person|link> <path>… [--text T] [--thread <ts>] [--title T]"
                )
            })?;
            let paths: Vec<String> = pos[1..].to_vec();
            if paths.is_empty() {
                bail!("slack upload needs at least one file path after the channel");
            }
            let text = one_of(rest, "--text", "--text-file")?;
            let channel = api::resolve_channel(slack, &target).await?;
            let thread = flag(rest, "--thread")
                .or_else(|| api::parse_permalink(&target).map(|(_, ts, t)| t.unwrap_or(ts)));
            let title = flag(rest, "--title");
            if title.is_some() && paths.len() > 1 {
                bail!("--title names one file; upload several without it");
            }
            let files = uploads_from(&paths, title)?;
            share(
                ctx,
                slack,
                &channel,
                files,
                text.as_deref(),
                thread.as_deref(),
            )
            .await
        }
        "draft" => {
            if rest.iter().any(|a| a == "--attach") {
                bail!(
                    "a Slack draft cannot carry files from here: drafts.create is undocumented \
                     and nothing confirms it accepts files uploaded with an OAuth token. Draft \
                     the text, then attach the file in Slack, or use `slack upload` to post it."
                );
            }
            reject_unknown_flags(
                rest,
                &["--text", "--text-file", "--thread", "--existing-dm"],
            )?;
            if token.kind == auth::Kind::Bot {
                bail!(
                    "a draft lives in a person's own composer, so it needs a user token; {} is a \
                     bot token. Run `sidekar slack login` without --bot.",
                    token.key
                );
            }
            let target = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Usage: sidekar slack draft <channel|person|message-link> --text <t>|--text-file <path> [--thread <ts>] [--existing-dm]"
                )
            })?;
            let text = one_of(rest, "--text", "--text-file")?
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("slack draft needs --text <t> or --text-file <path>")
                })?;
            let channel =
                draft_channel(slack, &target, rest.iter().any(|a| a == "--existing-dm")).await?;
            let thread = flag(rest, "--thread")
                .or_else(|| api::parse_permalink(&target).map(|(_, ts, t)| t.unwrap_or(ts)));
            let id = api::draft_create(slack, &channel, &text, thread.as_deref()).await?;
            out!(
                ctx,
                "Drafted in {channel}{} (draft {}). Nothing has been sent: it is in Slack under \
                 Drafts & Sent for you to edit, send or discard.",
                if thread.is_some() { ", in thread" } else { "" },
                if id.is_empty() { "?" } else { &id }
            );
            if !token.team_id.is_empty() {
                out!(
                    ctx,
                    "https://app.slack.com/client/{}/{channel}",
                    token.team_id
                );
            }
            Ok(())
        }
        "bookmarks" => {
            reject_unknown_flags(rest, &[])?;
            let target = pos
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Usage: sidekar slack bookmarks <channel>"))?;
            let channel = api::resolve_channel(slack, &target).await?;
            let found = api::bookmarks(slack, &channel).await?;
            if found.is_empty() {
                out!(ctx, "No bookmarks in {target}.");
            }
            for b in found {
                out!(
                    ctx,
                    "{}\t{}{}\t{}",
                    b.kind,
                    if b.emoji.is_empty() {
                        String::new()
                    } else {
                        format!("{} ", b.emoji)
                    },
                    b.title,
                    b.link
                );
            }
            Ok(())
        }
        "users" => {
            reject_unknown_flags(rest, &["--all", "--limit"])?;
            let all = rest.iter().any(|a| a == "--all");
            let filter = pos.join(" ").trim_start_matches('@').to_lowercase();
            let limit = flag_usize(rest, "--limit").unwrap_or(usize::MAX);
            let mut shown = 0;
            for u in api::users(slack, usize::MAX).await? {
                if !all && (u.deleted || u.is_bot) {
                    continue;
                }
                let hay = format!("{} {} {} {}", u.name, u.display_name, u.real_name, u.email)
                    .to_lowercase();
                if !filter.is_empty() && !hay.contains(&filter) {
                    continue;
                }
                if shown >= limit {
                    break;
                }
                shown += 1;
                out!(
                    ctx,
                    "{}\t@{}\t{}\t{}{}",
                    u.id,
                    u.name,
                    u.label(),
                    u.email,
                    if u.deleted {
                        "\t(deactivated)"
                    } else if u.is_bot {
                        "\t(bot)"
                    } else {
                        ""
                    }
                );
            }
            if shown == 0 {
                out!(ctx, "No users match.");
            }
            Ok(())
        }
        "user" => {
            reject_unknown_flags(rest, &[])?;
            let who = pos.first().cloned().ok_or_else(|| {
                anyhow::anyhow!("Usage: sidekar slack user <id|email|@handle|name>")
            })?;
            let u = api::resolve_user(slack, &who).await?;
            out!(ctx, "{}\t@{}", u.id, u.name);
            out!(ctx, "Name: {}", u.real_name);
            if !u.display_name.is_empty() {
                out!(ctx, "Display: {}", u.display_name);
            }
            if !u.email.is_empty() {
                out!(ctx, "Email: {}", u.email);
            }
            if !u.tz.is_empty() {
                out!(ctx, "Timezone: {}", u.tz);
            }
            if u.is_bot {
                out!(ctx, "Bot: yes");
            }
            if u.deleted {
                out!(ctx, "Deactivated: yes");
            }
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}

/// One doctor probe: a label, a read-only method, and its arguments.
type Probe = (&'static str, &'static str, Vec<(&'static str, String)>);

/// Check the token resolves, mints, identifies, and reaches each API family.
async fn doctor(ctx: &mut AppContext, requested: Option<&str>) -> Result<()> {
    let stored = auth::tokens()?;
    out!(ctx, "Stored tokens: {}", stored.len());
    if stored.is_empty() {
        out!(ctx, "  none — run `sidekar slack setup`");
        return Ok(());
    }
    let token = auth::resolve_token(requested)?;
    out!(
        ctx,
        "Checking {} ({} token)",
        token.key,
        token.kind.as_str()
    );
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
    let slack = match Slack::connect(&token).await {
        Ok(s) => s,
        Err(e) => {
            out!(ctx, "  FAIL token\n{e}");
            return Ok(());
        }
    };
    match auth::identify(&slack).await {
        Ok(who) => out!(ctx, "  ok   auth.test: {} in {}", who.user, who.team),
        Err(e) => {
            out!(ctx, "  FAIL auth.test: {e}");
            return Ok(());
        }
    }
    let mut checks: Vec<Probe> = vec![
        (
            "channels",
            "conversations.list",
            vec![("limit", "1".into())],
        ),
        ("users", "users.list", vec![("limit", "1".into())]),
    ];
    if token.kind == auth::Kind::User {
        checks.push((
            "search",
            "search.messages",
            vec![("query", "a".into()), ("count", "1".into())],
        ));
    }
    for (name, method, params) in checks {
        match slack.get(method, &params).await {
            Ok(_) => out!(ctx, "  ok   {name}"),
            Err(e) => out!(
                ctx,
                "  FAIL {name}: {}",
                e.to_string().lines().next().unwrap_or("")
            ),
        }
    }
    Ok(())
}

/// Where a draft goes. A person means your DM with them; `conversations.open`
/// creates that DM if there is none (invisible to them until something is
/// sent). With `existing_only`, a person you have no DM with is refused
/// rather than opened.
pub(crate) async fn draft_channel(
    slack: &crate::slack::Slack,
    target: &str,
    existing_only: bool,
) -> Result<String> {
    if !(existing_only && api::is_person(target)) {
        return api::resolve_channel(slack, target).await;
    }
    let user = api::resolve_user(slack, target).await?;
    api::existing_dm(slack, &user.id).await?.ok_or_else(|| {
        anyhow::anyhow!(
            "you have no DM with {target} yet, and --existing-dm says not to open one. \
             Drop --existing-dm to open it (they see nothing until a message is sent)."
        )
    })
}

/// The warning when `--limit` asks for more than one search page holds.
pub(crate) fn search_cap_note(limit: usize) -> Option<String> {
    (limit > api::SEARCH_MAX).then(|| {
        format!(
            "note: Slack search returns at most {} matches per request; showing the newest {}, \
             not {limit}. Narrow the query (after:, in:, from:) to reach older ones.",
            api::SEARCH_MAX,
            api::SEARCH_MAX
        )
    })
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

/// The steps for creating the Slack app, with a manifest that sets every
/// scope and the redirect URL in one paste.
pub(crate) fn setup_walkthrough(
    app: &str,
    port: u16,
    token_key: &str,
    id_key: &str,
    secret_key: &str,
) -> String {
    let manifest = serde_json::to_string_pretty(&auth::manifest(app, port)).unwrap_or_default();
    format!(
        "Creating a Slack app for sidekar\n\n\
         1. Create the app from a manifest:\n   \
            https://api.slack.com/apps?new_app=1  →  From an app manifest  →  pick the workspace\n   \
            Paste this (JSON tab):\n\n{manifest}\n\n   \
            It requests user scopes (act as you: read, post, DM, search, bookmarks) and bot scopes\n   \
            (act as the app, no search), registers {redirect} as the redirect,\n   \
            and leaves token rotation off so a token does not expire under you.\n\n\
         2. Basic Information → App Credentials. Store the Client ID and Client Secret:\n   \
            sidekar kv set {id_key} '<client id>' --tag=slack,oauth\n   \
            sidekar kv set {secret_key} '<client secret>' --tag=slack,oauth\n\n\
         3. Authorize (a user token; add --bot for the app's bot token instead):\n   \
            sidekar slack login --token {token_key} --client-id {id_key} --client-secret {secret_key}\n   \
            A workspace that restricts apps sends this to an admin for approval first.\n\n\
         4. Confirm:\n   \
            sidekar slack doctor --token {token_key}\n\n\
         Already have a token (OAuth & Permissions → User/Bot OAuth Token after installing)?\n   \
            sidekar kv set {token_key} 'xoxp-…' && sidekar slack add --token {token_key}\n\n\
         Using another port? Pass --port N here and to login; the redirect becomes\n\
         http://localhost:N/callback and must be listed on the app.",
        redirect = slack::auth::redirect_uri(port),
    )
}

/// A file id from the first positional (an id or a file link).
fn file_arg(pos: &[String], sub: &str) -> Result<String> {
    let raw = pos
        .first()
        .ok_or_else(|| anyhow::anyhow!("Usage: sidekar slack {sub} <file-id|file-link>"))?;
    api::parse_file_id(raw).ok_or_else(|| {
        anyhow::anyhow!(
            "{raw} is not a Slack file id (F…) or file link; `slack read` lists files on messages"
        )
    })
}

/// Read local files for upload; `title` applies to a single file.
pub(crate) fn uploads_from(paths: &[String], title: Option<String>) -> Result<Vec<api::Upload>> {
    // Every path from metadata first, so a bad last path or an oversized
    // file is reported before gigabytes of the others are read.
    for p in paths {
        crate::attachments::check_upload(p, crate::attachments::SLACK_UPLOAD)?;
    }
    paths
        .iter()
        .map(|p| {
            let (bytes, name, _) =
                crate::attachments::read_upload(p, crate::attachments::SLACK_UPLOAD)?;
            Ok(api::Upload {
                name,
                title: title.clone(),
                bytes,
            })
        })
        .collect()
}

/// Upload and share files, then say where they went.
async fn share(
    ctx: &mut AppContext,
    slack: &Slack,
    channel: &str,
    files: Vec<api::Upload>,
    text: Option<&str>,
    thread: Option<&str>,
) -> Result<()> {
    let n = files.len();
    let shared = api::upload_files(slack, channel, files, text, thread).await?;
    out!(
        ctx,
        "Shared {n} file(s) in {channel}{}{}.",
        if thread.is_some() { ", in thread" } else { "" },
        if text.is_some_and(|t| !t.trim().is_empty()) {
            " with a message"
        } else {
            ""
        }
    );
    for (id, link) in shared {
        out!(ctx, "{id}\t{link}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
