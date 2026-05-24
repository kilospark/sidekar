//! Element-targeted interactions: resolve refs/queries, perform AX actions, set values,
//! snapshot trees, menu paths, and web-focus retry.

use super::focus_guard::FocusGuard;
use super::macos;
use super::refs;
use super::types::*;
use anyhow::{Context, Result, bail};
use std::ffi::c_void;
use std::ptr;

const kAXValueAttribute: &str = "AXValue";
const kAXPressAction: &str = "AXPress";
const kAXWebAreaRole: &str = "AXWebArea";

#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub pid: i32,
    pub path: DesktopElementPath,
    pub role: String,
    pub title: Option<String>,
}

pub fn resolve_target(pid: i32, target: &str) -> Result<ResolvedTarget> {
    if let Some(ref_id) = refs::parse_ref(target) {
        refs::load_refs();
        let refmap = refs::ref_map().lock().unwrap();
        let entry = refmap
            .get(ref_id)
            .with_context(|| format!("Ref {ref_id} not found. Run `sidekar desktop see` or `find`."))?;
        return Ok(ResolvedTarget {
            pid: entry.pid,
            path: entry.path.clone(),
            role: entry.role.clone(),
            title: entry.title.clone(),
        });
    }

    let matches = macos::find_elements(pid, target)?;
    let first = matches
        .first()
        .with_context(|| format!("No element found matching \"{target}\""))?;
    Ok(ResolvedTarget {
        pid,
        path: first.path.clone(),
        role: first.role.clone(),
        title: first.title.clone(),
    })
}

pub fn with_resolved_element<F, T>(target: &ResolvedTarget, f: F) -> Result<T>
where
    F: FnOnce(macos::AxElementRef) -> Result<T>,
{
    super::focus_guard::ax_enablement().assert_for_pid(target.pid);
    let app = macos::create_application_element(target.pid);
    let element = macos::resolve_element(app, &target.path)
        .ok_or_else(|| anyhow::anyhow!("Element no longer exists (stale ref/path)"))?;
    let result = f(element);
    macos::release_ax_element(element);
    macos::release_ax_element(app);
    result
}

pub fn perform_action_on_target(target: &ResolvedTarget, action: &str) -> Result<DesktopActionResult> {
    if action.trim().is_empty() {
        bail!("action name required");
    }
    with_resolved_element(target, |element| {
        let guard = FocusGuard::new();
        let ctx = guard.begin(target.pid, ptr::null_mut(), element as *mut c_void);
        let err = macos::ax_perform_action(element, action);
        guard.end(ctx);
        if err != macos::AX_ERROR_SUCCESS {
            bail!("AX action {action} failed (error {err})");
        }
        Ok(DesktopActionResult {
            action: action.to_string(),
            role: target.role.clone(),
            title: target.title.clone(),
            method: "axPerformAction".into(),
        })
    })
}

pub fn set_value_on_target(target: &ResolvedTarget, value: &str) -> Result<DesktopSetValueResult> {
    if target.role.to_ascii_lowercase().contains("secure") {
        bail!("refusing to set value on secure/password field; use `desktop type` instead");
    }
    with_resolved_element(target, |element| {
        if !macos::ax_is_value_settable(element) {
            bail!("AXValue is not settable on this element");
        }
        let old_value = macos::ax_string_attribute_pub(element, kAXValueAttribute);
        let err = macos::ax_set_value(element, value);
        if err != macos::AX_ERROR_SUCCESS {
            bail!("AXUIElementSetAttributeValue failed (error {err})");
        }
        Ok(DesktopSetValueResult {
            role: target.role.clone(),
            title: target.title.clone(),
            old_value,
            new_value: value.to_string(),
        })
    })
}

pub fn snapshot_interactive_elements(
    pid: i32,
    max_depth: usize,
    max_elements: usize,
) -> Result<Vec<DesktopElementMatch>> {
    super::focus_guard::ax_enablement().assert_for_pid(pid);
    let mut elements = macos::collect_interactive_elements(pid, max_depth, max_elements)?;
    if !elements.iter().any(|e| {
        e.role == "AXTextField" || e.role == "AXTextArea" || e.role == "AXComboBox"
    }) && macos::try_web_area_focus(pid)? {
        elements = macos::collect_interactive_elements(pid, max_depth, max_elements)?;
    }
    refs::assign_refs_for_pid(pid, &mut elements);
    Ok(elements)
}

pub fn click_menu_path(pid: i32, path: &str) -> Result<String> {
    let parts: Vec<String> = path
        .split('>')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        bail!("menu path required, e.g. \"File > New Window\"");
    }
    macos::click_menu_items(pid, &parts)
}
