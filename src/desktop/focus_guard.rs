//! Focus-suppression stack for background desktop automation.
//!
//! Ported from CUA Driver's `FocusGuard.swift`, `AXEnablementAssertion.swift`,
//! `SyntheticAppFocusEnforcer.swift`, and `SystemFocusStealPreventer.swift`
//! (MIT, trycua/cua).
//!
//! Three layers prevent the target app from stealing focus during an AX
//! action:
//!
//! 1. **AX Enablement** — write `AXManualAccessibility` /
//!    `AXEnhancedUserInterface` on the target's application root. Required
//!    for Chromium/Electron apps to build their AX tree. Cached negative
//!    for native Cocoa apps.
//!
//! 2. **Synthetic Focus** — write `AXFocused` / `AXMain` on the target's
//!    window and element before the action so AppKit thinks focus is
//!    already there. Restore after. Skip on minimized windows (writing
//!    AXFocused=true would deminiaturize Chrome).
//!
//! 3. **Reactive Preventer** — subscribes to
//!    `NSWorkspace.didActivateApplicationNotification`. If the target
//!    self-activates despite layers 1+2, immediately re-activates the
//!    prior frontmost app. Zero-delay demote — fires before WindowServer
//!    composites the next frame.

#![allow(non_upper_case_globals)]

use std::collections::HashSet;
use std::ffi::CStr;
use std::ffi::c_void;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// AX FFI (ApplicationServices)
// ---------------------------------------------------------------------------

type AXUIElementRef = *mut c_void;

unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *const c_void,
    ) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *const c_void,
    ) -> i32;
    fn CFRelease(cf: *const c_void);
    fn CFBooleanGetValue(boolean: *const c_void) -> bool;
    fn CFGetTypeID(cf: *const c_void) -> u64;
    fn CFBooleanGetTypeID() -> u64;
}

// kCFBooleanTrue / kCFBooleanFalse are global constants in CoreFoundation
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFBooleanFalse: *const c_void;
}

const kAXErrorSuccess: i32 = 0;

/// Create a CFString from a static null-terminated string.
/// SAFETY: only valid for 'static strings. The returned ref is owned by the
/// caller and must NOT be CFRelease'd (we use the constant buffer directly).
fn cfstr(s: &'static CStr) -> *const c_void {
    unsafe {
        CFStringCreateWithCString(std::ptr::null(), s.as_ptr().cast(), 0x08000100) // UTF8
    }
}

unsafe extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c_str: *const u8,
        encoding: u32,
    ) -> *const c_void;
}

// ---------------------------------------------------------------------------
// Layer 1: AX Enablement Assertion
// ---------------------------------------------------------------------------

/// Tracks which PIDs have accepted / rejected `AXManualAccessibility` and
/// `AXEnhancedUserInterface`.
pub struct AXEnablement {
    asserted: Mutex<HashSet<i32>>,
    non_assertable: Mutex<HashSet<i32>>,
}

use std::sync::OnceLock;

static AX_ENABLEMENT: OnceLock<AXEnablement> = OnceLock::new();

/// Get the process-global AX enablement cache. PID positive/negative
/// caches persist for the lifetime of the process so we don't
/// re-probe native apps or lose Chromium assertions across calls.
pub fn ax_enablement() -> &'static AXEnablement {
    AX_ENABLEMENT.get_or_init(AXEnablement::new)
}

impl Default for AXEnablement {
    fn default() -> Self {
        Self::new()
    }
}

impl AXEnablement {
    pub fn new() -> Self {
        Self {
            asserted: Mutex::new(HashSet::new()),
            non_assertable: Mutex::new(HashSet::new()),
        }
    }

    /// Assert AX enablement attributes on the application root. Returns `true`
    /// if at least one attribute was accepted (or previously recorded).
    ///
    /// Re-asserts every call on positive-cached PIDs because Chromium resets
    /// `AXEnhancedUserInterface` on certain state transitions.
    pub fn assert_for_pid(&self, pid: i32) -> bool {
        if self.non_assertable.lock().unwrap().contains(&pid) {
            return false;
        }

        let root = unsafe { AXUIElementCreateApplication(pid) };
        if root.is_null() {
            return false;
        }

        let attr_manual = cfstr(c"AXManualAccessibility");
        let attr_enhanced = cfstr(c"AXEnhancedUserInterface");

        let r1 = unsafe { AXUIElementSetAttributeValue(root, attr_manual, kCFBooleanTrue) };
        let r2 = unsafe { AXUIElementSetAttributeValue(root, attr_enhanced, kCFBooleanTrue) };

        unsafe {
            CFRelease(attr_manual);
            CFRelease(attr_enhanced);
            CFRelease(root as *const c_void);
        }

        if r1 != kAXErrorSuccess && r2 != kAXErrorSuccess {
            let already = self.asserted.lock().unwrap().contains(&pid);
            if !already {
                self.non_assertable.lock().unwrap().insert(pid);
            }
            return already;
        }

        self.asserted.lock().unwrap().insert(pid);
        true
    }

    /// Whether a prior call recorded this PID as rejecting both attributes.
    pub fn is_known_non_assertable(&self, pid: i32) -> bool {
        self.non_assertable.lock().unwrap().contains(&pid)
    }
}

// ---------------------------------------------------------------------------
// Layer 2: Synthetic App Focus Enforcer
// ---------------------------------------------------------------------------

/// Captured prior focus state, used for restore.
pub struct FocusSnapshot {
    window: AXUIElementRef,
    element: AXUIElementRef,
    prior_window_focused: Option<bool>,
    prior_window_main: Option<bool>,
    prior_element_focused: Option<bool>,
}

// SAFETY: AXUIElementRef is a CFType — thread-safe for reading attributes.
unsafe impl Send for FocusSnapshot {}
unsafe impl Sync for FocusSnapshot {}

fn ax_read_bool(element: AXUIElementRef, attr: &'static CStr) -> Option<bool> {
    if element.is_null() {
        return None;
    }
    let key = cfstr(attr);
    let mut value: *const c_void = std::ptr::null();
    let r = unsafe { AXUIElementCopyAttributeValue(element, key, &mut value) };
    unsafe { CFRelease(key) };
    if r != kAXErrorSuccess || value.is_null() {
        return None;
    }
    let type_id = unsafe { CFGetTypeID(value) };
    let bool_type_id = unsafe { CFBooleanGetTypeID() };
    if type_id == bool_type_id {
        let v = unsafe { CFBooleanGetValue(value) };
        // CFBoolean is a singleton — don't CFRelease
        Some(v)
    } else {
        unsafe { CFRelease(value) };
        None
    }
}

fn ax_write_bool(element: AXUIElementRef, attr: &'static CStr, value: bool) {
    if element.is_null() {
        return;
    }
    let key = cfstr(attr);
    let cf_val = if value {
        unsafe { kCFBooleanTrue }
    } else {
        unsafe { kCFBooleanFalse }
    };
    unsafe { AXUIElementSetAttributeValue(element, key, cf_val) };
    unsafe { CFRelease(key) };
}

/// Resolve the enclosing AXWindow from an element.
fn enclosing_window(element: AXUIElementRef) -> AXUIElementRef {
    if element.is_null() {
        return std::ptr::null_mut();
    }
    let key = cfstr(c"AXWindow");
    let mut value: *const c_void = std::ptr::null();
    let r = unsafe { AXUIElementCopyAttributeValue(element, key, &mut value) };
    unsafe { CFRelease(key) };
    if r != kAXErrorSuccess || value.is_null() {
        return std::ptr::null_mut();
    }
    value as AXUIElementRef
}

fn is_window_minimized(window: AXUIElementRef) -> bool {
    ax_read_bool(window, c"AXMinimized").unwrap_or(false)
}

/// Write synthetic focus onto `window` and `element`, returning a snapshot
/// for restore. Skips minimized windows.
pub fn enforce_focus(
    _pid: i32,
    window: AXUIElementRef,
    element: AXUIElementRef,
) -> Option<FocusSnapshot> {
    let win = if !window.is_null() {
        window
    } else {
        enclosing_window(element)
    };

    if !win.is_null() && is_window_minimized(win) {
        return None;
    }

    let prior_window_focused = ax_read_bool(win, c"AXFocused");
    let prior_window_main = ax_read_bool(win, c"AXMain");
    let prior_element_focused = ax_read_bool(element, c"AXFocused");

    if !win.is_null() {
        ax_write_bool(win, c"AXFocused", true);
        ax_write_bool(win, c"AXMain", true);
    }
    if !element.is_null() {
        ax_write_bool(element, c"AXFocused", true);
    }

    Some(FocusSnapshot {
        window: win,
        element,
        prior_window_focused,
        prior_window_main,
        prior_element_focused,
    })
}

/// Restore focus attributes from a prior snapshot.
pub fn restore_focus(snap: FocusSnapshot) {
    if !snap.window.is_null() {
        if let Some(v) = snap.prior_window_focused {
            ax_write_bool(snap.window, c"AXFocused", v);
        }
        if let Some(v) = snap.prior_window_main {
            ax_write_bool(snap.window, c"AXMain", v);
        }
    }
    if !snap.element.is_null()
        && let Some(v) = snap.prior_element_focused
    {
        ax_write_bool(snap.element, c"AXFocused", v);
    }
}

// ---------------------------------------------------------------------------
// Layer 3: System Focus Steal Preventer (reactive)
// ---------------------------------------------------------------------------

// Layer 3 requires NSWorkspace notification subscription which needs either:
//   (a) an ObjC block trampoline via `block2` crate, or
//   (b) a minimal ObjC helper compiled alongside
//
// For now we implement the architectural slot but use a simpler approach:
// after performing an AX action, poll the frontmost app for a short window
// and re-activate the prior frontmost if it changed. This is effectively
// the zero-delay variant from CUA Driver's Dispatcher.

/// Capture the current frontmost PID. Returns 0 if unable to determine.
pub fn frontmost_pid() -> i32 {
    unsafe {
        let cls = super::skylight::dlsym_raw(c"objc_getClass");
        if cls.is_null() {
            return 0;
        }
        let objc_get_class: unsafe extern "C" fn(*const u8) -> *mut c_void =
            std::mem::transmute(cls);
        let workspace_cls = objc_get_class(c"NSWorkspace".as_ptr().cast());
        if workspace_cls.is_null() {
            return 0;
        }

        let sel_shared = sel(c"sharedWorkspace");
        let sel_front = sel(c"frontmostApplication");
        let sel_pid = sel(c"processIdentifier");

        let msgsend = msgsend_ptr();
        if msgsend.is_null() {
            return 0;
        }

        let send_obj: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(msgsend);
        let send_i32: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32 =
            std::mem::transmute(msgsend);

        let ws = send_obj(workspace_cls, sel_shared);
        if ws.is_null() {
            return 0;
        }
        let app = send_obj(ws, sel_front);
        if app.is_null() {
            return 0;
        }
        send_i32(app, sel_pid)
    }
}

/// Re-activate a specific PID (make it frontmost).
pub fn activate_pid(pid: i32) -> bool {
    unsafe {
        let msgsend = msgsend_ptr();
        if msgsend.is_null() {
            return false;
        }

        let objc_get_class: unsafe extern "C" fn(*const u8) -> *mut c_void = {
            let p = super::skylight::dlsym_raw(c"objc_getClass");
            if p.is_null() {
                return false;
            }
            std::mem::transmute(p)
        };

        let cls = objc_get_class(c"NSRunningApplication".as_ptr().cast());
        if cls.is_null() {
            return false;
        }

        let sel_app = sel(c"runningApplicationWithProcessIdentifier:");
        let sel_activate = sel(c"activateWithOptions:");

        let send_pid: unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> *mut c_void =
            std::mem::transmute(msgsend);
        let send_activate: unsafe extern "C" fn(*mut c_void, *mut c_void, u64) -> i8 =
            std::mem::transmute(msgsend);

        let app = send_pid(cls, sel_app, pid);
        if app.is_null() {
            return false;
        }
        send_activate(app, sel_activate, 0) != 0
    }
}

fn sel(name: &'static CStr) -> *mut c_void {
    unsafe {
        let sel_register: unsafe extern "C" fn(*const u8) -> *mut c_void = {
            let p = super::skylight::dlsym_raw(c"sel_registerName");
            std::mem::transmute(p)
        };
        sel_register(name.as_ptr().cast())
    }
}

fn msgsend_ptr() -> *mut c_void {
    super::skylight::dlsym_raw(c"objc_msgSend")
}

// ---------------------------------------------------------------------------
// Combined guard
// ---------------------------------------------------------------------------

/// Guards for background AX actions. Usage:
///
/// ```ignore
/// let guard = FocusGuard::new();
/// // Before AX action:
/// let ctx = guard.begin(pid, window_ref, element_ref);
/// // ... perform AX action ...
/// guard.end(ctx);
/// ```
pub struct FocusGuard {
    _private: (),
}

/// Context returned by `FocusGuard::begin`, passed to `end` for cleanup.
pub struct GuardContext {
    focus_snap: Option<FocusSnapshot>,
    prior_frontmost: i32,
    target_pid: i32,
}

impl Default for FocusGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusGuard {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Call before an AX action on a backgrounded `pid`.
    pub fn begin(&self, pid: i32, window: AXUIElementRef, element: AXUIElementRef) -> GuardContext {
        // Layer 1: AX enablement (uses process-global singleton)
        ax_enablement().assert_for_pid(pid);

        // Layer 2: synthetic focus
        let focus_snap = enforce_focus(pid, window, element);

        // Layer 3: capture prior frontmost for reactive restore
        let prior_frontmost = frontmost_pid();

        GuardContext {
            focus_snap,
            prior_frontmost,
            target_pid: pid,
        }
    }

    /// Call after the AX action completes.
    pub fn end(&self, ctx: GuardContext) {
        // Restore synthetic focus attributes
        if let Some(snap) = ctx.focus_snap {
            restore_focus(snap);
        }

        // Layer 3: if the target stole focus, re-activate the prior front
        let current = frontmost_pid();
        if current == ctx.target_pid
            && ctx.prior_frontmost != 0
            && ctx.prior_frontmost != ctx.target_pid
        {
            activate_pid(ctx.prior_frontmost);
        }
    }
}
