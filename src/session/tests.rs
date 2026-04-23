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
fn format_relative_age_picks_coarsest_unit_under_threshold() {
    // Pure function; no HOME guard needed.
    let now = 1_000_000.0;
    // Boundary sanity: just under each threshold rolls into the
    // previous unit, at/over rolls into the next. This catches the
    // off-by-one where we might emit "60s" instead of "1m".
    assert_eq!(super::format_relative_age(now - 0.0, now), "0s");
    assert_eq!(super::format_relative_age(now - 59.0, now), "59s");
    assert_eq!(super::format_relative_age(now - 60.0, now), "1m");
    assert_eq!(super::format_relative_age(now - 3599.0, now), "59m");
    assert_eq!(super::format_relative_age(now - 3600.0, now), "1h");
    assert_eq!(super::format_relative_age(now - 86_399.0, now), "23h");
    assert_eq!(super::format_relative_age(now - 86_400.0, now), "1d");
    assert_eq!(
        super::format_relative_age(now - (6.0 * 86_400.0), now),
        "6d"
    );
    assert_eq!(
        super::format_relative_age(now - (7.0 * 86_400.0), now),
        "1w"
    );
    assert_eq!(
        super::format_relative_age(now - (29.0 * 86_400.0), now),
        "4w"
    );
    assert_eq!(
        super::format_relative_age(now - (30.0 * 86_400.0), now),
        "30d+"
    );
    assert_eq!(
        super::format_relative_age(now - (365.0 * 86_400.0), now),
        "30d+"
    );
    // Future timestamp (clock skew): clamp to "now".
    assert_eq!(super::format_relative_age(now + 5.0, now), "0s");
}

#[test]
fn last_prompt_snippet_extracts_first_text_block_and_truncates() {
    use crate::providers::{ContentBlock, Role};
    let make = |content_json: &str| super::SessionWithCount {
        session: super::Session {
            id: "x".into(),
            cwd: "/".into(),
            model: "m".into(),
            provider: "c".into(),
            name: None,
            created_at: 0.0,
            updated_at: 0.0,
        },
        messages: 1,
        last_user_content_json: Some(content_json.to_string()),
    };
    // Short prompt — no truncation, no ellipsis.
    let sc = make(&serde_json::to_string(&vec![ContentBlock::Text {
        text: "hello".into(),
    }])
    .unwrap());
    assert_eq!(sc.last_prompt_snippet(30).as_deref(), Some("hello"));

    // Long prompt — truncated to N chars with a trailing ellipsis.
    let sc = make(
        &serde_json::to_string(&vec![ContentBlock::Text {
            text: "a".repeat(100),
        }])
        .unwrap(),
    );
    let snip = sc.last_prompt_snippet(30).unwrap();
    assert_eq!(snip.chars().count(), 31, "30 chars + 1 ellipsis");
    assert!(snip.ends_with('…'));

    // Newlines collapse to single spaces so the preview stays on
    // one terminal line.
    let sc = make(
        &serde_json::to_string(&vec![ContentBlock::Text {
            text: "line one\n\nline two".into(),
        }])
        .unwrap(),
    );
    assert_eq!(
        sc.last_prompt_snippet(30).as_deref(),
        Some("line one line two")
    );

    // Multi-block messages (text + tool-result): only the first
    // text block is considered. Tool results are noisy and have
    // nothing title-like.
    let blocks = vec![
        ContentBlock::ToolResult {
            tool_use_id: "id".into(),
            content: "huge dump".into(),
            is_error: false,
        },
        ContentBlock::Text {
            text: "why did that happen?".into(),
        },
    ];
    let sc = make(&serde_json::to_string(&blocks).unwrap());
    assert_eq!(
        sc.last_prompt_snippet(30).as_deref(),
        Some("why did that happen?")
    );

    // No text block at all → None (we don't invent a preview).
    let blocks = vec![ContentBlock::ToolResult {
        tool_use_id: "id".into(),
        content: "dump".into(),
        is_error: false,
    }];
    let sc = make(&serde_json::to_string(&blocks).unwrap());
    assert!(sc.last_prompt_snippet(30).is_none());

    // Unicode safety: truncation boundary must land on a char, not
    // mid-UTF-8. "🦀" is 4 bytes in UTF-8.
    let sc = make(
        &serde_json::to_string(&vec![ContentBlock::Text {
            text: "🦀".repeat(50),
        }])
        .unwrap(),
    );
    let snip = sc.last_prompt_snippet(5).unwrap();
    assert_eq!(snip.chars().count(), 6, "5 crabs + 1 ellipsis");
    assert!(snip.starts_with("🦀🦀🦀🦀🦀"));

    // No user content at all → None (covers current-session-empty
    // path where header line still shows but snippet is omitted).
    let sc = super::SessionWithCount {
        session: super::Session {
            id: "x".into(),
            cwd: "/".into(),
            model: "m".into(),
            provider: "c".into(),
            name: None,
            created_at: 0.0,
            updated_at: 0.0,
        },
        messages: 0,
        last_user_content_json: None,
    };
    assert!(sc.last_prompt_snippet(30).is_none());

    // Silence unused-import warning when Role isn't exercised.
    let _ = Role::User;
}

#[test]
fn list_sessions_with_counts_populates_last_user_content() -> Result<()> {
    use crate::providers::{ChatMessage, ContentBlock, Role};
    with_test_home(|| {
        let cwd = "/repo/prompts";
        let id = create_session(cwd, "m", "c")?;
        let msg = ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "debug the memory leak in session manager".into(),
            }],
        };
        append_message(&id, &msg)?;
        // Also append an assistant message — the query should
        // ignore it and still return the user prompt above.
        let asst = ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "analyzing...".into(),
            }],
        };
        append_message(&id, &asst)?;

        let list = list_sessions_with_counts(cwd, 10, true)?;
        assert_eq!(list.len(), 1);
        let sc = &list[0];
        assert_eq!(sc.messages, 2);
        assert!(sc.last_user_content_json.is_some());
        let snip = sc.last_prompt_snippet(30).unwrap();
        // "debug the memory leak in sessio" = first 30 chars +  "…"
        assert_eq!(snip.chars().count(), 31);
        assert!(snip.starts_with("debug the memory leak"));
        Ok(())
    })
}

#[test]
fn list_sessions_with_counts_last_user_content_is_none_when_only_assistant() -> Result<()> {
    use crate::providers::{ChatMessage, ContentBlock, Role};
    with_test_home(|| {
        // Defensive: if a session somehow has only an assistant
        // message (shouldn't happen in practice but the schema
        // permits it), last_user_content_json must be None so the
        // snippet is skipped rather than showing assistant text.
        let cwd = "/repo/assistant-only";
        let id = create_session(cwd, "m", "c")?;
        let asst = ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        };
        append_message(&id, &asst)?;

        let list = list_sessions_with_counts(cwd, 10, true)?;
        assert_eq!(list.len(), 1);
        assert!(list[0].last_user_content_json.is_none());
        assert!(list[0].last_prompt_snippet(30).is_none());
        Ok(())
    })
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
