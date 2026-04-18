//! Global mouse/keyboard monitor via CGEventTap.
//!
//! A dedicated thread owns the runloop, the tap callback pushes events into a
//! bounded ring buffer, and CLI commands read that buffer. Start is
//! idempotent; stop tears down the runloop and joins the thread.
//!
//! Requires Accessibility permission (same as AX tree walks). If unavailable,
//! `start` returns an error pointing the user at `sidekar desktop trust`.

#![cfg(target_os = "macos")]
#![allow(non_upper_case_globals, dead_code)]

use anyhow::{Result, bail};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::os::raw::c_int;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// CGEventTap / CFRunLoop FFI
// ---------------------------------------------------------------------------

type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFMachPortRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;

type CGEventMask = u64;
type CGEventType = u32;

const kCGEventLeftMouseDown: CGEventType = 1;
const kCGEventLeftMouseUp: CGEventType = 2;
const kCGEventRightMouseDown: CGEventType = 3;
const kCGEventRightMouseUp: CGEventType = 4;
const kCGEventMouseMoved: CGEventType = 5;
const kCGEventLeftMouseDragged: CGEventType = 6;
const kCGEventRightMouseDragged: CGEventType = 7;
const kCGEventKeyDown: CGEventType = 10;
const kCGEventKeyUp: CGEventType = 11;
const kCGEventScrollWheel: CGEventType = 22;
const kCGEventOtherMouseDown: CGEventType = 25;
const kCGEventOtherMouseUp: CGEventType = 26;

const kCGHeadInsertEventTap: u32 = 0;
const kCGSessionEventTap: u32 = 1;
const kCGEventTapOptionListenOnly: u32 = 1;

const kCGKeyboardEventKeycode: u32 = 9;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;

    fn CFMachPortCreateRunLoopSource(
        allocator: *mut c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;

    fn CFRunLoopAddSource(
        loop_ref: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: *const c_void,
    );

    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
    fn CFRunLoopStop(loop_ref: CFRunLoopRef);
    fn CFRelease(cf: *const c_void);
    fn CFRunLoopRemoveSource(
        loop_ref: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: *const c_void,
    );

    static kCFRunLoopCommonModes: *const c_void;
}

// ---------------------------------------------------------------------------
// Event record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MonitorEvent {
    pub t_ms: u64,
    pub seq: u64,
    pub kind: &'static str,
    pub x: f64,
    pub y: f64,
    pub keycode: i64,
}

impl MonitorEvent {
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "t": self.t_ms,
            "s": self.seq,
            "k": self.kind,
            "x": self.x,
            "y": self.y,
            "keycode": self.keycode,
        })
    }
}

// ---------------------------------------------------------------------------
// Monitor state
// ---------------------------------------------------------------------------

struct MonitorState {
    events: VecDeque<MonitorEvent>,
    cap: usize,
    next_seq: u64,
    total_received: u64,
    total_dropped: u64,
    runloop: Option<CFRunLoopRef>,
    thread_running: bool,
}

impl MonitorState {
    fn new(cap: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(cap.min(4096)),
            cap,
            next_seq: 0,
            total_received: 0,
            total_dropped: 0,
            runloop: None,
            thread_running: false,
        }
    }

    fn push(&mut self, kind: &'static str, x: f64, y: f64, keycode: i64) {
        if self.events.len() >= self.cap {
            self.events.pop_front();
            self.total_dropped += 1;
        }
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let seq = self.next_seq;
        self.next_seq = seq.saturating_add(1);
        self.events.push_back(MonitorEvent {
            t_ms: now,
            seq,
            kind,
            x,
            y,
            keycode,
        });
        self.total_received += 1;
    }
}

unsafe impl Send for MonitorHandle {}
unsafe impl Sync for MonitorHandle {}

/// Raw pointers from CoreFoundation are not naturally Send; wrapping them
/// here and keeping all access behind the global mutex makes it safe.
struct MonitorHandle {
    state: Arc<Mutex<MonitorState>>,
}

fn monitor() -> &'static MonitorHandle {
    static H: OnceLock<MonitorHandle> = OnceLock::new();
    H.get_or_init(|| MonitorHandle {
        state: Arc::new(Mutex::new(MonitorState::new(5000))),
    })
}

// ---------------------------------------------------------------------------
// Tap callback
// ---------------------------------------------------------------------------

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let state = unsafe { &*(user_info as *const Arc<Mutex<MonitorState>>) };
    let kind = match event_type {
        kCGEventLeftMouseDown => Some("mouse_down_left"),
        kCGEventLeftMouseUp => Some("mouse_up_left"),
        kCGEventRightMouseDown => Some("mouse_down_right"),
        kCGEventRightMouseUp => Some("mouse_up_right"),
        kCGEventOtherMouseDown => Some("mouse_down_other"),
        kCGEventOtherMouseUp => Some("mouse_up_other"),
        kCGEventLeftMouseDragged => Some("mouse_drag_left"),
        kCGEventRightMouseDragged => Some("mouse_drag_right"),
        kCGEventMouseMoved => Some("mouse_move"),
        kCGEventScrollWheel => Some("scroll"),
        kCGEventKeyDown => Some("key_down"),
        kCGEventKeyUp => Some("key_up"),
        _ => None,
    };
    if let Some(kind) = kind {
        let loc = unsafe { CGEventGetLocation(event) };
        let keycode = if event_type == kCGEventKeyDown || event_type == kCGEventKeyUp {
            unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) }
        } else {
            -1
        };
        if let Ok(mut s) = state.lock() {
            s.push(kind, loc.x, loc.y, keycode);
        }
    }
    event
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn start() -> Result<()> {
    if !super::macos::accessibility_granted() {
        // Trigger the macOS prompt if the process has not asked before.
        // Daemonized processes often inherit TCC separately from the CLI
        // that launched them — this is the first chance to get the grant.
        let _ = super::macos::prompt_accessibility();
        bail!(
            "Accessibility permission required for the daemon process.\n\
             macOS tracks Accessibility grants per-invocation; the daemon\n\
             is a separate process from the CLI. Open System Settings →\n\
             Privacy & Security → Accessibility and make sure the sidekar\n\
             binary is enabled, then restart the daemon:\n\
             \n\
                 sidekar daemon stop && sidekar daemon start"
        );
    }
    {
        let state = monitor().state.lock().unwrap();
        if state.thread_running {
            return Ok(()); // idempotent
        }
    }

    let state_arc = monitor().state.clone();
    // We need to pass a raw pointer into the tap callback that outlives the
    // runloop. Leak a cloned Arc so it sticks around; stop() will drop it.
    // Wrap in a usize so it can cross the thread boundary — the raw pointer
    // itself is not Send, but usize is.
    let user_ptr_usize = Box::into_raw(Box::new(state_arc.clone())) as usize;

    // Events of interest: all mouse + keyboard + scroll. 64-bit mask where
    // each bit is (1 << CGEventType).
    let mask: CGEventMask = (1 << kCGEventLeftMouseDown as u64)
        | (1 << kCGEventLeftMouseUp as u64)
        | (1 << kCGEventRightMouseDown as u64)
        | (1 << kCGEventRightMouseUp as u64)
        | (1 << kCGEventOtherMouseDown as u64)
        | (1 << kCGEventOtherMouseUp as u64)
        | (1 << kCGEventLeftMouseDragged as u64)
        | (1 << kCGEventRightMouseDragged as u64)
        | (1 << kCGEventMouseMoved as u64)
        | (1 << kCGEventKeyDown as u64)
        | (1 << kCGEventKeyUp as u64)
        | (1 << kCGEventScrollWheel as u64);

    // Channel used to hand the runloop ref back to the control thread once
    // the tap thread has entered its runloop. Wrap in RunloopPtr so the raw
    // pointer can be sent — access stays behind the global mutex.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Option<RunloopPtr>>();

    thread::Builder::new()
        .name("sidekar-monitor".to_string())
        .spawn(move || {
            let user_ptr = user_ptr_usize as *mut c_void;
            let tap = unsafe {
                CGEventTapCreate(
                    kCGSessionEventTap,
                    kCGHeadInsertEventTap,
                    kCGEventTapOptionListenOnly,
                    mask,
                    tap_callback,
                    user_ptr,
                )
            };
            if tap.is_null() {
                let _ = ready_tx.send(None);
                unsafe {
                    drop(Box::from_raw(user_ptr as *mut Arc<Mutex<MonitorState>>));
                }
                return;
            }
            let runloop_source = unsafe {
                CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0)
            };
            let current_loop = unsafe { CFRunLoopGetCurrent() };
            unsafe {
                CFRunLoopAddSource(current_loop, runloop_source, kCFRunLoopCommonModes);
            }
            let _ = ready_tx.send(Some(RunloopPtr(current_loop)));
            unsafe { CFRunLoopRun() };
            unsafe {
                CFRunLoopRemoveSource(current_loop, runloop_source, kCFRunLoopCommonModes);
                CFRelease(runloop_source as *const c_void);
                CFRelease(tap as *const c_void);
                drop(Box::from_raw(user_ptr as *mut Arc<Mutex<MonitorState>>));
            }
        })
        .map_err(|e| anyhow::anyhow!("failed to spawn monitor thread: {e}"))?;

    let Ok(Some(loop_ref)) = ready_rx.recv() else {
        bail!(
            "CGEventTap creation failed. This usually means Accessibility is\n\
             not granted to the sidekar binary. Run `sidekar desktop trust`\n\
             and grant Accessibility if not already."
        );
    };

    let mut state = monitor().state.lock().unwrap();
    state.runloop = Some(loop_ref.0);
    state.thread_running = true;
    Ok(())
}

/// Send-safe wrapper around a CoreFoundation runloop pointer. The pointer
/// itself is not `Send`, but the runloop stays alive for the lifetime of
/// the monitor thread, and access is serialized through `MonitorState`.
struct RunloopPtr(CFRunLoopRef);
unsafe impl Send for RunloopPtr {}

pub fn stop() -> Result<()> {
    let loop_ref = {
        let mut state = monitor().state.lock().unwrap();
        if !state.thread_running {
            return Ok(());
        }
        let rl = state.runloop.take();
        state.thread_running = false;
        rl
    };
    if let Some(rl) = loop_ref {
        unsafe { CFRunLoopStop(rl) };
    }
    Ok(())
}

pub fn clear() -> usize {
    let mut state = monitor().state.lock().unwrap();
    let n = state.events.len();
    state.events.clear();
    n
}

pub fn snapshot(limit: Option<usize>) -> Vec<MonitorEvent> {
    let state = monitor().state.lock().unwrap();
    let total = state.events.len();
    let take = limit.unwrap_or(total).min(total);
    let skip = total.saturating_sub(take);
    state.events.iter().skip(skip).cloned().collect()
}

pub fn stats() -> serde_json::Value {
    let state = monitor().state.lock().unwrap();
    serde_json::json!({
        "running": state.thread_running,
        "pending": state.events.len(),
        "cap": state.cap,
        "totalReceived": state.total_received,
        "totalDropped": state.total_dropped,
    })
}

// Suppress unused-const warnings on the event-type constants we don't match
// against (keep them listed for future use / reference).
#[allow(dead_code)]
const _KEEP_UNUSED: () = {
    let _ = c_int::default;
};
