use super::*;
use serde_json::json;

#[test]
fn text_of_joins_runs_within_a_paragraph() {
    let doc = json!({"body": {"content": [
        {"paragraph": {"elements": [
            {"textRun": {"content": "Hello "}},
            {"textRun": {"content": "world\n"}}
        ]}}
    ]}});
    assert_eq!(text_of(&doc), "Hello world\n");
}

#[test]
fn text_of_skips_non_paragraph_structure() {
    // sectionBreak and table carry no readable runs; including them would
    // produce garbage rather than text.
    let doc = json!({"body": {"content": [
        {"sectionBreak": {}},
        {"paragraph": {"elements": [{"textRun": {"content": "real text"}}]}},
        {"table": {"rows": 2}}
    ]}});
    assert_eq!(text_of(&doc), "real text");
}

#[test]
fn an_empty_document_is_empty_not_a_panic() {
    assert_eq!(text_of(&json!({})), "");
    assert_eq!(text_of(&json!({"body": {"content": []}})), "");
}
