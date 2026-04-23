//! `sidekar journal <list|show>` — CLI surface for the REPL
//! journaling subsystem.
//!
//! Read-only. Writes happen exclusively from inside a live REPL
//! session (the background polling task); inspecting from the
//! CLI is useful for debugging, for sharing a specific journal
//! with a teammate, or for operators auditing what's been
//! auto-promoted.
//!
//! Deliberately omits `journal now`: forcing a journaling pass
//! from the CLI would require reconstructing a Provider from a
//! session's credential/model — the reconstruction itself isn't
//! complicated, but the resulting journal would belong to a
//! session that isn't currently being used, muddying the
//! provenance. For a fast turnaround on journaling changes, run
//! the REPL with `SIDEKAR_JOURNAL_IDLE_SECS=5` and a short
//! conversation.

use anyhow::{Result, bail};

use crate::AppContext;
use crate::out;
use crate::repl::journal::store::{self, JournalRow};

pub fn cmd_journal(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "" | "help" | "--help" | "-h" => {
            print_help(ctx);
            Ok(())
        }
        "list" => cmd_list(ctx, &args[1..]),
        "show" => cmd_show(ctx, &args[1..]),
        other => {
            bail!(
                "unknown journal subcommand: {other}. \
                 Use `sidekar journal` (no args) for help."
            );
        }
    }
}

fn print_help(ctx: &mut AppContext) {
    out!(ctx, "\x1b[1mUsage:\x1b[0m sidekar journal <list|show> [args]");
    out!(ctx, "");
    out!(
        ctx,
        "  list [N] [--project P]   Recent journals for current project"
    );
    out!(
        ctx,
        "                           (default N=10, --project overrides cwd)"
    );
    out!(ctx, "  show <id>                Full 12-section view of one journal");
    out!(ctx, "");
    out!(
        ctx,
        "\x1b[2mJournals are written from inside an active REPL session.\x1b[0m"
    );
    out!(
        ctx,
        "\x1b[2mToggle via: /journal on|off (slash) or SIDEKAR_JOURNAL env.\x1b[0m"
    );
}

fn cmd_list(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    // Parse optional N (default 10) and --project <name>. Positional
    // N comes before --project for consistency with other sidekar
    // CLIs that follow `cmd <N> [--flag=value]` shape.
    let mut n: usize = 10;
    let mut project: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--project" {
            project = Some(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
            continue;
        }
        if let Some(rest) = arg.strip_prefix("--project=") {
            project = Some(rest.to_string());
            i += 1;
            continue;
        }
        if let Ok(parsed) = arg.parse::<usize>() {
            n = parsed;
            i += 1;
            continue;
        }
        bail!("unknown argument to `journal list`: {arg}");
    }
    n = n.min(200); // Hard cap to keep the CLI render bounded.

    let project = project.unwrap_or_else(|| crate::scope::resolve_project_name(None));
    let rows = store::recent_for_project(&project, n)?;
    if rows.is_empty() {
        out!(ctx, "No journals yet for project `{project}`.");
        return Ok(());
    }

    let now = now_unix_secs();
    out!(
        ctx,
        "\x1b[1mJournals\x1b[0m \x1b[2m(project={project}, {n} most recent)\x1b[0m"
    );
    for r in rows {
        let age = crate::session::format_relative_age(r.created_at, now);
        let head = if r.headline.is_empty() {
            "(no headline)".to_string()
        } else {
            r.headline.clone()
        };
        // Show session prefix (first 8 chars) — enough to disambiguate
        // in a typical per-project journal list without filling the
        // row with the full UUID.
        let sid_short = r.session_id[..r.session_id.len().min(8)].to_string();
        out!(
            ctx,
            "  [{id:>5}] {age:>8}  {sid_short}  {head}",
            id = r.id
        );
    }
    Ok(())
}

fn cmd_show(ctx: &mut AppContext, args: &[String]) -> Result<()> {
    let id_arg = match args.first() {
        Some(s) => s,
        None => bail!("usage: sidekar journal show <id>"),
    };
    let id: i64 = id_arg
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid id: {id_arg}"))?;

    let row = match store::get_by_id(id)? {
        Some(r) => r,
        None => {
            out!(ctx, "No journal with id {id}.");
            return Ok(());
        }
    };
    out!(ctx, "{}", render_show(&row));
    Ok(())
}

fn render_show(row: &JournalRow) -> String {
    use std::fmt::Write;
    let outcome = crate::repl::journal::parse::parse_response(&row.structured_json);
    let j = outcome.journal;
    let now = now_unix_secs();
    let age = crate::session::format_relative_age(row.created_at, now);

    let mut out = String::with_capacity(1024);
    let _ = writeln!(
        out,
        "\x1b[1mJournal [{id}]\x1b[0m  {age}  project={p}  session={sid}",
        id = row.id,
        p = row.project,
        sid = &row.session_id[..row.session_id.len().min(8)],
    );
    let _ = writeln!(
        out,
        "\x1b[2mmodel={m}  cred={c}  tokens_in={ti}  tokens_out={to}\x1b[0m",
        m = row.model_used,
        c = row.cred_used,
        ti = row.tokens_in,
        to = row.tokens_out,
    );
    if outcome.was_degraded {
        let _ = writeln!(
            out,
            "\x1b[33m(parse degraded: {r})\x1b[0m",
            r = outcome.reason
        );
    }

    let emit_str = |out: &mut String, label: &str, v: &str| {
        let v = v.trim();
        if !v.is_empty() {
            let _ = writeln!(out, "\x1b[1m{label}:\x1b[0m {v}");
        }
    };
    let emit_list = |out: &mut String, label: &str, vs: &[String]| {
        if vs.iter().all(|v| v.trim().is_empty()) {
            return;
        }
        let _ = writeln!(out, "\x1b[1m{label}:\x1b[0m");
        for v in vs {
            let v = v.trim();
            if !v.is_empty() {
                let _ = writeln!(out, "  - {v}");
            }
        }
    };

    emit_str(&mut out, "Active task", &j.active_task);
    emit_str(&mut out, "Goal", &j.goal);
    emit_list(&mut out, "Constraints", &j.constraints);
    emit_list(&mut out, "Completed", &j.completed);
    emit_str(&mut out, "Active state", &j.active_state);
    emit_list(&mut out, "In progress", &j.in_progress);
    emit_list(&mut out, "Blocked", &j.blocked);
    emit_list(&mut out, "Decisions", &j.decisions);
    emit_list(&mut out, "Resolved questions", &j.resolved_questions);
    emit_list(&mut out, "Pending user asks", &j.pending_user_asks);
    emit_list(&mut out, "Relevant files", &j.relevant_files);
    emit_str(&mut out, "Critical context", &j.critical_context);
    out
}

fn now_unix_secs() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    // Keep the CLI tests thin — the store layer is already covered
    // by src/repl/journal/store.rs and parse by
    // src/repl/journal/parse.rs. Here we only want to verify the
    // formatter paths don't panic on realistic input and that empty
    // and missing cases produce sensible output strings.

    use super::*;
    use crate::repl::journal::parse::StructuredJournal;

    fn sample_row() -> JournalRow {
        JournalRow {
            id: 42,
            session_id: "session-abc-12345678".into(),
            project: "/tmp/proj".into(),
            created_at: 1_000.0,
            from_entry_id: "e-a".into(),
            to_entry_id: "e-b".into(),
            structured_json: serde_json::to_string(&StructuredJournal {
                active_task: "ship step 10".into(),
                goal: "land journaling".into(),
                constraints: vec!["use cargo test --lib".into()],
                completed: vec!["schema migration".into(), "store helpers".into()],
                ..Default::default()
            })
            .unwrap(),
            headline: "step 10 — cli + slash".into(),
            previous_id: None,
            model_used: "claude-opus-4".into(),
            cred_used: "anthropic-main".into(),
            tokens_in: 2048,
            tokens_out: 512,
        }
    }

    #[test]
    fn render_show_includes_header_and_all_non_empty_fields() {
        let r = sample_row();
        let out = render_show(&r);
        assert!(out.contains("Journal [42]"));
        assert!(out.contains("project=/tmp/proj"));
        // 8-char session prefix.
        assert!(out.contains("session=session-"));
        assert!(out.contains("claude-opus-4"));
        assert!(out.contains("Active task:"));
        assert!(out.contains("ship step 10"));
        assert!(out.contains("Constraints:"));
        assert!(out.contains("use cargo test --lib"));
        assert!(out.contains("Completed:"));
        assert!(out.contains("schema migration"));
    }

    #[test]
    fn render_show_skips_empty_fields() {
        let r = JournalRow {
            structured_json: serde_json::to_string(&StructuredJournal {
                active_task: "only thing".into(),
                ..Default::default()
            })
            .unwrap(),
            ..sample_row()
        };
        let out = render_show(&r);
        // Label is ANSI-bolded in the output, so we check the two
        // halves separately rather than a single substring (the
        // \x1b[0m escape sits between "Active task:" and " only
        // thing"). Both halves must be present, and the active
        // task value must follow.
        assert!(out.contains("Active task:"));
        assert!(out.contains("only thing"));
        // All the other labels must NOT appear — they'd be empty
        // filler. A future change that adds "(none)" placeholders
        // would break this test, which is the intent.
        assert!(!out.contains("Goal:"));
        assert!(!out.contains("Constraints:"));
        assert!(!out.contains("Decisions:"));
    }

    #[test]
    fn render_show_surfaces_degraded_parse() {
        let r = JournalRow {
            structured_json: "totally not json".into(),
            ..sample_row()
        };
        let out = render_show(&r);
        assert!(out.contains("parse degraded:"));
        // Critical context still renders — it's where the degraded
        // parser stashes raw text.
        assert!(out.contains("Critical context:"));
    }

    #[test]
    fn render_show_handles_short_session_id_without_panic() {
        // Guard against a regression in the &r.session_id[..8]
        // indexing if ids are ever shorter than 8 chars.
        let r = JournalRow {
            session_id: "abc".into(),
            ..sample_row()
        };
        let out = render_show(&r);
        assert!(out.contains("session=abc"));
    }
}
