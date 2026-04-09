use super::*;

#[test]
fn parse_action_options_defaults_to_non_retrying_required_steps() {
    let options = parse_action_options(&json!({"tool": "click"}));
    assert_eq!(options.wait, None);
    assert_eq!(options.retries, 0);
    assert_eq!(options.retry_delay, 0);
    assert!(!options.optional);
}

#[test]
fn parse_action_options_reads_retry_fields() {
    let options = parse_action_options(&json!({
        "tool": "click",
        "wait": 750,
        "retries": 2,
        "retry_delay": 300,
        "optional": true
    }));
    assert_eq!(options.wait, Some(750));
    assert_eq!(options.retries, 2);
    assert_eq!(options.retry_delay, 300);
    assert!(options.optional);
}

#[test]
fn compute_wait_ms_prefers_explicit_wait() {
    assert_eq!(compute_wait_ms("click", true, Some(900), 200), 900);
}

#[test]
fn compute_wait_ms_only_applies_smart_wait_on_success() {
    assert_eq!(compute_wait_ms("click", true, None, 150), 650);
    assert_eq!(compute_wait_ms("click", false, None, 150), 150);
    assert_eq!(compute_wait_ms("read", true, None, 150), 150);
}
