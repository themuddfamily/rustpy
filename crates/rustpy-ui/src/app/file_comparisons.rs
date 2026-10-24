//! Captured comparison operands: opening a picker never changes its source owner.
use super::*;
use crate::diff_view::SideBySide;
use rustpy_core::vfs::{normalize_path, ReadLimits, Vfs};

const MAX_COMPARE_BYTES: usize = 1024 * 1024;

/// Immutable right-hand operand, captured before a modal picker opens.
/// Comparing does not mutate or require the source document to remain open.
pub(super) struct ComparisonSource {
    pub title: String,
    pub text: String,
    pub path: Option<PathBuf>,
    pub asset: bool,
}

fn bounded_text(text: String) -> Result<String, String> {
    if text.len() > MAX_COMPARE_BYTES {
        Err("Comparison text exceeds the 1 MiB limit".into())
    } else {
        Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
    }
}

impl IdeApp {
    pub(super) fn capture_comparison(
        &self,
        path: Option<&Path>,
        selection: bool,
    ) -> Result<ComparisonSource, String> {
        let tab = match path {
            Some(path) => {
                let key = normalize_path(path);
                self.tabs.iter().find(|tab| {
                    tab.state
                        .path
                        .as_deref()
                        .is_some_and(|p| normalize_path(p) == key)
                })
            }
            None => self.active.and_then(|i| self.tabs.get(i)),
        };
        if let Some(tab) = tab {
            let caret = tab.state.primary();
            let selected = selection && caret.has_selection();
            return Ok(ComparisonSource {
                title: if selected {
                    "Selection".into()
                } else {
                    tab.state.title()
                },
                text: bounded_text(if selected {
                    tab.state.doc.slice(caret.range())
                } else {
                    tab.state.doc.text()
                })?,
                asset: tab.image.is_some() || tab.notebook.is_some(),
                path: tab.state.path.clone(),
            });
        }
        let path = path.ok_or_else(|| "Open an editor to compare first".to_owned())?;
        let normalized = normalize_path(path);
        let path = normalized.as_path();
        let snapshot = Vfs::new()
            .read(
                path,
                ReadLimits {
                    max_file_bytes: MAX_COMPARE_BYTES,
                    max_cached_bytes: 0,
                    max_cached_files: 0,
                },
            )
            .map_err(|error| format!("Cannot compare {}: {error}", path.display()))?;
        let (text, _) = rustpy_text::Encoding::decode_for_path(
            &snapshot.bytes,
            path,
            self.encoding_override(path)?,
        )
        .map_err(|error| format!("Cannot decode {}: {error}", path.display()))?;
        Ok(ComparisonSource {
            title: path.display().to_string(),
            text: bounded_text(text)?,
            asset: false,
            path: Some(path.to_owned()),
        })
    }

    pub(super) fn compare_captured_file(
        &mut self,
        source: &ComparisonSource,
        other: &Path,
    ) -> Result<(), String> {
        if self.settings.diff_merge.external_files {
            if source.asset {
                if let Some(path) = &source.path {
                    return self.external_diff_paths(path, other, false);
                }
            }
            let right = self.capture_comparison(Some(other), false)?;
            return self.external_text_diff(&right.text, &source.text);
        }
        if source.asset {
            return self.open_asset_diff(
                source
                    .path
                    .clone()
                    .ok_or("Asset comparison requires a file path")?,
                other.into(),
            );
        }
        let other = match self.capture_comparison(Some(other), false) {
            Ok(other) => other,
            Err(error) => {
                if other.is_file()
                    && (super::diff_assets::asset_path(other)
                        || error.to_ascii_lowercase().contains("binary"))
                {
                    if let Some(path) = &source.path {
                        return self.open_asset_diff(path.clone(), other.into());
                    }
                }
                return Err(error);
            }
        };
        if other.asset {
            if let (Some(left), Some(right)) = (&source.path, &other.path) {
                return self.open_asset_diff(left.clone(), right.clone());
            }
        }
        self.publish_comparison("Compare Files", source, other.title, other.text);
        Ok(())
    }

    pub(super) fn compare_captured_editor(
        &mut self,
        source: &ComparisonSource,
        other: ComparisonSource,
    ) -> Result<(), String> {
        if source.asset || other.asset {
            if let (Some(left), Some(right)) = (&source.path, &other.path) {
                return self.open_asset_diff(left.clone(), right.clone());
            }
            return Err("Image/notebook comparisons require file-backed operands".into());
        }
        if self.settings.diff_merge.external_files {
            return self.external_text_diff(&other.text, &source.text);
        }
        if other.text.len() > MAX_COMPARE_BYTES {
            return Err("Comparison text exceeds 1 MiB".into());
        }
        self.publish_comparison("Compare Editors", source, other.title, other.text);
        Ok(())
    }
    pub(super) fn open_comparison_picker(&mut self, path: Option<&Path>) {
        if let Some(path) = path.filter(|path| path.is_dir()) {
            self.open_directory_comparison_picker(path);
            return;
        }
        let captured = self.capture_comparison(path, false).or_else(|error| {
            if let Some(path) = path.filter(|path| {
                path.is_file()
                    && (super::diff_assets::asset_path(path)
                        || error.to_ascii_lowercase().contains("binary"))
            }) {
                Ok(ComparisonSource {
                    title: path.display().to_string(),
                    text: String::new(),
                    path: Some(path.into()),
                    asset: true,
                })
            } else {
                Err(error)
            }
        });
        match captured {
            Ok(source) => {
                let start = source
                    .path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_owned)
                    .or_else(|| self.project.as_ref().map(|p| p.root.clone()))
                    .or_else(dirs::home_dir)
                    .unwrap_or_else(|| PathBuf::from("/"));
                self.dialog = Dialog::CompareFilePick(Box::new((FileBrowser::new(start), source)));
            }
            Err(error) => self.set_status(error),
        }
    }

    pub(super) fn confirm_comparison_picker(
        &mut self,
        state: Box<(FileBrowser, ComparisonSource)>,
        path: &Path,
    ) {
        if let Err(error) = self.compare_captured_file(&state.1, path) {
            self.set_status(error);
            self.dialog = Dialog::CompareFilePick(state);
        }
    }

    pub(super) fn compare_current_clipboard(&mut self) -> Result<(), String> {
        #[cfg(test)]
        if let Some(value) = tests::clipboard_override() {
            return self.compare_clipboard_with(|| value);
        }
        self.compare_clipboard_with(|| {
            crate::system_clipboard::open()
                .and_then(|mut clipboard| clipboard.get_text())
                .map_err(|error| format!("Cannot read clipboard: {error}"))
        })
    }

    fn compare_clipboard_with(
        &mut self,
        read: impl FnOnce() -> Result<String, String>,
    ) -> Result<(), String> {
        let source = self.capture_comparison(None, true)?;
        if source.asset {
            return Err("Compare with Clipboard requires a text editor".into());
        }
        // An empty clipboard is a real operand. Unavailable clipboard is an error,
        // never an unrelated older IDE copy.
        let clipboard = bounded_text(read()?)?;
        if self.settings.diff_merge.external_files {
            return self.external_text_diff(&clipboard, &source.text);
        }
        self.publish_comparison(
            "Compare with Clipboard",
            &source,
            "Clipboard".into(),
            clipboard,
        );
        Ok(())
    }

    fn publish_comparison(
        &mut self,
        title: &str,
        source: &ComparisonSource,
        left_title: String,
        left: String,
    ) {
        let mut diff = SideBySide::new(&left, &source.text);
        if source.title != "Selection" {
            if let Some(path) = &source.path {
                diff = diff.with_edit_target(path.clone(), source.text.clone());
            }
        }
        let state = Box::new(DiffDialog {
            title: title.into(),
            left_title,
            right_title: source.title.clone(),
            diff,
        });
        self.publish_diff_workspace(state);
    }
}

#[cfg(test)]
#[path = "file_comparisons_tests.rs"]
mod tests;
