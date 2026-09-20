use super::*;

#[test]
fn human_size_scales_and_admits_when_there_is_none() {
    assert_eq!(human_size(Some("512")), "512B");
    assert_eq!(human_size(Some("2048")), "2.0KB");
    assert_eq!(human_size(Some("5242880")), "5.0MB");
    // Google-native docs report no size at all; guessing one would be a lie.
    assert_eq!(human_size(None), "-");
    assert_eq!(human_size(Some("not-a-number")), "-");
}

#[test]
fn str_at_never_panics_on_a_missing_field() {
    let v = serde_json::json!({"id": "abc"});
    assert_eq!(str_at(&v, "id"), "abc");
    assert_eq!(str_at(&v, "name"), "");
}
