use crate::*;

use super::dispatch;

pub async fn cmd_browser_run(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let session_id = args
        .first()
        .context("Usage: sidekar browser run <sessionId> [<subcommand> args...]")?;
    ctx.set_current_session(session_id.clone());
    ctx.hydrate_connection_from_state()?;

    if args.len() > 1 {
        Box::pin(dispatch(ctx, "browser", &args[1..])).await
    } else {
        run_command_file(ctx).await
    }
}

async fn run_command_file(ctx: &mut AppContext) -> Result<()> {
    let session_id = ctx.require_session_id()?.to_string();
    let cmd_file = ctx.command_file(&session_id);
    let content = fs::read_to_string(&cmd_file)
        .with_context(|| format!("Cannot read {}", cmd_file.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Invalid JSON in {}", cmd_file.display()))?;

    let entries = if parsed.is_array() {
        serde_json::from_value::<Vec<CommandFileEntry>>(parsed)?
    } else {
        vec![serde_json::from_value::<CommandFileEntry>(parsed)?]
    };

    for entry in entries {
        if entry.command.trim().is_empty() {
            bail!("Missing \"command\" field in command file");
        }
        let mut browser_args: Vec<String> = vec![entry.command.clone()];
        browser_args.extend(entry.args.iter().map(json_value_to_arg));
        Box::pin(dispatch(ctx, "browser", &browser_args)).await?;
    }

    Ok(())
}
