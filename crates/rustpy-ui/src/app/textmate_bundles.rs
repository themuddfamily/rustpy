//! Settings › Editor › TextMate Bundles: grammars from TextMate bundles and
//! VS Code extensions highlight files RustPy has no built-in highlighter for
//! (plain text and unknown file types).
//!
//! The configured bundles are loaded into a [`Registry`] on a background
//! thread whenever the list in the settings changes. An editor of such a file
//! gets the grammar whose file associations match its name — the grammar's
//! `fileTypes`, or the `extensions` / `filenames` / `filenamePatterns` of the
//! language a VS Code `package.json` contributes the grammar for — or, failing
//! that, its first line (`firstLine`, `firstLineMatch`). Grammars are linked
//! (with the grammars they include) when a file first needs them.
//!
//! When the settings page is shown, another thread compiles the patterns of
//! each grammar on its own, one grammar at a time, to list those the regex
//! translation cannot express. Linked grammars embed the grammars they
//! include, so compiling every linked grammar would cost many times more
//! (a folder of 100 VS Code grammars: minutes and gigabytes instead of
//! seconds and megabytes).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustpy_plugins::GrammarContribution;
use rustpy_settings::editorconfig::glob_match;
use rustpy_settings::TextMateBundle;
use rustpy_textmate::{Grammar, RawGrammar, Registry, UnsupportedPattern};
use serde_json::Value;

use super::*;

/// File associations of one grammar.
#[derive(Clone, Debug, Default)]
struct Association {
    scope: String,
    /// Extensions without the leading dot, lower case (`rs`, `d.ts`).
    extensions: Vec<String>,
    /// Whole file names, lower case (`makefile`).
    file_names: Vec<String>,
    /// Globs on the file name, or on the path when they contain `/`.
    patterns: Vec<String>,
    /// VS Code `firstLine` regexes.
    first_line: Vec<regex::Regex>,
    /// The associations as written, for the settings page.
    labels: Vec<String>,
}

impl Association {
    /// How specifically this matches a file (higher wins), like VS Code: a
    /// file name beats a pattern, which beats an extension; longer patterns
    /// and extensions beat shorter ones (`d.ts` over `ts`).
    fn score(&self, file_name: &str, path: &str) -> Option<usize> {
        let lower = file_name.to_lowercase();
        if self.file_names.contains(&lower) {
            return Some(1_000_000);
        }
        let pattern = self
            .patterns
            .iter()
            .filter(|p| glob_match(p, if p.contains('/') { path } else { file_name }))
            .map(|p| p.len())
            .max();
        if let Some(len) = pattern {
            return Some(10_000 + len);
        }
        let extension = self
            .extensions
            .iter()
            .filter(|e| {
                lower.len() > e.len()
                    && lower.ends_with(e.as_str())
                    && lower.as_bytes()[lower.len() - e.len() - 1] == b'.'
            })
            .map(|e| e.len())
            .max();
        extension.map(|len| 1_000 + len)
    }

    /// Adds the associations' labels to `out` (once each).
    fn describe(&self, out: &mut Vec<String>) {
        for label in &self.labels {
            if !out.contains(label) {
                out.push(label.clone());
            }
        }
    }
}

/// A grammar's own `fileTypes` entries: extensions (`rs`) or whole file names
/// (`Makefile`, written capitalized by convention).
fn file_types_association(scope: &str, file_types: &[String]) -> Association {
    let entries: Vec<&str> = file_types
        .iter()
        .map(|t| t.trim_start_matches('.'))
        .filter(|t| !t.is_empty())
        .collect();
    let lower: Vec<String> = entries.iter().map(|t| t.to_lowercase()).collect();
    let labels = entries
        .iter()
        .map(|t| {
            if t.starts_with(|c: char| c.is_uppercase()) {
                t.to_string()
            } else {
                format!("*.{t}")
            }
        })
        .collect();
    Association {
        scope: scope.to_string(),
        extensions: lower.clone(),
        file_names: lower,
        labels,
        ..Default::default()
    }
}

/// A VS Code language's file associations for one of its grammars.
fn language_association(scope: &str, lang: &ManifestLanguage) -> Association {
    let extensions: Vec<&str> = lang
        .extensions
        .iter()
        .map(|e| e.trim_start_matches('.'))
        .filter(|e| !e.is_empty())
        .collect();
    let first_line: Vec<regex::Regex> = lang
        .first_line
        .iter()
        .filter_map(|re| regex::Regex::new(re).ok())
        .collect();
    let labels = extensions
        .iter()
        .map(|e| format!("*.{e}"))
        .chain(lang.filenames.iter().cloned())
        .chain(lang.patterns.iter().cloned())
        .chain((!first_line.is_empty()).then(|| "first line".to_string()))
        .collect();
    Association {
        scope: scope.to_string(),
        extensions: extensions.iter().map(|e| e.to_lowercase()).collect(),
        file_names: lang.filenames.iter().map(|n| n.to_lowercase()).collect(),
        patterns: lang.patterns.clone(),
        first_line,
        labels,
    }
}

/// A grammar of a configured bundle, as listed on the settings page.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GrammarInfo {
    scope: String,
    name: String,
    /// File associations (`*.rs`, `Makefile`, `first line`).
    files: Vec<String>,
}

/// What a configured bundle provided at the last load.
#[derive(Clone, Debug, Default)]
struct BundleReport {
    path: PathBuf,
    grammars: Vec<GrammarInfo>,
    errors: Vec<String>,
}

/// A language contributed by a VS Code extension (`contributes.languages`).
#[derive(Clone, Debug, Default)]
struct ManifestLanguage {
    extensions: Vec<String>,
    filenames: Vec<String>,
    patterns: Vec<String>,
    first_line: Option<String>,
    alias: Option<String>,
}

/// A grammar contributed by a VS Code extension (`contributes.grammars`).
#[derive(Clone, Debug)]
struct ManifestGrammar {
    language: Option<String>,
    scope: String,
    path: PathBuf,
}

/// The TextMate parts of a VS Code extension's `package.json`.
#[derive(Clone, Debug, Default)]
struct Manifest {
    languages: Vec<(String, ManifestLanguage)>,
    grammars: Vec<ManifestGrammar>,
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Reads `dir/package.json` when it contributes languages or grammars.
fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let contributes = value.get("contributes")?;
    let mut manifest = Manifest::default();
    for lang in contributes
        .get("languages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = lang.get("id").and_then(Value::as_str) else {
            continue;
        };
        manifest.languages.push((
            id.to_string(),
            ManifestLanguage {
                extensions: strings(lang.get("extensions")),
                filenames: strings(lang.get("filenames")),
                patterns: strings(lang.get("filenamePatterns")),
                first_line: lang
                    .get("firstLine")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                alias: strings(lang.get("aliases")).into_iter().next(),
            },
        ));
    }
    for grammar in contributes
        .get("grammars")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(scope), Some(path)) = (
            grammar.get("scopeName").and_then(Value::as_str),
            grammar.get("path").and_then(Value::as_str),
        ) else {
            continue;
        };
        manifest.grammars.push(ManifestGrammar {
            language: grammar
                .get("language")
                .and_then(Value::as_str)
                .map(str::to_string),
            scope: scope.to_string(),
            path: dir.join(path),
        });
    }
    (!manifest.languages.is_empty() || !manifest.grammars.is_empty()).then_some(manifest)
}

/// Folders of a bundle that may hold a VS Code `package.json`: the bundle and
/// its sub-folders (a folder of extensions such as `~/.vscode/extensions`).
fn manifest_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    children.sort();
    std::iter::once(dir.to_path_buf())
        .chain(children)
        .filter(|d| d.join("package.json").is_file())
        .collect()
}

/// Everything loaded from a list of bundles.
#[derive(Default)]
struct Loaded {
    registry: Registry,
    /// In bundle order (later ones win ties).
    associations: Vec<Association>,
    /// Scope → language name.
    names: HashMap<String, String>,
    /// One per configured bundle.
    reports: Vec<BundleReport>,
}

impl Loaded {
    /// Every loaded grammar (the one in use for each scope), for the
    /// unsupported-pattern check.
    fn raw_grammars(&self) -> Vec<RawGrammar> {
        let mut seen = std::collections::HashSet::new();
        self.reports
            .iter()
            .flat_map(|r| r.grammars.iter())
            .filter(|g| seen.insert(g.scope.clone()))
            .filter_map(|g| self.registry.raw(&g.scope).cloned())
            .collect()
    }
}

/// Loads one bundle (a folder or a grammar file) into `registry`.
fn load_bundle(
    registry: &mut Registry,
    path: &Path,
    scopes: &mut Vec<String>,
    manifests: &mut Vec<Manifest>,
    errors: &mut Vec<String>,
) {
    if path.is_file() {
        match registry.load_file(path) {
            Ok(scope) => scopes.push(scope),
            Err(e) => errors.push(e.to_string()),
        }
        return;
    }
    if !path.is_dir() {
        errors.push(format!("{} does not exist", path.display()));
        return;
    }
    let load = registry.load_bundle(path);
    scopes.extend(load.loaded);
    let relative = |file: &Path| {
        file.strip_prefix(path)
            .unwrap_or(file)
            .display()
            .to_string()
    };
    for (file, e) in load.errors {
        errors.push(format!("{}: {e}", relative(&file)));
    }
    for dir in manifest_dirs(path) {
        let Some(manifest) = read_manifest(&dir) else {
            continue;
        };
        // Grammars whose file names the scan does not recognize.
        for grammar in &manifest.grammars {
            if scopes.contains(&grammar.scope) {
                continue;
            }
            match registry.load_file(&grammar.path) {
                Ok(scope) => scopes.push(scope),
                Err(e) => errors.push(format!("{}: {e}", relative(&grammar.path))),
            }
        }
        manifests.push(manifest);
    }
}

/// Loads the grammars of enabled plugins into `registry`, each associated
/// with the file extensions its plugin declares (the grammar's own
/// `fileTypes` when it declares none) and named by the plugin's language
/// name. A grammar that cannot be loaded is logged and skipped.
fn load_plugin_grammars(
    registry: &mut Registry,
    grammars: &[GrammarContribution],
) -> Vec<(Association, String)> {
    let mut out = Vec::new();
    for g in grammars {
        let scope = match registry.load_file(&g.path) {
            Ok(scope) => scope,
            Err(e) => {
                log::warn!(
                    "plugin {}: TextMate grammar {}: {e}",
                    g.plugin_id,
                    g.path.display()
                );
                continue;
            }
        };
        let extensions: Vec<String> = g
            .extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        let association = if extensions.is_empty() {
            file_types_association(
                &scope,
                registry
                    .raw(&scope)
                    .map(|raw| raw.file_types())
                    .unwrap_or_default(),
            )
        } else {
            Association {
                scope,
                labels: extensions.iter().map(|e| format!("*.{e}")).collect(),
                extensions,
                ..Default::default()
            }
        };
        out.push((association, g.language.clone()));
    }
    out
}

/// Loads the grammars plugins contribute, then the enabled bundles (in
/// order, so later ones replace grammars with the same scope name and win
/// ties: the user's bundles take precedence over plugins).
fn load(bundles: &[TextMateBundle], plugin_grammars: &[GrammarContribution]) -> Loaded {
    let mut registry = Registry::new();
    let plugins = load_plugin_grammars(&mut registry, plugin_grammars);
    let mut reports = Vec::new();
    // Per bundle: the scopes it loaded and its manifests.
    let mut per_bundle: Vec<(Vec<String>, Vec<Manifest>)> = Vec::new();
    for bundle in bundles {
        let mut report = BundleReport {
            path: bundle.path.clone(),
            ..Default::default()
        };
        let mut scopes = Vec::new();
        let mut manifests = Vec::new();
        if bundle.enabled {
            load_bundle(
                &mut registry,
                &bundle.path,
                &mut scopes,
                &mut manifests,
                &mut report.errors,
            );
        }
        let mut seen = std::collections::HashSet::new();
        scopes.retain(|s| seen.insert(s.clone()));
        reports.push(report);
        per_bundle.push((scopes, manifests));
    }
    // Languages of every manifest by id (a language may get its file
    // associations from one extension and its grammar from another).
    let mut languages: HashMap<String, ManifestLanguage> = HashMap::new();
    for (id, lang) in per_bundle
        .iter()
        .flat_map(|(_, manifests)| manifests.iter().flat_map(|m| m.languages.iter()))
    {
        let merged = languages.entry(id.clone()).or_default();
        merged.extensions.extend(lang.extensions.iter().cloned());
        merged.filenames.extend(lang.filenames.iter().cloned());
        merged.patterns.extend(lang.patterns.iter().cloned());
        merged.first_line = lang.first_line.clone().or(merged.first_line.take());
        merged.alias = merged.alias.take().or(lang.alias.clone());
    }
    let mut loaded = Loaded::default();
    for (association, language) in plugins {
        loaded.names.insert(association.scope.clone(), language);
        loaded.associations.push(association);
    }
    for ((scopes, manifests), report) in per_bundle.iter().zip(&mut reports) {
        let mut bundle_assocs: Vec<Association> = Vec::new();
        for scope in scopes {
            let Some(raw) = registry.raw(scope) else {
                continue;
            };
            loaded
                .names
                .insert(scope.clone(), raw.name().unwrap_or(scope).to_string());
            bundle_assocs.push(file_types_association(scope, raw.file_types()));
        }
        for grammar in manifests.iter().flat_map(|m| m.grammars.iter()) {
            let Some(lang) = grammar.language.as_ref().and_then(|id| languages.get(id)) else {
                continue;
            };
            if !scopes.contains(&grammar.scope) {
                continue;
            }
            if let Some(alias) = &lang.alias {
                loaded.names.insert(grammar.scope.clone(), alias.clone());
            }
            bundle_assocs.push(language_association(&grammar.scope, lang));
        }
        for scope in scopes {
            let mut files = Vec::new();
            for assoc in bundle_assocs.iter().filter(|a| &a.scope == scope) {
                assoc.describe(&mut files);
            }
            if registry
                .raw(scope)
                .is_some_and(|raw| raw.first_line_match().is_some())
                && !files.iter().any(|f| f == "first line")
            {
                files.push("first line".into());
            }
            let name = loaded
                .names
                .get(scope)
                .cloned()
                .unwrap_or_else(|| scope.clone());
            report.grammars.push(GrammarInfo {
                scope: scope.clone(),
                name,
                files,
            });
        }
        loaded.associations.extend(bundle_assocs);
    }
    loaded.registry = registry;
    loaded.reports = reports;
    loaded
}

/// The grammars of the configured TextMate bundles.
#[derive(Default)]
pub(super) struct TextMateBundles {
    loaded: Loaded,
    /// Patterns each grammar cannot use, by scope (once checked).
    unsupported: HashMap<String, Vec<UnsupportedPattern>>,
    /// The bundle list last loaded (or being loaded).
    requested: Option<Vec<TextMateBundle>>,
    /// The load in progress.
    load_rx: Option<crossbeam_channel::Receiver<Loaded>>,
    /// A load finished without a thread (nothing enabled), not installed yet.
    ready: Option<Loaded>,
    /// The pattern check of the loaded grammars in progress.
    check_rx: Option<crossbeam_channel::Receiver<(String, Vec<UnsupportedPattern>)>>,
    /// Whether the pattern check of the loaded grammars has started.
    checked: bool,
    /// Settings page: the path being added and a problem with it.
    add_path: String,
    message: Option<String>,
    /// Grammars of the enabled plugins (loaded before the bundles).
    plugin_grammars: Vec<GrammarContribution>,
    /// The plugin host generation `plugin_grammars` come from.
    plugin_generation: Option<u64>,
}

impl TextMateBundles {
    pub(super) fn scratch_languages(&self) -> Vec<(String, String)> {
        let mut choices = Vec::new();
        let scopes = self.loaded.registry.scope_names();
        let mut counts = HashMap::new();
        for scope in &scopes {
            *counts
                .entry(crate::scratches::textmate_token(scope))
                .or_insert(0usize) += 1;
        }
        for scope in scopes {
            let token = crate::scratches::textmate_token(scope);
            if counts.get(&token) != Some(&1) {
                continue;
            }
            let name = self
                .loaded
                .names
                .get(scope)
                .map(String::as_str)
                .unwrap_or(scope);
            choices.push((name.to_string(), token));
        }
        choices.sort();
        choices.dedup();
        choices
    }
    fn explicit_scratch_scope(&self, token: &str) -> Option<&str> {
        let mut matching = self
            .loaded
            .registry
            .scope_names()
            .into_iter()
            .filter(|scope| crate::scratches::textmate_token(scope) == token);
        let scope = matching.next()?;
        if matching.next().is_some() {
            return None;
        }
        Some(scope)
    }

    pub(super) fn grammar_for_scratch(
        &self,
        state: &EditorState,
    ) -> Option<(Arc<Grammar>, String)> {
        self.grammar_for(state)
    }

    /// Uses the grammars of the enabled plugins: `grammars` is asked when the
    /// plugins changed (`generation`), and the grammars load again with the
    /// bundles when they differ from the ones in use — also when only a
    /// grammar file's contents changed (`sha256`), as in a plugin update.
    fn set_plugin_grammars(
        &mut self,
        generation: u64,
        grammars: impl FnOnce() -> Vec<GrammarContribution>,
    ) {
        if self.plugin_generation == Some(generation) {
            return;
        }
        self.plugin_generation = Some(generation);
        let grammars = grammars();
        if grammars != self.plugin_grammars {
            self.plugin_grammars = grammars;
            self.requested = None;
        }
    }

    /// Starts loading `bundles` when they differ from the last request and
    /// installs finished loads. Returns whether new grammars were installed
    /// (editors should pick their grammars again).
    fn sync(&mut self, bundles: &[TextMateBundle]) -> bool {
        if self.requested.as_deref() != Some(bundles) {
            self.request(bundles.to_vec());
        }
        self.poll()
    }

    fn request(&mut self, bundles: Vec<TextMateBundle>) {
        // Dropping the receiver lets a load still in progress finish unseen.
        self.load_rx = None;
        self.requested = Some(bundles.clone());
        let plugins = self.plugin_grammars.clone();
        if !bundles.iter().any(|b| b.enabled) && plugins.is_empty() {
            self.ready = Some(load(&bundles, &plugins));
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("textmate-bundles".into())
            .spawn(move || {
                let _ = tx.send(load(&bundles, &plugins));
            });
        match spawned {
            Ok(_) => self.load_rx = Some(rx),
            Err(e) => {
                log::warn!("cannot start loading TextMate bundles: {e}");
                self.ready = Some(Loaded::default());
            }
        }
    }

    fn poll(&mut self) -> bool {
        let mut installed = false;
        if let Some(loaded) = self.ready.take() {
            self.install(loaded);
            installed = true;
        }
        if let Some(rx) = &self.load_rx {
            match rx.try_recv() {
                Ok(loaded) => {
                    self.load_rx = None;
                    self.install(loaded);
                    installed = true;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                // The thread died: keep the grammars there are.
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.load_rx = None,
            }
        }
        if let Some(rx) = &self.check_rx {
            let mut results = Vec::new();
            loop {
                match rx.try_recv() {
                    Ok(result) => results.push(result),
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        self.check_rx = None;
                        break;
                    }
                }
            }
            self.unsupported.extend(results);
        }
        installed
    }

    fn install(&mut self, loaded: Loaded) {
        for report in &loaded.reports {
            for error in &report.errors {
                log::warn!("TextMate bundle {}: {error}", report.path.display());
            }
        }
        self.loaded = loaded;
        self.unsupported.clear();
        self.check_rx = None;
        self.checked = false;
    }

    /// Lists the patterns of every loaded grammar that the regex translation
    /// cannot express, on a background thread (once per load; for the
    /// settings page). Each grammar is compiled on its own, without the
    /// grammars it includes, and dropped again.
    fn start_check(&mut self) {
        if self.checked || self.loading() {
            return;
        }
        self.checked = true;
        let raws = self.loaded.raw_grammars();
        if raws.is_empty() {
            return;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("textmate-patterns".into())
            .spawn(move || {
                for raw in raws {
                    let unsupported = Grammar::from_raw(&raw).unsupported_patterns();
                    if tx
                        .send((raw.scope_name().to_string(), unsupported))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        match spawned {
            Ok(_) => self.check_rx = Some(rx),
            Err(e) => log::warn!("cannot start checking TextMate grammars: {e}"),
        }
    }

    fn loading(&self) -> bool {
        self.load_rx.is_some() || self.ready.is_some()
    }

    /// Whether bundles are loading or their patterns being checked.
    fn busy(&self) -> bool {
        self.loading() || self.check_rx.is_some()
    }

    /// The grammar for a file and its language name, by file name, else by
    /// first line.
    fn pick(
        &self,
        path: &Path,
        first_line: impl FnOnce() -> String,
    ) -> Option<(Arc<Grammar>, String)> {
        let file_name = path.file_name()?.to_str()?;
        if let Some(token) = path
            .extension()
            .and_then(|ext| ext.to_str())
            .filter(|ext| crate::scratches::is_textmate_token(ext))
        {
            let scope = self.explicit_scratch_scope(token)?;
            return Some((
                self.loaded.registry.grammar(scope)?,
                self.loaded
                    .names
                    .get(scope)
                    .cloned()
                    .unwrap_or_else(|| scope.to_string()),
            ));
        }
        let full = path.to_string_lossy().replace('\\', "/");
        let mut best: Option<(usize, &str)> = None;
        for assoc in &self.loaded.associations {
            if let Some(score) = assoc.score(file_name, &full) {
                if best.is_none_or(|(s, _)| score >= s) {
                    best = Some((score, &assoc.scope));
                }
            }
        }
        let scope = match best {
            Some((_, scope)) => scope.to_string(),
            // First-line detection only for files without an extension (scripts):
            // `notes.txt` must not become Raku because its first line says "my class".
            None if path.extension().is_some() => return None,
            None => {
                let line = first_line();
                let line = line.trim_end_matches(['\r', '\n']);
                match self
                    .loaded
                    .associations
                    .iter()
                    .rev()
                    .find(|a| a.first_line.iter().any(|re| re.is_match(line)))
                {
                    Some(assoc) => assoc.scope.clone(),
                    None => self.loaded.registry.scope_for_first_line(line)?.to_string(),
                }
            }
        };
        let grammar = self.loaded.registry.grammar(&scope)?;
        let name = self.loaded.names.get(&scope).cloned().unwrap_or(scope);
        Some((grammar, name))
    }

    /// The grammar for an editor whose file type has no built-in highlighter
    /// (plain text or unknown); `None` for other editors.
    fn grammar_for(&self, state: &EditorState) -> Option<(Arc<Grammar>, String)> {
        let path = state.path.as_deref()?;
        let base = state.base_language();
        // Editors opened with another language than their file name gives
        // (hex dumps of binary files) keep it.
        if !matches!(base, Language::PlainText | Language::Other)
            || Language::from_path(path) != base
            || path.starts_with("<typeshed>")
        {
            return None;
        }
        // Hex dumps of binary files (read-only, `00000000  ...` lines) whose name
        // also maps to plain text (`trace.log`, extension-less) stay plain.
        if state.doc.is_read_only() && state.doc.line_text(0).starts_with("00000000  ") {
            return None;
        }
        self.pick(path, || state.doc.line_text(0))
    }

    /// The settings page's part for one bundle: its grammars and errors.
    fn show_report(&self, ui: &mut egui::Ui, theme: &UiTheme, bundle: &TextMateBundle) {
        let dim = |text: String| egui::RichText::new(text).color(theme.text_dim);
        if !bundle.enabled {
            ui.label(dim("Disabled".into()));
            return;
        }
        let Some(report) = self.loaded.reports.iter().find(|r| r.path == bundle.path) else {
            ui.label(dim(if self.loading() {
                "Loading…".into()
            } else {
                "Not loaded".into()
            }));
            return;
        };
        if report.grammars.is_empty() && report.errors.is_empty() {
            ui.label(dim("No grammars found (*.tmLanguage, *.tmLanguage.json, *.tmGrammar.json or package.json grammars)".into()));
        }
        if !report.grammars.is_empty() {
            egui::Grid::new(("textmate_grammars", &report.path))
                .num_columns(4)
                .spacing([14.0, 2.0])
                .show(ui, |ui| {
                    for g in &report.grammars {
                        ui.label(egui::RichText::new(&g.name).color(theme.text));
                        ui.label(dim(g.scope.clone()));
                        let files = if g.files.is_empty() {
                            "embedded in other grammars only".to_string()
                        } else {
                            g.files.join(", ")
                        };
                        // Long association lists (VS Code's XML: hundreds of names) must not
                        // widen the Settings window: shorten, with the full list on hover.
                        let short: String = if files.chars().count() > 60 {
                            files.chars().take(57).collect::<String>() + "…"
                        } else {
                            files.clone()
                        };
                        ui.label(dim(short)).on_hover_text(files);
                        match self.unsupported.get(&g.scope) {
                            None if self.check_rx.is_some() => {
                                ui.label(dim("checking patterns…".into()));
                            }
                            None => {
                                ui.label("");
                            }
                            Some(list) if list.is_empty() => {
                                ui.label(dim("all patterns supported".into()));
                            }
                            Some(list) => {
                                let details: Vec<String> = list
                                    .iter()
                                    .take(15)
                                    .map(|p| {
                                        format!(
                                            "{} — {}",
                                            p.pattern.chars().take(80).collect::<String>(),
                                            p.reason
                                        )
                                    })
                                    .collect();
                                let more = if list.len() > 15 {
                                    format!("\n… and {} more", list.len() - 15)
                                } else {
                                    String::new()
                                };
                                let text = format!(
                                    "{} unsupported pattern{} (skipped)",
                                    list.len(),
                                    if list.len() == 1 { "" } else { "s" }
                                );
                                ui.label(egui::RichText::new(text).color(theme.warning))
                                    .on_hover_text(format!("{}{more}", details.join("\n")));
                            }
                        }
                        ui.end_row();
                    }
                });
        }
        for error in &report.errors {
            ui.label(egui::RichText::new(error).color(theme.error));
        }
    }
}

/// The path of a bundle to add from the settings page's input.
fn bundle_path_from_input(input: &str, existing: &[TextMateBundle]) -> Result<PathBuf, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter the path of a bundle folder or of a grammar file".into());
    }
    let path = match input.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(input),
    };
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let path = dunce::canonicalize(&path).unwrap_or(path);
    if existing.iter().any(|b| b.path == path) {
        return Err(format!("{} is already in the list", path.display()));
    }
    Ok(path)
}

impl IdeApp {
    /// Loads the TextMate bundles when the list in the settings changed and
    /// gives open editors the grammars of the new set (every frame).
    pub(super) fn sync_textmate_bundles(&mut self, ctx: &egui::Context) {
        self.settings_ui.textmate.set_plugin_grammars(
            crate::plugin_host::generation(),
            crate::plugin_host::textmate_grammars,
        );
        // `rustpy --disable-plugins`: no user-installed grammars this session.
        let bundles: &[TextMateBundle] = if rustpy_settings::plugins_disabled() {
            &[]
        } else {
            &self.settings.textmate_bundles
        };
        if self.settings_ui.textmate.sync(bundles) {
            let bundles = &self.settings_ui.textmate;
            for tab in &mut self.tabs {
                if tab.image.is_none() && tab.notebook.is_none() {
                    tab.state
                        .set_textmate_grammar(bundles.grammar_for(&tab.state));
                }
            }
        }
        if self.settings_ui.textmate.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Gives a newly opened editor the TextMate grammar for its file, if it
    /// has no built-in highlighter and a bundle provides one.
    pub(super) fn attach_textmate_grammar(&self, state: &mut EditorState) {
        if let Some(found) = self.settings_ui.textmate.grammar_for(state) {
            state.set_textmate_grammar(Some(found));
        }
    }

    pub(super) fn page_textmate_bundles(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        ui.heading("TextMate Bundles");
        ui.label(egui::RichText::new("Grammars from TextMate bundles (.tmbundle), VS Code extensions (a folder with package.json and syntaxes/), folders of either, or single .tmLanguage / .tmLanguage.json files highlight the files RustPy has no built-in highlighter for: plain text and unknown file types. When two bundles provide the same language, the later one wins.").color(theme.text_dim).small());
        ui.add_space(6.0);
        let tm = &mut self.settings_ui.textmate;
        tm.start_check();
        let mut add = false;
        ui.horizontal(|ui| {
            let input = ui.add(
                egui::TextEdit::singleline(&mut tm.add_path)
                    .hint_text("Bundle folder or grammar file")
                    .desired_width(380.0),
            );
            add = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            add |= ui.button("Add").clicked();
            if ui
                .button("Reload")
                .on_hover_text("Load every bundle again from disk")
                .clicked()
            {
                tm.requested = None;
            }
            if tm.busy() {
                crate::a11y::spinner(ui);
            }
        });
        if add {
            match bundle_path_from_input(&tm.add_path, &self.settings.textmate_bundles) {
                Ok(path) => {
                    self.settings.textmate_bundles.push(TextMateBundle {
                        path,
                        enabled: true,
                    });
                    tm.add_path.clear();
                    tm.message = None;
                }
                Err(message) => tm.message = Some(message),
            }
        }
        if let Some(message) = &tm.message {
            ui.label(egui::RichText::new(message).color(theme.error));
        }
        ui.add_space(6.0);
        if self.settings.textmate_bundles.is_empty() {
            ui.label(egui::RichText::new("No bundles added.").color(theme.text_dim));
        }
        let mut remove = None;
        for (i, bundle) in self.settings.textmate_bundles.iter_mut().enumerate() {
            egui::Frame::new()
                .fill(theme.hover_bg)
                .inner_margin(egui::Margin::same(6))
                .corner_radius(4)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut bundle.enabled, "")
                            .on_hover_text("Use this bundle");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Remove").clicked() {
                                remove = Some(i);
                            }
                            // The path takes the rest of the row (shortened when long).
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let path = bundle.path.display().to_string();
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&path).strong().color(theme.text),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(path);
                                },
                            );
                        });
                    });
                    tm.show_report(ui, &theme, bundle);
                });
            ui.add_space(4.0);
        }
        if let Some(i) = remove {
            self.settings.textmate_bundles.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch folder under the system temp directory.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rustpy-textmate-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn grammar(scope: &str, name: &str, file_types: &[&str], extra: &str) -> String {
        let types: Vec<String> = file_types.iter().map(|t| format!("\"{t}\"")).collect();
        format!(
            r#"{{ "scopeName": "{scope}", "name": "{name}", "fileTypes": [{}] {extra}, "patterns": [ {{ "match": "\\b(?:if|fn)\\b", "name": "keyword.control.x" }} ] }}"#,
            types.join(", ")
        )
    }

    fn bundle(path: &Path) -> TextMateBundle {
        TextMateBundle {
            path: path.to_path_buf(),
            enabled: true,
        }
    }

    #[test]
    fn explicit_scratch_grammars_cover_unassociated_filename_firstline_and_long_scopes() {
        let root = tempfile::tempdir().unwrap();
        let mut list = Vec::new();
        let scopes = [
            "source.noassociation".to_string(),
            "source.filename".to_string(),
            "source.firstline".to_string(),
            format!("source.{}", "long".repeat(80)),
        ];
        for (index, scope) in scopes.iter().enumerate() {
            let path = root.path().join(format!("grammar{index}.tmLanguage.json"));
            let file_types = if index == 1 {
                vec!["Specialfile"]
            } else {
                vec![]
            };
            let extra = if index == 2 {
                r#", "firstLineMatch": "^#!special$""#
            } else {
                ""
            };
            write(
                &path,
                &grammar(scope, &format!("Grammar {index}"), &file_types, extra),
            );
            list.push(bundle(&path));
        }
        let mut app = IdeApp::new(Settings::default(), None);
        app.settings_ui.textmate = loaded(&list);
        let choices = app.settings_ui.textmate.scratch_languages();
        assert_eq!(choices.len(), scopes.len());
        let dir = root.path().join("scratches");
        for (index, scope) in scopes.iter().enumerate() {
            let token = crate::scratches::textmate_token(scope);
            assert!(choices.contains(&(format!("Grammar {index}"), token.clone())));
            let path = crate::scratches::create(&dir, &token).unwrap();
            std::fs::write(&path, "if π🙂\n").unwrap();
            app.open_file(&path);
            let state = &app.tabs[app.active.unwrap()].state;
            assert_eq!(state.textmate_grammar().unwrap().scope_name(), scope);
            assert_eq!(state.language_label(), format!("Grammar {index}"));
            let plain = crate::scratches::change_language(&path, "txt").unwrap();
            app.tabs.clear();
            app.open_file(&plain);
            assert!(app.tabs[app.active.unwrap()]
                .state
                .textmate_grammar()
                .is_none());
            app.change_scratch_language(&plain, &token);
            let changed = app.tabs[app.active.unwrap()].state.path.clone().unwrap();
            assert_eq!(
                app.tabs[app.active.unwrap()]
                    .state
                    .textmate_grammar()
                    .unwrap()
                    .scope_name(),
                scope
            );
            app.tabs.clear();
            // A freshly loaded registry and ordinary open_file restore the scope.
            app.settings_ui.textmate = loaded(&list);
            app.open_file(&changed);
            assert_eq!(
                app.tabs[app.active.unwrap()]
                    .state
                    .textmate_grammar()
                    .unwrap()
                    .scope_name(),
                scope
            );
            assert_eq!(std::fs::read_to_string(&changed).unwrap(), "if π🙂\n");
            assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), "if π🙂\n");
            app.tabs.clear();
        }
        let buffer = crate::scratches::next_buffer(&dir).unwrap();
        let token = crate::scratches::textmate_token(&scopes[3]);
        let changed = crate::scratches::change_language(&buffer, &token).unwrap();
        for _ in 1..crate::scratches::BUFFER_COUNT {
            crate::scratches::next_buffer(&dir).unwrap();
        }
        assert_eq!(crate::scratches::next_buffer(&dir).unwrap(), changed);
        assert!(picked(&app.settings_ui.textmate, "/p/a.rustpy-tm-deadbeef", "").is_none());
    }

    fn picked(bundles: &TextMateBundles, path: &str, first_line: &str) -> Option<(String, String)> {
        bundles
            .pick(Path::new(path), || first_line.to_string())
            .map(|(g, name)| (g.scope_name().to_string(), name))
    }

    fn loaded(list: &[TextMateBundle]) -> TextMateBundles {
        let mut bundles = TextMateBundles::default();
        bundles.install(load(list, &[]));
        bundles
    }

    fn plugin_grammar(path: &Path, extensions: &[&str]) -> GrammarContribution {
        GrammarContribution {
            plugin_id: "com.example.toy".into(),
            path: path.to_path_buf(),
            language: "Toy".into(),
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            scope_name: "source.toy".into(),
            sha256: "0".repeat(64),
        }
    }

    #[test]
    fn plugin_grammars_load_by_their_declared_extensions_and_lose_to_bundles() {
        let dir = scratch("plugins");
        // No fileTypes: the plugin manifest's extensions associate the grammar.
        let toy = dir.join("plugin/syntaxes/toy.tmLanguage.json");
        write(&toy, &grammar("source.toy", "Toy grammar", &[], ""));
        let mut bundles = TextMateBundles::default();
        bundles.install(load(&[], &[plugin_grammar(&toy, &["toy", ".tY"])]));
        assert_eq!(
            picked(&bundles, "/p/a.toy", ""),
            Some(("source.toy".into(), "Toy".into()))
        );
        assert_eq!(
            picked(&bundles, "/p/a.TY", ""),
            Some(("source.toy".into(), "Toy".into()))
        );
        assert_eq!(picked(&bundles, "/p/a.txt", ""), None);
        // Without declared extensions, the grammar's fileTypes count.
        let typed = dir.join("plugin/syntaxes/typed.tmLanguage.json");
        write(&typed, &grammar("source.typed", "Typed", &["typ"], ""));
        bundles.install(load(&[], &[plugin_grammar(&typed, &[])]));
        assert_eq!(
            picked(&bundles, "/p/a.typ", ""),
            Some(("source.typed".into(), "Toy".into()))
        );
        // A bundle of the user's for the same extension wins over the plugin.
        let user = dir.join("user.tmLanguage.json");
        write(&user, &grammar("source.mytoy", "My Toy", &["toy"], ""));
        bundles.install(load(&[bundle(&user)], &[plugin_grammar(&toy, &["toy"])]));
        assert_eq!(
            picked(&bundles, "/p/a.toy", ""),
            Some(("source.mytoy".into(), "My Toy".into()))
        );
        // A plugin grammar that does not load is skipped; the bundles still load.
        let broken = dir.join("plugin/syntaxes/broken.tmLanguage.json");
        write(&broken, "{ nope");
        bundles.install(load(&[bundle(&user)], &[plugin_grammar(&broken, &["toy"])]));
        assert_eq!(
            picked(&bundles, "/p/a.toy", ""),
            Some(("source.mytoy".into(), "My Toy".into()))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn plugin_changes_reload_only_when_their_grammars_changed() {
        let g = plugin_grammar(Path::new("/p/toy.tmLanguage.json"), &["toy"]);
        let mut bundles = TextMateBundles {
            requested: Some(Vec::new()),
            ..TextMateBundles::default()
        };
        bundles.set_plugin_grammars(1, Vec::new);
        assert!(
            bundles.requested.is_some(),
            "no plugin grammars: nothing to reload"
        );
        bundles.set_plugin_grammars(2, || vec![g.clone()]);
        assert!(
            bundles.requested.is_none(),
            "new plugin grammars load again"
        );
        bundles.requested = Some(Vec::new());
        bundles.set_plugin_grammars(2, || unreachable!("the same generation is not asked again"));
        bundles.set_plugin_grammars(3, || vec![g.clone()]);
        assert!(
            bundles.requested.is_some(),
            "a plugin change without grammar changes reloads nothing"
        );
        // An update keeps the grammar's path; its contents changed.
        bundles.requested = Some(Vec::new());
        let updated = GrammarContribution {
            sha256: "1".repeat(64),
            ..g.clone()
        };
        bundles.set_plugin_grammars(4, || vec![updated.clone()]);
        assert!(
            bundles.requested.is_none(),
            "an updated grammar loads again"
        );
        bundles.set_plugin_grammars(5, Vec::new);
        assert!(
            bundles.requested.is_none() && bundles.plugin_grammars.is_empty(),
            "a disabled plugin's grammar goes"
        );
    }

    /// Review finding: a plugin update kept highlighting with the old grammar
    /// until a restart, because the updated plugin names the same grammar file.
    #[test]
    fn a_plugin_update_brings_its_new_grammar() {
        let dir = scratch("plugin-update");
        let config = dir.join("config");
        let plugin = |version: &str, keyword: &str| {
            let src = dir.join(format!("src-{version}"));
            let manifest = serde_json::json!({"id": "com.example.toy", "name": "Toy", "version": version,
                "contributes": {"textmate_grammars": [{"language": "Toy", "extensions": ["toy"], "path": "syntaxes/toy.tmLanguage.json"}]}});
            write(&src.join("plugin.json"), &manifest.to_string());
            let grammar = serde_json::json!({"scopeName": "source.toy", "patterns": [{"match": format!("\\b{keyword}\\b"), "name": "keyword.control.toy"}]});
            write(
                &src.join("syntaxes/toy.tmLanguage.json"),
                &grammar.to_string(),
            );
            src
        };
        // What the editor highlights as keywords in a `.toy` file once the grammars of `generation` are in.
        let keywords = |bundles: &mut TextMateBundles,
                        generation: u64,
                        registry: &rustpy_plugins::PluginRegistry| {
            bundles.set_plugin_grammars(generation, || registry.contributions().textmate_grammars);
            let started = std::time::Instant::now();
            while !bundles.sync(&[]) {
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(20),
                    "the grammars never loaded"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let (grammar, language) = bundles
                .pick(Path::new("/p/a.toy"), String::new)
                .expect("the plugin grammar");
            assert_eq!(language, "Toy");
            let line = "let x = 1";
            let tokens = grammar.tokenize_line(line, &mut rustpy_textmate::StackState::new());
            tokens
                .iter()
                .filter(|t| t.scopes.iter().any(|s| s.starts_with("keyword")))
                .map(|t| line[t.range.clone()].to_string())
                .collect::<Vec<_>>()
        };
        let mut registry = rustpy_plugins::PluginRegistry::load(&config);
        registry.install_from_dir(&plugin("1.2.0", "let")).unwrap();
        let mut bundles = TextMateBundles::default();
        assert_eq!(keywords(&mut bundles, 1, &registry), ["let"]);
        registry.install_from_dir(&plugin("1.3.0", "x")).unwrap();
        assert_eq!(
            registry.get("com.example.toy").unwrap().manifest.version,
            "1.3.0"
        );
        assert_eq!(
            keywords(&mut bundles, 2, &registry),
            ["x"],
            "the updated grammar is used without a restart"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn tmbundle_grammars_are_picked_by_file_types_and_first_line() {
        let dir = scratch("tmbundle");
        let b = dir.join("Zap.tmbundle");
        write(
            &b.join("Syntaxes/Zap.tmLanguage.json"),
            &grammar(
                "source.zap",
                "Zap",
                &["zap", "Zapfile"],
                r#", "firstLineMatch": "^#!.*\\bzap\\b""#,
            ),
        );
        write(&b.join("Syntaxes/broken.tmLanguage.json"), "{ not json");
        let bundles = loaded(&[bundle(&b)]);
        assert_eq!(
            picked(&bundles, "/p/main.zap", ""),
            Some(("source.zap".into(), "Zap".into()))
        );
        assert_eq!(
            picked(&bundles, "/p/MAIN.ZAP", ""),
            Some(("source.zap".into(), "Zap".into()))
        );
        assert_eq!(
            picked(&bundles, "/p/Zapfile", ""),
            Some(("source.zap".into(), "Zap".into()))
        );
        assert_eq!(
            picked(&bundles, "/p/script", "#!/usr/bin/env zap"),
            Some(("source.zap".into(), "Zap".into()))
        );
        assert_eq!(picked(&bundles, "/p/script", "#!/bin/sh"), None);
        assert_eq!(picked(&bundles, "/p/main.zapx", ""), None);
        let report = &bundles.loaded.reports[0];
        assert_eq!(
            report.grammars,
            vec![GrammarInfo {
                scope: "source.zap".into(),
                name: "Zap".into(),
                files: vec!["*.zap".into(), "Zapfile".into(), "first line".into()]
            }]
        );
        assert_eq!(report.errors.len(), 1);
        assert!(
            report.errors[0]
                .replace('\\', "/")
                .starts_with("Syntaxes/broken.tmLanguage.json: invalid JSON grammar"),
            "{:?}",
            report.errors
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn vscode_extensions_associate_grammars_through_package_json() {
        let dir = scratch("vscode");
        // A folder of extensions, like ~/.vscode/extensions.
        let ext = dir.join("rust-lang.rust-1.0");
        write(
            &ext.join("syntaxes/rust.tmLanguage.json"),
            &grammar("source.rust", "Rust grammar", &[], ""),
        );
        // Grammar files with other names are loaded through package.json.
        write(
            &ext.join("syntaxes/kconf.json"),
            &grammar("source.kconfig", "kconfig", &[], ""),
        );
        write(
            &ext.join("package.json"),
            r#"{ "name": "rust", "contributes": {
                "languages": [
                    { "id": "rust", "extensions": [".rs"], "aliases": ["Rust", "rust"] },
                    { "id": "kconfig", "filenames": ["Kconfig"], "filenamePatterns": ["Kconfig.*", "**/configs/*_defconfig"], "firstLine": "^#\\s*kconfig" }
                ],
                "grammars": [
                    { "language": "rust", "scopeName": "source.rust", "path": "./syntaxes/rust.tmLanguage.json" },
                    { "language": "kconfig", "scopeName": "source.kconfig", "path": "./syntaxes/kconf.json" },
                    { "scopeName": "source.injection", "path": "./syntaxes/missing.json", "injectTo": ["source.rust"] }
                ] } }"#,
        );
        let bundles = loaded(&[bundle(&dir)]);
        assert_eq!(
            picked(&bundles, "/p/src/lib.rs", ""),
            Some(("source.rust".into(), "Rust".into()))
        );
        assert_eq!(
            picked(&bundles, "/p/Kconfig", ""),
            Some(("source.kconfig".into(), "kconfig".into()))
        );
        assert_eq!(
            picked(&bundles, "/p/Kconfig.debug", ""),
            Some(("source.kconfig".into(), "kconfig".into()))
        );
        assert_eq!(
            picked(&bundles, "/linux/arch/configs/x86_defconfig", ""),
            Some(("source.kconfig".into(), "kconfig".into()))
        );
        assert_eq!(picked(&bundles, "/p/defconfig", ""), None);
        assert_eq!(
            picked(&bundles, "/p/notes", "# kconfig for the board"),
            Some(("source.kconfig".into(), "kconfig".into()))
        );
        let report = &bundles.loaded.reports[0];
        let names: Vec<(&str, &str)> = report
            .grammars
            .iter()
            .map(|g| (g.scope.as_str(), g.name.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![("source.rust", "Rust"), ("source.kconfig", "kconfig")]
        );
        assert_eq!(report.grammars[0].files, vec!["*.rs".to_string()]);
        assert_eq!(
            report.grammars[1].files,
            vec![
                "Kconfig".to_string(),
                "Kconfig.*".into(),
                "**/configs/*_defconfig".into(),
                "first line".into()
            ]
        );
        // The injection grammar's file is missing.
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(report.errors[0].contains("missing.json"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn later_bundles_win_and_disabled_or_missing_ones_load_nothing() {
        let dir = scratch("order");
        let a = dir.join("a.tmLanguage.json");
        let b = dir.join("b.tmLanguage.json");
        write(&a, &grammar("source.one", "One A", &["one"], ""));
        write(&b, &grammar("source.one", "One B", &["one"], ""));
        let both = loaded(&[bundle(&a), bundle(&b)]);
        assert_eq!(
            picked(&both, "x.one", ""),
            Some(("source.one".into(), "One B".into()))
        );
        let off = loaded(&[
            bundle(&a),
            TextMateBundle {
                path: b.clone(),
                enabled: false,
            },
        ]);
        assert_eq!(
            picked(&off, "x.one", ""),
            Some(("source.one".into(), "One A".into()))
        );
        assert!(off.loaded.reports[1].grammars.is_empty());
        let missing = loaded(&[bundle(&dir.join("nope"))]);
        assert!(missing.loaded.reports[0].errors[0].ends_with("does not exist"));
        assert_eq!(picked(&missing, "x.one", ""), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn longer_extensions_and_whole_names_take_precedence() {
        let assoc = |scope: &str, extensions: &[&str], names: &[&str]| Association {
            scope: scope.into(),
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
            file_names: names.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let types = file_types_association(
            "source.x",
            &["x".into(), ".y".into(), "Xfile".into(), String::new()],
        );
        assert_eq!(
            types.labels,
            vec!["*.x".to_string(), "*.y".into(), "Xfile".into()]
        );
        assert_eq!(
            types.extensions,
            vec!["x".to_string(), "y".into(), "xfile".into()]
        );
        let ts = assoc("source.ts", &["ts"], &[]);
        let dts = assoc("source.dts", &["d.ts"], &[]);
        assert!(dts.score("x.d.ts", "/x.d.ts") > ts.score("x.d.ts", "/x.d.ts"));
        assert_eq!(dts.score("x.ts", "/x.ts"), None);
        let make = assoc("source.make", &["mk"], &["makefile"]);
        assert!(make.score("Makefile", "/Makefile") > dts.score("x.d.ts", "/x.d.ts"));
        // A dot file named like an extension.
        let ignore = assoc("source.ignore", &["gitignore"], &[]);
        assert!(ignore.score(".gitignore", "/.gitignore").is_some());
        assert_eq!(ignore.score("gitignore", "/gitignore"), None);
    }

    #[test]
    fn editors_with_built_in_highlighting_or_another_language_get_no_grammar() {
        let dir = scratch("eligible");
        let g = dir.join("all.tmLanguage.json");
        write(
            &g,
            &grammar(
                "source.all",
                "All",
                &["py", "txt", "bin", "zz"],
                r#", "firstLineMatch": "^#!""#,
            ),
        );
        let bundles = loaded(&[bundle(&g)]);
        let editor = |path: &str, text: &str| {
            EditorState::new(Document::new(text), Some(PathBuf::from(path)))
        };
        assert!(bundles.grammar_for(&editor("/p/a.py", "")).is_none());
        assert!(bundles.grammar_for(&editor("/p/a.zz", "")).is_some());
        assert!(bundles.grammar_for(&editor("/p/notes.txt", "")).is_some());
        assert!(bundles
            .grammar_for(&editor("/p/run", "#!/bin/sh"))
            .is_some());
        // A binary file shown as a hex dump (plain text, unlike its name).
        let mut hex = EditorState::from_text("00000000  7f 45", Language::PlainText);
        hex.path = Some(PathBuf::from("/p/a.bin"));
        assert!(bundles.grammar_for(&hex).is_none());
        // Untitled editors have no file name.
        assert!(bundles
            .grammar_for(&EditorState::from_text("#!", Language::PlainText))
            .is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn attaching_a_grammar_switches_the_language_and_back() {
        let dir = scratch("attach");
        let g = dir.join("zap.tmLanguage.json");
        write(&g, &grammar("source.zap", "Zap", &["zap"], ""));
        let bundles = loaded(&[bundle(&g)]);
        let mut state = EditorState::new(Document::new("if x\n"), Some(PathBuf::from("/p/a.zap")));
        assert_eq!(state.language, Language::Other);
        assert_eq!(state.language_label(), "Text");
        let found = bundles.grammar_for(&state);
        state.set_textmate_grammar(found);
        assert_eq!(state.language_label(), "Zap");
        let spans = state.highlight_spans().to_vec();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].tag, rustpy_highlight::HighlightTag::Keyword);
        let start = std::time::Instant::now();
        loop {
            let (view_spans, pending) = state.view_highlight_spans(0, 10);
            if !pending {
                assert_eq!(view_spans, spans);
                break;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(30));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // Picking again (bundle reload with the same grammar) keeps the state.
        let again = bundles.grammar_for(&state);
        assert!(again.is_some());
        state.set_textmate_grammar(again);
        assert_eq!(state.base_language(), Language::Other);
        // A plain-text file goes back to plain text (and its checks) without.
        let mut notes = EditorState::new(Document::new("if\n"), Some(PathBuf::from("/p/Zapfile")));
        assert_eq!(notes.language, Language::PlainText);
        notes.set_textmate_grammar(Some((
            bundles.loaded.registry.grammar("source.zap").unwrap(),
            "Zap".into(),
        )));
        assert_eq!(notes.language, Language::Other);
        assert_eq!(notes.base_language(), Language::PlainText);
        notes.set_textmate_grammar(None);
        assert_eq!(notes.language, Language::PlainText);
        assert_eq!(notes.language_label(), "Plain text");
        assert!(notes.textmate_grammar().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn loads_in_the_background_and_reports_unsupported_patterns() {
        let dir = scratch("background");
        let g = dir.join("odd.tmLanguage.json");
        // A back-reference to a group with unbounded text cannot be expressed.
        write(
            &g,
            r#"{ "scopeName": "source.odd", "fileTypes": ["odd"], "patterns": [ { "match": "(\\w+)-\\1", "name": "keyword.x" }, { "match": "\\bok\\b", "name": "keyword.y" } ] }"#,
        );
        let mut bundles = TextMateBundles::default();
        // Nothing configured: installs an empty set at once.
        assert!(bundles.sync(&[]));
        assert!(!bundles.sync(&[]));
        assert!(!bundles.busy());
        let list = vec![bundle(&g)];
        let wait = |bundles: &mut TextMateBundles| {
            let mut installed = false;
            let start = std::time::Instant::now();
            loop {
                installed |= bundles.sync(&list);
                if !bundles.busy() || start.elapsed() > std::time::Duration::from_secs(20) {
                    return installed;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        assert!(wait(&mut bundles));
        assert!(!bundles.busy());
        assert_eq!(
            picked(&bundles, "a.odd", ""),
            Some(("source.odd".into(), "source.odd".into()))
        );
        // Patterns are checked once asked for (by the settings page).
        assert!(bundles.unsupported.is_empty());
        bundles.start_check();
        assert!(!wait(&mut bundles));
        let unsupported = &bundles.unsupported["source.odd"];
        assert_eq!(unsupported.len(), 1, "{unsupported:?}");
        assert_eq!(unsupported[0].pattern, "(\\w+)-\\1");
        // Reload (forget the request) loads again.
        bundles.requested = None;
        let started = bundles.sync(&list) || bundles.busy();
        assert!(started);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bundle_paths_are_checked_before_they_are_added() {
        let dir = scratch("input");
        assert!(bundle_path_from_input("  ", &[]).is_err());
        assert!(
            bundle_path_from_input(&dir.join("missing").display().to_string(), &[])
                .unwrap_err()
                .ends_with("does not exist")
        );
        let path = bundle_path_from_input(&format!(" {} ", dir.display()), &[]).unwrap();
        assert_eq!(path, dunce::canonicalize(&dir).unwrap());
        assert!(
            bundle_path_from_input(&dir.display().to_string(), &[bundle(&path)])
                .unwrap_err()
                .ends_with("already in the list")
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
