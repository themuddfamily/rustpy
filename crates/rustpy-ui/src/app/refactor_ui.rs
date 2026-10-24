//! Refactoring correctness glue: keeping the analysis in step with the disk,
//! project-wide Safe Delete with "Delete Anyway", the "this occurrence / all
//! occurrences" choice of Extract Variable / Constant, import-aware Rename
//! and Delete in the Project view, and Move (F6).

use super::*;
use rustpy_refactor::refactor2::{self, FileOp};

/// Paths touched by planned file operations.
pub(super) fn file_op_paths(ops: &[FileOp]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for op in ops {
        match op {
            FileOp::Edit(p, _) | FileOp::Create(p, _) | FileOp::Delete(p) => out.push(p.clone()),
            FileOp::Move(from, to) => out.extend([from.clone(), to.clone()]),
        }
    }
    out
}

/// Applies refactoring edits to a file that is not open, refusing (rather
/// than panicking or corrupting it) when they do not fit its current text.
pub(super) fn edit_closed_file(path: &Path, edits: &[(TextRange, String)]) -> Result<(), String> {
    transform_closed_file(path, false, |text| {
        refactor::try_apply_edits(text, edits)
            .map(|text| (text, ()))
            .map_err(|e| e.to_string())
    })
}

/// Read, transform and conditionally write one immutable bounded baseline.
pub(super) fn transform_closed_file<T>(
    path: &Path,
    python_source: bool,
    transform: impl FnOnce(&str) -> Result<(String, T), String>,
) -> Result<T, String> {
    let vfs = rustpy_core::vfs::Vfs::new();
    let limits = rustpy_core::vfs::ReadLimits::default();
    let snapshot = vfs
        .read(path, limits)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if python_source {
        rustpy_text::Encoding::decode_python_source(&snapshot.bytes)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let (text, encoding) = rustpy_text::Encoding::decode(&snapshot.bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let (new_text, result) = transform(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if new_text != text {
        let bytes = encoding
            .encode(&new_text)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        crate::editor::save_actions::checked_write(
            &vfs,
            path,
            &bytes,
            Some(snapshot.revision),
            limits,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(result)
}

fn is_module_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("py")
        && path.file_name().and_then(|n| n.to_str()) != Some("__init__.py")
}

fn is_package_dir(path: &Path) -> bool {
    path.is_dir() && path.join("__init__.py").is_file()
}

fn is_module_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(rustpy_core::is_ident_start)
        && chars.all(rustpy_core::is_ident_continue)
        && !features::PYTHON_KEYWORDS.contains(&name)
}

/// The new name of a module renamed as a plain file (its name cannot be
/// imported): what was typed, plus `.py` unless it carries an extension
/// already (`a.b.py`, `notes.txt`). A package directory takes the name
/// without `.py`.
fn plain_rename_name(typed: &str, is_dir: bool) -> String {
    if is_dir {
        typed.strip_suffix(".py").unwrap_or(typed).to_string()
    } else if typed.contains('.') {
        typed.to_string()
    } else {
        format!("{typed}.py")
    }
}

impl IdeApp {
    pub(super) fn analysis_handle(&self) -> Option<Arc<Analysis>> {
        self.project.as_ref().map(|p| p.analysis.clone())
    }

    pub(super) fn tab_of(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(path))
    }

    // ----- analysis cache --------------------------------------------------

    /// Drops the analysis copies of editor texts whose tabs are gone (closed
    /// with or without saving, evicted by the tab limit), so the analysis
    /// reads those files from disk again.
    pub(super) fn release_closed_editors(&self, analysis: &Analysis) {
        for f in analysis.open_files() {
            if !f.path.starts_with("<typeshed>") && self.tab_of(&f.path).is_none() {
                analysis.close_file(&f.path);
            }
        }
    }

    /// Tells the analysis that `paths` (files or directories) changed on
    /// disk, in one batch (a watcher burst can hold thousands of events).
    /// Python files open in an editor keep their editor text.
    pub(super) fn invalidate_analysis_paths(&mut self, paths: &[PathBuf]) {
        let Some(analysis) = self.analysis_handle() else {
            return;
        };
        // (A large-file placeholder holds no editor text: the disk copy counts.)
        let in_editor: std::collections::HashSet<&Path> = self
            .tabs
            .iter()
            .filter(|t| t.state.language == Language::Python)
            .filter_map(|t| t.state.path.as_deref())
            .filter(|p| !self.bg.large_files.contains_key(*p))
            .collect();
        let mut seen: std::collections::HashSet<&Path> = std::collections::HashSet::new();
        let batch: Vec<PathBuf> = paths
            .iter()
            .filter(|p| seen.insert(p.as_path()) && !in_editor.contains(p.as_path()))
            .filter(|p| {
                !(p.extension().is_some() && !code_insight::is_module_file(p) && !p.is_dir())
            })
            .cloned()
            .collect();
        if !batch.is_empty() {
            analysis.invalidate_paths(&batch);
            self.reinspect_open_tabs();
        }
    }

    /// Re-reads everything the analysis loaded from disk (after Replace in
    /// Files, VCS operations, File › Synchronize).
    pub(super) fn reload_analysis_from_disk(&mut self) {
        self.code_vision.note_rescan();
        if let Some(a) = self.analysis_handle() {
            a.invalidate_disk_files();
            self.reinspect_open_tabs();
        }
    }

    /// Open editors re-run inspections (an import may resolve now, or no longer).
    fn reinspect_open_tabs(&mut self) {
        for t in &mut self.tabs {
            t.state.semantic_version = None;
        }
    }

    /// Pushes every open Python editor into the analysis now: project-wide
    /// refactorings must see the text of all of them, not just the active one.
    pub(super) fn sync_open_tabs_now(&mut self) {
        let Some(analysis) = self.analysis_handle() else {
            return;
        };
        for tab in &mut self.tabs {
            let Some(path) = tab.state.path.clone() else {
                continue;
            };
            // A large-file placeholder is empty (not loaded): the analysis keeps reading the disk.
            if tab.state.language != Language::Python
                || path.starts_with("<typeshed>")
                || self.bg.large_files.contains_key(&path)
            {
                continue;
            }
            let v = tab.state.doc.version();
            if tab.state.synced_version != Some(v) {
                analysis.set_file_text(&path, &tab.state.doc.text());
                tab.state.synced_version = Some(v);
            }
        }
    }

    // ----- Extract Variable / Constant occurrences -------------------------

    /// Extracts the expression at `range`. For a variable or constant that
    /// occurs more than once, `replace_all == None` first asks whether to
    /// replace this occurrence only or all of them (the occurrences are
    /// highlighted meanwhile).
    pub(super) fn run_extract_with(
        &mut self,
        kind: ExtractKind,
        range: TextRange,
        replace_all: Option<bool>,
    ) {
        let Some(i) = self.active else { return };
        let Some((analysis, file)) = self.analysis_file(i) else {
            return;
        };
        if replace_all.is_none() && matches!(kind, ExtractKind::Variable | ExtractKind::Constant) {
            let occurrences =
                refactor::extract_occurrences(&file, range, kind == ExtractKind::Constant);
            if occurrences.len() > 1 {
                // "All" is preselected unless merging the occurrences may change behaviour.
                let all_by_default = refactor::replace_all_by_default(&file, range);
                let n = occurrences.len();
                self.set_sticky_highlights(i, occurrences);
                let items = vec![
                    (
                        "This one".to_string(),
                        "Replace this occurrence only".to_string(),
                    ),
                    (format!("All {n}"), format!("Replace all {n} occurrences")),
                ];
                self.choice = Some(ChoicePopup {
                    selected: usize::from(all_by_default),
                    ..ChoicePopup::new(
                        ChoiceKind::ExtractOccurrences(kind, range),
                        format!("{n} occurrences found"),
                        items,
                    )
                });
                return;
            }
        }
        if matches!(kind, ExtractKind::Field | ExtractKind::Parameter) {
            self.open_extract_dialog(
                if kind == ExtractKind::Field {
                    super::extract_ui::ExtractRequest::Field
                } else {
                    super::extract_ui::ExtractRequest::Parameter
                },
                range,
            );
            return;
        }
        let all = replace_all.unwrap_or(false);
        let r = match kind {
            ExtractKind::Variable => refactor::extract_variable(&analysis, &file, range, all),
            ExtractKind::Constant => refactor::introduce_constant(&file, range, all),
            ExtractKind::Field => refactor::introduce_field(&analysis, &file, range),
            ExtractKind::Parameter => refactor::introduce_parameter(&analysis, &file, range),
        };
        self.apply_refactoring(r);
    }

    // ----- Move (F6) ---------------------------------------------------------

    /// F6: lifts a nested function to the module level, or moves the
    /// top-level function / class around the caret to another module.
    pub(super) fn move_refactoring(&mut self) {
        self.open_move_dialog();
    }

    // ----- Project view: Rename / Delete -------------------------------------

    /// Project view › Rename…: modules and packages go through Rename Module
    /// (imports are updated); other files and folders, and scratch files (no
    /// module imports them; the extension may change), are renamed as is.
    pub(super) fn open_tree_rename(&mut self, path: PathBuf) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.project.is_some()
            && !crate::scratches::is_scratch(&path)
            && (is_module_file(&path) || is_package_dir(&path))
        {
            let name = name.strip_suffix(".py").unwrap_or(&name).to_string();
            self.dialog = Dialog::RenameModule { path, name };
        } else {
            self.dialog = Dialog::RenamePath { path, name };
        }
    }

    /// Rename Module: renames the module or package and rewrites the imports
    /// of it. A typed `.py` is ignored. A name that cannot be imported
    /// (`my-script`), or a file outside the importable roots, is renamed as a
    /// plain file, but only when no import refers to it.
    pub(super) fn rename_module_from_dialog(
        &mut self,
        path: &Path,
        typed: &str,
    ) -> Result<(), String> {
        let typed = typed.trim();
        let name = typed
            .strip_suffix(".pyi")
            .or_else(|| typed.strip_suffix(".py"))
            .unwrap_or(typed);
        let Some(analysis) = self.analysis_handle() else {
            return Err("No project".into());
        };
        if !self.save_all() {
            return Err("Save failed; refactoring cancelled".into());
        }
        self.sync_open_tabs_now();
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let files = self.project_files();
        let module_file = if path.is_dir() {
            path.join("__init__.py")
        } else {
            path.to_path_buf()
        };
        let importable =
            analysis.resolver.module_name_of(&module_file).is_some() || path.starts_with(&root);
        if is_module_name(name) && importable {
            let ops = rustpy_refactor::rename_family::module_rename(
                &analysis, &root, path, name, &files,
            )?;
            self.apply_file_ops(ops, "Rename module");
            return Ok(());
        }
        let importers = if importable {
            refactor2::module_import_sites(&analysis, &root, path, &files)
        } else {
            Vec::new()
        };
        if !importers.is_empty() {
            return Err(format!(
                "'{name}' is not a valid module name, and {} import(s) refer to this module",
                importers.len()
            ));
        }
        // A package is renamed through its directory, also from its `__init__.py`.
        let target = match path.parent() {
            Some(dir) if path.file_name().and_then(|n| n.to_str()) == Some("__init__.py") => {
                dir.to_path_buf()
            }
            _ => path.to_path_buf(),
        };
        let file_name = plain_rename_name(typed, target.is_dir());
        let new_path = target.with_file_name(&file_name);
        if new_path.exists() {
            return Err(format!("{} already exists", new_path.display()));
        }
        self.apply_file_ops(
            vec![FileOp::Move(target.clone(), new_path.clone())],
            "Rename",
        );
        if target.exists() {
            return Err(format!("{} could not be renamed", target.display()));
        }
        self.set_status(format!(
            "Renamed to {} (not importable under that name)",
            new_path.display()
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_ops_report_every_touched_path() {
        let ops = vec![
            FileOp::Edit("/p/a.py".into(), Vec::new()),
            FileOp::Move("/p/b.py".into(), "/p/c.py".into()),
            FileOp::Create("/p/d.py".into(), String::new()),
            FileOp::Delete("/p/pkg".into()),
        ];
        let paths: Vec<String> = file_op_paths(&ops)
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            paths,
            vec!["/p/a.py", "/p/b.py", "/p/c.py", "/p/d.py", "/p/pkg"]
        );
    }

    #[test]
    fn closed_edits_preserve_encoding_and_refuse_unrepresentable_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        for encoding in [
            rustpy_text::Encoding::Latin1,
            rustpy_text::Encoding::Utf8Bom,
            rustpy_text::Encoding::Utf16Le,
            rustpy_text::Encoding::Utf16Be,
        ] {
            let source = if encoding == rustpy_text::Encoding::Latin1 {
                "# coding: latin-1\r\nx = 1\r\n"
            } else {
                "x = 1\r\n"
            };
            let original = encoding.encode(source).unwrap();
            std::fs::write(&path, &original).unwrap();
            let at = source.rfind("1").unwrap() as u32;
            edit_closed_file(
                &path,
                &[(
                    TextRange::new(TextSize::new(at), TextSize::new(at + 1)),
                    "2".into(),
                )],
            )
            .unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                encoding
                    .encode(&source.replacen("x = 1", "x = 2", 1))
                    .unwrap()
            );
            let bytes = std::fs::read(&path).unwrap();
            if encoding == rustpy_text::Encoding::Latin1 {
                assert!(edit_closed_file(
                    &path,
                    &[(TextRange::empty(TextSize::new(at)), "😀".into())]
                )
                .is_err());
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn closed_file_edits_refuse_text_that_moved() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("rustpy-refactor-ui-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.py");
        std::fs::write(&path, "x = 1\n").unwrap();
        let edit = |s: u32, e: u32, t: &str| {
            (
                TextRange::new(TextSize::new(s), TextSize::new(e)),
                t.to_string(),
            )
        };
        edit_closed_file(&path, &[edit(0, 1, "y")]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "y = 1\n");
        // Out of bounds, or inside a multi-byte character: refused, file untouched.
        assert!(edit_closed_file(&path, &[edit(4, 40, "2")]).is_err());
        std::fs::write(&path, "é = 1\n").unwrap();
        assert!(edit_closed_file(&path, &[edit(1, 2, "z")]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "é = 1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_renames_keep_the_extension() {
        // `a.b.py` cannot be imported, and stays a Python file.
        assert_eq!(plain_rename_name("a.b.py", false), "a.b.py");
        assert_eq!(plain_rename_name("my-script", false), "my-script.py");
        assert_eq!(plain_rename_name("my-script.py", false), "my-script.py");
        assert_eq!(plain_rename_name("notes.txt", false), "notes.txt");
        assert_eq!(plain_rename_name("my-pkg", true), "my-pkg");
        assert_eq!(plain_rename_name("my-pkg.py", true), "my-pkg");
    }

    #[test]
    fn module_names_and_kinds() {
        assert!(is_module_name("helpers") && is_module_name("_private2"));
        assert!(
            !is_module_name("my-script")
                && !is_module_name("class")
                && !is_module_name("2fast")
                && !is_module_name("")
        );
        assert!(is_module_file(Path::new("/p/util.py")));
        assert!(
            !is_module_file(Path::new("/p/__init__.py"))
                && !is_module_file(Path::new("/p/notes.txt"))
        );
    }
}
