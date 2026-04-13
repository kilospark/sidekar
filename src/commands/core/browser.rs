use super::*;
use crate::output::PlainOutput;

#[derive(serde::Serialize)]
struct LaunchOutput {
    browser: String,
    profile: String,
    headless: bool,
    already_running: bool,
    session_id: String,
    command_file: String,
}

impl crate::output::CommandOutput for LaunchOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.already_running {
            writeln!(w, "Browser already running.")?;
        } else if self.headless {
            writeln!(w, "{} launched successfully (headless).", self.browser)?;
        } else {
            writeln!(w, "{} launched successfully.", self.browser)?;
        }
        if self.profile != "default" {
            writeln!(w, "Profile: {}", self.profile)?;
        }
        writeln!(w, "Session: {}", self.session_id)?;
        writeln!(w, "Command file: {}", self.command_file)?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ConnectOutput {
    session_id: String,
    command_file: String,
}

impl crate::output::CommandOutput for ConnectOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Session: {}", self.session_id)?;
        writeln!(w, "Command file: {}", self.command_file)?;
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct NewTabOutput {
    id: String,
    url: String,
}

impl crate::output::CommandOutput for NewTabOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "New tab: [{}] {}", self.id, self.url)
    }
}

#[derive(serde::Serialize)]
struct CloseTabOutput {
    closed_tab_id: String,
    remaining_tabs: usize,
}

impl crate::output::CommandOutput for CloseTabOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        writeln!(w, "Closed tab {}", self.closed_tab_id)?;
        if self.remaining_tabs == 0 {
            writeln!(w, "No tabs remaining in this session.")?;
        } else {
            writeln!(
                w,
                "No active tab is selected now. Choose one explicitly with: sidekar tab <id>"
            )?;
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
struct ReadUrlsOutput {
    sections: Vec<ReadUrlSection>,
}

#[derive(serde::Serialize)]
struct ReadUrlSection {
    url: String,
    text: String,
}

impl crate::output::CommandOutput for ReadUrlsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        for (i, s) in self.sections.iter().enumerate() {
            if i > 0 {
                writeln!(w)?;
            }
            writeln!(w, "--- {} ---", s.url)?;
            writeln!(w, "{}", s.text)?;
        }
        Ok(())
    }
}

pub(crate) async fn cmd_launch(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let preferred_browser = args
        .windows(2)
        .find_map(|pair| {
            if pair[0] == "--browser" {
                Some(pair[1].clone())
            } else {
                None
            }
        })
        .or_else(|| crate::config::load_config().browser);

    let headless = args.iter().any(|a| a == "--headless");

    let profile = args
        .windows(2)
        .find_map(|pair| {
            if pair[0] == "--profile" {
                Some(pair[1].clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "default".to_string());

    let profile = if profile == "new" {
        format!("sidekar-{:08x}", rand::random::<u32>())
    } else {
        profile
    };

    let profile = if headless {
        format!("{profile}.headless")
    } else {
        profile
    };

    ctx.current_profile = profile.clone();

    let user_data_dir = ctx.chrome_profile_dir_for(&profile);
    let port_file = ctx.chrome_port_file_for(&profile);

    if let Ok(saved) = fs::read_to_string(&port_file) {
        if let Ok(saved_port) = saved.trim().parse::<u16>() {
            ctx.cdp_port = saved_port;
            if get_debug_tabs(ctx).await.is_ok() {
                if profile == "default"
                    && let Some(ref wanted) = preferred_browser
                {
                    let running = detect_browser_from_port(ctx).await.unwrap_or_default();
                    if !running.to_lowercase().contains(&wanted.to_lowercase()) {
                        bail!(
                            "Default browser already running ({running}). Use --profile <name> to launch a separate {wanted} instance."
                        );
                    }
                }
                ctx.headless = headless;
                ctx.launch_browser_name = detect_browser_from_port(ctx).await;
                let (_has_own, session_id) = connect_inner(ctx).await?;
                let browser_label = ctx.launch_browser_name.clone().unwrap_or_default();
                let output = LaunchOutput {
                    browser: browser_label,
                    profile: profile.clone(),
                    headless,
                    already_running: true,
                    session_id: session_id.clone(),
                    command_file: ctx.command_file(&session_id).display().to_string(),
                };
                out!(ctx, "{}", crate::output::to_string(&output)?);
                return Ok(());
            }
        }
        if let Err(e) = fs::remove_file(&port_file) {
            wlog!("failed removing stale port file: {e}");
        }
    }

    if let Some(port) = env::var("CDP_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        ctx.cdp_port = port;
    } else {
        ctx.cdp_port = find_free_port()?;
    }

    let browser = if let Some(ref name) = preferred_browser {
        find_browser_by_name(name).ok_or_else(|| {
            anyhow!(
                "Browser '{name}' not found. Available: chrome, edge, brave, arc, vivaldi, chromium"
            )
        })?
    } else {
        find_browser().ok_or_else(|| {
            anyhow!(
                "No Chromium-based browser found. Install Chrome/Edge/Brave/Chromium or set CHROME_PATH."
            )
        })?
    };
    ctx.launch_browser_name = Some(browser.name.clone());

    fs::create_dir_all(&user_data_dir)
        .with_context(|| format!("failed creating {}", user_data_dir.display()))?;

    let mut chrome_args = vec![
        format!("--remote-debugging-port={}", ctx.cdp_port),
        format!("--user-data-dir={}", user_data_dir.to_string_lossy()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-blink-features=AutomationControlled".to_string(),
        "--test-type".to_string(),
    ];
    if headless {
        chrome_args.push("--headless=new".to_string());
        ctx.headless = true;
    }

    #[cfg(target_os = "macos")]
    let use_open_gn = !headless && browser.path.contains(".app/Contents/MacOS/");
    #[cfg(not(target_os = "macos"))]
    let use_open_gn = false;

    let mut command = if use_open_gn {
        let app_bundle = browser
            .path
            .split(".app/Contents/MacOS/")
            .next()
            .unwrap()
            .to_string()
            + ".app";
        let mut cmd = Command::new("open");
        cmd.arg("-g");
        cmd.arg("-n");
        cmd.arg("-a");
        cmd.arg(&app_bundle);
        cmd.arg("--args");
        for a in &chrome_args {
            cmd.arg(a);
        }
        cmd
    } else {
        let mut cmd = Command::new(&browser.path);
        for a in &chrome_args {
            cmd.arg(a);
        }
        cmd
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if !use_open_gn {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
    }

    let _child = command
        .spawn()
        .with_context(|| format!("failed launching browser at {}", browser.path))?;

    let mut ready = false;
    let mut last_err = String::new();
    for _ in 0..60 {
        sleep(Duration::from_millis(500)).await;
        match get_debug_tabs(ctx).await {
            Ok(tabs) if !tabs.is_empty() => match verify_cdp_ready(ctx).await {
                Ok(()) => {
                    ready = true;
                    break;
                }
                Err(e) => {
                    last_err = format!("WS check failed: {e:#}");
                }
            },
            Ok(_) => {
                last_err = "HTTP up but no tabs yet".to_string();
            }
            Err(e) => {
                last_err = format!("{e:#}");
            }
        }
    }
    if !ready {
        bail!(
            "{} launched but debug port not responding after 30s. Last error: {}",
            browser.name,
            last_err
        );
    }

    if let Ok(tabs) = get_debug_tabs(ctx).await
        && let Some(tab) = tabs.first()
        && let Some(ref ws_url) = tab.web_socket_debugger_url
        && let Ok(mut cdp) = DirectCdp::connect(ws_url).await
    {
        let _ = cdp
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({
                    "source": "Object.defineProperty(navigator, 'webdriver', { get: () => false });"
                }),
            )
            .await;
        cdp.close().await;
    }

    let initial_tabs: Vec<String> = get_debug_tabs(ctx)
        .await
        .map(|tabs| tabs.iter().map(|t| t.id.clone()).collect())
        .unwrap_or_default();

    fs::write(&port_file, ctx.cdp_port.to_string())
        .with_context(|| format!("failed writing {}", port_file.display()))?;

    let (has_own_window, session_id) = connect_inner(ctx).await?;
    let output = LaunchOutput {
        browser: browser.name.clone(),
        profile: profile.clone(),
        headless,
        already_running: false,
        session_id: session_id.clone(),
        command_file: ctx.command_file(&session_id).display().to_string(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);

    if has_own_window && !ctx.headless {
        for tab_id in &initial_tabs {
            if let Err(e) = http_put_text(ctx, &format!("/json/close/{tab_id}")).await {
                wlog!("failed closing initial tab {tab_id}: {e}");
            }
        }
    }

    Ok(())
}

pub(crate) async fn cmd_connect(ctx: &mut AppContext) -> Result<bool> {
    let (has_own_window, session_id) = connect_inner(ctx).await?;
    let output = ConnectOutput {
        session_id: session_id.clone(),
        command_file: ctx.command_file(&session_id).display().to_string(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(has_own_window)
}

async fn connect_inner(ctx: &mut AppContext) -> Result<(bool, String)> {
    let session_id = new_session_id();
    ctx.set_current_session(session_id.clone());

    let (new_tab, has_own_window) = if ctx.isolated {
        match create_new_window(ctx, None).await {
            Ok(tab) => (tab, true),
            Err(e) => {
                crate::broker::try_log_event(
                    "warn",
                    "browser",
                    "new window failed, falling back to tab",
                    Some(&format!("{e:#}")),
                );
                (create_new_tab(ctx, None).await?, false)
            }
        }
    } else {
        (create_new_tab(ctx, None).await?, false)
    };

    if let Some(ref ws_url) = new_tab.web_socket_debugger_url
        && let Ok(mut cdp) = DirectCdp::connect(ws_url).await
    {
        let _ = cdp
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({
                    "source": "Object.defineProperty(navigator, 'webdriver', { get: () => false });"
                }),
            )
            .await;
        cdp.close().await;
    }

    let window_id = if has_own_window {
        if let Some(ws_url) = &new_tab.web_socket_debugger_url {
            get_window_id_for_target(ctx, ws_url).await.ok()
        } else {
            None
        }
    } else {
        None
    };

    if ctx.launch_browser_name.is_none() {
        ctx.launch_browser_name = detect_browser_from_port(ctx).await;
    }

    let state = SessionState {
        session_id: session_id.clone(),
        active_tab_id: Some(new_tab.id.clone()),
        tabs: vec![new_tab.id.clone()],
        port: Some(ctx.cdp_port),
        host: Some(ctx.cdp_host.clone()),
        browser_name: ctx.launch_browser_name.clone(),
        window_id,
        profile: if ctx.current_profile != "default" {
            Some(ctx.current_profile.clone())
        } else {
            None
        },
        ..SessionState::default()
    };
    ctx.save_session_state(&state)?;
    fs::write(ctx.last_session_file(), &session_id)
        .context("failed writing last session pointer")?;

    Ok((has_own_window, session_id))
}

pub(crate) async fn cmd_kill(ctx: &mut AppContext) -> Result<()> {
    let state = ctx.load_session_state()?;
    let profile = state
        .profile
        .clone()
        .unwrap_or_else(|| "default".to_string());

    if profile == "default" {
        bail!("Cannot kill default profile. Use 'close' to close your tabs.");
    }

    let port_file = ctx.chrome_port_file_for(&profile);
    if let Ok(port_str) = fs::read_to_string(&port_file)
        && let Ok(port) = port_str.trim().parse::<u16>()
    {
        let old_port = ctx.cdp_port;
        ctx.cdp_port = port;
        if let Ok(mut cdp) = open_cdp(ctx).await {
            let _ = cdp.send("Browser.close", json!({})).await;
        }
        ctx.cdp_port = old_port;
    }

    let _ = fs::remove_file(&port_file);
    let profile_dir = ctx.chrome_profile_dir_for(&profile);
    let _ = fs::remove_dir_all(&profile_dir);

    let session_id = ctx.require_session_id()?.to_string();
    let _ = fs::remove_file(ctx.session_state_file(&session_id));

    let msg = format!("Killed profile '{profile}' and cleaned up.");
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    Ok(())
}

#[derive(serde::Serialize)]
struct TabOut {
    id: String,
    title: Option<String>,
    url: Option<String>,
    active: bool,
}

#[derive(serde::Serialize)]
struct TabsOutput {
    items: Vec<TabOut>,
}

impl crate::output::CommandOutput for TabsOutput {
    fn render_text(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        if self.items.is_empty() {
            writeln!(w, "No tabs owned by this session.")?;
            return Ok(());
        }
        for tab in &self.items {
            let active = if tab.active { " *" } else { "" };
            writeln!(
                w,
                "[{}] {} - {}{}",
                tab.id,
                tab.title.clone().unwrap_or_else(|| "(untitled)".to_string()),
                tab.url.clone().unwrap_or_else(|| "(no url)".to_string()),
                active
            )?;
        }
        Ok(())
    }
}

pub(crate) async fn cmd_tabs(ctx: &mut AppContext, _args: &[String]) -> Result<()> {
    let all_tabs = get_debug_tabs(ctx).await?;
    let state = ctx.load_session_state()?;
    let owned_ids = state
        .tabs
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let output = TabsOutput {
        items: all_tabs
            .into_iter()
            .filter(|t| owned_ids.contains(&t.id))
            .map(|t| {
                let active = state.active_tab_id.as_deref() == Some(t.id.as_str());
                TabOut {
                    id: t.id,
                    title: t.title,
                    url: t.url,
                    active,
                }
            })
            .collect(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_tab(ctx: &mut AppContext, tab_id: &str) -> Result<()> {
    let mut state = ctx.load_session_state()?;
    if !state.tabs.iter().any(|id| id == tab_id) {
        bail!("Tab {tab_id} is not owned by this session.");
    }

    let all_tabs = get_debug_tabs(ctx).await?;
    let tab = all_tabs
        .iter()
        .find(|t| t.id == tab_id)
        .cloned()
        .ok_or_else(|| anyhow!("Tab {tab_id} not found in Chrome"))?;

    state.active_tab_id = Some(tab_id.to_string());
    ctx.save_session_state(&state)?;

    if !ctx.isolated {
        let _ = http_put_text(ctx, &format!("/json/activate/{tab_id}")).await;
    }
    let msg = format!(
        "Switched to tab: {}",
        tab.title
            .or(tab.url)
            .unwrap_or_else(|| "(untitled)".to_string())
    );
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(msg))?);
    Ok(())
}

pub(crate) async fn cmd_new_tab(ctx: &mut AppContext, url: Option<&str>) -> Result<()> {
    let mut state = ctx.load_session_state()?;
    if let Ok(live_tabs) = get_debug_tabs(ctx).await {
        let live_ids: HashSet<String> = live_tabs.iter().map(|t| t.id.clone()).collect();
        state.tabs.retain(|id| live_ids.contains(id));
    }
    let max_tabs = crate::config::load_config().max_tabs;
    if state.tabs.len() >= max_tabs {
        bail!("Tab limit reached ({max_tabs}). Close a tab first, or increase max_tabs in config.");
    }
    let new_tab = create_new_tab(ctx, url).await?;

    if let Some(ref ws_url) = new_tab.web_socket_debugger_url
        && let Ok(mut cdp) = DirectCdp::connect(ws_url).await
    {
        let _ = cdp
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({
                    "source": "Object.defineProperty(navigator, 'webdriver', { get: () => false });"
                }),
            )
            .await;
        cdp.close().await;
    }

    state.tabs.push(new_tab.id.clone());
    state.active_tab_id = Some(new_tab.id.clone());
    ctx.save_session_state(&state)?;

    let output = NewTabOutput {
        id: new_tab.id,
        url: new_tab.url.unwrap_or_else(|| "about:blank".to_string()),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_close(ctx: &mut AppContext) -> Result<()> {
    let mut state = ctx.load_session_state()?;
    let tab_id = state
        .active_tab_id
        .clone()
        .ok_or_else(|| anyhow!("No active tab"))?;

    http_put_text(ctx, &format!("/json/close/{tab_id}")).await?;
    state.tabs.retain(|id| id != &tab_id);
    state.active_tab_id = None;
    ctx.save_session_state(&state)?;

    let output = CloseTabOutput {
        closed_tab_id: tab_id,
        remaining_tabs: state.tabs.len(),
    };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}

pub(crate) async fn cmd_back(ctx: &mut AppContext) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let nav = cdp.send("Page.getNavigationHistory", json!({})).await?;

    let current_index = nav
        .get("currentIndex")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Invalid navigation history"))?;
    let entries = nav
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Invalid navigation history entries"))?;

    if current_index <= 0 {
        bail!("No previous page in history.");
    }

    let entry_id = entries[(current_index - 1) as usize]
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Missing history entry id"))?;

    cdp.send(
        "Page.navigateToHistoryEntry",
        json!({ "entryId": entry_id }),
    )
    .await?;
    sleep(Duration::from_millis(500)).await;
    let brief = get_page_brief(&mut cdp).await?;
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(brief))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_forward(ctx: &mut AppContext) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    let nav = cdp.send("Page.getNavigationHistory", json!({})).await?;

    let current_index = nav
        .get("currentIndex")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Invalid navigation history"))?;
    let entries = nav
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Invalid navigation history entries"))?;

    if current_index >= entries.len() as i64 - 1 {
        bail!("No next page in history.");
    }

    let entry_id = entries[(current_index + 1) as usize]
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Missing history entry id"))?;

    cdp.send(
        "Page.navigateToHistoryEntry",
        json!({ "entryId": entry_id }),
    )
    .await?;
    sleep(Duration::from_millis(500)).await;
    let brief = get_page_brief(&mut cdp).await?;
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(brief))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_reload(ctx: &mut AppContext) -> Result<()> {
    let mut cdp = open_cdp(ctx).await?;
    prepare_cdp(ctx, &mut cdp).await?;
    cdp.send("Page.reload", json!({})).await?;
    wait_for_ready_state_complete(&mut cdp, Duration::from_secs(15)).await?;
    let brief = get_page_brief(&mut cdp).await?;
    out!(ctx, "{}", crate::output::to_string(&PlainOutput::new(brief))?);
    cdp.close().await;
    Ok(())
}

pub(crate) async fn cmd_search(
    ctx: &mut AppContext,
    query: &str,
    engine: Option<&str>,
    max_tokens: usize,
) -> Result<()> {
    let encoded: String = query
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect();

    let search_url = match engine.unwrap_or("google") {
        "google" => format!("https://www.google.com/search?q={encoded}"),
        "bing" => format!("https://www.bing.com/search?q={encoded}"),
        "duckduckgo" | "ddg" => format!("https://duckduckgo.com/?q={encoded}"),
        custom if custom.starts_with("http") => format!("{custom}{encoded}"),
        other => bail!("Unknown engine: {other}. Use google, bing, duckduckgo, or a URL."),
    };

    cmd_navigate(ctx, &search_url, true).await?;
    ctx.output.clear();

    cmd_read(ctx, None, if max_tokens > 0 { max_tokens } else { 4000 }).await
}

pub(crate) async fn cmd_readurls(
    ctx: &mut AppContext,
    urls: &[String],
    max_tokens: usize,
) -> Result<()> {
    let effective_max = if max_tokens > 0 { max_tokens } else { 2000 };

    let mut tab_ids: Vec<String> = Vec::new();
    for url in urls {
        let tab = create_new_tab(ctx, Some(url.as_str())).await?;
        let mut state = ctx.load_session_state()?;
        state.tabs.push(tab.id.clone());
        ctx.save_session_state(&state)?;
        tab_ids.push(tab.id.clone());
    }

    sleep(Duration::from_secs(3)).await;

    let original_state = ctx.load_session_state()?;
    let original_tab = original_state.active_tab_id.clone();

    let mut sections: Vec<ReadUrlSection> = Vec::new();
    for (i, tab_id) in tab_ids.iter().enumerate() {
        let mut state = ctx.load_session_state()?;
        state.active_tab_id = Some(tab_id.clone());
        ctx.save_session_state(&state)?;

        let mut cdp = open_cdp(ctx).await?;
        prepare_cdp(ctx, &mut cdp).await?;
        let _ = wait_for_ready_state_complete(&mut cdp, Duration::from_secs(10)).await;

        let script = build_read_extract_script(None)?;
        let context_id = get_frame_context_id(ctx, &mut cdp).await?;
        let result =
            runtime_evaluate_with_context(&mut cdp, &script, true, false, context_id).await?;
        let mut output = result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let char_budget = effective_max.saturating_mul(4);
        if output.len() > char_budget {
            let boundary = output.floor_char_boundary(char_budget);
            output = format!("{}\n... (truncated)", &output[..boundary]);
        }

        sections.push(ReadUrlSection {
            url: urls[i].clone(),
            text: output,
        });
        cdp.close().await;
    }

    for tab_id in &tab_ids {
        let _ = http_put_text(ctx, &format!("/json/close/{tab_id}")).await;
        let mut state = ctx.load_session_state()?;
        state.tabs.retain(|id| id != tab_id);
        ctx.save_session_state(&state)?;
    }

    if let Some(orig) = original_tab {
        let mut state = ctx.load_session_state()?;
        state.active_tab_id = Some(orig);
        ctx.save_session_state(&state)?;
    }

    let output = ReadUrlsOutput { sections };
    out!(ctx, "{}", crate::output::to_string(&output)?);
    Ok(())
}
