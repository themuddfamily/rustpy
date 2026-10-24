//! Change Signature dialog (Ctrl+F6): edits a working copy of the
//! parameters (rename, default, kind, order, add, remove) and applies it in
//! one step to the definition and every call site in the project.

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_refactor::change_signature::{self, ParamKind, SigParam, SignatureModel};

pub(super) struct ChangeSignatureState {
    pub path: PathBuf,
    pub model: SignatureModel,
    /// `(calls to update, files, calls through untyped receivers)`.
    pub calls: (usize, usize, usize),
    pub new_name: String,
    pub new_kind: ParamKind,
    pub new_default: String,
    pub new_value: String,
    pub error: Option<String>,
}

fn kind_label(kind: ParamKind) -> &'static str {
    match kind {
        ParamKind::PositionalOnly => "positional-only",
        ParamKind::Normal => "normal",
        ParamKind::KeywordOnly => "keyword-only",
        ParamKind::VarArgs => "*args",
        ParamKind::KwArgs => "**kwargs",
    }
}

enum RowOp {
    Up(usize),
    Down(usize),
    Remove(usize),
}

impl IdeApp {
    /// Ctrl+F6: opens Change Signature for the function around the caret.
    pub(super) fn open_change_signature(&mut self) {
        let Some(i) = self.active else { return };
        self.sync_open_tabs_now();
        let Some((analysis, file)) = self.analysis_file(i) else {
            self.set_status("Change Signature works for Python files in a project");
            return;
        };
        let caret = self.tabs[i].state.primary().head();
        match change_signature::signature_target(&analysis, &file, caret) {
            Ok((file, model)) => {
                self.open_file(&file.path);
                let files = self.project_files();
                let calls = change_signature::call_site_counts(&analysis, &file, &model, &files);
                self.dialog = Dialog::ChangeSignature(Box::new(ChangeSignatureState {
                    path: file.path.clone(),
                    model,
                    calls,
                    new_name: String::new(),
                    new_kind: ParamKind::Normal,
                    new_default: String::new(),
                    new_value: String::new(),
                    error: None,
                }));
            }
            Err(e) => self.set_status(format!("Cannot change the signature: {e}")),
        }
    }

    pub(super) fn draw_change_signature(
        &mut self,
        ctx: &egui::Context,
        mut st: Box<ChangeSignatureState>,
    ) {
        let theme = self.theme.clone();
        let (mut open, mut refactor, mut preview, mut cancel) = (true, false, false, false);
        let mut row_op: Option<RowOp> = None;
        let mut add = false;
        let implicit = st.model.implicit_first;
        egui::Window::new(format!("Change Signature — {}", st.model.func_name)).collapsible(false).resizable(true).default_width(620.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            ui.label(egui::RichText::new("Parameters").strong());
            let n = st.model.params.len();
            egui::Grid::new("change_signature_params").num_columns(6).spacing([10.0, 4.0]).show(ui, |ui| {
                ui.label(egui::RichText::new("Name").color(theme.text_dim).small());
                let kind_header = ui.label(egui::RichText::new("Kind").color(theme.text_dim).small());
                ui.label(egui::RichText::new("Default").color(theme.text_dim).small());
                ui.label(egui::RichText::new("Annotation").color(theme.text_dim).small());
                ui.label(egui::RichText::new("Existing calls").color(theme.text_dim).small());
                ui.label("");
                ui.end_row();
                for (i, p) in st.model.params.iter_mut().enumerate() {
                    let fixed = implicit && i == 0;
                    let star = matches!(p.kind, ParamKind::VarArgs | ParamKind::KwArgs);
                    ui.add_enabled_ui(!fixed, |ui| ui.add_sized([140.0, 20.0], egui::TextEdit::singleline(&mut p.name).font(egui::TextStyle::Monospace)));
                    ui.add_enabled_ui(!fixed && !star, |ui| {
                        egui::ComboBox::from_id_salt(("sig_kind", i)).selected_text(kind_label(p.kind)).width(120.0).show_ui(ui, |ui| {
                            for k in [ParamKind::PositionalOnly, ParamKind::Normal, ParamKind::KeywordOnly] {
                                ui.selectable_value(&mut p.kind, k, kind_label(k));
                            }
                        }).combo_labelled(&kind_header);
                    });
                    let mut default = p.default.clone().unwrap_or_default();
                    let edited = ui.add_enabled_ui(!fixed && !star, |ui| ui.add_sized([120.0, 20.0], egui::TextEdit::singleline(&mut default).hint_text("none").font(egui::TextStyle::Monospace)).changed()).inner;
                    if edited {
                        p.default = Some(default).filter(|d| !d.trim().is_empty());
                    }
                    let mut annotation=p.annotation.clone().unwrap_or_default();
                    if ui.add_enabled(!fixed,egui::TextEdit::singleline(&mut annotation).desired_width(100.0)).changed() {
                        p.annotation=Some(annotation).filter(|value|!value.trim().is_empty());
                    }
                    let mut call_value=p.call_value.clone().unwrap_or_default();
                    if ui.add_enabled(p.origin.is_none() && !star,egui::TextEdit::singleline(&mut call_value).desired_width(100.0).hint_text("use default")).changed() {
                        p.call_value=Some(call_value).filter(|value|!value.trim().is_empty());
                    }
                    ui.horizontal(|ui| {
                        let lowest = usize::from(implicit);
                        if ui.add_enabled(i > lowest, egui::Button::new("Up").small()).on_hover_text("Move up").clicked() {
                            row_op = Some(RowOp::Up(i));
                        }
                        if ui.add_enabled(i >= lowest && i + 1 < n, egui::Button::new("Down").small()).on_hover_text("Move down").clicked() {
                            row_op = Some(RowOp::Down(i));
                        }
                        if ui.add_enabled(!fixed, egui::Button::new("Remove").small()).clicked() {
                            row_op = Some(RowOp::Remove(i));
                        }
                    });
                    ui.end_row();
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let add_label = ui.label("Add:");
                egui::ComboBox::from_id_salt("signature_new_kind").selected_text(kind_label(st.new_kind)).show_ui(ui,|ui| {
                    for kind in [ParamKind::PositionalOnly,ParamKind::Normal,ParamKind::VarArgs,ParamKind::KeywordOnly,ParamKind::KwArgs] {
                        ui.selectable_value(&mut st.new_kind,kind,kind_label(kind));
                    }
                }).combo_labelled(&add_label);
                ui.add(egui::TextEdit::singleline(&mut st.new_name).hint_text("name").desired_width(110.0));
                ui.add(egui::TextEdit::singleline(&mut st.new_default).hint_text("default (optional)").desired_width(120.0)).on_hover_text("Default value in the signature: used by calls that do not pass the parameter");
                ui.add(egui::TextEdit::singleline(&mut st.new_value).hint_text("value at call sites").desired_width(130.0)).on_hover_text("Passed by every existing call, also when the parameter has a default");
                if ui.add_enabled(!st.new_name.trim().is_empty(), egui::Button::new("Add")).clicked() {
                    add = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Return annotation:");
                let mut annotation=st.model.return_annotation.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut annotation).changed() {st.model.return_annotation=Some(annotation).filter(|value|!value.trim().is_empty());}
            });
            ui.checkbox(&mut st.model.propagate_overrides,"Propagate to overriding methods and their calls");
            ui.separator();
            let returns=st.model.return_annotation.as_ref().map(|value|format!(" -> {value}")).unwrap_or_default();
            ui.label(egui::RichText::new(format!("def {}({}){returns}", st.model.func_name, st.model.render())).monospace().color(theme.text));
            let (calls, files, untyped) = st.calls;
            let mut summary = format!("{calls} direct call site(s) in {files} file(s); overriding declarations and their calls are included when enabled.");
            let passed: Vec<String> = st.model.params.iter().filter(|p| p.origin.is_none()).filter_map(|p| p.call_value.as_ref().map(|v| format!("{}={v}", p.name))).collect();
            if !passed.is_empty() {
                summary.push_str(&format!(" Existing calls pass {}.", passed.join(", ")));
            }
            if untyped > 0 {
                summary.push_str(&format!(" {untyped} call(s) through untyped receivers are left alone."));
            }
            ui.label(egui::RichText::new(summary).color(theme.text_dim).small());
            let check = change_signature::validate(&st.model);
            if let Some(e) = st.error.as_ref().or(check.as_ref().err()) {
                ui.colored_label(theme.error, e);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(check.is_ok() && st.model.is_changed(),egui::Button::new("Preview")).clicked() {preview=true;}
                if ui.add_enabled(check.is_ok() && st.model.is_changed(), egui::Button::new("Refactor")).clicked() {
                    refactor = true;
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
        });
        match row_op {
            Some(RowOp::Up(i)) => st.model.params.swap(i, i - 1),
            Some(RowOp::Down(i)) => st.model.params.swap(i, i + 1),
            Some(RowOp::Remove(i)) => {
                st.model.params.remove(i);
            }
            None => {}
        }
        if row_op.is_some() {
            st.error = None;
        }
        if add {
            let mut p = SigParam::added(&st.new_name, Some(&st.new_default), Some(&st.new_value));
            p.kind = st.new_kind;
            if matches!(p.kind, ParamKind::VarArgs | ParamKind::KwArgs) {
                p.default = None;
                p.call_value = None;
            }
            let rank = |kind| match kind {
                ParamKind::PositionalOnly => 0,
                ParamKind::Normal => 1,
                ParamKind::VarArgs => 2,
                ParamKind::KeywordOnly => 3,
                ParamKind::KwArgs => 4,
            };
            let at = st
                .model
                .params
                .iter()
                .enumerate()
                .position(|(index, q)| {
                    index >= usize::from(implicit)
                        && (rank(q.kind) > rank(p.kind)
                            || (rank(q.kind) == rank(p.kind)
                                && matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly)
                                && p.default.is_none()
                                && q.default.is_some()))
                })
                .unwrap_or(st.model.params.len());
            st.model.params.insert(at, p);
            st.new_name.clear();
            st.new_default.clear();
            st.new_value.clear();
            st.error = None;
        }
        if refactor || preview {
            match self.apply_change_signature(&st, preview) {
                Ok(()) => {
                    self.focus_editor = true;
                    return;
                }
                Err(e) => st.error = Some(e),
            }
        }
        if open && !cancel {
            self.dialog = Dialog::ChangeSignature(st);
        } else {
            self.focus_editor = true;
        }
    }

    fn apply_change_signature(
        &mut self,
        st: &ChangeSignatureState,
        preview: bool,
    ) -> Result<(), String> {
        let i = self
            .tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(st.path.as_path()))
            .ok_or_else(|| format!("{} is no longer open", st.path.display()))?;
        self.sync_open_tabs_now();
        let (analysis, file) = self.analysis_file(i).ok_or("No analysis")?;
        let files = self.project_files();
        let (multi, summary) =
            change_signature::change_signature(&analysis, &file, &st.model, &files)?;
        if preview {
            self.preview_multi(multi);
        } else {
            self.apply_multi(Ok(multi));
        }
        if !summary.skipped.is_empty() {
            self.usages_title = format!(
                "Calls of '{}' to check by hand ({})",
                st.model.func_name,
                summary.skipped.len()
            );
            self.usages = summary
                .skipped
                .iter()
                .map(|(u, reason)| Usage {
                    line_text: format!("{}    ← {reason}", u.line_text.trim()),
                    ..u.clone()
                })
                .collect();
            self.bottom = Some(BottomTool::Find);
        }
        self.set_status(format!(
            "Change signature of '{}'{}: {}",
            st.model.func_name,
            if preview { " (preview)" } else { "" },
            summary.describe()
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_on_imported_call_edits_the_callee_instead_of_the_enclosing_function() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library.py");
        let caller = root.path().join("caller.py");
        std::fs::write(&library, "def send(value: int) -> int:\n    return value\n").unwrap();
        let text = "from library import send\ndef caller():\n    return send(1)\n";
        std::fs::write(&caller, text).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.open_file(&caller);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(
                text.rfind("send").unwrap() as u32,
            ))]);
        app.open_change_signature();
        let Dialog::ChangeSignature(state) = &app.dialog else {
            panic!("Change Signature not opened")
        };
        assert_eq!(state.path, library);
        assert_eq!(state.model.func_name, "send");
        assert_eq!(state.model.return_annotation.as_deref(), Some("int"));
        assert!(state.model.propagate_overrides);
        assert_eq!(std::fs::read_to_string(caller).unwrap(), text);
    }

    #[test]
    fn dialog_exposes_return_annotations_propagation_and_explicit_preview_without_mutating_source()
    {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        let text = "def send(value):\n    return value\n";
        std::fs::write(&path, text).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.open_file(&path);
        app.tabs[app.active.unwrap()]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(4))]);
        app.open_change_signature();
        let state = match std::mem::replace(&mut app.dialog, Dialog::None) {
            Dialog::ChangeSignature(state) => state,
            _ => panic!("missing signature dialog"),
        };
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let mut state = Some(state);
        let mut shapes = Vec::new();
        // Render the retained dialog after its invisible Area sizing frame.
        for pass in 0..2 {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 1000.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    let context = context.ctx();
                    if let Some(state) = state.take() {
                        app.draw_change_signature(context, state);
                    }
                },
            );
            shapes = std::mem::take(&mut output.shapes);
            output.drop_without_applying_deltas();
            if pass == 0 {
                let Dialog::ChangeSignature(retained) =
                    std::mem::replace(&mut app.dialog, Dialog::None)
                else {
                    panic!("signature dialog disappeared during its sizing frame")
                };
                assert_eq!(retained.path, path);
                state = Some(retained);
            }
        }
        for label in [
            "Annotation",
            "Existing calls",
            "Return annotation:",
            "Propagate to overriding methods and their calls",
            "Preview",
        ] {
            assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains(label))),"missing {label}");
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }
}
