use super::*;

#[test]
fn project_root_falls_back_to_canonical_cwd() {
    let tmp = env::temp_dir().join("sidekar-scope-project-root-test");
    fs::create_dir_all(&tmp).unwrap();
    let expected = fs::canonicalize(&tmp).unwrap();
    assert_eq!(
        resolve_project_root(tmp.to_str()),
        expected.to_string_lossy()
    );
    let _ = fs::remove_dir_all(&tmp);
}
