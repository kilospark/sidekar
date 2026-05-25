use crate::*;

pub fn extension_tab_id_from_ctx(ctx: &AppContext) -> Option<u64> {
    ctx.override_tab_id
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
}

pub async fn cmd_browser_ext(_ctx: &AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().cloned().unwrap_or_default();
    if sub.is_empty() {
        bail!(
            "Usage: sidekar browser ext <subcommand> [args...]\n\nRun 'sidekar help browser ext' for subcommands."
        );
    }

    let (command, sub_args): (String, Vec<String>) = match sub.as_str() {
        "monitor" => {
            let msub = args
                .get(1)
                .map(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Usage: sidekar browser ext monitor <start|stop|status> [tab_id...|all]"
                    )
                })?;
            let tail: Vec<String> = args.iter().skip(2).cloned().collect();
            match msub {
                "start" => ("monitor-start".to_string(), tail),
                "stop" => ("monitor-stop".to_string(), Vec::new()),
                "status" => ("monitor-status".to_string(), Vec::new()),
                other => anyhow::bail!(
                    "Unknown ext monitor subcommand: {other} (use start, stop, status)"
                ),
            }
        }
        _ => (
            sub,
            if args.len() > 1 {
                args[1..].to_vec()
            } else {
                vec![]
            },
        ),
    };

    let default_tab = extension_tab_id_from_ctx(_ctx);
    crate::ext::send_cli_command(&command, &sub_args, default_tab).await
}
