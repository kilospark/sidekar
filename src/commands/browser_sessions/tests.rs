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
