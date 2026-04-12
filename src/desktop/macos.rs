//! Pure Rust macOS desktop automation via Accessibility and AppKit FFI.
//!
//! Replaces the Swift bridge (native/macos-automation/) with direct unsafe calls
//! to ApplicationServices (AXUIElement) and AppKit (NSWorkspace/NSRunningApplication).

#![allow(
    non_upper_case_globals,
    clippy::missing_safety_doc,
    clippy::too_many_arguments,
    clippy::unnecessary_cast
)]

use crate::desktop::types::*;
use anyhow::{Result, bail};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, msg_send};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;
use std::ffi::c_void;
use std::path::Path;
use std::ptr;

// ---------------------------------------------------------------------------
// ApplicationServices / Accessibility FFI bindings
// ---------------------------------------------------------------------------

type AXUIElementRef = *const c_void;
type AXValueRef = *const c_void;

const AX_ERROR_SUCCESS: i32 = 0;

const kAXValueTypeCGPoint: i32 = 1;
const kAXValueTypeCGSize: i32 = 2;

// CGWindowList constants
const kCGWindowListOptionOnScreenOnly: u32 = 1 << 0;
const kCGWindowListExcludeDesktopElements: u32 = 1 << 4;
const kCGNullWindowID: u32 = 0;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

const kAXWindowsAttribute: &str = "AXWindows";
const kAXTitleAttribute: &str = "AXTitle";
const kAXRoleAttribute: &str = "AXRole";
const kAXValueAttribute: &str = "AXValue";
const kAXPositionAttribute: &str = "AXPosition";
const kAXSizeAttribute: &str = "AXSize";
const kAXMainAttribute: &str = "AXMain";
const kAXFocusedAttribute: &str = "AXFocused";
const kAXChildrenAttribute: &str = "AXChildren";
const kAXIdentifierAttribute: &str = "AXIdentifier";
const kAXPressAction: &str = "AXPress";

unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: core_foundation_sys::string::CFStringRef,
        value: *mut core_foundation_sys::base::CFTypeRef,
    ) -> i32;
    fn AXUIElementCopyActionNames(
        element: AXUIElementRef,
        names: *mut core_foundation_sys::array::CFArrayRef,
    ) -> i32;
    fn AXUIElementPerformAction(
        element: AXUIElementRef,
        action: core_foundation_sys::string::CFStringRef,
    ) -> i32;
    fn AXValueGetValue(value: AXValueRef, value_type: i32, value_ptr: *mut c_void) -> bool;

    fn CGWindowListCopyWindowInfo(
        option: u32,
        relative_to_window: u32,
    ) -> core_foundation_sys::array::CFArrayRef;

    fn CFRelease(cf: *const c_void);
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFArrayGetCount(array: core_foundation_sys::array::CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(
        array: core_foundation_sys::array::CFArrayRef,
        idx: isize,
    ) -> *const c_void;
}

// ---------------------------------------------------------------------------
// AX attribute helpers
// ---------------------------------------------------------------------------

fn ax_copy_attribute(
    element: AXUIElementRef,
    attr: &str,
) -> Option<core_foundation_sys::base::CFTypeRef> {
    let cf_attr = CFString::new(attr);
    let mut value: core_foundation_sys::base::CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, cf_attr.as_concrete_TypeRef(), &mut value)
    };
    if err == AX_ERROR_SUCCESS && !value.is_null() {
        Some(value)
    } else {
        None
    }
}

fn ax_string_attribute(element: AXUIElementRef, attr: &str) -> Option<String> {
    let value = ax_copy_attribute(element, attr)?;
    // Verify the value is actually a CFString before casting
    let type_id = unsafe { core_foundation_sys::base::CFGetTypeID(value) };
    let string_type_id = unsafe { core_foundation_sys::string::CFStringGetTypeID() };
    if type_id != string_type_id {
        // Not a string (could be CFNumber, CFBoolean, etc.) — release and skip
        unsafe { CFRelease(value as *const c_void) };
        return None;
    }
    let cf_str = unsafe {
        CFString::wrap_under_create_rule(value as core_foundation_sys::string::CFStringRef)
    };
    Some(cf_str.to_string())
}

fn ax_bool_attribute(element: AXUIElementRef, attr: &str) -> bool {
    let Some(value) = ax_copy_attribute(element, attr) else {
        return false;
    };
    let cf_type = unsafe { CFType::wrap_under_create_rule(value) };

    if cf_type.type_of() == CFBoolean::type_id() {
        // Re-wrap as get_rule since cf_type already owns it
        let b = unsafe {
            CFBoolean::wrap_under_get_rule(
                value as core_foundation_sys::base::CFTypeRef as *const _,
            )
        };
        return b == CFBoolean::true_value();
    }

    if cf_type.type_of() == CFNumber::type_id() {
        let n = unsafe {
            CFNumber::wrap_under_get_rule(
                value as core_foundation_sys::base::CFTypeRef
                    as core_foundation_sys::number::CFNumberRef,
            )
        };
        return n.to_i32() == Some(1);
    }

    false
}

fn ax_point_attribute(element: AXUIElementRef, attr: &str) -> Option<CGPoint> {
    let value = ax_copy_attribute(element, attr)?;
    let mut point = CGPoint::default();
    let ok = unsafe {
        AXValueGetValue(
            value as AXValueRef,
            kAXValueTypeCGPoint,
            &mut point as *mut CGPoint as *mut c_void,
        )
    };
    unsafe { CFRelease(value as *const c_void) };
    if ok { Some(point) } else { None }
}

fn ax_size_attribute(element: AXUIElementRef, attr: &str) -> Option<CGSize> {
    let value = ax_copy_attribute(element, attr)?;
    let mut size = CGSize::default();
    let ok = unsafe {
        AXValueGetValue(
            value as AXValueRef,
            kAXValueTypeCGSize,
            &mut size as *mut CGSize as *mut c_void,
        )
    };
    unsafe { CFRelease(value as *const c_void) };
    if ok { Some(size) } else { None }
}

/// Get the children AXUIElements of `element`. Returns empty vec on failure.
/// Each returned element is retained; caller is responsible for releasing via CFRelease.
fn ax_children(element: AXUIElementRef) -> Vec<AXUIElementRef> {
    let Some(value) = ax_copy_attribute(element, kAXChildrenAttribute) else {
        return vec![];
    };
    let array_ref = value as core_foundation_sys::array::CFArrayRef;
    let len = unsafe { CFArrayGetCount(array_ref) };
    let mut children = Vec::with_capacity(len as usize);
    for i in 0..len {
        let child = unsafe { CFArrayGetValueAtIndex(array_ref, i) };
        if !child.is_null() {
            // Retain because the array owns these references
            unsafe { CFRetain(child) };
            children.push(child as AXUIElementRef);
        }
    }
    // Release the array itself (returned by CopyAttributeValue)
    unsafe { CFRelease(value as *const c_void) };
    children
}

/// Get action names for an element.
fn ax_action_names(element: AXUIElementRef) -> Vec<String> {
    let mut names_ref: core_foundation_sys::array::CFArrayRef = ptr::null();
    let err = unsafe { AXUIElementCopyActionNames(element, &mut names_ref) };
    if err != AX_ERROR_SUCCESS || names_ref.is_null() {
        return vec![];
    }
    let len = unsafe { CFArrayGetCount(names_ref) };
    let mut actions = Vec::with_capacity(len as usize);
    for i in 0..len {
        let item = unsafe { CFArrayGetValueAtIndex(names_ref, i) };
        if !item.is_null() {
            let s = unsafe {
                CFString::wrap_under_get_rule(item as core_foundation_sys::string::CFStringRef)
            };
            actions.push(s.to_string());
        }
    }
    unsafe { CFRelease(names_ref as *const c_void) };
    actions
}

// ---------------------------------------------------------------------------
// frontmost_app_pid — NSWorkspace.shared.frontmostApplication
// ---------------------------------------------------------------------------

pub fn frontmost_app_pid() -> Option<i32> {
    unsafe {
        let workspace = NSWorkspace::sharedWorkspace();
        let app: Option<Retained<NSRunningApplication>> =
            msg_send![&workspace, frontmostApplication];
        app.map(|a| {
            let pid: i32 = msg_send![&a, processIdentifier];
            pid
        })
    }
}

// list_apps — NSWorkspace.shared.runningApplications
// ---------------------------------------------------------------------------

pub fn list_apps() -> Result<Vec<DesktopAppInfo>> {
    unsafe {
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = workspace.runningApplications();

        let mut result = Vec::new();
        let count = apps.count();
        for i in 0..count {
            let app: &NSRunningApplication = &apps.objectAtIndex(i);

            // activationPolicy == .regular (0)
            let policy: isize = msg_send![app, activationPolicy];
            if policy != 0 {
                continue;
            }

            let name: Option<Retained<NSString>> = msg_send![app, localizedName];
            let name = match name {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => continue,
            };

            let bundle_id: Option<Retained<NSString>> = msg_send![app, bundleIdentifier];
            let bundle_id = bundle_id.map(|b| b.to_string());

            let pid: i32 = msg_send![app, processIdentifier];
            let is_active: bool = msg_send![app, isActive];

            result.push(DesktopAppInfo {
                pid,
                bundle_id,
                name,
                is_active,
            });
        }
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// CGWindowList — get real window IDs for screencapture
// ---------------------------------------------------------------------------

/// Get CGWindowIDs for a given PID by querying CGWindowListCopyWindowInfo.
/// Returns a vec of (window_id, title, bounds) for matching windows.
fn cg_window_ids_for_pid(pid: i32) -> Vec<(u32, Option<String>)> {
    let mut result = Vec::new();
    let options = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
    let info_list = unsafe { CGWindowListCopyWindowInfo(options, kCGNullWindowID) };
    if info_list.is_null() {
        return result;
    }

    let count = unsafe { CFArrayGetCount(info_list) };
    let pid_key = CFString::new("kCGWindowOwnerPID");
    let id_key = CFString::new("kCGWindowNumber");
    let name_key = CFString::new("kCGWindowName");

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(info_list, i) };
        if dict.is_null() {
            continue;
        }
        let dict_ref = dict as core_foundation_sys::dictionary::CFDictionaryRef;

        // Check PID
        let mut pid_val: *const c_void = ptr::null();
        let has_pid = unsafe {
            core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
                dict_ref,
                pid_key.as_concrete_TypeRef() as *const c_void,
                &mut pid_val,
            )
        };
        if has_pid == 0 || pid_val.is_null() {
            continue;
        }
        let mut owner_pid: i64 = 0;
        let ok = unsafe {
            core_foundation_sys::number::CFNumberGetValue(
                pid_val as core_foundation_sys::number::CFNumberRef,
                core_foundation_sys::number::kCFNumberSInt64Type,
                &mut owner_pid as *mut i64 as *mut c_void,
            )
        };
        if !ok || owner_pid as i32 != pid {
            continue;
        }

        // Get window ID
        let mut id_val: *const c_void = ptr::null();
        let has_id = unsafe {
            core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
                dict_ref,
                id_key.as_concrete_TypeRef() as *const c_void,
                &mut id_val,
            )
        };
        if has_id == 0 || id_val.is_null() {
            continue;
        }
        let mut win_id: i64 = 0;
        let ok = unsafe {
            core_foundation_sys::number::CFNumberGetValue(
                id_val as core_foundation_sys::number::CFNumberRef,
                core_foundation_sys::number::kCFNumberSInt64Type,
                &mut win_id as *mut i64 as *mut c_void,
            )
        };
        if !ok {
            continue;
        }

        // Get window name (optional)
        let mut name_val: *const c_void = ptr::null();
        let has_name = unsafe {
            core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
                dict_ref,
                name_key.as_concrete_TypeRef() as *const c_void,
                &mut name_val,
            )
        };
        let name = if has_name != 0 && !name_val.is_null() {
            let type_id = unsafe {
                core_foundation_sys::base::CFGetTypeID(
                    name_val as core_foundation_sys::base::CFTypeRef,
                )
            };
            let string_type_id = unsafe { core_foundation_sys::string::CFStringGetTypeID() };
            if type_id == string_type_id {
                let cf_str = unsafe {
                    CFString::wrap_under_get_rule(
                        name_val as core_foundation_sys::string::CFStringRef,
                    )
                };
                Some(cf_str.to_string())
            } else {
                None
            }
        } else {
            None
        };

        result.push((win_id as u32, name));
    }

    unsafe { CFRelease(info_list as *const c_void) };
    result
}

// ---------------------------------------------------------------------------
// list_windows — AXUIElement windows for a pid
// ---------------------------------------------------------------------------

pub fn list_windows(pid: i32) -> Result<Vec<DesktopWindowInfo>> {
    let app_element = unsafe { AXUIElementCreateApplication(pid) };

    let cf_attr = CFString::new(kAXWindowsAttribute);
    let mut windows_ref: core_foundation_sys::base::CFTypeRef = ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(app_element, cf_attr.as_concrete_TypeRef(), &mut windows_ref)
    };

    if err != AX_ERROR_SUCCESS || windows_ref.is_null() {
        unsafe { CFRelease(app_element as *const c_void) };
        return Ok(vec![]);
    }

    // Get real CGWindowIDs for this pid to match with AX windows
    let cg_windows = cg_window_ids_for_pid(pid);

    let array_ref = windows_ref as core_foundation_sys::array::CFArrayRef;
    let count = unsafe { CFArrayGetCount(array_ref) };
    let mut result = Vec::with_capacity(count as usize);

    for i in 0..count {
        let window = unsafe { CFArrayGetValueAtIndex(array_ref, i) } as AXUIElementRef;

        let title = ax_string_attribute(window, kAXTitleAttribute);
        let position = ax_point_attribute(window, kAXPositionAttribute);
        let size = ax_size_attribute(window, kAXSizeAttribute);
        let is_main = ax_bool_attribute(window, kAXMainAttribute);
        let is_focused = ax_bool_attribute(window, kAXFocusedAttribute);

        let frame = DesktopRect {
            x: position.map(|p| p.x).unwrap_or(0.0),
            y: position.map(|p| p.y).unwrap_or(0.0),
            width: size.map(|s| s.width).unwrap_or(0.0),
            height: size.map(|s| s.height).unwrap_or(0.0),
        };

        // Match CG window by title (best heuristic since AX doesn't expose CGWindowID)
        let window_id = cg_windows.iter().find_map(|(wid, name)| {
            if let (Some(ax_title), Some(cg_name)) = (title.as_deref(), name.as_deref()) {
                if ax_title == cg_name {
                    Some(*wid)
                } else {
                    None
                }
            } else {
                None
            }
        });

        result.push(DesktopWindowInfo {
            pid,
            window_id,
            title,
            frame,
            is_main,
            is_focused,
        });
    }

    unsafe {
        CFRelease(windows_ref as *const c_void);
        CFRelease(app_element as *const c_void);
    };
    Ok(result)
}

// ---------------------------------------------------------------------------
// find_elements — walk AX tree matching query
// ---------------------------------------------------------------------------

pub fn find_elements(pid: i32, query: &str) -> Result<Vec<DesktopElementMatch>> {
    let lower_query = query.to_lowercase();
    let mut matches = Vec::new();

    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    walk_tree(app_element, pid, &[], 0, 0, 12, &lower_query, &mut matches);
    unsafe { CFRelease(app_element as *const c_void) };

    Ok(matches)
}

fn walk_tree(
    element: AXUIElementRef,
    pid: i32,
    chain: &[DesktopElementStep],
    sibling_index: usize,
    depth: usize,
    max_depth: usize,
    query: &str,
    matches: &mut Vec<DesktopElementMatch>,
) {
    if depth >= max_depth || matches.len() >= 50 {
        return;
    }

    let role = ax_string_attribute(element, kAXRoleAttribute).unwrap_or_else(|| "AXUnknown".into());
    let title = ax_string_attribute(element, kAXTitleAttribute);
    let value = ax_string_attribute(element, kAXValueAttribute);
    let identifier = ax_string_attribute(element, kAXIdentifierAttribute);

    let step = DesktopElementStep {
        role: role.clone(),
        title: title.clone(),
        identifier: identifier.clone(),
        index: sibling_index,
    };

    let mut current_chain = chain.to_vec();
    current_chain.push(step);

    // Check if this element matches
    let searchables = [
        role.to_lowercase(),
        title.as_deref().unwrap_or("").to_lowercase(),
        value.as_deref().unwrap_or("").to_lowercase(),
        identifier.as_deref().unwrap_or("").to_lowercase(),
    ];

    if searchables.iter().any(|s| s.contains(query)) {
        let position = ax_point_attribute(element, kAXPositionAttribute);
        let size = ax_size_attribute(element, kAXSizeAttribute);

        let frame = match (position, size) {
            (Some(pos), Some(sz)) => Some(DesktopRect {
                x: pos.x,
                y: pos.y,
                width: sz.width,
                height: sz.height,
            }),
            _ => None,
        };

        let actions = ax_action_names(element);

        let path = DesktopElementPath {
            pid,
            chain: current_chain.clone(),
        };

        matches.push(DesktopElementMatch {
            path,
            role: role.clone(),
            title: title.clone(),
            value: value.map(|v| {
                if v.len() > 200 {
                    v[..200].to_string()
                } else {
                    v
                }
            }),
            frame,
            actions,
        });
    }

    // Recurse into children
    let children = ax_children(element);
    for (i, child) in children.iter().enumerate() {
        walk_tree(
            *child,
            pid,
            &current_chain,
            i,
            depth + 1,
            max_depth,
            query,
            matches,
        );
        unsafe { CFRelease(*child as *const c_void) };
    }
}

// ---------------------------------------------------------------------------
// click_element — find + AXPress (or return coordinates for fallback)
// ---------------------------------------------------------------------------

pub fn click_element(pid: i32, query: &str) -> Result<DesktopClickResult> {
    let matches = find_elements(pid, query)?;

    let first = match matches.first() {
        Some(m) => m,
        None => {
            return Ok(DesktopClickResult {
                kind: "notFound".into(),
                role: None,
                title: None,
                x: None,
                y: None,
            });
        }
    };

    // Try to resolve the element and AXPress it
    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    if let Some(element) = resolve_element(app_element, &first.path) {
        let actions = ax_action_names(element);
        if actions.iter().any(|a| a == kAXPressAction) {
            let cf_action = CFString::new(kAXPressAction);
            let err = unsafe { AXUIElementPerformAction(element, cf_action.as_concrete_TypeRef()) };
            unsafe {
                CFRelease(element as *const c_void);
                CFRelease(app_element as *const c_void);
            };
            if err == AX_ERROR_SUCCESS {
                return Ok(DesktopClickResult {
                    kind: "axPress".into(),
                    role: Some(first.role.clone()),
                    title: first.title.clone(),
                    x: None,
                    y: None,
                });
            }
        } else {
            unsafe { CFRelease(element as *const c_void) };
        }
    }
    unsafe { CFRelease(app_element as *const c_void) };

    // Fallback: return coordinates for coordinate click
    if let Some(ref frame) = first.frame {
        let center_x = frame.x + frame.width / 2.0;
        let center_y = frame.y + frame.height / 2.0;
        return Ok(DesktopClickResult {
            kind: "fallbackClick".into(),
            role: Some(first.role.clone()),
            title: first.title.clone(),
            x: Some(center_x),
            y: Some(center_y),
        });
    }

    Ok(DesktopClickResult {
        kind: "noFrame".into(),
        role: Some(first.role.clone()),
        title: first.title.clone(),
        x: None,
        y: None,
    })
}

/// Walk the AX tree following a DesktopElementPath to find the actual element.
/// Returns a retained AXUIElementRef that the caller must release, or None.
fn resolve_element(root: AXUIElementRef, path: &DesktopElementPath) -> Option<AXUIElementRef> {
    let mut current = root;
    // Retain root so we can release `current` uniformly in the loop
    unsafe { CFRetain(current as *const c_void) };

    for step in &path.chain {
        let children = ax_children(current);
        // Release the previous current (we retained it)
        unsafe { CFRelease(current as *const c_void) };

        // Try to find by role + title
        let mut found: Option<AXUIElementRef> = None;
        for child in &children {
            let role = ax_string_attribute(*child, kAXRoleAttribute).unwrap_or_default();
            let title = ax_string_attribute(*child, kAXTitleAttribute);

            if role == step.role && title == step.title {
                // Retain the match
                unsafe { CFRetain(*child as *const c_void) };
                found = Some(*child);
                break;
            }
        }

        // Fallback: use index
        if found.is_none() && step.index < children.len() {
            let child = children[step.index];
            unsafe { CFRetain(child as *const c_void) };
            found = Some(child);
        }

        // Release all children
        for child in &children {
            unsafe { CFRelease(*child as *const c_void) };
        }

        match found {
            Some(f) => current = f,
            None => return None,
        }
    }

    Some(current)
}

// ---------------------------------------------------------------------------
// launch_app — NSWorkspace
// ---------------------------------------------------------------------------

pub fn launch_app(name: &str) -> Result<()> {
    unsafe {
        let workspace = NSWorkspace::sharedWorkspace();

        // Try launchApplication: (deprecated but simple and works)
        let ns_name = NSString::from_str(name);
        let ok: bool = msg_send![&workspace, launchApplication: &*ns_name];
        if ok {
            return Ok(());
        }

        // Try common app paths
        for dir in &[
            "/Applications",
            "/System/Applications",
            "/Applications/Utilities",
        ] {
            let path_str = format!("{}/{}.app", dir, name);
            let path = Path::new(&path_str);
            if path.exists() {
                let ns_url_class = objc2::runtime::AnyClass::get(c"NSURL").unwrap();
                let ns_path = NSString::from_str(&path_str);
                let url: Option<Retained<AnyObject>> =
                    msg_send![ns_url_class, fileURLWithPath: &*ns_path];
                if let Some(url) = url {
                    let config_class =
                        objc2::runtime::AnyClass::get(c"NSWorkspaceOpenConfiguration").unwrap();
                    let config: Retained<AnyObject> = msg_send![config_class, configuration];
                    let _: () = msg_send![&workspace, openApplicationAtURL: &*url, configuration: &*config, completionHandler: ptr::null::<c_void>()];
                    return Ok(());
                }
            }
        }

        bail!("Failed to launch '{name}'. Check the app name or path.")
    }
}

// ---------------------------------------------------------------------------
// activate_app / quit_app — NSRunningApplication
// ---------------------------------------------------------------------------

pub fn activate_app(pid: i32) -> Result<()> {
    unsafe {
        let app: Option<Retained<NSRunningApplication>> = msg_send![
            NSRunningApplication::class(),
            runningApplicationWithProcessIdentifier: pid
        ];
        let Some(app) = app else {
            bail!("Failed to activate app (pid {pid})")
        };
        // NSApplicationActivationOptions: 0 = default (no special flags)
        let ok: bool = msg_send![&app, activateWithOptions: 0usize];
        if ok {
            Ok(())
        } else {
            bail!("Failed to activate app (pid {pid})")
        }
    }
}

pub fn quit_app(pid: i32) -> Result<()> {
    unsafe {
        let app: Option<Retained<NSRunningApplication>> = msg_send![
            NSRunningApplication::class(),
            runningApplicationWithProcessIdentifier: pid
        ];
        let Some(app) = app else {
            bail!("Failed to quit app (pid {pid})")
        };
        let ok: bool = msg_send![&app, terminate];
        if ok {
            Ok(())
        } else {
            bail!("Failed to quit app (pid {pid})")
        }
    }
}
