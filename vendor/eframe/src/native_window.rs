//! Owned native viewport windows for integrations such as embedded webviews.
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use winit::window::Window;

struct Registry<T>(HashMap<egui::ViewportId, Weak<T>>);
impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}
impl<T> Clone for Registry<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> Registry<T> {
    fn get(&self, viewport: egui::ViewportId) -> Option<Arc<T>> {
        self.0.get(&viewport)?.upgrade()
    }
    fn register(&mut self, viewport: egui::ViewportId, window: &Arc<T>) {
        self.0.retain(|_, window| window.strong_count() > 0);
        self.0.insert(viewport, Arc::downgrade(window));
    }
}
fn key() -> egui::Id {
    egui::Id::new("eframe-native-window-registry")
}

pub(crate) fn register(ctx: &egui::Context, viewport: egui::ViewportId, window: &Arc<Window>) {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Registry<Window>>(key())
            .register(viewport, window)
    });
}

/// The backend-owned window for this viewport, if it has been created and is alive.
///
/// Call on the native UI thread. The registry contains weak references; it does
/// not prolong window lifetimes. Headless or already destroyed viewports return None.
pub fn get(ctx: &egui::Context, viewport: egui::ViewportId) -> Option<Arc<Window>> {
    ctx.data(|data| data.get_temp::<Registry<Window>>(key()))
        .and_then(|registry| registry.get(viewport))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_owner_lookup_never_falls_back_to_root_or_retains_a_closed_window() {
        let root = Arc::new("root");
        let child = Arc::new("child");
        let id = egui::ViewportId::from_hash_of("floating-web-preview");
        let mut registry = Registry::default();
        registry.register(egui::ViewportId::ROOT, &root);
        registry.register(id, &child);
        assert_eq!(*registry.get(id).unwrap(), "child");
        assert_eq!(Arc::strong_count(&child), 1);
        drop(child);
        assert!(registry.get(id).is_none());
        assert_eq!(*registry.get(egui::ViewportId::ROOT).unwrap(), "root");
        let reopened = Arc::new("replacement");
        registry.register(id, &reopened);
        assert_eq!(*registry.get(id).unwrap(), "replacement");
    }
}

/// Opt-in project-window hosts keep a hidden root solely to drive independent
/// native viewports. Ordinary applications retain the occluded-window fast path.
pub fn set_root_ui_when_hidden(ctx:&egui::Context,enabled:bool){ctx.data_mut(|data|data.insert_temp(egui::Id::new("eframe-root-ui-when-hidden"),enabled));}
pub(crate) fn root_ui_when_hidden(ctx:&egui::Context)->bool{ctx.data(|data|data.get_temp::<bool>(egui::Id::new("eframe-root-ui-when-hidden"))).unwrap_or(false)}
