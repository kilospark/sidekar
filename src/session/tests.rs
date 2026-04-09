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
