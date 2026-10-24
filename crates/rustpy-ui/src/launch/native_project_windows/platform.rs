//! AppKit tab grouping is restricted to the host's exact project window list.
//! Tool windows, detached editors and windows from other processes are excluded.
#[cfg(target_os = "macos")]
pub(super) fn merge(ctx: &egui::Context, projects: &[egui::ViewportId]) -> Result<(), String> {
    use objc2::{msg_send, rc::Retained, runtime::AnyObject, sel, MainThreadMarker};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if MainThreadMarker::new().is_none() {
        return Err("Project window tab groups require the native main thread".into());
    }
    let mut windows = Vec::new();
    for viewport in projects {
        let window = eframe::native_window::get(ctx, *viewport)
            .ok_or("A project window has not finished opening")?;
        let handle = window.window_handle().map_err(|e| e.to_string())?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err("Expected an AppKit project window".into());
        };
        // SAFETY: eframe owns a live AppKit NSView on this native UI thread.
        // Retain its NSWindow for this operation, never across a viewport close.
        let native = unsafe {
            let view = handle.ns_view.cast::<AnyObject>().as_ref();
            let native: *mut AnyObject = msg_send![view, window];
            Retained::retain(native).ok_or("Project NSView no longer has a window")?
        };
        // SAFETY: NSWindow implements these selectors on supported macOS;
        // availability is tested before sending addTabbedWindow:ordered:.
        unsafe {
            let supported: bool =
                msg_send![&*native,respondsToSelector:sel!(addTabbedWindow:ordered:)];
            if !supported {
                return Err("Native window tab groups require macOS 10.12 or later".into());
            }
            let style: usize = msg_send![&*native, styleMask];
            if style & 1 == 0 {
                return Err("Enable native window headers before merging project windows".into());
            }
            if style & (1 << 14) != 0 {
                return Err("Exit native full screen before merging project windows".into());
            }
        }
        windows.push(native);
    }
    if windows.len() < 2 {
        return Ok(());
    }
    let identifier = objc2_foundation::NSString::from_str(&format!(
        "org.rustpy.projects.{}",
        std::process::id()
    ));
    // SAFETY: each retained object is a distinct host-owned NSWindow. Preferred
    // mode permits tab groups; NSWindowAbove (1) appends in registry order.
    unsafe {
        for window in &windows {
            let _: () = msg_send![&**window,setTabbingIdentifier:&*identifier];
            let _: () = msg_send![&**window,setTabbingMode:1isize];
        }
        let first = &*windows[0];
        for window in windows.iter().skip(1) {
            let first_group: *mut AnyObject = msg_send![first, tabGroup];
            let other_group: *mut AnyObject = msg_send![&**window, tabGroup];
            if first_group.is_null() || first_group != other_group {
                let _: () = msg_send![first,addTabbedWindow:&**window,ordered:1isize];
            }
        }
        let _: () = msg_send![first,makeKeyAndOrderFront:std::ptr::null::<AnyObject>()];
    }
    Ok(())
}

/// Configure before the project window is first ordered on screen. New-window
/// requests remain separate until Merge All is invoked, and can never become
/// automatic tabs of an unrelated editor/tool window in this process.
#[cfg(target_os = "macos")]
pub(super) fn prepare(ctx: &egui::Context, viewport: egui::ViewportId) -> Result<(), String> {
    use objc2::{msg_send, runtime::AnyObject, MainThreadMarker};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if MainThreadMarker::new().is_none() {
        return Err("Project window preparation requires the native main thread".into());
    }
    let window = eframe::native_window::get(ctx, viewport)
        .ok_or("Waiting for the native project viewport")?;
    let handle = window.window_handle().map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("Expected an AppKit project window".into());
    };
    let identifier = objc2_foundation::NSString::from_str(&format!(
        "org.rustpy.projects.{}",
        std::process::id()
    ));
    // SAFETY: eframe retains this NSView and NSWindow on the native UI thread.
    unsafe {
        let view = handle.ns_view.cast::<AnyObject>().as_ref();
        let native: *mut AnyObject = msg_send![view, window];
        if native.is_null() {
            return Err("Project NSView no longer has a window".into());
        }
        let _: () = msg_send![native,setTabbingIdentifier:&*identifier];
        let _: () = msg_send![native,setTabbingMode:2isize];
    }
    Ok(())
}
