//! Guards for the static site in `www/public`.
//!
//! The site has no framework and no build step, so the nav, the footer and the
//! theme variables are pasted into ten-odd files with nothing keeping them
//! equal. Both site bugs found on 2026-09-20 came from that: five public navs
//! had drifted into four different link sets, three of them losing the only
//! link into the app, and `modal.js` styled one of the two ways this site can
//! be light, so a confirm dialog painted dark-mode red onto a light background.
//!
//! Neither was hard to find by eye. Both were invisible to anything automatic,
//! which is what these tests change. They read the shipped files directly, so
//! they keep working however the pages are edited.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Pages that share the public header.
const PUBLIC_PAGES: &[&str] = &[
    "index.html",
    "docs.html",
    "extension.html",
    "privacy.html",
    "terms.html",
];

/// Pages that share the signed-in header.
const APP_PAGES: &[&str] = &[
    "dashboard.html",
    "sessions.html",
    "devices.html",
    "settings.html",
];

fn public_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("www/public")
}

fn read(page: &str) -> String {
    let path = public_dir().join(page);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
}

/// The `<nav>` element of a page.
fn nav_of(page: &str) -> String {
    let html = read(page);
    let start = html
        .find("<nav")
        .unwrap_or_else(|| panic!("{page} has no <nav>"));
    let end = html[start..]
        .find("</nav>")
        .unwrap_or_else(|| panic!("{page} has no </nav>"))
        + start;
    html[start..end].to_string()
}

/// Visible link labels in a page's nav, in order.
fn nav_labels(page: &str) -> Vec<String> {
    let nav = nav_of(page);
    let mut out = Vec::new();
    let mut rest = nav.as_str();
    while let Some(open) = rest.find("<a ") {
        rest = &rest[open..];
        let Some(gt) = rest.find('>') else { break };
        let Some(close) = rest.find("</a>") else { break };
        if close > gt {
            let text = rest[gt + 1..close].trim();
            // Skip the logo, whose anchor wraps images rather than a label.
            if !text.is_empty() && !text.contains('<') {
                out.push(text.to_string());
            }
        }
        rest = &rest[close + 4..];
    }
    out
}

#[test]
fn every_public_page_offers_the_same_navigation() {
    let mut by_page: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for page in PUBLIC_PAGES {
        by_page.insert(page, nav_labels(page));
    }
    let reference = by_page[PUBLIC_PAGES[0]].clone();
    for (page, labels) in &by_page {
        assert_eq!(
            labels, &reference,
            "\n{page} nav differs from {}.\n  {page}: {labels:?}\n  {}: {reference:?}\n\
             With no framework the nav is pasted into every page, so a change has to be \
             made in all of them.",
            PUBLIC_PAGES[0], PUBLIC_PAGES[0]
        );
    }
}

#[test]
fn every_app_page_offers_the_same_navigation() {
    let reference = nav_labels(APP_PAGES[0]);
    for page in APP_PAGES {
        assert_eq!(
            nav_labels(page),
            reference,
            "{page} nav differs from {}",
            APP_PAGES[0]
        );
    }
}

#[test]
fn public_pages_keep_a_way_into_the_app() {
    // extension, privacy and terms each lost this, leaving a visitor on those
    // pages with no Sign in or Dashboard link at all.
    for page in PUBLIC_PAGES {
        let nav = nav_of(page);
        assert!(
            nav.contains(r#"id="nav-auth-wrap""#),
            "{page} has no #nav-auth-wrap, so the shared JS cannot place Sign in / Dashboard"
        );
        assert!(
            nav.contains(r#"id="theme-toggle""#),
            "{page} has no #theme-toggle"
        );
    }
}

#[test]
fn legal_pages_are_reachable_from_every_footer() {
    // Privacy and Terms belong in the footer, not the header. This is what lets
    // the header stay short without the pages becoming unreachable.
    for page in PUBLIC_PAGES {
        let html = read(page);
        let start = html.find("<footer").unwrap_or_else(|| panic!("{page} has no footer"));
        let footer = &html[start..];
        assert!(footer.contains("/privacy"), "{page} footer omits Privacy");
        assert!(footer.contains("/terms"), "{page} footer omits Terms");
    }
}

#[test]
fn at_most_one_nav_item_is_marked_active() {
    for page in PUBLIC_PAGES.iter().chain(APP_PAGES) {
        let count = nav_of(page).matches("nav-cta").count();
        assert!(
            count <= 1,
            "{page} marks {count} nav items active; at most one can be the current page"
        );
    }
}

/// Anything styling itself per theme must handle all three states.
///
/// This site can be light two ways — `html.light` when someone chose it, and
/// `:root:not(.dark)` under a `prefers-color-scheme` media query when they did
/// not — and dark two ways for the same reason. Handling only the explicit
/// class leaves the default case painted in the wrong theme, which is exactly
/// how the confirm dialog's danger button ended up unreadable.
#[test]
fn theme_aware_files_cover_the_default_and_the_explicit_choice() {
    let dir = public_dir();
    let mut checked = 0;
    for sub in ["js", "css"] {
        let Ok(entries) = fs::read_dir(dir.join(sub)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let body = fs::read_to_string(&path).unwrap_or_default();
            if !body.contains("html.light") && !body.contains("html.dark") {
                continue;
            }
            checked += 1;
            assert!(
                body.contains("prefers-color-scheme"),
                "{} styles html.light or html.dark but never prefers-color-scheme, so the \
                 default theme — a system preference with no class set — keeps the other \
                 theme's colours.",
                path.display()
            );
        }
    }
    assert!(checked > 0, "found no theme-aware js/css to check; has the layout moved?");
}
