use super::*;

const TABS: &str = "\
[1490524604] Dashboard - Turnset
  https://turnset.ai/dashboard
[1490524640] APIs & Services – Google Cloud console
  https://console.cloud.google.com/apis/library/gmail.googleapis.com?project=p
[1490524641] Sign in - Google Accounts
  https://accounts.google.com/v3/signin
";

#[test]
fn the_right_tab_is_found_by_url() {
    let id = tab_id_for(
        TABS,
        "https://console.cloud.google.com/apis/library/gmail.googleapis.com?project=p",
    );
    assert_eq!(id.as_deref(), Some("1490524640"));
}

#[test]
fn a_rewritten_query_string_still_matches() {
    // The console rewrites its own URLs on load, so matching must ignore
    // everything after the '?'.
    let id = tab_id_for(
        TABS,
        "https://console.cloud.google.com/apis/library/gmail.googleapis.com?project=other&x=1",
    );
    assert_eq!(id.as_deref(), Some("1490524640"));
}

#[test]
fn a_url_that_is_not_open_yields_nothing() {
    assert_eq!(tab_id_for(TABS, "https://example.com/nope"), None);
}

#[test]
fn id_in_reads_only_numeric_tab_ids() {
    assert_eq!(id_in("[123] Title").as_deref(), Some("123"));
    assert_eq!(id_in("  [4567] Another"), Some("4567".to_string()));
    assert_eq!(id_in("no brackets here"), None);
    assert_eq!(id_in("[not-a-number] Title"), None);
}

#[test]
fn an_enabled_api_is_recognised_by_its_disable_button() {
    // "Disable" is the reliable signal; "API enabled" only shows transiently
    // right after clicking.
    assert!(api_is_enabled("Gmail API\nDisable\nManage"));
    assert!(api_is_enabled("API enabled"));
    assert!(!api_is_enabled("Gmail API\nEnable\nLearn more"));
}

#[test]
fn an_unconfigured_project_is_detected() {
    assert!(needs_consent_screen(
        "Google Auth Platform not configured yet\nGet started"
    ));
    assert!(!needs_consent_screen("OAuth Overview\nMetrics\nClients"));
}

#[test]
fn an_internal_app_is_caught_before_it_becomes_a_403() {
    // Internal refuses every outside address; finding out via org_internal
    // later is the failure this check exists to prevent.
    assert!(is_internal("Publishing status\nInternal\nMake external"));
    assert!(!is_internal("Publishing status\nTesting\nPublish app"));
}

#[test]
fn every_api_the_commands_need_is_listed() {
    assert_eq!(REQUIRED_APIS.len(), 5);
    for a in ["gmail", "drive", "calendar-json", "sheets", "docs"] {
        assert!(REQUIRED_APIS.contains(&a), "missing {a}");
    }
}

#[test]
fn console_urls_carry_the_project() {
    assert!(consent_url("p").ends_with("project=p"));
    assert!(audience_url("p").ends_with("project=p"));
    assert!(create_client_url("p").ends_with("project=p"));
    assert!(api_url("p", "gmail").contains("gmail.googleapis.com"));
    assert!(api_url("p", "gmail").ends_with("project=p"));
}

#[test]
fn the_client_id_is_found_in_the_creation_dialog() {
    let page = "OAuth client created\nClient ID\n912445963142-tn7ngoe3jf5j13bi8esia7v8c366vt2c.apps.googleusercontent.com\nClient secret";
    assert_eq!(
        find_client_id(page).as_deref(),
        Some("912445963142-tn7ngoe3jf5j13bi8esia7v8c366vt2c.apps.googleusercontent.com")
    );
}

#[test]
fn the_secret_is_found_while_it_is_still_shown() {
    // Google shows this exactly once; missing it here means creating another.
    let page = "Client secret\nGOCSPX-abc123DEF456ghi789\nDownload JSON";
    assert_eq!(
        find_client_secret(page).as_deref(),
        Some("GOCSPX-abc123DEF456ghi789")
    );
}

#[test]
fn a_masked_secret_is_not_mistaken_for_the_real_one() {
    // The client detail page shows "****GWpR" rather than the value; treating
    // that as success would store a secret that cannot authenticate.
    let page = "Client secret\n****GWpR\nCreation date";
    assert_eq!(find_client_secret(page), None);
}

#[test]
fn a_page_without_credentials_yields_neither() {
    let page = "Create OAuth client ID\nApplication type\nDesktop app";
    assert_eq!(find_client_id(page), None);
    assert_eq!(find_client_secret(page), None);
}

#[test]
fn the_application_type_combobox_is_the_last_one() {
    // The console header carries a search combobox; the form's is further down.
    let tree = "[5] combobox: \n[6] input: Enter query to search\n[37] combobox: ";
    assert_eq!(last_combobox_ref(tree).as_deref(), Some("37"));
    assert_eq!(last_combobox_ref("no comboboxes"), None);
}
