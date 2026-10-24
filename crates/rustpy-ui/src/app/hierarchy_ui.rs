//! Captured hierarchy queries and class-diagram production.
use super::*;
use crate::diagram_panel::DiagramClass;

impl IdeApp {
    fn sync_hierarchy_buffers(&self) {
        for (index, tab) in self.tabs.iter().enumerate() {
            if tab.state.language == Language::Python {
                let _ = self.analysis_file(index);
            }
        }
    }

    pub(super) fn show_hierarchy(&mut self, kind: HierarchyKind) {
        let Some(index) = self.active else { return };
        self.sync_analysis_now();
        let Some((_, file)) = self.analysis_file(index) else {
            return;
        };
        let caret = self.tabs[index].state.primary().head();
        self.show_hierarchy_at(kind, file.path.clone(), TextRange::empty(caret));
    }

    pub(super) fn refresh_hierarchy(&mut self, kind: HierarchyKind) {
        if let Some((path, range)) = self.hierarchy_panel.origin.clone() {
            self.show_hierarchy_at(kind, path, range);
        }
    }

    pub(super) fn show_hierarchy_at(
        &mut self,
        kind: HierarchyKind,
        path: PathBuf,
        range: TextRange,
    ) {
        self.sync_hierarchy_buffers();
        let Some(analysis) = self
            .project
            .as_ref()
            .map(|project| project.analysis.clone())
        else {
            return;
        };
        let Some(file) = analysis.file_for_path(&path) else {
            return;
        };
        let caret = range.start();
        let name = file
            .text
            .get(usize::from(caret)..)
            .unwrap_or("")
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next()
            .unwrap_or("")
            .to_owned();
        let mut files = self.project_files();
        use crate::hierarchy_panel::HierarchyScope;
        files.retain(|candidate| match self.hierarchy_panel.scope {
            HierarchyScope::Project => true,
            HierarchyScope::Directory => candidate.parent() == path.parent(),
            HierarchyScope::File => candidate == &path,
        });
        let mut roots: Vec<hierarchy::HierarchyNode> = match kind {
            HierarchyKind::Both => {
                let mut roots = Vec::new();
                if let Some(mut node) = hierarchy::supertypes(&analysis, &file, caret) {
                    node.detail = "Supertypes".into();
                    roots.push(node);
                }
                if let Some(mut node) = hierarchy::subtypes(&analysis, &file, caret, &files) {
                    node.detail = "Subtypes".into();
                    roots.push(node);
                }
                roots
            }
            HierarchyKind::Supertypes => hierarchy::supertypes(&analysis, &file, caret)
                .into_iter()
                .collect(),
            HierarchyKind::Subtypes => hierarchy::subtypes(&analysis, &file, caret, &files)
                .into_iter()
                .collect(),
            HierarchyKind::Method => hierarchy::method_hierarchy(&analysis, &file, caret, &files)
                .into_iter()
                .collect(),
            HierarchyKind::Callers | HierarchyKind::Callees => {
                let Some(mut root) = hierarchy::callable_root(&analysis, &file, caret) else {
                    self.set_status("Place the caret on a function or method");
                    return;
                };
                let Some(owner) = analysis.file_for_path(&root.path) else {
                    return;
                };
                root.children = if kind == HierarchyKind::Callers {
                    rustpy_search::usages::callers(&analysis, &owner, root.range.start(), &files)
                } else {
                    hierarchy::callees(&analysis, &owner, root.range.start())
                };
                vec![root]
            }
        };
        fn filter(nodes: &mut Vec<hierarchy::HierarchyNode>, path: &Path, scope: HierarchyScope) {
            nodes.retain(|node| match scope {
                HierarchyScope::Project => true,
                HierarchyScope::Directory => node.path.parent() == path.parent(),
                HierarchyScope::File => node.path == path,
            });
            for node in nodes {
                filter(&mut node.children, path, scope);
            }
        }
        // Keep the base visible; the selected scope controls all related nodes.
        for node in &mut roots {
            filter(&mut node.children, &path, self.hierarchy_panel.scope);
        }
        if roots.is_empty() {
            self.set_status("Hierarchy: nothing found at this symbol");
            return;
        }
        self.hierarchy_panel
            .set(kind, format!("{} of {name}", kind.title()), roots);
        self.hierarchy_panel.origin = Some((path, range));
        if let Some(root) = self.hierarchy_panel.roots.first() {
            self.hierarchy_panel
                .expanded_calls
                .insert((root.path.clone(), root.range));
        }
        self.bottom = Some(BottomTool::Hierarchy);
    }

    pub(super) fn expand_call_node(&mut self, path: &Path, range: TextRange, kind: HierarchyKind) {
        self.sync_hierarchy_buffers();
        let Some(analysis) = self.project.as_ref().map(|p| p.analysis.clone()) else {
            return;
        };
        let file = match analysis.file_for_path(path) {
            Some(f) => f,
            None => {
                let Ok(text) = std::fs::read_to_string(path) else {
                    return;
                };
                analysis.set_file_text(path, &text);
                let Some(f) = analysis.file_for_path(path) else {
                    return;
                };
                f
            }
        };
        let files = self.project_files();
        let mut children = match kind {
            HierarchyKind::Callers => {
                rustpy_search::usages::callers(&analysis, &file, range.start(), &files)
            }
            HierarchyKind::Callees => hierarchy::callees(&analysis, &file, range.start()),
            _ => Vec::new(),
        };
        if let Some((origin, _)) = &self.hierarchy_panel.origin {
            use crate::hierarchy_panel::HierarchyScope;
            children.retain(|node| match self.hierarchy_panel.scope {
                HierarchyScope::Project => true,
                HierarchyScope::Directory => node.path.parent() == origin.parent(),
                HierarchyScope::File => &node.path == origin,
            });
        }
        self.hierarchy_panel
            .set_children(&path.to_path_buf(), range, children);
    }

    /// Navigate → Show Diagram: UML class diagram of the active file (plus direct bases from other files).
    pub(super) fn show_diagram(&mut self) {
        let Some(i) = self.active else { return };
        self.sync_hierarchy_buffers();
        let Some((analysis, file)) = self.analysis_file(i) else {
            self.set_status("Show Diagram works for Python files in a project");
            return;
        };
        use rustpy_syntax::SyntaxKind;
        let inf = rustpy_semantic::infer::Inferer::new(&analysis);
        let root = file.parse.syntax_node();
        let mut classes: Vec<DiagramClass> = Vec::new();
        let mut queue: Vec<rustpy_semantic::types::ClassRef> = Vec::new();
        for node in root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::CLASS_DEF)
        {
            if let Some(def) = rustpy_syntax::ast::ClassDef::cast(node.clone()) {
                queue.push(rustpy_semantic::types::ClassRef {
                    file: file.id,
                    range: node.text_range(),
                    name: def.name_text().into(),
                });
            }
        }
        let class_key = |class: &rustpy_semantic::types::ClassRef| {
            format!("{:?}:{}", class.file, usize::from(class.range.start()))
        };
        let mut dependencies = Vec::new();
        let mut implementations = Vec::new();
        let mut seen: Vec<rustpy_semantic::types::ClassRef> = Vec::new();
        let mut truncated = false;
        while let Some(c) = queue.pop() {
            if seen.contains(&c) {
                continue;
            }
            if seen.len() >= 60 {
                truncated = true;
                continue;
            }
            seen.push(c.clone());
            let Some(info) = inf.class_info(&c) else {
                continue;
            };
            let Some(cfile) = analysis.file(c.file) else {
                continue;
            };
            let mut bases = Vec::new();
            for range in &info.base_exprs {
                let root = cfile.parse.syntax_node();
                let Some(expression) = root
                    .descendants()
                    .filter_map(rustpy_syntax::ast::Expr::cast)
                    .find(|expression| expression.syntax().text_range() == *range)
                else {
                    continue;
                };
                let rustpy_semantic::types::Type::Class(base, _) =
                    inf.type_of_expr(&cfile, &expression)
                else {
                    continue;
                };
                if base.name.as_str() == "object" {
                    continue;
                }
                bases.push(class_key(&base));
                // Protocol is a special-form alias in maintained typing stubs,
                // not necessarily an MRO class literally named "Protocol".
                // Share inference's canonical import/provider classification.
                if inf.canonical_protocol(&base) {
                    implementations.push((class_key(&c), class_key(&base)));
                }
                if analysis
                    .file(base.file)
                    .is_some_and(|file| !file.path.starts_with("<typeshed>"))
                {
                    queue.push(base);
                }
            }
            // Resolve referenced classes through the semantic model; aliases and imported
            // constructors retain the actual defining class rather than a spelling guess.
            if let Some(node) = cfile
                .parse
                .syntax_node()
                .descendants()
                .find(|node| node.text_range() == c.range)
            {
                for expression in node
                    .descendants()
                    .filter_map(rustpy_syntax::ast::Expr::cast)
                    .take(4097)
                    .enumerate()
                {
                    let (index, expression) = expression;
                    if index == 4096 {
                        truncated = true;
                        break;
                    }
                    if !matches!(
                        expression,
                        rustpy_syntax::ast::Expr::Name(_) | rustpy_syntax::ast::Expr::Attribute(_)
                    ) {
                        continue;
                    }
                    let target = match inf.type_of_expr(&cfile, &expression) {
                        rustpy_semantic::types::Type::Class(target, _) => Some(target),
                        _ => None,
                    };
                    if let Some(target) = target {
                        if target != c
                            && !bases.contains(&class_key(&target))
                            && !analysis
                                .file(target.file)
                                .is_some_and(|file| file.path.starts_with("<typeshed>"))
                        {
                            let edge = (class_key(&c), class_key(&target));
                            if !dependencies.contains(&edge) {
                                dependencies.push(edge);
                            }
                            queue.push(target);
                        }
                    }
                }
            }
            let mut fields = Vec::new();
            let mut methods = Vec::new();
            for (name, m) in info.members.iter() {
                match m.kind {
                    rustpy_semantic::classes::MemberKind::Method
                    | rustpy_semantic::classes::MemberKind::StaticMethod
                    | rustpy_semantic::classes::MemberKind::ClassMethod => {
                        methods.push(name.to_string())
                    }
                    rustpy_semantic::classes::MemberKind::NestedClass => {}
                    _ => fields.push(name.to_string()),
                }
            }
            let name_range = {
                let r = cfile.parse.syntax_node();
                r.covering_element(c.range)
                    .into_node()
                    .and_then(|n| {
                        n.children_with_tokens()
                            .filter_map(|e| e.into_token())
                            .find(|t| t.kind() == SyntaxKind::IDENT)
                            .map(|t| t.text_range())
                    })
                    .unwrap_or(c.range)
            };
            classes.push(DiagramClass {
                id: class_key(&c),
                name: c.name.to_string(),
                path: cfile.path.clone(),
                range: name_range,
                bases,
                fields,
                methods,
            });
        }
        if classes.is_empty() {
            self.set_status("No classes in this file");
            return;
        }
        let title = format!(
            "Classes in {}{}",
            self.tabs[i].state.title(),
            if truncated {
                " (bounded preview: first 60 classes / 4096 references per class)"
            } else {
                ""
            }
        );
        if let Some(project) = &self.project {
            self.diagram_panel.project_context(&project.root);
        }
        self.diagram_panel.set(title, classes);
        self.diagram_panel.capture_sources(&analysis);
        self.diagram_panel.dependencies = dependencies;
        self.diagram_panel.implementations = implementations;
        self.bottom = Some(BottomTool::Diagram);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;

    #[test]
    fn hierarchy_switch_uses_captured_base_after_editor_navigation_and_scopes_children() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("base.py");
        let source = "class Base: pass\nclass Local(Base): pass\nclass Unrelated: pass\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![Caret::at(TextSize::new(
                source.find("Base").unwrap() as u32
            ))]);
        app.show_hierarchy(HierarchyKind::Subtypes);
        assert_eq!(app.hierarchy_panel.roots[0].name, "Base");
        assert_eq!(app.hierarchy_panel.roots[0].children[0].name, "Local");
        app.tabs[index]
            .state
            .set_carets(vec![Caret::at(TextSize::new(
                source.find("Unrelated").unwrap() as u32,
            ))]);
        app.refresh_hierarchy(HierarchyKind::Both);
        assert!(app
            .hierarchy_panel
            .roots
            .iter()
            .all(|node| node.name == "Base"));
        assert_eq!(app.hierarchy_panel.roots.len(), 2);
        app.hierarchy_panel.scope = crate::hierarchy_panel::HierarchyScope::File;
        app.refresh_hierarchy(HierarchyKind::Subtypes);
        assert!(app.hierarchy_panel.roots[0]
            .children
            .iter()
            .all(|node| node.path == path));
        app.show_hierarchy_at(
            HierarchyKind::Supertypes,
            path.clone(),
            TextRange::empty(TextSize::new(source.find("Local").unwrap() as u32)),
        );
        assert_eq!(app.hierarchy_panel.roots[0].name, "Local");
        assert_eq!(app.hierarchy_panel.roots[0].children[0].name, "Base");
    }

    #[test]
    fn actual_diagram_producer_resolves_dependency_and_protocol_base_aliases() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("diagram.py");
        let source="from typing import Protocol as Interface\nclass Contract(Interface):\n    def run(self): ...\nclass Payload: pass\nclass Worker(Contract):\n    def run(self):\n        return Payload()\nclass Protocol: pass\nclass LocalContract(Protocol): pass\nclass LocalWorker(LocalContract): pass\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        app.open_file(&path);
        app.show_diagram();
        let panel = &app.diagram_panel;
        let id = |name: &str| {
            panel
                .classes
                .iter()
                .find(|class| class.name == name)
                .unwrap()
                .id
                .clone()
        };
        assert!(panel.dependencies.contains(&(id("Worker"), id("Payload"))));
        assert!(panel
            .implementations
            .contains(&(id("Worker"), id("Contract"))));
        assert!(!panel
            .implementations
            .contains(&(id("LocalWorker"), id("LocalContract"))));
        assert!(!panel
            .implementations
            .contains(&(id("LocalContract"), id("Protocol"))));
        assert!(panel.svg_image().contains("Worker"));
    }
}
