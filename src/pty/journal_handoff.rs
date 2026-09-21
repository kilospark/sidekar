//! Journaling for PTY-wrapped agents.
//!
//! `sidekar repl` journals itself: it watches for idle, summarises the slice,
//! and lands candidates in `memory_candidates`. A wrapped agent — `sidekar
//! claude`, `codex`, `cursor-agent` — got none of that, because journaling
//! hangs off the REPL loop and a wrapper has no loop of its own. The whole
//! learning path therefore ran only where sidekar owns the conversation, which
//! is the case that matters least.
//!
//! The transcripts were never the obstacle. `memory::import` already reads
//! Claude Code, Codex, Cursor, Gemini, opencode and Copilot from disk; nothing
//! had ever called it, so `memory_import_log` sat empty on a machine that had
//! been running wrapped agents for months. This runs it when a wrapped agent
//! exits, scoped to that harness, in a detached process so a slow extraction
//! cannot hold the terminal.
//!
//! Firing on every exit is only affordable because the import now reads its own
//! log and skips files it has already seen — see `should_read` in
//! `memory::import::commands`, and `context/journaling.md`.

use std::process::{Command, Stdio};

/// Harnesses `memory import` can read.
///
/// The wrapper also runs `grok` and `pi`, which write no transcript the importer
/// knows how to parse, so they exit with nothing to hand off. Keep this in step
/// with `memory::import::sources::SOURCE_IDS`.
const IMPORTABLE: &[&str] = &["claude", "codex", "cursor", "gemini", "opencode", "copilot"];

/// The `--source` name for a wrapped agent, if its transcripts are readable.
///
/// The cursor family registers under several names but writes one store, so
/// they all map to the same source.
pub(crate) fn source_for(agent: &str) -> Option<&'static str> {
    let normalized = match agent {
        "cursor" | "cursor-agent" | "agent" => "cursor",
        other => other,
    };
    IMPORTABLE
        .iter()
        .copied()
        .find(|s| *s == normalized)
        // Checked against the importer's own list rather than trusted: if a
        // source is ever renamed there, this stops spawning a command that
        // would exit on "Unknown source" into a /dev/null stderr.
        .filter(|s| crate::memory::import::is_known_source(s))
}

/// True when journaling is switched on.
///
/// Shares the `journal` key with the REPL rather than adding a second switch:
/// somebody who turned journaling off meant it for the whole tool, and finding
/// a wrapper still summarising their session would be a nasty surprise.
pub(crate) fn enabled() -> bool {
    crate::runtime::journal()
}

/// True when the import has an LLM credential to work with.
///
/// `memory import` needs one to extract anything, and takes it from
/// `--credential`, `SIDEKAR_CREDENTIAL` or `config set credential`. With none
/// of those it exits on "no credential configured" — into a closed stderr, so
/// the only evidence is a process that ran and did nothing. Checking first
/// means the handoff stays a no-op on an unconfigured machine instead of
/// spawning a doomed process on every exit.
fn has_credential() -> bool {
    std::env::var("SIDEKAR_CREDENTIAL").is_ok_and(|v| !v.trim().is_empty())
        || !crate::config::config_get("credential").trim().is_empty()
}

/// Hand this session's transcript to `memory import` after the agent exits.
///
/// Detached and silent. The agent has already gone and the human has their
/// prompt back, so this must not print, must not block, and must not become a
/// reason an exit hangs.
pub(crate) fn spawn_after_exit(agent: &str, cwd: &str) {
    if !enabled() {
        return;
    }
    let Some(source) = source_for(agent) else {
        return;
    };
    if !has_credential() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };

    let mut child = Command::new(exe);
    child
        // --yes because nobody is here to answer: stdin is null, and without it
        // the import reaches its confirmation prompt and abandons the run.
        .args(["memory", "import", &format!("--source={source}"), "--yes"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Own session: the wrapper is exiting and its process group is about to be
    // torn down, which would take this with it.
    unsafe {
        use std::os::unix::process::CommandExt;
        child.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = child.spawn();
}

#[cfg(test)]
mod tests;
