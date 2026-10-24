//! Editor breadcrumbs share the current syntax tree with Context Info.
use super::*;
use rustpy_syntax::{ast, SyntaxKind as K, SyntaxNode};

fn definition(node: SyntaxNode) -> Option<(String, TextRange)> {
    match node.kind() {
        K::FUNC_DEF => ast::FuncDef::cast(node)
            .map(|function| (format!("{}()", function.name_text()), function.text_range())),
        K::CLASS_DEF => {
            ast::ClassDef::cast(node).map(|class| (class.name_text(), class.text_range()))
        }
        _ => None,
    }
}

/// Siblings have the same lexical parent; a same-named method in another
/// class is never a sibling merely because its outline depth is equal.
fn siblings(root: &SyntaxNode, range: TextRange) -> Vec<(String, TextRange)> {
    root.descendants()
        .find(|node| {
            node.text_range() == range && matches!(node.kind(), K::FUNC_DEF | K::CLASS_DEF)
        })
        .and_then(|node| node.parent())
        .map(|parent| parent.children().filter_map(definition).collect())
        .unwrap_or_default()
}

impl IdeApp {
    pub(super) fn editor_breadcrumbs(&mut self, ui: &mut egui::Ui, index: usize) {
        if !self.settings.editor.show_breadcrumbs || index >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[index];
        if self
            .settings
            .editor
            .preferences
            .breadcrumbs_disabled
            .iter()
            .any(|name| {
                name.eq_ignore_ascii_case(&crate::editor::preferences::language_key(&tab.state))
            })
        {
            return;
        }
        let title = tab.state.title();
        let crumbs = if tab.state.language == Language::Python {
            tab.state.breadcrumbs()
        } else {
            Vec::new()
        };
        let groups: Vec<_> = if crumbs.is_empty() {
            Vec::new()
        } else {
            // The tree the crumbs came from (a large file's may lag a keystroke).
            let root = tab.state.frame_syntax().syntax_node();
            crumbs
                .iter()
                .map(|(_, range)| siblings(&root, *range))
                .collect()
        };
        let view = tab.view.id;
        let theme = self.theme.clone();
        let panel = if self.settings.editor.breadcrumbs_at_top {
            egui::Panel::top(view.with("breadcrumbs-top"))
        } else {
            egui::Panel::bottom(view.with("breadcrumbs-bottom"))
        };
        let mut jump = None;
        panel
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(theme.panel_bg)
                    .inner_margin(egui::Margin::symmetric(8, 2)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt(view.with("breadcrumbs-scroll"))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            self.editor_file_breadcrumb(ui, index, &title);
                            for ((name, range), peers) in crumbs.iter().zip(&groups) {
                                ui.push_id(range.start(), |ui| {
                                    ui.label(
                                        egui::RichText::new("›").color(theme.text_dim).size(12.0),
                                    );
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new(name)
                                                    .color(theme.text)
                                                    .size(12.0),
                                            )
                                            .frame(false),
                                        )
                                        .clicked()
                                    {
                                        jump = Some(*range);
                                    }
                                    ui.menu_button("⌄", |ui| {
                                        ui.set_min_width(170.0);
                                        ui.label("Sibling definitions");
                                        ui.separator();
                                        egui::ScrollArea::vertical().max_height(320.0).show(
                                            ui,
                                            |ui| {
                                                for (peer, target) in peers {
                                                    if ui
                                                        .selectable_label(target == range, peer)
                                                        .clicked()
                                                    {
                                                        jump = Some(*target);
                                                        ui.close();
                                                    }
                                                }
                                            },
                                        );
                                    })
                                    .response
                                    .on_hover_text(format!("Choose a sibling of {name}"));
                                });
                            }
                        });
                    });
            });
        if let Some(range) = jump {
            self.jump_breadcrumb(index, range);
        }
    }

    fn jump_breadcrumb(&mut self, index: usize, range: TextRange) {
        if index >= self.tabs.len() {
            return;
        }
        self.mark_nav_jump();
        self.active = Some(index);
        self.tabs[index].state.reveal_offset(range.start());
        self.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(range.start())]);
        self.focus_editor = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_choices_follow_lexical_parent_and_preserve_duplicate_names() {
        let source = "class A:\n    def first(self):\n        pass\n    async def second(self):\n        pass\nclass B:\n    def first(self):\n        pass\ndef outer():\n    def inner():\n        pass\n    def other():\n        pass\n";
        let parse = rustpy_syntax::parse(source);
        let root = parse.syntax_node();
        let first = root
            .descendants()
            .filter_map(definition)
            .find(|(name, _)| name == "first()")
            .unwrap();
        assert_eq!(
            siblings(&root, first.1)
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["first()", "second()"]
        );
        let inner = root
            .descendants()
            .filter_map(definition)
            .find(|(name, _)| name == "inner()")
            .unwrap();
        assert_eq!(
            siblings(&root, inner.1)
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["inner()", "other()"]
        );
        let class = root
            .descendants()
            .filter_map(definition)
            .find(|(name, _)| name == "A")
            .unwrap();
        assert_eq!(
            siblings(&root, class.1)
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "outer()"]
        );
    }

    #[test]
    fn top_position_round_trips_and_sibling_navigation_keeps_back_location() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.py");
        let source = "class A:\n    def first(self):\n        return 1\n    def second(self):\n        return 2\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.editor.breadcrumbs_at_top = true;
        settings.trust_project(directory.path());
        let loaded: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(loaded.editor.breadcrumbs_at_top);
        let mut app = IdeApp::new(loaded, Some(directory.path().to_path_buf()));
        app.open_file(&path);
        let initial = TextSize::new(source.find("return 1").unwrap() as u32);
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(initial)]);
        let root = app.tabs[0].state.syntax().syntax_node();
        let target = root
            .descendants()
            .filter_map(definition)
            .find(|(name, _)| name == "second()")
            .unwrap()
            .1;
        app.jump_breadcrumb(0, target);
        assert_eq!(app.tabs[0].state.primary().head(), target.start());
        let context = egui::Context::default();
        app.handle_action(Action::NavigateBack, &context);
        assert_eq!(app.tabs[0].state.primary().head(), initial);
        app.handle_action(Action::ContextInfo, &context);
        assert!(app.context_info.as_ref().unwrap().0.contains("first()"));
    }
}
