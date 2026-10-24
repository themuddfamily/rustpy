//! Rename (Shift+F6, roadmap §2.8): in place when every usage is certain and
//! in this file, else the Rename dialog ("Search in comments and strings",
//! Preview) and the Refactoring Preview, which also opens by itself when the
//! rename found dynamic usages, comment/string occurrences or conflicts.
//!
//! Changes are applied all or nothing: every place must still hold the old
//! name. Open editors get one undo step and a Local History snapshot each,
//! closed files a Local History snapshot before they are written. The
//! in-place rename itself is `inplace_rename.rs`, which refuses a new name
//! that merges with another name of the scope (the old name comes back).

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::refactor_preview::{GroupKind, PreviewItem, RefactorPreview};
use rustpy_refactor::refactor2::FileOp;
use rustpy_refactor::rename::{self, RenameConflict, RenamePlan};
use rustpy_refactor::rename_family::{self, FamilyOptions, FamilyPlan, HierarchyChoice};
use rustpy_settings::scopes::{PredefinedScope, ScopeSelection};
use rustpy_syntax::ast::AstNode;

/// Where Rename started: the symbol at `offset` in `path`, at document `version`.
#[derive(Clone, Debug)]
struct RenameOrigin {
    path: PathBuf,
    offset: TextSize,
    version: u64,
}

/// The Rename dialog.
pub(super) struct RenameDialogState {
    origin: RenameOrigin,
    /// `method 'run'`.
    what: String,
    name: String,
    search_in_comments: bool,
    family: FamilyOptions,
    scope: ScopeSelection,
    hierarchy_count: usize,
    error: Option<String>,
    /// The name is selected once the text field exists.
    select_name: bool,
}

/// The Refactoring Preview of a rename.
pub(super) struct RenamePreviewState {
    old_name: String,
    new_name: String,
    what: String,
    preview: RefactorPreview,
    /// Why the preview opened instead of renaming at once.
    reasons: Vec<(String, bool)>,
    error: Option<String>,
    family: Option<FamilyPlan>,
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

fn usage_item(u: &Usage, checked: bool) -> PreviewItem {
    let note = if u.is_definition {
        "def"
    } else if u.is_write {
        "write"
    } else {
        ""
    };
    PreviewItem {
        path: u.path.clone(),
        range: u.range,
        line: u.line,
        column: u.column,
        line_text: u.line_text.clone(),
        checked,
        note: note.into(),
    }
}

fn conflict_item(c: &RenameConflict) -> PreviewItem {
    PreviewItem {
        path: c.path.clone(),
        range: c.range,
        line: c.line,
        column: c.column,
        line_text: c.line_text.clone(),
        checked: false,
        note: c.message.clone(),
    }
}

/// The preview tree of `plan`, and why it is worth a look (`true`: a conflict).
fn rename_preview(plan: &RenamePlan) -> (RefactorPreview, Vec<(String, bool)>) {
    let mut p = RefactorPreview::default();
    p.add_group(
        GroupKind::Conflicts,
        "Conflicts",
        "Renaming changes what this code means",
        plan.conflicts.iter().map(conflict_item).collect(),
    );
    p.add_group(
        GroupKind::Code,
        "Code usages to rename",
        "Definitions and references resolved to the symbol",
        plan.code.iter().map(|u| usage_item(u, true)).collect(),
    );
    p.add_group(
        GroupKind::Dynamic,
        "Dynamic usages (type unknown or ambiguous)",
        "Attributes of objects whose type is unknown, or may be another class with a member of that name (x: A | B), and keyword arguments of calls that do not resolve, matched by name only: they may belong to another symbol. Check the ones that are this symbol.",
        plan.dynamic.iter().map(|u| usage_item(u, false)).collect(),
    );
    p.add_group(
        GroupKind::NonCode,
        "Occurrences in comments and strings",
        "Whole-word matches in comments and string literals",
        plan.text.iter().map(|u| usage_item(u, true)).collect(),
    );
    let mut reasons = Vec::new();
    if !plan.conflicts.is_empty() {
        reasons.push((
            format!(
                "{}: the rename changes what the code means.",
                plural(plan.conflicts.len(), "conflict")
            ),
            true,
        ));
    }
    if !plan.dynamic.is_empty() {
        reasons.push((format!("{} on objects of unknown or ambiguous type matched by name only; renamed only if checked.", plural(plan.dynamic.len(), "dynamic usage")), false));
    }
    if !plan.text.is_empty() {
        reasons.push((
            format!(
                "{} in comments and strings.",
                plural(plan.text.len(), "occurrence")
            ),
            false,
        ));
    }
    (p, reasons)
}

/// Edits renaming every place to `new_name`, grouped by file (in first-seen
/// order), sorted, without duplicates.
fn edits_by_file(
    places: Vec<(PathBuf, TextRange)>,
    new_name: &str,
) -> Vec<(PathBuf, Vec<(TextRange, String)>)> {
    let mut by_file: Vec<(PathBuf, Vec<(TextRange, String)>)> = Vec::new();
    for (path, range) in places {
        match by_file.iter_mut().find(|(p, _)| *p == path) {
            Some((_, edits)) => edits.push((range, new_name.to_string())),
            None => by_file.push((path, vec![(range, new_name.to_string())])),
        }
    }
    for (_, edits) in &mut by_file {
        edits.sort_by_key(|(r, _)| r.start());
        edits.dedup_by_key(|(r, _)| *r);
    }
    by_file
}

/// Whether every edited range of an open editor still holds `old_name`.
fn holds_old_name(
    doc: &rustpy_text::Document,
    edits: &[(TextRange, String)],
    old_name: &str,
) -> bool {
    edits
        .iter()
        .all(|(r, _)| r.end() <= doc.len() && doc.slice(*r) == old_name)
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Checked for every closed file before a rename changes anything: the
/// file still holds `old_name` at each edited range, and it can be written
/// (not read-only, not locked by another program).
fn check_closed_file(
    path: &Path,
    edits: &[(TextRange, String)],
    old_name: &str,
) -> Result<(), String> {
    let name = file_label(path);
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{name} cannot be read ({e}): nothing was renamed"))?;
    rename::apply_rename_edits(&text, edits, old_name).map_err(|_| {
        format!("{name} changed since the usages were found: nothing was renamed. Run Rename again")
    })?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| format!("{name} cannot be written ({e}): nothing was renamed"))?;
    Ok(())
}

/// The status after a rename was applied to `files` files: an error when
/// some closed files could not be written after all (`failed`), since the
/// code is then only partly renamed.
fn rename_outcome(
    old_name: &str,
    new_name: &str,
    count: usize,
    files: usize,
    failed: &[String],
) -> Result<String, String> {
    if failed.is_empty() {
        return Ok(format!(
            "Renamed '{old_name}' to '{new_name}': {} in {}",
            plural(count, "occurrence"),
            plural(files, "file")
        ));
    }
    let what = if failed.len() == 1 {
        "1 file was".to_string()
    } else {
        format!("{} files were", failed.len())
    };
    Err(format!(
        "Rename of '{old_name}' to '{new_name}' is incomplete: {what} not changed ({}). The other files were renamed; Local History keeps their 'Before Rename' versions",
        failed.join("; ")
    ))
}

/// The status bar while an in-place rename is typed (short enough not to
/// run into the status bar's widgets).
impl IdeApp {
    fn tab_index_of(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(path))
    }

    pub(super) fn sync_rename_text_sources(&self) {
        if let Some(analysis) = self.analysis_handle() {
            for tab in &self.tabs {
                if tab.state.language == Language::Python || tab.state.doc.is_read_only() {
                    continue;
                }
                if let Some(path) = &tab.state.path {
                    if !self.bg.large_files.contains_key(path) {
                        analysis.set_file_text(path, &tab.state.doc.text());
                    }
                }
            }
        }
    }
    fn rename_module_target(
        &mut self,
        analysis: &Analysis,
        file: &std::sync::Arc<rustpy_semantic::db::FileInfo>,
        offset: TextSize,
        symbol: &usages::Symbol,
    ) -> bool {
        if let usages::Symbol::Name { def, .. } = symbol {
            if usages::is_renaming_alias(def.binding()) {
                return false;
            }
        }
        if rename::check_rename_target(analysis, symbol).is_ok() {
            return false;
        }
        let root = file.parse.syntax_node();
        let token = root
            .token_at_offset(offset)
            .right_biased()
            .or_else(|| root.token_at_offset(offset).left_biased());
        let expression = token.and_then(|token| {
            token
                .parent_ancestors()
                .find_map(rustpy_syntax::ast::Expr::cast)
        });
        let Some(expression) = expression else {
            return false;
        };
        if let rustpy_semantic::types::Type::Module(id) =
            rustpy_semantic::infer::Inferer::new(analysis).type_of_expr(file, &expression)
        {
            if let Some(module) = analysis.file(id) {
                if usages::is_library_file(analysis, &module) {
                    return false;
                }
                let path = module.path.clone();
                let name = if module.is_package_init() {
                    path.parent()
                        .and_then(|path| path.file_name())
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                } else {
                    path.file_stem()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                self.dialog = Dialog::RenameModule { path, name };
                return true;
            }
        }
        false
    }
    fn apply_family_rename(
        &mut self,
        family: &FamilyPlan,
        places: Vec<(PathBuf, TextRange)>,
    ) -> Result<(), String> {
        self.sync_open_tabs_now();
        self.sync_rename_text_sources();
        for (path, original) in &family.snapshots {
            let current = if let Some(index) = self.tab_index_of(path) {
                self.tabs[index].state.doc.text()
            } else {
                std::fs::read_to_string(path).map_err(|error| error.to_string())?
            };
            if &current != original {
                return Err(format!(
                    "{} changed since Rename was planned; reopen Rename",
                    path.display()
                ));
            }
        }
        let analysis = self.analysis_handle().ok_or("No project analysis")?;
        let files = self.project_files();
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        let operations = rename_family::transaction(&analysis, family, places, &files, &root)?;
        if !family.extra.is_empty() {
            self.preview_file_ops(operations, "Rename related symbols/files");
            return Ok(());
        }
        let failures = self.apply_file_ops_failures(operations, "Rename");
        if !failures.is_empty() {
            return Err(failures.join("\n"));
        }
        self.set_status(format!(
            "Renamed '{}' to '{}'",
            family.primary.old_name, family.primary.new_name
        ));
        Ok(())
    }

    /// The Rename dialog (`force_dialog`, or when the rename cannot be done
    /// in place), else an in-place rename through main's engine
    /// (`inplace_rename.rs`), which checks the new name when it ends.
    pub(super) fn open_rename(&mut self, force_dialog: bool, seed: Option<String>) {
        self.sync_open_tabs_now();
        let Some(i) = self.active else { return };
        if self.tabs[i].state.language != Language::Python {
            self.open_framework_rename(i);
            return;
        }
        let Some((analysis, file)) = self.analysis_file(i) else {
            self.set_status("Rename works on Python symbols in a project");
            return;
        };
        let caret = self.tabs[i].state.primary().head();
        let Some(sym) = usages::symbol_at(&analysis, &file, caret) else {
            self.open_framework_rename(i);
            return;
        };
        if self.rename_module_target(&analysis, &file, caret, &sym) {
            return;
        }
        let files = self.project_files();
        let hierarchy_count = rename_family::hierarchy_targets(
            &analysis,
            &file,
            caret,
            &files,
            HierarchyChoice::EntireHierarchy,
        )
        .len();
        let extended = hierarchy_count > 0
            || !rename_family::semantic_strings(&analysis, &sym, &files).is_empty()
            || match rustpy_refactor::rename_framework::related_edits(
                &analysis,
                &sym,
                "__rename_probe",
                &files,
            ) {
                Ok(edits) => !edits.is_empty(),
                Err(_) => true,
            };
        if let Err(e) = rename::check_rename_target(&analysis, &sym) {
            self.set_status(e);
            return;
        }
        let old_name = sym.name().to_string();
        let search_in_comments = self.settings.refactoring.search_in_comments;
        if self.settings.refactoring.rename_in_place
            && (!matches!(sym.kind(), "variable" | "parameter" | "class attribute")
                || self.settings.refactoring.rename_variables_in_place)
            && !self.settings.refactoring.preview_refactoring
            && !force_dialog
            && !extended
        {
            let files = self.project_files();
            if let (Some(r), Ok(check)) = (
                rename::inplace_rename(&analysis, &file, caret, &files, search_in_comments),
                rename::rename_check(&analysis, &file, caret),
            ) {
                self.start_inplace_rename(
                    i,
                    r.ranges,
                    caret,
                    super::inplace_rename::InplaceKind::Rename,
                    check,
                );
                if r.strings_left_out > 0 {
                    self.set_status(format!("Rename '{old_name}' in place: {} string literal(s) with the same text are left as they are (Shift+F6 again for the dialog)", r.strings_left_out));
                }
                return;
            }
        }
        let Some(path) = self.tabs[i].state.path.clone() else {
            return;
        };
        let version = self.tabs[i].state.doc.version();
        self.dialog = Dialog::Rename(Box::new(RenameDialogState {
            origin: RenameOrigin {
                path,
                offset: caret,
                version,
            },
            what: format!("{} '{old_name}'", sym.kind()),
            name: seed.unwrap_or(old_name),
            search_in_comments,
            family: FamilyOptions::default(),
            scope: ScopeSelection::Predefined(PredefinedScope::AllPlaces),
            hierarchy_count,
            error: None,
            select_name: true,
        }));
    }

    /// The rename the dialog describes, computed on the current editor texts.
    fn plan_rename(&mut self, st: &RenameDialogState) -> Result<FamilyPlan, String> {
        self.sync_open_tabs_now();
        let i = self
            .tab_index_of(&st.origin.path)
            .ok_or("The file is no longer open")?;
        if self.tabs[i].state.doc.version() != st.origin.version {
            return Err(
                "The file changed after Rename started: cancel and start Rename again".into(),
            );
        }
        let (analysis, file) = self
            .analysis_file(i)
            .ok_or("The file is not part of the project's analysis")?;
        self.sync_rename_text_sources();
        let mut files = self.project_files();
        files.extend(self.tabs.iter().filter_map(|tab| tab.state.path.clone()));
        files.sort();
        files.dedup();
        let mut context = self.capture_scope_context();
        if context.project_files.is_empty() {
            let scope = self.project_scope();
            context.project_files = std::sync::Arc::new(
                files
                    .iter()
                    .filter(|path| scope.as_ref().is_some_and(|scope| scope.contains(path)))
                    .cloned()
                    .collect(),
            );
        }
        let compiled = self.settings.scopes.compile(&st.scope)?;
        let mut options = st.family.clone();
        options.base.search_in_comments = st.search_in_comments;
        options.reference_scope = Some(
            files
                .iter()
                .filter(|path| compiled.matches(path, &context))
                .cloned()
                .collect(),
        );
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        rename_family::plan(
            &analysis,
            &file,
            st.origin.offset,
            st.name.trim(),
            &files,
            &root,
            &options,
        )
    }

    /// Renames `old_name` to `new_name` at `places`, all or nothing: if any
    /// place no longer holds `old_name` (its editor or file changed since the
    /// usages were found) or a closed file cannot be written, nothing is
    /// changed. A closed file that still fails to be written makes the
    /// result an error (the rename is incomplete), not a success. Returns
    /// `(occurrences, files)`.
    fn apply_rename_changes(
        &mut self,
        old_name: &str,
        new_name: &str,
        places: Vec<(PathBuf, TextRange)>,
    ) -> Result<(usize, usize), String> {
        let by_file = edits_by_file(places, new_name);
        for (path, edits) in &by_file {
            match self.tab_index_of(path) {
                Some(t) => {
                    // A read-only editor (a library file) would refuse its edits after
                    // the others were changed: nothing is renamed instead.
                    if self.tabs[t].state.doc.is_read_only() {
                        return Err(format!(
                            "{} is read-only: nothing was renamed. Make it writable (the status bar's lock) to rename there too",
                            file_label(path)
                        ));
                    }
                    if !holds_old_name(&self.tabs[t].state.doc, edits, old_name) {
                        return Err(format!(
                            "{} changed since the usages were found: nothing was renamed. Run Rename again",
                            file_label(path)
                        ));
                    }
                }
                None => check_closed_file(path, edits, old_name)?,
            }
        }
        let count: usize = by_file.iter().map(|(_, e)| e.len()).sum();
        let files = by_file.len();
        let failed = self.apply_file_ops_failures(
            by_file
                .into_iter()
                .map(|(p, e)| FileOp::Edit(p, e))
                .collect(),
            "Rename",
        );
        if let Some(p) = &mut self.project {
            p.git_refreshed = -100.0;
        }
        match rename_outcome(old_name, new_name, count, files, &failed) {
            Ok(status) => {
                self.set_status(status);
                Ok((count, files))
            }
            Err(e) => {
                self.set_status(e.clone());
                Err(e)
            }
        }
    }

    pub(super) fn rename_dialog(&mut self, ctx: &egui::Context, mut st: Box<RenameDialogState>) {
        let theme = self.theme.clone();
        let (mut open, mut refactor, mut preview, mut cancel) = (true, false, false, false);
        egui::Window::new("Rename").id(egui::Id::new("rename_dialog")).collapsible(false).resizable(false).default_width(440.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            ui.label(format!("Rename {} and its usages to:", st.what));
            let r = ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(400.0));
            r.request_focus();
            if st.select_name && egui::TextEdit::load_state(ui.ctx(), r.id).is_some() {
                crate::theme::select_all_text(ui, &r, &st.name);
                st.select_name = false;
            }
            if (r.lost_focus() || r.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                refactor = true;
            }
            ui.checkbox(&mut st.search_in_comments, "Search in comments and strings").on_hover_text("Also rename whole-word occurrences in comments and string literals (shown in the preview)");
            ui.checkbox(&mut st.family.search_references,"Search for references");
            let names=self.scope_names();super::scope_ui::scope_selector(ui,&mut st.scope,&names,"rename-reference-scope");
            if st.hierarchy_count>0 {ui.label(format!("{} related method/parameter declarations",st.hierarchy_count));egui::ComboBox::from_id_salt("rename-hierarchy").selected_text(match st.family.hierarchy {HierarchyChoice::Declaration=>"Only this declaration",HierarchyChoice::Overrides=>"This declaration and overrides",HierarchyChoice::EntireHierarchy=>"Base and overriding declarations"}).show_ui(ui,|ui|{ui.selectable_value(&mut st.family.hierarchy,HierarchyChoice::Declaration,"Only this declaration");ui.selectable_value(&mut st.family.hierarchy,HierarchyChoice::Overrides,"This declaration and overrides");ui.selectable_value(&mut st.family.hierarchy,HierarchyChoice::EntireHierarchy,"Base and overriding declarations");}).combo_named(crate::i18n::tr_static("Hierarchy"));}
            ui.checkbox(&mut st.family.dynamic_strings,"Canonical getattr/setattr and metadata string references");
            ui.checkbox(&mut st.family.text_files,"Text occurrences in non-Python files (opt in)");
            ui.checkbox(&mut st.family.related_tests,"Rename related test names and test files");
            ui.checkbox(&mut st.family.similar_variables,"Rename similarly named variables of this class");
            ui.checkbox(&mut st.family.rename_inheritors,"Rename similarly named inheritor classes");
            ui.checkbox(&mut st.family.rename_matching_file,"Rename matching module file and paired stub");
            ui.checkbox(&mut st.family.django,"Related Django context/URL names and template references");

            if let Some(e) = &st.error {
                ui.label(egui::RichText::new(e).color(theme.error));
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Preview").on_hover_text("Review every change before renaming").clicked() {
                        preview = true;
                    }
                    if ui.button("Refactor").clicked() {
                        refactor = true;
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
        if !(refactor || preview) {
            if open && !cancel {
                self.dialog = Dialog::Rename(st);
            } else {
                self.focus_editor = true;
            }
            return;
        }
        // The checkbox is the setting (Settings › Editor › Refactoring).
        if st.search_in_comments != self.settings.refactoring.search_in_comments {
            self.settings.refactoring.search_in_comments = st.search_in_comments;
            let _ = self.settings.save();
        }
        let family = match self.plan_rename(&st) {
            Ok(plan) => plan,
            Err(e) => {
                st.error = Some(e);
                self.dialog = Dialog::Rename(st);
                return;
            }
        };
        let plan = &family.primary;
        if preview
            || self.settings.refactoring.preview_refactoring
            || plan.needs_preview()
            || !family.extra.is_empty()
            || !family.notes.is_empty()
        {
            let (tree, mut reasons) = rename_preview(plan);
            reasons.extend(family.notes.iter().cloned().map(|note| (note, false)));
            self.dialog = Dialog::RenamePreview(Box::new(RenamePreviewState {
                old_name: plan.old_name.clone(),
                new_name: plan.new_name.clone(),
                what: st.what.clone(),
                preview: tree,
                reasons,
                error: None,
                family: Some(family),
            }));
            return;
        }
        let places = plan
            .code
            .iter()
            .map(|u| (u.path.clone(), u.range))
            .collect();
        match self.apply_family_rename(&family, places) {
            Ok(_) => self.focus_editor = true,
            Err(e) => {
                st.error = Some(e);
                self.dialog = Dialog::Rename(st);
            }
        }
    }

    pub(super) fn rename_preview_dialog(
        &mut self,
        ctx: &egui::Context,
        mut st: Box<RenamePreviewState>,
    ) {
        let theme = self.theme.clone();
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let (mut open, mut apply, mut cancel) = (true, false, false);
        let mut show: Option<(PathBuf, TextRange)> = None;
        let center = ctx.content_rect().center();
        egui::Window::new("Refactoring Preview")
            .id(egui::Id::new("rename_preview"))
            .collapsible(false)
            .resizable(true)
            .default_size([780.0, 460.0])
            .default_pos(center - Vec2::new(390.0, 260.0))
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label(
                    egui::RichText::new(format!("Rename {} to '{}'", st.what, st.new_name))
                        .strong()
                        .color(theme.text),
                );
                for (reason, conflict) in &st.reasons {
                    ui.label(egui::RichText::new(reason).color(if *conflict {
                        theme.error
                    } else {
                        theme.warning
                    }));
                }
                ui.separator();
                let tree_height = (ui.available_height() - 40.0).max(120.0);
                egui::ScrollArea::vertical()
                    .id_salt("rename_preview_scroll")
                    .max_height(tree_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        show =
                            st.preview
                                .ui(ui, &theme, &root, egui::Id::new("rename_preview_tree"));
                    });
                ui.separator();
                if let Some(e) = &st.error {
                    ui.label(egui::RichText::new(e).color(theme.error));
                }
                ui.horizontal(|ui| {
                    let (n, files) = (st.preview.checked_count(), st.preview.checked_files());
                    ui.label(
                        egui::RichText::new(format!(
                            "{} in {}",
                            plural(n, "change"),
                            plural(files, "file")
                        ))
                        .color(theme.text_dim),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        let label = if st.preview.conflict_count() > 0 {
                            "Do Refactor Anyway"
                        } else {
                            "Do Refactor"
                        };
                        if ui.add_enabled(n > 0, egui::Button::new(label)).clicked() {
                            apply = true;
                        }
                    });
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
        if let Some((path, range)) = show {
            self.navigate_to(&path, range);
        }
        if apply {
            let places = st
                .preview
                .checked()
                .map(|i| (i.path.clone(), i.range))
                .collect();
            let result = if let Some(family) = &st.family {
                self.apply_family_rename(family, places)
            } else {
                self.apply_rename_changes(&st.old_name, &st.new_name, places)
                    .map(|_| ())
            };
            match result {
                Ok(_) => {
                    self.focus_editor = true;
                    return;
                }
                Err(e) => st.error = Some(e),
            }
            self.dialog = Dialog::RenamePreview(st);
        } else if open && !cancel {
            self.dialog = Dialog::RenamePreview(st);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_exposes_hierarchy_scope_and_related_name_choices_without_editing_source() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.py");
        let source =
            "class Base:\n    def run(self): pass\nclass Child(Base):\n    def run(self): pass\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        let mut app = IdeApp::new(settings, Some(root.path().to_owned()));
        app.open_file(&path);
        let index = app.active.unwrap();
        app.tabs[index]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::new(
                source.find("run(self)").unwrap() as u32,
            ))]);
        app.open_rename(true, None);
        let Dialog::Rename(state) = std::mem::replace(&mut app.dialog, Dialog::None) else {
            panic!("missing Rename dialog")
        };
        assert!(state.hierarchy_count > 0);
        assert!(state.family.search_references);
        assert!(!state.family.text_files);
        assert_eq!(
            state.scope,
            ScopeSelection::Predefined(PredefinedScope::AllPlaces)
        );
        let mut state = Some(state);
        let context = egui::Context::default();
        crate::theme::install_fonts(&context);
        let mut shapes = Vec::new();
        // The Area sizing pass retains state but does not paint visible labels.
        for pass in 0..2 {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 1200.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    let context = context.ctx();
                    if let Some(state) = state.take() {
                        app.rename_dialog(context, state);
                    }
                },
            );
            shapes = std::mem::take(&mut output.shapes);
            output.drop_without_applying_deltas();
            if pass == 0 {
                let Dialog::Rename(retained) = std::mem::replace(&mut app.dialog, Dialog::None)
                else {
                    panic!("rename dialog disappeared during its sizing frame")
                };
                assert!(retained.hierarchy_count > 0);
                assert!(retained.family.search_references);
                assert!(!retained.family.text_files);
                assert_eq!(
                    retained.scope,
                    ScopeSelection::Predefined(PredefinedScope::AllPlaces)
                );
                state = Some(retained);
            }
        }
        for label in [
            "Search for references",
            "Base and overriding declarations",
            "Rename related test names and test files",
            "Rename similarly named variables of this class",
        ] {
            assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains(label))),"{label}");
        }
        assert_eq!(app.tabs[index].state.doc.text(), source);
    }

    #[test]
    fn edits_are_grouped_and_checked_before_anything_changes() {
        let r = |s: u32| TextRange::at(TextSize::new(s), TextSize::new(3));
        let places = vec![
            (PathBuf::from("/p/b.py"), r(10)),
            (PathBuf::from("/p/a.py"), r(4)),
            (PathBuf::from("/p/b.py"), r(0)),
            (PathBuf::from("/p/b.py"), r(10)),
        ];
        let by_file = edits_by_file(places, "go");
        assert_eq!(by_file.len(), 2);
        assert_eq!(by_file[0].0, PathBuf::from("/p/b.py"));
        assert_eq!(
            by_file[0]
                .1
                .iter()
                .map(|(r, _)| u32::from(r.start()))
                .collect::<Vec<_>>(),
            vec![0, 10],
            "sorted, duplicates dropped"
        );
        assert!(by_file.iter().flat_map(|(_, e)| e).all(|(_, t)| t == "go"));
        // An editor whose text moved under the ranges is refused.
        let doc = rustpy_text::Document::new("run = 1\nx = run\n");
        let edits = vec![(r(0), "go".to_string()), (r(12), "go".to_string())];
        assert!(holds_old_name(&doc, &edits, "run"));
        let edited = rustpy_text::Document::new("# moved\nrun = 1\nx = run\n");
        assert!(!holds_old_name(&edited, &edits, "run"));
        assert!(
            !holds_old_name(&rustpy_text::Document::new("run"), &edits, "run"),
            "a range past the end"
        );
    }

    /// Keeps a file unwritable while alive.
    struct Unwritable(PathBuf);

    impl Drop for Unwritable {
        fn drop(&mut self) {
            let _ = std::process::Command::new("chattr")
                .arg("-i")
                .arg(&self.0)
                .output();
            if let Ok(meta) = std::fs::metadata(&self.0) {
                let mut perms = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(&self.0, perms);
            }
        }
    }

    fn writable(path: &Path) -> bool {
        std::fs::OpenOptions::new().append(true).open(path).is_ok()
    }

    /// Makes `path` unwritable: read-only, or immutable (`chattr +i`) for
    /// root, whom permissions do not stop. `None` when this system cannot.
    fn unwritable(path: &Path) -> Option<Unwritable> {
        let guard = Unwritable(path.to_path_buf());
        let mut perms = std::fs::metadata(path).ok()?.permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(path, perms).ok()?;
        if !writable(path) {
            return Some(guard);
        }
        let _ = std::process::Command::new("chattr")
            .arg("+i")
            .arg(path)
            .output();
        (!writable(path)).then_some(guard)
    }

    /// Review: a closed file that could not be written was reported as
    /// "Renamed …" (apply_file_ops' "not changed" status was overwritten).
    #[test]
    fn closed_files_are_checked_first_and_a_failed_write_is_no_success() {
        let dir = crate::git_jobs::TestDir::new("rename-closed");
        let path = dir.path().join("other.py");
        std::fs::write(&path, "from main import run\nrun()\n").unwrap();
        let r = |s: u32| TextRange::at(TextSize::new(s), TextSize::new(3));
        let edits = vec![(r(17), "go".to_string()), (r(21), "go".to_string())];
        assert_eq!(check_closed_file(&path, &edits, "run"), Ok(()));
        // Changed since the usages were found, or gone: nothing is renamed.
        assert!(check_closed_file(&path, &edits, "jog")
            .unwrap_err()
            .contains("changed since the usages were found: nothing was renamed"));
        assert!(
            check_closed_file(&dir.path().join("gone.py"), &edits, "run")
                .unwrap_err()
                .contains("cannot be read")
        );
        // Read-only or locked: refused before anything changes.
        match unwritable(&path) {
            Some(_guard) => {
                let e = check_closed_file(&path, &edits, "run").unwrap_err();
                assert!(
                    e.starts_with("other.py cannot be written (")
                        && e.ends_with("): nothing was renamed"),
                    "{e}"
                );
            }
            None => {
                eprintln!("cannot make a file unwritable here: the write check is not exercised")
            }
        }
        // Applied: a file that still could not be written makes it an error.
        assert_eq!(
            rename_outcome("run", "go", 3, 2, &[]),
            Ok("Renamed 'run' to 'go': 3 occurrences in 2 files".to_string())
        );
        let e = rename_outcome(
            "run",
            "go",
            3,
            2,
            &["/p/other.py: Permission denied (os error 13)".to_string()],
        )
        .unwrap_err();
        assert!(e.starts_with("Rename of 'run' to 'go' is incomplete: 1 file was not changed (/p/other.py: Permission denied (os error 13))"), "{e}");
    }

    fn usage(path: &str, line: usize, dynamic: bool) -> Usage {
        Usage {
            path: PathBuf::from(path),
            range: TextRange::at(TextSize::new(4), TextSize::new(3)),
            line,
            column: 4,
            line_text: "    run()".into(),
            is_write: false,
            is_definition: line == 0,
            dynamic,
            non_code: false,
        }
    }

    #[test]
    fn the_preview_groups_a_plan() {
        let plan = RenamePlan {
            old_name: "run".into(),
            new_name: "go".into(),
            code: vec![usage("/p/a.py", 0, false), usage("/p/b.py", 4, false)],
            dynamic: vec![usage("/p/c.py", 2, true)],
            text: Vec::new(),
            conflicts: Vec::new(),
        };
        let (p, reasons) = rename_preview(&plan);
        assert_eq!(
            p.groups.iter().map(|g| g.kind).collect::<Vec<_>>(),
            vec![GroupKind::Code, GroupKind::Dynamic]
        );
        assert_eq!(
            (p.checked_count(), p.checked_files()),
            (2, 2),
            "dynamic usages start unchecked"
        );
        assert_eq!(p.groups[0].items[0].note, "def");
        assert_eq!(reasons.len(), 1);
        assert!(
            reasons[0]
                .0
                .starts_with("1 dynamic usage on objects of unknown or ambiguous type"),
            "{reasons:?}"
        );
    }
}
