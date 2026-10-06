use super::*;

#[test]
fn each_profile_has_its_own_last_session_pointer() {
    let _home = crate::ScratchHome::new();
    let mut ctx = AppContext::new().unwrap();
    let default = ctx.last_session_file();
    assert_eq!(default, ctx.sticky_session_file(), "default's pointer is the sticky one");

    ctx.current_profile = "work".to_string();
    let work = ctx.last_session_file();
    assert_ne!(work, default, "work must not reuse default's session");
    assert!(
        work.file_name().unwrap().to_string_lossy().ends_with("-profile-work"),
        "{work:?}"
    );

    ctx.current_profile = "work.headless".to_string();
    assert_eq!(ctx.last_session_file(), work, "a headless run shares its profile's pointer");

    ctx.current_profile = "default".to_string();
    assert_eq!(
        ctx.last_session_file(),
        default,
        "default keeps the original file, so existing sessions carry over"
    );
}

/// Save a session of `profile` and point `pointer` at it.
fn session(ctx: &mut AppContext, id: &str, profile: &str, pointer: &std::path::Path) {
    ctx.set_current_session(id.to_string());
    ctx.save_session_state(&SessionState {
        session_id: id.to_string(),
        profile: (profile != "default").then(|| profile.to_string()),
        port: Some(9222),
        ..SessionState::default()
    })
    .unwrap();
    fs::write(pointer, id).unwrap();
    ctx.clear_current_session();
}

#[test]
fn a_plain_command_still_follows_the_session_used_last_whatever_its_profile() {
    // An agent that launched `--profile muse` and then runs plain commands
    // keeps driving muse, as before pointers were per profile.
    let _home = crate::ScratchHome::new();
    let mut ctx = AppContext::new().unwrap();
    let sticky = ctx.sticky_session_file();
    session(&mut ctx, "s-muse", "muse", &sticky);

    assert!(ctx.auto_discover_last_session().is_ok());
    assert_eq!(ctx.current_session_id.as_deref(), Some("s-muse"));
}

#[test]
fn naming_a_profile_never_reuses_another_profiles_session() {
    let _home = crate::ScratchHome::new();
    let mut ctx = AppContext::new().unwrap();
    let sticky = ctx.sticky_session_file();
    session(&mut ctx, "s-muse", "muse", &sticky);

    // `--profile default`: the sticky pointer names muse's session.
    ctx.profile_explicit = true;
    assert!(ctx.auto_discover_last_session().is_err());
    assert_eq!(ctx.current_session_id, None);

    // `--profile work`, its pointer somehow naming chennai's session.
    ctx.current_profile = "work".to_string();
    let work_pointer = ctx.last_session_file();
    session(&mut ctx, "s-chennai", "chennai", &work_pointer);
    assert!(ctx.auto_discover_last_session().is_err());
}

#[test]
fn naming_a_profile_reuses_its_own_session_headless_or_not() {
    let _home = crate::ScratchHome::new();
    let mut ctx = AppContext::new().unwrap();
    ctx.profile_explicit = true;
    ctx.current_profile = "work".to_string();
    let pointer = ctx.last_session_file();

    session(&mut ctx, "s-work", "work", &pointer);
    assert!(ctx.auto_discover_last_session().is_ok());

    session(&mut ctx, "s-work-headless", "work.headless", &pointer);
    assert!(ctx.auto_discover_last_session().is_ok());
    assert_eq!(ctx.current_session_id.as_deref(), Some("s-work-headless"));
}
