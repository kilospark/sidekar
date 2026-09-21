use super::*;

#[test]
fn parse_duration_supports_common_suffixes() {
    assert_eq!(parse_duration("30s").unwrap(), 30);
    assert_eq!(parse_duration("5m").unwrap(), 300);
    assert_eq!(parse_duration("2h").unwrap(), 7200);
    assert_eq!(parse_duration("14d").unwrap(), 14 * 86400);
    assert_eq!(parse_duration("2w").unwrap(), 2 * 604800);
    assert_eq!(parse_duration("90").unwrap(), 90);
    assert!(parse_duration("").is_err());
    assert!(parse_duration("abc").is_err());
}

#[test]
fn parse_args_defaults_are_sane() {
    let opts = parse_args(&[]).unwrap();
    assert_eq!(opts.scope_filter, ScopeFilter::All);
    assert_eq!(opts.max_sessions, 5);
    assert!(!opts.dry_run);
    assert!(!opts.no_llm);
    assert!(!opts.assume_yes);
}

#[test]
fn parse_args_source_validation() {
    let err = parse_args(&["--source=bogus".to_string()]).unwrap_err();
    assert!(err.contains("Unknown source"));
}

#[test]
fn parse_args_scope_validation() {
    assert!(parse_args(&["--scope=weird".to_string()]).is_err());
    assert_eq!(
        parse_args(&["--scope=project".to_string()])
            .unwrap()
            .scope_filter,
        ScopeFilter::Project
    );
}

#[test]
fn parse_args_handles_multiple_sources() {
    let opts = parse_args(&["--source=manifests,claude".to_string()]).unwrap();
    assert_eq!(opts.sources, vec!["manifests", "claude"]);
}

#[test]
fn default_model_picks_cheap_for_known_providers() {
    assert!(default_model_for_provider("anthropic").contains("haiku"));
    assert!(default_model_for_provider("codex").contains("mini"));
    assert!(default_model_for_provider("gemini").contains("flash"));
}

#[test]
fn resolve_project_for_path_global_shortcircuits() {
    let opts = ImportOptions {
        sources: vec![],
        project_override: Some("override".into()),
        scope_filter: ScopeFilter::All,
        since_secs: None,
        max_sessions: 5,
        no_llm: true,
        credential: None,
        model: None,
        dry_run: true,
        assume_yes: false,
        verbose: false,
    };
    assert_eq!(
        resolve_project_for_path(Path::new("/tmp/x"), &opts, crate::scope::GLOBAL_SCOPE),
        "global"
    );
}

#[test]
fn resolve_project_for_path_honors_override() {
    let opts = ImportOptions {
        sources: vec![],
        project_override: Some("override".into()),
        scope_filter: ScopeFilter::All,
        since_secs: None,
        max_sessions: 5,
        no_llm: true,
        credential: None,
        model: None,
        dry_run: true,
        assume_yes: false,
        verbose: false,
    };
    assert_eq!(
        resolve_project_for_path(Path::new("/tmp/x"), &opts, crate::scope::PROJECT_SCOPE),
        "override"
    );
}

// ---- the import log short-circuit -----------------------------------------
//
// These lock the behaviour that makes `memory import` cheap enough to run on a
// schedule rather than by hand. The PTY wrapper fires it on every agent exit;
// without the short-circuit that is one LLM call per transcript per exit, all
// of it deduped away on write.

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sidekar-import-log-test-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    dir.join(name)
}

#[test]
fn a_file_unchanged_since_its_last_import_is_not_read_again() {
    let path = scratch("unchanged.jsonl");
    fs::write(&path, "one session's worth of turns").unwrap();
    let hash = file_hash(&path).unwrap();

    let mut report = SourceReport::new("claude");
    let read = should_read_with(&mut report, "import:claude:session", &path, |_, _| {
        Some(hash.clone())
    });

    assert!(!read, "an identical file should not reach the LLM twice");
    assert_eq!(report.files_skipped_unchanged, 1);
    assert!(
        report.examined.is_empty(),
        "a skipped file must not be logged as examined, or the log would be \
         rewritten on every run for files nobody read"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn a_file_that_changed_since_its_last_import_is_read_again() {
    let path = scratch("changed.jsonl");
    fs::write(&path, "turns, plus the ones added since").unwrap();

    let mut report = SourceReport::new("claude");
    let read = should_read_with(&mut report, "import:claude:session", &path, |_, _| {
        Some("the hash it had last time".to_string())
    });

    assert!(read);
    assert_eq!(report.files_skipped_unchanged, 0);
    assert_eq!(report.examined.len(), 1);
    assert_eq!(report.examined[0].source_kind, "import:claude:session");
    assert_eq!(report.examined[0].content_hash, file_hash(&path).unwrap());
    let _ = fs::remove_file(&path);
}

#[test]
fn a_file_never_imported_before_is_read() {
    let path = scratch("fresh.jsonl");
    fs::write(&path, "a session sidekar has not seen").unwrap();

    let mut report = SourceReport::new("codex");
    let read = should_read_with(&mut report, "import:codex:session", &path, |_, _| None);

    assert!(read);
    assert_eq!(report.examined.len(), 1);
    let _ = fs::remove_file(&path);
}

#[test]
fn a_file_that_cannot_be_hashed_fails_open() {
    // Deleted between detection and extraction, or unreadable. Reading it costs
    // one wasted open; skipping it could drop a session permanently, so the
    // cheap mistake is the right one.
    let mut report = SourceReport::new("claude");
    let read = should_read_with(
        &mut report,
        "import:claude:session",
        &scratch("not-here.jsonl"),
        |_, _| panic!("must not consult the log for a file it cannot hash"),
    );

    assert!(read);
    assert!(
        report.examined.is_empty(),
        "nothing to log: without a hash there is no way to skip it next time"
    );
}

#[test]
fn the_examined_list_carries_what_the_log_is_keyed_on() {
    // record_import upserts on (source_kind, file_path); an ExaminedFile that
    // did not carry both would log against the wrong row.
    let path = scratch("keys.jsonl");
    fs::write(&path, "x").unwrap();

    let mut report = SourceReport::new("cursor");
    should_read_with(&mut report, "import:cursor:session", &path, |_, _| None);

    let e = &report.examined[0];
    assert_eq!(e.source_kind, "import:cursor:session");
    assert_eq!(e.path, path);
    assert!(!e.content_hash.is_empty());
    let _ = fs::remove_file(&path);
}
