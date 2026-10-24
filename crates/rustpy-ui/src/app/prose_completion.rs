//! Explicit text-only lookup in Python comments and actual docstrings.
use super::*;
use rustpy_syntax::{ast, SyntaxKind, SyntaxNode};

fn item(name: &str, kind: CompletionKind, detail: &str) -> CompletionItem {
    CompletionItem {
        label: name.into(),
        kind,
        detail: detail.into(),
        insert: name.into(),
        caret_offset: None,
        priority: 100,
    }
}

fn unicode_docstring(doc: &ast::StringLike) -> bool {
    match doc {
        ast::StringLike::Single(string) => !string.is_bytes(),
        ast::StringLike::Concat(strings) => {
            strings.parts().next().is_some()
                && strings
                    .parts()
                    .all(|part| matches!(part, ast::Expr::String(string) if !string.is_bytes()))
        }
    }
}

fn docstring_owner(root: &SyntaxNode, caret: TextSize) -> Option<Option<ast::FuncDef>> {
    for node in root.descendants() {
        let function = ast::FuncDef::cast(node.clone());
        let doc = if let Some(function) = &function {
            function.docstring()
        } else if let Some(class) = ast::ClassDef::cast(node.clone()) {
            class.docstring()
        } else if let Some(module) = ast::Module::cast(node) {
            module.docstring()
        } else {
            None
        };
        if doc
            .is_some_and(|doc| unicode_docstring(&doc) && doc.syntax().text_range().contains(caret))
        {
            return Some(function);
        }
    }
    None
}

impl IdeApp {
    pub(in crate::app) fn open_prose_completion(&mut self, explicit: bool) -> bool {
        use rustpy_semantic::completion_ctx::CompletionContext;
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        if tab.state.language != Language::Python {
            return false;
        }
        let caret = tab.state.primary().head();
        let context = self.tabs[index].state.completion_context_at(caret);
        if !matches!(
            context,
            CompletionContext::Comment | CompletionContext::StringLiteral
        ) {
            return false;
        }
        let source = self.tabs[index].state.doc.text();
        let root = self.tabs[index].state.syntax().syntax_node();
        let owner = if context == CompletionContext::StringLiteral {
            let Some(owner) = docstring_owner(&root, caret) else {
                return false;
            };
            owner
        } else {
            None
        };
        // Typing can refresh an explicitly opened lookup, but prose must not
        // acquire an automatic popup or an auto-import worker of its own.
        let explicit = explicit || (self.completion.visible && self.completion.explicit);
        if !explicit {
            return true;
        }
        let before = &source[..usize::from(caret)];
        let start = before
            .rfind(|ch: char| !rustpy_core::is_ident_continue(ch))
            .map_or(0, |at| at + before[at..].chars().next().unwrap().len_utf8());
        let prefix = &before[start..];
        let line = before.rsplit('\n').next().unwrap_or_default();
        let head = line[..line.len() - prefix.len()].trim_start();
        let head = if head.starts_with(['r', 'R', 'u', 'U']) && head[1..].starts_with(['\'', '"']) {
            &head[1..]
        } else {
            head
        };
        let head = head.trim_start_matches(['\'', '"']);
        let mut items = Vec::new();
        if source.len() <= 1024 * 1024 {
            if context == CompletionContext::StringLiteral && head == ":" {
                for field in [
                    "param", "type", "return", "rtype", "raises", "ivar", "vartype",
                ] {
                    items.push(item(field, CompletionKind::Keyword, "Docstring field"));
                }
            } else if context == CompletionContext::StringLiteral
                && head
                    .strip_prefix(":param ")
                    .is_some_and(|rest| !rest.contains(':'))
            {
                if let Some(params) = owner.and_then(|function| function.params()) {
                    items.extend(
                        params
                            .params()
                            .filter_map(|parameter| parameter.name())
                            .map(|name| {
                                item(
                                    name.text(),
                                    CompletionKind::Parameter,
                                    "Documented function parameter",
                                )
                            }),
                    );
                }
            } else {
                // These are document words, not callable/code candidates: accepting
                // one never adds parentheses, imports, live templates or snippets.
                for token in root
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                {
                    if token.kind() == SyntaxKind::IDENT
                        && token.text_range().start() != TextSize::new(start as u32)
                    {
                        items.push(item(
                            token.text(),
                            CompletionKind::Variable,
                            "Name in this Python document",
                        ));
                    }
                }
                items.extend(
                    features::PYTHON_KEYWORDS
                        .iter()
                        .map(|name| item(name, CompletionKind::Keyword, "Python keyword")),
                );
            }
        }
        let prefs = &self.settings.completion;
        let matching = rustpy_semantic::completion2::CompletionSettings {
            show_as_you_type: prefs.show_as_you_type,
            case_sensitive: prefs.case_sensitive,
            match_middle: prefs.match_middle,
            hide_used: prefs.hide_used,
        };
        let items = rustpy_semantic::completion2::filter_items(items, prefix, &matching);
        let mut items = self.order_completion_items(items, prefix);
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert(item.label.clone()));
        let count = items.len();
        self.completion_ui.due = None;
        self.completion_ui.paths = false;
        self.completion_ui.reset_help();
        self.import_completion = None;
        self.completion = CompletionPopup {
            visible: count > 0,
            items,
            imports: vec![None; count],
            selected: if prefs.preselect_first { 0 } else { usize::MAX },
            prefix_start: TextSize::new(start as u32),
            doc_version: self.tabs[index].state.doc.version(),
            caret,
            rect: None,
            explicit,
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
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
        ]
    }

    fn frame(app: &mut IdeApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        let time = ctx.input(|input| input.time) + 0.1;
        let output = ctx.run_ui(
            egui::RawInput {
                events,
                time: Some(time),
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                app.dialogs(ui.ctx());
                egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
            },
        );
        output.drop_without_applying_deltas();
    }

    fn lookup(marked: &str) -> (tempfile::TempDir, IdeApp, egui::Context, String) {
        let caret = marked.find('⟂').unwrap();
        let source = marked.replacen('⟂', "", 1);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.py");
        std::fs::write(&path, &source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        settings.completion.show_as_you_type = false;
        settings.completion.insert_parentheses = true;
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(caret as u32))]);
        app.focus_editor = true;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);
        assert!(ctx.memory(|memory| memory.has_focus(app.tabs[index].view.id)));
        assert!(
            !app.completion.visible,
            "prose must not open an automatic lookup"
        );
        frame(&mut app, &ctx, key(egui::Key::Space, egui::Modifiers::CTRL));
        assert!(
            app.completion.visible,
            "actual Ctrl+Space opens prose lookup: source={source:?}, caret={caret}, items={:?}",
            app.completion.items
        );
        frame(&mut app, &ctx, vec![]);
        frame(&mut app, &ctx, vec![]);
        assert!(app.completion.visible, "lookup survives idle editor frames");
        if !source.contains("# type:") {
            assert!(app.import_completion.is_none());
            assert!(app.completion.imports.iter().all(Option::is_none));
        }
        assert_eq!(app.tabs[index].state.doc.text(), source);
        assert!(!app.tabs[index].state.doc.can_undo());
        (directory, app, ctx, source)
    }

    fn accept(app: &mut IdeApp, ctx: &egui::Context, label: &str, replace: bool) {
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|item| item.label == label)
            .unwrap_or_else(|| panic!("missing prose candidate {label}"));
        frame(
            app,
            ctx,
            key(
                if replace {
                    egui::Key::Tab
                } else {
                    egui::Key::Enter
                },
                egui::Modifiers::NONE,
            ),
        );
        assert!(!app.completion.visible);
    }

    #[test]
    fn actual_comment_lookup_inserts_names_and_keywords_without_callable_side_effects() {
        for (marked, label, expected) in [
            (
                "def π_target(): pass\n# π_t⟂\n",
                "π_target",
                "def π_target(): pass\n# π_target\n",
            ),
            ("# ret⟂\n", "return", "# return\n"),
        ] {
            let (directory, mut app, ctx, original) = lookup(marked);
            accept(&mut app, &ctx, label, false);
            assert_eq!(app.tabs[0].state.doc.text(), expected);
            assert!(app.param_info.is_none());
            assert!(app.tabs[0].state.undo());
            assert_eq!(app.tabs[0].state.doc.text(), original);
            assert!(app.tabs[0].state.redo());
            assert_eq!(app.tabs[0].state.doc.text(), expected);
            assert_eq!(
                std::fs::read_to_string(directory.path().join("fixture.py")).unwrap(),
                original
            );
        }
    }

    #[test]
    fn actual_docstring_lookup_completes_fields_and_only_own_parameters() {
        let marked = "def report(π_value, *items, **options):\n    \"\"\":pa⟂\"\"\"\n    pass\n";
        let (_directory, mut app, ctx, original) = lookup(marked);
        accept(&mut app, &ctx, "param", false);
        assert_eq!(
            app.tabs[0].state.doc.text(),
            original.replace(":pa", ":param")
        );
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), original);

        for name in ["π_value", "items", "options"] {
            let marked = "unrelated = 1\ndef outer(other):\n    def report(π_value, *items, **options):\n        \"\"\":param ⟂\"\"\"\n        pass\n";
            let (_directory, mut app, ctx, original) = lookup(marked);
            let names: Vec<_> = app
                .completion
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect();
            assert!(
                names.contains(&"π_value")
                    && names.contains(&"items")
                    && names.contains(&"options")
            );
            assert!(!names.contains(&"other") && !names.contains(&"unrelated"));
            accept(&mut app, &ctx, name, false);
            assert_eq!(
                app.tabs[0].state.doc.text(),
                original.replace(":param ", &format!(":param {name}"))
            );
            assert!(app.tabs[0].state.undo());
            assert_eq!(app.tabs[0].state.doc.text(), original);
        }
    }

    #[test]
    fn actual_docstring_names_remain_text_and_type_comments_preserve_suffixes() {
        for (marked, label, expected, replace) in [
            (
                "class Widget: pass\ndef use():\n    \"\"\"Accept a Wid⟂.\"\"\"\n",
                "Widget",
                "class Widget: pass\ndef use():\n    \"\"\"Accept a Widget.\"\"\"\n",
                false,
            ),
            (
                "value = [] # type: li⟂stBad\n",
                "list",
                "value = [] # type: list\n",
                true,
            ),
            (
                "from typing import Optional\nvalue = None # type: Opt⟂\n",
                "Optional",
                "from typing import Optional\nvalue = None # type: Optional\n",
                false,
            ),
        ] {
            let (_directory, mut app, ctx, original) = lookup(marked);
            accept(&mut app, &ctx, label, replace);
            assert_eq!(app.tabs[0].state.doc.text(), expected);
            assert!(app.tabs[0].state.undo());
            assert_eq!(app.tabs[0].state.doc.text(), original);
        }
    }

    #[test]
    fn explicit_prose_lookup_refreshes_after_typing_without_starting_autoimport() {
        let (_directory, mut app, ctx, original) = lookup("target = 1\n# targ⟂\n");
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::ModifiersChanged(egui::Modifiers::NONE),
                egui::Event::Text("e".into()),
            ],
        );
        assert!(app.completion.visible);
        assert!(app.completion.explicit);
        assert!(app.import_completion.is_none());
        accept(&mut app, &ctx, "target", false);
        assert_eq!(app.tabs[0].state.doc.text(), "target = 1\n# target\n");
        assert!(app.tabs[0].state.undo());
        assert_eq!(
            app.tabs[0].state.doc.text(),
            original.replace("# targ", "# targe")
        );
    }

    #[test]
    fn prose_acceptance_respects_readonly_and_ordinary_strings_keep_their_provider() {
        let (_directory, mut app, ctx, original) = lookup("target = 1\n# targ⟂\n");
        app.tabs[0].state.doc.set_read_only(true);
        accept(&mut app, &ctx, "target", false);
        assert_eq!(app.tabs[0].state.doc.text(), original);
        assert!(!app.tabs[0].state.doc.can_undo());

        let source = "target = 1\ntext = 'targ'\n";
        app.tabs[0].state.doc.set_read_only(false);
        let path = app.tabs[0].state.path.clone();
        app.tabs[0].state =
            crate::editor::EditorState::new(rustpy_text::Document::new(source), path);
        let caret = source.find("targ'").unwrap() + 4;
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(caret as u32))]);
        assert!(
            !app.open_prose_completion(true),
            "ordinary assigned string is not a docstring"
        );
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }

    #[test]
    fn first_bytes_expressions_are_not_docstrings_for_any_python_owner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.py");
        for (prefix, indent) in [
            ("", ""),
            ("class Owner:\n", "    "),
            ("def owner(value):\n", "    "),
        ] {
            for (literal, expected) in [
                ("b\"\"\"ret⟂\"\"\"", false),
                ("b\"\"\"part\"\"\" b\"\"\"ret⟂\"\"\"", false),
                ("\"\"\"part\"\"\" b\"\"\"ret⟂\"\"\"", false),
                ("b\"\"\"part\"\"\" \"\"\"ret⟂\"\"\"", false),
                ("f\"\"\"part\"\"\" \"\"\"ret⟂\"\"\"", false),
                ("\"\"\"ret⟂\"\"\"", true),
                ("r\"\"\"ret⟂\"\"\"", true),
                ("\"\"\"part\"\"\" u\"\"\"ret⟂\"\"\"", true),
            ] {
                let marked = format!("{prefix}{indent}{literal}\n");
                let caret = TextSize::new(marked.find('⟂').unwrap() as u32);
                let source = marked.replace('⟂', "");
                std::fs::write(&path, &source).unwrap();
                let mut app = IdeApp::new_welcome(Settings::default());
                app.open_file(&path);
                let index = app.active.unwrap();
                app.tabs[index]
                    .state
                    .set_carets(vec![crate::editor::Caret::at(caret)]);
                let root = app.tabs[index].state.syntax().syntax_node();
                assert_eq!(
                    docstring_owner(&root, caret).is_some(),
                    expected,
                    "{marked}"
                );
                let identity = app.tabs[index].state.doc.identity();
                let version = app.tabs[index].state.doc.version();
                assert_eq!(app.open_prose_completion(true), expected, "{marked}");
                if expected {
                    assert!(app
                        .completion
                        .items
                        .iter()
                        .any(|item| item.label == "return"));
                } else {
                    assert!(
                        !app.completion.visible,
                        "ordinary-string provider retains ownership"
                    );
                }
                assert_eq!(app.tabs[index].state.doc.identity(), identity);
                assert_eq!(app.tabs[index].state.doc.version(), version);
                assert_eq!(app.tabs[index].state.doc.text(), source);
                assert!(!app.tabs[index].state.doc.can_undo());
            }
        }
    }
}
