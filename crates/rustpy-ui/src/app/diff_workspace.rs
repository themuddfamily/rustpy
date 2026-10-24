//! App ownership for editable diff tabs/windows and visual merge results.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::diff_view::{merge::MergeViewer, SideBySide};
#[derive(Default)]
pub(super) struct Workspace {
    pub(super) assets: Option<super::diff_assets::AssetWindow>,
    pub(super) external: Option<super::diff_assets::ExternalJob>,
    tabs: std::collections::HashMap<PathBuf, Box<DiffDialog>>,
    next: u64,
    sequence: Vec<(PathBuf, PathBuf)>,
    sequence_index: usize,
    pub(super) external_result: Option<((PathBuf, String, String, String), String)>,
    pub(super) merge: Option<(PathBuf, String, String, String, MergeViewer)>,
}
pub(super) fn filter_line_changes(
    changes: &[rustpy_vcs::LineChange],
    document: &Document,
    options: &rustpy_settings::diff_merge::DiffMergeSettings,
) -> Vec<rustpy_vcs::LineChange> {
    if !options.highlight_modified {
        return Vec::new();
    }
    changes
        .iter()
        .filter(|change| {
            if options.whitespace_changes {
                return true;
            }
            let old: String = change
                .old_text
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            let current: String = (change.start
                ..(change.start + change.count).min(document.line_count()))
                .map(|line| document.line_text(line))
                .collect::<String>()
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            old != current
        })
        .cloned()
        .collect()
}
impl IdeApp {
    pub(super) fn diff_controls_owned(&self) -> bool {
        self.diff_asset_controls_owned()
            || matches!(
                self.dialog,
                Dialog::Diff(_) | Dialog::ResolveConflict { .. }
            )
            || self
                .active
                .and_then(|index| self.tabs.get(index))
                .and_then(|tab| tab.state.path.as_ref())
                .is_some_and(|path| self.diff_workspace.tabs.contains_key(path))
    }
    pub(super) fn set_diff_file_sequence(
        &mut self,
        files: Vec<(PathBuf, PathBuf)>,
        current: &(PathBuf, PathBuf),
    ) {
        self.diff_workspace.sequence_index =
            files.iter().position(|pair| pair == current).unwrap_or(0);
        self.diff_workspace.sequence = files;
    }
    fn advance_diff_file(&mut self, direction: i8) -> bool {
        if direction == 0
            || !self.settings.diff_merge.next_file
            || self.diff_workspace.sequence.is_empty()
        {
            return false;
        }
        let current = self.diff_workspace.sequence_index;
        let target = if direction > 0 {
            current + 1
        } else {
            current.saturating_sub(1)
        };
        if target == current || target >= self.diff_workspace.sequence.len() {
            return false;
        }
        self.diff_workspace.sequence_index = target;
        let (left, right) = self.diff_workspace.sequence[target].clone();
        if let Err(error) = self.compare_paths_workspace(&left, &right) {
            self.set_status(error);
        }
        true
    }
    pub(super) fn retire_diff_workspace(&mut self) {
        let open: std::collections::HashSet<_> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.state.path.clone())
            .collect();
        self.diff_workspace
            .tabs
            .retain(|path, _| open.contains(path));
    }
    pub(super) fn visual_merge_workspace(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        base: &str,
        ours: &str,
        theirs: &str,
    ) -> Option<String> {
        let same = self
            .diff_workspace
            .merge
            .as_ref()
            .is_some_and(|(old, b, o, t, _)| old == path && b == base && o == ours && t == theirs);
        if !same {
            self.diff_workspace.merge = Some((
                path.into(),
                base.into(),
                ours.into(),
                theirs.into(),
                MergeViewer::new(base, theirs, ours, self.settings.diff_merge.clone()),
            ));
        }
        if !self.settings.diff_merge.merge_program.trim().is_empty()
            && ui.button("Open external merge tool").clicked()
        {
            if let Err(error) = self.start_external_merge(path, base, ours, theirs) {
                self.set_status(error);
            }
        }
        if self
            .diff_workspace
            .external_result
            .as_ref()
            .is_some_and(|((owner, b, o, t), _)| {
                owner == path && b == base && o == ours && t == theirs
            })
        {
            let (_, result) = self.diff_workspace.external_result.take().unwrap();
            self.diff_workspace
                .merge
                .as_mut()
                .unwrap()
                .4
                .set_result(result);
        }
        let viewer = &mut self.diff_workspace.merge.as_mut().unwrap().4;
        if viewer.ui(ui, &self.theme) {
            Some(viewer.result.clone())
        } else {
            None
        }
    }
    fn apply_diff_edit(&mut self, diff: &SideBySide) {
        let Some(edit) = diff.take_apply() else {
            return;
        };
        let replacement = edit.replacement.clone();
        if self.apply_diff_request(edit) {
            diff.acknowledge_apply(replacement);
        }
    }
    pub(super) fn apply_diff_request(&mut self, edit: crate::diff_view::ApplyEdit) -> bool {
        let existing = self
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_ref() == Some(&edit.path));
        let current = existing
            .map(|index| self.tabs[index].state.doc.text())
            .or_else(|| {
                self.capture_comparison(Some(&edit.path), false)
                    .ok()
                    .map(|source| source.text)
            });
        if current.as_deref() != Some(edit.expected.as_str()) {
            self.set_status(
                "Diff apply cancelled: local source changed since comparison; compare again",
            );
            return false;
        }
        self.open_file(&edit.path);
        let Some(index) = self.active.filter(|index| {
            self.tabs
                .get(*index)
                .is_some_and(|tab| tab.state.path.as_ref() == Some(&edit.path))
        }) else {
            return false;
        };
        if !self.refresh_file_permissions(index) {
            return false;
        }
        let length = self.tabs[index].state.doc.len();
        let range = TextRange::new(0.into(), length);
        if !self.tabs[index]
            .state
            .apply_text_edits(vec![(range, edit.replacement.clone())], "Apply diff change")
        {
            self.set_status("Diff apply refused by the editor's modification policy");
            return false;
        }
        self.set_status("Applied diff changes to local editor; save to write them to disk");
        true
    }
    pub(super) fn publish_diff_workspace(&mut self, state: Box<DiffDialog>) {
        if self.settings.diff_merge.editor_tabs {
            self.diff_workspace.next += 1;
            let path = PathBuf::from(format!(
                "<diff:{}>/{}",
                self.diff_workspace.next, state.title
            ));
            let mut doc = Document::new("");
            doc.set_read_only(true);
            self.push_tab(EditorState::new(doc, Some(path.clone())));
            self.diff_workspace.tabs.insert(path, state);
            self.dialog = Dialog::None;
        } else {
            self.dialog = Dialog::Diff(state);
        }
    }
    pub(super) fn rich_diff_dialog(&mut self, ctx: &egui::Context, st: Box<DiffDialog>) {
        let mut open = true;
        let mut editor = false;
        let mut escaped = false;
        egui::Window::new(&st.title)
            .open(&mut open)
            .default_size([1100.0, 700.0])
            .show_modal(ctx, |ui| {
                if ui.button("Open diff in editor tab").clicked() {
                    editor = true;
                }
                st.diff.ui(
                    ui,
                    &self.theme,
                    egui::FontId::monospace(self.settings.editor.font_size),
                    &st.left_title,
                    &st.right_title,
                );
                escaped = ui
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            });
        self.apply_diff_edit(&st.diff);
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::D)) {
            self.open_comparison_picker(None);
            return;
        }
        if self.advance_diff_file(st.diff.take_boundary()) {
            return;
        }
        if editor {
            self.diff_workspace.next += 1;
            let path = PathBuf::from(format!("<diff:{}>/{}", self.diff_workspace.next, st.title));
            let mut document = Document::new("");
            document.set_read_only(true);
            self.push_tab(EditorState::new(document, Some(path.clone())));
            self.diff_workspace.tabs.insert(path, st);
            self.focus_editor = true;
        } else if open && !escaped {
            self.dialog = Dialog::Diff(st);
        } else {
            self.focus_editor = true;
        }
    }
    pub(super) fn diff_editor_tab(&mut self, ui: &mut egui::Ui, index: usize) -> bool {
        let Some(path) = self.tabs.get(index).and_then(|tab| tab.state.path.clone()) else {
            return false;
        };
        let Some(st) = self.diff_workspace.tabs.remove(&path) else {
            return false;
        };
        let mut window = false;
        ui.horizontal(|ui| {
            ui.label(&st.title);
            window = ui.button("Open diff in window").clicked();
        });
        st.diff.ui(
            ui,
            &self.theme,
            egui::FontId::monospace(self.settings.editor.font_size),
            &st.left_title,
            &st.right_title,
        );
        self.apply_diff_edit(&st.diff);
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.request_close_tab(index);
            return true;
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::D)) {
            self.open_comparison_picker(None);
        }
        let _ = self.advance_diff_file(st.diff.take_boundary());
        if window {
            self.dialog = Dialog::Diff(st);
        } else {
            self.diff_workspace.tabs.insert(path, st);
        }
        true
    }
    pub(super) fn diff_merge_settings_page(&mut self, ui: &mut egui::Ui) {
        let options = &mut self.settings.diff_merge;
        ui.heading("Diff & Merge");
        ui.checkbox(
            &mut options.highlight_modified,
            "Highlight modified lines in gutter / error stripe",
        );
        ui.checkbox(
            &mut options.whitespace_changes,
            "Show whitespace-only changes in gutter",
        );
        ui.checkbox(&mut options.unified, "Unified by default");
        ui.checkbox(&mut options.synchronize, "Synchronize scrolling");
        ui.checkbox(&mut options.collapse, "Collapse unchanged fragments");
        ui.add(
            egui::DragValue::new(&mut options.context)
                .range(0..=100)
                .prefix("Context lines "),
        );
        ui.checkbox(&mut options.align, "Align changes");
        ui.checkbox(&mut options.whitespace, "Show whitespaces");
        ui.checkbox(&mut options.line_numbers, "Show line numbers");
        ui.checkbox(
            &mut options.next_file,
            "Go to next file after the last change",
        );
        ui.checkbox(
            &mut options.auto_nonconflicting,
            "Auto-apply non-conflicting changes",
        );
        ui.checkbox(
            &mut options.enable_nonconflicting,
            "Enable merge of non-conflicting changes",
        );
        ui.checkbox(&mut options.editor_tabs, "Open comparisons in editor tabs");
        egui::ComboBox::from_id_salt("default-diff-ignore")
            .selected_text(format!("Ignore {:?}", options.ignore))
            .show_ui(ui, |ui| {
                for value in [
                    rustpy_settings::diff_merge::Ignore::None,
                    rustpy_settings::diff_merge::Ignore::Leading,
                    rustpy_settings::diff_merge::Ignore::Trailing,
                    rustpy_settings::diff_merge::Ignore::All,
                ] {
                    ui.selectable_value(&mut options.ignore, value, format!("{value:?}"));
                }
            })
            .combo_named(crate::i18n::tr_static("Ignore"));
        egui::ComboBox::from_id_salt("default-diff-highlight")
            .selected_text(format!("Highlight {:?}", options.highlight))
            .show_ui(ui, |ui| {
                for value in [
                    rustpy_settings::diff_merge::Highlight::Line,
                    rustpy_settings::diff_merge::Highlight::Word,
                    rustpy_settings::diff_merge::Highlight::Character,
                    rustpy_settings::diff_merge::Highlight::Split,
                    rustpy_settings::diff_merge::Highlight::None,
                ] {
                    ui.selectable_value(&mut options.highlight, value, format!("{value:?}"));
                }
            })
            .combo_named(crate::i18n::tr_static("Highlight"));
        ui.separator();
        ui.label("External tools (program invoked directly; no shell)");
        ui.checkbox(
            &mut options.external_files,
            "Use external diff tool for files",
        );
        ui.checkbox(
            &mut options.external_folders,
            "Use external diff tool for folders",
        );
        ui.label("Diff program");
        ui.text_edit_singleline(&mut options.diff_program);
        ui.label("Diff arguments ($LEFT$, $RIGHT$)");
        ui.text_edit_singleline(&mut options.diff_arguments);
        ui.label("Merge program");
        ui.text_edit_singleline(&mut options.merge_program);
        ui.label("Merge arguments ($LEFT$, $BASE$, $RIGHT$, $OUTPUT$)");
        ui.text_edit_singleline(&mut options.merge_arguments);
    }
}
