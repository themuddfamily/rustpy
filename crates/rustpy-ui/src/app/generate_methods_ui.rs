//! Generate selections are captured from the current document, never inferred
//! from display labels or reapplied to a different tab after a popup opens.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_refactor::generation::{
    self, ClassTarget, InheritedMethod, Property, Request, TestSubject,
};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct GenerateMethodsUi {
    session: Option<Session>,
}
struct Session {
    document: rustpy_text::DocumentId,
    version: u64,
    source: String,
    project: Option<PathBuf>,
    kind: Kind,
    error: Option<String>,
}
enum Kind {
    Members {
        target: Box<ClassTarget>,
        request: Request,
        fields: BTreeSet<String>,
        inherited: Vec<InheritedMethod>,
        selected: BTreeSet<usize>,
    },
    Tests {
        source_path: PathBuf,
        subjects: Vec<TestSubject>,
        selected: BTreeSet<usize>,
        pytest: bool,
        framework: String,
        target_dir: PathBuf,
        file_name: String,
        class_name: String,
        method_prefix: String,
        setup: bool,
        teardown: bool,
    },
}
impl GenerateMethodsUi {
    pub(super) fn active(&self) -> bool {
        self.session.is_some()
    }
}

impl IdeApp {
    pub(super) fn open_generate_members(&mut self, key: &str) {
        let Some(i) = self.active.filter(|i| *i < self.tabs.len()) else {
            return;
        };
        if !matches!(
            self.tabs[i].state.language,
            Language::Python | Language::Cython
        ) {
            self.set_status("Generate methods is available in Python source");
            return;
        }
        if self.tabs[i].state.doc.is_read_only() {
            self.set_status("Cannot generate in a read-only document");
            return;
        }
        let state = &mut self.tabs[i].state;
        let source = state.doc.text();
        let caret = state.primary().head();
        let root = state.syntax().syntax_node();
        let mut target = match ClassTarget::capture(&source, &root, caret) {
            Ok(target) => target,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        let inherited = self
            .analysis_file(i)
            .map(|(db, file)| {
                generation::resolve_class_facts(&db, &file, &mut target);
                generation::inherited_methods(&db, &file, &target)
            })
            .unwrap_or_default();
        let mut request = Request::default();
        match key {
            "__init__" => request.constructor = true,
            "__eq__" => request.dunders = vec!["__eq__".into(), "__hash__".into()],
            "__lt__" => {
                request.dunders = vec![
                    "__lt__".into(),
                    "__le__".into(),
                    "__gt__".into(),
                    "__ge__".into(),
                ]
            }
            "property" => {
                if let Some(field) = target.fields.iter().find(|field| !field.init_only) {
                    request.property = Some(Property {
                        field: field.name.clone(),
                        getter: true,
                        setter: true,
                        deleter: false,
                    });
                }
            }
            "__slots__" => request.slots = true,
            "__post_init__" => request.post_init = true,
            "override" => {}
            key if generation::DUNDERS.iter().any(|(name, _)| *name == key) => {
                request.dunders.push(key.into())
            }
            _ => {}
        }
        let fields = target
            .fields
            .iter()
            .map(|field| field.name.clone())
            .collect();
        let state = &self.tabs[i].state;
        self.generate_methods.session = Some(Session {
            document: state.doc.identity(),
            version: state.doc.version(),
            source,
            project: self.project.as_ref().map(|project| project.root.clone()),
            kind: Kind::Members {
                target: Box::new(target),
                request,
                fields,
                inherited,
                selected: BTreeSet::new(),
            },
            error: None,
        });
        self.completion.visible = false;
        self.choice = None;
    }
    pub(super) fn open_generate_test_picker(&mut self, source_path: PathBuf) {
        let Some(i) = self.active.filter(|i| *i < self.tabs.len()) else {
            return;
        };
        let state = &self.tabs[i].state;
        let source = state.doc.text();
        if state.path.as_ref() != Some(&source_path) {
            self.set_status("Return to the source file before creating its test");
            return;
        }
        let subjects = generation::test_subjects(&source);
        if subjects.is_empty() {
            self.set_status("No functions or methods are available to test");
            return;
        }
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_else(|| source_path.parent().unwrap_or(Path::new(".")).to_path_buf());
        let pytest = rustpy_test::configured_framework(&root, &self.settings.python_testing.runner)
            .unwrap_or(rustpy_test::Framework::Unittest)
            == rustpy_test::Framework::Pytest;
        let default_target = rustpy_test::related::new_test_path(&root, &source_path);
        let module = source_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let pattern = &self.settings.python_testing.test_file_pattern;
        let file_name = if pattern.trim().is_empty() {
            default_target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        } else {
            pattern.replace("{module}", &module)
        };
        self.generate_methods.session = Some(Session {
            document: state.doc.identity(),
            version: state.doc.version(),
            source,
            project: self.project.as_ref().map(|project| project.root.clone()),
            kind: Kind::Tests {
                source_path,
                subjects,
                selected: BTreeSet::new(),
                pytest,
                framework: "auto".into(),
                target_dir: default_target.parent().unwrap_or(&root).into(),
                file_name,
                class_name: self.settings.python_testing.test_class_name.clone(),
                method_prefix: self.settings.python_testing.test_method_prefix.clone(),
                setup: self.settings.python_testing.setup,
                teardown: self.settings.python_testing.teardown,
            },
            error: None,
        });
    }
    fn generate_session_current(&self, session: &Session) -> bool {
        self.active
            .and_then(|i| self.tabs.get(i))
            .is_some_and(|tab| {
                tab.state.doc.identity() == session.document
                    && tab.state.doc.version() == session.version
                    && tab.state.doc.text() == session.source
                    && !tab.state.doc.is_read_only()
            })
            && self.project.as_ref().map(|project| project.root.clone()) == session.project
    }
    pub(super) fn apply_generate_selection(&mut self) -> bool {
        let Some(mut session) = self.generate_methods.session.take() else {
            return false;
        };
        if !self.generate_session_current(&session) {
            self.set_status("The source changed; reopen Generate");
            return false;
        }
        let result: Result<(), String> = match &mut session.kind {
            Kind::Members {
                target,
                request,
                fields,
                inherited,
                selected,
            } => {
                request.fields = fields.iter().cloned().collect();
                request.inherited = selected
                    .iter()
                    .map(|index| inherited[*index].clone())
                    .collect();
                let current = self.active.and_then(|i| self.analysis_file(i));
                let stale = request.inherited.iter().any(|method| {
                    current
                        .as_ref()
                        .and_then(|(db, _)| db.file_for_path(&method.source_path))
                        .is_none_or(|file| file.text.as_ref() != method.source.as_ref())
                });
                if stale {
                    Err("An inherited method changed; reopen Generate".into())
                } else {
                    generation::generate(target, request).and_then(|edits| {
                        let i = self.active.unwrap();
                        if self.tabs[i]
                            .state
                            .apply_text_edits(edits, "Generate members")
                        {
                            self.focus_editor = true;
                            Ok(())
                        } else {
                            Err("The document refused generation".into())
                        }
                    })
                }
            }
            Kind::Tests {
                source_path,
                subjects,
                selected,
                pytest,
                framework,
                target_dir,
                file_name,
                class_name,
                method_prefix,
                setup,
                teardown,
            } => {
                let root = session.project.clone().unwrap_or_else(|| {
                    source_path.parent().unwrap_or(Path::new(".")).to_path_buf()
                });
                let chosen = selected
                    .iter()
                    .map(|index| subjects[*index].clone())
                    .collect::<Vec<_>>();
                (if framework == "doctest" {
                    super::test_frameworks::doctest_source(
                        &rustpy_test::related::import_path(&root, source_path),
                        &chosen,
                    )
                } else {
                    generation::test_source(
                        &rustpy_test::related::import_path(&root, source_path),
                        &chosen,
                        if framework == "auto" {
                            *pytest
                        } else {
                            framework == "pytest"
                        },
                    )
                })
                .and_then(|source| {
                    super::test_frameworks::customize_test_source(
                        source,
                        if framework == "auto" {
                            if *pytest {
                                "pytest"
                            } else {
                                "unittest"
                            }
                        } else {
                            framework
                        },
                        class_name,
                        *setup,
                        *teardown,
                    )
                })
                .and_then(|source| {
                    let source = source.replace("def test_", &format!("def {method_prefix}"));
                    if !rustpy_syntax::parse(&source).errors().is_empty() {
                        return Err("Choose a valid test method prefix".into());
                    }
                    Ok(source)
                })
                .and_then(|source| {
                    use std::io::Write;
                    if Path::new(file_name.as_str())
                        .file_name()
                        .and_then(|name| name.to_str())
                        != Some(file_name.as_str())
                        || !file_name.ends_with(".py")
                    {
                        return Err(
                            "Choose a Python test filename without directory separators".into()
                        );
                    }
                    let target = target_dir.join(file_name.as_str());
                    if !target_dir.is_absolute() {
                        return Err("Choose an absolute target directory".into());
                    }
                    if target.exists() {
                        return Err("The test file already exists; open it instead".into());
                    }
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    let mut file = tempfile::NamedTempFile::new_in(
                        target.parent().ok_or("Test file has no parent")?,
                    )
                    .map_err(|error| error.to_string())?;
                    file.write_all(source.as_bytes())
                        .and_then(|_| file.as_file().sync_all())
                        .map_err(|error| error.to_string())?;
                    file.persist_noclobber(&target)
                        .map_err(|error| error.to_string())?;
                    if let Some(project) = &mut self.project {
                        project.tree.invalidate_all();
                    }
                    self.mark_nav_jump();
                    self.open_file(&target);
                    Ok(())
                })
            }
        };
        match result {
            Ok(()) => {
                self.set_status("Generation complete");
                true
            }
            Err(error) => {
                session.error = Some(error);
                self.generate_methods.session = Some(session);
                false
            }
        }
    }
    pub(super) fn generate_methods_keys(&mut self, ctx: &egui::Context) -> bool {
        if !self.generate_methods.active() {
            return false;
        }
        self.run_panel.launcher.reset_ctrl_gesture();
        self.search_everywhere_gesture.reset();
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.generate_methods.session = None;
            self.focus_editor = true;
        }
        true
    }
    pub(super) fn generate_methods_popup(&mut self, ctx: &egui::Context) {
        let Some(session) = &mut self.generate_methods.session else {
            return;
        };
        let mut apply = false;
        let mut cancel = false;
        // Editor-attached chooser: its keys are routed through the editor, so
        // the editor must stay in the focus cycle.
        egui::Window::new("Generate")
            .id(egui::Id::new("generate_methods_selection"))
            .collapsible(false)
            .resizable(true)
            .show_modeless(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| match &mut session.kind {
                        Kind::Members {
                            target,
                            request,
                            fields,
                            inherited,
                            selected,
                        } => {
                            ui.label(format!("Class {}", target.name));
                            ui.label("Fields");
                            for field in &target.fields {
                                let mut checked = fields.contains(&field.name);
                                if ui.checkbox(&mut checked, &field.name).changed() {
                                    if checked {
                                        fields.insert(field.name.clone());
                                    } else {
                                        fields.remove(&field.name);
                                    }
                                }
                            }
                            ui.separator();
                            ui.checkbox(&mut request.constructor, "Constructor");
                            for (name, label) in generation::DUNDERS {
                                let mut checked =
                                    request.dunders.iter().any(|selected| selected == name);
                                if ui
                                    .checkbox(&mut checked, format!("{name} — {label}"))
                                    .changed()
                                {
                                    if checked {
                                        request.dunders.push((*name).into());
                                    } else {
                                        request.dunders.retain(|selected| selected != name);
                                    }
                                }
                            }
                            ui.checkbox(&mut request.slots, "__slots__");
                            if target.dataclass {
                                ui.checkbox(&mut request.post_init, "__post_init__");
                            }
                            ui.separator();
                            let mut property = request.property.is_some();
                            if ui.checkbox(&mut property, "Property").changed() {
                                request.property = if property {
                                    target.fields.iter().find(|field| !field.init_only).map(
                                        |field| Property {
                                            field: field.name.clone(),
                                            getter: true,
                                            setter: true,
                                            deleter: false,
                                        },
                                    )
                                } else {
                                    None
                                };
                            }
                            if let Some(property) = &mut request.property {
                                egui::ComboBox::from_id_salt("generate_property_field")
                                    .selected_text(&property.field)
                                    .show_ui(ui, |ui| {
                                        for field in
                                            target.fields.iter().filter(|field| !field.init_only)
                                        {
                                            ui.selectable_value(
                                                &mut property.field,
                                                field.name.clone(),
                                                &field.name,
                                            );
                                        }
                                    })
                                    .combo_named(crate::i18n::tr_static("Property field"));
                                ui.checkbox(&mut property.getter, "Getter");
                                ui.checkbox(&mut property.setter, "Setter");
                                ui.checkbox(&mut property.deleter, "Deleter");
                            }
                            if !inherited.is_empty() {
                                ui.separator();
                                ui.label("Inherited methods");
                                for (index, method) in inherited.iter().enumerate() {
                                    let mut checked = selected.contains(&index);
                                    if ui
                                        .checkbox(
                                            &mut checked,
                                            format!(
                                                "{} — {}{}",
                                                method.signature,
                                                method.owner,
                                                if method.abstract_ { " (abstract)" } else { "" }
                                            ),
                                        )
                                        .changed()
                                    {
                                        if checked {
                                            selected.insert(index);
                                        } else {
                                            selected.remove(&index);
                                        }
                                    }
                                }
                            }
                        }
                        Kind::Tests {
                            subjects,
                            selected,
                            pytest,
                            framework,
                            target_dir,
                            file_name,
                            class_name,
                            method_prefix,
                            setup,
                            teardown,
                            ..
                        } => {
                            egui::ComboBox::from_id_salt("create-test-framework")
                                .selected_text(if framework == "auto" {
                                    if *pytest {
                                        "pytest"
                                    } else {
                                        "unittest"
                                    }
                                } else {
                                    framework.as_str()
                                })
                                .show_ui(ui, |ui| {
                                    for name in
                                        ["pytest", "unittest", "doctest", "nose", "nose2", "trial"]
                                    {
                                        ui.selectable_value(
                                            framework,
                                            name.into(),
                                            crate::i18n::tr_static(name),
                                        );
                                    }
                                })
                                .combo_named(crate::i18n::tr_static("Test framework"));
                            let mut directory = target_dir.to_string_lossy().into_owned();
                            ui.label("Target directory");
                            if ui.text_edit_singleline(&mut directory).changed() {
                                *target_dir = directory.into();
                            }
                            ui.label("Test file name");
                            ui.text_edit_singleline(file_name);
                            ui.label("Test class name (optional)");
                            ui.text_edit_singleline(class_name);
                            ui.label("Test method prefix");
                            ui.text_edit_singleline(method_prefix);
                            ui.checkbox(setup, "Generate setUp / setup_method");
                            ui.checkbox(teardown, "Generate tearDown / teardown_method");
                            ui.label("Functions and methods");
                            for (index, subject) in subjects.iter().enumerate() {
                                let mut checked = selected.contains(&index);
                                if ui.checkbox(&mut checked, &subject.key).changed() {
                                    if checked {
                                        selected.insert(index);
                                    } else {
                                        selected.remove(&index);
                                    }
                                }
                            }
                        }
                    });
                if let Some(error) = &session.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    apply = ui.button("Generate").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if cancel {
            self.generate_methods.session = None;
            self.focus_editor = true;
        } else if apply {
            self.apply_generate_selection();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(source: &str) -> (IdeApp, tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subject.py");
        std::fs::write(&path, source).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.open_file(&path);
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::from(
                (source.find("class ").map(|at| at + 6).unwrap_or(0)) as u32,
            ))]);
        (app, dir, path)
    }
    fn owned_frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::LayerId {
        let mut editor_layer = None;
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                let editor = egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                editor_layer = Some(editor.response.layer_id);
                app.draw_choice_popup(ui.ctx());
                app.generate_methods_popup(ui.ctx());
            },
        )
        .drop_without_applying_deltas();
        editor_layer.unwrap()
    }
    fn owned_key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            },
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
        ]
    }
    #[test]
    fn actual_generate_modal_owns_paste_then_escape_restores_real_editor_input() {
        let source = "class Record:\n    π: int\n    pass\n";
        let (mut app, _dir, path) = fixture(source);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let editor_layer = owned_frame(&mut app, &ctx, 0.0, vec![]);
        assert!(app.tabs[0].view.focused);
        let editor_caret = app.tabs[0]
            .view
            .caret_screen_rect
            .expect("real editor caret geometry")
            .center();
        let selections = app.tabs[0].state.selections();
        let offset = app.tabs[0].state.primary().head();
        owned_frame(
            &mut app,
            &ctx,
            0.1,
            owned_key(
                egui::Key::Insert,
                egui::Modifiers {
                    alt: true,
                    ..Default::default()
                },
            ),
        );
        assert!(matches!(
            app.choice.as_ref().map(|choice| choice.kind),
            Some(ChoiceKind::Generate)
        ));
        owned_frame(
            &mut app,
            &ctx,
            0.2,
            vec![egui::Event::Text("__repr__".into())],
        );
        let choice = app.choice.as_ref().unwrap();
        assert_eq!(choice.items[choice.current().unwrap()].0, "__repr__");
        owned_frame(
            &mut app,
            &ctx,
            0.3,
            owned_key(egui::Key::Enter, egui::Modifiers::NONE),
        );
        assert!(app.generate_methods.active());
        assert!(app.choice.is_none());
        // Reacquire the actual host editor through a physical click while
        // Generate is open. This catches a raw-input leak even though the
        // preceding chooser had surrendered keyboard focus.
        assert_eq!(
            ctx.layer_id_at(editor_caret),
            Some(editor_layer),
            "the real caret point must be outside the Generate window"
        );
        for (time, pressed) in [(0.31, true), (0.32, false)] {
            owned_frame(
                &mut app,
                &ctx,
                time,
                vec![
                    egui::Event::PointerMoved(editor_caret),
                    egui::Event::PointerButton {
                        pos: editor_caret,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        owned_frame(
            &mut app,
            &ctx,
            0.4,
            vec![egui::Event::Paste("DO_NOT_INSERT_🙂".into())],
        );
        assert!(app.generate_methods.active());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert_eq!(app.tabs[0].state.selections(), selections);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert!(!app.editor_keyboard());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        owned_frame(
            &mut app,
            &ctx,
            0.5,
            owned_key(egui::Key::Escape, egui::Modifiers::NONE),
        );
        assert!(!app.generate_methods.active());
        assert!(app.editor_keyboard());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert_eq!(app.tabs[0].state.selections(), selections);
        assert!(!app.tabs[0].state.doc.can_undo());
        owned_frame(&mut app, &ctx, 0.6, vec![egui::Event::Paste("X".into())]);
        let mut expected = source.to_string();
        expected.insert(usize::from(offset), 'X');
        assert_eq!(app.tabs[0].state.doc.text(), expected);
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert_eq!(app.tabs[0].state.selections(), selections);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
    #[test]
    fn actual_generate_selection_applies_selected_fields_once_and_undo_restores_selection() {
        let source = "class Record:\n    one: int\n    two: str\n    pass\n";
        let (mut app, _dir, path) = fixture(source);
        let carets = app.tabs[0].state.selections();
        app.generate_member("__repr__".into());
        assert!(app.generate_methods.active());
        if let Kind::Members { fields, .. } =
            &mut app.generate_methods.session.as_mut().unwrap().kind
        {
            fields.remove("one");
        }
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.generate_methods_popup(ui.ctx())
        })
        .drop_without_applying_deltas();
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert!(app.apply_generate_selection());
        assert!(!app.generate_methods.active());
        let changed = app.tabs[0].state.doc.text();
        assert!(changed.contains("two={self.two!r}"));
        assert!(!changed.contains("one={self.one!r}"));
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert_eq!(app.tabs[0].state.selections(), carets);
        assert!(app.tabs[0].state.redo());
        assert_eq!(app.tabs[0].state.doc.text(), changed);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
    #[test]
    fn stale_source_and_readonly_refuse_actual_generation_without_history() {
        let source = "class Record:\n    pass\n";
        let (mut app, _dir, _path) = fixture(source);
        app.generate_member("__str__".into());
        assert!(app.generate_methods.active());
        app.tabs[0]
            .state
            .insert_text("x", &crate::editor::EditOptions::default());
        let changed = app.tabs[0].state.doc.text();
        assert!(!app.apply_generate_selection());
        assert_eq!(app.tabs[0].state.doc.text(), changed);
        app.tabs[0].state.undo();
        app.tabs[0].state.doc.set_read_only(true);
        app.generate_member("__str__".into());
        assert!(!app.generate_methods.active());
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    #[test]
    fn create_test_picker_uses_actual_declared_methods_and_framework_without_replacing_source() {
        let source = "class Service:\n    def first(self):\n        pass\n    def second(self):\n        pass\n\ndef utility():\n    pass\n";
        for pytest in [true, false] {
            let (mut app, dir, path) = fixture(source);
            app.open_generate_test_picker(path.clone());
            if let Kind::Tests {
                selected,
                subjects,
                pytest: framework,
                ..
            } = &mut app.generate_methods.session.as_mut().unwrap().kind
            {
                *framework = pytest;
                for (index, subject) in subjects.iter().enumerate() {
                    if subject.key == "Service.second" || subject.key == "utility" {
                        selected.insert(index);
                    }
                }
            }
            assert!(app.apply_generate_selection());
            let target = rustpy_test::related::new_test_path(dir.path(), &path);
            let generated = std::fs::read_to_string(&target).unwrap();
            assert!(generated.contains("def test_second(self):"));
            assert!(!generated.contains("test_first"));
            assert!(generated.contains("test_utility"));
            assert_eq!(generated.contains("unittest.TestCase"), !pytest);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
            assert_eq!(
                app.tabs[app.active.unwrap()].state.path.as_ref(),
                Some(&target)
            );
        }
    }
}
