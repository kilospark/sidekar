#[allow(unused_imports)]
use super::*;

// ---------------------------------------------------------------------------
// System prompt
// ---------------------------------------------------------------------------

/// Build the REPL system prompt. Optionally injects a reference
/// block of prior session journals for the given project. Pass
/// `None` to skip journal injection (e.g. tests, or contexts
/// where the scope hasn't been resolved yet).
///
/// The project identifier should be the same string that the
/// journaling task uses (`scope::resolve_project_name`) —
/// otherwise the injection lookup hits the wrong bucket.
pub(super) fn build_system_prompt_with_project(project: Option<&str>) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let today = chrono_lite_today();

    // Body comes from the `prompts` table (`repl.system`), which is
    // user-editable. The environment block is generated here rather than
    // stored, so an edited prompt can never lose the cwd/date anchor.
    let body = crate::prompts::get(crate::prompts::KEY_REPL_SYSTEM);
    let mut prompt = format!(
        "{}\n\n## Environment\n- Working directory: {cwd}\n- Date: {today}\n",
        body.trim_end()
    );

    // Inject project + global memory context (decisions, constraints, conventions, etc.)
    if let Ok(brief) = crate::memory::startup_brief(5) {
        let brief = brief.trim();
        if !brief.is_empty() {
            prompt.push_str("\n## Memory\n");
            prompt.push_str(brief);
            prompt.push('\n');
        }
    }

    // Persona from AGENTS.md in cwd (de-facto standard used by Codex, Cursor, etc.)
    if let Ok(persona) = std::fs::read_to_string("AGENTS.md") {
        let persona = persona.trim();
        if !persona.is_empty() {
            prompt.push_str("\n## Persona\n");
            prompt.push_str(persona);
            prompt.push('\n');
        }
    }

    // Journal injection: prior session journals for this project.
    // Gated on runtime::journal() — disabling journaling also hides
    // previously-collected journals from the system prompt, which
    // is what a user running `--no-journal` to reproduce a clean
    // baseline would expect. The block is appended last so that
    // user-authored AGENTS.md still leads; journals are
    // supplementary recall, not authority.
    if crate::runtime::journal()
        && let Some(p) = project
    {
        let block = crate::repl::journal::build_injection_block(p);
        if !block.is_empty() {
            prompt.push_str(&block);
        }
    }

    prompt
}

fn chrono_lite_today() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let mut y = 1970i64;
    let mut remaining = days as i64;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let months = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 1;
    for &days_in_month in &months {
        if remaining < days_in_month {
            break;
        }
        remaining -= days_in_month;
        m += 1;
    }
    format!("{y}-{m:02}-{:02}", remaining + 1)
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}
