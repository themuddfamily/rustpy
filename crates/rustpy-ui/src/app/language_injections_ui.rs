//! Actual intentions, picker, reference navigation and isolated fragment editor.
use super::*;
use crate::a11y::DialogWindow;
use crate::editor::language_injections::{Fragment, Language as InjectedLanguage};
use rustpy_text::DocumentId;

#[derive(Clone)]
pub(super) struct Owner {
    document: DocumentId,
    version: u64,
    path: Option<PathBuf>,
    view: egui::Id,
    caret: TextSize,
}
#[derive(Default)]
pub(super) enum InjectionUi {
    #[default]
    Closed,
    Picker {
        owner: Owner,
    },
    Fragment {
        owner: Owner,
        fragment: Box<Fragment>,
        editor: Box<EditorState>,
        view: Box<EditorView>,
    },
}
impl InjectionUi {
    pub fn active(&self) -> bool {
        !matches!(self, Self::Closed)
    }
}
impl IdeApp {
    fn injection_owner(&self) -> Option<Owner> {
        let tab = self.tabs.get(self.active?)?;
        Some(Owner {
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            path: tab.state.path.clone(),
            view: tab.view.id,
            caret: tab.state.primary().head(),
        })
    }
    fn injection_owner_index(&self, owner: &Owner) -> Option<usize> {
        self.active.filter(|i| {
            self.tabs.get(*i).is_some_and(|t| {
                owner.document == t.state.doc.identity()
                    && owner.version == t.state.doc.version()
                    && owner.path == t.state.path
                    && owner.view == t.view.id
                    && owner.caret == t.state.primary().head()
            })
        })
    }
    pub(super) fn inject_language_action(&mut self) {
        let Some(i) = self.active else { return };
        let state = &mut self.tabs[i].state;
        let source = state.doc.text();
        let parse = state.syntax().clone();
        if state.language != Language::Python
            || crate::editor::language_injections::target(
                &source,
                &parse,
                state.primary().head(),
                InjectedLanguage::Json,
            )
            .is_none()
        {
            self.set_status("Caret must be inside a complete plain Python string");
            return;
        }
        let Some(owner) = self.injection_owner() else {
            return;
        };
        self.injection_ui = InjectionUi::Picker { owner };
    }
    pub(super) fn edit_injected_fragment_action(&mut self) {
        let Some(i) = self.active else { return };
        if !self.refresh_file_permissions(i) {
            return;
        }
        let Some(injection) = self.tabs[i].state.current_injection() else {
            self.set_status("No injected fragment at caret");
            return;
        };
        let fragment = match Fragment::open(&self.tabs[i].state, injection) {
            Ok(f) => f,
            Err(e) => {
                self.set_status(e);
                return;
            }
        };
        let language = match fragment.injection().language {
            InjectedLanguage::Python => Language::Python,
            InjectedLanguage::Sql => Language::Sql,
            InjectedLanguage::Html => Language::Html,
            InjectedLanguage::Json => Language::Json,
            InjectedLanguage::Yaml => Language::Yaml,
            InjectedLanguage::Markdown => Language::Markdown,
            _ => Language::PlainText,
        };
        let mut editor = EditorState::from_text(&fragment.text, language);
        editor
            .doc
            .set_read_only(self.tabs[i].state.doc.is_read_only());
        let view = EditorView::new(egui::Id::new(("injected-fragment", editor.doc.identity())));
        let owner = self.injection_owner().unwrap();
        self.injection_ui = InjectionUi::Fragment {
            owner,
            fragment: Box::new(fragment),
            editor: Box::new(editor),
            view: Box::new(view),
        };
    }
    pub(super) fn navigate_injected_reference(&mut self) {
        let Some(owner) = self.injection_owner() else {
            return;
        };
        let Some(i) = self.injection_owner_index(&owner) else {
            return;
        };
        let Some(injection) = self.tabs[i].state.current_injection() else {
            self.set_status("No file reference at caret");
            return;
        };
        let result = Fragment::open(&self.tabs[i].state, injection)
            .and_then(|fragment| fragment.reference());
        let path = match result {
            Ok(p) => p,
            Err(e) => {
                self.set_status(e);
                return;
            }
        };
        let path = if path.is_absolute() {
            path
        } else {
            let Some(parent) = owner.path.as_deref().and_then(Path::parent) else {
                self.set_status("Relative reference requires a saved host file");
                return;
            };
            parent.join(path)
        };
        if self.injection_owner_index(&owner).is_none() {
            return;
        }
        if !path.is_file() {
            self.set_status("Referenced file does not exist or is not a regular file");
            return;
        }
        self.mark_nav_jump();
        self.open_file(&path);
        self.focus_editor = true;
    }
    pub(super) fn injected_intentions(&mut self) -> Vec<QuickFix> {
        let Some(i) = self.active else { return vec![] };
        let state = &mut self.tabs[i].state;
        if state.language != Language::Python {
            return vec![];
        };
        let source = state.doc.text();
        let parse = state.syntax().clone();
        if crate::editor::language_injections::target(
            &source,
            &parse,
            state.primary().head(),
            InjectedLanguage::Json,
        )
        .is_none()
        {
            return vec![];
        }
        let mut titles = vec![
            "Inject language or reference",
            "Remove temporary language injection",
        ];
        if let Some(injection) = state.current_injection() {
            titles.push("Edit injected fragment");
            if injection.language == InjectedLanguage::FileReference {
                titles.push("Go to injected file reference");
            }
        }
        titles
            .into_iter()
            .map(|title| QuickFix {
                title: title.into(),
                edits: vec![],
            })
            .collect()
    }
    pub(super) fn injected_intention_action(&mut self, title: &str) -> bool {
        match title {
            "Inject language or reference" => self.inject_language_action(),
            "Edit injected fragment" => self.edit_injected_fragment_action(),
            "Remove temporary language injection" => {
                if let Some(i) = self.active {
                    self.tabs[i].state.remove_temporary_injection();
                }
            }
            "Go to injected file reference" => self.navigate_injected_reference(),
            _ => return false,
        }
        true
    }
    pub(super) fn open_temporary_regex_checker(&mut self) {
        let Some(i) = self.active else { return };
        let Some(injection) = self.tabs[i]
            .state
            .current_injection()
            .filter(|i| i.language == InjectedLanguage::Regex)
        else {
            self.set_status("Caret is not inside an injected regex string");
            return;
        };
        match Fragment::open(&self.tabs[i].state, injection) {
            Ok(f) => {
                let flags = if f.injection().verbose {
                    "VERBOSE".into()
                } else {
                    String::new()
                };
                self.dialog = Dialog::CheckRegex {
                    pattern: f.text,
                    flags,
                    sample: String::new(),
                    result: None,
                    error: None,
                    rx: None,
                }
            }
            Err(e) => self.set_status(e),
        }
    }
    fn injected_fragment_events(
        &mut self,
        ctx: &egui::Context,
        editor: &mut EditorState,
        events: &[ViewEvent],
    ) {
        self.record_clipboard_events(events);
        for event in events {
            match event {
                ViewEvent::Action(Action::ReformatCode | Action::ReformatFile) => {
                    match super::reformat_workflows::format_fragment_editor(editor) {
                        Ok(lines) => self
                            .set_status(format!("{lines} lines reformatted in injected fragment")),
                        Err(error) => self.set_status(error),
                    }
                }
                ViewEvent::Action(Action::Paste) => {
                    if let Some(text) = self.clipboard_text() {
                        if text.len() <= 128 * 1024 {
                            editor.paste_payload(
                                &text,
                                self.clipboard_payload.as_ref(),
                                &crate::editor::EditOptions::default(),
                            );
                        } else {
                            self.set_status("Injected fragment clipboard exceeds 128 KiB");
                        }
                    } else {
                        self.set_status("The clipboard is empty");
                    }
                }
                ViewEvent::Action(action @ (Action::Copy | Action::Cut)) => {
                    let payload = editor.copy_payload();
                    let text = if *action == Action::Cut {
                        editor.cut()
                    } else {
                        editor.copy_text()
                    };
                    if !text.is_empty() {
                        ctx.copy_text(text.clone());
                        self.remember_copied(text);
                        self.clipboard_payload = Some(payload);
                    }
                }
                ViewEvent::Action(_) | ViewEvent::LargePaste(_) | ViewEvent::StringPaste(_) => {
                    self.set_status("This action is unavailable in the injected fragment editor")
                }
                _ => {}
            }
        }
    }
    pub(super) fn injection_dialog(&mut self, ctx: &egui::Context) {
        let current = std::mem::take(&mut self.injection_ui);
        let mut keep = None;
        let mut chosen = None;
        let mut apply = None;
        let mut preview_fragment = false;
        let mut debug_fragment = false;
        let mut cancelled = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        match current {
            InjectionUi::Closed => return,
            InjectionUi::Picker { owner } => {
                if self.injection_owner_index(&owner).is_none() {
                    self.set_status("Injection target changed; reopen chooser");
                    self.focus_editor = true;
                    return;
                }
                egui::Window::new("Inject language or reference")
                    .collapsible(false)
                    .resizable(false)
                    .show_modal(ctx, |ui| {
                        for language in [
                            InjectedLanguage::Python,
                            InjectedLanguage::Regex,
                            InjectedLanguage::Sql,
                            InjectedLanguage::Html,
                            InjectedLanguage::Json,
                            InjectedLanguage::Yaml,
                            InjectedLanguage::Markdown,
                            InjectedLanguage::FileReference,
                        ] {
                            if ui.button(language.name()).clicked() {
                                chosen = Some(language);
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                    });
                if let Some(language) = chosen {
                    if let Some(i) = self.injection_owner_index(&owner) {
                        match self.tabs[i].state.inject_language(language) {
                            Ok(()) => self.set_status(format!(
                                "Temporary {} injection; source edits clear it",
                                language.name()
                            )),
                            Err(e) => self.set_status(e),
                        }
                    }
                    self.focus_editor = true;
                } else if !cancelled {
                    keep = Some(InjectionUi::Picker { owner });
                }
            }
            InjectionUi::Fragment {
                owner,
                mut fragment,
                mut editor,
                mut view,
            } => {
                let Some(i) = self.injection_owner_index(&owner) else {
                    self.set_status("Injected fragment host changed; reopen it");
                    self.focus_editor = true;
                    return;
                };
                if !self.refresh_file_permissions(i) {
                    self.focus_editor = true;
                    return;
                }
                editor
                    .doc
                    .set_read_only(self.tabs[i].state.doc.is_read_only());
                egui::Window::new(format!("Edit injected fragment — {}",fragment.injection().language.name())).default_size(Vec2::new(720.0,480.0)).show_modal(ctx,|ui|{
                    ui.label("Apply changes the original string as one undo step; Cancel leaves it unchanged.");
                    let fragment_breakpoints: Vec<_> = (0..fragment.text.lines().count()).filter_map(|line| {
                        let host = fragment.host_line(&self.tabs[i].state, line)?;
                        self.breakpoints.get(owner.path.as_deref()?, host + 1).map(|_| (line, crate::editor::view::BpGlyph::Enabled))
                    }).collect();
                    let opts=crate::editor::EditOptions::default();let cfg=crate::editor::view::ViewConfig {scheme:&self.scheme,theme:&self.theme,settings:&self.settings.editor,opts:&opts,keymap:&self.keymap,font_size:self.settings.editor.font_size,search_highlights:&[],request_focus:!view.focused,run_lines:&[],line_changes:&[],breakpoints:&fragment_breakpoints,exec_line:None,inline_values:&[],bookmarks:&[],gutter_markers:&[],gutter_quick_fixes:&[],code_vision:None,annotations:None,coverage:&[],keyboard:true};
                    let events = ui.allocate_ui(Vec2::new(ui.available_width(),320.0),|ui|crate::editor::view::show(ui,&mut editor,&mut view,&cfg)).inner;
                    if fragment.injection().language == InjectedLanguage::Python && editor.doc.text() == fragment.text {
                        for event in &events {
                            let line = match event {
                                ViewEvent::GutterClick(line) => Some(*line),
                                ViewEvent::Action(Action::ToggleLineBreakpoint) => Some(editor.doc.text()[..usize::from(editor.primary().head())].bytes().filter(|byte| *byte == b'\n').count()),
                                _ => None,
                            };
                            if let Some(host_line) = line.and_then(|line| fragment.host_line(&self.tabs[i].state, line)) {
                                if let Some(path) = owner.path.as_deref() { self.toggle_breakpoint(path, host_line); }
                            }
                        }
                    }
                    self.injected_fragment_events(ctx, &mut editor, &events);
                    if fragment.injection().language==InjectedLanguage::Sql {
                        let text=editor.doc.text();let end=usize::from(editor.primary().head());let before=text.get(..end).unwrap_or("");let prefix=before.rsplit(|c:char|!c.is_alphanumeric()&&c!='_').next().unwrap_or("");
                        let (words,issues)=self.db_panel.sql_schema_hints(owner.path.as_deref(),before);
                        egui::CollapsingHeader::new("Database schema completion / inspections").show(ui,|ui|{ui.horizontal_wrapped(|ui|{for word in words.into_iter().take(30){if ui.small_button(&word).clicked(){editor.insert_text(word.strip_prefix(prefix).unwrap_or(&word),&crate::editor::EditOptions::default());}}});for issue in issues{ui.label(issue);}if ui.button("Run SQL in scoped database console").clicked(){if self.safe_mode{self.notify_safe_mode("Database execution");}else{match self.db_panel.open_scoped_sql(owner.path.as_deref(),text.clone()){Ok(())=>self.bottom=Some(BottomTool::Database),Err(e)=>self.set_status(e)}}}});
                    }

                    ui.horizontal(|ui| {
                        if ui.add_enabled(!editor.doc.is_read_only(), egui::Button::new("Reformat Fragment")).clicked() {
                            match super::reformat_workflows::format_fragment_editor(&mut editor) {
                                Ok(lines) => self.set_status(format!("{lines} lines reformatted; Apply to update the host")),
                                Err(error) => self.set_status(error),
                            }
                        }
                        if ui.add_enabled(!editor.doc.is_read_only(), egui::Button::new("Apply")).clicked() {
                            apply = Some(editor.doc.text());
                        }
                        if fragment.injection().language == InjectedLanguage::Python && ui.button("Debug Python Fragment").clicked() { debug_fragment=true; }
                        if ui.button("Preview refactoring").clicked() {preview_fragment=true;}
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                    });
                });
                if debug_fragment {
                    if editor.doc.text() != fragment.text {
                        self.set_status("Apply fragment edits before debugging its source map");
                    } else {
                        self.debug_python_fragment(&owner, &fragment);
                        cancelled = true;
                    }
                }
                if preview_fragment {
                    fragment.text = editor.doc.text();
                    match fragment.refactoring_edit(&self.tabs[i].state) {
                        Ok(Some(edit)) => self.preview_single_refactoring(Refactoring {
                            title: "Refactor injected fragment".into(),
                            edits: vec![edit],
                            rename_ranges: Vec::new(),
                            name: String::new(),
                        }),
                        Ok(None) => self.set_status("Injected fragment unchanged"),
                        Err(error) => self.set_status(error),
                    }
                    self.injection_ui = InjectionUi::Closed;
                    return;
                }
                if let Some(text) = apply {
                    if !self.refresh_file_permissions(i) {
                        self.injection_ui = InjectionUi::Fragment {
                            owner,
                            fragment,
                            editor,
                            view,
                        };
                        return;
                    }
                    fragment.text = text;
                    match fragment.apply(&mut self.tabs[i].state) {
                        Ok(changed) => {
                            if changed {
                                self.tabs[i].state.semantic_version = None;
                            }
                            self.set_status(if changed {
                                "Injected fragment applied"
                            } else {
                                "Injected fragment unchanged"
                            });
                            self.focus_editor = true
                        }
                        Err(e) => {
                            self.set_status(e);
                            keep = Some(InjectionUi::Fragment {
                                owner,
                                fragment,
                                editor,
                                view,
                            });
                        }
                    }
                } else if !cancelled {
                    keep = Some(InjectionUi::Fragment {
                        owner,
                        fragment,
                        editor,
                        view,
                    });
                }
            }
        }
        if let Some(state) = keep {
            self.injection_ui = state;
        } else {
            self.focus_editor = true;
        }
    }
}

/// Immutable debugger source and its original document/range ownership.
pub(super) struct DebugSource {
    root: PathBuf,
    path: PathBuf,
    owner: Owner,
    fragment: Fragment,
}
impl IdeApp {
    fn debug_python_fragment(&mut self, owner: &Owner, fragment: &Fragment) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode || self.injection_owner_index(owner).is_none() {
            self.set_status("Trust the project and reopen the current fragment before debugging");
            return;
        }
        let Some(root) = self.project.as_ref().map(|project| project.root.clone()) else {
            return;
        };
        let Some(host) = owner.path.as_ref() else {
            self.set_status("Save the host file before debugging an injected fragment");
            return;
        };
        if self.injected_debug_sources.len() >= 128 {
            self.set_status(
                "Injected debugger snapshot limit reached; reopen the project to clear snapshots",
            );
            return;
        }
        let result = (|| -> std::io::Result<PathBuf> {
            use std::io::Write;
            let metadata = root.join(".rustpy");
            match std::fs::create_dir(&metadata) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error),
            }
            if !dunce::canonicalize(&metadata)?.starts_with(dunce::canonicalize(&root)?) {
                return Err(std::io::Error::other(
                    "Debugger metadata directory escapes the project",
                ));
            }
            let directory = metadata.join("debug-fragments");
            match std::fs::create_dir(&directory) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error),
            }
            if !dunce::canonicalize(&directory)?.starts_with(dunce::canonicalize(&root)?) {
                return Err(std::io::Error::other(
                    "Debugger snapshot directory escapes the project",
                ));
            }
            for suffix in 0..1024 {
                let path = directory.join(format!("fragment-{}-{suffix}.py", self.next_session_id));
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(mut file) => {
                        file.write_all(fragment.text.as_bytes())?;
                        return Ok(path);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(std::io::Error::other(
                "No unused injected debugger snapshot name",
            ))
        })();
        let path = match result {
            Ok(path) => path,
            Err(error) => {
                self.set_status(format!("Cannot prepare injected debugger source: {error}"));
                return;
            }
        };
        let Some(index) = self.injection_owner_index(owner) else {
            return;
        };
        for line0 in 0..fragment.text.lines().count() {
            if let Some(host_line) = fragment.host_line(&self.tabs[index].state, line0) {
                if let Some(bp) = self.breakpoints.get(host, host_line + 1).cloned() {
                    self.breakpoints.set(&path, line0 + 1, bp);
                }
            }
        }
        self.injected_debug_sources.push(DebugSource {
            root: root.clone(),
            path: path.clone(),
            owner: owner.clone(),
            fragment: fragment.clone(),
        });
        let cfg = RunConfig {
            name: format!(
                "Injected Python in {}",
                host.file_name().unwrap_or_default().to_string_lossy()
            ),
            kind: RunConfigKind::Python { script: path },
            working_dir: host.parent().map(Path::to_path_buf),
            temporary: true,
            ..Default::default()
        };
        self.launch_config(cfg, crate::run_context::RunMode::Debug);
        self.injection_ui = InjectionUi::Closed;
    }
    pub(super) fn injected_debug_location(
        &self,
        path: &Path,
        line: usize,
    ) -> Option<(PathBuf, usize)> {
        let root = &self.project.as_ref()?.root;
        let source = self
            .injected_debug_sources
            .iter()
            .find(|source| source.root == *root && source.path == path)?;
        let tab = self.tabs.iter().find(|tab| {
            tab.state.doc.identity() == source.owner.document
                && tab.state.path == source.owner.path
                && tab.state.doc.version() == source.owner.version
        })?;
        Some((
            source.owner.path.clone()?,
            source
                .fragment
                .host_line(&tab.state, line.checked_sub(1)?)?
                + 1,
        ))
    }
}

impl IdeApp {
    pub(super) fn sync_injected_debug_breakpoints(&mut self, host: &Path) {
        let Some(root) = self.project.as_ref().map(|project| &project.root) else {
            return;
        };
        let updates: Vec<_> = self
            .injected_debug_sources
            .iter()
            .filter(|source| source.root == *root && source.owner.path.as_deref() == Some(host))
            .filter_map(|source| {
                let tab = self.tabs.iter().find(|tab| {
                    tab.state.doc.identity() == source.owner.document
                        && tab.state.path == source.owner.path
                        && tab.state.doc.version() == source.owner.version
                })?;
                let points: Vec<_> = (0..source.fragment.text.lines().count())
                    .filter_map(|line| {
                        let host_line = source.fragment.host_line(&tab.state, line)?;
                        Some((line + 1, self.breakpoints.get(host, host_line + 1)?.clone()))
                    })
                    .collect();
                Some((source.path.clone(), points))
            })
            .collect();
        for (path, points) in updates {
            for line in self.breakpoints.lines(&path) {
                self.breakpoints.remove(&path, line);
            }
            for (line, bp) in points {
                self.breakpoints.set(&path, line, bp);
            }
            if let Some(session) = &mut self.debug {
                self.breakpoints.sync(&path, session);
            }
            for (session, _) in &mut self.debug_sessions {
                self.breakpoints.sync(&path, session);
            }
        }
    }
}
