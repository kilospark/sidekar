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
pub(crate) const IMPORTABLE: &[&str] =
    &["claude", "codex", "cursor", "gemini", "opencode", "copilot"];

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

/// The credential the import would extract with, if it can be decided.
///
/// Checked before spawning rather than left to fail inside the child: the child
/// runs detached with stderr closed, so "no credential configured" there is
/// invisible. Knowing here means the wrapper can say so once instead.
pub(crate) fn credential() -> Option<String> {
    crate::config::background_credential()
}

/// Key under which the "journaling is off" notice records that it has fired.
///
/// Not a `CONFIG_KEYS` entry, so it stays out of `sidekar config list`: it is
/// bookkeeping, not a setting anybody should edit.
const NOTICE_SHOWN_KEY: &str = "_journal_handoff_notice_shown";

/// Say once, on the terminal, that wrapped-agent journaling is not running.
///
/// The alternative — what shipped first — is a machine that quietly never
/// learns anything and gives its owner no way to find out. Printing every time
/// would be nagging, so this fires once per machine and then stays quiet; the
/// durable answer is `sidekar journal status`.
fn note_journaling_is_off_once(agent: &str) {
    if source_for(agent).is_none() {
        return;
    }
    if crate::config::config_get(NOTICE_SHOWN_KEY) == "1" {
        return;
    }
    let stored = crate::providers::oauth::list_credentials();
    eprintln!(
        "\nsidekar: session journaling is off — {}.\n         \
         Turn it on with `sidekar config set credential <name>`, or see \
         `sidekar journal status`.\n         (said once; not again on this machine)",
        if stored.is_empty() {
            "no LLM credential is stored".to_string()
        } else {
            format!(
                "{} credentials are stored and none is set as the default",
                stored.len()
            )
        }
    );
    let _ = crate::config::config_set(NOTICE_SHOWN_KEY, "1");
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
    if credential().is_none() {
        note_journaling_is_off_once(agent);
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
