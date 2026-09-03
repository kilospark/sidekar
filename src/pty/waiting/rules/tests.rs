use super::*;

#[test]
fn comments_and_blank_lines_are_not_markers() {
    let doc = "\
# a comment
do you want to

  # indented comment
proceed?
";
    let rules = Rules::from_documents(doc, "", "");
    assert_eq!(rules.question_markers, vec!["do you want to", "proceed?"]);
}

#[test]
fn markers_are_lowercased_for_case_insensitive_matching() {
    let rules = Rules::from_documents("Do You Want To", "(Y/N)", "");
    assert_eq!(rules.question_markers, vec!["do you want to"]);
    assert_eq!(rules.yes_no_markers, vec!["(y/n)"]);
}

#[test]
fn only_the_first_character_of_a_composer_line_is_read() {
    // The rest of the line is free to carry a note about the glyph.
    let doc = "\
>
❯   claude, codex
›
";
    let rules = Rules::from_documents("", "", doc);
    assert_eq!(rules.composer_markers, vec!['>', '❯', '›']);
}

#[test]
fn shipped_defaults_parse_into_every_list() {
    let rules = Rules::builtin();
    assert!(
        rules
            .question_markers
            .contains(&"do you want to".to_string()),
        "{:?}",
        rules.question_markers
    );
    assert!(rules.yes_no_markers.contains(&"(y/n)".to_string()));
    assert!(rules.composer_markers.contains(&'>'));
    assert!(rules.composer_markers.contains(&'❯'));
}

#[test]
fn shipped_defaults_carry_no_comment_text_into_the_markers() {
    // A `#` line leaking through would match agent prose containing a hash.
    for marker in &Rules::builtin().question_markers {
        assert!(!marker.starts_with('#'), "comment leaked: {marker}");
    }
}

#[test]
fn an_emptied_list_falls_back_instead_of_disabling_detection() {
    let builtin = Rules::builtin();
    assert_eq!(
        or_builtin(Vec::<String>::new(), builtin.question_markers.clone()),
        builtin.question_markers
    );
    // A non-empty edit is taken as-is.
    assert_eq!(
        or_builtin(vec!["custom".to_string()], builtin.question_markers),
        vec!["custom".to_string()]
    );
}
