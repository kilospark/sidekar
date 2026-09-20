use super::*;
use serde_json::json;

#[test]
fn a_bare_date_means_all_day() {
    assert_eq!(time_field("2026-09-20"), json!({"date": "2026-09-20"}));
}

#[test]
fn a_timestamp_stays_a_timestamp() {
    assert_eq!(
        time_field("2026-09-20T14:00:00-04:00"),
        json!({"dateTime": "2026-09-20T14:00:00-04:00"})
    );
}

#[test]
fn when_reads_both_timed_and_all_day_events() {
    assert_eq!(
        when(Some(&json!({"dateTime": "2026-09-20T14:00:00Z"}))),
        "2026-09-20T14:00:00Z"
    );
    assert_eq!(when(Some(&json!({"date": "2026-09-20"}))), "2026-09-20");
    assert_eq!(when(None), "");
}

#[test]
fn an_untitled_event_is_labelled_rather_than_blank() {
    let e = to_event(&json!({"id": "x", "start": {"date": "2026-09-20"}}));
    assert_eq!(e.summary, "(no title)");
    assert_eq!(e.attendees, 0);
}

#[test]
fn rfc3339_window_is_well_formed_and_ordered() {
    let now = rfc3339_in_days(0);
    let later = rfc3339_in_days(7);
    assert!(now.ends_with('Z') && now.len() == 20, "got {now}");
    assert!(later > now, "{later} should sort after {now}");
}
