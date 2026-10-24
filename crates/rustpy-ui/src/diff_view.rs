//! Shared configurable viewer; the stable constructor/UI are used by Local History.
pub mod assets;
pub mod merge;
pub mod model;
mod viewer;
use crate::theme::UiTheme;
use rustpy_settings::diff_merge::DiffMergeSettings;
use std::{
    path::PathBuf,
    sync::{LazyLock, Mutex, RwLock},
};
/// Scheme keys for diff backgrounds and their RGB defaults when a color scheme
/// does not override them; shared by the viewer and the color scheme editor.
pub const DIFF_DELETED: (&str, [u8; 3]) = ("diff.deleted", [170, 40, 40]);
pub const DIFF_INSERTED: (&str, [u8; 3]) = ("diff.inserted", [30, 130, 60]);
/// Default RGB for a diff scheme key, or `None` for other keys.
pub fn diff_color_default(key: &str) -> Option<[u8; 3]> {
    [DIFF_DELETED, DIFF_INSERTED]
        .into_iter()
        .find(|(name, _)| *name == key)
        .map(|(_, rgb)| rgb)
}
static DEFAULTS: LazyLock<RwLock<DiffMergeSettings>> =
    LazyLock::new(|| RwLock::new(DiffMergeSettings::default()));
pub fn configure(settings: &DiffMergeSettings) {
    if let Ok(mut defaults) = DEFAULTS.write() {
        *defaults = settings.clone();
    }
}
pub struct ApplyEdit {
    pub path: PathBuf,
    pub expected: String,
    pub replacement: String,
}
pub struct SideBySide {
    pub changes: usize,
    state: Mutex<viewer::Viewer>,
    target: Mutex<Option<(PathBuf, String)>>,
}
impl SideBySide {
    pub fn new(old: &str, new: &str) -> Self {
        let options = DEFAULTS
            .read()
            .map(|value| value.clone())
            .unwrap_or_default();
        let model = model::Model::new(old, new, options.ignore, options.align);
        let changes = model
            .hunks
            .iter()
            .map(|hunk| hunk.left.len() + hunk.right.len())
            .sum();
        Self {
            changes,
            state: Mutex::new(viewer::Viewer::new(old, new, options, model)),
            target: Mutex::new(None),
        }
    }
    pub fn with_edit_target(mut self, path: PathBuf, expected: String) -> Self {
        if let Ok(state) = self.state.get_mut() {
            state.editable = true;
        }
        *self
            .target
            .get_mut()
            .unwrap_or_else(|error| error.into_inner()) = Some((path, expected));
        self
    }
    pub fn texts(&self) -> (String, String) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        (state.left.clone(), state.right.clone())
    }
    pub fn take_apply(&self) -> Option<ApplyEdit> {
        let mut state = self.state.lock().ok()?;
        if !std::mem::take(&mut state.apply) {
            return None;
        }
        let target = self.target.lock().ok()?;
        let (path, expected) = target.as_ref()?;
        Some(ApplyEdit {
            path: path.clone(),
            expected: expected.clone(),
            replacement: state.right.clone(),
        })
    }
    pub fn acknowledge_apply(&self, text: String) {
        if let Ok(mut target) = self.target.lock() {
            if let Some((_, expected)) = target.as_mut() {
                *expected = text;
            }
        }
    }
    pub fn take_boundary(&self) -> i8 {
        self.state
            .lock()
            .map(|mut state| std::mem::take(&mut state.boundary))
            .unwrap_or(0)
    }
    pub fn ui(
        &self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
        left_title: &str,
        right_title: &str,
    ) {
        if let Ok(mut state) = self.state.lock() {
            ui.push_id(self as *const Self as usize, |ui| {
                state.ui(ui, theme, mono, left_title, right_title)
            });
        }
    }
}
