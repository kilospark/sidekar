//! Drive the Google Cloud console to stand up an OAuth client.
//!
//! Google exposes no public API for the consent screen or for creating OAuth
//! clients — deliberately, since both are the gate on access to user data. So
//! the only way an agent can do this unattended is the same way a person does:
//! through the console. Sidekar already drives a browser, so it drives its own.
//!
//! Every step verifies before moving on and names itself when it fails. A
//! walkthrough that half-completes and reports success would leave a project in
//! a state nobody can reason about, so a failed step stops the run and says
//! which console page to finish by hand.

use anyhow::{Context, Result, bail};
use std::process::Command;
use std::time::Duration;

/// APIs the Gmail, Drive, Calendar, Sheets and Docs commands need.
pub const REQUIRED_APIS: &[&str] = &["gmail", "drive", "calendar-json", "sheets", "docs"];

/// How long a console page gets to settle before we read it.
const SETTLE: Duration = Duration::from_secs(8);

pub struct Plan {
    pub project: String,
    pub account: String,
    pub app_name: String,
    pub token_key: String,
    pub client_id_key: String,
    pub client_secret_key: String,
}

/// Console URLs for a project, in the order the flow needs them.
pub fn consent_url(project: &str) -> String {
    format!("https://console.cloud.google.com/auth/overview?project={project}")
}

pub fn audience_url(project: &str) -> String {
    format!("https://console.cloud.google.com/auth/audience?project={project}")
}

pub fn api_url(project: &str, api: &str) -> String {
    format!("https://console.cloud.google.com/apis/library/{api}.googleapis.com?project={project}")
}

pub fn create_client_url(project: &str) -> String {
    format!("https://console.cloud.google.com/auth/clients/create?project={project}")
}

/// Run one `sidekar browser ext …` and hand back its stdout.
fn ext(args: &[&str]) -> Result<String> {
    let exe = std::env::current_exe()?;
    let out = Command::new(exe)
        .args(["browser", "ext"])
        .args(args)
        .output()
        .context("could not run sidekar browser ext")?;
    if !out.status.success() {
        bail!(
            "browser ext {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Open a fresh tab and return its id, so the run never touches a tab it did
/// not create.
pub fn open_tab(url: &str) -> Result<String> {
    ext(&["new-tab", url])?;
    std::thread::sleep(SETTLE);
    let tabs = ext(&["tabs"])?;
    tab_id_for(&tabs, url).ok_or_else(|| anyhow::anyhow!("opened {url} but could not find its tab"))
}

/// Find the tab showing `url`, matching on the part before any query string
/// because the console rewrites its own URLs on load.
pub(crate) fn tab_id_for(tabs_output: &str, url: &str) -> Option<String> {
    let needle = url.split('?').next().unwrap_or(url);
    let lines: Vec<&str> = tabs_output.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("http") && line.contains(needle) {
            // The id is on the heading line immediately above the URL line.
            for back in (0..i).rev() {
                if let Some(id) = id_in(lines[back]) {
                    return Some(id);
                }
            }
        }
    }
    None
}

pub(crate) fn id_in(line: &str) -> Option<String> {
    let start = line.find('[')?;
    let end = line[start..].find(']')? + start;
    let id = &line[start + 1..end];
    id.chars()
        .all(|c| c.is_ascii_digit())
        .then(|| id.to_string())
}

pub fn navigate(tab: &str, url: &str) -> Result<()> {
    ext(&["navigate", "--tab", tab, url])?;
    std::thread::sleep(SETTLE);
    Ok(())
}

pub fn read(tab: &str) -> Result<String> {
    ext(&["read", "--tab", tab])
}

pub fn click_text(tab: &str, text: &str) -> Result<()> {
    ext(&["click", "--tab", tab, &format!("text:{text}")])?;
    std::thread::sleep(Duration::from_secs(4));
    Ok(())
}

pub fn close_tab(tab: &str) {
    let _ = ext(&["close", tab]);
}

/// True when the API library page shows the API already on.
///
/// The button reads "Disable" once enabled, which is the only reliable signal:
/// the page says "API enabled" only transiently after you click.
pub(crate) fn api_is_enabled(page: &str) -> bool {
    let lower = page.to_lowercase();
    lower.contains("disable") || lower.contains("api enabled")
}

/// True when the consent screen has not been configured for this project.
pub(crate) fn needs_consent_screen(page: &str) -> bool {
    page.contains("Google Auth Platform not configured yet") || page.contains("Get started")
}

/// True when the audience page reports the app as Internal.
///
/// Internal refuses every address outside the organisation, which is exactly
/// what this flow exists to avoid, so it is worth catching rather than
/// discovering later as a 403.
pub(crate) fn is_internal(page: &str) -> bool {
    page.contains("Make external")
}

/// One line of progress, so a run that stops is legible about where.
pub type Progress<'a> = &'a mut dyn FnMut(&str);

/// Ensure the five APIs are on, enabling any that are not.
pub fn ensure_apis(tab: &str, project: &str, say: Progress<'_>) -> Result<()> {
    for api in REQUIRED_APIS {
        navigate(tab, &api_url(project, api))?;
        if api_is_enabled(&read(tab)?) {
            say(&format!("  {api}: already on"));
            continue;
        }
        click_text(tab, "Enable")?;
        std::thread::sleep(Duration::from_secs(8));
        if api_is_enabled(&read(tab)?) {
            say(&format!("  {api}: enabled"));
        } else {
            bail!(
                "could not enable the {api} API. Finish it at {} and re-run.",
                api_url(project, api)
            );
        }
    }
    Ok(())
}

/// Check the consent screen exists and is External, which is what lets an
/// address outside the organisation consent at all.
pub fn check_consent_screen(tab: &str, project: &str, say: Progress<'_>) -> Result<()> {
    navigate(tab, &consent_url(project))?;
    if needs_consent_screen(&read(tab)?) {
        bail!(
            "this project has no consent screen yet. Create one as External at {} \
             (App name, support email, then Audience: External), then re-run.",
            consent_url(project)
        );
    }
    navigate(tab, &audience_url(project))?;
    let page = read(tab)?;
    if is_internal(&page) {
        bail!(
            "this project's consent screen is Internal, which refuses every address outside \
             the organisation with 403 org_internal. Switch it to External at {} and re-run.",
            audience_url(project)
        );
    }
    say("  consent screen: External");
    if !page.contains("Test users") && !page.contains("Testing") {
        say(
            "  note: could not confirm Testing status; add the account under Test users if login is refused",
        );
    }
    Ok(())
}

/// Create a Desktop OAuth client and capture its credentials.
///
/// The secret is returned rather than stored here: Google shows it exactly once,
/// in the dialog that appears on create, and anything that navigates away before
/// reading it loses the secret for good.
pub fn create_client(tab: &str, project: &str, app_name: &str) -> Result<(String, String)> {
    navigate(tab, &create_client_url(project))?;
    // Application type is a combobox; open it and take the Desktop entry.
    let tree = ext(&["ax-tree", "--tab", tab])?;
    let combo = last_combobox_ref(&tree)
        .ok_or_else(|| anyhow::anyhow!("could not find the Application type control"))?;
    ext(&["click", "--tab", tab, &combo])?;
    std::thread::sleep(Duration::from_secs(3));
    click_text(tab, "Desktop app")?;

    let tree = ext(&["ax-tree", "--tab", tab])?;
    if let Some(name_ref) = first_empty_input_ref(&tree) {
        ext(&["type", "--tab", tab, &name_ref, app_name])?;
    }
    std::thread::sleep(Duration::from_secs(2));
    click_text(tab, "Create")?;

    // Poll: the dialog carries both values and is the only place the secret
    // ever appears.
    for _ in 0..10 {
        std::thread::sleep(Duration::from_secs(2));
        let page = read(tab)?;
        if let (Some(id), Some(secret)) = (find_client_id(&page), find_client_secret(&page)) {
            return Ok((id, secret));
        }
    }
    bail!(
        "the client was submitted but its id and secret did not appear. Check {} — if a client \
         exists without a secret you can add one, since Google will not show the original again.",
        create_client_url(project)
    )
}

pub(crate) fn find_client_id(page: &str) -> Option<String> {
    page.split_whitespace()
        .find(|w| w.ends_with(".apps.googleusercontent.com") && w.contains('-'))
        .map(|w| {
            w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-')
                .to_string()
        })
}

pub(crate) fn find_client_secret(page: &str) -> Option<String> {
    page.split_whitespace()
        .find(|w| w.starts_with("GOCSPX-") && w.len() > 15)
        .map(|w| {
            w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .to_string()
        })
}

pub(crate) fn last_combobox_ref(tree: &str) -> Option<String> {
    tree.lines()
        .filter(|l| l.contains("combobox"))
        .next_back()
        .and_then(id_in)
}

pub(crate) fn first_empty_input_ref(tree: &str) -> Option<String> {
    tree.lines()
        .find(|l| l.trim_end().ends_with("input:") || l.contains("] input: "))
        .filter(|l| !l.contains("search"))
        .and_then(id_in)
}

#[cfg(test)]
mod tests;
