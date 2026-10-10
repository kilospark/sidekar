//! `sidekar slack`: sign-in and the Web API commands.
//!
//! One command rather than Google's split (`google` for accounts, `gmail` and
//! friends for the APIs) because there is one API behind it. Account verbs
//! (`login`, `add`, `accounts`, `use`, `status`, `doctor`, `setup`, `logout`)
//! sit beside the API verbs, and every API verb takes `--token <KV_KEY>`.

use super::google::{flag, flag_usize, one_of, positional_with_switches, reject_unknown_flags};
use crate::AppContext;
use crate::slack::{self, Slack, api, auth};
use anyhow::{Result, bail};

/// Flags that take no value, anywhere under `slack`.
const SWITCHES: &[&str] = &[
    "--bot",
    "--no-browser",
    "--print-url",
    "--broadcast",
    "--member",
    "--all",
];

const USAGE: &str = "Usage: sidekar slack <command> …\n\
  Account:\n  \
  setup [--port N] [--app-name NAME]        the app to create, with a ready manifest\n  \
  login --token <KV_KEY> --client-id <KV_KEY> --client-secret <KV_KEY>\n        \
        [--bot] [--team <T…>] [--port N] [--no-browser]   OAuth; user token unless --bot\n  \
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
  user <person>\n\
  Write:\n  \
  send <channel> TEXT [--thread <ts>] [--broadcast]\n  \
  dm <person> TEXT\n  \
  TEXT is --text <t> or --text-file <path>\n\n\
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
        | "dm" | "users" | "user" => {
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
                if !m.files.is_empty() {
                    out!(ctx, "    [files: {}]", m.files.join(", "));
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
            let found =
                api::search(slack, &query, flag_usize(rest, "--limit").unwrap_or(20)).await?;
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
                &["--text", "--text-file"]
            } else {
                &["--text", "--text-file", "--thread", "--broadcast"]
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
            let text = one_of(rest, "--text", "--text-file")?
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("slack {sub} needs --text <t> or --text-file <path>")
                })?;
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
            It requests user scopes (act as you: read, post, DM, search) and bot scopes\n   \
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

#[cfg(test)]
mod tests;
