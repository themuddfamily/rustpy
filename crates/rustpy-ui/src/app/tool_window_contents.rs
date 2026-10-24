//! Existing left-tool content authority, shared by docked and native hosts.
use super::*;
impl IdeApp {
    pub(super) fn left_tool_contents(&mut self, ui: &mut egui::Ui) {
        self.tool_window_gear(ui, layout::ToolRef::Left(self.left_tool));
        let theme = self.theme.clone();
        if !self.tool_window_has_frame_host() {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(
                        self.left_tool == LeftTool::Project,
                        egui::RichText::new(crate::i18n::tr_static("Project")).strong(),
                    )
                    .clicked()
                {
                    self.left_tool = LeftTool::Project;
                }
                if ui
                    .selectable_label(
                        self.left_tool == LeftTool::Structure,
                        egui::RichText::new(crate::i18n::tr_static("Structure")).strong(),
                    )
                    .clicked()
                {
                    self.left_tool = LeftTool::Structure;
                }
                if ui
                    .selectable_label(
                        self.left_tool == LeftTool::Bookmarks,
                        egui::RichText::new(crate::i18n::tr_static("Bookmarks")).strong(),
                    )
                    .clicked()
                {
                    self.left_tool = LeftTool::Bookmarks;
                }
            });
        }
        if self.left_tool == LeftTool::Project {
            ui.horizontal(|ui| {
                self.project_navigation_options(ui);
                ui.add_space(4.0);
                if ui.small_button("⟳").icon_hint("Refresh").clicked() {
                    if let Some(p) = &mut self.project {
                        p.tree.invalidate_all();
                    }
                    self.refresh_project_files();
                }
                if ui
                    .small_button("⊙")
                    .icon_hint("Select Opened File (Alt+F1)")
                    .clicked()
                {
                    let ctx2 = ui.ctx().clone();
                    self.handle_action(Action::SelectInProjectView, &ctx2);
                }
                if ui.small_button("⊟").icon_hint("Collapse All").clicked() {
                    if let Some(p) = &mut self.project {
                        let root = p.root.clone();
                        p.tree.expanded.clear();
                        p.tree.expanded.insert(root);
                    }
                }
            });
        }
        ui.separator();
        if self.left_tool == LeftTool::Structure {
            if !self.notebook_structure_view(ui) {
                self.structure_view(ui);
            }
            return;
        }
        if self.left_tool == LeftTool::Bookmarks {
            self.bookmarks_view(ui);
            return;
        }
        ui.checkbox(
            &mut self.settings.editor.structure.show_members_in_project,
            "Show Members in Project View",
        );
        self.project_members_view(ui);
        self.project_view_toolbar(ui);
        let mut action = None;
        let keyboard = self.editor_controls_available();
        if let Some(p) = &mut self.project {
            p.tree.scratch_languages = self.settings_ui.textmate.scratch_languages();
            let owner = crate::run_context::ProjectOwner {
                root: p.root.clone(),
                session: self.file_session,
            };
            ui.data_mut(|data| data.insert_temp(crate::run_context::tree_owner_key(), owner));
            action = p.tree.ui(ui, &theme, &self.keymap, keyboard);
        }
        self.autoscroll_to_source(ui.ctx());
        match action {
            Some(TreeAction::Invoke(path, action)) => {
                self.project_path_action(path, action, ui.ctx())
            }
            Some(TreeAction::InvokeMany(paths, action)) => self.project_many_actions(paths, action),
            Some(TreeAction::Transfer(paths, target, copy)) => {
                self.project_transfer_dialog(paths, target, copy)
            }
            Some(TreeAction::Clipboard(paths, cut)) => self.project_clipboard(paths, cut, ui.ctx()),
            Some(TreeAction::Paste(target)) => self.project_paste(target, ui.ctx()),
            Some(TreeAction::DeleteMany(paths)) => {
                if self.dumb_mode_admits_with(Action::SafeDelete, |_| {
                    crate::dumb_mode::Target::Paths(paths.clone())
                }) {
                    self.project_delete_many(paths)
                }
            }
            Some(TreeAction::Analyze(path, kind)) => self.project_analyze(path, kind),
            Some(TreeAction::OpenAssociated(path)) => self.project_open_associated(path),
            Some(TreeAction::Encoding(path, encoding)) => self.project_encoding(path, encoding),
            Some(TreeAction::LineEndings(path, ending)) => self.project_line_endings(path, ending),
            Some(TreeAction::CreateGist(path)) => self.project_create_gist(path, ui.ctx()),
            Some(TreeAction::OpenFile(path)) => {
                if self.settings.editor.open_in_preview_tab {
                    self.open_preview(&path);
                } else {
                    self.open_file(&path);
                }
                // A single click keeps the keyboard in the Project view.
                self.focus_editor = false;
            }
            Some(TreeAction::GenerateNativeStub(path)) => {
                if let Some(project) = &mut self.project {
                    project.tree.selected = Some(path);
                }
                self.generate_native_stub();
            }
            Some(TreeAction::OpenInRightSplit(path)) => self.open_in_right_split(&path),
            Some(TreeAction::FocusEditor) => self.focus_editor = true,
            Some(TreeAction::OpenFileDoubleClick(path)) => {
                self.open_file(&path);
                if let Some(t) = self.active.and_then(|i| self.tabs.get_mut(i)) {
                    t.preview = false;
                }
            }
            Some(TreeAction::NewFromTemplate(dir, name)) => self.project_new_template(dir, name),
            Some(TreeAction::EditFileTemplates) => self.project_edit_templates(),
            Some(TreeAction::NewDataSource(dir)) => self.project_new_data_source(&dir),
            Some(TreeAction::NewGitignore(dir)) => self.open_gitignore_templates(dir),
            Some(TreeAction::NewFile(dir, kind)) => {
                self.dialog = Dialog::NewFileKind {
                    dir,
                    kind,
                    name: crate::plugin_host::default_file_name(kind)
                        .unwrap_or_else(|| default_name_for(kind).into()),
                }
            }
            Some(TreeAction::NewDirectory(dir)) => {
                self.dialog = Dialog::NewFileKind {
                    dir,
                    kind: "directory",
                    name: "new_dir".into(),
                }
            }
            Some(TreeAction::NewPackage(dir)) => {
                self.dialog = Dialog::NewFileKind {
                    dir,
                    kind: "package",
                    name: "new_package".into(),
                }
            }
            Some(TreeAction::Rename(path)) => {
                if self.dumb_mode_admits_with(Action::Rename, |_| {
                    crate::dumb_mode::Target::Path(path.clone())
                }) {
                    self.open_tree_rename(path)
                }
            }
            Some(TreeAction::Delete(path)) => {
                if self.dumb_mode_admits_with(Action::SafeDelete, |_| {
                    crate::dumb_mode::Target::Path(path.clone())
                }) {
                    self.open_tree_delete(path)
                }
            }
            Some(TreeAction::CompareFile(path)) => self.open_comparison_picker(Some(&path)),
            Some(TreeAction::LocalHistory(path)) => self.open_path_history(&path),
            Some(TreeAction::LocalHistoryLabel(path)) => self.begin_history_label(path),
            Some(TreeAction::CopyPath(path)) => {
                self.open_copy_path_target(copy_reference_paths::CopyTarget::path(path));
            }
            Some(TreeAction::CopyReference(path)) => {
                self.copy_reference_target(copy_reference_paths::CopyTarget::path(path));
            }
            Some(TreeAction::ContextRun(request)) => self.project_context_request(*request),
            Some(TreeAction::Run(path)) => {
                self.launch_path(&path, crate::run_context::RunMode::Run)
            }
            Some(TreeAction::Debug(path)) => {
                self.launch_path(&path, crate::run_context::RunMode::Debug)
            }
            Some(TreeAction::NewScratch) => self.dialog = Dialog::NewScratch(String::new(), 0),
            Some(TreeAction::ChangeScratchLanguage(path, ext)) => {
                self.change_scratch_language(&path, &ext)
            }
            Some(TreeAction::OpenInTerminal(dir)) => {
                self.bottom = Some(BottomTool::Terminal);
                let env = crate::terminal_panel::terminal_env(
                    self.settings.terminal.activate_virtualenv,
                    self.project.as_ref().and_then(|p| p.interpreter.as_ref()),
                );
                if let Err(e) = self.terminal_panel.open(&dir, &env) {
                    self.set_status(format!("Cannot open terminal: {e}"));
                }
            }
            Some(TreeAction::AddToGitignore(path)) => {
                if let Some(root) = self
                    .vcs_workspace
                    .mappings
                    .root_for(&path)
                    .cloned()
                    .or_else(|| {
                        self.project
                            .as_ref()
                            .and_then(|p| p.git.as_ref())
                            .map(|g| g.root().to_path_buf())
                    })
                {
                    let rel = path
                        .strip_prefix(&root)
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    match rustpy_vcs::add_to_gitignore(&root, &rel) {
                        Ok(()) => self.set_status(format!("Added '{rel}' to .gitignore")),
                        Err(e) => self.set_status(e.to_string()),
                    }
                    if let Some(p) = &mut self.project {
                        p.git_refreshed = -100.0;
                    }
                }
            }
            Some(TreeAction::MarkAs(path, kind)) => self.mark_directory(&path, kind),
            Some(TreeAction::RevealInFiles(path)) => {
                let dir = if path.is_dir() {
                    path.clone()
                } else {
                    path.parent().map(Path::to_path_buf).unwrap_or(path.clone())
                };
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else if cfg!(windows) {
                    "explorer"
                } else {
                    "xdg-open"
                };
                match std::process::Command::new(opener).arg(&dir).spawn() {
                    Ok(_) => self.set_status(format!("Opened {}", dir.display())),
                    Err(e) => self.set_status(format!("Cannot open file manager: {e}")),
                }
            }
            Some(TreeAction::FindInDirectory(dir, replace)) => {
                let seed = self.selected_text_seed();
                self.find_in_files.open_in_directory(dir, replace, seed);
            }
            Some(TreeAction::SelectDir(_)) | None => {}
        }
    }
}
