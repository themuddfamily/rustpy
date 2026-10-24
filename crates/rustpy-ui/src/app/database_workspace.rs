//! SQL injections use the editor's decoded literal authority and a captured DB scope.
use super::*;
impl IdeApp {
    pub(super) fn run_database_sql(&mut self) {
        if !self.builtin_plugin_enabled("database") {
            self.set_status("The bundled Database plugin is disabled");
            return;
        }
        if self.project_safe_mode() {
            self.notify_safe_mode("Database execution");
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        let tab = &mut self.tabs[index];
        let path = tab.state.path.clone();
        let sql = if let Some(injection) = tab.state.current_injection() {
            if injection.language != crate::editor::language_injections::Language::Sql {
                self.set_status("Caret is not in an injected SQL string");
                return;
            }
            match crate::editor::language_injections::Fragment::open(&tab.state, injection) {
                Ok(fragment) => fragment.text,
                Err(e) => {
                    self.set_status(e);
                    return;
                }
            }
        } else {
            let caret = tab.state.primary();
            if caret.has_selection() {
                tab.state.doc.slice(caret.range())
            } else if tab.state.language == Language::Sql {
                tab.state.doc.text()
            } else {
                self.set_status("Select SQL or place the caret in an injected SQL string");
                return;
            }
        };
        let Some(project) = &self.project else {
            return;
        };
        let root = project.root.clone();
        self.db_panel.load(&root);
        self.db_panel
            .set_database_execution_allowed(!self.safe_mode);
        match self.db_panel.open_scoped_sql(path.as_deref(), sql) {
            Ok(()) => self.bottom = Some(BottomTool::Database),
            Err(e) => self.set_status(e),
        }
    }
    pub(super) fn database_resolution_scope(&mut self) {
        if !self.builtin_plugin_enabled("database") {
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        let Some(path) = self.tabs[index].state.path.clone() else {
            return;
        };
        let Some(project) = &self.project else {
            return;
        };
        self.db_panel.load(&project.root);
        self.db_panel.edit_database_scope(&path);
        self.bottom = Some(BottomTool::Database);
    }
}

impl IdeApp {
    pub(super) fn open_database_completion(&mut self, explicit: bool) -> bool {
        if !self.builtin_plugin_enabled("database") {
            return false;
        }
        let Some(index) = self.active else {
            return false;
        };
        let tab = &mut self.tabs[index];
        let text = tab.state.doc.text();
        let end = usize::from(tab.state.primary().head());
        let start_limit = if tab.state.language == Language::Sql {
            0
        } else {
            let Some(injection) = tab
                .state
                .current_injection()
                .filter(|i| i.language == crate::editor::language_injections::Language::Sql)
            else {
                return false;
            };
            let start = usize::from(injection.content.start());
            if text.get(start..end).is_none_or(|s| s.contains('\\')) {
                return false;
            }
            start
        };
        let Some(before) = text.get(start_limit..end) else {
            return false;
        };
        let prefix = before
            .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("");
        let start = end - prefix.len();
        let (words, _) = self
            .db_panel
            .sql_schema_hints(tab.state.path.as_deref(), before);
        let items = words
            .into_iter()
            .map(|word| CompletionItem {
                label: word.clone().into(),
                insert: word,
                detail: "Database resolution scope".into(),
                kind: CompletionKind::Variable,
                caret_offset: None,
                priority: 0,
            })
            .collect::<Vec<_>>();
        self.completion = CompletionPopup {
            visible: !items.is_empty(),
            imports: vec![None; items.len()],
            items,
            selected: 0,
            prefix_start: TextSize::new(start as u32),
            doc_version: tab.state.doc.version(),
            caret: tab.state.primary().head(),
            explicit,
            rect: None,
        };
        self.completion_ui.paths = true;
        self.completion_ui.due = None;
        true
    }
}

impl IdeApp {
    pub(super) fn database_process_labels(&self) -> Vec<String> {
        let mut labels: Vec<_> = self
            .db_panel
            .database_owners()
            .into_iter()
            .map(|(_, name, _, stopping)| {
                format!(
                    "Database {name}{}",
                    if stopping { " (stopping)" } else { "" }
                )
            })
            .collect();
        let native_changes = self.db_panel.database_native_pending_changes();
        let changes = self
            .db_panel
            .database_pending_changes()
            .saturating_sub(native_changes);
        if changes > 0 {
            labels.push(format!(
                "Database: {changes} pending change batches will be discarded"
            ));
        }
        if native_changes > 0 {
            labels.push(format!(
                "Database: {native_changes} key/document edit sessions have local edits or unconfirmed writes; submitted writes may already be stored"
            ));
        }
        labels.extend(
            rustpy_db::native::poll_retiring()
                .into_iter()
                .map(|_| "Database connection is still stopping".to_owned()),
        );
        labels
    }
    pub(super) fn database_process_stamp(&self) -> (Vec<u64>, usize, Vec<String>) {
        let mut native = self.db_panel.database_native_stamps();
        native.extend(rustpy_db::native::poll_retiring());
        (
            self.db_panel
                .database_owners()
                .into_iter()
                .map(|owner| owner.0)
                .collect(),
            self.db_panel.database_pending_changes(),
            native,
        )
    }
}
