//! Typed extraction choices; all edits pass through the shared refactoring transaction.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_refactor::{
    extraction::{self, CallableDestination, CallableOptions},
    refactor2::MultiFileRefactoring,
};
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtractRequest {
    Callable,
    Field,
    Parameter,
    Superclass,
}
pub(super) struct ExtractState {
    request: ExtractRequest,
    path: PathBuf,
    source: String,
    range: TextRange,
    name: String,
    destination: CallableDestination,
    duplicates: bool,
    current_method: bool,
    default: String,
    call_value: String,
    module: String,
    members: Vec<(String, bool)>,
    error: Option<String>,
}
impl IdeApp {
    pub(super) fn open_extract_dialog(&mut self, request: ExtractRequest, range: TextRange) {
        let Some(index) = self.active else {
            return;
        };
        self.sync_open_tabs_now();
        let Some((analysis, file)) = self.analysis_file(index) else {
            self.set_status("Extraction requires a Python source file");
            return;
        };
        let mut state = ExtractState {
            request,
            path: file.path.clone(),
            source: file.text.to_string(),
            range,
            name: "extracted".into(),
            destination: CallableDestination::Automatic,
            duplicates: false,
            current_method: false,
            default: String::new(),
            call_value: String::new(),
            module: String::new(),
            members: Vec::new(),
            error: None,
        };
        match request {
            ExtractRequest::Parameter => {
                match refactor::introduce_parameter(&analysis, &file, range) {
                    Ok(plan) => {
                        state.name = plan.name;
                        state.default =
                            rustpy_refactor::refactor::expression_candidates(&file, range.start())
                                .into_iter()
                                .find(|candidate| candidate == &range)
                                .map(|range| {
                                    file.text[usize::from(range.start())..usize::from(range.end())]
                                        .to_string()
                                })
                                .unwrap_or_default();
                    }
                    Err(error) => {
                        self.set_status(error);
                        return;
                    }
                }
            }
            ExtractRequest::Field => match refactor::introduce_field(&analysis, &file, range) {
                Ok(plan) => state.name = plan.name,
                Err(error) => {
                    self.set_status(error);
                    return;
                }
            },
            ExtractRequest::Superclass => {
                state.name = "Base".into();
                let Some(class) = hierarchy::class_at(&analysis, &file, range.start()) else {
                    self.set_status("Place the caret in a class");
                    return;
                };
                if let Some(info) =
                    rustpy_semantic::infer::Inferer::new(&analysis).class_info(&class)
                {
                    state.members = info
                        .members
                        .keys()
                        .map(|name| (name.to_string(), false))
                        .collect();
                    state.members.sort();
                }
            }
            ExtractRequest::Callable => {}
        }
        self.dialog = Dialog::Extract(Box::new(state));
    }
    pub(super) fn draw_extract_dialog(
        &mut self,
        ctx: &egui::Context,
        mut state: Box<ExtractState>,
    ) {
        let title = match state.request {
            ExtractRequest::Callable => "Extract Method / Function",
            ExtractRequest::Field => "Extract Field",
            ExtractRequest::Parameter => "Extract Parameter",
            ExtractRequest::Superclass => "Extract Superclass",
        };
        let mut open = true;
        let mut execute = false;
        let mut preview = false;
        let mut cancel = false;
        egui::Window::new(title).collapsible(false).resizable(true).default_width(580.0).open(&mut open).show_modal(ctx,|ui| {
            ui.horizontal(|ui| {ui.label("Name:");ui.add_enabled(state.request!=ExtractRequest::Field,egui::TextEdit::singleline(&mut state.name));});
            match state.request {
                ExtractRequest::Callable=>{
                    egui::ComboBox::from_id_salt("extract-destination").selected_text(destination_label(state.destination)).show_ui(ui,|ui| {
                        for destination in [CallableDestination::Automatic,CallableDestination::Function,CallableDestination::InstanceMethod,CallableDestination::StaticMethod,CallableDestination::ClassMethod] {ui.selectable_value(&mut state.destination,destination,destination_label(destination));}
                    }).combo_named(crate::i18n::tr_static("Destination"));
                    ui.checkbox(&mut state.duplicates,"Replace exact duplicate statement blocks in this scope");
                    ui.horizontal(|ui|{ui.label("Extract to module (optional):");ui.text_edit_singleline(&mut state.module);});
                    ui.label("A module destination extracts a top-level function and updates imports through Move Symbol.");
                    ui.label("Inputs and tuple outputs are inferred from bindings. Await and sync generator delegation are preserved; escaping break/continue, async generators and early returns produce conflicts.");
                },
                ExtractRequest::Field=>{ui.radio_value(&mut state.current_method,false,"Initialize in __init__ when dependencies permit");ui.radio_value(&mut state.current_method,true,"Initialize in the current method");ui.label("The introduced field can be renamed in place after application.");},
                ExtractRequest::Parameter=>{
                    ui.horizontal(|ui|{ui.label("Definition default:");ui.text_edit_singleline(&mut state.default);});
                    ui.horizontal(|ui|{ui.label("Value at existing calls:");ui.text_edit_singleline(&mut state.call_value);});
                    ui.label("Leave the call value blank to use the definition default. Resolved callers and compatible overrides are updated together.");
                },
                ExtractRequest::Superclass=>{
                    ui.label("Members to move:");egui::ScrollArea::vertical().max_height(220.0).show(ui,|ui|{for (name,selected) in &mut state.members {ui.checkbox(selected,name.as_str());}});
                    ui.horizontal(|ui|{ui.label("New module (optional):");ui.text_edit_singleline(&mut state.module);});
                    ui.label("A relative .py path is resolved from the project root. Imports and class callers follow the existing Move Symbol planner.");
                }
            }
            if let Some(error)=&state.error {ui.colored_label(self.theme.error,error);}
            ui.horizontal(|ui|{if ui.button("Preview").clicked(){preview=true;}
if ui.button("Refactor").clicked(){execute=true;}
if ui.button("Cancel").clicked()||ui.input(|input|input.key_pressed(egui::Key::Escape)){cancel=true;}});
        });
        if execute || preview {
            match self.plan_and_apply_extraction(&state, preview) {
                Ok(()) => {
                    self.focus_editor = true;
                    return;
                }
                Err(error) => state.error = Some(error),
            }
        }
        if open && !cancel {
            self.dialog = Dialog::Extract(state);
        } else {
            self.focus_editor = true;
        }
    }
    fn plan_and_apply_extraction(
        &mut self,
        state: &ExtractState,
        preview: bool,
    ) -> Result<(), String> {
        self.sync_open_tabs_now();
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_ref() == Some(&state.path))
            .ok_or("The source file is no longer open")?;
        let (analysis, file) = self
            .analysis_file(index)
            .ok_or("Cannot analyze the source file")?;
        if file.text.as_ref() != state.source.as_str() {
            return Err("The source changed while extraction was open; reopen the dialog".into());
        }
        let files = self.project_files();
        match state.request {
            ExtractRequest::Callable if nonempty(&state.module).is_some() => {
                let root = self
                    .project
                    .as_ref()
                    .map(|project| project.root.clone())
                    .ok_or("Open a project first")?;
                let target = PathBuf::from(state.module.trim());
                let target = if target.is_absolute() {
                    target
                } else {
                    root.join(target)
                };
                if target.extension().and_then(|extension| extension.to_str()) != Some("py") {
                    return Err("Choose a Python .py module".into());
                }
                let operations = extraction::extract_callable_to_module(
                    &analysis,
                    &file,
                    state.range,
                    state.name.trim(),
                    &CallableOptions {
                        destination: CallableDestination::Function,
                        replace_duplicates: state.duplicates,
                    },
                    &target,
                    rustpy_refactor::ProjectEditContext {
                        root: &root,
                        files: &files,
                    },
                )?;
                if preview {
                    self.preview_file_ops(operations, "Extract function to module");
                } else {
                    self.apply_file_ops(operations, "Extract function to module");
                }
            }
            ExtractRequest::Callable | ExtractRequest::Field => {
                let plan = if state.request == ExtractRequest::Callable {
                    extraction::extract_callable(
                        &analysis,
                        &file,
                        state.range,
                        state.name.trim(),
                        &CallableOptions {
                            destination: state.destination,
                            replace_duplicates: state.duplicates,
                        },
                    )?
                } else {
                    refactor::introduce_field_at(
                        &analysis,
                        &file,
                        state.range,
                        state.current_method,
                    )?
                };
                if preview {
                    self.preview_multi(MultiFileRefactoring {
                        title: plan.title,
                        changes: vec![(file.path.clone(), plan.edits)],
                    });
                } else {
                    self.apply_refactoring(Ok(plan));
                }
            }
            ExtractRequest::Parameter => {
                let (plan, summary) = extraction::extract_parameter(
                    &analysis,
                    &file,
                    state.range,
                    Some(state.name.trim()),
                    nonempty(&state.default),
                    nonempty(&state.call_value),
                    &files,
                )?;
                if preview {
                    self.preview_multi(plan);
                } else {
                    self.apply_multi(Ok(plan));
                }
                if !summary.skipped.is_empty() {
                    self.set_status(format!("Extract Parameter: {}", summary.describe()));
                }
            }
            ExtractRequest::Superclass => {
                let root = self
                    .project
                    .as_ref()
                    .map(|project| project.root.clone())
                    .ok_or("Open a project first")?;
                let target = nonempty(&state.module).map(|path| {
                    let path = PathBuf::from(path);
                    if path.is_absolute() {
                        path
                    } else {
                        root.join(path)
                    }
                });
                if target.as_ref().is_some_and(|path| {
                    path.extension().and_then(|value| value.to_str()) != Some("py")
                }) {
                    return Err("Choose a Python .py module".into());
                }
                let members = state
                    .members
                    .iter()
                    .filter(|(_, selected)| *selected)
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                if members.is_empty() {
                    return Err("Choose at least one member".into());
                }
                let operations = extraction::extract_superclass_to_module(
                    &analysis,
                    &file,
                    state.range.start(),
                    state.name.trim(),
                    &members,
                    target.as_deref(),
                    rustpy_refactor::ProjectEditContext {
                        root: &root,
                        files: &files,
                    },
                )?;
                if preview {
                    self.preview_file_ops(operations, "Extract superclass");
                } else {
                    self.apply_file_ops(operations, "Extract superclass");
                }
            }
        }
        Ok(())
    }
}
fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}
fn destination_label(destination: CallableDestination) -> &'static str {
    match destination {
        CallableDestination::Automatic => "Automatic method/function",
        CallableDestination::Function => "Top-level function",
        CallableDestination::InstanceMethod => "Instance method",
        CallableDestination::StaticMethod => "Static method",
        CallableDestination::ClassMethod => "Class method",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(source: &str) -> (tempfile::TempDir, IdeApp) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.open_file(&path);
        (root, app)
    }
    #[test]
    fn actual_extract_dialog_retains_typed_destination_duplicate_and_source_snapshot_controls() {
        let source = "def run(value):\n    result = value + 1\n    print(result)\n";
        let (_root, mut app) = app(source);
        let start = source.find("result =").unwrap();
        let end = source.find("    print").unwrap();
        app.open_extract_dialog(
            ExtractRequest::Callable,
            TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
        );
        let Dialog::Extract(state) = std::mem::replace(&mut app.dialog, Dialog::None) else {
            panic!("missing typed extract dialog")
        };
        assert_eq!(state.source, source);
        assert_eq!(state.destination, CallableDestination::Automatic);
        let mut state = Some(state);
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let mut shapes = Vec::new();
        // egui's Area deliberately paints its first frame invisibly while it
        // measures the window. Retain the real dialog state into its visible frame.
        for pass in 0..2 {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 900.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    let context = context.ctx();
                    if let Some(state) = state.take() {
                        app.draw_extract_dialog(context, state);
                    }
                },
            );
            shapes = std::mem::take(&mut output.shapes);
            // No GPU renderer participates in this fixture. Dispose atlas deltas
            // before any assertion can unwind and trigger a second panic.
            output.drop_without_applying_deltas();
            if pass == 0 {
                let Dialog::Extract(retained) = std::mem::replace(&mut app.dialog, Dialog::None)
                else {
                    panic!("extract dialog disappeared during its sizing frame")
                };
                assert_eq!(retained.source, source);
                assert_eq!(retained.destination, CallableDestination::Automatic);
                state = Some(retained);
            }
        }
        for label in [
            "Automatic method/function",
            "Replace exact duplicate statement blocks in this scope",
            "Preview",
            "Refactor",
        ] {
            assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains(label))),"{label}");
        }
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), source);
    }
    #[test]
    fn parameter_choices_capture_default_and_field_has_explicit_current_method_placement() {
        let source = "class C:\n    def run(self):\n        return 10\n";
        let (_root, mut app) = app(source);
        let start = source.find("10").unwrap();
        let range = TextRange::new(TextSize::new(start as u32), TextSize::new(start as u32 + 2));
        app.open_extract_dialog(ExtractRequest::Parameter, range);
        let Dialog::Extract(state) = &app.dialog else {
            panic!("missing parameter dialog")
        };
        assert_eq!(state.default, "10");
        assert_eq!(state.name, "number");
        app.open_extract_dialog(ExtractRequest::Field, range);
        let Dialog::Extract(state) = &app.dialog else {
            panic!("missing field dialog")
        };
        assert!(!state.current_method);
        assert_eq!(state.name, "number");
    }
}
