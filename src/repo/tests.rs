use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    env::temp_dir().join(format!(
        "sidekar-repo-{name}-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

#[test]
fn finds_repo_root_from_subdir() -> Result<()> {
    let root = temp_dir("root");
    fs::create_dir_all(root.join(".git"))?;
    fs::create_dir_all(root.join("nested/deeper"))?;
    let found = find_repo_root(&root.join("nested/deeper")).context("missing root")?;
    assert_eq!(found, root);
    let _ = fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn collects_files_respecting_sidekarignore() -> Result<()> {
    let root = temp_dir("collect");
    fs::create_dir_all(root.join(".git"))?;
    fs::write(root.join(".gitignore"), "ignored.txt\n")?;
    fs::write(root.join(".sidekarignore"), "private/**\n")?;
    fs::write(root.join("keep.rs"), "fn main() {}\n")?;
    fs::write(root.join("ignored.txt"), "nope\n")?;
    fs::create_dir_all(root.join("private"))?;
    fs::write(root.join("private/secret.md"), "secret\n")?;

    let matcher = MatcherSet::new(&root, &[], &[])?;
    let snapshot = collect_repo_files(&root, &matcher, &[], DEFAULT_MAX_FILE_BYTES)?;
    let paths = snapshot
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"keep.rs"));
    assert!(!paths.contains(&"ignored.txt"));
    assert!(!paths.contains(&"private/secret.md"));

    let _ = fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn tree_reports_estimated_tokens() {
    let root = PathBuf::from("/tmp/example");
    let files = vec![
        RepoFile {
            path: "src/main.rs".into(),
            chars: 16,
            est_tokens: 4,
            language: Some("rust"),
            content: "fn main() {}\n".into(),
        },
        RepoFile {
            path: "README.md".into(),
            chars: 20,
            est_tokens: 5,
            language: Some("markdown"),
            content: "# Example\n".into(),
        },
    ];

    let tree = build_tree_string(&root, &files);
    assert!(tree.contains("example/ (~9 tok, 2 files)"));
    assert!(tree.contains("src/ (~4 tok, 1 files)"));
    assert!(tree.contains("main.rs (~4 tok)"));
}

#[test]
fn discovers_project_actions_from_common_files() -> Result<()> {
    let root = temp_dir("actions");
    fs::create_dir_all(root.join("tests"))?;
    fs::write(
        root.join("package.json"),
        r#"{"scripts":{"test":"vitest","lint":"eslint .","build":"next build"}}"#,
    )?;
    fs::write(
        root.join("pyproject.toml"),
        "[tool.pytest.ini_options]\naddopts = \"-q\"\n[tool.ruff]\n",
    )?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(
        root.join("Makefile"),
        "test:\n\t@echo ok\nlint:\n\t@echo lint\n",
    )?;

    let actions = discover_project_actions(&root)?;
    let ids = actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"npm:test"));
    assert!(ids.contains(&"npm:lint"));
    assert!(ids.contains(&"npm:build"));
    assert!(ids.contains(&"python:test"));
    assert!(ids.contains(&"python:lint"));
    assert!(ids.contains(&"cargo:test"));
    assert!(ids.contains(&"cargo:check"));
    assert!(ids.contains(&"cargo:build"));
    assert!(ids.contains(&"make:test"));
    assert!(ids.contains(&"make:lint"));

    let _ = fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn parses_git_status_outputs() {
    let porcelain = parse_porcelain_status_output(
        " M src/main.rs\nA  src/new.rs\nD  src/old.rs\nR  src/old_name.rs -> src/new_name.rs\n?? notes.txt\n",
    );
    assert_eq!(
        porcelain,
        vec![
            ChangeEntry {
                path: "notes.txt".into(),
                status: ChangeStatus::Untracked,
            },
            ChangeEntry {
                path: "src/main.rs".into(),
                status: ChangeStatus::Modified,
            },
            ChangeEntry {
                path: "src/new.rs".into(),
                status: ChangeStatus::Added,
            },
            ChangeEntry {
                path: "src/new_name.rs".into(),
                status: ChangeStatus::Renamed,
            },
            ChangeEntry {
                path: "src/old.rs".into(),
                status: ChangeStatus::Deleted,
            },
        ]
    );

    let diff = parse_name_status_output(
        "M\tsrc/main.rs\nA\tsrc/new.rs\nD\tsrc/old.rs\nR100\tsrc/old_name.rs\tsrc/new_name.rs\n",
    );
    assert_eq!(
        diff,
        vec![
            ChangeEntry {
                path: "src/main.rs".into(),
                status: ChangeStatus::Modified,
            },
            ChangeEntry {
                path: "src/new.rs".into(),
                status: ChangeStatus::Added,
            },
            ChangeEntry {
                path: "src/new_name.rs".into(),
                status: ChangeStatus::Renamed,
            },
            ChangeEntry {
                path: "src/old.rs".into(),
                status: ChangeStatus::Deleted,
            },
        ]
    );
}

#[test]
fn extracts_symbol_summaries_from_rust_source() {
    let symbols = extract_symbol_summaries(
        "src/lib.rs",
        "pub struct App {}\nasync fn start() {}\npub trait Runner {}\n",
        10,
    );
    assert_eq!(
        symbols,
        vec![
            RepoSymbol {
                name: "App".into(),
                kind: "struct".into(),
                line: 1,
            },
            RepoSymbol {
                name: "start".into(),
                kind: "function".into(),
                line: 2,
            },
            RepoSymbol {
                name: "Runner".into(),
                kind: "trait".into(),
                line: 3,
            },
        ]
    );
}
