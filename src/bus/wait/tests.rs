use super::*;

#[test]
fn until_parses_every_documented_spelling() {
    assert_eq!(WaitUntil::parse("settled").unwrap(), WaitUntil::Settled);
    assert_eq!(
        WaitUntil::parse("idle").unwrap(),
        WaitUntil::State(ActivityState::Idle)
    );
    for spelling in ["needs-input", "needs_input"] {
        assert_eq!(
            WaitUntil::parse(spelling).unwrap(),
            WaitUntil::State(ActivityState::NeedsInput),
            "{spelling}"
        );
    }
    for spelling in ["working", "agent-working", "agent_working"] {
        assert_eq!(
            WaitUntil::parse(spelling).unwrap(),
            WaitUntil::State(ActivityState::AgentWorking),
            "{spelling}"
        );
    }
    for spelling in ["user-typing", "user_typing"] {
        assert_eq!(
            WaitUntil::parse(spelling).unwrap(),
            WaitUntil::State(ActivityState::UserTyping),
            "{spelling}"
        );
    }
}

#[test]
fn until_rejects_unknown_states_and_names_the_valid_ones() {
    let err = WaitUntil::parse("blocked").unwrap_err().to_string();
    assert!(err.contains("needs-input"), "{err}");
}

#[test]
fn settled_covers_idle_and_needs_input_only() {
    let settled = WaitUntil::Settled;
    assert!(settled.satisfied_by(ActivityState::Idle));
    // An agent parked on a permission prompt has stopped working; the caller
    // needs to know that as much as it needs to know about a clean finish.
    assert!(settled.satisfied_by(ActivityState::NeedsInput));
    assert!(!settled.satisfied_by(ActivityState::AgentWorking));
    assert!(!settled.satisfied_by(ActivityState::UserTyping));
    assert!(!settled.satisfied_by(ActivityState::Unknown));
}

#[test]
fn explicit_state_matches_exactly() {
    let want = WaitUntil::State(ActivityState::NeedsInput);
    assert!(want.satisfied_by(ActivityState::NeedsInput));
    assert!(!want.satisfied_by(ActivityState::Idle));
}

#[test]
fn timeout_requires_a_positive_number_of_milliseconds() {
    assert_eq!(parse_timeout("5000").unwrap(), 5000);
    assert!(parse_timeout("0").is_err());
    assert!(parse_timeout("-1").is_err());
    assert!(parse_timeout("30s").is_err());
}

#[test]
fn describe_names_the_target_for_the_timeout_message() {
    assert_eq!(WaitUntil::Settled.describe(), "settled");
    assert_eq!(
        WaitUntil::State(ActivityState::AgentWorking).describe(),
        "agent_working"
    );
}
