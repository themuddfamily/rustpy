//! Actual native child-webview integration, not a text preview or browser-launch substitute.
//!
//! Build the desktop binary with `--features native-web-preview` to include Wry.
//! Windows requires the WebView2 runtime; macOS uses WKWebView. Linux requires
//! GTK3 + WebKitGTK 4.1 development/runtime packages (for example
//! libgtk-3-dev and libwebkit2gtk-4.1-dev). The eframe/winit child-window path
//! uses X11; run the IDE through XWayland on Wayland desktops. A GTK-native
//! Wayland host would require replacing the outer winit window/container.
//! Wry/GTK are optional so source-only/headless builds do not acquire desktop
//! engine dependencies. The UI reports an unavailable engine explicitly.
use crossbeam_channel::Sender;
#[derive(Clone, Debug)]
pub enum Event {
    #[cfg(feature = "native-web-preview")]
    Title(String),
    #[cfg(feature = "native-web-preview")]
    Navigated(String),
    #[cfg(feature = "native-web-preview")]
    External(String),
}
#[derive(Default)]
pub struct Host {
    id: u64,
    #[cfg(feature = "native-web-preview")]
    view: Option<wry::WebView>,
    /// Dropped after `view` (field order).
    #[cfg(all(feature = "native-web-preview", windows))]
    context: Option<wry::WebContext>,
    current_url: Option<String>,
    #[cfg(feature = "native-web-preview")]
    visible: std::cell::Cell<bool>,
    #[cfg(test)]
    hide_requests: std::cell::Cell<usize>,
}
impl Host {
    pub fn available() -> bool {
        cfg!(feature = "native-web-preview")
    }
    pub fn id(&self) -> Option<u64> {
        self.current_url.as_ref().map(|_| self.id)
    }
    pub fn is_open(&self) -> bool {
        self.current_url.is_some()
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn hide(&self) {
        #[cfg(test)]
        self.hide_requests.set(self.hide_requests.get() + 1);
        #[cfg(feature = "native-web-preview")]
        if self.visible.get()
            && self
                .view
                .as_ref()
                .is_some_and(|view| view.set_visible(false).is_ok())
        {
            self.visible.set(false);
        }
    }
    #[cfg(test)]
    pub(crate) fn hide_requests(&self) -> usize {
        self.hide_requests.get()
    }
    pub fn pump(&self) {
        #[cfg(all(feature = "native-web-preview", target_os = "linux"))]
        if gtk::is_initialized_main_thread() {
            for _ in 0..16 {
                if !gtk::events_pending() {
                    break;
                }
                gtk::main_iteration_do(false);
            }
        }
    }
    pub fn navigate(
        &mut self,
        frame: &impl raw_window_handle::HasWindowHandle,
        url: &str,
        rect: egui::Rect,
        scale: f32,
        javascript: bool,
        events: Sender<Event>,
    ) -> Result<(), String> {
        #[cfg(not(feature = "native-web-preview"))]
        {
            let _ = (frame, url, rect, scale, javascript, events);
            Err("This build has no native browser engine. Build with native-web-preview (WebView2 / WKWebView / WebKitGTK). External browsers remain separate actions.".into())
        }
        #[cfg(feature = "native-web-preview")]
        {
            self.reset();
            #[cfg(target_os = "linux")]
            use wry::raw_window_handle::HasWindowHandle;
            #[cfg(target_os = "linux")]
            {
                let handle = frame.window_handle().map_err(|e| e.to_string())?;
                if matches!(
                    handle.as_raw(),
                    wry::raw_window_handle::RawWindowHandle::Wayland(_)
                ) {
                    return Err(
                        "Embedded preview requires the X11/XWayland backend with this eframe host"
                            .into(),
                    );
                }
                gtk::init().map_err(|e| format!("Cannot initialize WebKitGTK host: {e}"))?;
            }
            let origin = url::Url::parse(url).map_err(|e| e.to_string())?.origin();
            let navigation = events.clone();
            let popup = events.clone();
            let title = events;
            // Without a data directory WebView2 keeps its profile in
            // `<exe>.WebView2` beside the executable: unwritable under Program
            // Files (the preview fails to start) and left behind in a per-user
            // install folder after uninstall. Keep it with RustPy's rebuildable
            // state instead (`--system-dir`/`RUSTPY_SYSTEM_DIR` honoured).
            #[cfg(windows)]
            let mut context = {
                let directory = rustpy_settings::Settings::system_dir().join("webview2");
                std::fs::create_dir_all(&directory)
                    .map_err(|e| format!("Cannot create {}: {e}", directory.display()))?;
                wry::WebContext::new(Some(directory))
            };
            #[cfg(windows)]
            let builder = wry::WebViewBuilder::new_with_web_context(&mut context);
            #[cfg(not(windows))]
            let builder = wry::WebViewBuilder::new();
            let mut builder = builder
                .with_url(url)
                .with_bounds(bounds(rect, scale))
                .with_visible(false)
                .with_incognito(true)
                .with_focused(false)
                .with_clipboard(false)
                .with_autoplay(false)
                .with_navigation_handler(move |url| {
                    let allowed = url == "about:blank"
                        || url::Url::parse(&url).is_ok_and(|u| {
                            u.origin() == origin && matches!(u.scheme(), "http" | "https")
                        });
                    let _ = navigation.try_send(if allowed {
                        Event::Navigated(url)
                    } else {
                        Event::External(url)
                    });
                    allowed
                })
                .with_new_window_req_handler(move |url, _| {
                    let _ = popup.try_send(Event::External(url));
                    wry::NewWindowResponse::Deny
                })
                .with_download_started_handler(|_, _| false)
                .with_permission_handler(|_| wry::PermissionResponse::Deny)
                .with_document_title_changed_handler(move |value| {
                    let _ = title.try_send(Event::Title(value));
                });
            if !javascript {
                builder = builder.with_javascript_disabled();
            }
            let view = builder
                .build_as_child(frame)
                .map_err(|e| format!("Cannot create embedded webview: {e}"))?;
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            self.id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.view = Some(view);
            #[cfg(windows)]
            {
                self.context = Some(context);
            }
            self.current_url = Some(url.into());
            Ok(())
        }
    }
    pub fn show(&self, rect: egui::Rect, scale: f32, zoom: f64) -> Result<(), String> {
        #[cfg(feature = "native-web-preview")]
        if let Some(view) = &self.view {
            view.set_bounds(bounds(rect, scale))
                .map_err(|e| e.to_string())?;
            view.zoom(zoom.clamp(0.25, 4.)).map_err(|e| e.to_string())?;
            // Wry's GTK backend maps/unmaps real widgets here. Preserve a
            // stable mapped child across egui repaints while GTK dispatches.
            if !self.visible.get() {
                view.set_visible(true).map_err(|e| e.to_string())?;
                self.visible.set(true);
            }
        }
        let _ = (rect, scale, zoom);
        Ok(())
    }
    pub fn reload(&self) -> Result<(), String> {
        #[cfg(feature = "native-web-preview")]
        if let Some(view) = &self.view {
            return view.reload().map_err(|e| e.to_string());
        }
        Ok(())
    }
    pub fn back(&self) -> Result<(), String> {
        #[cfg(feature = "native-web-preview")]
        if let Some(view) = &self.view {
            return view.go_back().map_err(|e| e.to_string());
        }
        Ok(())
    }
    pub fn forward(&self) -> Result<(), String> {
        #[cfg(feature = "native-web-preview")]
        if let Some(view) = &self.view {
            return view.go_forward().map_err(|e| e.to_string());
        }
        Ok(())
    }
}
#[cfg(feature = "native-web-preview")]
fn bounds(rect: egui::Rect, scale: f32) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::PhysicalPosition::new(
            (rect.left() * scale) as i32,
            (rect.top() * scale) as i32,
        )
        .into(),
        size: wry::dpi::PhysicalSize::new(
            (rect.width() * scale).max(1.) as u32,
            (rect.height() * scale).max(1.) as u32,
        )
        .into(),
    }
}
