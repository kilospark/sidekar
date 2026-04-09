use super::*;

#[test]
fn rewrite_osc_titles_prepends_formatted_nick_prefix() {
    let raw = b"\x1b]0;claude\x07";
    let rewritten = rewrite_osc_titles(raw, "borzoi - ");
    assert_eq!(rewritten.as_ref(), b"\x1b]0;borzoi - claude\x07");
}
