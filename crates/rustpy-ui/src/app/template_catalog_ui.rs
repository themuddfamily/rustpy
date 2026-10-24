//! Framework catalogue completion and navigation for template arguments:
//! `{% url %}` names, `{% load %}` libraries and their tags/filters, template
//! names (in templates and Python render calls) and Jinja `url_for` endpoints.
use super::*;
use rustpy_project::framework_catalog::Catalog;
use rustpy_semantic::framework_names::{self, Declaration};
use rustpy_semantic::framework_templates::Slot;

fn item(label: &str, detail: &str, kind: CompletionKind) -> CompletionItem {
    CompletionItem {
        label: label.into(),
        kind,
        detail: detail.into(),
        insert: label.into(),
        caret_offset: None,
        priority: 0,
    }
}

/// Catalogue tags (at a statement's tag position) or filters (after `|`)
/// available at `caret`: builtins plus what earlier `{% load %}` tags import.
pub(super) fn catalog_template_names(
    text: &str,
    caret: usize,
    catalog: &Catalog,
) -> Vec<(String, String)> {
    let Some(before) = text.get(..caret) else {
        return Vec::new();
    };
    let (open, statement) = match (before.rfind("{{"), before.rfind("{%")) {
        (Some(a), Some(b)) => (a.max(b), b > a),
        (Some(a), None) => (a, false),
        (None, Some(b)) => (b, true),
        _ => return Vec::new(),
    };
    let context = &before[open + 2..];
    if context.contains("%}") || context.contains("}}") {
        return Vec::new();
    }
    let word = context
        .rfind(|ch: char| !rustpy_core::is_ident_continue(ch))
        .map_or(context, |at| &context[at + 1..]);
    let head = context[..context.len() - word.len()].trim_end();
    let tags = statement && head.trim_start_matches(['-', '+']).trim().is_empty();
    let filters = head.ends_with('|');
    if !tags && !filters {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = if tags {
        catalog
            .tags
            .iter()
            .map(|tag| (tag.clone(), "template tag".into()))
            .collect()
    } else {
        catalog
            .filters
            .iter()
            .map(|filter| (filter.clone(), "template filter".into()))
            .collect()
    };
    for segment in before[..open].split("{%").skip(1).take(4096) {
        let Some(end) = segment.find("%}") else {
            continue;
        };
        let words: Vec<_> = segment[..end]
            .trim_start_matches(['-', '+'])
            .split_whitespace()
            .collect();
        if words.first() != Some(&"load") {
            continue;
        }
        // `{% load a b from lib %}` imports names; `{% load lib1 lib2 %}` libraries.
        let (libraries, only) = match words.iter().position(|word| *word == "from") {
            Some(at) => (
                words.get(at + 1..at + 2).unwrap_or(&[]),
                Some(&words[1..at]),
            ),
            None => (&words[1..], None),
        };
        for library in libraries {
            let Some((_, library_tags, library_filters)) = catalog
                .libraries
                .iter()
                .find(|(name, _, _)| name == library)
            else {
                continue;
            };
            let names = if tags { library_tags } else { library_filters };
            for name in names {
                if only.is_none_or(|only| only.contains(&name.as_str())) {
                    out.push((name.clone(), format!("{{% load {library} %}}")));
                }
            }
        }
    }
    out.retain(|(name, _)| name.starts_with(word));
    out
}

/// Completions for a name before `from` in `{% load names from library %}`:
/// that library's tags and filters, or nothing while the library is unknown.
pub(super) fn loaded_library_names(
    text: &str,
    caret: usize,
    catalog: &Catalog,
) -> Vec<(String, &'static str, CompletionKind)> {
    let Some(library) = rustpy_semantic::framework_templates::load_from_library(text, caret) else {
        return Vec::new();
    };
    let Some((_, tags, filters)) = catalog
        .libraries
        .iter()
        .find(|(name, _, _)| *name == library)
    else {
        return Vec::new();
    };
    tags.iter()
        .map(|tag| (tag.clone(), "template tag", CompletionKind::Keyword))
        .chain(
            filters
                .iter()
                .map(|filter| (filter.clone(), "template filter", CompletionKind::Function)),
        )
        .collect()
}

impl IdeApp {
    /// Project sources whose text mentions `marker`, loaded so route
    /// declarations in files no editor has opened are still found.
    fn framework_sources(&self, marker: &str) -> Vec<Arc<rustpy_semantic::FileInfo>> {
        let Some(project) = &self.project else {
            return Vec::new();
        };
        self.project_files()
            .into_iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "py"))
            .take(4096)
            .filter(|path| {
                project
                    .analysis
                    .current_text(path)
                    .is_some_and(|text| text.len() <= 1024 * 1024 && text.contains(marker))
            })
            .take(512)
            .filter_map(|path| project.analysis.file_for_path(&path))
            .collect()
    }
    fn django_url_declarations(&self) -> Vec<Declaration> {
        framework_names::django_urls_in(self.framework_sources("urlpatterns"))
    }
    fn flask_endpoint_declarations(&self) -> Vec<Declaration> {
        framework_names::flask_endpoints_in(self.framework_sources("flask"))
    }
    fn template_name_list(&self) -> Vec<String> {
        let Some(project) = &self.project else {
            return Vec::new();
        };
        let mut configured: Vec<_> = project.tree.template_dirs.iter().cloned().collect();
        configured.sort();
        rustpy_project::frameworks::template_names(&project.root, &configured)
    }

    /// Items for a `{% url %}`, `{% load %}`, template-name or `url_for`
    /// argument at `caret`; `None` when the caret is not in such a slot.
    pub(super) fn template_slot_items(
        &self,
        text: &str,
        caret: usize,
    ) -> Option<(usize, Vec<CompletionItem>)> {
        let (slot, start) = rustpy_semantic::framework_templates::slot(text, caret)?;
        let prefix = text.get(start..caret)?;
        let names: Vec<(String, &str, CompletionKind)> = match slot {
            Slot::UrlName => self
                .django_url_declarations()
                .into_iter()
                .map(|declaration| declaration.name)
                .chain(self.framework_catalog_snapshot().urls)
                .map(|name| (name, "Django URL name", CompletionKind::Constant))
                .collect(),
            Slot::Library => self
                .framework_catalog_snapshot()
                .libraries
                .into_iter()
                .map(|(name, _, _)| (name, "template library", CompletionKind::Module))
                .collect(),
            Slot::LoadedName => {
                loaded_library_names(text, caret, &self.framework_catalog_snapshot())
            }
            Slot::TemplateName => self
                .template_name_list()
                .into_iter()
                .map(|name| (name, "template", CompletionKind::Module))
                .collect(),
            Slot::Endpoint => self
                .flask_endpoint_declarations()
                .into_iter()
                .map(|declaration| declaration.name)
                .chain(["static".to_string()])
                .map(|name| (name, "Flask endpoint", CompletionKind::Function))
                .collect(),
        };
        let mut items: Vec<CompletionItem> = Vec::new();
        for (name, detail, kind) in names {
            if name.starts_with(prefix) && !items.iter().any(|item| item.label == name) {
                items.push(item(&name, detail, kind));
            }
        }
        items.sort_by(|a, b| a.label.cmp(&b.label));
        Some((start, items))
    }

    /// Adds catalogue tags/filters (builtins and loaded libraries) to the
    /// hard-coded template vocabulary.
    pub(super) fn merge_catalog_template_items(
        &self,
        text: &str,
        caret: usize,
        items: &mut Vec<CompletionItem>,
    ) {
        let catalog = self.framework_catalog_snapshot();
        for (name, detail) in catalog_template_names(text, caret, &catalog) {
            if !items.iter().any(|item| item.label == name) {
                items.push(item(&name, &detail, CompletionKind::Keyword));
            }
        }
    }

    /// Template names inside `render(request, '…')`-style Python calls.
    pub(super) fn python_template_name_completion(
        &mut self,
        file: &rustpy_semantic::FileInfo,
        caret: TextSize,
        explicit: bool,
    ) -> bool {
        let Some(start) = framework_names::template_argument(file, caret) else {
            return false;
        };
        self.python_template_names_at(start, caret, explicit)
    }

    /// The template-name lookup for the string argument starting at `start`.
    pub(super) fn python_template_names_at(
        &mut self,
        start: TextSize,
        caret: TextSize,
        explicit: bool,
    ) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let text = self.tabs[index].state.doc.text();
        let Some(prefix) = text.get(usize::from(start)..usize::from(caret)) else {
            return false;
        };
        let mut items: Vec<_> = self
            .template_name_list()
            .into_iter()
            .filter(|name| name.starts_with(prefix))
            .map(|name| item(&name, "template", CompletionKind::Module))
            .collect();
        items.sort_by(|a, b| a.label.cmp(&b.label));
        self.completion = CompletionPopup {
            visible: !items.is_empty(),
            imports: vec![None; items.len()],
            items,
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: start,
            doc_version: self.tabs[index].state.doc.version(),
            caret,
            explicit,
            rect: None,
        };
        self.import_completion = None;
        self.completion_ui.due = None;
        self.completion_ui.paths = true;
        self.refresh_completion_order();
        true
    }

    /// Go to the `path(name=…)` declaration of a `{% url %}` name or the view
    /// of a Jinja `url_for` endpoint at `offset`.
    pub(super) fn template_url_declaration(&mut self, offset: TextSize) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        if !matches!(
            self.tabs[index].state.language,
            Language::Template | Language::Html
        ) {
            return false;
        }
        let text = self.tabs[index].state.doc.text();
        let Some(link) = rustpy_semantic::framework_templates::url_links(&text)
            .into_iter()
            .find(|link| link.range.contains_inclusive(offset))
        else {
            return false;
        };
        let declarations = if link.kind == "url" {
            self.django_url_declarations()
        } else {
            self.flask_endpoint_declarations()
        };
        match framework_names::find(&declarations, &link.name) {
            Some(location) => self.navigate_locations(vec![location], "No URL declaration"),
            None => self.set_status(format!("No declaration found for '{}'", link.name)),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog {
            tags: vec!["block".into(), "load".into(), "url".into()],
            filters: vec!["lower".into(), "length".into()],
            libraries: vec![
                (
                    "humanize".into(),
                    vec![],
                    vec!["intcomma".into(), "naturaltime".into()],
                ),
                (
                    "custom".into(),
                    vec!["show_menu".into(), "show_tree".into()],
                    vec!["shout".into()],
                ),
            ],
            ..Default::default()
        }
    }

    fn names(text: &str) -> Vec<String> {
        let mut names: Vec<_> = catalog_template_names(text, text.len(), &catalog())
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn catalogue_tags_and_filters_follow_load_scope() {
        assert_eq!(names("{% sh"), Vec::<String>::new());
        assert_eq!(names("{% load custom %}{% sh"), ["show_menu", "show_tree"]);
        assert_eq!(
            names("{% load show_tree from custom %}{% sh"),
            ["show_tree"]
        );
        assert_eq!(
            names("{% load humanize custom %}{{ n|"),
            ["intcomma", "length", "lower", "naturaltime", "shout"]
        );
        assert_eq!(names("{% load humanize %}{{ n|na"), ["naturaltime"]);
        // Arguments of a tag are neither tag names nor filters.
        assert!(names("{% load custom %}{% if sh").is_empty());
    }

    #[test]
    fn load_names_before_from_offer_the_library_tags_and_filters() {
        let names = |marked: &str| {
            let caret = marked.find('|').unwrap();
            let text = marked.replace('|', "");
            let (slot, _) = rustpy_semantic::framework_templates::slot(&text, caret).unwrap();
            let mut names: Vec<_> = match slot {
                Slot::LoadedName => loaded_library_names(&text, caret, &catalog())
                    .into_iter()
                    .map(|(name, _, _)| name)
                    .collect(),
                Slot::Library => catalog()
                    .libraries
                    .into_iter()
                    .map(|(name, _, _)| name)
                    .collect(),
                other => panic!("{marked}: {other:?}"),
            };
            names.sort();
            names
        };
        assert_eq!(
            names("{% load show_menu | from custom %}"),
            ["shout", "show_menu", "show_tree"]
        );
        assert_eq!(
            names("{% load | from humanize %}"),
            ["intcomma", "naturaltime"]
        );
        // Unknown or not yet typed library: no names, and never library names.
        assert!(names("{% load | from missing %}").is_empty());
        assert!(names("{% load a| from %}").is_empty());
        // The library word and a `{% load %}` without `from` offer libraries.
        assert_eq!(names("{% load a from | %}"), ["custom", "humanize"]);
        assert_eq!(names("{% load humanize | %}"), ["custom", "humanize"]);
    }

    #[test]
    fn slot_items_offer_template_names_libraries_and_urls() {
        let dir = tempfile::TempDir::with_prefix("rustpy-template-catalog").unwrap();
        let root = dir.path();
        for (file, text) in [
            ("templates/base.html", ""),
            ("blog/templates/blog/post.html", ""),
            (
                "blog/urls.py",
                "from django.urls import path\napp_name = 'blog'\nurlpatterns = [path('', v, name='index')]\n",
            ),
            (
                "app.py",
                "from flask import Flask\napp = Flask(__name__)\n@app.route('/')\ndef home(): pass\n",
            ),
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let mut app = IdeApp::new(Settings::default(), None);
        app.project = Some(Project {
            root: root.to_path_buf(),
            tree: ProjectTree::new(root.to_path_buf()),
            analysis: Arc::new(Analysis::for_project(root)),
            run_configs: RunConfigStore::load(root),
            interpreters: Vec::new(),
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app.load_project_structure(root);
        app.rebuild_file_index();
        assert!(app
            .project_index
            .files
            .wait_idle(std::time::Duration::from_secs(20)));
        let labels = |text: &str| {
            let (start, items) = app.template_slot_items(text, text.len()).unwrap();
            assert!(start <= text.len());
            items
                .into_iter()
                .map(|item| item.label.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(labels("{% extends '"), ["base.html", "blog/post.html"]);
        assert_eq!(labels("{% include 'bl"), ["blog/post.html"]);
        assert_eq!(labels("{% url 'blog:"), ["blog:index"]);
        assert_eq!(labels("{{ url_for('h"), ["home"]);
        assert!(app.template_slot_items("{{ title", 8).is_none());

        // Ctrl+B on the URL name reaches the `name=` declaration.
        let template = root.join("templates/page.html");
        std::fs::write(&template, "{% url 'blog:index' %}").unwrap();
        app.open_file(&template);
        assert!(app.template_url_declaration(TextSize::new(10)));
        let active = app.active.unwrap();
        assert_eq!(
            app.tabs[active].state.path.as_deref(),
            Some(root.join("blog/urls.py").as_path())
        );
    }
}
