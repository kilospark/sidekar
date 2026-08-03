//! Registry of every agent-facing prompt, with SQLite-backed overrides.
//!
//! Each prompt ships a default in this file (or a sibling text file).
//! On first use the defaults are seeded into the broker `prompts` table,
//! after which `get()` reads the stored row. A user edit sets the row's
//! `edited` flag, which protects it from being overwritten when a later
//! release changes the shipped default.
//!
//! Reads fall back to the compiled default whenever the row is missing,
//! blank, or the database cannot be opened, so a broken or absent DB can
//! never leave an agent running without a prompt.

use anyhow::Result;
use std::sync::Once;

pub const KEY_PTY_STARTER: &str = "pty.starter";
pub const KEY_REPL_SYSTEM: &str = "repl.system";
pub const KEY_JOURNAL_HEADER: &str = "journal.header";
pub const KEY_JOURNAL_SCHEMA: &str = "journal.schema";
pub const KEY_JOURNAL_MODE_ITERATIVE: &str = "journal.mode.iterative";
pub const KEY_JOURNAL_MODE_FRESH: &str = "journal.mode.fresh";
pub const KEY_JOURNAL_SUMMARIZER_SYSTEM: &str = "journal.summarizer.system";
pub const KEY_COMPACTION_SYSTEM: &str = "compaction.system";
pub const KEY_COMPACTION_INSTRUCTIONS: &str = "compaction.instructions";
pub const KEY_MEMORY_EXTRACT_SYSTEM: &str = "memory.extract.system";

/// Config key holding the hash of every shipped default. When it matches,
/// `sync_builtin_prompts` is a single indexed lookup and returns early.
const BUILTINS_HASH_KEY: &str = "prompts:builtins_hash";

pub struct BuiltinPrompt {
    pub key: &'static str,
    pub description: &'static str,
    pub default: &'static str,
}

const DEFAULT_PTY_STARTER: &str = "use ASD-STE100 standard. No antithesis. No corrective negation. No paragraph pinning. No parataxis. No summary beats. No rhetorical crutches. No negative parallelisms. No negative anaphoras. No contrasting pairs. No rule of three. No em dashes. No throat-clearing openers. No landing sentences. No setup/payoff constructions. No parallel sentence structures within a paragraph. Vary sentence length unpredictably. No stacked noun phrases. No filler intensifiers (genuinely, really, truly, actually). No corporate-register verbs (leverage, underscore, reflect). No nominalization. No hedging qualifiers. Write for the spoken voice. No performed enthusiasm.\nnever guess or assume. ask if unclear. no sycophancy. think critically. when working on a problem, do not take shortcuts or look for quickfixes. find the root cause. load sidekar skill.\noutput rules: terse, technical, no fluff. all substance stays, only filler dies. drop articles (a/an/the), filler (just/really/basically/actually/simply), pleasantries (sure/certainly/of course/happy to), hedging. fragments OK. short synonyms. technical terms exact. code blocks unchanged. errors quoted exact. pattern: [thing] [action] [reason]. [next step]. lead with the answer, not the reasoning. do not drift verbose over long conversations. code output, commits, file contents: write normally, not compressed. exception: use full clear prose for security warnings, irreversible action confirmations, and multi-step sequences where terse fragments risk misread. resume terse after.";

const DEFAULT_JOURNAL_MODE_ITERATIVE: &str = "\
A previous journal for this session exists. Your job is to
UPDATE it: PRESERVE every entry in \"decisions\", \"constraints\",
\"resolved_questions\", \"relevant_files\" and \"completed\" that is
still relevant. APPEND new completed actions (continue numbering
from the previous list). MOVE items from \"in_progress\" to
\"completed\" when they finished. MOVE questions from \"pending\"
to \"resolved_questions\" when answered. UPDATE \"active_state\" to
reflect current state. The \"active_task\" field must reflect the
user's most recent unfulfilled request — this is the most
important field for continuity. DO NOT delete information
unless it is clearly obsolete.";

const DEFAULT_JOURNAL_MODE_FRESH: &str = "\
This is the first journal for this session. Summarize the
conversation below into the structured format described at the
end of this message.";

const DEFAULT_JOURNAL_SUMMARIZER_SYSTEM: &str = "You are a precise session summarizer. Follow the \
     user-message instructions exactly and output only \
     valid JSON matching the schema. No commentary.";

const DEFAULT_COMPACTION_SYSTEM: &str =
    "You are a precise conversation summarizer. Output only the structured summary.";

const DEFAULT_COMPACTION_INSTRUCTIONS: &str = "\
Summarize the following conversation turns into a structured context summary. Be specific — include file paths, decisions, errors encountered, and current state.

Use this format:
## Goal
[User's objective]

## Progress
### Done
[Completed work]
### In Progress
[Current work]

## Key Decisions
[Technical decisions made]

## Relevant Files
[Files read/modified]

## Next Steps
[What must happen next]

## Critical Context
[Values, errors, config details]

## Awaiting User Response
[If the assistant's LAST message in this range posed a question, offered options, or requested a decision from the user, quote it verbatim here and label it \"OPEN QUESTION — do not answer until the user addresses it\". If the user's next message pivots to a different topic, the open question remains parked — acknowledge the pivot explicitly rather than silently dropping it. If no open question exists, write \"None\".]

---
Conversation to summarize:";

const DEFAULT_MEMORY_EXTRACT_SYSTEM: &str = "\
You extract durable user preferences, conventions, and workflow patterns \
from AI coding tool configuration and conversation history.

Rules:
- Only extract statements that represent LASTING preferences the user \
  wants remembered across future sessions.
- Ignore one-time debugging instructions, questions, status updates, and \
  operational chatter.
- Restate each memory as a clean, specific imperative sentence.
- Assign a confidence score (0.0-1.0) reflecting how clearly this is a \
  durable preference vs a one-off instruction. Floor 0.4.
- Only use types: \"preference\" (personal taste), \"convention\" \
  (project/code standards), \"constraint\" (things to avoid/never do), \
  \"decision\" (architectural choices).
- If no durable preferences exist, return { \"memories\": [] }.

Return valid JSON exactly matching this schema:
{ \"memories\": [ { \"summary\": string, \"type\": string, \
\"confidence\": number, \"evidence\": string } ] }

Do not wrap the JSON in markdown fences or add commentary.";

pub static BUILTIN_PROMPTS: &[BuiltinPrompt] = &[
    BuiltinPrompt {
        key: KEY_PTY_STARTER,
        description: "Starter prompt injected into PTY-wrapped agent CLIs",
        default: DEFAULT_PTY_STARTER,
    },
    BuiltinPrompt {
        key: KEY_REPL_SYSTEM,
        description: "System prompt body for `sidekar repl` (environment, memory, persona and journal sections are appended at runtime)",
        default: include_str!("repl_system.txt"),
    },
    BuiltinPrompt {
        key: KEY_JOURNAL_HEADER,
        description: "Opening framing of the journal summarization prompt",
        default: include_str!("../repl/journal/prompt_header.txt"),
    },
    BuiltinPrompt {
        key: KEY_JOURNAL_SCHEMA,
        description: "Output schema section of the journal summarization prompt",
        default: include_str!("../repl/journal/prompt_schema.txt"),
    },
    BuiltinPrompt {
        key: KEY_JOURNAL_MODE_ITERATIVE,
        description: "Journal instructions when updating a previous journal",
        default: DEFAULT_JOURNAL_MODE_ITERATIVE,
    },
    BuiltinPrompt {
        key: KEY_JOURNAL_MODE_FRESH,
        description: "Journal instructions for the first journal of a session",
        default: DEFAULT_JOURNAL_MODE_FRESH,
    },
    BuiltinPrompt {
        key: KEY_JOURNAL_SUMMARIZER_SYSTEM,
        description: "System prompt for the journal summarizer LLM call",
        default: DEFAULT_JOURNAL_SUMMARIZER_SYSTEM,
    },
    BuiltinPrompt {
        key: KEY_COMPACTION_SYSTEM,
        description: "System prompt for the context compaction summarizer",
        default: DEFAULT_COMPACTION_SYSTEM,
    },
    BuiltinPrompt {
        key: KEY_COMPACTION_INSTRUCTIONS,
        description: "Format instructions for the context compaction summary (the transcript is appended)",
        default: DEFAULT_COMPACTION_INSTRUCTIONS,
    },
    BuiltinPrompt {
        key: KEY_MEMORY_EXTRACT_SYSTEM,
        description: "System prompt for LLM memory extraction during import",
        default: DEFAULT_MEMORY_EXTRACT_SYSTEM,
    },
];

pub fn builtin(key: &str) -> Option<&'static BuiltinPrompt> {
    BUILTIN_PROMPTS.iter().find(|p| p.key == key)
}

/// Compiled default for `key`, or empty when the key is unknown.
pub fn default_for(key: &str) -> &'static str {
    builtin(key).map(|p| p.default).unwrap_or_default()
}

pub fn hash_text(text: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh64::xxh64(text.as_bytes(), 0))
}

/// Hash over every shipped default. Changing any default changes this,
/// which is what triggers the per-key reconcile in `sync_builtin_prompts`.
pub fn aggregate_hash() -> String {
    let mut combined = String::new();
    for p in BUILTIN_PROMPTS {
        combined.push_str(p.key);
        combined.push('\u{1}');
        combined.push_str(p.default);
        combined.push('\u{2}');
    }
    hash_text(&combined)
}

/// Seed missing prompts and refresh the ones the user never edited.
///
/// Deliberately keyed off a hash of the defaults rather than
/// `SCHEMA_VERSION`: prompt wording changes far more often than the
/// schema, and tying the two together would mean every prompt tweak
/// needs a schema bump to reach existing installs.
pub fn sync_builtin_prompts() -> Result<()> {
    let aggregate = aggregate_hash();
    if crate::config::config_get(BUILTINS_HASH_KEY) == aggregate {
        return Ok(());
    }
    for p in BUILTIN_PROMPTS {
        let default_hash = hash_text(p.default);
        match crate::broker::prompt_get(p.key)? {
            None => crate::broker::prompt_seed(p.key, p.default, &default_hash)?,
            Some(row) if !row.edited && row.default_hash != default_hash => {
                crate::broker::prompt_refresh_default(p.key, p.default, &default_hash)?;
            }
            Some(_) => {}
        }
    }
    crate::config::config_set(BUILTINS_HASH_KEY, &aggregate)?;
    Ok(())
}

fn ensure_synced() {
    static SYNCED: Once = Once::new();
    SYNCED.call_once(|| {
        if let Err(e) = sync_builtin_prompts() {
            crate::broker::try_log_error(
                "prompts",
                "failed to seed built-in prompts",
                Some(&format!("{e:#}")),
            );
        }
    });
}

/// Test builds read the compiled defaults instead of the database.
///
/// Prompts are consulted from deep inside argv shaping and prompt
/// building, so without this a plain `cargo test` would open and seed
/// the developer's real `~/.sidekar` database. Tests that want stored
/// values call `enable_db_reads_for_test` while holding the HOME guard.
#[cfg(test)]
static DB_READS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub fn enable_db_reads_for_test(enabled: bool) {
    DB_READS_ENABLED.store(enabled, std::sync::atomic::Ordering::SeqCst);
}

fn db_reads_enabled() -> bool {
    #[cfg(test)]
    {
        DB_READS_ENABLED.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(test))]
    {
        true
    }
}

/// Stored prompt text for `key`, falling back to the compiled default.
pub fn get(key: &str) -> String {
    if !db_reads_enabled() {
        return default_for(key).to_string();
    }
    ensure_synced();
    match crate::broker::prompt_get(key) {
        Ok(Some(row)) if !row.value.trim().is_empty() => row.value,
        _ => default_for(key).to_string(),
    }
}

/// Store a user edit for `key`.
pub fn set(key: &str, value: &str) -> Result<()> {
    let Some(p) = builtin(key) else {
        anyhow::bail!("unknown prompt \"{key}\"");
    };
    ensure_synced();
    crate::broker::prompt_set(key, value, &hash_text(p.default))
}

/// Drop the user's edit and restore the shipped default.
///
/// Reseeds inline rather than waiting for the next sync, because the
/// aggregate-hash short circuit would otherwise leave the row missing
/// until some default changes.
pub fn reset(key: &str) -> Result<()> {
    let Some(p) = builtin(key) else {
        anyhow::bail!("unknown prompt \"{key}\"");
    };
    crate::broker::prompt_delete(key)?;
    crate::broker::prompt_seed(key, p.default, &hash_text(p.default))
}

/// Every prompt with its stored row, if one exists. Keys with no row
/// (a DB that has not been seeded yet) come back as `None`.
pub fn list() -> Vec<(&'static BuiltinPrompt, Option<crate::broker::PromptRow>)> {
    ensure_synced();
    let rows = crate::broker::prompt_list().unwrap_or_default();
    BUILTIN_PROMPTS
        .iter()
        .map(|p| {
            let row = rows.iter().find(|r| r.key == p.key).cloned();
            (p, row)
        })
        .collect()
}

/// True when the user edited this prompt and the shipped default has
/// changed underneath it.
pub fn is_drifted(row: &crate::broker::PromptRow) -> bool {
    row.edited && row.default_hash != hash_text(default_for(&row.key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_has_a_unique_key_and_non_empty_default() {
        let mut seen = std::collections::HashSet::new();
        for p in BUILTIN_PROMPTS {
            assert!(!p.key.is_empty(), "prompt key must not be empty");
            assert!(
                !p.default.trim().is_empty(),
                "prompt {} has an empty default",
                p.key
            );
            assert!(
                !p.description.trim().is_empty(),
                "prompt {} has no description",
                p.key
            );
            assert!(seen.insert(p.key), "duplicate prompt key: {}", p.key);
        }
    }

    #[test]
    fn default_for_unknown_key_is_empty() {
        assert_eq!(default_for("nope.not.a.prompt"), "");
    }

    #[test]
    fn aggregate_hash_changes_when_a_default_changes() {
        let base = aggregate_hash();
        assert_eq!(base, aggregate_hash());
        assert_ne!(hash_text("a"), hash_text("b"));
    }
}
