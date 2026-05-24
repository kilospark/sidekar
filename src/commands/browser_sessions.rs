use crate::*;

#[derive(serde::Serialize)]
struct BrowserSessionSummary {
    id: String,
    browser: String,
    profile: String,
    tab_count: usize,
    active_tab: String,
    updated: String,
}

#[derive(serde::Serialize)]
struct BrowserSessionsOutput {
    items: Vec<BrowserSessionSummary>,
}

impl crate::output::CommandOutput for BrowserSessionsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No browser sessions.")?;
            return Ok(());
        }
        writeln!(
            w,
            "{:<10} {:<10} {:<12} {:<6} {:<10} UPDATED",
            "ID", "BROWSER", "PROFILE", "TABS", "ACTIVE"
        )?;
        for s in &self.items {
            writeln!(
                w,
                "{:<10} {:<10} {:<12} {:<6} {:<10} {}",
                s.id, s.browser, s.profile, s.tab_count, s.active_tab, s.updated
            )?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct BrowserSessionDetail {
    id: String,
    browser: String,
    profile: String,
    host: String,
    port: Option<u16>,
    active_tab: Option<String>,
    tabs: Vec<String>,
    window_id: Option<i64>,
    state_file: String,
    updated: String,
}

impl crate::output::CommandOutput for BrowserSessionDetail {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "id: {}", self.id)?;
        writeln!(w, "browser: {}", self.browser)?;
        writeln!(w, "profile: {}", self.profile)?;
        writeln!(w, "host: {}", self.host)?;
        writeln!(
            w,
            "port: {}",
            self.port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string())
        )?;
        writeln!(
            w,
            "active_tab: {}",
            self.active_tab.as_deref().unwrap_or("-")
        )?;
        writeln!(
            w,
            "tabs: {}",
            if self.tabs.is_empty() {
                "-".to_string()
            } else {
                self.tabs.join(", ")
            }
        )?;
        writeln!(
            w,
            "window_id: {}",
            self.window_id
                .map(|x| x.to_string())
                .unwrap_or_else(|| "-".to_string())
        )?;
        writeln!(w, "state_file: {}", self.state_file)?;
        writeln!(w, "updated: {}", self.updated)?;
        writeln!(w)?;
        writeln!(
            w,
            "Run commands with: sidekar run {} browser <subcommand> [args...]",
            self.id
        )?;
        Ok(())
    }
}

fn format_session_age(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.0}s ago")
    } else if secs < 3600.0 {
        format!("{:.0}m ago", secs / 60.0)
    } else if secs < 86400.0 {
        format!("{:.0}h ago", secs / 3600.0)
    } else {
        format!("{:.0}d ago", secs / 86400.0)
    }
}

pub fn cmd_browser_sessions(args: &[String]) -> Result<()> {
    let ctx = AppContext::new()?;
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => {
            let sessions = list_browser_sessions(&ctx)?;
            let items = sessions
                .into_iter()
                .map(|s| BrowserSessionSummary {
                    id: s.session_id,
                    browser: s.browser_name.unwrap_or_else(|| "-".into()),
                    profile: s.profile.unwrap_or_else(|| "default".into()),
                    tab_count: s.tabs.len(),
                    active_tab: s.active_tab_id.unwrap_or_else(|| "-".into()),
                    updated: s
                        .updated_at
                        .and_then(|ts| ts.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| format_session_age(d.as_secs_f64()))
                        .unwrap_or_else(|| "-".to_string()),
                })
                .collect();
            crate::output::emit(&BrowserSessionsOutput { items })?;
            Ok(())
        }
        "show" => {
            let session_id = args
                .get(1)
                .context("Usage: sidekar browser sessions show <sessionId>")?;
            let session = get_browser_session(&ctx, session_id)?;
            let detail = BrowserSessionDetail {
                id: session.session_id,
                browser: session.browser_name.unwrap_or_else(|| "-".into()),
                profile: session.profile.unwrap_or_else(|| "default".into()),
                host: session
                    .host
                    .unwrap_or_else(|| DEFAULT_CDP_HOST.into()),
                port: session.port,
                active_tab: session.active_tab_id,
                tabs: session.tabs,
                window_id: session.window_id,
                state_file: session.state_path.display().to_string(),
                updated: session
                    .updated_at
                    .and_then(|ts| ts.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| format_session_age(d.as_secs_f64()))
                    .unwrap_or_else(|| "-".to_string()),
            };
            crate::output::emit(&detail)?;
            Ok(())
        }
        _ => bail!("Usage: sidekar browser sessions <list|show>"),
    }
}
