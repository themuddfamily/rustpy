//! Editor actions share the Alt+Enter initiating document/version/caret. No
//! delayed action may edit whichever tab happens to become active meanwhile.
use super::*;
use crate::a11y::DialogWindow;
use crate::actions::Action as IdeAction;
use rustpy_analysis::intentions::editor_actions::{self, EditorRequest, Refactoring};
pub(super) struct EditorState {
    owner: Owner,
    request: Request,
}
#[derive(Clone, Debug)]
pub(super) enum Request {
    Editor(EditorRequest),
    Install(String),
    AddRequirement(String),
    Replace {
        range: TextRange,
        text: String,
        title: String,
    },
}
fn requirement_actions(path: &Path, text: &str, caret: TextSize) -> Vec<(String, Request)> {
    if !path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("requirements") && name.ends_with(".txt"))
    {
        return Vec::new();
    }
    let offset = usize::from(caret);
    let mut out = Vec::new();
    for record in rustpy_packages::requirements_file::parse(text).records {
        if !(record.range.start..record.range.end).contains(&offset)
            && !(offset == text.len() && record.range.end == offset)
        {
            continue;
        }
        if let rustpy_packages::requirements_file::Syntax::Requirement {
            target: rustpy_packages::requirements_file::Target::Named(ref named),
            ref requirement,
            editable: false,
            ..
        } = record.syntax
        {
            out.push((
                format!("Install package '{requirement}'"),
                Request::Install(requirement.clone()),
            ));
            let normalized = named.to_string();
            // Only rewrite a single physical spec without comments/options: no
            // environment marker, URL, pip hash or include may be discarded.
            let original = &text[record.range.clone()];
            if !original.trim_end_matches(['\r', '\n']).contains('#')
                && record.text.trim() == requirement.trim()
                && normalized != requirement.trim()
            {
                let suffix = if original.ends_with("\r\n") {
                    "\r\n"
                } else if original.ends_with('\n') {
                    "\n"
                } else {
                    ""
                };
                out.push((
                    "Convert requirements spec to PEP 508".into(),
                    Request::Replace {
                        range: TextRange::new(
                            TextSize::new(record.range.start as u32),
                            TextSize::new(record.range.end as u32),
                        ),
                        text: format!("{normalized}{suffix}"),
                        title: "Convert requirements spec".into(),
                    },
                ));
            }
        } else if matches!(
            record.syntax,
            rustpy_packages::requirements_file::Syntax::Invalid
        ) {
            // A common legacy pin uses '='; fix only if the entire corrected
            // dependency is accepted by the authoritative PEP 508 parser.
            let raw = record.text.trim();
            if text[record.range.clone()].trim() != raw {
                continue;
            }
            if let Some((name, version)) = raw.split_once('=') {
                if !name.contains(['<', '>', '!', '~', '=']) && !version.contains('=') {
                    let normalized = format!("{name}=={version}");
                    if rustpy_packages::requirements_file::parse_named(&normalized).is_ok() {
                        let suffix = if text[record.range.clone()].ends_with('\n') {
                            "\n"
                        } else {
                            ""
                        };
                        out.push((
                            "Convert requirements spec to PEP 508".into(),
                            Request::Replace {
                                range: TextRange::new(
                                    TextSize::new(record.range.start as u32),
                                    TextSize::new(record.range.end as u32),
                                ),
                                text: format!("{normalized}{suffix}"),
                                title: "Convert requirements spec".into(),
                            },
                        ));
                    }
                }
            }
        }
    }
    out
}
impl IdeApp {
    pub(super) fn editor_context_requests(&mut self) -> Vec<(String, Request)> {
        let Some(index) = self.active else {
            return Vec::new();
        };
        let tab = &self.tabs[index];
        let caret = tab.state.primary().head();
        let text = tab.state.doc.text();
        let path = tab.state.path.clone();
        let selected = tab.state.primary().has_selection();
        let mut out = Vec::new();
        if let Some((_, file)) = self.analysis_file(index) {
            out.extend(
                editor_actions::requests(&file, caret)
                    .into_iter()
                    .filter(|(_, request)| {
                        !matches!(request, EditorRequest::Refactor(Refactoring::Method)) || selected
                    })
                    .map(|(title, request)| (title, Request::Editor(request))),
            );
            // Dependency suggestions come from actual import bindings, with the
            // maintained import-name→distribution mapping (yaml→PyYAML).
            if let Some(binding) = file
                .scopes
                .bindings
                .iter()
                .find(|binding| binding.node_range.contains_inclusive(caret))
            {
                if let rustpy_semantic::scope::BindingKind::Import(info) = &binding.kind {
                    if info.level == 0 && !info.module.is_empty() && !file.is_stub {
                        if let Some(distribution) =
                            rustpy_packages::distribution_for_module(&info.module)
                        {
                            let resolved = self
                                .analysis_file(index)
                                .and_then(|(db, _)| db.module(&info.module));
                            let project_module = resolved.as_ref().is_some_and(|file| {
                                self.project
                                    .as_ref()
                                    .is_some_and(|project| file.path.starts_with(&project.root))
                            });
                            let stdlib = rustpy_semantic::typeshed::is_stdlib(&info.module);
                            if !project_module && !stdlib {
                                out.push((
                                    format!("Add '{distribution}' to requirements"),
                                    Request::AddRequirement(distribution),
                                ));
                            }
                        }
                    }
                }
            }
        } else {
            out.extend(
                editor_actions::text_requests(&text, caret)
                    .into_iter()
                    .map(|(title, request)| (title, Request::Editor(request))),
            )
        }
        if let Some(path) = path {
            out.extend(requirement_actions(&path, &text, caret))
        }
        out
    }
    pub(super) fn begin_editor_request(&mut self, request: Request) {
        if let Some(owner) = self.intention_owner() {
            self.intention_menu.editor = Some(EditorState { owner, request });
        }
    }
    pub(super) fn editor_request_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.intention_menu.editor.take() else {
            return;
        };
        if !self.intention_owner_current(&state.owner) || self.non_text_source_hidden() {
            self.set_status("Editor target changed; reopen context actions");
            return;
        }
        let mut finished = true;
        match &mut state.request {
            Request::Editor(EditorRequest::Color { range, rgb }) => {
                let (mut apply, mut cancel) = (false, false);
                egui::Window::new("Color picker")
                    .collapsible(false)
                    .resizable(false)
                    .show_modal(ctx, |ui| {
                        ui.color_edit_button_srgb(rgb);
                        ui.monospace(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]));
                        ui.horizontal(|ui| {
                            apply = ui.button("Apply").clicked();
                            cancel = ui.button("Cancel").clicked();
                        });
                    });
                cancel |= ctx.input(|input| input.key_pressed(egui::Key::Escape));
                if apply {
                    if let Some(index) = self.active {
                        self.tabs[index].state.apply_text_edits(
                            vec![(
                                *range,
                                format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]),
                            )],
                            "Change color",
                        );
                    }
                } else if !cancel {
                    finished = false;
                }
            }
            Request::Editor(EditorRequest::Regex(pattern)) => {
                let contextual = self.active.is_some_and(|index| {
                    let tab = &mut self.tabs[index];
                    let source = tab.state.doc.text();
                    let parse = tab.state.syntax().clone();
                    rustpy_regex::injections(&source, &parse)
                        .iter()
                        .any(|injection| {
                            injection.string_range.contains_inclusive(state.owner.caret)
                        })
                });
                if contextual {
                    self.handle_action(IdeAction::CheckRegExp, ctx)
                } else {
                    self.dialog = Dialog::CheckRegex {
                        pattern: pattern.clone(),
                        flags: String::new(),
                        sample: String::new(),
                        result: None,
                        error: None,
                        rx: None,
                    };
                }
            }
            Request::Editor(EditorRequest::Copy(value)) => ctx.copy_text(value.clone()),
            Request::Editor(EditorRequest::OpenUrl(url)) => {
                if url.starts_with("https://") || url.starts_with("http://") {
                    self.open_url(url)
                }
            }
            Request::Editor(EditorRequest::Refactor(kind)) => self.handle_action(
                match kind {
                    Refactoring::Variable => IdeAction::ExtractVariable,
                    Refactoring::Constant => IdeAction::ExtractConstant,
                    Refactoring::Field => IdeAction::ExtractField,
                    Refactoring::Parameter => IdeAction::ExtractParameter,
                    Refactoring::Method => IdeAction::ExtractMethod,
                    Refactoring::Inline => IdeAction::Inline,
                },
                ctx,
            ),
            Request::Editor(EditorRequest::ConvertPackage) => {
                self.handle_action(IdeAction::ConvertToPackage, ctx)
            }
            Request::Editor(EditorRequest::ConvertModule) => {
                self.handle_action(IdeAction::ConvertToModule, ctx)
            }
            Request::Editor(EditorRequest::CreateTest) => {
                if let Some(path) = state.owner.path.clone() {
                    self.open_generate_test_picker(path)
                }
            }
            Request::Editor(EditorRequest::NavigateTest) => self.go_to_test(),
            Request::Install(spec) => self.install_package(spec),
            Request::AddRequirement(spec) => {
                if let Err(error) = self.add_dependency_to_requirements(spec) {
                    self.set_status(error)
                }
            }
            Request::Replace { range, text, title } => {
                if let Some(index) = self.active {
                    self.tabs[index]
                        .state
                        .apply_text_edits(vec![(*range, text.clone())], title);
                }
            }
        }
        if !finished {
            self.intention_menu.editor = Some(state)
        }
    }
    fn add_dependency_to_requirements(&mut self, spec: &str) -> Result<(), String> {
        if self.safe_mode {
            return Err("Trust the project before adding a dependency file".into());
        }
        let parsed = rustpy_packages::requirements::parse_requirement(spec, 1)
            .ok_or("Not a named PEP 508 dependency")?;
        let root = self
            .project
            .as_ref()
            .ok_or("Open a project first")?
            .root
            .clone();
        let root_real = dunce::canonicalize(&root).map_err(|error| error.to_string())?;
        let target = root.join("requirements.txt");
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || !dunce::canonicalize(&target)
                        .map_err(|error| error.to_string())?
                        .starts_with(&root_real)
                {
                    return Err("Requirements destination is not an owned project file".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
        self.open_file(&target);
        let index = self
            .active
            .filter(|index| self.tabs[*index].state.path.as_ref() == Some(&target))
            .ok_or("Cannot open requirements.txt")?;
        if self.tabs[index].state.doc.is_read_only() {
            return Err("Requirements file is read-only".into());
        }
        let text = self.tabs[index].state.doc.text();
        if rustpy_packages::parse_requirements(&text)
            .iter()
            .any(|requirement| {
                rustpy_packages::normalize_name(&requirement.name)
                    == rustpy_packages::normalize_name(&parsed.name)
            })
        {
            self.set_status("Package is already declared in requirements.txt");
            return Ok(());
        }
        let prefix = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        self.tabs[index].state.apply_text_edits(
            vec![(
                TextRange::empty(TextSize::new(text.len() as u32)),
                format!("{prefix}{spec}\n"),
            )],
            "Add package to requirements",
        );
        self.focus_editor = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(source: &str) -> (IdeApp, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        (app, root)
    }
    #[test]
    fn requirements_conversion_and_install_use_authoritative_complete_spec() {
        let path = Path::new("/project/requirements.txt");
        let source = "Requests (>=2.0) ; python_version >= '3.9'\n";
        let actions = requirement_actions(path, source, TextSize::new(3));
        assert!(actions.iter().any(|(_,action)|matches!(action,Request::Install(spec)if spec.contains("python_version")&&spec.contains(">=2.0"))));
        let (_, request) = actions
            .into_iter()
            .find(|(title, _)| title.starts_with("Convert requirements"))
            .unwrap();
        let Request::Replace { range, text, .. } = request else {
            panic!("replacement required")
        };
        let converted = rustpy_analysis::intentions::apply_edits(source, &[(range, text)]);
        let before = rustpy_packages::requirements_file::parse_named(source.trim()).unwrap();
        let after = rustpy_packages::requirements_file::parse_named(converted.trim()).unwrap();
        assert_eq!(before, after);
        let actions = requirement_actions(path, "requests=2.0\n", TextSize::new(2));
        assert!(actions.iter().any(
            |(_, action)| matches!(action,Request::Replace{text,..}if text=="requests==2.0\n")
        ));
        let actions =
            requirement_actions(path, "requests (>=2) # keep comment\n", TextSize::new(2));
        assert!(!actions
            .iter()
            .any(|(_, action)| matches!(action, Request::Replace { .. })));
        assert!(
            requirement_actions(Path::new("main.py"), "requests=2.0", TextSize::new(2)).is_empty()
        );
    }
    #[test]
    fn dependency_insertion_preserves_unsaved_requirements_and_avoids_duplicate_distribution() {
        let (mut app, root) = app("import yaml\n");
        app.safe_mode = false;
        app.add_dependency_to_requirements("PyYAML>=6").unwrap();
        let index = app.active.unwrap();
        assert_eq!(
            app.tabs[index].state.path.as_deref(),
            Some(root.path().join("requirements.txt").as_path())
        );
        assert_eq!(app.tabs[index].state.doc.text(), "PyYAML>=6\n");
        app.tabs[index].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "# unsaved\n".into())],
            "edit requirements",
        );
        app.add_dependency_to_requirements("pyyaml").unwrap();
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            "# unsaved\nPyYAML>=6\n"
        );
        app.add_dependency_to_requirements("requests").unwrap();
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            "# unsaved\nPyYAML>=6\nrequests\n"
        );
    }
    #[test]
    fn delayed_editor_action_refuses_changed_document_before_clipboard_or_edit() {
        let (mut app, _root) = app("value = '#123456'\n");
        let index = app.active.unwrap();
        app.begin_editor_request(Request::Editor(EditorRequest::Color {
            range: TextRange::new(TextSize::new(9), TextSize::new(16)),
            rgb: [255, 0, 0],
        }));
        app.tabs[index].state.apply_text_edits(
            vec![(TextRange::empty(TextSize::new(0)), "# changed\n".into())],
            "change",
        );
        let context = egui::Context::default();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            app.editor_request_dialog(ui.ctx())
        });
        // No renderer consumes textures in this model-level dialog fixture.
        output.textures_delta.clear();
        drop(output);
        assert!(app.intention_menu.editor.is_none());
        assert!(app.tabs[index].state.doc.text().contains("'#123456'"));
    }
    #[test]
    fn regex_and_refactoring_requests_use_existing_dialogs_without_premature_edits() {
        let (mut app, _root) = app("value = r'a+'\n");
        app.begin_editor_request(Request::Editor(EditorRequest::Regex("a+".into())));
        let context = egui::Context::default();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            app.editor_request_dialog(ui.ctx())
        });
        // No renderer consumes textures in this model-level dialog fixture.
        output.textures_delta.clear();
        drop(output);
        assert!(matches!(&app.dialog,Dialog::CheckRegex{pattern,..}if pattern=="a+"));
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            "value = r'a+'\n"
        );
    }
    #[cfg(unix)]
    #[test]
    fn dependency_creation_refuses_existing_symlink_before_writing() {
        let (mut app, root) = app("import yaml\n");
        app.safe_mode = false;
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("external.txt");
        std::fs::write(&path, "original\n").unwrap();
        std::os::unix::fs::symlink(&path, root.path().join("requirements.txt")).unwrap();
        assert!(app.add_dependency_to_requirements("PyYAML").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original\n");
    }
}
