use super::*;

#[test]
fn an_explicit_choice_wins_over_everything() {
    let stored = vec!["ks".to_string(), "or-kb".to_string()];
    assert_eq!(
        choose_credential(Some("sf-karthik"), "ks", &stored),
        Some("sf-karthik".to_string())
    );
    assert_eq!(
        choose_credential(None, "ks", &stored),
        Some("ks".to_string())
    );
}

#[test]
fn a_single_stored_credential_is_used_without_being_asked_for() {
    // The rule that keeps a fresh install from silently doing nothing. With one
    // credential there is no decision to make, so making the user state it
    // would only mean the machine never learns and never says why.
    let stored = vec!["ks".to_string()];
    assert_eq!(choose_credential(None, "", &stored), Some("ks".to_string()));
}

#[test]
fn several_stored_and_no_default_is_left_unanswered() {
    // Each of these bills somewhere different. Guessing would spend somebody's
    // money on their behalf, so this stays None and the caller says so out loud.
    let stored = vec!["ks".to_string(), "sf-karthik".to_string()];
    assert_eq!(choose_credential(None, "", &stored), None);
}

#[test]
fn nothing_stored_is_also_unanswered() {
    assert_eq!(choose_credential(None, "", &[]), None);
}

#[test]
fn blank_and_padded_values_are_not_mistaken_for_choices() {
    // An env var set to empty is how a shell says "unset"; a config value with
    // a stray newline is how a copy-paste arrives.
    let stored = vec!["ks".to_string()];
    assert_eq!(
        choose_credential(Some(""), "", &stored),
        Some("ks".to_string())
    );
    assert_eq!(
        choose_credential(Some("  "), "", &stored),
        Some("ks".to_string())
    );
    assert_eq!(
        choose_credential(None, " sf-karthik\n", &[]),
        Some("sf-karthik".to_string())
    );
}
