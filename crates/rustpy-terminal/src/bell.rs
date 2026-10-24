//! Platform bell. At most one native display request is in flight.
use std::sync::atomic::{AtomicBool, Ordering};
static RINGING: AtomicBool = AtomicBool::new(false);
pub fn ring() {
    if RINGING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        native();
        RINGING.store(false, Ordering::Release);
    });
}
#[cfg(windows)]
fn native() {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBeep(kind: u32) -> i32;
    }
    unsafe {
        MessageBeep(u32::MAX);
    }
}
#[cfg(target_os = "macos")]
fn native() {
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        fn NSBeep();
    }
    unsafe {
        NSBeep();
    }
}
#[cfg(target_os = "linux")]
fn native() {
    use std::ffi::{c_char, c_int, c_void};
    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
    }
    unsafe {
        let library = dlopen(c"libX11.so.6".as_ptr(), 2);
        if library.is_null() {
            fallback();
            return;
        }
        let open = dlsym(library, c"XOpenDisplay".as_ptr());
        let bell = dlsym(library, c"XBell".as_ptr());
        let flush = dlsym(library, c"XFlush".as_ptr());
        let close = dlsym(library, c"XCloseDisplay".as_ptr());
        if !open.is_null() && !bell.is_null() && !flush.is_null() && !close.is_null() {
            let open: unsafe extern "C" fn(*const c_char) -> *mut c_void =
                std::mem::transmute(open);
            let bell: unsafe extern "C" fn(*mut c_void, c_int) -> c_int = std::mem::transmute(bell);
            let flush: unsafe extern "C" fn(*mut c_void) -> c_int = std::mem::transmute(flush);
            let close: unsafe extern "C" fn(*mut c_void) -> c_int = std::mem::transmute(close);
            let display = open(std::ptr::null());
            if !display.is_null() {
                bell(display, 0);
                flush(display);
                close(display);
            } else {
                fallback();
            }
        } else {
            fallback();
        }
        dlclose(library);
    }
}
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn native() {
    fallback();
}
#[cfg(not(any(windows, target_os = "macos")))]
fn fallback() {
    use std::io::Write;
    let _ = std::io::stderr().write_all(b"\x07");
}
