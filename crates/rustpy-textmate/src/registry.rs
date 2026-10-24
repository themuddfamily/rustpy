//! A set of grammars keyed by scope name; grammars obtained from it resolve
//! includes of each other (`"include": "source.css"`).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::grammar::{link, Grammar};
use crate::matcher::{Matcher, SearchCtx};
use crate::raw::{GrammarError, RawGrammar};

/// Directory levels [`Registry::load_bundle`] descends.
const MAX_BUNDLE_DEPTH: usize = 8;

/// File name suffixes recognized as grammars in bundles (case-insensitive).
const GRAMMAR_SUFFIXES: [&str; 4] = [
    ".tmlanguage",
    ".tmlanguage.json",
    ".tmgrammar.json",
    ".tmlanguage.plist",
];

/// Grammars by scope name.
#[derive(Default)]
pub struct Registry {
    grammars: BTreeMap<String, Arc<RawGrammar>>,
    /// Linked grammars, dropped whenever the set changes.
    linked: Mutex<HashMap<String, Arc<Grammar>>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("scopes", &self.grammars.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Result of [`Registry::load_bundle`].
#[derive(Debug, Default)]
pub struct BundleLoad {
    /// Scope names of the grammars added.
    pub loaded: Vec<String>,
    /// Grammar files that could not be loaded.
    pub errors: Vec<(PathBuf, GrammarError)>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Adds (or replaces) a grammar; returns its scope name.
    pub fn add(&mut self, grammar: RawGrammar) -> String {
        let scope = grammar.scope_name().to_string();
        self.grammars.insert(scope.clone(), Arc::new(grammar));
        self.invalidate();
        scope
    }

    /// Adds a `.tmLanguage.json` grammar.
    pub fn add_json(&mut self, src: &str) -> Result<String, GrammarError> {
        Ok(self.add(RawGrammar::from_json(src)?))
    }

    /// Adds an XML plist (`.tmLanguage`) grammar.
    pub fn add_plist(&mut self, src: &str) -> Result<String, GrammarError> {
        Ok(self.add(RawGrammar::from_plist(src)?))
    }

    /// Adds a grammar file: JSON for `*.json`, otherwise sniffed from the
    /// content.
    pub fn load_file(&mut self, path: &Path) -> Result<String, GrammarError> {
        let src = std::fs::read_to_string(path).map_err(|e| GrammarError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let is_json = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        let raw = if is_json {
            RawGrammar::from_json(&src)?
        } else {
            RawGrammar::parse(&src)?
        };
        Ok(self.add(raw))
    }

    /// Adds every grammar file (`*.tmLanguage`, `*.tmLanguage.json`,
    /// `*.tmGrammar.json`) under `dir`: a TextMate `.tmbundle` (grammars in
    /// `Syntaxes/`) or a VS Code extension (`syntaxes/`). Files load in path
    /// order, so for duplicate scope names the last one wins.
    pub fn load_bundle(&mut self, dir: &Path) -> BundleLoad {
        let mut files = Vec::new();
        let mut result = BundleLoad::default();
        collect_grammar_files(dir, 0, &mut files, &mut result.errors);
        files.sort();
        for file in files {
            match self.load_file(&file) {
                Ok(scope) => result.loaded.push(scope),
                Err(e) => result.errors.push((file, e)),
            }
        }
        result
    }

    /// Removes a grammar; returns whether it was present.
    pub fn remove(&mut self, scope_name: &str) -> bool {
        let removed = self.grammars.remove(scope_name).is_some();
        if removed {
            self.invalidate();
        }
        removed
    }

    /// Whether a grammar with this scope name is registered.
    pub fn contains(&self, scope_name: &str) -> bool {
        self.grammars.contains_key(scope_name)
    }

    /// Registered scope names, sorted.
    pub fn scope_names(&self) -> Vec<&str> {
        self.grammars.keys().map(String::as_str).collect()
    }

    /// The parsed (unlinked) grammar, e.g. for its name and file types.
    pub fn raw(&self, scope_name: &str) -> Option<&RawGrammar> {
        self.grammars.get(scope_name).map(Arc::as_ref)
    }

    /// The grammar for `scope_name`, linked with every registered grammar it
    /// includes. Linked grammars are cached until the registry changes;
    /// grammars handed out earlier keep working with what they linked.
    pub fn grammar(&self, scope_name: &str) -> Option<Arc<Grammar>> {
        if let Some(grammar) = self
            .linked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(scope_name)
        {
            return Some(grammar.clone());
        }
        let raw = self.grammars.get(scope_name)?;
        let lookup = |scope: &str| self.grammars.get(scope).map(Arc::as_ref);
        let grammar = Arc::new(link(raw, &lookup));
        self.linked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(scope_name.to_string(), grammar.clone());
        Some(grammar)
    }

    /// The scope of the grammar whose `fileTypes` match a file name: an
    /// entry equal to the name (`Makefile`) or to its extension (`rs`,
    /// case-insensitive; multi-part such as `html.erb` too). The longest
    /// matching entry wins.
    pub fn scope_for_file_name(&self, file_name: &str) -> Option<&str> {
        let file_name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
        let lower = file_name.to_lowercase();
        let mut best: Option<(&str, usize)> = None;
        for (scope, grammar) in &self.grammars {
            for file_type in grammar.file_types() {
                let ft = file_type.trim_start_matches('.');
                if ft.is_empty() {
                    continue;
                }
                let ft_lower = ft.to_lowercase();
                // `x.rs` for `rs`; also `.gitignore` for `gitignore`.
                let matches = file_name == ft
                    || lower
                        .strip_suffix(&ft_lower)
                        .is_some_and(|stem| stem.ends_with('.'));
                if matches && best.is_none_or(|(_, len)| ft.len() > len) {
                    best = Some((scope, ft.len()));
                }
            }
        }
        best.map(|(scope, _)| scope)
    }

    /// The scope of the first grammar (by scope name) whose `firstLineMatch`
    /// matches `line` (e.g. a `#!/usr/bin/env python3` shebang).
    pub fn scope_for_first_line(&self, line: &str) -> Option<&str> {
        let text = format!("{}\n", line.trim_end_matches(['\r', '\n']));
        let ctx = SearchCtx {
            allow_a: true,
            g_pos: Some(0),
        };
        self.grammars.iter().find_map(|(scope, grammar)| {
            let matcher = Matcher::new(grammar.first_line_match()?).ok()?;
            matcher.search(&text, 0, ctx, false).map(|_| scope.as_str())
        })
    }

    fn invalidate(&mut self) {
        self.linked
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

fn collect_grammar_files(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    errors: &mut Vec<(PathBuf, GrammarError)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            errors.push((
                dir.to_path_buf(),
                GrammarError::Io {
                    path: dir.to_path_buf(),
                    message: e.to_string(),
                },
            ));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `file_type` does not follow symlinks, so linked directories cannot
        // loop.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if depth < MAX_BUNDLE_DEPTH {
                collect_grammar_files(&path, depth + 1, out, errors);
            }
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if GRAMMAR_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        let mut r = Registry::new();
        r.add_json(
            r#"{ "scopeName": "source.a", "fileTypes": ["a", "gitignore"], "firstLineMatch": "^#!.*\\bal\\b" }"#,
        )
        .unwrap();
        r.add_json(r#"{ "scopeName": "text.html.a", "fileTypes": ["html.a", "Afile"] }"#)
            .unwrap();
        r
    }

    #[test]
    fn file_type_lookup() {
        let r = registry();
        assert_eq!(r.scope_for_file_name("x.a"), Some("source.a"));
        assert_eq!(r.scope_for_file_name("dir/x.A"), Some("source.a"));
        assert_eq!(r.scope_for_file_name("x.html.a"), Some("text.html.a"));
        assert_eq!(r.scope_for_file_name("Afile"), Some("text.html.a"));
        assert_eq!(r.scope_for_file_name("afile"), None);
        // A whole name equal to an entry matches too (`Makefile`).
        assert_eq!(r.scope_for_file_name("a"), Some("source.a"));
        assert_eq!(r.scope_for_file_name("xa"), None);
        assert_eq!(r.scope_for_file_name(".gitignore"), Some("source.a"));
        assert_eq!(r.scope_for_file_name("x.b"), None);
    }

    #[test]
    fn first_line_lookup() {
        let r = registry();
        assert_eq!(
            r.scope_for_first_line("#!/usr/bin/env al\n"),
            Some("source.a")
        );
        assert_eq!(r.scope_for_first_line("print(1)"), None);
    }

    #[test]
    fn linked_grammars_are_cached_until_the_set_changes() {
        let mut r = registry();
        let g1 = r.grammar("source.a").unwrap();
        assert!(Arc::ptr_eq(&g1, &r.grammar("source.a").unwrap()));
        assert!(r.grammar("source.missing").is_none());
        r.add_json(r#"{ "scopeName": "source.b" }"#).unwrap();
        assert!(!Arc::ptr_eq(&g1, &r.grammar("source.a").unwrap()));
        assert_eq!(r.scope_names(), vec!["source.a", "source.b", "text.html.a"]);
        assert!(r.remove("source.b"));
        assert!(!r.remove("source.b"));
        assert!(!r.contains("source.b"));
        assert_eq!(r.raw("source.a").unwrap().file_types().len(), 2);
    }
}
