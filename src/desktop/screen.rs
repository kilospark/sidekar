use crate::*;

/// Capture a desktop screenshot.
/// If pid is provided, captures the frontmost window of that app.
/// Uses CGWindowListCreateImage (CoreGraphics) — no CLI dependency.
/// Falls back to `screencapture` CLI if CG returns null.
#[cfg(target_os = "macos")]
pub async fn capture_desktop_screenshot(pid: Option<i32>, output_path: &Path) -> Result<()> {
    if let Some(pid) = pid {
        // Try CGWindowList first (works even when AX can't enumerate windows)
        if let Some(wid) = frontmost_window_id_for_pid(pid)
            && capture_window_cg(wid, output_path)?
        {
            return Ok(());
        }
        // Fallback: try AX-based window list
        let windows = crate::desktop::native::list_windows(pid)?;
        if let Some(win_id) = windows.first().and_then(|w| w.window_id)
            && capture_window_cg(win_id, output_path)?
        {
            return Ok(());
        }
    } else {
        // Full-screen capture
        if capture_display_cg(output_path)? {
            return Ok(());
        }
    }

    // Fallback to screencapture CLI
    capture_via_cli(pid, output_path).await
}

/// Find the frontmost on-screen window for a PID via CGWindowListCopyWindowInfo.
/// Returns the CGWindowID of the topmost layer-0 window, or None.
#[cfg(target_os = "macos")]
fn frontmost_window_id_for_pid(pid: i32) -> Option<u32> {
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use std::ffi::c_void;

    #[allow(clashing_extern_declarations)]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
    }

    // kCGWindowListOptionOnScreenOnly = 1 << 0 = 1
    // kCGNullWindowID = 0
    let info_list = unsafe { CGWindowListCopyWindowInfo(1, 0) };
    if info_list.is_null() {
        return None;
    }

    let array: CFArray<CFDictionary<CFString, CFType>> =
        unsafe { CFArray::wrap_under_create_rule(info_list as _) };

    let key_pid = CFString::new("kCGWindowOwnerPID");
    let key_wid = CFString::new("kCGWindowNumber");
    let key_layer = CFString::new("kCGWindowLayer");

    for i in 0..array.len() {
        let dict = unsafe { array.get_unchecked(i) };
        let Some(pid_val) = dict.find(&key_pid) else {
            continue;
        };
        let cf_pid: CFNumber =
            unsafe { CFNumber::wrap_under_get_rule(pid_val.as_CFTypeRef() as _) };
        let Some(entry_pid) = cf_pid.to_i32() else {
            continue;
        };
        if entry_pid != pid {
            continue;
        }

        // Check layer == 0 (normal windows, not menubars/overlays)
        if let Some(layer_val) = dict.find(&key_layer) {
            let cf_layer: CFNumber =
                unsafe { CFNumber::wrap_under_get_rule(layer_val.as_CFTypeRef() as _) };
            if let Some(layer) = cf_layer.to_i32()
                && layer != 0
            {
                continue;
            }
        }

        // Get window ID
        if let Some(wid_val) = dict.find(&key_wid) {
            let cf_wid: CFNumber =
                unsafe { CFNumber::wrap_under_get_rule(wid_val.as_CFTypeRef() as _) };
            if let Some(wid) = cf_wid.to_i32() {
                return Some(wid as u32);
            }
        }
    }

    None
}

/// Capture a single window by CGWindowID using CGWindowListCreateImage.
/// Returns Ok(true) on success, Ok(false) if the API returned null.
#[cfg(target_os = "macos")]
fn capture_window_cg(window_id: u32, output_path: &Path) -> Result<bool> {
    use std::ffi::c_void;

    type CGImageRef = *const c_void;

    // CGRect for null rect (capture window bounds)
    #[repr(C)]
    struct CGRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    unsafe extern "C" {
        fn CGWindowListCreateImage(
            screen_bounds: CGRect,
            list_option: u32,
            window_id: u32,
            image_option: u32,
        ) -> CGImageRef;
    }

    // kCGWindowListOptionIncludingWindow = 1 << 3 = 8
    // kCGWindowImageBoundsIgnoreFraming = 1 << 0 = 1
    // kCGWindowImageShouldBeOpaque = 1 << 1 = 2 (not needed)
    // CGRectNull = {inf, inf, 0, 0}
    let null_rect = CGRect {
        x: f64::INFINITY,
        y: f64::INFINITY,
        w: 0.0,
        h: 0.0,
    };

    let image = unsafe {
        CGWindowListCreateImage(
            null_rect, 8, // kCGWindowListOptionIncludingWindow
            window_id, 1, // kCGWindowImageBoundsIgnoreFraming
        )
    };

    if image.is_null() {
        return Ok(false);
    }

    let saved = save_cgimage_as_png(image, output_path);
    unsafe {
        CFRelease(image);
    }
    saved?;
    Ok(true)
}

/// Capture the full main display using CGWindowListCreateImage.
#[cfg(target_os = "macos")]
fn capture_display_cg(output_path: &Path) -> Result<bool> {
    use std::ffi::c_void;

    type CGImageRef = *const c_void;

    #[repr(C)]
    struct CGRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    unsafe extern "C" {
        fn CGWindowListCreateImage(
            screen_bounds: CGRect,
            list_option: u32,
            window_id: u32,
            image_option: u32,
        ) -> CGImageRef;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayPixelsWide(display: u32) -> usize;
        fn CGDisplayPixelsHigh(display: u32) -> usize;
    }

    let display_id = unsafe { CGMainDisplayID() };
    let w = unsafe { CGDisplayPixelsWide(display_id) } as f64;
    let h = unsafe { CGDisplayPixelsHigh(display_id) } as f64;

    let rect = CGRect {
        x: 0.0,
        y: 0.0,
        w,
        h,
    };

    // kCGWindowListOptionAll = 0
    // kCGNullWindowID = 0
    // kCGWindowImageDefault = 0
    let image = unsafe { CGWindowListCreateImage(rect, 0, 0, 0) };

    if image.is_null() {
        return Ok(false);
    }

    let saved = save_cgimage_as_png(image, output_path);
    unsafe {
        CFRelease(image);
    }
    saved?;
    Ok(true)
}

/// Write a CGImageRef to disk as PNG using ImageIO.
#[cfg(target_os = "macos")]
fn save_cgimage_as_png(image: *const std::ffi::c_void, path: &Path) -> Result<()> {
    use std::ffi::c_void;

    type CFURLRef = *const c_void;
    type CFStringRef = *const c_void;
    type CGImageDestinationRef = *mut c_void;
    type CGImageRef = *const c_void;

    unsafe extern "C" {
        fn CFURLCreateWithFileSystemPath(
            allocator: *const c_void,
            file_path: CFStringRef,
            path_style: i32,
            is_directory: bool,
        ) -> CFURLRef;
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            c_str: *const u8,
            encoding: u32,
        ) -> CFStringRef;
        fn CGImageDestinationCreateWithURL(
            url: CFURLRef,
            type_: CFStringRef,
            count: usize,
            options: *const c_void,
        ) -> CGImageDestinationRef;
        fn CGImageDestinationAddImage(
            dest: CGImageDestinationRef,
            image: CGImageRef,
            properties: *const c_void,
        );
        fn CGImageDestinationFinalize(dest: CGImageDestinationRef) -> bool;
    }

    let path_str = path.to_str().ok_or_else(|| anyhow!("non-UTF8 path"))?;
    let c_path = format!("{path_str}\0");

    unsafe {
        let cf_path = CFStringCreateWithCString(
            std::ptr::null(),
            c_path.as_ptr(),
            0x08000100, // kCFStringEncodingUTF8
        );
        if cf_path.is_null() {
            bail!("failed to create CFString for path");
        }

        // kCFURLPOSIXPathStyle = 0
        let url = CFURLCreateWithFileSystemPath(std::ptr::null(), cf_path, 0, false);
        CFRelease(cf_path);
        if url.is_null() {
            bail!("failed to create CFURL");
        }

        let png_type =
            CFStringCreateWithCString(std::ptr::null(), c"public.png".as_ptr().cast(), 0x08000100);

        let dest = CGImageDestinationCreateWithURL(url, png_type, 1, std::ptr::null());
        CFRelease(url);
        CFRelease(png_type);

        if dest.is_null() {
            bail!("failed to create image destination");
        }

        CGImageDestinationAddImage(dest, image, std::ptr::null());
        let ok = CGImageDestinationFinalize(dest);
        CFRelease(dest as *const c_void);

        if !ok {
            bail!("CGImageDestinationFinalize failed");
        }
    }
    Ok(())
}

/// Fallback: screencapture CLI.
#[cfg(target_os = "macos")]
async fn capture_via_cli(pid: Option<i32>, output_path: &Path) -> Result<()> {
    use std::process::Command as StdCommand;

    let mut cmd = StdCommand::new("screencapture");
    cmd.arg("-x"); // no sound
    cmd.arg("-o"); // no shadow

    if let Some(pid) = pid {
        let windows = crate::desktop::native::list_windows(pid)?;
        if let Some(win_id) = windows.first().and_then(|w| w.window_id) {
            cmd.arg("-l");
            cmd.arg(win_id.to_string());
        }
    }

    cmd.arg(output_path.to_string_lossy().as_ref());

    let status = cmd.status().context("failed to run screencapture")?;
    if !status.success() {
        bail!("screencapture exited with status {status}");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

#[cfg(not(target_os = "macos"))]
pub async fn capture_desktop_screenshot(_pid: Option<i32>, _output_path: &Path) -> Result<()> {
    bail!("Desktop screenshot is only available on macOS")
}
