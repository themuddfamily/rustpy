//! Import-only workflows share the semantic unused-binding authority, style
//! renderer and checked edit/Undo boundary. No formatter subprocess is needed.
use super::*;
use rustpy_refactor::refactor2::FileOp;

#[derive(Default)]
pub(super) struct State {
    pending: Option<(rustpy_text::DocumentId, u64, f64)>,
}
impl IdeApp {
    pub(super) fn optimize_imports_action(&mut self) {
        let Some(i) = self.active else { return };
        if self.tabs[i].state.doc.is_read_only() {
            self.set_status("Cannot optimize read-only imports");
            return;
        }
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        let edits = self.import_workflow_edits(&analysis, &file);
        if edits.is_empty() {
            self.set_status("Imports are already optimized");
            return;
        }
        if self.settings.code_style.preview_optimize_imports {
            self.preview_file_ops(
                vec![FileOp::Edit(file.path.clone(), edits)],
                "Optimize Imports",
            );
        } else {
            self.tabs[i]
                .state
                .apply_text_edits(edits, "Optimize Imports");
            self.set_status("Imports optimized");
            self.editor_format_notification("Imports optimized");
        }
    }
    fn import_workflow_edits(
        &self,
        analysis: &rustpy_semantic::Analysis,
        file: &Arc<rustpy_semantic::FileInfo>,
    ) -> Vec<rustpy_semantic::edit_helpers::Edit> {
        if file.text.len() > 1024 * 1024 || !file.parse.errors().is_empty() {
            return Vec::new();
        }
        if self.code_style_excludes(&file.path).unwrap_or(true)
            || self
                .settings
                .editor
                .editorconfig_for(&file.path)
                .ide
                .formatter_enabled
                == Some(false)
        {
            return Vec::new();
        }
        let style = &self.settings.code_style;
        let options = super::reformat_workflows::options_for(
            &self.settings.editor,
            style,
            &file.path,
            &file.text,
            style.keep_line_breaks,
        );
        super::reformat_workflows::policy_edits(
            &file.text,
            rustpy_format::imports::optimize_imports_with_style(
                file,
                &inspections::unused_import_ranges_with(analysis, file),
                &options.python.imports,
            ),
            &options,
        )
    }
    pub(super) fn import_workflow_frame(
        &mut self,
        i: usize,
        typed: &str,
        now: f64,
        focused: bool,
        ctx: &egui::Context,
    ) {
        if !focused
            || self.tabs[i].state.doc.is_read_only()
            || self.tabs[i].state.language != Language::Python
        {
            return;
        }
        if !typed.is_empty() {
            self.import_workflows.pending = Some((
                self.tabs[i].state.doc.identity(),
                self.tabs[i].state.doc.version(),
                now + 1.5,
            ));
        }
        let Some((document, version, due)) = self.import_workflows.pending else {
            return;
        };
        if self.settings.code_style.optimize_imports_on_fly && now < due {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((due - now).max(0.01)));
        }
        if !self.settings.code_style.optimize_imports_on_fly
            || now < due
            || self.completion.visible
            || self.import_completion.is_some()
        {
            return;
        }
        self.import_workflows.pending = None;
        if self.tabs[i].state.doc.identity() != document
            || self.tabs[i].state.doc.version() != version
        {
            return;
        }
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        // Do not remove an import while its own statement is being edited.
        use rustpy_syntax::ast::{self, AstNode};
        let caret = self.tabs[i].state.primary().head();
        if file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::Stmt::cast)
            .any(|s| {
                matches!(s, ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_))
                    && s.text_range().contains_inclusive(caret)
            })
        {
            return;
        }
        let edits = self.import_workflow_edits(&analysis, &file);
        self.tabs[i]
            .state
            .apply_text_edits(edits, "Optimize imports on the fly");
    }
    pub(super) fn optimize_checked_commit_imports(&mut self) -> Result<(), String> {
        let project = self.project.as_ref().ok_or("No project")?;
        let analysis = project.analysis.clone();
        let root = project.root.clone();
        let mut paths: Vec<_> = self
            .git_panel
            .checked
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    root.join(p)
                }
            })
            .collect();
        paths.sort();
        paths.dedup();
        if paths.len() > 128 {
            return Err("Optimize Imports supports at most 128 checked files per commit".into());
        }
        let mut operations = Vec::new();
        let mut bytes = 0usize;
        let mut revisions = Vec::new();
        for path in paths {
            if !matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("py" | "pyi" | "pyw")
            ) {
                continue;
            }
            if self
                .tabs
                .iter()
                .enumerate()
                .any(|(i, t)| t.state.path.as_ref() == Some(&path) && self.tab_changed_on_disk(i))
            {
                return Err(format!("{} changed on disk", path.display()));
            }
            let file = if let Some(i) = self
                .tabs
                .iter()
                .position(|t| t.state.path.as_ref() == Some(&path))
            {
                self.analysis_file(i).map(|(_, f)| f)
            } else {
                if !path.is_file() {
                    continue;
                }
                let snapshot = self
                    .file_vfs()
                    .read(&path, self.editor_read_limits())
                    .map_err(|e| e.to_string())?;
                if snapshot.bytes.len() > 1024 * 1024 {
                    return Err("Checked Python file exceeds 1 MiB".into());
                }
                let (text, _) =
                    rustpy_text::Encoding::decode(&snapshot.bytes).map_err(|e| e.to_string())?;
                revisions.push((path.clone(), snapshot.revision));
                let id = analysis.set_file_text(&path, &text);
                analysis.file(id)
            }
            .ok_or("Cannot capture checked Python file")?;
            bytes += file.text.len();
            if bytes > 16 * 1024 * 1024 {
                return Err("Checked import input exceeds 16 MiB".into());
            }
            let edits = self.import_workflow_edits(&analysis, &file);
            if !edits.is_empty() {
                operations.push(FileOp::Edit(path, edits));
            }
        }
        for (path, revision) in revisions {
            if self
                .file_vfs()
                .read(&path, self.editor_read_limits())
                .map_err(|e| e.to_string())?
                .revision
                != revision
            {
                return Err(format!(
                    "{} changed while preparing imports",
                    path.display()
                ));
            }
        }
        let errors = self.apply_file_ops_failures(operations, "Optimize imports before commit");
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl IdeApp {
    pub(super) fn start_automatic_import(&mut self, i: usize, typed: &str) {
        if self.tabs[i].state.doc.is_read_only() || self.tabs[i].state.language != Language::Python
        {
            return;
        }
        // Wait until the user finishes a name. Merely typing a prefix must not
        // insert imports for a different, longer identifier.
        if typed
            .chars()
            .last()
            .is_none_or(rustpy_core::is_ident_continue)
            || self.completion.visible
            || self.import_completion.is_some()
        {
            return;
        }
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        let actual = self.tabs[i].state.primary().head();
        let mut end = usize::from(actual);
        while end > 0 {
            let ch = file.text[..end].chars().next_back().unwrap();
            if ch.is_whitespace() || matches!(ch, '(' | ')' | ',' | ':') {
                end -= ch.len_utf8();
            } else {
                break;
            }
        }
        if end == 0 {
            return;
        }
        let start = file.text[..end]
            .rfind(|c: char| !rustpy_core::is_ident_continue(c))
            .map_or(0, |offset| {
                offset + file.text[offset..].chars().next().unwrap().len_utf8()
            });
        if start == end {
            return;
        }
        let Some(reference) = file.scopes.reference_at(TextSize::new(start as u32)) else {
            return;
        };
        if reference.resolved.is_some()
            || reference.is_write
            || reference.range
                != TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
        {
            return;
        }
        if !features::completion_context(&file.text, TextSize::new(end as u32)).is_code() {
            return;
        }
        self.completion = CompletionPopup {
            prefix_start: TextSize::new(start as u32),
            doc_version: self.tabs[i].state.doc.version(),
            caret: actual,
            ..Default::default()
        };
        self.start_import_completion_mode(
            analysis,
            file,
            TextSize::new(end as u32),
            true,
            false,
            true,
        );
    }
    pub(super) fn accept_unambiguous_import(
        &mut self,
        result: Vec<rustpy_semantic::completion2::ImportCompletion>,
    ) {
        if !self.settings.code_style.add_unambiguous_imports_on_fly || result.len() >= 200 {
            return;
        }
        let Some(i) = self.active else { return };
        if self.tabs[i].state.doc.is_read_only() {
            return;
        }
        let Some((_, file)) = self.analysis_file(i) else {
            return;
        };
        let start = usize::from(self.completion.prefix_start);
        let Some(reference) = file.scopes.reference_at(TextSize::new(start as u32)) else {
            return;
        };
        if reference.resolved.is_some() || reference.is_write {
            return;
        }
        let word = reference.name.as_str();
        let mut exact: Vec<_> = result
            .into_iter()
            .filter(|c| {
                !self
                    .settings
                    .editor
                    .preferences
                    .excluded_import(&format!("{}.{}", c.module, c.name))
                    && c.item.label == word
                    && (c.name == word || (c.name.is_empty() && c.module == word))
            })
            .collect();
        exact.sort_by(|a, b| (&a.module, &a.name).cmp(&(&b.module, &b.name)));
        exact.dedup_by(|a, b| a.module == b.module && a.name == b.name);
        if exact.len() != 1 {
            return;
        }
        let candidate = exact.pop().unwrap();
        let qualified =
            !self.settings.editor.preferences.prefer_from_imports && !candidate.name.is_empty();
        let edit = rustpy_semantic::completion2::import_edit_for(
            &file,
            &candidate.module,
            if qualified { "" } else { &candidate.name },
        );
        if edit.1.is_empty() {
            return;
        }
        let mut edits = vec![edit];
        if qualified {
            edits.push((
                TextRange::new(
                    TextSize::new(start as u32),
                    TextSize::new((start + word.len()) as u32),
                ),
                format!("{}.{word}", candidate.module),
            ));
        }
        self.tabs[i]
            .state
            .apply_text_edits(edits, "Add unambiguous import");
        self.set_status(format!("Imported {word} from {}", candidate.module));
    }
}

/// Class-qualified static methods use the same import/edit acceptance path as
/// class candidates: import the owner, insert Owner.method, never a bare method.
pub(super) fn static_import_candidates(
    db: &rustpy_semantic::Analysis,
    file: &Arc<rustpy_semantic::FileInfo>,
    caret: TextSize,
    files: &[PathBuf],
) -> Vec<rustpy_semantic::completion2::ImportCompletion> {
    use rustpy_semantic::{classes::MemberKind, scope::BindingKind, types::Type, Inferer};
    let offset = usize::from(caret);
    let start = file.text[..offset]
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map_or(0, |at| {
            at + file.text[at..].chars().next().unwrap().len_utf8()
        });
    let prefix = &file.text[start..offset];
    if prefix.is_empty() {
        return Vec::new();
    }
    let inf = Inferer::new(db);
    let mut output = Vec::new();
    for path in files.iter().take(4096) {
        let Some(other) = db.file_for_path(path) else {
            continue;
        };
        let Some(module) = other.module_name.as_ref() else {
            continue;
        };
        if path == &file.path {
            continue;
        }
        for (name, ids) in &other.scopes.scope(other.scopes.module_scope()).bindings {
            if name.starts_with('_')
                || file
                    .scopes
                    .resolve(file.scopes.scope_at(caret), name)
                    .is_some()
            {
                continue;
            }
            let Some(id) = ids
                .last()
                .filter(|id| matches!(other.scopes.binding(**id).kind, BindingKind::Class))
            else {
                continue;
            };
            let ty = inf.type_of_binding(&other, *id);
            let Type::Class(class, _) = &ty else { continue };
            let Some(info) = inf.class_info(class) else {
                continue;
            };
            for member in info.members.values().filter(|m| {
                m.kind == MemberKind::StaticMethod
                    && m.name.starts_with(prefix)
                    && !m.name.starts_with('_')
            }) {
                // Unknown decorator replacements do not establish callable slots.
                let Some(slot) = inf.lookup_member(&ty, &member.name) else {
                    continue;
                };
                if slot.signature().is_none() {
                    continue;
                }
                let insert = format!("{name}.{}()", member.name);
                let cursor = insert.len() - 1;
                output.push(rustpy_semantic::completion2::ImportCompletion {
                    item: CompletionItem {
                        label: member.name.clone(),
                        kind: CompletionKind::Method,
                        detail: format!("{module}.{name} (static method)"),
                        insert,
                        caret_offset: Some(cursor),
                        priority: 25,
                    },
                    module: module.clone(),
                    name: name.to_string(),
                    is_module: false,
                });
                if output.len() >= 200 {
                    return output;
                }
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;
    fn app(source: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("main.py");
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        app.open_file(&path);
        (directory, app, path)
    }
    #[test]
    fn explicit_optimizer_is_import_only_and_one_undo() {
        let source = "import os\nimport sys\nprint(sys.version)\nvalue=  1\n";
        let (_root, mut app, _) = app(source);
        app.optimize_imports_action();
        assert!(!app.tabs[0].state.doc.text().contains("import os"));
        assert!(app.tabs[0].state.doc.text().contains("value=  1"));
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    #[test]
    fn optimizer_preview_defers_changes_and_read_only_is_immutable() {
        let source = "import os\nvalue=1\n";
        let (_root, mut app, _) = app(source);
        app.settings.code_style.preview_optimize_imports = true;
        app.optimize_imports_action();
        assert_eq!(app.tabs[0].state.doc.text(), source);
        assert!(matches!(app.dialog, Dialog::RefactoringPreview(_)));
        app.dialog = Dialog::None;
        app.settings.code_style.preview_optimize_imports = false;
        app.tabs[0].state.doc.set_read_only(true);
        app.optimize_imports_action();
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }

    #[test]
    fn idle_optimization_waits_and_never_rewrites_current_import() {
        let source = "import os\nvalue=1\n";
        let (_root, mut app, _) = app(source);
        app.settings.code_style.optimize_imports_on_fly = true;
        app.tabs[0]
            .state
            .set_carets(vec![Caret::at(TextSize::new(2))]);
        let ctx = egui::Context::default();
        app.import_workflow_frame(0, "s", 0.0, true, &ctx);
        app.import_workflow_frame(0, "", 1.0, true, &ctx);
        assert_eq!(app.tabs[0].state.doc.text(), source);
        app.import_workflow_frame(0, "", 2.0, true, &ctx);
        assert_eq!(app.tabs[0].state.doc.text(), source);
        app.tabs[0]
            .state
            .set_carets(vec![Caret::at(TextSize::new(source.len() as u32))]);
        app.import_workflow_frame(0, "1", 3.0, true, &ctx);
        app.import_workflow_frame(0, "", 5.0, true, &ctx);
        assert!(!app.tabs[0].state.doc.text().contains("import os"));
    }
    fn candidate(module: &str, name: &str) -> rustpy_semantic::completion2::ImportCompletion {
        rustpy_semantic::completion2::ImportCompletion {
            item: CompletionItem {
                label: name.into(),
                kind: CompletionKind::Class,
                detail: module.into(),
                insert: name.into(),
                caret_offset: None,
                priority: 20,
            },
            module: module.into(),
            name: name.into(),
            is_module: false,
        }
    }
    #[test]
    fn unambiguous_import_requires_exact_unique_name_and_is_undoable() {
        let source = "Widget()\n";
        let (_root, mut app, _) = app(source);
        app.settings.code_style.add_unambiguous_imports_on_fly = true;
        app.completion.prefix_start = TextSize::new(0);
        app.accept_unambiguous_import(vec![candidate("one", "Widget"), candidate("two", "Widget")]);
        assert_eq!(app.tabs[0].state.doc.text(), source);
        app.accept_unambiguous_import(vec![candidate("one", "Widgets")]);
        assert_eq!(app.tabs[0].state.doc.text(), source);
        app.accept_unambiguous_import(vec![candidate("one", "Widget")]);
        assert!(app.tabs[0]
            .state
            .doc
            .text()
            .contains("from one import Widget"));
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    #[test]
    fn commit_import_optimization_changes_only_checked_python_files() {
        let source = "import os\nvalue=  1\n";
        let (root, mut app, path) = app(source);
        let other = root.path().join("other.py");
        std::fs::write(&other, source).unwrap();
        app.git_panel.checked.insert(path.clone());
        app.optimize_checked_commit_imports().unwrap();
        assert!(!app.tabs[0].state.doc.text().contains("import os"));
        assert!(app.tabs[0].state.doc.text().contains("value=  1"));
        assert_eq!(std::fs::read_to_string(other).unwrap(), source);
    }
    #[test]
    fn commit_optimizer_preserves_closed_crlf_file_and_conditional_imports() {
        let source = "print('main')\n";
        let (root, mut app, _) = app(source);
        let closed = root.path().join("closed.py");
        std::fs::write(
            &closed,
            "import os\r\nimport sys\r\nif False:\r\n    import optional\r\nprint(sys.version)\r\n",
        )
        .unwrap();
        app.git_panel.checked.insert(closed.clone());
        app.optimize_checked_commit_imports().unwrap();
        assert_eq!(
            std::fs::read_to_string(closed).unwrap(),
            "import sys\r\nif False:\r\n    import optional\r\nprint(sys.version)\r\n"
        );
    }

    #[test]
    fn static_popup_imports_actual_owner_and_inserts_qualified_member() {
        let (root, app, _) = app("prod\n");
        let helper = root.path().join("helper.py");
        std::fs::write(&helper,"class Factory:\n    @staticmethod\n    def produce(value: int) -> int:\n        return value\n    def ordinary(self): pass\n").unwrap();
        let (db, file) = app.analysis_file(0).unwrap();
        let result = static_import_candidates(&db, &file, TextSize::new(4), &[helper]);
        let item = result.iter().find(|c| c.item.label == "produce").unwrap();
        assert_eq!(item.name, "Factory");
        assert_eq!(item.module, "helper");
        assert_eq!(item.item.insert, "Factory.produce()");
    }
}
