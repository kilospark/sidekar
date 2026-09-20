use super::*;

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn flag_reads_both_spellings() {
    let a = v(&["--to", "x@y.com", "--subject=Hi"]);
    assert_eq!(flag(&a, "--to").as_deref(), Some("x@y.com"));
    assert_eq!(flag(&a, "--subject").as_deref(), Some("Hi"));
    assert_eq!(flag(&a, "--body"), None);
}

#[test]
fn positional_skips_a_flag_and_the_value_it_eats() {
    // "from:bob" is the query; --limit and its 5 are not part of it.
    let a = v(&["from:bob", "--limit", "5"]);
    assert_eq!(positional(&a), vec!["from:bob"]);
}

#[test]
fn positional_keeps_the_value_after_an_equals_flag() {
    // --limit=5 carries its own value, so the next word is still the query.
    let a = v(&["--limit=5", "is:unread"]);
    assert_eq!(positional(&a), vec!["is:unread"]);
}

#[test]
fn positional_joins_a_multi_word_query() {
    let a = v(&["subject:invoice", "newer_than:2d", "--limit", "3"]);
    assert_eq!(positional(&a).join(" "), "subject:invoice newer_than:2d");
}

#[test]
fn flag_usize_ignores_a_non_number() {
    assert_eq!(flag_usize(&v(&["--limit", "12"]), "--limit"), Some(12));
    assert_eq!(flag_usize(&v(&["--limit", "lots"]), "--limit"), None);
}

#[test]
fn parse_grid_splits_rows_on_pipes_and_cells_on_commas() {
    assert_eq!(parse_grid("a,b|c,d"), vec![vec!["a", "b"], vec!["c", "d"]]);
}

#[test]
fn parse_grid_trims_whitespace_around_cells() {
    assert_eq!(
        parse_grid("a , b | c ,d"),
        vec![vec!["a", "b"], vec!["c", "d"]]
    );
}

#[test]
fn parse_grid_handles_a_single_cell_and_empty_cells() {
    assert_eq!(parse_grid("solo"), vec![vec!["solo"]]);
    assert_eq!(parse_grid("a,,c"), vec![vec!["a", "", "c"]]);
}
