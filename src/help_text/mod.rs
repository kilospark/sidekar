mod agent;
mod browser;
mod page;
mod system;

pub fn command_help_text(command: &str) -> Option<&'static str> {
    browser::get(command)
        .or_else(|| page::get(command))
        .or_else(|| agent::get(command))
        .or_else(|| system::get(command))
}

pub fn custom_help_commands() -> Vec<&'static str> {
    let mut out = Vec::new();
    out.extend_from_slice(browser::COMMANDS);
    out.extend_from_slice(page::COMMANDS);
    out.extend_from_slice(agent::COMMANDS);
    out.extend_from_slice(system::COMMANDS);
    out
}
