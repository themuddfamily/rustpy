//! Code insight glue: the module resolver for a project and interpreter, and
//! keeping module lookups in step with packages installed into a virtual
//! environment inside the project (roadmap §2.2 "Module index").

use super::*;
use rustpy_semantic::module::ModuleResolver;

/// The resolver for the project `roots` (content root first, then source
/// roots) and the selected interpreter. Without an interpreter, source roots
/// still apply (Python 3.12 on the host platform is assumed).
pub(super) fn resolver_for(
    roots: Vec<PathBuf>,
    interpreter: Option<&Interpreter>,
) -> ModuleResolver {
    match interpreter {
        Some(i) => {
            ModuleResolver::new(roots, i.search_paths(), i.major_minor()).with_platform(&i.platform)
        }
        None => ModuleResolver::new(roots, Vec::new(), (3, 12)),
    }
}

/// The dotted module name "Copy Reference" uses for `path`: the importable
/// name (relative to the deepest source root or interpreter path, as for
/// imports), else the path below the project `root`.
pub(super) fn reference_module(
    resolver: &ModuleResolver,
    root: &Path,
    path: &Path,
) -> Option<String> {
    if let Some(module) = resolver.module_name_of(path) {
        return Some(module);
    }
    let rel = path.strip_prefix(root).ok()?;
    let module = rel
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(".");
    Some(module.trim_end_matches(".__init__").to_string())
}

/// Whether a file-system event for `path` adds or removes an importable
/// top-level entry of a `site-packages` / `dist-packages` directory (a
/// package being installed or uninstalled), e.g. `.venv/lib/python3.12/
/// site-packages/requests` or `…/site-packages/ujson.cpython-312-x86_64-linux-gnu.so`.
pub(super) fn is_installed_package_entry(path: &Path) -> bool {
    let in_site = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "site-packages" || n == "dist-packages");
    if !in_site {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    !name.ends_with(".dist-info")
        && !name.ends_with(".egg-info")
        && name != "__pycache__"
        && !name.ends_with(".pth")
        && !name.starts_with('~')
}

/// Whether `path` is a file the module index cares about: Python sources,
/// stubs and compiled extension modules.
pub(super) fn is_module_file(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some("py" | "pyi") => true,
        Some("so" | "pyd") => path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(rustpy_semantic::module::extension_module_name)
            .is_some(),
        _ => false,
    }
}

/// Where the "Rename element" quick fix (names shadowing an outer scope)
/// starts Rename: its problem under the caret, else the first on the caret's
/// line (Alt+Enter offers the fixes of both).
pub(super) fn rename_fix_target(
    quick_fixes: &[(TextRange, Vec<QuickFix>)],
    caret: TextSize,
    line_of: impl Fn(TextSize) -> usize,
) -> Option<TextSize> {
    let line = line_of(caret);
    let mut on_line = None;
    for (range, fixes) in quick_fixes {
        if !fixes
            .iter()
            .any(|f| f.title == inspections::RENAME_ELEMENT_FIX)
        {
            continue;
        }
        if range.contains_inclusive(caret) {
            return Some(caret);
        }
        if on_line.is_none() && line_of(range.start()) == line {
            on_line = Some(range.start());
        }
    }
    on_line
}

/// Documentation from the interpreter and on the web (roadmap §2.10): the
/// interpreter's docstrings for builtins and compiled modules (dumped once per
/// interpreter version in the background, cached in the configuration
/// directory) and the External Documentation link of Quick Documentation.
#[derive(Default)]
pub(super) struct DocsState {
    /// Interpreter (path, version) whose docstrings are loaded or loading.
    for_interpreter: Option<(String, bool)>,
    docs: Option<Arc<rustpy_semantic::runtime_docs::RuntimeDocs>>,
    pending: Option<crossbeam_channel::Receiver<rustpy_semantic::runtime_docs::RuntimeDocs>>,
    /// External Documentation URL of the symbol shown by Quick Documentation.
    pub(super) quick_doc_url: Option<String>,
}

impl IdeApp {
    /// Loads (or dumps) the docstrings of a newly selected interpreter in the
    /// background, and gives them to every new analysis database.
    pub(super) fn poll_runtime_docs(&mut self) {
        let project_safe_mode = self.project_safe_mode();
        let Some(p) = &self.project else {
            self.docs = DocsState::default();
            return;
        };
        if p.interpreter.is_none() {
            self.docs.pending = None;
            self.docs.docs = None;
            self.docs.for_interpreter = None;
            if !p.analysis.runtime_docs().is_empty() {
                p.analysis.set_runtime_docs(Arc::new(Default::default()));
            }
            return;
        }
        if let Some(i) = &p.interpreter {
            let allowed = rustpy_run::docs::supports(i) && !project_safe_mode;
            let identity = rustpy_run::docs::cache_file_name(i);
            let known = self
                .docs
                .for_interpreter
                .as_ref()
                .is_some_and(|(key, permitted)| *key == identity && *permitted == allowed);
            if !known {
                self.docs.for_interpreter = Some((identity, allowed));
                self.docs.pending = None;
                self.docs.docs = None;
                p.analysis.set_runtime_docs(Arc::new(Default::default()));
                if allowed {
                    let (tx, rx) = crossbeam_channel::bounded(1);
                    let interpreter = i.clone();
                    let cache = Settings::config_dir().join("runtime_docs");
                    std::thread::spawn(move || {
                        if let Some(text) = rustpy_run::docs::load_or_dump(&interpreter, &cache) {
                            let docs = rustpy_semantic::runtime_docs::RuntimeDocs::from_json(&text);
                            log::info!(
                                "interpreter docstrings: {} entries for Python {}",
                                docs.len(),
                                interpreter.version
                            );
                            let _ = tx.send(docs);
                        }
                    });
                    self.docs.pending = Some(rx);
                }
            }
        }
        if let Some(rx) = &self.docs.pending {
            match rx.try_recv() {
                Ok(docs) => {
                    self.docs.docs = Some(Arc::new(docs));
                    self.docs.pending = None;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.docs.pending = None,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        }
        if let (Some(docs), Some(p)) = (&self.docs.docs, &self.project) {
            if !Arc::ptr_eq(&p.analysis.runtime_docs(), docs) {
                p.analysis.set_runtime_docs(docs.clone());
            }
        }
    }

    /// The External Documentation page of the symbol at the caret.
    pub(super) fn external_doc_url_at_caret(&self) -> Option<String> {
        let i = self.active?;
        let (analysis, file) = self.analysis_file(i)?;
        let offset = self.tabs[i].state.primary().head();
        let python = self
            .project
            .as_ref()
            .and_then(|p| p.interpreter.as_ref())
            .map(|i| i.major_minor())
            .unwrap_or((3, 12));
        let patterns: Vec<(String, String)> = self
            .settings
            .external_doc_urls
            .iter()
            .map(|u| (u.module.clone(), u.url.clone()))
            .collect();
        rustpy_semantic::external_docs::external_doc_url(
            &analysis, &file, offset, python, &patterns,
        )
    }

    /// External Documentation (Shift+F1): the symbol's page in the browser.
    pub(super) fn external_documentation(&mut self) {
        match self.external_doc_url_at_caret() {
            Some(url) => self.open_url(&url),
            None => self.set_status("No external documentation for the symbol at the caret"),
        }
    }

    /// Shared configured browser routing for documentation and source hyperlinks.
    pub(super) fn open_url(&mut self, url: &str) {
        self.open_web_url(url, None, false);
    }

    /// Settings › Tools › Python External Documentation: URL patterns per module.
    pub(super) fn page_external_documentation(&mut self, ui: &mut egui::Ui) {
        let dim = self.theme.text_dim;
        ui.heading("Python External Documentation");
        ui.label(egui::RichText::new("External Documentation (Shift+F1, and the link in Quick Documentation) opens the standard library on docs.python.org for the interpreter's version. Explicit URL patterns apply to modules and members, including project documentation and stdlib mirrors; the most specific package prefix wins. Installed packages without a pattern open their PyPI page.").color(dim).small());
        ui.label(egui::RichText::new("Placeholders: {module.name} {module.basename} {element.name} {element.qname} {class.name} {function.name} {package} {python.version}").color(dim).small());
        ui.add_space(6.0);
        if ui.button("+ Add pattern").clicked() {
            self.settings
                .external_doc_urls
                .push(rustpy_settings::ExternalDocUrl {
                    module: String::new(),
                    url: "https://example.org/docs/{module.name}.{element.qname}.html".into(),
                });
        }
        let mut remove: Option<usize> = None;
        for (i, u) in self.settings.external_doc_urls.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut u.module)
                        .desired_width(140.0)
                        .hint_text("module"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut u.url)
                        .desired_width(420.0)
                        .hint_text("URL pattern"),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
            if let Some(message) =
                rustpy_semantic::external_docs::pattern_error(u.module.trim(), u.url.trim())
            {
                ui.colored_label(self.theme.error, message);
            }
        }
        if let Some(i) = remove {
            self.settings.external_doc_urls.remove(i);
        }
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Built-in patterns (a pattern above for the same module takes precedence):",
            )
            .color(dim)
            .small(),
        );
        egui::Grid::new("builtin_doc_patterns")
            .num_columns(2)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                for (module, url) in rustpy_semantic::external_docs::DEFAULT_PATTERNS {
                    ui.label(egui::RichText::new(*module).monospace());
                    ui.label(egui::RichText::new(*url).monospace().color(dim));
                    ui.end_row();
                }
            });
    }

    /// The "Rename element" quick fix: Rename on the shadowing name.
    pub(super) fn rename_element_fix(&mut self) {
        let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) else {
            return;
        };
        let caret = tab.state.primary().head();
        let Some(at) =
            rename_fix_target(&tab.state.quick_fixes, caret, |o| tab.state.doc.line_of(o))
        else {
            return;
        };
        if at != caret {
            tab.state.set_carets(vec![crate::editor::Caret::at(at)]);
        }
        self.pending_action = Some(Action::Rename);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_package_installs_and_module_files() {
        assert!(is_installed_package_entry(Path::new(
            "/p/.venv/lib/python3.12/site-packages/requests"
        )));
        assert!(is_installed_package_entry(Path::new(
            "/p/.venv/lib/python3.12/site-packages/ujson.cpython-312-x86_64-linux-gnu.so"
        )));
        assert!(is_installed_package_entry(Path::new(
            "/usr/lib/python3/dist-packages/yaml"
        )));
        assert!(!is_installed_package_entry(Path::new(
            "/p/.venv/lib/python3.12/site-packages/requests-2.31.0.dist-info"
        )));
        assert!(!is_installed_package_entry(Path::new(
            "/p/.venv/lib/python3.12/site-packages/requests/api.py"
        )));
        assert!(!is_installed_package_entry(Path::new(
            "/p/.venv/bin/python"
        )));
        assert!(is_module_file(Path::new(
            "/p/pkg/fast.cpython-312-x86_64-linux-gnu.so"
        )));
        assert!(is_module_file(Path::new("/p/pkg/mod.pyi")));
        assert!(!is_module_file(Path::new("/p/lib/libfoo.so.1")));
        assert!(!is_module_file(Path::new("/p/README.md")));
        let r = resolver_for(vec![PathBuf::from("/p"), PathBuf::from("/p/src")], None);
        assert_eq!(r.roots.len(), 2);
        assert!(r.search_paths.is_empty());
    }

    #[test]
    fn copy_reference_names_modules_like_imports() {
        let root = Path::new("/p");
        let r = resolver_for(vec![root.to_path_buf(), root.join("src")], None);
        // A Sources Root names its modules (`pkg.mod`, not `src.pkg.mod`).
        assert_eq!(
            reference_module(&r, root, &root.join("src/pkg/mod.py")).as_deref(),
            Some("pkg.mod")
        );
        assert_eq!(
            reference_module(&r, root, &root.join("src/pkg/__init__.py")).as_deref(),
            Some("pkg")
        );
        assert_eq!(
            reference_module(&r, root, &root.join("main.py")).as_deref(),
            Some("main")
        );
        // Outside the resolver's roots: the path below the project root.
        let bare = ModuleResolver::new(Vec::new(), Vec::new(), (3, 12));
        assert_eq!(
            reference_module(&bare, root, &root.join("tools/gen/__init__.py")).as_deref(),
            Some("tools.gen")
        );
        assert_eq!(
            reference_module(&bare, root, Path::new("/elsewhere/x.py")),
            None
        );
    }

    #[test]
    fn rename_fix_targets_the_shadowing_name() {
        let text = "def load(data, json):\n    return data\n";
        let line_of = |o: TextSize| text[..usize::from(o)].matches('\n').count();
        let rename = || {
            vec![QuickFix {
                title: inspections::RENAME_ELEMENT_FIX.into(),
                edits: Vec::new(),
            }]
        };
        let at = |s: &str| TextSize::new(text.find(s).unwrap() as u32);
        let range = |s: &str| TextRange::at(at(s), TextSize::new(s.len() as u32));
        let fixes = vec![
            (range("data"), rename()),
            (range("json"), rename()),
            (
                range("load"),
                vec![QuickFix {
                    title: "Other".into(),
                    edits: Vec::new(),
                }],
            ),
        ];
        // Under the caret: Rename starts where the caret is.
        assert_eq!(
            rename_fix_target(&fixes, at("json") + TextSize::new(2), line_of),
            Some(at("json") + TextSize::new(2))
        );
        // Elsewhere on the line: the first shadowing name.
        assert_eq!(
            rename_fix_target(&fixes, at("def"), line_of),
            Some(at("data"))
        );
        // Another line: nothing.
        assert_eq!(rename_fix_target(&fixes, at("return"), line_of), None);
    }
}

#[cfg(test)]
mod excluded_project_import_regression {
    use super::*;
    #[test]
    fn actual_project_exclusion_survives_open_editor() {
        let root = tempfile::tempdir().unwrap();
        let excluded = root.path().join("vendor");
        std::fs::create_dir_all(&excluded).unwrap();
        let file = excluded.join("hidden.py");
        std::fs::write(&file, "value = 1\n").unwrap();
        std::fs::write(root.path().join("main.py"), "import vendor.hidden\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.open_project(root.path());
        app.project
            .as_mut()
            .expect("actual project opened")
            .tree
            .excluded
            .insert(excluded);
        app.apply_interpreter_paths();
        app.open_file(&file);
        let project = app.project.as_ref().unwrap();
        assert!(
            project.analysis.module("vendor.hidden").is_none(),
            "editor-open excluded source must not become an import"
        );
        assert!(project.analysis.resolver.resolve("vendor.hidden").is_none());
        assert!(
            project.analysis.file_for_path(&file).is_some(),
            "exclusion must not prohibit explicit editor ownership"
        );
        assert!(app
            .tabs
            .iter()
            .any(|tab| tab.state.path.as_deref() == Some(file.as_path())));
    }
}

#[cfg(test)]
mod selected_interpreter_excluded_container_regression {
    use super::*;
    #[test]
    fn actual_project_exclusion_does_not_hide_selected_interpreter_dependency() {
        let root = tempfile::tempdir().unwrap();
        let venv = root.path().join(".venv");
        let site = venv.join("lib/python3.12/site-packages");
        let library = site.join("dependency");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir(root.path().join("vendor")).unwrap();
        let api = library.join("api.py");
        std::fs::write(library.join("__init__.py"), "").unwrap();
        std::fs::write(&api, "value = 1\n").unwrap();
        std::fs::write(root.path().join("vendor/hidden.py"), "value = 2\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.open_project(root.path());
        let project = app.project.as_mut().expect("actual project opened");
        project.tree.excluded.insert(venv.clone());
        project.tree.excluded.insert(root.path().join("vendor"));
        project.interpreter = Some(Interpreter {
            path: venv.join("bin/python"),
            version: "3.12.0".into(),
            prefix: venv,
            sys_path: vec![root.path().to_path_buf(), site],
            is_venv: true,
            platform: "linux".into(),
            kind: "fixture".into(),
        });
        app.apply_interpreter_paths();
        app.open_file(&api);
        let project = app.project.as_ref().unwrap();
        assert!(project.analysis.module("dependency.api").is_some());
        assert_eq!(
            project.analysis.resolver.resolve_source("dependency.api"),
            Some(api.clone())
        );
        assert_eq!(
            project.analysis.resolver.module_name_of(&api).as_deref(),
            Some("dependency.api")
        );
        assert!(project
            .analysis
            .resolver
            .top_level_modules()
            .contains(&"dependency".into()));
        assert!(project
            .analysis
            .resolver
            .submodules("dependency")
            .contains(&"api".into()));
        assert!(project.analysis.module("vendor.hidden").is_none());
        assert!(app
            .tabs
            .iter()
            .any(|tab| tab.state.path.as_deref() == Some(api.as_path())));
    }
}

#[cfg(test)]
mod documentation_interpreter_selection_tests {
    use super::*;
    #[test]
    fn changing_or_removing_interpreter_clears_prior_docs_before_any_new_worker_result() {
        let (_root, mut app, _) = super::super::quick_documentation_ui::tests::fixture();
        let old = Arc::new(rustpy_semantic::runtime_docs::RuntimeDocs::from_json(
            r#"{"format":1,"docs":{"builtins.len":"Wrong interpreter docs"}}"#,
        ));
        app.docs.docs = Some(old.clone());
        app.docs.for_interpreter = Some(("previous".into(), true));
        app.project.as_ref().unwrap().analysis.set_runtime_docs(old);
        app.project.as_mut().unwrap().interpreter = Some(Interpreter {
            path: PathBuf::from("/missing/remote-python"),
            version: "3.13.0".into(),
            prefix: PathBuf::from("/remote"),
            sys_path: vec![],
            is_venv: false,
            platform: "linux".into(),
            kind: "ssh".into(),
        });
        app.poll_runtime_docs();
        assert!(app.docs.docs.is_none());
        assert!(app.docs.pending.is_none());
        assert!(app
            .project
            .as_ref()
            .unwrap()
            .analysis
            .runtime_docs()
            .is_empty());
        app.project.as_mut().unwrap().interpreter = None;
        app.poll_runtime_docs();
        assert!(app.docs.for_interpreter.is_none());
    }
}
