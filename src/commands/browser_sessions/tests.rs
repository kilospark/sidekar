use super::*;
use std::time::{Duration, SystemTime};

#[test]
fn age_is_the_time_since_the_update_not_since_1970() {
    let now = SystemTime::now();
    assert_eq!(session_age(Some(now - Duration::from_secs(5))), "5s ago");
    assert_eq!(session_age(Some(now - Duration::from_secs(180))), "3m ago");
    assert_eq!(session_age(Some(now - Duration::from_secs(2 * 3600))), "2h ago");
    assert_eq!(session_age(Some(now - Duration::from_secs(3 * 86_400))), "3d ago");
}

#[test]
fn a_future_time_reads_as_just_now_and_a_missing_one_as_a_dash() {
    let ahead = SystemTime::now() + Duration::from_secs(60);
    assert_eq!(session_age(Some(ahead)), "0s ago");
    assert_eq!(session_age(None), "-");
}

#[test]
fn the_text_view_leads_with_when_and_json_keeps_the_age() {
    let out = BrowserSessionsOutput {
        items: vec![BrowserSessionSummary {
            id: "s1".into(),
            browser: "chrome".into(),
            profile: "default".into(),
            tab_count: 1,
            active_tab: "-".into(),
            updated: "3m ago".into(),
            updated_at: Some(1_789_357_009),
        }],
    };
    let mut buf = Vec::new();
    crate::output::CommandOutput::render_text(&out, &mut buf).unwrap();
    let shown = String::from_utf8(buf).unwrap();
    assert!(shown.contains("2026-09-14T03:36:49Z (3m ago)"), "{shown}");
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(json["items"][0]["updated"], "3m ago");
    assert!(json["items"][0].get("updated_at").is_none());
}
