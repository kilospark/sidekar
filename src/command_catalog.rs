mod account;
mod agent;
mod browser;
mod code;
mod system;

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandGroup {
    Browser,
    Page,
    Interact,
    Code,
    Data,
    Desktop,
    Agent,
    Jobs,
    Account,
    System,
}

impl CommandGroup {
    pub fn title(self) -> &'static str {
        match self {
            Self::Browser => "Browser",
            Self::Page => "Page",
            Self::Interact => "Interact",
            Self::Code => "Code",
            Self::Data => "Data",
            Self::Desktop => "Desktop",
            Self::Agent => "Agent",
            Self::Jobs => "Jobs",
            Self::Account => "Account",
            Self::System => "System",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
    pub group: CommandGroup,
    pub aliases: &'static [&'static str],
    pub requires_session: bool,
    pub auto_launch_browser: bool,
    pub ext_routable: bool,
}

const fn spec(
    name: &'static str,
    usage: &'static str,
    summary: &'static str,
    group: CommandGroup,
    requires_session: bool,
    auto_launch_browser: bool,
    ext_routable: bool,
) -> CommandSpec {
    CommandSpec {
        name,
        usage,
        summary,
        group,
        aliases: &[],
        requires_session,
        auto_launch_browser,
        ext_routable,
    }
}

#[allow(clippy::too_many_arguments)]
const fn spec_aliases(
    name: &'static str,
    usage: &'static str,
    summary: &'static str,
    group: CommandGroup,
    aliases: &'static [&'static str],
    requires_session: bool,
    auto_launch_browser: bool,
    ext_routable: bool,
) -> CommandSpec {
    CommandSpec {
        name,
        usage,
        summary,
        group,
        aliases,
        requires_session,
        auto_launch_browser,
        ext_routable,
    }
}

const REMOVED_COMMANDS: &[(&str, &str)] = &[
    ("who", "bus who"),
    ("bus-send", "bus send"),
    ("bus_send", "bus send"),
    ("bus-done", "bus done"),
    ("bus_done", "bus done"),
    ("cron-create", "cron create"),
    ("cron_create", "cron create"),
    ("cron-list", "cron list"),
    ("cron_list", "cron list"),
    ("cron-delete", "cron delete"),
    ("cron_delete", "cron delete"),
    ("desktop-screenshot", "desktop screenshot"),
    ("desktop_screenshot", "desktop screenshot"),
    ("desktop-apps", "desktop apps"),
    ("desktop_apps", "desktop apps"),
    ("desktop-windows", "desktop windows"),
    ("desktop_windows", "desktop windows"),
    ("desktop-find", "desktop find"),
    ("desktop_find", "desktop find"),
    ("desktop-click", "desktop click"),
    ("desktop_click", "desktop click"),
    ("desktop-press", "desktop press"),
    ("desktop_press", "desktop press"),
    ("desktop-type", "desktop type"),
    ("desktop_type", "desktop type"),
    ("desktop-paste", "desktop paste"),
    ("desktop_paste", "desktop paste"),
    ("desktop-launch", "desktop launch"),
    ("desktop_launch", "desktop launch"),
    ("desktop-activate", "desktop activate"),
    ("desktop_activate", "desktop activate"),
    ("desktop-quit", "desktop quit"),
    ("desktop_quit", "desktop quit"),
    ("axtree", "ax-tree"),
    ("ax_tree", "ax-tree"),
    ("newtab", "new-tab"),
    ("new_tab", "new-tab"),
    ("readurls", "read-urls"),
    ("read_urls", "read-urls"),
    ("inserttext", "insert-text"),
    ("insert_text", "insert-text"),
    ("waitfor", "wait-for"),
    ("wait_for", "wait-for"),
    ("waitfornav", "wait-for-nav"),
    ("wait_for_nav", "wait-for-nav"),
    ("sw", "service-workers"),
    ("service_workers", "service-workers"),
];

pub fn command_specs() -> &'static [CommandSpec] {
    static COMMAND_SPECS: OnceLock<Vec<CommandSpec>> = OnceLock::new();
    COMMAND_SPECS
        .get_or_init(|| {
            let mut out = Vec::new();
            out.extend_from_slice(browser::COMMANDS);
            out.extend_from_slice(agent::COMMANDS);
            out.extend_from_slice(account::COMMANDS);
            out.extend_from_slice(system::COMMANDS);
            out.extend_from_slice(code::COMMANDS);
            out
        })
        .as_slice()
}

pub fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    public_command_spec(name)
}

fn handler_name(public_name: &str) -> &str {
    match public_name {
        "ax-tree" => "axtree",
        "new-tab" => "newtab",
        "read-urls" => "readurls",
        "insert-text" => "inserttext",
        "wait-for" => "waitfor",
        "wait-for-nav" => "waitfornav",
        "service-workers" => "sw",
        other => other,
    }
}

fn public_command_spec(name: &str) -> Option<&'static CommandSpec> {
    command_specs()
        .iter()
        .find(|spec| spec.name == name || spec.aliases.contains(&name))
}

pub fn removed_command_replacement(name: &str) -> Option<&'static str> {
    REMOVED_COMMANDS
        .iter()
        .find_map(|(removed, replacement)| (*removed == name).then_some(*replacement))
}

pub fn canonical_command_name(name: &str) -> Option<&'static str> {
    public_command_spec(name).map(|spec| spec.name)
}

pub fn is_known_command(name: &str) -> bool {
    public_command_spec(name).is_some()
}

pub fn command_handler(name: &str) -> Option<&'static str> {
    if let Some(spec) = public_command_spec(name) {
        return Some(handler_name(spec.name));
    }

    command_specs()
        .iter()
        .find(|spec| handler_name(spec.name) == name)
        .map(|spec| handler_name(spec.name))
}

pub fn command_requires_session(name: &str) -> bool {
    public_command_spec(name)
        .map(|spec| spec.requires_session)
        .unwrap_or(false)
}

pub fn command_should_auto_launch_browser(name: &str) -> bool {
    public_command_spec(name)
        .map(|spec| spec.auto_launch_browser)
        .unwrap_or(false)
}

pub fn is_ext_routable_command(name: &str) -> bool {
    public_command_spec(name)
        .map(|spec| spec.ext_routable)
        .unwrap_or(false)
}

/// Render a compact `Group: cmd1, cmd2, ...` catalog for embedding in an LLM
/// tool description. No colors, no flags, just the top-level command names
/// grouped by category — the model gets a one-shot map of the CLI surface
/// without needing to call `sidekar help` for discovery.
///
/// Sourced from the same `command_specs()` that powers `sidekar help`, so
/// there is a single source of truth.
pub fn render_tool_catalog() -> &'static str {
    static CATALOG: OnceLock<String> = OnceLock::new();
    CATALOG.get_or_init(|| {
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
        let mut out = String::new();
        for group in groups {
            let mut first = true;
            for spec in command_specs() {
                if spec.group != group {
                    continue;
                }
                if first {
                    out.push_str(group.title());
                    out.push_str(": ");
                    first = false;
                } else {
                    out.push_str(", ");
                }
                out.push_str(spec.name);
            }
            if !first {
                out.push('\n');
            }
        }
        out
    })
}
