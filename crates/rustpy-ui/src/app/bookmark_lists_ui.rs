//! Named bookmark lists and file/folder targets share the existing bookmark ledger.
use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_settings::BookmarkKind;

const DEFAULT_LIST: &str = "Default";
#[derive(Default)]
pub(super) struct BookmarkListsUi {
    names: Vec<String>,
    // Default is an implicit UI list in legacy/missing workspace metadata.
    default_was_persisted: bool,
    selected: String,
    draft: String,
    path_input: String,
    preview: Option<usize>,
    query: String,
    group_folders: bool,
    select_only: bool,
    follow_source: bool,
    pin_preview: bool,
    hide_preview: bool,
}
impl BookmarkListsUi {
    pub(super) fn restore(&mut self, names: Vec<String>) {
        self.default_was_persisted = names.iter().any(|name| name == DEFAULT_LIST);
        self.names = vec![DEFAULT_LIST.into()];
        for name in names {
            self.insert_name(&name);
        }
        self.selected = DEFAULT_LIST.into();
        self.preview = None;
    }
    pub(super) fn selected_name(&self) -> String {
        if self.selected.is_empty() {
            DEFAULT_LIST.into()
        } else {
            self.selected.clone()
        }
    }
    pub(super) fn persisted_names(&self) -> Vec<String> {
        self.names
            .iter()
            .filter(|name| self.default_was_persisted || name.as_str() != DEFAULT_LIST)
            .cloned()
            .collect()
    }
    fn insert_name(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 128
            || self.names.len() >= 64
            || self.names.iter().any(|n| n == name)
        {
            return false;
        }
        self.names.push(name.into());
        true
    }
}
pub(super) fn list_name(bookmark: &Bookmark) -> &str {
    if bookmark.list.is_empty() {
        DEFAULT_LIST
    } else {
        &bookmark.list
    }
}
impl IdeApp {
    pub(super) fn reveal_bookmark_path(&mut self, path: &Path) {
        if let Some(index) = self
            .bookmarks
            .iter()
            .position(|bookmark| bookmark.path == path)
        {
            self.bookmark_lists.selected = list_name(&self.bookmarks[index]).to_string();
            self.bookmark_lists.preview = Some(index);
        } else {
            self.set_status("This file has no bookmark; add one with F11");
        }
    }

    pub(super) fn go_to_numbered_bookmark(&mut self, digit: char) {
        self.reconcile_document_markers();
        let target = self
            .bookmarks
            .iter()
            .find(|bookmark| bookmark.mnemonic == Some(digit))
            .cloned();
        if let Some(target) = target {
            self.open_bookmark_target(&target);
        } else {
            self.set_status(format!("No bookmark with mnemonic {digit}"));
        }
    }
    pub(super) fn restore_bookmark_lists(&mut self, names: Vec<String>) {
        self.bookmark_lists.restore(names);
        for bookmark in &self.bookmarks {
            self.bookmark_lists.insert_name(list_name(bookmark));
        }
    }
    pub(super) fn add_bookmark_list(&mut self, name: &str) -> bool {
        if self.bookmark_lists.names.is_empty() {
            self.bookmark_lists.restore(Vec::new());
        }
        if !self.bookmark_lists.insert_name(name) {
            return false;
        }
        self.bookmark_lists.selected = name.trim().into();
        self.save_workspace();
        true
    }
    pub(super) fn rename_bookmark_list(&mut self, old: &str, new: &str) -> bool {
        let new = new.trim();
        if old == DEFAULT_LIST
            || new.is_empty()
            || new.len() > 128
            || self.bookmark_lists.names.iter().any(|n| n == new)
        {
            return false;
        }
        let Some(index) = self.bookmark_lists.names.iter().position(|n| n == old) else {
            return false;
        };
        self.bookmark_lists.names[index] = new.into();
        for bookmark in &mut self.bookmarks {
            if list_name(bookmark) == old {
                bookmark.list = new.into();
            }
        }
        if self.bookmark_lists.selected == old {
            self.bookmark_lists.selected = new.into();
        }
        self.save_workspace();
        true
    }
    /// Deleting a list moves its entries to Default; it never discards bookmarks.
    pub(super) fn delete_bookmark_list(&mut self, name: &str) -> bool {
        if name == DEFAULT_LIST || !self.bookmark_lists.names.iter().any(|n| n == name) {
            return false;
        }
        self.bookmark_lists.names.retain(|n| n != name);
        for bookmark in &mut self.bookmarks {
            if list_name(bookmark) == name {
                bookmark.list.clear();
            }
        }
        if self.bookmark_lists.selected == name {
            self.bookmark_lists.selected = DEFAULT_LIST.into();
        }
        self.save_workspace();
        true
    }
    pub(super) fn add_path_bookmark(&mut self, path: &Path) -> bool {
        // User action only: never stat or read source while painting rows.
        let Ok(metadata) = std::fs::metadata(path) else {
            self.set_status("Bookmark target does not exist");
            return false;
        };
        let kind = if metadata.is_dir() {
            BookmarkKind::Folder
        } else if metadata.is_file() {
            BookmarkKind::File
        } else {
            return false;
        };
        let path = match dunce::canonicalize(path) {
            Ok(path) => path,
            Err(_) => return false,
        };
        if self
            .bookmarks
            .iter()
            .any(|b| b.path == path && b.kind == kind)
        {
            return false;
        }
        if self.bookmark_lists.names.is_empty() {
            self.bookmark_lists.restore(Vec::new());
        }
        self.bookmarks.push(Bookmark {
            path,
            line: 0, // File/folder identity cannot collide with a line-1 bookmark.
            mnemonic: None,
            description: String::new(),
            kind,
            list: self.bookmark_lists.selected.clone(),
        });
        self.save_workspace();
        true
    }
    pub(super) fn open_bookmark_target(&mut self, bookmark: &Bookmark) {
        if bookmark.kind == BookmarkKind::Folder {
            if !bookmark.path.is_dir()
                || !self.project.as_ref().is_some_and(|project| {
                    bookmark.path.starts_with(&project.root)
                        || project
                            .tree
                            .content_roots
                            .iter()
                            .any(|root| bookmark.path.starts_with(root))
                })
            {
                self.set_status("Folder bookmark is unavailable in the current project");
                return;
            }
            self.mark_nav_jump();
            if let Some(project) = &mut self.project {
                project.tree.reveal(&bookmark.path);
                project.tree.expanded.insert(bookmark.path.clone());
                project.tree.request_focus();
            }
            self.left_tool = LeftTool::Project;
            self.layout.left_visible = true;
            self.focus_editor = false;
            self.set_status(format!("Folder bookmark: {}", bookmark.path.display()));
            return;
        }
        self.mark_nav_jump();
        self.open_file(&bookmark.path);
        if let Some(tab) = self
            .opened_editor_index(&bookmark.path)
            .and_then(|i| self.tabs.get_mut(i))
        {
            tab.view.cancel_session_restore();
            if bookmark.kind == BookmarkKind::Line {
                tab.state.go_to_line(bookmark.line, None);
            }
        }
        self.focus_editor = true;
    }
    fn bookmark_preview(&self, bookmark: &Bookmark) -> String {
        if bookmark.kind == BookmarkKind::Folder {
            return bookmark.path.display().to_string();
        }
        let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.state.path.as_deref() == Some(bookmark.path.as_path()))
        else {
            return "Open bookmark to preview".into();
        };
        let line = bookmark
            .line
            .saturating_sub(1)
            .min(tab.state.doc.line_count().saturating_sub(1));
        (line.saturating_sub(2)..(line + 3).min(tab.state.doc.line_count()))
            .map(|line| {
                let range = tab.state.doc.line_range(line);
                let end = tab.state.doc.clamp(TextSize::new(
                    u32::from(range.start())
                        .saturating_add(960)
                        .min(u32::from(range.end())),
                ));
                let text = tab.state.doc.slice(TextRange::new(range.start(), end));
                format!(
                    "{}  {}",
                    line + 1,
                    text.trim_end_matches(['\r', '\n'])
                        .chars()
                        .take(240)
                        .collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub(super) fn bookmark_lists_view(&mut self, ui: &mut egui::Ui) {
        if self.bookmark_lists.names.is_empty() {
            self.restore_bookmark_lists(Vec::new());
        }
        let mut create = false;
        let mut rename = false;
        let mut delete = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("bookmark_list")
                .selected_text(&self.bookmark_lists.selected)
                .show_ui(ui, |ui| {
                    for name in &self.bookmark_lists.names {
                        ui.selectable_value(&mut self.bookmark_lists.selected, name.clone(), name);
                    }
                })
                .combo_named(crate::i18n::tr_static("Bookmark list"));
            ui.text_edit_singleline(&mut self.bookmark_lists.draft);
            create = ui.button("New list").clicked();
            rename = ui
                .add_enabled(
                    self.bookmark_lists.selected != DEFAULT_LIST,
                    egui::Button::new("Rename list"),
                )
                .clicked();
            delete = ui
                .add_enabled(
                    self.bookmark_lists.selected != DEFAULT_LIST,
                    egui::Button::new("Delete list"),
                )
                .clicked();
        });
        let selected = self.bookmark_lists.selected.clone();
        let draft = self.bookmark_lists.draft.clone();
        if create && self.add_bookmark_list(&draft) {
            self.bookmark_lists.draft.clear();
        }
        if rename && self.rename_bookmark_list(&selected, &draft) {
            self.bookmark_lists.draft.clear();
        }
        if delete {
            self.delete_bookmark_list(&selected);
        }
        let mut add = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.bookmark_lists.path_input)
                    .hint_text("File or folder path"),
            );
            add = ui.button("Add file or folder").clicked();
        });
        if add {
            let path = PathBuf::from(&self.bookmark_lists.path_input);
            self.add_path_bookmark(&path);
        }
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.bookmark_lists.query)
                    .hint_text("Speed search bookmarks"),
            );
            ui.checkbox(&mut self.bookmark_lists.group_folders, "Group by directory");
            let mut follow = !self.bookmark_lists.select_only;
            ui.checkbox(&mut follow, "Autoscroll to source");
            self.bookmark_lists.select_only = !follow;
            ui.checkbox(
                &mut self.bookmark_lists.follow_source,
                "Autoscroll from source",
            );
            ui.checkbox(&mut self.bookmark_lists.pin_preview, "Pin preview");
            let mut preview = !self.bookmark_lists.hide_preview;
            ui.checkbox(&mut preview, "Preview");
            self.bookmark_lists.hide_preview = !preview;
        });
        if self.bookmark_lists.follow_source && !self.bookmark_lists.pin_preview {
            if let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) {
                if let Some(path) = tab.state.path.as_ref() {
                    let line = tab.state.doc.line_of(tab.state.primary().head()) + 1;
                    if let Some((index, _)) = self
                        .bookmarks
                        .iter()
                        .enumerate()
                        .filter(|(_, bookmark)| {
                            &bookmark.path == path
                                && list_name(bookmark) == self.bookmark_lists.selected
                                && bookmark.line <= line
                        })
                        .max_by_key(|(_, bookmark)| bookmark.line)
                    {
                        self.bookmark_lists.preview = Some(index);
                    }
                }
            }
        }
        let mut open = None;
        let mut remove = None;
        let mut edit = None;
        let mut membership = None;
        egui::ScrollArea::vertical()
            .id_salt("bookmark_lists_rows")
            .max_height(350.0)
            .show(ui, |ui| {
                let mut rows: Vec<_> = self
                    .bookmarks
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| {
                        list_name(b) == self.bookmark_lists.selected
                            && crate::project_tree::speed_search_matches(
                                &format!("{} {} {:?}", b.path.display(), b.description, b.kind),
                                &self.bookmark_lists.query,
                            )
                    })
                    .collect();
                rows.sort_by_key(|(_, b)| (&b.path, b.line));
                if rows.is_empty() {
                    ui.label("No bookmarks in this list");
                }
                let mut last_folder = None;
                for (index, bookmark) in rows {
                    if self.bookmark_lists.group_folders && last_folder != bookmark.path.parent() {
                        last_folder = bookmark.path.parent();
                        ui.strong(last_folder.unwrap_or(Path::new(".")).display().to_string());
                    }
                    let kind = match bookmark.kind {
                        BookmarkKind::Line => format!("line {}", bookmark.line),
                        BookmarkKind::File => "file".into(),
                        BookmarkKind::Folder => "folder".into(),
                    };
                    let label = format!(
                        "{} · {} {}",
                        bookmark
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        kind,
                        bookmark.description
                    );
                    let preview = self.bookmark_preview(bookmark);
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            let response = ui
                                .selectable_label(self.bookmark_lists.preview == Some(index), label)
                                .on_hover_text(preview);
                            if response.clicked() {
                                if !self.bookmark_lists.pin_preview {
                                    self.bookmark_lists.preview = Some(index);
                                }
                                if !self.bookmark_lists.select_only {
                                    open = Some(index);
                                }
                            }
                            if response.double_clicked()
                                || response.has_focus()
                                    && ui.input(|input| input.key_pressed(egui::Key::Enter))
                            {
                                open = Some(index);
                            }
                            if self.bookmark_lists.follow_source
                                && self.bookmark_lists.preview == Some(index)
                            {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            egui::ComboBox::from_id_salt("membership")
                                .selected_text(list_name(bookmark))
                                .show_ui(ui, |ui| {
                                    for name in &self.bookmark_lists.names {
                                        if ui
                                            .selectable_label(list_name(bookmark) == name, name)
                                            .clicked()
                                        {
                                            membership = Some((index, name.clone()));
                                        }
                                    }
                                })
                                .combo_named(crate::i18n::tr_static("Bookmark list"));
                            if ui
                                .small_button("…")
                                .on_hover_text("Edit description")
                                .clicked()
                            {
                                edit = Some(index);
                            }
                            if ui
                                .small_button("×")
                                .on_hover_text("Remove bookmark")
                                .clicked()
                            {
                                remove = Some(index);
                            }
                        });
                    });
                }
            });
        if let Some(index) = open {
            if !self.bookmark_lists.pin_preview {
                self.bookmark_lists.preview = Some(index);
            }
            let bookmark = self.bookmarks[index].clone();
            self.open_bookmark_target(&bookmark);
        }
        if let Some((index, name)) = membership {
            self.bookmarks[index].list = name;
            self.save_workspace();
        }
        if let Some(index) = edit {
            self.dialog = Dialog::BookmarkDescription {
                index,
                text: self.bookmarks[index].description.clone(),
            };
        }
        if let Some(index) = remove {
            self.bookmarks.remove(index);
            self.bookmark_lists.preview = None;
            self.save_workspace();
        }
        if let Some(bookmark) = (!self.bookmark_lists.hide_preview)
            .then(|| {
                self.bookmark_lists
                    .preview
                    .and_then(|index| self.bookmarks.get(index))
            })
            .flatten()
        {
            ui.separator();
            ui.label(self.bookmark_preview(bookmark));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_lists_move_rename_delete_and_keep_empty_lists_without_losing_bookmarks() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.restore_bookmark_lists(vec!["Empty".into()]);
        assert!(app.add_bookmark_list("Review"));
        app.bookmarks.push(Bookmark {
            path: PathBuf::from("/p/a.py"),
            line: 2,
            mnemonic: Some('A'),
            description: "π🙂".into(),
            kind: BookmarkKind::Line,
            list: "Review".into(),
        });
        assert!(app.rename_bookmark_list("Review", "Follow up"));
        assert_eq!(app.bookmarks[0].list, "Follow up");
        assert!(app.delete_bookmark_list("Follow up"));
        assert_eq!(list_name(&app.bookmarks[0]), DEFAULT_LIST);
        assert_eq!(app.bookmarks[0].description, "π🙂");
        assert_eq!(app.bookmarks[0].mnemonic, Some('A'));
        assert!(app
            .bookmark_lists
            .persisted_names()
            .contains(&"Empty".to_string()));
        assert!(!app.delete_bookmark_list(DEFAULT_LIST));
        assert!(!app.add_bookmark_list("  "));
        assert!(!app.rename_bookmark_list("Empty", DEFAULT_LIST));
        let names = app.bookmark_lists.persisted_names();
        app.restore_bookmark_lists(names.clone());
        assert_eq!(app.bookmark_lists.persisted_names(), names);
    }
    #[test]
    fn real_file_folder_navigation_and_current_unsaved_line_preview() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("unicode.py");
        let folder = root.path().join("pkg");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&file, "π = 1\nvalue = 2\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.safe_mode = true;
        app.open_project(root.path());
        assert!(app.add_bookmark_list("Navigation"));
        assert!(app.add_path_bookmark(&file));
        assert!(app.add_path_bookmark(&folder));
        assert!(!app.add_path_bookmark(&folder));
        let directory = app
            .bookmarks
            .iter()
            .find(|b| b.kind == BookmarkKind::Folder)
            .unwrap()
            .clone();
        let tabs_before = app.tabs.len();
        app.layout.left_visible = false;
        app.left_tool = LeftTool::Bookmarks;
        app.open_bookmark_target(&directory);
        assert!(app.layout.left_visible);
        assert!(matches!(app.left_tool, LeftTool::Project));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.project
                .as_mut()
                .unwrap()
                .tree
                .ui(ui, &app.theme, &app.keymap, true);
        })
        .drop_without_applying_deltas();
        assert!(crate::project_tree::ProjectTree::has_focus(&ctx));
        assert_eq!(
            app.tabs.len(),
            tabs_before,
            "folder must not become an editor document"
        );
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected.as_deref(),
            Some(folder.as_path())
        );
        assert!(app
            .project
            .as_ref()
            .unwrap()
            .tree
            .expanded
            .contains(&folder));
        let file_bookmark = app
            .bookmarks
            .iter()
            .find(|b| b.kind == BookmarkKind::File)
            .unwrap()
            .clone();
        app.open_bookmark_target(&file_bookmark);
        let index = app.opened_editor_index(&file).unwrap();
        app.tabs[index].state.go_to_line(2, None);
        app.tabs[index]
            .state
            .insert_text("UNSAVED 🙂 ", &EditOptions::default());
        let line = Bookmark {
            path: file.clone(),
            line: 2,
            mnemonic: None,
            description: String::new(),
            kind: BookmarkKind::Line,
            list: "Navigation".into(),
        };
        assert!(app
            .bookmark_preview(&line)
            .contains("2  UNSAVED 🙂 value = 2"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "π = 1\nvalue = 2\n"
        );
        let ws = rustpy_settings::Workspace::load(root.path());
        assert!(ws.bookmark_lists.contains(&"Navigation".to_string()));
        assert!(ws
            .bookmarks
            .iter()
            .any(|b| b.kind == BookmarkKind::Folder && b.path == folder));
    }
    #[test]
    fn external_configured_content_root_folder_is_revealed_and_focused() {
        let root = tempfile::tempdir().unwrap();
        let content = tempfile::tempdir().unwrap();
        let folder = content.path().join("package");
        std::fs::create_dir(&folder).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.safe_mode = true;
        app.open_project(root.path());
        app.project
            .as_mut()
            .unwrap()
            .tree
            .content_roots
            .push(content.path().to_path_buf());
        app.layout.left_visible = false;
        app.left_tool = LeftTool::Bookmarks;
        let bookmark = Bookmark {
            path: folder.clone(),
            line: 0,
            mnemonic: None,
            description: String::new(),
            kind: BookmarkKind::Folder,
            list: String::new(),
        };
        app.open_bookmark_target(&bookmark);
        assert!(app.layout.left_visible);
        assert!(matches!(app.left_tool, LeftTool::Project));
        let tree = &app.project.as_ref().unwrap().tree;
        assert_eq!(tree.selected.as_deref(), Some(folder.as_path()));
        assert!(tree.expanded.contains(&folder));
        assert!(tree.expanded.contains(content.path()));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.project
                .as_mut()
                .unwrap()
                .tree
                .ui(ui, &app.theme, &app.keymap, true);
        })
        .drop_without_applying_deltas();
        assert!(crate::project_tree::ProjectTree::has_focus(&ctx));
        assert!(app.tabs.is_empty());
    }

    #[test]
    fn external_folder_refusal_preserves_hidden_tool_and_project_selection() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.safe_mode = true;
        app.open_project(root.path());
        app.layout.left_visible = false;
        app.left_tool = LeftTool::Bookmarks;
        let selected = app.project.as_ref().unwrap().tree.selected.clone();
        let expanded = app.project.as_ref().unwrap().tree.expanded.clone();
        let bookmark = Bookmark {
            path: external.path().to_path_buf(),
            line: 0,
            mnemonic: None,
            description: String::new(),
            kind: BookmarkKind::Folder,
            list: String::new(),
        };
        app.open_bookmark_target(&bookmark);
        assert!(!app.layout.left_visible);
        assert!(matches!(app.left_tool, LeftTool::Bookmarks));
        assert_eq!(app.project.as_ref().unwrap().tree.selected, selected);
        assert_eq!(app.project.as_ref().unwrap().tree.expanded, expanded);
    }

    #[test]
    fn actual_tool_window_pointer_creates_empty_list() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.restore_bookmark_lists(Vec::new());
        app.bookmark_lists.draft = "Review".into();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut button = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            },
            |ui| app.bookmark_lists_view(ui),
        );
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                if text.galley.text() == "New list" {
                    button = Some(text.pos + text.galley.rect.center().to_vec2());
                }
            }
        }
        output.drop_without_applying_deltas();
        let pos = button.expect("real list creation button");
        for pressed in [true, false] {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 700.0),
                    )),
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    ..Default::default()
                },
                |ui| app.bookmark_lists_view(ui),
            )
            .drop_without_applying_deltas();
        }
        assert_eq!(app.bookmark_lists.selected, "Review");
        assert!(app.bookmark_lists.names.contains(&"Review".to_string()));
        assert!(app.bookmarks.is_empty());
    }
    #[test]
    fn legacy_bookmark_workspace_defaults_to_line_and_default_list() {
        let ws: rustpy_settings::Workspace = serde_json::from_str(
            r#"{"bookmarks":[{"path":"a.py","line":2,"mnemonic":"A","description":"old"}]}"#,
        )
        .unwrap();
        assert_eq!(ws.bookmarks[0].kind, BookmarkKind::Line);
        assert!(ws.bookmarks[0].list.is_empty());
        assert!(ws.bookmark_lists.is_empty());
        let restored: rustpy_settings::Workspace =
            serde_json::from_slice(&serde_json::to_vec(&ws).unwrap()).unwrap();
        assert_eq!(restored, ws);
    }
    #[test]
    fn implicit_default_is_not_a_workspace_edit_but_custom_empty_lists_are() {
        let mut lists = BookmarkListsUi::default();
        lists.restore(Vec::new());
        assert_eq!(lists.selected_name(), DEFAULT_LIST);
        assert!(lists.persisted_names().is_empty());
        assert!(lists.insert_name("Empty review"));
        assert_eq!(lists.persisted_names(), ["Empty review"]);
        lists.restore(vec![DEFAULT_LIST.into()]);
        assert_eq!(
            lists.persisted_names(),
            [DEFAULT_LIST],
            "preserve explicitly saved legacy metadata"
        );

        let root = tempfile::tempdir().unwrap();
        let path = Workspace::path_for(root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"damaged workspace").unwrap();
        let mut settings = Settings {
            confirm_exit: false,
            ..Default::default()
        };
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
        assert_eq!(app.bookmark_lists.selected_name(), DEFAULT_LIST);
        app.set_startup_task("Reverted task", true);
        app.set_startup_task("Reverted task", false);
        assert!(app
            .workspace_client
            .as_ref()
            .unwrap()
            .pending
            .bookmark_lists
            .is_none());
        assert!(app
            .workspace_client
            .as_ref()
            .unwrap()
            .quit_clean_provisional());
        assert!(matches!(
            app.prepare_coordinated_quit("implicit-default"),
            window_lifecycle::QuitProgress::Prepared { .. }
        ));
        app.abort_coordinated_quit("implicit-default");
        assert_eq!(std::fs::read(&path).unwrap(), b"damaged workspace");
        assert!(app.add_bookmark_list("Actual empty user list"));
        assert_eq!(
            app.workspace_client
                .as_ref()
                .unwrap()
                .pending
                .bookmark_lists
                .as_ref(),
            Some(&vec!["Actual empty user list".to_string()])
        );
        assert!(!app
            .workspace_client
            .as_ref()
            .unwrap()
            .quit_clean_provisional());
        assert_eq!(std::fs::read(&path).unwrap(), b"damaged workspace");
    }
}
