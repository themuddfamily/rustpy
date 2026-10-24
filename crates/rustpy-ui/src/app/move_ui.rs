//! Move destinations and compatibility choices over the shared preview/Undo API.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_refactor::{
    movement::{self, MoveOptions},
    refactor2::{self, FileOp},
};
use rustpy_syntax::ast::{self, AstNode};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MoveKind {
    Symbol,
    Module,
    MakeFunction,
    MakeStatic,
    ViaParameter,
    ClassVariable,
    NestedFunction,
}
pub(super) struct MoveState {
    path: PathBuf,
    source: String,
    offset: TextSize,
    kind: MoveKind,
    target: String,
    class_name: String,
    parameter: String,
    name: String,
    dependencies: Vec<(String, bool)>,
    move_usages: bool,
    keep_reexports: bool,
    browser: FileBrowser,
    error: Option<String>,
}
impl IdeApp {
    pub(super) fn open_move_dialog(&mut self) {
        let Some(index) = self.active else {
            return;
        };
        self.sync_open_tabs_now();
        let Some((analysis, file)) = self.analysis_file(index) else {
            self.set_status("Move requires a Python source file");
            return;
        };
        let offset = self.tabs[index].state.primary().head();
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        let function = rustpy_semantic::edit_helpers::ancestor_at::<ast::FuncDef>(
            &file.parse.syntax_node(),
            offset,
        );
        let kind = if function
            .as_ref()
            .is_some_and(|function| function.is_method())
        {
            MoveKind::MakeFunction
        } else if function.as_ref().is_some_and(|function| {
            function
                .syntax()
                .ancestors()
                .skip(1)
                .any(|node| node.kind() == rustpy_syntax::SyntaxKind::FUNC_DEF)
        }) {
            MoveKind::NestedFunction
        } else {
            MoveKind::Symbol
        };
        let dependencies = movement::dependencies(&file, offset)
            .unwrap_or_default()
            .into_iter()
            .map(|name| (name, true))
            .collect();
        let name = function
            .map(|function| function.name_text())
            .unwrap_or_else(|| "moved".into());
        let _ = analysis;
        self.dialog = Dialog::Move(Box::new(MoveState {
            path: file.path.clone(),
            source: file.text.to_string(),
            offset,
            kind,
            target: String::new(),
            class_name: String::new(),
            parameter: String::new(),
            name,
            dependencies,
            move_usages: true,
            keep_reexports: true,
            browser: FileBrowser::new(root),
            error: None,
        }));
    }
    pub(super) fn draw_move_dialog(&mut self, ctx: &egui::Context, mut state: Box<MoveState>) {
        let mut open = true;
        let mut apply = false;
        let mut preview = false;
        let mut cancel = false;
        egui::Window::new("Move To…").collapsible(false).resizable(true).default_width(680.0).open(&mut open).show_modal(ctx,|ui|{
            ui.label(state.path.display().to_string());
            egui::ComboBox::from_id_salt("move-operation").selected_text(kind_label(state.kind)).show_ui(ui,|ui|{for kind in [MoveKind::Symbol,MoveKind::Module,MoveKind::MakeFunction,MoveKind::MakeStatic,MoveKind::ViaParameter,MoveKind::ClassVariable,MoveKind::NestedFunction]{ui.selectable_value(&mut state.kind,kind,kind_label(kind));}}).combo_named(crate::i18n::tr_static("Move operation"));
            if !matches!(state.kind,MoveKind::ClassVariable|MoveKind::NestedFunction|MoveKind::ViaParameter){
                ui.horizontal(|ui|{ui.label(if state.kind==MoveKind::Module{"Destination directory or full path:"}else{"Destination module (.py):"});ui.text_edit_singleline(&mut state.target);});
                ui.collapsing("Browse destination",|ui|{if let Some(path)=state.browser.ui(ui,&self.theme,true,220.0){state.target=path.to_string_lossy().into_owned();}
if ui.button("Use shown directory").clicked(){state.target=state.browser.path_input.clone();}});
            }
            if state.kind==MoveKind::MakeStatic{ui.horizontal(|ui|{ui.label("Destination class:");ui.text_edit_singleline(&mut state.class_name);});}
            if matches!(state.kind,MoveKind::MakeFunction|MoveKind::ViaParameter){ui.horizontal(|ui|{ui.label("New callable name:");ui.text_edit_singleline(&mut state.name);});}
            if state.kind==MoveKind::ViaParameter{ui.horizontal(|ui|{ui.label("Typed receiver parameter:");ui.text_edit_singleline(&mut state.parameter);});ui.label("The destination parameter becomes the receiver; the source receiver remains an explicit argument. The original descriptor delegates to the moved implementation.");}
            if matches!(state.kind,MoveKind::Symbol|MoveKind::MakeFunction){ui.label("Move source dependencies with the declaration:");for (name,selected) in &mut state.dependencies{ui.checkbox(selected,name.as_str());}ui.checkbox(&mut state.keep_reexports,"Keep old-module compatibility reexports");}
            if matches!(state.kind,MoveKind::Symbol|MoveKind::MakeFunction|MoveKind::MakeStatic){ui.checkbox(&mut state.move_usages,"Move resolved usages to the destination");}
            if matches!(state.kind,MoveKind::MakeFunction|MoveKind::MakeStatic){ui.label("The old callable remains as a forwarding compatibility bridge for method values, external callers and unknown receivers.");}
            if state.kind==MoveKind::ClassVariable{ui.label("Promote one immutable literal self.attribute default into a plain class. Custom storage and descriptors are conflicts.");}
            if let Some(error)=&state.error{ui.colored_label(self.theme.error,error);}
            ui.horizontal(|ui|{preview=ui.button("Preview").clicked();apply=ui.button("Refactor and Move").clicked();cancel=ui.button("Cancel").clicked()||ui.input(|input|input.key_pressed(egui::Key::Escape));});
        });
        if apply || preview {
            match self.plan_move(&state) {
                Ok(operations) => {
                    if preview {
                        self.preview_file_ops(operations, "Move");
                    } else {
                        self.apply_file_ops(operations, "Move");
                    }
                    self.focus_editor = true;
                    return;
                }
                Err(error) => state.error = Some(error),
            }
        }
        if open && !cancel {
            self.dialog = Dialog::Move(state);
        } else {
            self.focus_editor = true;
        }
    }
    fn plan_move(&mut self, state: &MoveState) -> Result<Vec<FileOp>, String> {
        self.sync_open_tabs_now();
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_ref() == Some(&state.path))
            .ok_or("Source is no longer open")?;
        let (analysis, file) = self.analysis_file(index).ok_or("Cannot analyze source")?;
        if file.text.as_ref() != state.source.as_str() {
            return Err("Source changed; reopen Move".into());
        }
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .ok_or("Open a project first")?;
        let paths = self.project_files();
        let target = if state.target.trim().is_empty() {
            file.path.clone()
        } else {
            let path = PathBuf::from(state.target.trim());
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        };
        if matches!(
            state.kind,
            MoveKind::Symbol | MoveKind::MakeFunction | MoveKind::MakeStatic
        ) && target.extension().and_then(|extension| extension.to_str()) != Some("py")
        {
            return Err("Choose a Python .py destination".into());
        }
        if target.exists() {
            let _ = analysis.file_for_path(&target);
        }
        let options = MoveOptions {
            dependencies: state
                .dependencies
                .iter()
                .filter(|(_, selected)| *selected)
                .map(|(name, _)| name.clone())
                .collect(),
            move_usages: state.move_usages,
            keep_reexports: state.keep_reexports,
            prefer_from_imports: self.settings.refactoring.prefer_from_imports,
        };
        match state.kind {
            MoveKind::Symbol => movement::move_symbols(
                &analysis,
                &file,
                state.offset,
                &target,
                &root,
                &paths,
                &options,
            ),
            MoveKind::Module => {
                let source = if file.is_package_init() {
                    file.path
                        .parent()
                        .ok_or("Missing package directory")?
                        .to_owned()
                } else {
                    file.path.clone()
                };
                let destination = if target.is_dir() {
                    target.join(source.file_name().ok_or("Missing file name")?)
                } else {
                    target
                };
                refactor2::move_module(&analysis, &root, &source, &destination, &paths)
            }
            MoveKind::MakeFunction => movement::make_function_in_module(
                &analysis,
                &file,
                state.offset,
                state.name.trim(),
                &target,
                rustpy_refactor::ProjectEditContext {
                    root: &root,
                    files: &paths,
                },
                &options,
            ),
            MoveKind::MakeStatic => {
                let destination = analysis
                    .file_for_path(&target)
                    .ok_or("Choose an existing destination module")?;
                let class = destination
                    .parse
                    .syntax_node()
                    .children()
                    .filter_map(ast::ClassDef::cast)
                    .find(|class| class.name_text() == state.class_name.trim())
                    .ok_or("Choose an existing top-level destination class")?;
                movement::function_to_static(
                    &analysis,
                    &file,
                    state.offset,
                    &destination,
                    class.syntax().text_range().start(),
                    rustpy_refactor::ProjectEditContext {
                        root: &root,
                        files: &paths,
                    },
                    state.move_usages,
                )
            }
            MoveKind::ViaParameter => movement::method_via_parameter(
                &analysis,
                &file,
                state.offset,
                state.parameter.trim(),
                state.name.trim(),
            ),
            MoveKind::ClassVariable => movement::attribute_to_class(&file, state.offset),
            MoveKind::NestedFunction => {
                let plan = refactor::move_to_top_level(&file, state.offset)?;
                Ok(vec![FileOp::Edit(file.path.clone(), plan.edits)])
            }
        }
    }
}
fn kind_label(kind: MoveKind) -> &'static str {
    match kind {
        MoveKind::Symbol => "Top-level declaration and dependencies",
        MoveKind::Module => "Module/package to directory",
        MoveKind::MakeFunction => "Make Function (method to top level)",
        MoveKind::MakeStatic => "Make Static Method (function to class)",
        MoveKind::ViaParameter => "Instance method to receiver parameter's class",
        MoveKind::ClassVariable => "Instance attribute to class variable",
        MoveKind::NestedFunction => "Nested function to top level",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_move_dialog_captures_method_transfer_and_does_not_mutate_source() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        let source = "class C:\n    def run(self, value):\n        return value\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(
                source.find("def run").unwrap() as u32,
            ))]);
        app.open_move_dialog();
        let Dialog::Move(state) = std::mem::replace(&mut app.dialog, Dialog::None) else {
            panic!("missing typed Move dialog")
        };
        assert_eq!(state.kind, MoveKind::MakeFunction);
        assert_eq!(state.source, source);
        let mut state = Some(state);
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let mut shapes = Vec::new();
        // The first Area frame measures the window without painting its text.
        // Render the retained production state again for visible controls.
        for pass in 0..2 {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 1100.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    let context = context.ctx();
                    if let Some(state) = state.take() {
                        app.draw_move_dialog(context, state);
                    }
                },
            );
            shapes = std::mem::take(&mut output.shapes);
            // Retire CPU-only atlas updates before assertions can unwind.
            output.drop_without_applying_deltas();
            if pass == 0 {
                let Dialog::Move(retained) = std::mem::replace(&mut app.dialog, Dialog::None)
                else {
                    panic!("dialog disappeared during its sizing frame")
                };
                assert_eq!(retained.kind, MoveKind::MakeFunction);
                assert_eq!(retained.source, source);
                state = Some(retained);
            }
        }
        for label in [
            "Make Function (method to top level)",
            "Browse destination",
            "Move resolved usages to the destination",
            "Preview",
            "Refactor and Move",
        ] {
            assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains(label))),"{label}");
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
    #[test]
    fn source_changes_are_rejected_before_move_planning_and_project_moves_use_shared_preview() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        std::fs::write(&path, "def run():\n    return 1\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        settings.refactoring.preview_refactoring = true;
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.open_file(&path);
        app.open_move_dialog();
        let Dialog::Move(state) = std::mem::replace(&mut app.dialog, Dialog::None) else {
            panic!("missing Move dialog")
        };
        let index = app.active.unwrap();
        app.tabs[index].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "# changed\n".into())],
            "Change source",
        );
        assert!(app
            .plan_move(&state)
            .unwrap_err()
            .contains("Source changed"));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "def run():\n    return 1\n"
        );
    }
}
