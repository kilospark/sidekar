use super::*;

#[test]
fn every_cursor_alias_maps_to_one_source() {
    // cursor registers under three names but writes a single store.
    for alias in ["cursor", "cursor-agent", "agent"] {
        assert_eq!(source_for(alias), Some("cursor"), "{alias}");
    }
}

#[test]
fn the_harnesses_import_can_read_are_recognised() {
    for agent in ["claude", "codex", "gemini", "opencode", "copilot"] {
        assert_eq!(source_for(agent), Some(agent));
    }
}

#[test]
fn an_agent_with_no_readable_transcript_hands_off_nothing() {
    // grok and pi are wrappable but memory import cannot parse what they write.
    // Spawning an import for those would cost an LLM call to find no input.
    for agent in ["grok", "pi", "not-an-agent"] {
        assert_eq!(source_for(agent), None, "{agent}");
    }
}

#[test]
fn every_source_we_name_is_one_the_importer_accepts() {
    // This is the failure that would be silent: rename a source in the importer
    // and the handoff keeps spawning imports that exit on "Unknown source",
    // with stderr pointed at /dev/null, so journaling just quietly stops.
    for source in IMPORTABLE {
        assert!(
            crate::memory::import::is_known_source(source),
            "{source} is not a --source= the importer accepts"
        );
    }
}

#[test]
fn the_journal_switch_is_the_same_one_the_repl_reads() {
    // Deliberately runtime::journal() rather than a second switch: somebody who
    // turned journaling off meant it for the whole tool, and a wrapper still
    // summarising their session would be a nasty surprise.
    assert_eq!(enabled(), crate::runtime::journal());
}
