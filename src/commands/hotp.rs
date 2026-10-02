//! `sidekar hotp`: counter-based one-time passwords (RFC 4226), as used by
//! Duo Mobile passcodes.
//!
//! Stored beside TOTP secrets, with the counter the next code is made from.
//! `get` issues a code and advances the counter in one step, so the same
//! code is never handed out twice — an HOTP server refuses a code it has
//! already accepted.

use crate::*;

const USAGE: &str = "\
Usage: sidekar hotp add <service> <account> <secret> [--counter=N] [--digits=6] [--algorithm=SHA1]
       sidekar hotp get <service> <account>
       sidekar hotp list
       sidekar hotp show <service> <account>
       sidekar hotp counter <service> <account> <N>
       sidekar hotp remove <id>";

pub async fn cmd_hotp(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        None | Some("list") => cmd_list(ctx),
        Some("add") => cmd_add(ctx, rest),
        Some("get") => cmd_get(ctx, rest),
        Some("show") => cmd_show(ctx, rest),
        Some("counter") => cmd_counter(ctx, rest),
        Some("remove") | Some("delete") | Some("rm") => cmd_remove(ctx, rest),
        Some(other) => bail!("Unknown subcommand: {other}\n{USAGE}"),
    }
}

fn plain(ctx: &mut AppContext, text: impl Into<String>) -> Result<()> {
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(text))?
    );
    Ok(())
}

fn cmd_add(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [service, account, secret] = positional[..] else {
        bail!("{USAGE}");
    };
    let mut counter: u64 = 0;
    let mut digits: i32 = 6;
    let mut algorithm = "SHA1".to_string();
    for arg in args.iter().filter(|a| a.starts_with("--")) {
        if let Some(v) = arg.strip_prefix("--counter=") {
            counter = v
                .parse()
                .map_err(|_| anyhow!("--counter takes a whole number, not {v:?}"))?;
        } else if let Some(v) = arg.strip_prefix("--digits=") {
            digits = v
                .parse()
                .ok()
                .filter(|d| (6..=8).contains(d))
                .ok_or_else(|| anyhow!("--digits takes 6, 7 or 8, not {v:?}"))?;
        } else if let Some(v) = arg.strip_prefix("--algorithm=") {
            algorithm = v.to_uppercase();
        } else {
            bail!("unknown option {arg}\n{USAGE}");
        }
    }
    if !matches!(algorithm.as_str(), "SHA1" | "SHA256" | "SHA512") {
        bail!("--algorithm takes SHA1, SHA256 or SHA512, not {algorithm:?}");
    }
    let secret = crate::secrets::normalize_totp_secret(secret)?;
    // Reject a bad secret here rather than on the first `get`.
    crate::secrets::hotp_code(&secret, &algorithm, digits, counter)?;

    crate::broker::hotp_add(service, account, &secret, &algorithm, digits, counter)?;
    crate::commands::push_sync_after_mutation();
    plain(
        ctx,
        format!("Added HOTP for {service} ({account}), next counter {counter}."),
    )
}

fn cmd_get(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let [service, account] = args else {
        bail!("Usage: sidekar hotp get <service> <account>");
    };
    // get_totp_code routes to the HOTP path, which advances the counter.
    let code = crate::secrets::get_totp_code(service, account)?
        .ok_or_else(|| anyhow!("No HOTP secret for {service} ({account})"))?;
    plain(ctx, code)
}

fn cmd_counter(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let [service, account, n] = args else {
        bail!("Usage: sidekar hotp counter <service> <account> <N>");
    };
    let counter: u64 = n
        .parse()
        .map_err(|_| anyhow!("counter takes a whole number, not {n:?}"))?;
    if !crate::broker::hotp_set_counter(service, account, counter)? {
        bail!("No HOTP secret for {service} ({account})");
    }
    crate::commands::push_sync_after_mutation();
    plain(ctx, format!("{service} ({account}) next counter {counter}."))
}

fn cmd_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let [service, account] = args else {
        bail!("Usage: sidekar hotp show <service> <account>");
    };
    let rec = crate::broker::totp_get(service, account)?
        .filter(|r| r.kind == crate::broker::KIND_HOTP)
        .ok_or_else(|| anyhow!("No HOTP secret for {service} ({account})"))?;
    plain(
        ctx,
        format!(
            "service:   {}\naccount:   {}\nkey:       {}\nalgorithm: {}\ndigits:    {}\ncounter:   {}",
            rec.service, rec.account, rec.secret, rec.algorithm, rec.digits, rec.counter
        ),
    )
}

#[derive(serde::Serialize)]
struct HotpOut {
    id: i64,
    service: String,
    account: String,
    algorithm: String,
    digits: i32,
    counter: u64,
}

#[derive(serde::Serialize)]
struct HotpListOutput {
    items: Vec<HotpOut>,
}

impl crate::output::CommandOutput for HotpListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "0 HOTP secrets.")?;
            return Ok(());
        }
        writeln!(w, "{} HOTP secrets:", self.items.len())?;
        for s in &self.items {
            writeln!(
                w,
                "  [{}] {} {} ({} digits, counter {})",
                s.id, s.service, s.account, s.digits, s.counter
            )?;
        }
        Ok(())
    }
}

fn cmd_list(ctx: &mut AppContext) -> Result<()> {
    let items = crate::broker::totp_list()?
        .into_iter()
        .filter(|s| s.kind == crate::broker::KIND_HOTP)
        .map(|s| HotpOut {
            id: s.id,
            service: s.service,
            account: s.account,
            algorithm: s.algorithm,
            digits: s.digits,
            counter: s.counter,
        })
        .collect();
    out!(ctx, "{}", crate::output::to_string(&HotpListOutput { items })?);
    Ok(())
}

fn cmd_remove(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id: i64 = args
        .first()
        .and_then(|a| a.parse().ok())
        .ok_or_else(|| anyhow!("Usage: sidekar hotp remove <id>"))?;
    crate::broker::totp_delete(id)?;
    crate::commands::push_sync_after_mutation();
    plain(ctx, format!("Removed HOTP secret {id}."))
}
