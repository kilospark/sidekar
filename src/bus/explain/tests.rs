use super::*;
use crate::activity::ActivityState;
use crate::broker::ActivityDetail;

fn detail(settled_at: Option<u64>, seen_at: Option<u64>) -> ActivityDetail {
    ActivityDetail {
        state: ActivityState::Idle,
        at: 1_000,
        reason: None,
        settled_at,
        seen_at,
    }
}

#[test]
fn ago_scales_from_seconds_to_days() {
    assert_eq!(ago(0), "just now");
    assert_eq!(ago(1), "just now");
    assert_eq!(ago(45), "45s ago");
    assert_eq!(ago(90), "1m ago");
    assert_eq!(ago(7_200), "2h ago");
    assert_eq!(ago(172_800), "2d ago");
}

#[test]
fn a_finish_nobody_looked_at_is_unseen() {
    assert!(detail(Some(500), None).finished_unseen());
}

#[test]
fn a_finish_looked_at_afterwards_is_seen() {
    assert!(!detail(Some(500), Some(600)).finished_unseen());
}

#[test]
fn presence_before_the_finish_does_not_count_as_seeing_it() {
    // The human was there, then walked away, and the turn ended after that.
    assert!(detail(Some(500), Some(400)).finished_unseen());
}

#[test]
fn an_agent_that_never_finished_a_turn_is_not_unseen() {
    // Nothing has completed, so there is nothing to have missed.
    assert!(!detail(None, None).finished_unseen());
    assert!(!detail(None, Some(600)).finished_unseen());
}
