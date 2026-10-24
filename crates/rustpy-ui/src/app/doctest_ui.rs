//! Completion inside real docstring prompt code, in an isolated analysis world.
use super::*;
impl IdeApp {
    pub(super) fn open_doctest_completion(&mut self, explicit: bool) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        if self.tabs[index].state.language != Language::Python {
            return false;
        }
        let caret = self.tabs[index].state.primary().head();
        // Examples live in docstrings: elsewhere, skip walking the whole tree
        // (the per-frame tree is enough to tell; a large file's may lag a
        // keystroke behind, which only delays this lookup to the next one).
        let parse = self.tabs[index].state.frame_syntax().clone();
        let caret_in_tree = caret.min(parse.syntax_node().text_range().end());
        let in_string = [
            parse
                .syntax_node()
                .token_at_offset(caret_in_tree)
                .left_biased(),
            parse
                .syntax_node()
                .token_at_offset(caret_in_tree)
                .right_biased(),
        ]
        .into_iter()
        .flatten()
        .any(|token| token.kind() == rustpy_syntax::SyntaxKind::STRING);
        if !in_string {
            return false;
        }
        let parse = self.tabs[index].state.syntax().clone();
        let source = self.tabs[index].state.doc.text();
        let Some(test) = rustpy_syntax::doctest::discover(&source, &parse)
            .into_iter()
            .find(|test| {
                test.lines
                    .iter()
                    .any(|line| line.code.contains_inclusive(caret))
            })
        else {
            return false;
        };
        let Some((virtual_source, virtual_caret, line)) =
            rustpy_syntax::doctest::completion_source(&source, &test, caret)
        else {
            self.completion.visible = false;
            self.import_completion = None;
            if explicit {
                self.set_status(
                    "Completion is unavailable inside an encoded or invalid example identifier",
                );
            }
            return true;
        };
        let before = &source[usize::from(line.start())..usize::from(caret)];
        let relative = before
            .rfind(|c: char| !rustpy_core::is_ident_continue(c))
            .map_or(0, |at| at + before[at..].chars().next().unwrap().len_utf8());
        let prefix = &before[relative..];
        // The isolated world is built, and completed off the UI thread, only
        // when no current result serves this prompt.
        let prefix_start = usize::from(line.start()) + relative;
        let found = self.doctest_completion_candidates(prefix_start, explicit, |app| {
            let (analysis, file) = app.analysis_file(index)?;
            let resolver = rustpy_semantic::module::ModuleResolver::new(
                analysis.resolver.roots.clone(),
                analysis.resolver.search_paths.clone(),
                analysis.resolver.python_version,
            )
            .with_stub_roots(analysis.resolver.stub_roots.iter().cloned())
            .with_platform(&analysis.resolver.platform)
            .with_excluded(analysis.resolver.excluded_paths().iter().cloned());
            let query = Analysis::new(resolver);
            query.set_runtime_types((*analysis.runtime_types()).clone());
            let mut bytes = virtual_source.len();
            for tab in app.tabs.iter().filter(|tab| tab.state.doc.is_modified()) {
                if let Some(path) = tab.state.path.as_ref().filter(|path| **path != file.path) {
                    let text = tab.state.doc.text();
                    bytes += text.len();
                    if bytes > 8 * 1024 * 1024 {
                        return None;
                    }
                    query.set_file_text(path, &text);
                }
            }
            let id = query.set_file_text(&file.path, &virtual_source);
            let target = query.file(id)?;
            Some((query, target, virtual_caret))
        });
        let Some(items) = found else {
            if self.completion_worker.pending() {
                self.show_loading_lookup(TextSize::new(prefix_start as u32), explicit);
            }
            return true;
        };
        let prefs = &self.settings.completion;
        let settings = rustpy_semantic::completion2::CompletionSettings {
            show_as_you_type: prefs.show_as_you_type,
            case_sensitive: prefs.case_sensitive,
            match_middle: prefs.match_middle,
            hide_used: prefs.hide_used,
        };
        let items = items
            .into_iter()
            .filter(|item| {
                !item.insert.contains(['\n', '\r', '\\'])
                    && !item.insert.contains("\"\"\"")
                    && !item.insert.contains("'''")
            })
            .collect();
        let items = rustpy_semantic::completion2::filter_items(items, prefix, &settings);
        let items = self.order_completion_items(items, prefix);
        let count = items.len();
        self.completion_ui.due = None;
        self.completion_ui.paths = false;
        self.import_completion = None;
        self.completion = CompletionPopup {
            visible: count > 0,
            items,
            imports: vec![None; count],
            selected: if prefs.preselect_first { 0 } else { usize::MAX },
            prefix_start: line.start() + TextSize::new(relative as u32),
            doc_version: self.tabs[index].state.doc.version(),
            caret,
            explicit,
            rect: None,
        };
        true
    }
}
/// [`is_doctest_prompt`] for an editor: only a `>>>`/`...` line is parsed.
pub(super) fn is_doctest_prompt_at(state: &crate::editor::EditorState, caret: TextSize) -> bool {
    let caret = caret.min(state.doc.len());
    let line = state.doc.line_range(state.doc.line_of(caret));
    let head = state
        .doc
        .slice(TextRange::new(line.start(), caret.max(line.start())));
    let head = head.trim_start();
    if !head.starts_with(">>>") && !head.starts_with("...") {
        return false;
    }
    is_doctest_prompt(&state.doc.text(), caret)
}

pub(super) fn is_doctest_prompt(text: &str, caret: TextSize) -> bool {
    if text.len() > 1024 * 1024 {
        return false;
    }
    let offset = usize::from(caret).min(text.len());
    if !text.is_char_boundary(offset) {
        return false;
    }
    let line = text[..offset]
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .trim_start();
    if !line.starts_with(">>>") && !line.starts_with("...") {
        return false;
    }
    let parse = rustpy_syntax::parse(text);
    rustpy_syntax::doctest::discover(text, &parse)
        .iter()
        .any(|test| {
            test.lines
                .iter()
                .any(|line| line.code.contains_inclusive(caret))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_completion_uses_module_and_prior_example_values_then_actual_accept_undo() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("examples.py");
        let source="def helper():\n    return 'π'\n\ndef example():\n    r\"\"\"Examples.\n    >>> value = helper()\n    >>> value.up\n    \"\"\"\n";
        std::fs::write(&path, source).unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        let base = root.path().to_path_buf();
        app.project = Some(Project {
            tree: ProjectTree::new(base.clone()),
            analysis: Arc::new(Analysis::for_project(&base)),
            root: base,
            run_configs: RunConfigStore::default(),
            interpreters: vec![],
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app.open_file(&path);
        let index = app.active.unwrap();
        let caret = TextSize::new((source.find("value.up").unwrap() + 8) as u32);
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(caret)]);
        app.sync_analysis_now();
        assert!(app.open_doctest_completion(true));
        let selected = app
            .completion
            .items
            .iter()
            .position(|item| item.label == "upper")
            .expect("real str member");
        app.completion.selected = selected;
        app.accept_completion(false);
        assert!(app.tabs[index]
            .state
            .doc
            .text()
            .contains(">>> value.upper("));
        app.tabs[index].state.undo();
        assert_eq!(app.tabs[index].state.doc.text().as_str(), source);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
}
