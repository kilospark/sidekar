use super::*;
use std::collections::HashSet;

#[test]
fn custom_help_entries_match_command_catalog() {
    let names = crate::help_text::custom_help_commands();
    let unique = names.iter().collect::<HashSet<_>>();
    assert_eq!(
        names.len(),
        unique.len(),
        "duplicate custom help entries found"
    );

    for name in names {
        assert!(
            crate::command_catalog::command_spec(name).is_some(),
            "custom help entry missing command spec: {name}"
        );
    }
}

#[test]
fn every_command_has_printable_help() {
    for spec in crate::command_catalog::command_specs() {
        assert!(
            command_help_text(spec.name).is_some() || command_spec_fallback(spec.name).is_some(),
            "command is missing help text: {}",
            spec.name
        );
    }
}
