use std::fmt::Write;

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
    write_section(
        &mut out,
        "Automation",
        &[
            (
                "browser",
                "Web automation: navigate, read, click, type, network, ext",
            ),
            ("desktop", "Apps, windows, input, menus, screenshots"),
            ("monitor", "Stream browser tab events to bus"),
        ],
        CYAN,
        YELLOW,
        BOLD,
        DIM,
        RST,
    );
    write_section(
        &mut out,
        "Agent",
        &[
            ("repl", "Interactive agent"),
            ("bus", "Inter-agent communication and coordination"),
            ("memory", "Durable memory"),
            ("tasks", "Local task graph"),
            ("journal", "Session journals"),
            ("agent-sessions", "Session history"),
            ("repo", "Repo packing and token estimates"),
            ("compact", "Output compaction"),
            ("kv", "Encrypted key/value secrets"),
            ("totp", "TOTP secrets"),
            ("cron", "Scheduled jobs"),
            ("loop", "Run prompt on interval"),
        ],
        CYAN,
        YELLOW,
        BOLD,
        DIM,
        RST,
    );
    write_section(
        &mut out,
        "Account",
        &[
            ("device", "Device auth and registration"),
            ("relay", "Active relay sessions"),
        ],
        CYAN,
        YELLOW,
        BOLD,
        DIM,
        RST,
    );
    write_section(
        &mut out,
        "Data",
        &[
            ("doc", "Markdown doc intelligence"),
            ("pack", "Pack JSON, YAML, or CSV"),
            ("unpack", "Unpack packed data"),
        ],
        CYAN,
        YELLOW,
        BOLD,
        DIM,
        RST,
    );
    write_section(
        &mut out,
        "System",
        &[
            ("daemon", "Background daemon"),
            ("proxy", "Captured proxy traffic"),
            ("config", "Settings"),
            ("event", "Event log"),
            ("install", "Install skill file"),
            ("uninstall", "Remove local data and skill files"),
            ("skill", "Print SKILL.md"),
        ],
        CYAN,
        YELLOW,
        BOLD,
        DIM,
        RST,
    );

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
        "  {GREEN}--host{RST}              {DIM}Browser-only: use extension transport; `--tab` uses extension tab ids{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--profile <name>{RST}    {DIM}Browser-only: use named managed Chrome profile{RST}"
    );
    let _ = writeln!(
        out,
        "  {GREEN}--tab <id>{RST}          {DIM}Browser-only: managed CDP target id or extension tab id{RST}"
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

fn write_section(
    out: &mut String,
    title: &str,
    rows: &[(&str, &str)],
    cyan: &str,
    yellow: &str,
    bold: &str,
    dim: &str,
    rst: &str,
) {
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    let _ = writeln!(out, "{yellow}{bold}{title}{rst}");
    for (name, summary) in rows {
        let _ = writeln!(
            out,
            "  {cyan}{:<width$}{rst}  {dim}{}{rst}",
            name,
            summary,
            width = width
        );
    }
    let _ = writeln!(out);
}
