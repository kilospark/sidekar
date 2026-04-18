//! Anti-detection scripts installed via CDP `Page.addScriptToEvaluateOnNewDocument`.
//!
//! Opt-in per session via `sidekar stealth on`. Each script returns an
//! `identifier` which is cached in session state so `prepare_cdp` can skip
//! re-registration on the same target.
//!
//! Scope: small, deterministic, no heuristics. Two patches:
//! - `navigator.webdriver` → `undefined` (most common headless tell)
//! - `UIEvent.prototype.sourceCapabilities` fakes a touchless capability for
//!   untrusted events only, so synthetic clicks don't expose `null`.
//!
//! Everything runs inside a try/catch so a failing patch never throws into
//! page code.

use anyhow::Result;
use serde_json::{Value, json};

use crate::cdp::CdpClient;

const NAVIGATOR_WEBDRIVER: &str = r#"
(() => {
  try {
    Object.defineProperty(Navigator.prototype, 'webdriver', {
      configurable: true,
      enumerable: true,
      get() { return undefined; }
    });
  } catch {}
})();
"#;

const SOURCE_CAPABILITIES_MOCK: &str = r#"
(() => {
  try {
    if (typeof UIEvent === 'undefined') return;
    const desc = Object.getOwnPropertyDescriptor(UIEvent.prototype, 'sourceCapabilities');
    if (!desc || !desc.get) return;
    const origGet = desc.get;
    Object.defineProperty(UIEvent.prototype, 'sourceCapabilities', {
      configurable: true,
      enumerable: desc.enumerable,
      get() {
        try {
          if (this.isTrusted) return origGet.call(this);
          if (typeof InputDeviceCapabilities === 'function') {
            return new InputDeviceCapabilities({ firesTouchEvents: false });
          }
          return origGet.call(this);
        } catch {
          return origGet.call(this);
        }
      }
    });
  } catch {}
})();
"#;

/// The list of scripts we install when stealth is enabled. Order matters only
/// if one depends on another — here they're independent.
pub fn stealth_scripts() -> &'static [(&'static str, &'static str)] {
    &[
        ("navigator_webdriver", NAVIGATOR_WEBDRIVER),
        ("source_capabilities_mock", SOURCE_CAPABILITIES_MOCK),
    ]
}

/// Install stealth scripts on the current CDP target. Returns the list of
/// CDP script identifiers registered in this call (empty if all scripts were
/// already installed).
pub async fn install_on_target(
    cdp: &mut CdpClient,
    already_installed: &[String],
) -> Result<Vec<String>> {
    let mut added = Vec::new();
    for (name, source) in stealth_scripts() {
        // Script names live in the identifier via a prefix we control, so we
        // can tell ours apart from identifiers CDP generates.
        let tag = format!("sidekar-stealth:{name}");
        if already_installed.iter().any(|id| id == &tag) {
            continue;
        }
        let resp: Value = cdp
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": source }),
            )
            .await?;
        if let Some(id) = resp.get("identifier").and_then(|v| v.as_str()) {
            added.push(format!("{tag}#{id}"));
        }
    }
    Ok(added)
}

/// Uninstall stealth scripts registered on the current target. Safe to call
/// even if none were installed.
pub async fn uninstall_from_target(cdp: &mut CdpClient, installed: &[String]) -> Result<()> {
    for tagged in installed {
        let Some(pos) = tagged.rfind('#') else {
            continue;
        };
        let id = &tagged[pos + 1..];
        let _ = cdp
            .send(
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({ "identifier": id }),
            )
            .await;
    }
    Ok(())
}
