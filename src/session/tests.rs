use super::*;
use std::{env, fs};

fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = crate::test_home_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("failed to lock test HOME mutex"))?;

    let old_home = env::var_os("HOME");
    let temp_home = env::temp_dir().join(format!("sidekar-session-test-{}", generate_id()));
    fs::create_dir_all(&temp_home)?;

    // Safety: tests run under a process-global mutex and restore HOME before returning.
    unsafe { env::set_var("HOME", &temp_home) };

    let result = f();

    match old_home {
        Some(home) => unsafe { env::set_var("HOME", home) },
        None => unsafe { env::remove_var("HOME") },
    }
    let _ = fs::remove_dir_all(&temp_home);
    result
}

#[test]
fn list_sessions_with_counts_filters_empty_when_requested() -> Result<()> {
    with_test_home(|| {
        // Three sessions in /repo/x: two empty, one with messages.
        // The filter must drop both empties, leaving only the
        // populated session. Without the filter, all three show.
        let cwd = "/repo/x";
        let empty1 = create_session(cwd, "m", "c")?;
        let populated = create_session(cwd, "m", "c")?;
        let empty2 = create_session(cwd, "m", "c")?;
        // Add a real message to `populated` only.
        let msg = crate::providers::ChatMessage {
            role: crate::providers::Role::User,
            content: vec![crate::providers::ContentBlock::Text {
                text: "hello".to_string(),
            }],
        };
        append_message(&populated, &msg)?;

        let all = list_sessions_with_counts(cwd, 20, false)?;
        assert_eq!(all.len(), 3, "unfiltered list returns all sessions");
        let filtered = list_sessions_with_counts(cwd, 20, true)?;
        assert_eq!(filtered.len(), 1, "filtered list hides empty sessions");
        assert_eq!(filtered[0].session.id, populated);
        assert_eq!(filtered[0].messages, 1);

        // Unused helpers silence unused-binding warnings and make the
        // intent explicit: these ids exist but should be filtered.
        assert_ne!(empty1, populated);
        assert_ne!(empty2, populated);
        Ok(())
    })
}

#[test]
fn list_sessions_with_counts_respects_cwd_scoping() -> Result<()> {
    with_test_home(|| {
        // A populated session in /repo/a must not appear when we list
        // /repo/b (the cwd column scopes the result set).
        let a = create_session("/repo/a", "m", "c")?;
        let msg = crate::providers::ChatMessage {
            role: crate::providers::Role::User,
            content: vec![crate::providers::ContentBlock::Text {
                text: "hi".into(),
            }],
        };
        append_message(&a, &msg)?;

        let list_b = list_sessions_with_counts("/repo/b", 20, true)?;
        assert!(list_b.is_empty(), "cwd /repo/b should see no sessions");

        let list_a = list_sessions_with_counts("/repo/a", 20, true)?;
        assert_eq!(list_a.len(), 1);
        assert_eq!(list_a[0].session.id, a);
        Ok(())
    })
}

#[test]
fn repl_input_history_is_scoped_deduped_and_bounded() -> Result<()> {
    with_test_home(|| {
        append_input_history("/repo/a", "a", "first", 3)?;
        append_input_history("/repo/a", "a", "first", 3)?;
        append_input_history("/repo/a", "a", "second", 3)?;
        append_input_history("/repo/b", "b", "other", 3)?;
        append_input_history("/repo/a", "a", "third", 3)?;
        append_input_history("/repo/a", "a", "fourth", 3)?;

        assert_eq!(
            load_input_history("/repo/a", 10)?,
            vec![
                "second".to_string(),
                "third".to_string(),
                "fourth".to_string()
            ]
        );
        assert_eq!(
            load_input_history("/repo/b", 10)?,
            vec!["other".to_string()]
        );
        Ok(())
    })
}
