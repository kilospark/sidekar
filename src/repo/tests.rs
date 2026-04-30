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
