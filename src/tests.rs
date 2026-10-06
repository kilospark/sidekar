use super::*;

#[test]
fn a_scratch_dir_is_removed_even_when_the_test_panics() {
    let mut path = None;
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let dir = ScratchDir::new("panic-test");
        std::fs::write(dir.join("fixture"), "x").unwrap();
        path = Some(dir.path().to_path_buf());
        panic!("the test failed");
    }));
    assert!(panicked.is_err());
    let path = path.expect("the test ran");
    assert!(!path.exists(), "{} was left behind", path.display());
}
