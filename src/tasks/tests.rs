use super::*;

/// Run `f` with HOME at a scratch directory, restored and removed afterwards
/// even if `f` panics.
fn with_test_home<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _home = crate::ScratchHome::new();
    f()
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
        let _other_task = insert_task("other", None, 0, crate::scope::PROJECT_SCOPE, Some(&other))?;

        let mut ctx = AppContext::new()?;
        cmd_tasks(&mut ctx, &["list".into()])?;
        let output = ctx.drain_output();
        assert!(output.contains("project"));
        assert!(output.contains("global [global]"));
        assert!(!output.contains("other"));
        Ok(())
    })
}

mod shown_times {
    use super::super::*;

    fn text<T: crate::output::CommandOutput>(v: &T) -> String {
        let mut buf = Vec::new();
        v.render_text(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn show_reads_millisecond_times_as_iso_utc() {
        let out = TaskShowOutput {
            id: 1,
            title: "t".into(),
            scope: "project".into(),
            project: None,
            status: "done".into(),
            priority: 0,
            ready: true,
            created_at: 1_789_357_009_709,
            updated_at: 1_789_357_069_000,
            completed_at: Some(1_789_357_069_000),
            notes: None,
            depends_on: vec![],
            blocks: vec![],
        };
        let shown = text(&out);
        assert!(
            shown.contains("created_at: 2026-09-14T03:36:49Z\n"),
            "{shown}"
        );
        assert!(
            shown.contains("updated_at: 2026-09-14T03:37:49Z\n"),
            "{shown}"
        );
        assert!(
            shown.contains("completed_at: 2026-09-14T03:37:49Z\n"),
            "{shown}"
        );
    }
}
