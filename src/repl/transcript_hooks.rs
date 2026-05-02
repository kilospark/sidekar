//! Side effects when persisted REPL transcript rows change outside `/compact`'s
//! inline replace (undo/prune share the same journal invalidation rule).

use anyhow::Result;

/// Clears `session_journals` for this session so entry-id bounds stay coherent.
pub(crate) fn on_transcript_mutation(session_id: &str) -> Result<()> {
    let deleted = crate::repl::journal::store::delete_all_journals_for_session(session_id)?;
    if deleted > 0 {
        crate::broker::try_log_event(
            "debug",
            "repl",
            &format!("cleared {deleted} session journal row(s) after transcript edit"),
            Some(session_id),
        );
    }
    Ok(())
}
