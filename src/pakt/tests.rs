use super::*;

#[test]
fn packs_and_unpacks_json_with_repeated_keys() {
    let value = serde_json::json!({
        "users": [
            {"display_name": "Alice", "email_address": "alice@example.com"},
            {"display_name": "Bob", "email_address": "bob@example.com"}
        ]
    });

    let packed = pack_document(&value, Format::Json).expect("pack");
    assert!(packed.contains(PACK_MAGIC));
    assert!(packed.contains("@keys"));
    let body = packed.lines().last().expect("packed body");
    assert!(!body.contains("\"display_name\""));

    let parsed = parse_packed_document(&packed).expect("parse");
    let unpacked = unpack_value(&parsed.body, &parsed.keys);
    assert_eq!(unpacked, value);
}

#[test]
fn csv_roundtrip_uses_array_of_objects() {
    let input = "name,email\nAlice,alice@example.com\nBob,bob@example.com\n";
    let value = parse_csv(input).expect("parse csv");
    let rendered = render_csv(&value).expect("render csv");
    let reparsed = parse_csv(&rendered).expect("reparse csv");
    assert_eq!(reparsed, value);
}
