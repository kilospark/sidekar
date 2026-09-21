//! Opening a spawned agent in a visible terminal window.
//!
//! The spawner already knows which terminal it is sitting in — every one of
//! these sets `TERM_PROGRAM`, and it survives the PTY wrapper into the agent's
//! own environment — so a spawned agent lands in the same app by default and
//! `--app` is only needed to override that.

use anyhow::{Result, bail};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalApp {
    AppleTerminal,
    ITerm,
    Ghostty,
    WezTerm,
    Kitty,
    Alacritty,
}

impl TerminalApp {
    /// The name the OS knows this app by — the AppleScript target for the two
    /// that need one, the bundle name `open -na` takes for the rest.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppleTerminal => "Terminal",
            Self::ITerm => "iTerm",
            Self::Ghostty => "Ghostty",
            Self::WezTerm => "WezTerm",
            Self::Kitty => "kitty",
            Self::Alacritty => "Alacritty",
        }
    }

    /// Accepts what a human would type as well as the `TERM_PROGRAM` spellings.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "terminal" | "terminal.app" | "apple_terminal" | "apple terminal" => {
                Some(Self::AppleTerminal)
            }
            "iterm" | "iterm2" | "iterm.app" => Some(Self::ITerm),
            "ghostty" => Some(Self::Ghostty),
            "wezterm" => Some(Self::WezTerm),
            "kitty" => Some(Self::Kitty),
            "alacritty" => Some(Self::Alacritty),
            _ => None,
        }
    }
}

/// The terminal this process is running under, as far as `TERM_PROGRAM` says.
///
/// `None` when nothing recognisable is set, which is the normal case for a
/// detached or SSH session and means there is no window to match.
pub fn detect() -> Option<TerminalApp> {
    let raw = std::env::var("TERM_PROGRAM").ok()?;
    // VS Code's integrated terminal reports itself but cannot open a window of
    // its own; treat it as "no match" so the caller falls back deliberately.
    if raw.eq_ignore_ascii_case("vscode") {
        return None;
    }
    TerminalApp::parse(&raw)
}

/// Wrap `command` so the session is also written to `log`.
///
/// `script` keeps the child on a pty, so the agent still renders as a TUI in the
/// window while the transcript records what it drew. It execs its argument
/// directly rather than going through a shell, so the command needs one of its
/// own: without it the leading `NAME=value` assignments are read as the program
/// to run and the window dies with "No such file or directory".
pub fn with_transcript(command: &str, log: &str) -> String {
    format!(
        "script -q {} /bin/sh -c {}",
        shell_quote(log),
        shell_quote(command)
    )
}

/// Single-quote for `/bin/sh`, closing and reopening around embedded quotes.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Escape for embedding inside an AppleScript double-quoted string.
fn applescript_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

fn osascript(script: &str) -> Result<()> {
    let status = Command::new("osascript").arg("-e").arg(script).status()?;
    if !status.success() {
        bail!("osascript failed while opening the terminal window");
    }
    Ok(())
}

/// Open `command` in a new window of `app`.
///
/// `command` is a single `/bin/sh` line, already quoted by the caller.
pub fn open_window(app: TerminalApp, command: &str) -> Result<()> {
    match app {
        TerminalApp::AppleTerminal => osascript(&format!(
            "tell application \"{}\"\nactivate\ndo script {}\nend tell",
            app.as_str(),
            applescript_quote(command)
        )),
        TerminalApp::ITerm => osascript(&format!(
            "tell application \"{}\"\nactivate\nset w to (create window with default profile)\n\
             tell current session of w to write text {}\nend tell",
            app.as_str(),
            applescript_quote(command)
        )),
        // The rest take a command straight off the argv, so no AppleScript layer.
        TerminalApp::Ghostty => open_app_with_args(app.as_str(), &["-e", "/bin/sh", "-c", command]),
        TerminalApp::WezTerm => {
            open_app_with_args(app.as_str(), &["start", "--", "/bin/sh", "-c", command])
        }
        TerminalApp::Kitty => open_app_with_args(app.as_str(), &["/bin/sh", "-c", command]),
        TerminalApp::Alacritty => {
            open_app_with_args(app.as_str(), &["-e", "/bin/sh", "-c", command])
        }
    }
}

fn open_app_with_args(app: &str, args: &[&str]) -> Result<()> {
    let status = Command::new("open")
        .arg("-na")
        .arg(app)
        .arg("--args")
        .args(args)
        .status()?;
    if !status.success() {
        bail!("could not open {app}; is it installed?");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
