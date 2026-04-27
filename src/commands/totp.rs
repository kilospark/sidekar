use crate::*;
use qrcode::{EcLevel, QrCode};
use totp_rs::{Algorithm, Secret, TOTP};

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub async fn cmd_totp(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        return cmd_totp_list(ctx).await;
    }
    match args[0].as_str() {
        "add" => cmd_totp_add(ctx, &args[1..]).await,
        "list" => cmd_totp_list(ctx).await,
        "get" => cmd_totp_get(ctx, &args[1..]).await,
        "show" => cmd_totp_show(ctx, &args[1..]).await,
        "qr" => cmd_totp_qr(ctx, &args[1..]).await,
        "remove" | "delete" | "rm" => cmd_totp_remove(ctx, &args[1..]).await,
        _ => bail!(
            "Unknown subcommand: {}. Use: add, list, get, show, qr, remove",
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
    let msg = format!(
        "Added TOTP for {} ({}). Current code: {}",
        service,
        account,
        totp.generate(now)
    );
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}

#[derive(serde::Serialize)]
struct TotpSecretOut {
    id: i64,
    service: String,
    account: String,
    algorithm: String,
    digits: i32,
    period: i32,
}

#[derive(serde::Serialize)]
struct TotpListOutput {
    items: Vec<TotpSecretOut>,
}

impl crate::output::CommandOutput for TotpListOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "0 TOTP secrets.")?;
            return Ok(());
        }
        writeln!(w, "{} TOTP secrets:", self.items.len())?;
        for s in &self.items {
            writeln!(
                w,
                "  [{}] {} {} ({} digits, {}s period)",
                s.id, s.service, s.account, s.digits, s.period
            )?;
        }
        Ok(())
    }
}

async fn cmd_totp_list(ctx: &mut AppContext) -> Result<()> {
    let secrets = crate::broker::totp_list()?;
    let output = TotpListOutput {
        items: secrets
            .into_iter()
            .map(|s| TotpSecretOut {
                id: s.id,
                service: s.service,
                account: s.account,
                algorithm: s.algorithm,
                digits: s.digits,
                period: s.period,
            })
            .collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
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
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(code))?
    );
    Ok(())
}

async fn cmd_totp_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar totp show <service> <account>");
    }
    let service = &args[0];
    let account = &args[1];

    let rec = crate::broker::totp_get(service, account)?
        .ok_or_else(|| anyhow::anyhow!("No TOTP secret found for {} ({})", service, account))?;

    let msg = format!(
        "service:   {}\naccount:   {}\nkey:       {}\nalgorithm: {}\ndigits:    {}\nperiod:    {}s",
        rec.service, rec.account, rec.secret, rec.algorithm, rec.digits, rec.period
    );
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}

async fn cmd_totp_qr(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: sidekar totp qr <service> <account>");
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
        Some(rec.service.clone()),
        rec.account.clone(),
    )
    .map_err(|e| anyhow::anyhow!("Failed to create TOTP: {}", e))?;

    let uri = totp.get_url();

    let code = QrCode::with_error_correction_level(uri.as_bytes(), EcLevel::M)
        .map_err(|e| anyhow::anyhow!("Failed to generate QR code: {}", e))?;

    // Render using Unicode half-block characters (▀/▄/█/ ) — 2 rows per char row.
    // Each module = 1 cell; quiet zone of 2 added on each side.
    let mut buf = String::new();
    let matrix = code.to_colors();
    let width = code.width();
    let quiet = 2usize;
    let total_w = width + quiet * 2;

    // top quiet rows
    for _ in 0..quiet {
        buf.push('\n');
    }

    // rows in pairs: upper half = row i, lower half = row i+1
    let mut row = 0usize;
    while row < width {
        // left quiet
        for _ in 0..quiet {
            buf.push(' ');
        }
        for col in 0..width {
            let top = matrix[row * width + col] == qrcode::Color::Dark;
            let bot = if row + 1 < width {
                matrix[(row + 1) * width + col] == qrcode::Color::Dark
            } else {
                false
            };
            buf.push(match (top, bot) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        // right quiet
        for _ in 0..quiet {
            buf.push(' ');
        }
        buf.push('\n');
        row += 2;
    }

    // bottom quiet rows
    for _ in 0..quiet {
        for _ in 0..total_w {
            buf.push(' ');
        }
        buf.push('\n');
    }

    buf.push('\n');
    buf.push_str(&uri);
    buf.push('\n');

    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(buf))?
    );
    Ok(())
}

async fn cmd_totp_remove(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: sidekar totp remove <id>  OR  sidekar totp remove <service> <account>");
    }
    let id = if args.len() >= 2 {
        // service + account form
        let service = &args[0];
        let account = &args[1];
        let rec = crate::broker::totp_get(service, account)?
            .ok_or_else(|| anyhow::anyhow!("No TOTP secret found for {} ({})", service, account))?;
        rec.id
    } else {
        // numeric id form
        args[0].parse::<i64>().context("Expected a numeric ID or <service> <account>")?
    };
    crate::broker::totp_delete(id)?;
    let msg = format!("Deleted TOTP secret {}.", id);
    out!(
        ctx,
        "{}",
        crate::output::to_string(&crate::output::PlainOutput::new(msg))?
    );
    Ok(())
}
