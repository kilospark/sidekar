use super::{CommandGroup, CommandSpec, spec};

pub const COMMANDS: &[CommandSpec] = &[
    spec(
        "pack",
        "[path|-] [--from=json|yaml|csv]",
        "Pack JSON, YAML, or CSV into a compact text format",
        CommandGroup::Data,
        false,
        false,
        false,
    ),
    spec(
        "unpack",
        "[path|-] [--to=json|yaml|csv]",
        "Restore packed text to JSON, YAML, or CSV",
        CommandGroup::Data,
        false,
        false,
        false,
    ),
    spec(
        "doc",
        "<outline|section|search|map> [args]",
        "Markdown document intelligence (outline, sections, search)",
        CommandGroup::Code,
        false,
        false,
        false,
    ),
];
