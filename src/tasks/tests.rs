use super::*;

fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = crate::test_home_lock()
        .lock()
        .map_err(|_| anyhow!("failed to lock test HOME mutex"))?;
    let old_home = env::var_os("HOME");
    let temp_home = env::temp_dir().join(format!("sidekar-tasks-test-{}", now_epoch_ms()));
    fs::create_dir_all(&temp_home)?;
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
fn prevents_cycles() -> Result<()> {
    with_test_home(|| {
        let project = crate::scope::resolve_project_name(None);
        let a = insert_task("A", None, 0, crate::scope::PROJECT_SCOPE, Some(&project))?;
        let b = insert_task("B", None, 0, crate::scope::PROJECT_SCOPE, Some(&project))?;
        add_dependency(a, b)?;
        let err = add_dependency(b, a).expect_err("cycle should fail");
        assert!(err.to_string().contains("cycle"));
        Ok(())
    })
}

#[test]
fn ready_list_hides_blocked_tasks() -> Result<()> {
    with_test_home(|| {
        let project = crate::scope::resolve_project_name(None);
        let a = insert_task("A", None, 0, crate::scope::PROJECT_SCOPE, Some(&project))?;
        let b = insert_task("B", None, 0, crate::scope::PROJECT_SCOPE, Some(&project))?;
        add_dependency(b, a)?;

        let mut ctx = AppContext::new()?;
        cmd_tasks(&mut ctx, &["list".into(), "--ready".into()])?;
        let output = ctx.drain_output();
        assert!(output.contains("[1]"));
        assert!(!output.contains("[2]"));

        update_task_status(a, "done")?;
        let mut ctx = AppContext::new()?;
        cmd_tasks(&mut ctx, &["list".into(), "--ready".into()])?;
        let output = ctx.drain_output();
        assert!(output.contains("[2]"));
        Ok(())
    })
}

#[test]
fn project_list_includes_global_tasks_but_not_other_projects() -> Result<()> {
    with_test_home(|| {
        let current = crate::scope::resolve_project_name(None);
        let other = "other-project".to_string();
        let _project_task = insert_task(
            "project",
            None,
            0,
            crate::scope::PROJECT_SCOPE,
            Some(&current),
        )?;
        let _global_task = insert_task("global", None, 0, crate::scope::GLOBAL_SCOPE, None)?;
        let _other_task =
            insert_task("other", None, 0, crate::scope::PROJECT_SCOPE, Some(&other))?;

        let mut ctx = AppContext::new()?;
        cmd_tasks(&mut ctx, &["list".into()])?;
        let output = ctx.drain_output();
        assert!(output.contains("project"));
        assert!(output.contains("global [global]"));
        assert!(!output.contains("other"));
        Ok(())
    })
}
