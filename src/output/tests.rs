use super::*;

#[test]
fn parse_accepts_canonical_names() {
    assert_eq!(OutputFormat::parse("text"), Some(OutputFormat::Text));
    assert_eq!(OutputFormat::parse("json"), Some(OutputFormat::Json));
    assert_eq!(OutputFormat::parse("toon"), Some(OutputFormat::Toon));
}

#[test]
fn parse_is_case_insensitive() {
    assert_eq!(OutputFormat::parse("JSON"), Some(OutputFormat::Json));
    assert_eq!(OutputFormat::parse("Toon"), Some(OutputFormat::Toon));
}

#[test]
fn parse_accepts_text_aliases() {
    assert_eq!(OutputFormat::parse("txt"), Some(OutputFormat::Text));
    assert_eq!(OutputFormat::parse("plain"), Some(OutputFormat::Text));
}

#[test]
fn parse_rejects_unknown() {
    assert_eq!(OutputFormat::parse("xml"), None);
    assert_eq!(OutputFormat::parse(""), None);
}

#[test]
fn default_is_text() {
    assert_eq!(OutputFormat::default(), OutputFormat::Text);
}
