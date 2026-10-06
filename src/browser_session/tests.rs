use super::*;

/// Run `test` with HOME at a scratch directory, restored and removed
/// afterwards even if `test` panics.
fn with_temp_home(test: impl FnOnce(&mut AppContext)) {
    let _home = crate::ScratchHome::new();
    let mut ctx = AppContext::new().unwrap();
    test(&mut ctx);
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
