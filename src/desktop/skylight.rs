//! SkyLight private SPI bridge for per-pid event delivery.
//!
//! Ported from CUA Driver's `SkyLightEventPost.swift` (MIT, trycua/cua).
//!
//! Two-layer story:
//!
//! 1. **Post path** — `SLEventPostToPid` wraps `SLEventPostToPSN` →
//!    `CGSTickleActivityMonitor` → `IOHIDPostEvent`. The public
//!    `CGEventPostToPid` skips the activity-monitor tickle, so events
//!    delivered through it don't register as "live input" to Chromium.
//!
//! 2. **Authentication** — on macOS 14+, WindowServer gates synthetic
//!    keyboard events against Chromium-like targets on an attached
//!    `SLSEventAuthenticationMessage`. Constructed per-event via ObjC
//!    class method, attached via `SLEventSetAuthenticationMessage`.
//!
//! All symbols resolved once at first use via `dlopen` + `dlsym`.
//! If anything fails to resolve, `post_to_pid` returns `false` and
//! callers fall back to the public `CGEventPostToPid`.

#![allow(non_upper_case_globals, non_camel_case_types)]

use std::ffi::c_void;
use std::os::raw::c_int;
use std::ptr;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// CGEvent FFI (public API, always available)
// ---------------------------------------------------------------------------

type CGEventRef = *mut c_void;

// ---------------------------------------------------------------------------
// SkyLight function pointer types
// ---------------------------------------------------------------------------

/// `void SLEventPostToPid(pid_t, CGEventRef)`
type SLEventPostToPidFn = unsafe extern "C" fn(pid: i32, event: CGEventRef);

/// `void SLEventSetAuthenticationMessage(CGEventRef, id)`
type SLEventSetAuthMessageFn = unsafe extern "C" fn(event: CGEventRef, msg: *mut c_void);

/// `void SLEventSetIntegerValueField(CGEventRef, CGEventField, int64_t)`
type SLEventSetIntFieldFn = unsafe extern "C" fn(event: CGEventRef, field: u32, value: i64);

/// `CGSConnectionID CGSMainConnectionID(void)`
type CGSMainConnectionIDFn = unsafe extern "C" fn() -> u32;

/// `void CGEventSetWindowLocation(CGEventRef, CGPoint)`
type CGEventSetWindowLocationFn = unsafe extern "C" fn(event: CGEventRef, point: CGPoint);

/// `OSStatus SLPSPostEventRecordTo(ProcessSerialNumber *psn, uint8_t *bytes)`
type SLPSPostEventRecordToFn = unsafe extern "C" fn(psn: *const u8, bytes: *const u8) -> i32;

/// `OSStatus _SLPSGetFrontProcess(ProcessSerialNumber *psn)`
type SLPSGetFrontProcessFn = unsafe extern "C" fn(psn: *mut u8) -> i32;

/// `OSStatus GetProcessForPID(pid_t, ProcessSerialNumber *)`
type GetProcessForPIDFn = unsafe extern "C" fn(pid: i32, psn: *mut u8) -> i32;

// ---------------------------------------------------------------------------
// ObjC runtime function pointers for auth message factory
// ---------------------------------------------------------------------------

/// `id objc_msgSend(Class, SEL, SLSEventRecord*, int32_t, uint32_t) -> id`
type ObjcMsgSendFactoryFn = unsafe extern "C" fn(
    cls: *mut c_void,
    sel: *mut c_void,
    record: *mut c_void,
    pid: i32,
    version: u32,
) -> *mut c_void;

unsafe extern "C" {
    fn dlopen(filename: *const u8, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const u8) -> *mut c_void;
    fn objc_getClass(name: *const u8) -> *mut c_void;
    fn sel_registerName(name: *const u8) -> *mut c_void;
}

const RTLD_LAZY: c_int = 1;
/// RTLD_DEFAULT — search all loaded images
const RTLD_DEFAULT: *mut c_void = (-2isize) as *mut c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

// ---------------------------------------------------------------------------
// Resolved handles — cached on first use
// ---------------------------------------------------------------------------

struct ResolvedCore {
    post_to_pid: SLEventPostToPidFn,
    set_auth_message: SLEventSetAuthMessageFn,
    msg_send_factory: ObjcMsgSendFactoryFn,
    message_class: *mut c_void,
    factory_selector: *mut c_void,
}

// SAFETY: The function pointers and ObjC class/selector refs are process-global
// and immutable after resolution. Safe to share across threads.
unsafe impl Send for ResolvedCore {}
unsafe impl Sync for ResolvedCore {}

struct ResolvedExtras {
    set_int_field: Option<SLEventSetIntFieldFn>,
    connection_id: Option<CGSMainConnectionIDFn>,
    set_window_location: Option<CGEventSetWindowLocationFn>,
    post_event_record_to: Option<SLPSPostEventRecordToFn>,
    get_front_process: Option<SLPSGetFrontProcessFn>,
    get_process_for_pid: Option<GetProcessForPIDFn>,
}

unsafe impl Send for ResolvedExtras {}
unsafe impl Sync for ResolvedExtras {}

struct Resolved {
    core: Option<ResolvedCore>,
    extras: ResolvedExtras,
}

fn ensure_frameworks_loaded() {
    unsafe {
        dlopen(
            b"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight\0".as_ptr(),
            RTLD_LAZY,
        );
        // Carbon needed for GetProcessForPID (PSN conversion)
        dlopen(
            b"/System/Library/Frameworks/Carbon.framework/Carbon\0".as_ptr(),
            RTLD_LAZY,
        );
    }
}

fn sym<T>(name: &[u8]) -> Option<T> {
    ensure_frameworks_loaded();
    let p = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
    if p.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute_copy(&p) })
    }
}

fn resolve_all() -> Resolved {
    let core = (|| {
        let post_to_pid: SLEventPostToPidFn = sym(b"SLEventPostToPid\0")?;
        let set_auth_message: SLEventSetAuthMessageFn =
            sym(b"SLEventSetAuthenticationMessage\0")?;
        let msg_send_factory: ObjcMsgSendFactoryFn = sym(b"objc_msgSend\0")?;

        let message_class =
            unsafe { objc_getClass(b"SLSEventAuthenticationMessage\0".as_ptr()) };
        if message_class.is_null() {
            return None;
        }

        let factory_selector = unsafe {
            sel_registerName(b"messageWithEventRecord:pid:version:\0".as_ptr())
        };
        if factory_selector.is_null() {
            return None;
        }

        Some(ResolvedCore {
            post_to_pid,
            set_auth_message,
            msg_send_factory,
            message_class,
            factory_selector,
        })
    })();

    let extras = ResolvedExtras {
        set_int_field: sym(b"SLEventSetIntegerValueField\0"),
        connection_id: sym(b"CGSMainConnectionID\0"),
        set_window_location: sym(b"CGEventSetWindowLocation\0"),
        post_event_record_to: sym(b"SLPSPostEventRecordTo\0"),
        get_front_process: sym(b"_SLPSGetFrontProcess\0"),
        get_process_for_pid: sym(b"GetProcessForPID\0"),
    };

    Resolved { core, extras }
}

fn resolved() -> &'static Resolved {
    static R: OnceLock<Resolved> = OnceLock::new();
    R.get_or_init(resolve_all)
}

// ---------------------------------------------------------------------------
// CGEvent record extraction
// ---------------------------------------------------------------------------

/// Extract the embedded `SLSEventRecord *` from a `CGEvent`. Layout:
/// `{CFRuntimeBase(16), uint32_t(4+4pad), SLSEventRecord*}` → offset 24.
/// Probe adjacent offsets for resilience across OS revisions.
unsafe fn extract_event_record(event: CGEventRef) -> *mut c_void {
    let base = event as *const u8;
    for offset in [24isize, 32, 16] {
        let slot = unsafe { base.offset(offset) as *const *mut c_void };
        let p = unsafe { *slot };
        if !p.is_null() {
            return p;
        }
    }
    ptr::null_mut()
}

// ---------------------------------------------------------------------------
// Raw dlsym for sibling modules that need ad-hoc symbol resolution
// ---------------------------------------------------------------------------

/// Resolve an arbitrary symbol name from all loaded images.
/// Null-terminated `name`. Returns null on failure.
pub fn dlsym_raw(name: &[u8]) -> *mut c_void {
    ensure_frameworks_loaded();
    unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// `true` when the auth-signed post path is fully resolved.
pub fn is_available() -> bool {
    resolved().core.is_some()
}

/// `true` when the focus-without-raise SPIs are all resolved.
pub fn is_focus_without_raise_available() -> bool {
    let e = &resolved().extras;
    e.get_front_process.is_some()
        && e.get_process_for_pid.is_some()
        && e.post_event_record_to.is_some()
}

/// `true` when `CGEventSetWindowLocation` resolved.
pub fn is_window_location_available() -> bool {
    resolved().extras.set_window_location.is_some()
}

/// Post `event` to `pid` via `SLEventPostToPid`.
///
/// `attach_auth_message`:
/// - `true` (keyboard path): attaches auth message so Chromium accepts
///   synthetic keyboard events as trusted input.
/// - `false` (mouse path): skips auth message so the event routes via
///   `IOHIDPostEvent` pipeline that Chromium's window event handler
///   subscribes to.
///
/// Returns `true` when the SPI resolved and the post was attempted.
pub fn post_to_pid(pid: i32, event: CGEventRef, attach_auth_message: bool) -> bool {
    let r = match &resolved().core {
        Some(r) => r,
        None => return false,
    };

    if attach_auth_message {
        let record = unsafe { extract_event_record(event) };
        if !record.is_null() {
            let msg = unsafe {
                (r.msg_send_factory)(
                    r.message_class,
                    r.factory_selector,
                    record,
                    pid,
                    0,
                )
            };
            if !msg.is_null() {
                unsafe { (r.set_auth_message)(event, msg) };
            }
            // On nil auth message we still attempt the post — the unsigned
            // path is valid on older OS releases.
        }
    }

    unsafe { (r.post_to_pid)(pid, event) };
    true
}

/// Stamp `value` onto `event` at the raw SkyLight field index `field`.
/// Returns `false` when the SPI didn't resolve.
pub fn set_integer_field(event: CGEventRef, field: u32, value: i64) -> bool {
    match resolved().extras.set_int_field {
        Some(f) => {
            unsafe { f(event, field, value) };
            true
        }
        None => false,
    }
}

/// SkyLight main connection ID for the current process.
pub fn main_connection_id() -> Option<u32> {
    resolved().extras.connection_id.map(|f| unsafe { f() })
}

/// Stamp a window-local point onto `event` via private
/// `CGEventSetWindowLocation` SPI.
pub fn set_window_location(event: CGEventRef, point: CGPoint) -> bool {
    match resolved().extras.set_window_location {
        Some(f) => {
            unsafe { f(event, point) };
            true
        }
        None => false,
    }
}

/// Copy the current frontmost process's PSN into `psn_buf` (8 bytes).
/// Returns `false` when the SPI isn't resolvable.
pub fn get_front_process(psn_buf: &mut [u8; 8]) -> bool {
    match resolved().extras.get_front_process {
        Some(f) => {
            let r = unsafe { f(psn_buf.as_mut_ptr()) };
            r == 0
        }
        None => false,
    }
}

/// Resolve `pid` to its PSN, writing 8 bytes into `psn_buf`.
pub fn get_process_psn(pid: i32, psn_buf: &mut [u8; 8]) -> bool {
    match resolved().extras.get_process_for_pid {
        Some(f) => {
            let r = unsafe { f(pid, psn_buf.as_mut_ptr()) };
            r == 0
        }
        None => false,
    }
}

/// Post a 248-byte synthetic event record via `SLPSPostEventRecordTo`.
/// Caller builds the buffer with the right defocus/focus marker and
/// target window id.
pub fn post_event_record_to(psn: &[u8; 8], bytes: &[u8; 0xF8]) -> bool {
    match resolved().extras.post_event_record_to {
        Some(f) => {
            let r = unsafe { f(psn.as_ptr(), bytes.as_ptr()) };
            r == 0
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// FocusWithoutRaise — ported from yabai / CUA Driver
// ---------------------------------------------------------------------------

/// Activate `target_pid`'s window `target_wid` without raising windows
/// or triggering Space follow.
///
/// Recipe (from yabai `window_manager_focus_window_without_raise`):
/// 1. `_SLPSGetFrontProcess(&prevPSN)` — capture current front.
/// 2. `GetProcessForPID(targetPid, &targetPSN)`.
/// 3. `SLPSPostEventRecordTo(prevPSN, buf)` with `buf[0x8a] = 0x02`
///    (defocus marker).
/// 4. `SLPSPostEventRecordTo(targetPSN, buf)` with `buf[0x8a] = 0x01`
///    (focus marker) and target window id at `buf[0x3c..0x3f]`.
///
/// Deliberately skips `SLPSSetFrontProcessWithOptions` — empirically
/// verified: skipping produces no raise, no Space follow, AND Chrome
/// still accepts subsequent synthetic clicks.
pub fn activate_without_raise(target_pid: i32, target_wid: u32) -> bool {
    if !is_focus_without_raise_available() {
        return false;
    }

    let mut prev_psn = [0u8; 8];
    let mut target_psn = [0u8; 8];

    if !get_front_process(&mut prev_psn) {
        return false;
    }
    if !get_process_psn(target_pid, &mut target_psn) {
        return false;
    }

    let mut buf = [0u8; 0xF8];
    buf[0x04] = 0xF8;
    buf[0x08] = 0x0D;
    // Little-endian CGWindowID at bytes 0x3c..0x3f
    buf[0x3C] = (target_wid & 0xFF) as u8;
    buf[0x3D] = ((target_wid >> 8) & 0xFF) as u8;
    buf[0x3E] = ((target_wid >> 16) & 0xFF) as u8;
    buf[0x3F] = ((target_wid >> 24) & 0xFF) as u8;

    // Defocus previous front
    buf[0x8A] = 0x02;
    let defocus_ok = post_event_record_to(&prev_psn, &buf);

    // Focus target
    buf[0x8A] = 0x01;
    let focus_ok = post_event_record_to(&target_psn, &buf);

    defocus_ok && focus_ok
}
