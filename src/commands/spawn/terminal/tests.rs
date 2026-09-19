use super::*;

#[test]
fn parse_accepts_term_program_spellings_and_human_ones() {
    assert_eq!(
        TerminalApp::parse("Apple_Terminal"),
        Some(TerminalApp::AppleTerminal)
    );
    assert_eq!(
        TerminalApp::parse("Terminal"),
        Some(TerminalApp::AppleTerminal)
    );
    assert_eq!(TerminalApp::parse("iTerm.app"), Some(TerminalApp::ITerm));
    assert_eq!(TerminalApp::parse("iterm2"), Some(TerminalApp::ITerm));
    assert_eq!(TerminalApp::parse("ghostty"), Some(TerminalApp::Ghostty));
    assert_eq!(TerminalApp::parse("GHOSTTY"), Some(TerminalApp::Ghostty));
    assert_eq!(TerminalApp::parse("  WezTerm "), Some(TerminalApp::WezTerm));
    assert_eq!(TerminalApp::parse("emacs"), None);
}

#[test]
fn shell_quote_survives_embedded_quotes() {
    assert_eq!(shell_quote("plain"), "'plain'");
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
    // A task string is user text and reaches /bin/sh intact or not at all.
    assert_eq!(
        shell_quote("review 'the diff'; rm -rf /"),
        r"'review '\''the diff'\''; rm -rf /'"
    );
}

#[test]
fn applescript_quote_escapes_backslashes_before_quotes() {
    assert_eq!(applescript_quote("plain"), "\"plain\"");
    assert_eq!(applescript_quote("say \"hi\""), "\"say \\\"hi\\\"\"");
    // Backslash first, or escaping the quote would re-escape its own backslash.
    assert_eq!(applescript_quote(r"c:\path"), "\"c:\\\\path\"");
}

#[test]
fn vscode_reports_no_matching_window() {
    // VS Code's integrated terminal sets TERM_PROGRAM but cannot open a window,
    // so detection must decline rather than hand back something unopenable.
    let prev = std::env::var("TERM_PROGRAM").ok();
    unsafe { std::env::set_var("TERM_PROGRAM", "vscode") };
    assert_eq!(detect(), None);
    unsafe { std::env::set_var("TERM_PROGRAM", "Apple_Terminal") };
    assert_eq!(detect(), Some(TerminalApp::AppleTerminal));
    match prev {
        Some(v) => unsafe { std::env::set_var("TERM_PROGRAM", v) },
        None => unsafe { std::env::remove_var("TERM_PROGRAM") },
    }
}
