use super::*;

#[test]
fn project_root_falls_back_to_canonical_cwd() {
    let tmp = crate::ScratchDir::new("scope-project-root-test");
    let expected = fs::canonicalize(tmp.path()).unwrap();
    assert_eq!(
        resolve_project_root(tmp.path().to_str()),
        expected.to_string_lossy()
    );
}
