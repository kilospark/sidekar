use super::*;
use serde_json::json;

#[test]
fn rows_from_reads_strings_and_numbers_alike() {
    let res = json!({"values": [["a", "b"], ["1", 2]]});
    assert_eq!(rows_from(&res), vec![vec!["a", "b"], vec!["1", "2"]]);
}

#[test]
fn an_empty_range_is_no_rows_not_a_panic() {
    assert!(rows_from(&json!({})).is_empty());
    assert!(rows_from(&json!({"values": []})).is_empty());
}

#[test]
fn squared_pads_the_ragged_rows_sheets_returns() {
    // Sheets omits trailing empty cells, so row 2 arrives shorter than row 1.
    let rows = vec![vec!["a".into(), "b".into(), "c".into()], vec!["d".into()]];
    let out = squared(rows);
    assert_eq!(out[0].len(), 3);
    assert_eq!(out[1], vec!["d", "", ""]);
}

#[test]
fn squared_on_nothing_stays_nothing() {
    assert!(squared(vec![]).is_empty());
}
