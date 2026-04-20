use crate::*;

/// Sanitize a string for use in filenames (replace /, \, : with -; collapse -- to -).
pub fn sanitize_for_filename(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    let mut result = String::with_capacity(replaced.len());
    for c in replaced.chars() {
        if c == '-' && result.ends_with('-') {
            continue;
        }
        result.push(c);
    }
    result
}

pub struct AppContext {
    pub current_session_id: Option<String>,
    pub cdp_port: u16,
    pub cdp_host: String,
    pub launch_browser_name: Option<String>,
    pub http: Client,
    pub output: String,
    pub session_id: String,
    pub tool_counts: std::collections::HashMap<String, u64>,
    pub session_start: std::time::Instant,
    pub isolated: bool,
    pub current_profile: String,
    /// Override active tab — connects directly to this tab ID, bypassing session ownership.
    pub override_tab_id: Option<String>,
    /// Browser launched in headless mode — skip window management operations.
    pub headless: bool,
    /// Agent identity when running inside a PTY wrapper or equivalent isolated context.
    pub agent_name: Option<String>,
}

impl AppContext {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("failed to initialize HTTP client")?;
        let ctx = Self {
            current_session_id: None,
            cdp_port: DEFAULT_CDP_PORT,
            cdp_host: DEFAULT_CDP_HOST.to_string(),
            launch_browser_name: None,
            http,
            output: String::new(),
            session_id: {
                let mut bytes = [0u8; 16];
                rand::rng().fill_bytes(&mut bytes);
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            },
            tool_counts: std::collections::HashMap::new(),
            session_start: std::time::Instant::now(),
            isolated: false,
            current_profile: "default".to_string(),
            override_tab_id: None,
            headless: false,
            agent_name: crate::runtime::agent_name(),
        };
        if let Err(e) = fs::create_dir_all(ctx.data_dir()) {
            wlog!("failed creating data dir: {e}");
        }
        if let Err(e) = fs::create_dir_all(ctx.chrome_profile_dir()) {
            wlog!("failed creating profile dir: {e}");
        }
        Ok(ctx)
    }

    pub fn drain_output(&mut self) -> String {
        let raw = std::mem::take(&mut self.output);
        if crate::runtime::color() {
            raw
        } else {
            crate::runtime::strip_ansi(&raw)
        }
    }

    pub fn data_dir(&self) -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".sidekar")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        env::temp_dir()
    }

    pub fn last_session_file(&self) -> PathBuf {
        if let Some(agent_name) = self.agent_name.as_deref() {
            let safe_name = sanitize_for_filename(agent_name);
            return self.data_dir().join(format!("last-session-{safe_name}"));
        }
        self.data_dir().join("last-session")
    }

    pub fn is_named_agent(&self) -> bool {
        self.agent_name.is_some()
    }

    pub fn session_state_file(&self, session_id: &str) -> PathBuf {
        self.data_dir().join(format!("state-{session_id}.json"))
    }

    pub fn command_file(&self, session_id: &str) -> PathBuf {
        self.tmp_dir()
            .join(format!("sidekar-command-{session_id}.json"))
    }

    pub fn chrome_profile_dir(&self) -> PathBuf {
        self.data_dir().join("profiles").join("default")
    }

    pub fn chrome_port_file(&self) -> PathBuf {
        self.data_dir().join("chrome-port")
    }

    pub fn chrome_profile_dir_for(&self, profile: &str) -> PathBuf {
        self.data_dir().join("profiles").join(profile)
    }

    pub fn chrome_port_file_for(&self, profile: &str) -> PathBuf {
        self.chrome_profile_dir_for(profile).join("cdp-port")
    }

    pub fn action_cache_file(&self) -> PathBuf {
        self.data_dir().join("action-cache.json")
    }

    pub fn tab_locks_file(&self) -> PathBuf {
        self.data_dir().join("tab-locks.json")
    }

    pub fn default_download_dir(&self) -> PathBuf {
        self.data_dir().join("downloads")
    }

    pub fn network_log_file(&self) -> PathBuf {
        let sid = self
            .current_session_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        self.tmp_dir().join(format!("sidekar-network-{sid}.json"))
    }

    pub fn require_session_id(&self) -> Result<&str> {
        self.current_session_id
            .as_deref()
            .ok_or_else(|| anyhow!("No active session"))
    }

    pub fn set_current_session(&mut self, session_id: String) {
        self.current_session_id = Some(session_id);
    }

    pub fn clear_current_session(&mut self) {
        self.current_session_id = None;
    }
}

/// Atomic JSON write: serialize to temp file, then rename into place.
/// Prevents corruption from crashes mid-write and partial reads by other processes.
pub(crate) fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension(format!(
        "tmp.{}.{:08x}",
        std::process::id(),
        rand::random::<u32>()
    ));
    let data = serde_json::to_string_pretty(value).context("failed serializing JSON")?;
    fs::write(&tmp, &data).with_context(|| format!("failed writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed renaming {} → {}", tmp.display(), path.display()))?;
    Ok(())
}
