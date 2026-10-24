//! Files that macOS asks RustPy to open: Finder's *Open With › RustPy*, double-clicking a
//! file whose default app is RustPy, or dropping files on the Dock icon. They arrive as an
//! "open documents" Apple Event, not as command-line arguments, and winit does not handle
//! that event, so this module installs its own handler (macOS only; elsewhere [`install`]
//! does nothing and [`take_opened_paths`] is always empty).
//!
//! AppKit installs its default handler while the application finishes launching, so ours
//! is installed from the `NSApplicationWillFinishLaunchingNotification` (registered by
//! [`install`] before the event loop starts): late enough to replace AppKit's, early enough
//! for the documents that launched the app. The bundle declares the document types in its
//! Info.plist (`packaging/macos/build-app.sh`).

use std::path::PathBuf;
use std::sync::Mutex;

/// Paths received and not yet taken by the IDE.
static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// Woken when paths arrive while the UI is idle.
static REPAINT: Mutex<Option<egui::Context>> = Mutex::new(None);

/// Queues paths for the IDE and wakes the UI.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn push_paths(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if let Ok(mut q) = OPENED.lock() {
        q.extend(paths);
    }
    if let Some(ctx) = REPAINT.lock().ok().and_then(|c| c.clone()) {
        ctx.request_repaint();
    }
}

/// The paths macOS asked to open since the last call. `ctx` is woken when more arrive.
pub fn take_opened_paths(ctx: &egui::Context) -> Vec<PathBuf> {
    if let Ok(mut r) = REPAINT.lock() {
        if r.is_none() {
            *r = Some(ctx.clone());
        }
    }
    OPENED
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// Prepares the "open documents" handler. Call once, before the event loop starts.
pub fn install() {
    #[cfg(target_os = "macos")]
    imp::install();
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{define_class, msg_send, sel, AnyThread};
    use objc2_foundation::{
        NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSString,
    };

    /// Four-character codes of the Apple Event Manager.
    const fn four_cc(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }
    const K_CORE_EVENT_CLASS: u32 = four_cc(b"aevt");
    const K_AE_OPEN_DOCUMENTS: u32 = four_cc(b"odoc");
    const KEY_DIRECT_OBJECT: u32 = four_cc(b"----");

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and the class does not
        // implement Drop.
        #[unsafe(super(NSObject))]
        #[name = "RustPyOpenDocumentsHandler"]
        struct OpenDocumentsHandler;

        impl OpenDocumentsHandler {
            /// NSApplicationWillFinishLaunchingNotification: AppKit has installed its own
            /// Apple Event handlers; replace the one for "open documents".
            #[unsafe(method(applicationWillFinishLaunching:))]
            fn will_finish_launching(&self, _notification: &NSNotification) {
                let manager = NSAppleEventManager::sharedAppleEventManager();
                // SAFETY: the selector names a method of this class taking two
                // NSAppleEventDescriptor arguments, and the handler object is never freed.
                unsafe {
                    let _: () = msg_send![&*manager, setEventHandler: self, andSelector: sel!(handleOpenDocuments:withReplyEvent:), forEventClass: K_CORE_EVENT_CLASS, andEventID: K_AE_OPEN_DOCUMENTS];
                }
            }

            #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
            fn handle_open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
                // A panic must not unwind into AppKit.
                if let Ok(paths) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| document_paths(event))) {
                    log::info!("macOS asked to open {paths:?}");
                    super::push_paths(paths);
                }
            }
        }
    );

    /// The files of an "open documents" event: its direct parameter is a list of file URLs
    /// (or aliases, which `fileURLValue` coerces).
    fn document_paths(event: &NSAppleEventDescriptor) -> Vec<PathBuf> {
        // SAFETY: paramDescriptorForKeyword: takes an AEKeyword (u32) and returns a
        // nullable NSAppleEventDescriptor.
        let list: Option<Retained<NSAppleEventDescriptor>> =
            unsafe { msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT] };
        let Some(list) = list else { return Vec::new() };
        let count = list.numberOfItems();
        let items: Vec<Retained<NSAppleEventDescriptor>> = if count > 0 {
            // AppleEvent lists are 1-based.
            (1..=count)
                .filter_map(|i| list.descriptorAtIndex(i))
                .collect()
        } else {
            // A single document can come as the descriptor itself.
            vec![list]
        };
        items
            .iter()
            .filter_map(|d| d.fileURLValue())
            .filter_map(|url| url.path())
            .map(|p| PathBuf::from(p.to_string()))
            .collect()
    }

    impl OpenDocumentsHandler {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }
    }

    pub(super) fn install() {
        let handler = OpenDocumentsHandler::new();
        let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
        // SAFETY: the selector names a method of the handler taking the notification.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &handler,
                sel!(applicationWillFinishLaunching:),
                Some(&*name),
                None,
            );
        }
        // Neither the notification center nor the Apple Event manager retains the handler:
        // it lives as long as the process.
        std::mem::forget(handler);
    }
}
