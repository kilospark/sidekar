use crate::*;
use totp_rs::{Algorithm, Secret, TOTP};

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub async fn cmd_totp(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar totp <add|list|get|remove> [args...]");
    }
    match args[0].as_str() {
        "add" => cmd_totp_add(ctx, &args[1..]).await,
        "list" => cmd_totp_list(ctx).await,
        "get" => cmd_totp_get(ctx, &args[1..]).await,
        "remove" | "delete" | "rm" => cmd_totp_remove(ctx, &args[1..]).await,
        _ => bail!(
            "Unknown subcommand: {}. Use: add, list, get, remove",
            args[0]
        ),
    }
}

async fn cmd_totp_add(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 3 {
        bail!(
            "Usage: sidekar totp add <service> <account> <secret> [--algorithm=SHA1] [--digits=6] [--period=30]"
        );
    }
    let service = &args[0];
    let account = &args[1];
    let secret = &args[2];

    let mut algorithm = "SHA1".to_string();
    let mut digits: i32 = 6;
    let mut period: i32 = 30;

    for arg in args.iter().skip(3) {
        if let Some(a) = arg.strip_prefix("--algorithm=") {
            algorithm = a.to_uppercase();
        } else if let Some(d) = arg.strip_prefix("--digits=") {
            digits = d.parse().unwrap_or(6);
        } else if let Some(p) = arg.strip_prefix("--period=") {
            period = p.parse().unwrap_or(30);
        }
    }

    let algo = match algorithm.as_str() {
        "SHA1" => Algorithm::SHA1,
        "SHA256" => Algorithm::SHA256,
        "SHA512" => Algorithm::SHA512,
        _ => bail!(
            "Invalid algorithm: {}. Use SHA1, SHA256, or SHA512",
            algorithm
        ),
    };

    let secret_bytes = Secret::Encoded(secret.to_string())
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("Invalid secret (expected base32): {}", e))?;

    let totp = TOTP::new(
        algo,
        digits as usize,
        1,
        period as u64,
        secret_bytes,
        None,
        (*account).to_string(),
    )
    .map_err(|e| anyhow::anyhow!("Invalid TOTP: {}", e))?;

    let now = unix_now();
    let _ = totp.generate(now);

    crate::broker::totp_add(service, account, secret, &algorithm, digits, period)?;
    out!(
        ctx,
        "Added TOTP for {} ({}). Current code: {}",
        service,
        account,
        totp.generate(now)
    );
    Ok(())
}

async fn cmd_totp_list(ctx: &mut AppContext) -> Result<()> {
    let secrets = crate::broker::totp_list()?;
    if secrets.is_empty() {
        out!(ctx, "No TOTP secrets stored.");
        return Ok(());
    }
    out!(ctx, "TOTP secrets:");
    for s in secrets {
        out!(
            ctx,
            "  {} {} ({} digits, {}s period)",
            s.service,
            s.account,
            s.digits,
            s.period
        );
    }
    Ok(())
}

async fn cmd_totp_get(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar totp get <service> <account>");
    }
    let service = &args[0];
    let account = &args[1];

    let rec = crate::broker::totp_get(service, account)?
        .ok_or_else(|| anyhow::anyhow!("No TOTP secret found for {} ({})", service, account))?;

    let algo = match rec.algorithm.as_str() {
        "SHA1" => Algorithm::SHA1,
        "SHA256" => Algorithm::SHA256,
        "SHA512" => Algorithm::SHA512,
        _ => Algorithm::SHA1,
    };

    let secret_bytes = Secret::Encoded(rec.secret.clone())
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("Invalid stored secret: {}", e))?;

    let totp = TOTP::new(
        algo,
        rec.digits as usize,
        1,
        rec.period as u64,
        secret_bytes,
        None,
        rec.account.clone(),
    )
    .map_err(|e| anyhow::anyhow!("Failed to create TOTP: {}", e))?;

    let now = unix_now();
    let code = totp.generate(now);
    out!(ctx, "{code}");
    Ok(())
}

async fn cmd_totp_remove(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar totp remove <id>");
    }
    let id = args[0].parse::<i64>().context("Invalid ID")?;
    crate::broker::totp_delete(id)?;
    out!(ctx, "Deleted TOTP secret {}.", id);
    Ok(())
}
