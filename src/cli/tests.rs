use super::render_help;
use crate::command_catalog::{CommandGroup, command_specs};

/// Every top-level command must appear in `sidekar help`.
///
/// The listing is hand-curated because the catalog also holds browser
/// subcommands, which do not belong at the top level. That curation is exactly
/// what drifts: `spawn`, `stop`, `gmail`, `drive`, `calendar`, `sheets` and
/// `docs` were all runnable and documented per-command while being invisible to
/// `sidekar help`, which is the one place an agent looks to find out what
/// exists.
#[test]
fn every_top_level_command_is_discoverable_from_help() {
    let help = render_help("test");
    let missing: Vec<&str> = command_specs()
        .iter()
        .filter(|s| {
            matches!(
                s.group,
                CommandGroup::Agent | CommandGroup::Account | CommandGroup::Jobs
            )
        })
        .map(|s| s.name)
        .filter(|name| !help.contains(*name))
        .collect();
    assert!(
        missing.is_empty(),
        "these commands run but cannot be found from `sidekar help`: {missing:?}\n\
         Add them to the listing in src/cli.rs."
    );
}

#[test]
fn help_names_only_commands_that_exist() {
    let help = render_help("test");
    // Section bodies are "  name  summary"; take the first word of each.
    for line in help.lines() {
        let Some(rest) = line.strip_prefix("  ") else {
            continue;
        };
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        if !name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            continue;
        }
        // The usage block at the top is "  sidekar <command> …", not a listing.
        if name == "sidekar" {
            continue;
        }
        assert!(
            crate::is_known_command(name) || crate::command_handler(name).is_some(),
            "`sidekar help` lists '{name}', which is not a runnable command"
        );
    }
}
