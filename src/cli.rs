use std::fmt::Write;

use crate::command_catalog::{CommandGroup, CommandSpec, command_specs};

pub fn render_help(version: &str) -> String {
    const BOLD: &str = "\x1b[1m";
    const DIM: &str = "\x1b[2m";
    const CYAN: &str = "\x1b[36m";
    const YELLOW: &str = "\x1b[33m";
    const GREEN: &str = "\x1b[32m";
    const RST: &str = "\x1b[0m";

    let mut out = String::new();
    let _ = writeln!(out, "{BOLD}sidekar{RST} {DIM}v{version}{RST}");
    let _ = writeln!(out);
    let _ = writeln!(out, "{BOLD}Usage:{RST} sidekar <command> [args]");
    let _ = writeln!(
        out,
        "       sidekar repl {DIM}[-c cred] [-m model] [-p prompt] [-r [session]] [--verbose]{RST}"
    );
    let _ = writeln!(out, "       sidekar <agent>  {DIM}(wrap agent in PTY){RST}");
    let _ = writeln!(out, "       sidekar help <command>");
    let _ = writeln!(out);

    let groups = [
        CommandGroup::Browser,
        CommandGroup::Page,
        CommandGroup::Interact,
        CommandGroup::Code,
        CommandGroup::Data,
        CommandGroup::Desktop,
        CommandGroup::Agent,
        CommandGroup::Jobs,
        CommandGroup::Account,
        CommandGroup::System,
    ];

    let visible_specs: Vec<&CommandSpec> = command_specs().iter().collect();
    let name_width = visible_specs
        .iter()
        .map(|spec| spec.name.len())
        .max()
        .unwrap_or(0);

    for group in groups {
        let specs: Vec<&CommandSpec> = visible_specs
            .iter()
            .copied()
            .filter(|spec| spec.group == group)
            .collect();
        if specs.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{YELLOW}{BOLD}{}{RST}", group.title());
        for spec in specs {
            let _ = writeln!(
                out,
                "  {CYAN}{:<width$}{RST}  {DIM}{}{RST}",
                spec.name,
                spec.summary,
                width = name_width
            );
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "{YELLOW}{BOLD}Global Flags{RST}");
    let _ = writeln!(
        out,
        "  {GREEN}--verbose{RST}           {DIM}Show debug output and API request details{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--quiet{RST}, {GREEN}-q{RST}          {DIM}Suppress non-essential output{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--format <fmt>{RST}      {DIM}Output format: text (default), json, toon, markdown{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--json{RST}              {DIM}Shorthand for --format=json (where supported){RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--toon{RST}              {DIM}Shorthand for --format=toon — compact LLM-friendly output{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--markdown{RST}, {GREEN}--md{RST}    {DIM}Shorthand for --format=markdown (where supported){RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--proxy{RST}             {DIM}Enable MITM proxy for sidekar <agent>{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--no-proxy{RST}          {DIM}Disable MITM proxy for sidekar <agent>{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--relay{RST}             {DIM}Enable relay tunnel for sidekar <agent>{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--no-relay{RST}          {DIM}Disable relay tunnel for sidekar <agent>{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--tab <id>{RST}          {DIM}Target a specific tab (bypasses session){RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--{RST}                  {DIM}End sidekar flags; pass remaining args to agent{RST}"
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{DIM}Respects NO_COLOR env var. ANSI colors are stripped when output is piped.{RST}"
    );
    out
}
