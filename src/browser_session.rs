use crate::*;

impl AppContext {
    pub fn load_session_state(&self) -> Result<SessionState> {
        let session_id = self.require_session_id()?.to_string();
        let path = self.session_state_file(&session_id);
        // Shared lock for consistent reads (exclusive lock taken by save)
        let lock_path = path.with_extension("lock");
        let _lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&lock_path)
            .ok()
            .and_then(|f| {
                f.lock_shared().ok();
                Some(f)
            });
        let mut state = if path.exists() {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed reading {}", path.display()))?;
            serde_json::from_str::<SessionState>(&content)
                .with_context(|| format!("corrupt browser session state at {}", path.display()))?
        } else if self.override_tab_id.is_some() {
            SessionState::default()
        } else {
            bail!("Unknown browser session: {session_id}. Use `sidekar browser-sessions list`.")
        };

        if state.session_id.is_empty() {
            state.session_id = session_id;
        }
        Ok(state)
    }

    pub fn save_session_state(&self, state: &SessionState) -> Result<()> {
        let session_id = self.require_session_id()?;
        let path = self.session_state_file(session_id);
        // File-level lock to prevent concurrent read-modify-write races
        let lock_path = path.with_extension("lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&lock_path)
            .with_context(|| format!("failed opening lock {}", lock_path.display()))?;
        lock_file.lock_exclusive().ok();
        let result = crate::atomic_write_json(&path, state);
        lock_file.unlock().ok();
        result
    }

    pub fn auto_discover_last_session(&mut self) -> Result<()> {
        let session_file = self.last_session_file();
        let sid = match fs::read_to_string(&session_file) {
            Ok(s) => {
                let trimmed = s.trim().to_string();
                if trimmed.is_empty() {
                    bail!("No active session");
                }
                trimmed
            }
            Err(_) => {
                // Per-agent file doesn't exist. If we're a named agent,
                // do NOT fall back to the generic file — that belongs to
                // another agent and would cause cross-session tab takeover.
                bail!("No active session");
            }
        };
        self.current_session_id = Some(sid);
        self.hydrate_connection_from_state()
    }

    pub fn hydrate_connection_from_state(&mut self) -> Result<()> {
        let state = self.load_session_state()?;
        if let Some(port) = state.port {
            self.cdp_port = port;
        }
        if let Some(host) = state.host {
            self.cdp_host = host;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BrowserSessionInfo {
    pub session_id: String,
    pub active_tab_id: Option<String>,
    pub tabs: Vec<String>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub browser_name: Option<String>,
    pub profile: Option<String>,
    pub window_id: Option<i64>,
    pub state_path: PathBuf,
    pub updated_at: Option<SystemTime>,
}

fn read_browser_session_state(path: &Path) -> Result<BrowserSessionInfo> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed reading {}", path.display()))?;
    let state = serde_json::from_str::<SessionState>(&content)
        .with_context(|| format!("corrupt browser session state at {}", path.display()))?;
    let file_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("state-"))
        .and_then(|name| name.strip_suffix(".json"))
        .unwrap_or_default()
        .to_string();
    let session_id = if state.session_id.is_empty() {
        file_id
    } else {
        state.session_id.clone()
    };
    let updated_at = fs::metadata(path).ok().and_then(|m| m.modified().ok());
    Ok(BrowserSessionInfo {
        session_id,
        active_tab_id: state.active_tab_id,
        tabs: state.tabs,
        port: state.port,
        host: state.host,
        browser_name: state.browser_name,
        profile: state.profile,
        window_id: state.window_id,
        state_path: path.to_path_buf(),
        updated_at,
    })
}

pub fn list_browser_sessions(ctx: &AppContext) -> Result<Vec<BrowserSessionInfo>> {
    let mut sessions = Vec::new();
    let data_dir = ctx.data_dir();
    let entries = match fs::read_dir(&data_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(sessions),
        Err(err) => {
            return Err(err).with_context(|| format!("failed listing {}", data_dir.display()));
        }
    };

    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed reading entry in {}", data_dir.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("state-") || !name.ends_with(".json") {
            continue;
        }
        match read_browser_session_state(&entry.path()) {
            Ok(info) => sessions.push(info),
            Err(err) => {
                wlog!("skipping browser session {}: {err}", entry.path().display());
            }
        }
    }

    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Ok(sessions)
}

pub fn get_browser_session(ctx: &AppContext, session_id: &str) -> Result<BrowserSessionInfo> {
    let path = ctx.session_state_file(session_id);
    if !path.exists() {
        bail!("Unknown browser session: {session_id}. Use `sidekar browser-sessions list`.")
    }
    read_browser_session_state(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_home(test: impl FnOnce(&mut AppContext)) {
        let _guard = test_home_lock().lock().unwrap();
        let old_home = env::var_os("HOME");
        let temp_home =
            env::temp_dir().join(format!("sidekar-browser-test-{}", rand::random::<u32>()));
        fs::create_dir_all(&temp_home).unwrap();
        unsafe {
            env::set_var("HOME", &temp_home);
        }

        let mut ctx = AppContext::new().unwrap();
        test(&mut ctx);

        if let Some(old_home) = old_home {
            unsafe {
                env::set_var("HOME", old_home);
            }
        } else {
            unsafe {
                env::remove_var("HOME");
            }
        }
        let _ = fs::remove_dir_all(&temp_home);
    }

    #[test]
    fn list_browser_sessions_reads_saved_state() {
        with_temp_home(|ctx| {
            ctx.set_current_session("deadbeef".to_string());
            ctx.save_session_state(&SessionState {
                session_id: "deadbeef".to_string(),
                active_tab_id: Some("101".to_string()),
                tabs: vec!["101".to_string(), "202".to_string()],
                port: Some(9222),
                browser_name: Some("Chrome".to_string()),
                profile: Some("testing".to_string()),
                ..SessionState::default()
            })
            .unwrap();

            let sessions = list_browser_sessions(ctx).unwrap();
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].session_id, "deadbeef");
            assert_eq!(sessions[0].active_tab_id.as_deref(), Some("101"));
            assert_eq!(sessions[0].tabs.len(), 2);
            assert_eq!(sessions[0].profile.as_deref(), Some("testing"));
        });
    }

    #[test]
    fn load_session_state_errors_for_unknown_explicit_session() {
        with_temp_home(|ctx| {
            ctx.set_current_session("missing123".to_string());
            let err = ctx.load_session_state().unwrap_err().to_string();
            assert!(err.contains("Unknown browser session: missing123"));
        });
    }

    #[test]
    fn load_session_state_allows_missing_state_in_tab_override_mode() {
        with_temp_home(|ctx| {
            ctx.set_current_session("tab-1234".to_string());
            ctx.override_tab_id = Some("1234".to_string());
            let state = ctx.load_session_state().unwrap();
            assert_eq!(state.session_id, "tab-1234");
        });
    }
}
