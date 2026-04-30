//! Background-safe keyboard and mouse input via CGEvent + SkyLight SPI.
//!
//! Ported from CUA Driver's `KeyboardInput.swift` and `MouseInput.swift`
//! (MIT, trycua/cua).
//!
//! Key differences from the existing `input.rs` (which uses enigo):
//! - Keyboard events can target a specific PID without stealing focus.
//! - Mouse clicks are delivered per-pid via `SLEventPostToPid` +
//!   `CGEventPostToPid` — no cursor warp, no focus steal.
//! - Falls back gracefully when SkyLight SPIs aren't available.

#![allow(non_upper_case_globals)]

use super::skylight;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// CoreGraphics FFI
// ---------------------------------------------------------------------------

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

// CGEventType
#[allow(dead_code)]
const kCGEventKeyDown: u32 = 10;
#[allow(dead_code)]
const kCGEventKeyUp: u32 = 11;
const kCGEventLeftMouseDown: u32 = 1;
const kCGEventLeftMouseUp: u32 = 2;
const kCGEventRightMouseDown: u32 = 3;
const kCGEventRightMouseUp: u32 = 4;
const kCGEventOtherMouseDown: u32 = 25;
const kCGEventOtherMouseUp: u32 = 26;
const kCGEventMouseMoved: u32 = 5;

// CGEventField
const kCGMouseEventClickState: u32 = 1;
const kCGMouseEventButtonNumber: u32 = 3;
const kCGMouseEventSubtype: u32 = 7;
const kCGMouseEventWindowUnderMousePointer: u32 = 91;
const kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent: u32 = 92;

// CGEventFlags
const kCGEventFlagMaskCommand: u64 = 1 << 20;
const kCGEventFlagMaskShift: u64 = 1 << 17;
const kCGEventFlagMaskAlternate: u64 = 1 << 19;
const kCGEventFlagMaskControl: u64 = 1 << 18;
const kCGEventFlagMaskSecondaryFn: u64 = 1 << 23;

// CGMouseButton
const kCGMouseButtonLeft: u32 = 0;
const kCGMouseButtonRight: u32 = 1;
const kCGMouseButtonCenter: u32 = 2;

// CGEventTapLocation
const kCGHIDEventTap: u32 = 0;

// CGEventSourceStateID
const kCGEventSourceStateHIDSystemState: i32 = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventPostToPid(pid: i32, event: CGEventRef);
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventKeyboardSetUnicodeString(
        event: CGEventRef,
        string_length: u32,
        unicode_string: *const u16,
    );
    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: u32,
        mouse_cursor_position: CGPoint,
        mouse_button: u32,
    ) -> CGEventRef;
    fn CGEventSetLocation(event: CGEventRef, location: CGPoint);
    fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;

    fn CFRelease(cf: *const c_void);

    fn clock_gettime_nsec_np(clock_id: u32) -> u64;
}

const CLOCK_UPTIME_RAW: u32 = 8;

// ---------------------------------------------------------------------------
// Modifier helpers
// ---------------------------------------------------------------------------

fn modifier_mask(modifiers: &[&str]) -> u64 {
    let mut mask: u64 = 0;
    for raw in modifiers {
        match raw.to_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" => mask |= kCGEventFlagMaskCommand,
            "shift" => mask |= kCGEventFlagMaskShift,
            "option" | "alt" | "opt" => mask |= kCGEventFlagMaskAlternate,
            "ctrl" | "control" => mask |= kCGEventFlagMaskControl,
            "fn" | "function" => mask |= kCGEventFlagMaskSecondaryFn,
            _ => {}
        }
    }
    mask
}

const MODIFIER_NAMES: &[&str] = &[
    "cmd", "command", "meta", "super", "shift", "option", "alt", "opt", "ctrl", "control", "fn",
    "function",
];

fn is_modifier(name: &str) -> bool {
    MODIFIER_NAMES.contains(&name.to_lowercase().as_str())
}

// ---------------------------------------------------------------------------
// Virtual key codes (from Carbon HIToolbox Events.h)
// ---------------------------------------------------------------------------

fn virtual_key_code(name: &str) -> Option<u16> {
    static MAP: OnceLock<HashMap<&'static str, u16>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m = HashMap::new();
        // Named keys
        m.insert("return", 0x24);
        m.insert("enter", 0x24);
        m.insert("tab", 0x30);
        m.insert("space", 0x31);
        m.insert("delete", 0x33);
        m.insert("backspace", 0x33);
        m.insert("forwarddelete", 0x75);
        m.insert("del", 0x75);
        m.insert("escape", 0x35);
        m.insert("esc", 0x35);
        m.insert("left", 0x7B);
        m.insert("leftarrow", 0x7B);
        m.insert("right", 0x7C);
        m.insert("rightarrow", 0x7C);
        m.insert("down", 0x7D);
        m.insert("downarrow", 0x7D);
        m.insert("up", 0x7E);
        m.insert("uparrow", 0x7E);
        m.insert("home", 0x73);
        m.insert("end", 0x77);
        m.insert("pageup", 0x74);
        m.insert("pagedown", 0x79);
        m.insert("f1", 0x7A);
        m.insert("f2", 0x78);
        m.insert("f3", 0x63);
        m.insert("f4", 0x76);
        m.insert("f5", 0x60);
        m.insert("f6", 0x61);
        m.insert("f7", 0x62);
        m.insert("f8", 0x64);
        m.insert("f9", 0x65);
        m.insert("f10", 0x6D);
        m.insert("f11", 0x67);
        m.insert("f12", 0x6F);
        // Letters
        for (c, code) in [
            ('a', 0x00u16),
            ('b', 0x0B),
            ('c', 0x08),
            ('d', 0x02),
            ('e', 0x0E),
            ('f', 0x03),
            ('g', 0x05),
            ('h', 0x04),
            ('i', 0x22),
            ('j', 0x26),
            ('k', 0x28),
            ('l', 0x25),
            ('m', 0x2E),
            ('n', 0x2D),
            ('o', 0x1F),
            ('p', 0x23),
            ('q', 0x0C),
            ('r', 0x0F),
            ('s', 0x01),
            ('t', 0x11),
            ('u', 0x20),
            ('v', 0x09),
            ('w', 0x0D),
            ('x', 0x07),
            ('y', 0x10),
            ('z', 0x06),
        ] {
            m.insert(
                // Leak a &str for the static map — one-time cost, 26 entries
                Box::leak(String::from(c).into_boxed_str()),
                code,
            );
        }
        // Digits
        for (c, code) in [
            ('0', 0x1Du16),
            ('1', 0x12),
            ('2', 0x13),
            ('3', 0x14),
            ('4', 0x15),
            ('5', 0x17),
            ('6', 0x16),
            ('7', 0x1A),
            ('8', 0x1C),
            ('9', 0x19),
        ] {
            m.insert(Box::leak(String::from(c).into_boxed_str()), code);
        }
        m
    });

    let lower = name.to_lowercase();
    map.get(lower.as_str()).copied()
}

// ---------------------------------------------------------------------------
// Keyboard: per-pid key synthesis
// ---------------------------------------------------------------------------

fn send_key(code: u16, down: bool, flags: u64, pid: Option<i32>) -> Result<()> {
    let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, down) };
    if event.is_null() {
        bail!("failed to create CGEvent for key code={code} down={down}");
    }
    unsafe { CGEventSetFlags(event, flags) };

    if let Some(pid) = pid {
        // Prefer SkyLight — routes through CGSTickleActivityMonitor which
        // Chromium needs to promote the synthetic event.
        if !skylight::post_to_pid(pid, event, true) {
            unsafe { CGEventPostToPid(pid, event) };
        }
    } else {
        unsafe { CGEventPost(kCGHIDEventTap, event) };
    }

    unsafe { CFRelease(event as *const c_void) };
    Ok(())
}

/// Press and release a single key, optionally with modifiers, optionally
/// targeting a specific PID (background-safe). When `pid` is `None`, posts
/// to the system HID tap (frontmost app).
pub fn press_key(key: &str, modifiers: &[&str], pid: Option<i32>) -> Result<()> {
    let code = virtual_key_code(key).ok_or_else(|| anyhow::anyhow!("unknown key: {key}"))?;
    let flags = modifier_mask(modifiers);
    send_key(code, true, flags, pid)?;
    send_key(code, false, flags, pid)?;
    Ok(())
}

/// Press a key combination. Modifier names are separated from the final
/// key. E.g. `hotkey(&["cmd", "shift", "s"], Some(pid))`.
pub fn hotkey(keys: &[&str], pid: Option<i32>) -> Result<()> {
    let mut modifiers = Vec::new();
    let mut final_key: Option<&str> = None;
    for raw in keys {
        if is_modifier(raw) {
            modifiers.push(*raw);
        } else {
            final_key = Some(raw);
        }
    }
    let key = final_key.ok_or_else(|| anyhow::anyhow!("hotkey combo has no non-modifier key"))?;
    press_key(key, &modifiers, pid)
}

/// Type each character as a synthetic key-down + key-up pair whose Unicode
/// payload is set via `CGEventKeyboardSetUnicodeString`. Bypasses
/// virtual-key mapping — accents, symbols, and emoji all go through.
pub fn type_characters(text: &str, delay_ms: u32, pid: Option<i32>) -> Result<()> {
    let clamped = delay_ms.clamp(0, 200);
    for ch in text.chars() {
        let utf16: Vec<u16> = ch.encode_utf16(&mut [0u16; 2]).to_vec();
        for key_down in [true, false] {
            let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null_mut(), 0, key_down) };
            if event.is_null() {
                bail!("failed to create unicode key event for '{ch}'");
            }
            unsafe {
                CGEventKeyboardSetUnicodeString(event, utf16.len() as u32, utf16.as_ptr());
            }
            if let Some(pid) = pid {
                if !skylight::post_to_pid(pid, event, true) {
                    unsafe { CGEventPostToPid(pid, event) };
                }
            } else {
                unsafe { CGEventPost(kCGHIDEventTap, event) };
            }
            unsafe { CFRelease(event as *const c_void) };
        }
        if clamped > 0 {
            std::thread::sleep(std::time::Duration::from_millis(clamped as u64));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scroll: keyboard-based (wheel events dropped by Chromium via SkyLight)
// ---------------------------------------------------------------------------

/// Scroll direction.
#[derive(Debug, Clone, Copy)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Scroll granularity.
#[derive(Debug, Clone, Copy)]
pub enum ScrollGranularity {
    Line,
    Page,
}

/// Scroll via synthesized keystrokes — PageUp/PageDown for page, arrow
/// keys for line. Keyboard events route through the auth-signed SkyLight
/// path, so they reach backgrounded Chromium/WebKit windows.
///
/// CUA Driver tried `CGEventCreateScrollWheelEvent2` via SkyLight but
/// Chromium silently drops them — no Scroll-specific auth subclass exists
/// in SkyLight, so the factory falls back to the bare parent class which
/// renderers reject.
pub fn scroll(
    direction: ScrollDirection,
    granularity: ScrollGranularity,
    amount: u32,
    pid: Option<i32>,
) -> Result<()> {
    let key = match (direction, granularity) {
        (ScrollDirection::Up, ScrollGranularity::Line) => "up",
        (ScrollDirection::Down, ScrollGranularity::Line) => "down",
        (ScrollDirection::Left, ScrollGranularity::Line) => "left",
        (ScrollDirection::Right, ScrollGranularity::Line) => "right",
        (ScrollDirection::Up, ScrollGranularity::Page) => "pageup",
        (ScrollDirection::Down, ScrollGranularity::Page) => "pagedown",
        // No standard horizontal page scroll — fall back to arrow keys
        (ScrollDirection::Left, ScrollGranularity::Page) => "left",
        (ScrollDirection::Right, ScrollGranularity::Page) => "right",
    };
    let clamped = amount.clamp(1, 50);
    for _ in 0..clamped {
        press_key(key, &[], pid)?;
        if clamped > 1 {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mouse: per-pid click synthesis
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    fn cg_button(self) -> u32 {
        match self {
            MouseButton::Left => kCGMouseButtonLeft,
            MouseButton::Right => kCGMouseButtonRight,
            MouseButton::Middle => kCGMouseButtonCenter,
        }
    }

    fn down_type(self) -> u32 {
        match self {
            MouseButton::Left => kCGEventLeftMouseDown,
            MouseButton::Right => kCGEventRightMouseDown,
            MouseButton::Middle => kCGEventOtherMouseDown,
        }
    }

    fn up_type(self) -> u32 {
        match self {
            MouseButton::Left => kCGEventLeftMouseUp,
            MouseButton::Right => kCGEventRightMouseUp,
            MouseButton::Middle => kCGEventOtherMouseUp,
        }
    }
}

/// Click at `(x, y)` in the frontmost window via HID tap.
/// Convenience wrapper for browser OS-click and other foreground scenarios.
pub fn click_frontmost(x: f64, y: f64, button: MouseButton, count: u32) -> Result<()> {
    click_frontmost_via_hid_tap(x, y, button, count)
}

/// Click at `(x, y)` screen-point targeting `pid`.
///
/// For backgrounded targets: uses the auth-signed SkyLight recipe
/// (FocusWithoutRaise → mouseMoved → off-screen primer → real click).
/// No cursor warp, no focus steal.
///
/// For frontmost targets or when SkyLight isn't available: falls back
/// to `CGEventPost(.cghidEventTap)` which moves the real cursor.
pub fn click_at_pid(
    x: f64,
    y: f64,
    pid: i32,
    button: MouseButton,
    count: u32,
    window_id: Option<u32>,
) -> Result<()> {
    // Check if target is frontmost — if so, use HID tap (only route
    // that reaches OpenGL/GHOST viewports like Blender).
    if is_pid_frontmost(pid) {
        return click_frontmost_via_hid_tap(x, y, button, count);
    }

    // Background path: auth-signed recipe for left single/double click
    if button == MouseButton::Left && count <= 2 && skylight::is_available() {
        return click_via_auth_signed(x, y, pid, count, window_id);
    }

    // Fallback: dual-post via SkyLight + public CGEventPostToPid
    click_via_dual_post(x, y, pid, button, count)
}

unsafe extern "C" {
    fn objc_getClass(name: *const u8) -> *mut c_void;
    fn sel_registerName(name: *const u8) -> *mut c_void;
}

/// `id objc_msgSend(id, SEL, pid_t) -> id`
type ObjcMsgSendPidFn = unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> *mut c_void;
/// `BOOL objc_msgSend(id, SEL) -> BOOL`
type ObjcMsgSendBoolFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i8;

fn is_pid_frontmost(pid: i32) -> bool {
    unsafe {
        let cls = objc_getClass(c"NSRunningApplication".as_ptr().cast());
        if cls.is_null() {
            return false;
        }
        let sel_run_app =
            sel_registerName(c"runningApplicationWithProcessIdentifier:".as_ptr().cast());
        let sel_active = sel_registerName(c"isActive".as_ptr().cast());

        // Resolve objc_msgSend — it's always loaded
        let msgsend_ptr = super::skylight::dlsym_raw(c"objc_msgSend");
        if msgsend_ptr.is_null() {
            return false;
        }
        let msgsend_pid: ObjcMsgSendPidFn = std::mem::transmute(msgsend_ptr);
        let msgsend_bool: ObjcMsgSendBoolFn = std::mem::transmute(msgsend_ptr);

        let app = msgsend_pid(cls, sel_run_app, pid);
        if app.is_null() {
            return false;
        }
        msgsend_bool(app, sel_active) != 0
    }
}

/// Auth-signed click recipe for backgrounded targets.
/// See CUA Driver's `clickViaAuthSignedPost` for full rationale.
fn click_via_auth_signed(
    x: f64,
    y: f64,
    pid: i32,
    count: u32,
    window_id: Option<u32>,
) -> Result<()> {
    let click_pairs = count.clamp(1, 2);
    let wid = window_id.unwrap_or(0);
    let wid_i64 = wid as i64;

    // Step 1: activate without raise
    if wid != 0 {
        skylight::activate_without_raise(pid, wid);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let point = CGPoint { x, y };
    let off_screen = CGPoint { x: -1.0, y: -1.0 };

    // Helper: create CGEvent for mouse
    let make_event = |mouse_type: u32, loc: CGPoint, click_count: i64| -> Result<CGEventRef> {
        let event = unsafe {
            CGEventCreateMouseEvent(std::ptr::null_mut(), mouse_type, loc, kCGMouseButtonLeft)
        };
        if event.is_null() {
            bail!("failed to create mouse event type={mouse_type}");
        }
        unsafe {
            CGEventSetLocation(event, loc);
            CGEventSetIntegerValueField(event, kCGMouseEventButtonNumber, 0);
            CGEventSetIntegerValueField(event, kCGMouseEventSubtype, 3);
            CGEventSetIntegerValueField(event, kCGMouseEventClickState, click_count);
            if wid != 0 {
                CGEventSetIntegerValueField(event, kCGMouseEventWindowUnderMousePointer, wid_i64);
                CGEventSetIntegerValueField(
                    event,
                    kCGMouseEventWindowUnderMousePointerThatCanHandleThisEvent,
                    wid_i64,
                );
            }
        }
        // Stamp target pid via SkyLight field 40
        skylight::set_integer_field(event, 40, pid as i64);
        Ok(event)
    };

    let post = |event: CGEventRef| {
        unsafe {
            // Stamp timestamp
            let ts = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
            CGEventSetIntegerValueField(event, 14, ts as i64); // field 14 = timestamp approximation
        }
        skylight::post_to_pid(pid, event, false);
    };

    // Step 3: mouseMoved at target
    let move_event = make_event(kCGEventMouseMoved, point, 0)?;
    post(move_event);
    unsafe { CFRelease(move_event as *const c_void) };
    std::thread::sleep(std::time::Duration::from_micros(15_000));

    // Step 4: off-screen primer click
    let primer_down = make_event(kCGEventLeftMouseDown, off_screen, 1)?;
    let primer_up = make_event(kCGEventLeftMouseUp, off_screen, 1)?;
    post(primer_down);
    std::thread::sleep(std::time::Duration::from_micros(1_000));
    post(primer_up);
    unsafe {
        CFRelease(primer_down as *const c_void);
        CFRelease(primer_up as *const c_void);
    }
    std::thread::sleep(std::time::Duration::from_micros(100_000));

    // Step 5: real click pair(s)
    for pair_idx in 1..=click_pairs {
        let state = pair_idx as i64;
        let down = make_event(kCGEventLeftMouseDown, point, state)?;
        let up = make_event(kCGEventLeftMouseUp, point, state)?;
        post(down);
        std::thread::sleep(std::time::Duration::from_micros(1_000));
        post(up);
        unsafe {
            CFRelease(down as *const c_void);
            CFRelease(up as *const c_void);
        }
        if pair_idx < click_pairs {
            std::thread::sleep(std::time::Duration::from_micros(80_000));
        }
    }

    Ok(())
}

/// Frontmost target: use system HID tap (reaches OpenGL/GHOST viewports).
fn click_frontmost_via_hid_tap(x: f64, y: f64, button: MouseButton, count: u32) -> Result<()> {
    let clamped = count.clamp(1, 3);
    let point = CGPoint { x, y };
    let cg_button = button.cg_button();
    let src = unsafe { CGEventSourceCreate(kCGEventSourceStateHIDSystemState) };

    // Leading mouseMoved
    let move_ev = unsafe { CGEventCreateMouseEvent(src, kCGEventMouseMoved, point, cg_button) };
    if !move_ev.is_null() {
        unsafe { CGEventPost(kCGHIDEventTap, move_ev) };
        unsafe { CFRelease(move_ev as *const c_void) };
    }
    std::thread::sleep(std::time::Duration::from_millis(30));

    for click_idx in 1..=clamped {
        let down = unsafe { CGEventCreateMouseEvent(src, button.down_type(), point, cg_button) };
        let up = unsafe { CGEventCreateMouseEvent(src, button.up_type(), point, cg_button) };
        if down.is_null() || up.is_null() {
            if !down.is_null() {
                unsafe { CFRelease(down as *const c_void) };
            }
            if !up.is_null() {
                unsafe { CFRelease(up as *const c_void) };
            }
            bail!("failed to create mouse event for frontmost click");
        }
        unsafe {
            CGEventSetIntegerValueField(down, kCGMouseEventClickState, click_idx as i64);
            CGEventSetIntegerValueField(up, kCGMouseEventClickState, click_idx as i64);
            CGEventPost(kCGHIDEventTap, down);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        unsafe {
            CGEventPost(kCGHIDEventTap, up);
            CFRelease(down as *const c_void);
            CFRelease(up as *const c_void);
        }
        if click_idx < clamped {
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }

    if !src.is_null() {
        unsafe { CFRelease(src as *const c_void) };
    }
    Ok(())
}

/// Dual-post fallback: SkyLight + public CGEventPostToPid.
fn click_via_dual_post(x: f64, y: f64, pid: i32, button: MouseButton, count: u32) -> Result<()> {
    let clamped = count.clamp(1, 3);
    let point = CGPoint { x, y };
    let cg_button = button.cg_button();

    for click_idx in 1..=clamped {
        let down = unsafe {
            CGEventCreateMouseEvent(std::ptr::null_mut(), button.down_type(), point, cg_button)
        };
        let up = unsafe {
            CGEventCreateMouseEvent(std::ptr::null_mut(), button.up_type(), point, cg_button)
        };
        if down.is_null() || up.is_null() {
            if !down.is_null() {
                unsafe { CFRelease(down as *const c_void) };
            }
            if !up.is_null() {
                unsafe { CFRelease(up as *const c_void) };
            }
            bail!("failed to create mouse event");
        }
        unsafe {
            CGEventSetIntegerValueField(down, kCGMouseEventClickState, click_idx as i64);
            CGEventSetIntegerValueField(up, kCGMouseEventClickState, click_idx as i64);
        }

        // SkyLight path
        skylight::post_to_pid(pid, down, false);
        // Public pid-routed post
        unsafe { CGEventPostToPid(pid, down) };

        std::thread::sleep(std::time::Duration::from_millis(30));

        skylight::post_to_pid(pid, up, false);
        unsafe { CGEventPostToPid(pid, up) };

        unsafe {
            CFRelease(down as *const c_void);
            CFRelease(up as *const c_void);
        }

        if click_idx < clamped {
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }

    Ok(())
}
