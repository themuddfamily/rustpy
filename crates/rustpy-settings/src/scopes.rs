//! Shared scope language and captured-file policy. No enumeration or filesystem IO.
//! Syntax: `file:pattern` or `file[module]:pattern` operands (`*` matches within a
//! directory, `//*` recursively), `$Name` references to other named scopes, `!`, `&&`,
//! `||` and parentheses; operands may be JSON-quoted strings.
use serde::{Deserialize, Serialize, Serializer};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PredefinedScope {
    #[default]
    Project,
    Production,
    Tests,
    Open,
    Changed,
    Current,
    ScratchesConsoles,
    AllPlaces,
    RecentlyViewed,
    RecentlyChanged,
}
impl PredefinedScope {
    pub const ALL: &'static [Self] = &[
        Self::Project,
        Self::Production,
        Self::Tests,
        Self::Open,
        Self::Changed,
        Self::Current,
        Self::ScratchesConsoles,
        Self::AllPlaces,
        Self::RecentlyViewed,
        Self::RecentlyChanged,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "Project Files",
            Self::Production => "Project Production Files",
            Self::Tests => "Project Test Files",
            Self::Open => "Open Files",
            Self::Changed => "Changed Files",
            Self::Current => "Current File",
            Self::ScratchesConsoles => "Scratches and Consoles",
            Self::AllPlaces => "All Places",
            Self::RecentlyViewed => "Recently Viewed Files",
            Self::RecentlyChanged => "Recently Changed Files",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeSelection {
    Predefined(PredefinedScope),
    Named(String),
    Custom(String),
}
impl Default for ScopeSelection {
    fn default() -> Self {
        Self::Predefined(PredefinedScope::Project)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedScope {
    pub name: String,
    pub pattern: String,
    pub color: Option<[u8; 3]>,
    /// "Share through VCS": stored in the project's `.rustpy/scopes.json`
    /// instead of the IDE settings. Shared scopes are merged into `named`
    /// when a project opens so every scope selector lists them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScopeSettings {
    /// Local and project-shared scopes in editor order; only local ones are
    /// written to the IDE settings.
    #[serde(serialize_with = "serialize_local_scopes")]
    pub named: Vec<NamedScope>,
    pub todo: ScopeSelection,
    pub inspections: ScopeSelection,
    pub project_view: ScopeSelection,
    pub file_colors_enabled: bool,
}
impl Default for ScopeSettings {
    fn default() -> Self {
        Self {
            named: Vec::new(),
            todo: ScopeSelection::default(),
            inspections: ScopeSelection::default(),
            project_view: ScopeSelection::Predefined(PredefinedScope::AllPlaces),
            file_colors_enabled: false,
        }
    }
}
fn serialize_local_scopes<S: Serializer>(named: &[NamedScope], out: S) -> Result<S::Ok, S::Error> {
    out.collect_seq(named.iter().filter(|scope| !scope.shared))
}
/// Project file of the scopes shared through VCS.
pub const PROJECT_SCOPES_FILE: &str = "scopes.json";
#[derive(Default, Serialize, Deserialize)]
struct ProjectScopes {
    #[serde(default)]
    scopes: Vec<NamedScope>,
}
impl ScopeSettings {
    pub fn project_path(root: &Path) -> PathBuf {
        root.join(crate::PROJECT_DIR_NAME).join(PROJECT_SCOPES_FILE)
    }
    fn read_project(path: &Path) -> Result<Option<ProjectScopes>, String> {
        use std::io::Read;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("Project scopes exceed 1 MiB".into());
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display()))
    }
    /// Replaces the shared scopes of any previously open project with the
    /// ones in `root`. A local scope keeps its name; a same-named shared
    /// scope is skipped and reported.
    pub fn load_project(&mut self, root: &Path) -> Result<(), String> {
        self.named.retain(|scope| !scope.shared);
        let Some(stored) = Self::read_project(&Self::project_path(root))? else {
            return Ok(());
        };
        let mut hidden = Vec::new();
        for mut scope in stored.scopes {
            if self
                .named
                .iter()
                .any(|existing| existing.name == scope.name)
            {
                hidden.push(scope.name);
                continue;
            }
            scope.shared = true;
            self.named.push(scope);
        }
        if hidden.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Shared scopes hidden by local scopes of the same name: {}",
                hidden.join(", ")
            ))
        }
    }
    /// Writes the shared scopes to `.rustpy/scopes.json`. Nothing is created
    /// for a project without shared scopes, an unchanged file is not
    /// rewritten, and a corrupt team file must be repaired explicitly.
    pub fn save_project(&self, root: &Path) -> Result<(), String> {
        let path = Self::project_path(root);
        let scopes: Vec<NamedScope> = self
            .named
            .iter()
            .filter(|scope| scope.shared)
            .map(|scope| NamedScope {
                shared: false,
                ..scope.clone()
            })
            .collect();
        match Self::read_project(&path)? {
            Some(stored) if stored.scopes == scopes => return Ok(()),
            None if scopes.is_empty() => return Ok(()),
            _ => {}
        }
        let directory = root.join(crate::PROJECT_DIR_NAME);
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let stage = path.with_extension("json.tmp");
        std::fs::write(
            &stage,
            serde_json::to_vec_pretty(&ProjectScopes { scopes }).map_err(|e| e.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        std::fs::rename(stage, path).map_err(|error| error.to_string())
    }
}
/// Scope-editor tree buttons of the Edit Scopes dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeAction {
    Include,
    IncludeRecursively,
    Exclude,
    ExcludeRecursively,
}
impl TreeAction {
    pub const ALL: [Self; 4] = [
        Self::Include,
        Self::IncludeRecursively,
        Self::Exclude,
        Self::ExcludeRecursively,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Include => "Include",
            Self::IncludeRecursively => "Include Recursively",
            Self::Exclude => "Exclude",
            Self::ExcludeRecursively => "Exclude Recursively",
        }
    }
    fn recursive(self) -> bool {
        matches!(self, Self::IncludeRecursively | Self::ExcludeRecursively)
    }
}
/// `file:` clause for a project-relative tree node (`""` is the project
/// root): a file, `dir/*` for a directory's own files, `dir//*` for its
/// whole subtree. A root-level file is anchored (`/name`) because a
/// slash-free pattern matches the file name in every directory. Operands
/// with operator characters are quoted.
pub fn tree_clause(relative: &str, directory: bool, recursive: bool) -> String {
    let relative = relative.trim_matches('/');
    let body = if relative.is_empty() {
        "*".to_string()
    } else if !directory && !relative.contains('/') {
        format!("/{relative}")
    } else if !directory {
        relative.to_string()
    } else if recursive {
        format!("{relative}//*")
    } else {
        format!("{relative}/*")
    };
    let token = format!("file:{body}");
    if token.contains(['!', '&', '|', '(', ')', '"']) || token.trim() != token {
        serde_json::to_string(&token).unwrap_or(token)
    } else {
        token
    }
}
/// Splits `text` at top-level (outside parentheses and quotes) `separator`s.
fn split_top(text: &str, separator: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut escaped, mut start) = (0usize, false, false, 0);
    let mut chars = text.char_indices();
    while let Some((at, ch)) = chars.next() {
        if quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quote = false;
            }
            continue;
        }
        match ch {
            '"' => quote = true,
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 && text[at..].starts_with(separator) => {
                parts.push(text[start..at].trim().to_string());
                start = at + separator.len();
                for _ in 1..separator.len() {
                    chars.next();
                }
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim().to_string());
    parts.retain(|part| !part.is_empty());
    parts
}
/// Applies a tree button to a scope pattern. Including drops a matching
/// top-level exclusion and appends `||clause`; excluding drops a matching
/// top-level inclusion, otherwise appends `&&!clause` (parenthesising a
/// disjunction so the exclusion applies to all of it).
pub fn apply_tree_action(
    pattern: &str,
    action: TreeAction,
    relative: &str,
    directory: bool,
) -> String {
    let clause = tree_clause(relative, directory, action.recursive());
    let negated = format!("!{clause}");
    let mut disjuncts: Vec<Vec<String>> = split_top(pattern, "||")
        .iter()
        .map(|part| split_top(part, "&&"))
        .filter(|part| !part.is_empty())
        .collect();
    let join = |disjuncts: &[Vec<String>]| {
        disjuncts
            .iter()
            .map(|part| part.join("&&"))
            .collect::<Vec<_>>()
            .join("||")
    };
    let is_clause = |part: &Vec<String>| part.len() == 1 && part[0] == clause;
    match action {
        TreeAction::Include | TreeAction::IncludeRecursively => {
            for part in &mut disjuncts {
                part.retain(|term| *term != negated);
            }
            // A disjunct that was only `!clause` ("everything but this")
            // becomes "everything" once the node is included again.
            if disjuncts.iter().any(|part| part.is_empty()) {
                return tree_clause("", true, true);
            }
            if !disjuncts.iter().any(is_clause) {
                disjuncts.push(vec![clause.clone()]);
            }
            join(&disjuncts)
        }
        TreeAction::Exclude | TreeAction::ExcludeRecursively => {
            let before = disjuncts.len();
            disjuncts.retain(|part| !is_clause(part));
            let excluded = disjuncts.len() == 1 && disjuncts[0].contains(&negated);
            if disjuncts.len() != before || excluded {
                return join(&disjuncts);
            }
            match disjuncts.len() {
                0 => negated,
                1 => format!("{}&&{negated}", join(&disjuncts)),
                _ => format!("({})&&{negated}", join(&disjuncts)),
            }
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct ScopeContext {
    pub root: PathBuf,
    /// Sorted authoritative ProjectScope/FileIndex catalogue, shared without copying.
    pub project_files: Arc<Vec<PathBuf>>,
    pub test_roots: Vec<PathBuf>,
    pub open_files: BTreeSet<PathBuf>,
    pub changed_files: BTreeSet<PathBuf>,
    pub current_file: Option<PathBuf>,
    pub recently_viewed: BTreeSet<PathBuf>,
    pub recently_changed: BTreeSet<PathBuf>,
    pub scratch_roots: Vec<PathBuf>,
    pub modules: BTreeMap<String, PathBuf>,
}
impl ScopeContext {
    pub fn is_project(&self, path: &Path) -> bool {
        self.project_files
            .binary_search_by(|entry| entry.as_path().cmp(path))
            .is_ok()
    }
    fn is_test(&self, path: &Path) -> bool {
        self.test_roots.iter().any(|root| path.starts_with(root))
            || (self.test_roots.is_empty()
                && (path
                    .components()
                    .any(|part| matches!(part.as_os_str().to_str(), Some("tests" | "test")))
                    || path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("test_") || name.ends_with("_test.py")
                        })))
    }
    pub fn predefined(&self, scope: PredefinedScope, path: &Path) -> bool {
        match scope {
            PredefinedScope::Project => self.is_project(path),
            PredefinedScope::Production => self.is_project(path) && !self.is_test(path),
            PredefinedScope::Tests => self.is_project(path) && self.is_test(path),
            PredefinedScope::Open => self.open_files.contains(path),
            PredefinedScope::Changed => self.changed_files.contains(path),
            PredefinedScope::Current => self.current_file.as_deref() == Some(path),
            PredefinedScope::ScratchesConsoles => {
                self.scratch_roots.iter().any(|root| path.starts_with(root))
            }
            PredefinedScope::AllPlaces => true,
            PredefinedScope::RecentlyViewed => self.recently_viewed.contains(path),
            PredefinedScope::RecentlyChanged => self.recently_changed.contains(path),
        }
    }
}
#[derive(Clone, Debug)]
enum Expr {
    Pattern {
        pattern: String,
        module: Option<String>,
    },
    Predefined(PredefinedScope),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}
#[derive(Clone, Debug)]
pub struct CompiledScope(Expr);
impl ScopeSettings {
    pub fn label(&self, selection: &ScopeSelection) -> String {
        match selection {
            ScopeSelection::Predefined(scope) => scope.label().into(),
            ScopeSelection::Named(name) => name.clone(),
            ScopeSelection::Custom(pattern) => format!("Custom: {pattern}"),
        }
    }
    pub fn compile(&self, selection: &ScopeSelection) -> Result<CompiledScope, String> {
        if self.named.len() > 128 {
            return Err("At most 128 named scopes are supported".into());
        }
        let mut names = BTreeSet::new();
        for scope in &self.named {
            if scope.name.trim().is_empty()
                || scope.name.len() > 128
                || !names.insert(scope.name.as_str())
                || PredefinedScope::ALL
                    .iter()
                    .any(|predefined| predefined.label() == scope.name)
            {
                return Err(
                    "Scope names must be unique and distinct from predefined scopes".into(),
                );
            }
        }
        let mut budget = 256;
        self.expand(selection, &mut Vec::new(), &mut budget)
            .map(CompiledScope)
    }
    fn expand(
        &self,
        selection: &ScopeSelection,
        chain: &mut Vec<String>,
        budget: &mut usize,
    ) -> Result<Expr, String> {
        match selection {
            ScopeSelection::Predefined(scope) => Ok(Expr::Predefined(*scope)),
            ScopeSelection::Named(name) => {
                if chain.len() >= 16 || chain.contains(name) {
                    return Err("Scope reference cycle/depth limit".into());
                }
                if let Some(scope) = PredefinedScope::ALL
                    .iter()
                    .find(|scope| scope.label() == name)
                {
                    return Ok(Expr::Predefined(*scope));
                }
                let named = self
                    .named
                    .iter()
                    .find(|scope| scope.name == *name)
                    .ok_or_else(|| format!("Unknown scope: {name}"))?;
                chain.push(name.clone());
                let result = self.parse(&named.pattern, chain, budget);
                chain.pop();
                result
            }
            ScopeSelection::Custom(pattern) => self.parse(pattern, chain, budget),
        }
    }
    fn parse(
        &self,
        pattern: &str,
        chain: &mut Vec<String>,
        budget: &mut usize,
    ) -> Result<Expr, String> {
        if pattern.trim().is_empty() || pattern.len() > 4096 {
            return Err("Scope pattern must contain 1–4096 bytes".into());
        }
        let mut parser = Parser {
            text: pattern,
            at: 0,
            settings: self,
            chain,
            budget,
        };
        let expr = parser.expression(0, 0)?;
        parser.space();
        if parser.at != pattern.len() {
            return Err("Unexpected scope syntax".into());
        }
        Ok(expr)
    }
    pub fn matches(
        &self,
        selection: &ScopeSelection,
        path: &Path,
        context: &ScopeContext,
    ) -> Result<bool, String> {
        Ok(self.compile(selection)?.matches(path, context))
    }
    pub fn filter_paths(
        &self,
        selection: &ScopeSelection,
        paths: &[PathBuf],
        context: &ScopeContext,
    ) -> Result<Vec<PathBuf>, String> {
        let scope = self.compile(selection)?;
        Ok(paths
            .iter()
            .filter(|path| scope.matches(path, context))
            .cloned()
            .collect())
    }
    /// Last matching named color wins, matching the editor's ordered scope list.
    pub fn color(&self, path: &Path, context: &ScopeContext) -> Option<[u8; 3]> {
        if !self.file_colors_enabled {
            return None;
        }
        self.named
            .iter()
            .rev()
            .filter_map(|scope| scope.color.map(|color| (scope, color)))
            .find_map(|(scope, color)| {
                self.compile(&ScopeSelection::Named(scope.name.clone()))
                    .ok()
                    .filter(|scope| scope.matches(path, context))
                    .map(|_| color)
            })
    }
}
struct Parser<'a, 'b> {
    text: &'a str,
    at: usize,
    settings: &'b ScopeSettings,
    chain: &'b mut Vec<String>,
    budget: &'b mut usize,
}
impl Parser<'_, '_> {
    fn space(&mut self) {
        while self
            .text
            .as_bytes()
            .get(self.at)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.at += 1;
        }
    }
    fn expression(&mut self, depth: usize, min: u8) -> Result<Expr, String> {
        self.space();
        if depth > 32 || *self.budget == 0 {
            return Err("Scope complexity limit".into());
        }
        *self.budget -= 1;
        let mut left = if self.text[self.at..].starts_with('!') {
            self.at += 1;
            Expr::Not(Box::new(self.expression(depth + 1, 3)?))
        } else if self.text[self.at..].starts_with('(') {
            self.at += 1;
            let inner = self.expression(depth + 1, 0)?;
            self.space();
            if !self.text[self.at..].starts_with(')') {
                return Err("Missing scope ')'".into());
            }
            self.at += 1;
            inner
        } else {
            self.atom()?
        };
        loop {
            self.space();
            let rest = &self.text[self.at..];
            let (op, precedence) = if rest.starts_with("&&") {
                (true, 2)
            } else if rest.starts_with("||") {
                (false, 1)
            } else {
                break;
            };
            if precedence < min {
                break;
            }
            self.at += 2;
            let right = self.expression(depth + 1, precedence + 1)?;
            left = if op {
                Expr::And(Box::new(left), Box::new(right))
            } else {
                Expr::Or(Box::new(left), Box::new(right))
            };
        }
        Ok(left)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        let start = self.at;
        let mut quote = false;
        let mut escaped = false;
        for ch in self.text[self.at..].chars() {
            if !quote && matches!(ch, '!' | '&' | '|' | '(' | ')') {
                break;
            }
            self.at += ch.len_utf8();
            if escaped {
                escaped = false;
            } else if ch == '\\' && quote {
                escaped = true;
            } else if ch == '"' {
                quote = !quote;
            }
        }
        if quote {
            return Err("Unclosed quoted scope pattern".into());
        }
        let token = self.text[start..self.at].trim();
        if token.is_empty() {
            return Err("Missing scope operand".into());
        }
        if let Some(name) = token.strip_prefix('$') {
            let name = decode(name)?;
            return self
                .settings
                .expand(&ScopeSelection::Named(name), self.chain, self.budget);
        }
        let token = decode(token)?;
        let (module, pattern) = if let Some(rest) = token.strip_prefix("file[") {
            let end = rest.find("]:").ok_or("Expected file[module]:pattern")?;
            (Some(rest[..end].to_string()), rest[end + 2..].to_string())
        } else {
            (
                None,
                token.strip_prefix("file:").unwrap_or(&token).to_string(),
            )
        };
        if pattern.is_empty() || pattern.len() > 1024 || pattern.contains(':') {
            return Err("Invalid file scope pattern".into());
        }
        Ok(Expr::Pattern { pattern, module })
    }
}
fn decode(text: &str) -> Result<String, String> {
    if text.starts_with('"') {
        serde_json::from_str(text).map_err(|_| "Invalid quoted scope operand".into())
    } else {
        Ok(text.to_string())
    }
}
impl CompiledScope {
    /// Allows enumeration consumers to avoid loading libraries/scratches for
    /// project-only expressions; all actual membership still uses matches().
    pub fn includes_non_project(&self) -> bool {
        fn outside(expr: &Expr) -> bool {
            match expr {
                Expr::Predefined(
                    PredefinedScope::Project | PredefinedScope::Production | PredefinedScope::Tests,
                ) => false,
                Expr::And(a, b) => outside(a) && outside(b),
                Expr::Or(a, b) => outside(a) || outside(b),
                _ => true,
            }
        }
        outside(&self.0)
    }
    pub fn matches(&self, path: &Path, context: &ScopeContext) -> bool {
        fn eval(expr: &Expr, path: &Path, context: &ScopeContext) -> bool {
            match expr {
                Expr::Predefined(scope) => context.predefined(*scope, path),
                Expr::Not(value) => !eval(value, path, context),
                Expr::And(a, b) => eval(a, path, context) && eval(b, path, context),
                Expr::Or(a, b) => eval(a, path, context) || eval(b, path, context),
                Expr::Pattern { pattern, module } => {
                    let root = match module {
                        Some(name) => match context.modules.get(name) {
                            Some(root) => root,
                            None => return false,
                        },
                        None => &context.root,
                    };
                    if module.is_some() && !path.starts_with(root) {
                        return false;
                    }
                    let relative = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    // A leading `/` anchors the pattern at the root, so
                    // `file:/setup.py` names only the root file whereas
                    // `file:setup.py` matches that file name anywhere.
                    if let Some(anchored) = pattern.strip_prefix('/') {
                        return glob(anchored, &relative);
                    }
                    let value = if pattern.contains('/') {
                        relative.as_str()
                    } else {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("")
                    };
                    glob(pattern, value)
                }
            }
        }
        eval(&self.0, path, context)
    }
}
fn glob(pattern: &str, value: &str) -> bool {
    if pattern.len() > 1024 || value.len() > 16384 {
        return false;
    }
    let mut pattern = pattern.replace("//", "/**/");
    if pattern.starts_with("*/") {
        pattern.insert(0, '*');
    }
    let p: Vec<_> = pattern.chars().collect();
    let v: Vec<_> = value.chars().collect();
    if p.len() * v.len() > 1_000_000 {
        return false;
    }
    let mut row = vec![false; v.len() + 1];
    row[0] = true;
    let mut i = 0;
    while i < p.len() {
        let mut next = vec![false; row.len()];
        if p[i] == '*' {
            let cross = p.get(i + 1) == Some(&'*');
            if cross {
                i += 1;
            }
            next[0] = row[0];
            if cross && p.get(i + 1) == Some(&'/') {
                let mut reached = row[0];
                for j in 1..row.len() {
                    next[j] = row[j] || (reached && v[j - 1] == '/');
                    reached |= row[j];
                }
                i += 1;
            } else {
                for j in 1..row.len() {
                    next[j] = row[j] || (next[j - 1] && (cross || v[j - 1] != '/'));
                }
            }
        } else {
            for j in 1..row.len() {
                next[j] = row[j - 1] && (p[i] == v[j - 1] || (p[i] == '?' && v[j - 1] != '/'));
            }
        }
        row = next;
        i += 1;
    }
    row[v.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> ScopeContext {
        let mut files = vec![
            PathBuf::from("/p/src/pkg/a.py"),
            PathBuf::from("/p/src/pkg/sub/b.py"),
            PathBuf::from("/p/src/pkg/test_bad.py"),
            PathBuf::from("/p/tests/test_a.py"),
            PathBuf::from("/p/readme.md"),
        ];
        files.sort();
        ScopeContext {
            root: "/p".into(),
            project_files: Arc::new(files),
            test_roots: vec!["/p/tests".into()],
            open_files: BTreeSet::from(["/p/src/pkg/a.py".into()]),
            changed_files: BTreeSet::from(["/p/src/pkg/sub/b.py".into()]),
            current_file: Some("/p/src/pkg/a.py".into()),
            recently_viewed: BTreeSet::from(["/p/readme.md".into()]),
            recently_changed: BTreeSet::from(["/p/src/pkg/sub/b.py".into()]),
            scratch_roots: vec!["/scratch".into()],
            modules: BTreeMap::from([("p".into(), "/p".into())]),
        }
    }
    #[test]
    fn actual_scope_example_has_recursive_directory_exclusion_and_boolean_precedence() {
        let settings = ScopeSettings::default();
        let context = context();
        let scope = settings
            .compile(&ScopeSelection::Custom(
                "file:src/*//*.py&&!file:*/test*".into(),
            ))
            .unwrap();
        for path in ["/p/src/pkg/a.py", "/p/src/pkg/sub/b.py"] {
            assert!(scope.matches(Path::new(path), &context));
        }
        for path in [
            "/p/src/pkg/test_bad.py",
            "/p/tests/test_a.py",
            "/p/readme.md",
        ] {
            assert!(!scope.matches(Path::new(path), &context));
        }
        let shallow = settings
            .compile(&ScopeSelection::Custom("file:src/pkg/*".into()))
            .unwrap();
        assert!(shallow.matches(Path::new("/p/src/pkg/a.py"), &context));
        assert!(!shallow.matches(Path::new("/p/src/pkg/sub/b.py"), &context));
        let union = settings
            .compile(&ScopeSelection::Custom(
                "(file:*.md||file:*.py)&&!file:*bad*".into(),
            ))
            .unwrap();
        assert!(union.matches(Path::new("/p/readme.md"), &context));
        assert!(!union.matches(Path::new("/p/src/pkg/test_bad.py"), &context));
    }
    #[test]
    fn named_references_cycles_quoted_paths_module_identity_and_complexity_are_guarded() {
        let mut settings = ScopeSettings::default();
        let context = context();
        settings.named.push(NamedScope {
            name: "Python source".into(),
            pattern: "file:src//*.py".into(),
            color: None,
            shared: false,
        });
        let selected = ScopeSelection::Custom("$\"Python source\"&&!file:*bad*".into());
        assert!(settings
            .matches(&selected, Path::new("/p/src/pkg/a.py"), &context)
            .unwrap());
        let module = settings
            .compile(&ScopeSelection::Custom("file[p]:src//*".into()))
            .unwrap();
        assert!(module.matches(Path::new("/p/src/pkg/a.py"), &context));
        assert!(!module.matches(Path::new("/other/src/pkg/a.py"), &context));
        settings.named.push(NamedScope {
            name: "cycle".into(),
            pattern: "$cycle".into(),
            color: None,
            shared: false,
        });
        assert!(settings
            .compile(&ScopeSelection::Named("cycle".into()))
            .is_err());
        for invalid in [
            "",
            "file:src//*&&",
            "(file:*.py",
            "$missing",
            "file[broken:*.py",
        ] {
            assert!(
                settings
                    .compile(&ScopeSelection::Custom(invalid.into()))
                    .is_err(),
                "{invalid}"
            );
        }
        assert!(settings
            .compile(&ScopeSelection::Custom(format!(
                "{}file:*.py{}",
                "(".repeat(40),
                ")".repeat(40)
            )))
            .is_err());
    }
    #[test]
    fn all_predefined_scopes_use_captured_authority_and_ordered_file_colors_roundtrip() {
        let mut settings = ScopeSettings::default();
        let context = context();
        for (scope, path) in [
            (PredefinedScope::Project, "/p/readme.md"),
            (PredefinedScope::Production, "/p/src/pkg/a.py"),
            (PredefinedScope::Tests, "/p/tests/test_a.py"),
            (PredefinedScope::Open, "/p/src/pkg/a.py"),
            (PredefinedScope::Changed, "/p/src/pkg/sub/b.py"),
            (PredefinedScope::Current, "/p/src/pkg/a.py"),
            (PredefinedScope::ScratchesConsoles, "/scratch/session.py"),
            (PredefinedScope::AllPlaces, "/library/third_party.py"),
            (PredefinedScope::RecentlyViewed, "/p/readme.md"),
            (PredefinedScope::RecentlyChanged, "/p/src/pkg/sub/b.py"),
        ] {
            assert!(context.predefined(scope, Path::new(path)), "{scope:?}");
        }
        assert!(!context.predefined(PredefinedScope::Project, Path::new("/p/ignored.py")));
        assert!(!context.predefined(PredefinedScope::Production, Path::new("/p/tests/test_a.py")));
        settings.file_colors_enabled = true;
        settings.named = vec![
            NamedScope {
                name: "All python".into(),
                pattern: "file:*.py".into(),
                color: Some([1, 2, 3]),
                shared: false,
            },
            NamedScope {
                name: "Source".into(),
                pattern: "file:src//*".into(),
                color: Some([4, 5, 6]),
                shared: false,
            },
        ];
        assert_eq!(
            settings.color(Path::new("/p/src/pkg/a.py"), &context),
            Some([4, 5, 6])
        );
        assert_eq!(
            settings.color(Path::new("/p/tests/test_a.py"), &context),
            Some([1, 2, 3])
        );
        let encoded = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<ScopeSettings>(&encoded).unwrap(),
            settings
        );
        assert!(!settings
            .compile(&ScopeSelection::Predefined(PredefinedScope::Project))
            .unwrap()
            .includes_non_project());
    }
    #[test]
    fn tree_actions_generate_include_exclude_clauses_that_compile_and_match() {
        let settings = ScopeSettings::default();
        let context = context();
        let matches = |pattern: &str, path: &str| {
            settings
                .compile(&ScopeSelection::Custom(pattern.into()))
                .unwrap()
                .matches(Path::new(path), &context)
        };
        let pattern = apply_tree_action("", TreeAction::IncludeRecursively, "src", true);
        assert_eq!(pattern, "file:src//*");
        let pattern =
            apply_tree_action(&pattern, TreeAction::Exclude, "src/pkg/test_bad.py", false);
        assert_eq!(pattern, "file:src//*&&!file:src/pkg/test_bad.py");
        assert!(matches(&pattern, "/p/src/pkg/sub/b.py"));
        assert!(!matches(&pattern, "/p/src/pkg/test_bad.py"));
        // Excluding again is idempotent; including drops the exclusion.
        assert_eq!(
            apply_tree_action(&pattern, TreeAction::Exclude, "src/pkg/test_bad.py", false),
            pattern
        );
        assert_eq!(
            apply_tree_action(&pattern, TreeAction::Include, "src/pkg/test_bad.py", false),
            "file:src//*||file:src/pkg/test_bad.py"
        );
        let pattern = apply_tree_action(&pattern, TreeAction::Include, "tests", true);
        assert_eq!(
            pattern,
            "file:src//*&&!file:src/pkg/test_bad.py||file:tests/*"
        );
        assert!(matches(&pattern, "/p/tests/test_a.py"));
        // An exclusion after a disjunction applies to all of it.
        let pattern = apply_tree_action(
            &pattern,
            TreeAction::ExcludeRecursively,
            "src/pkg/sub",
            true,
        );
        assert_eq!(
            pattern,
            "(file:src//*&&!file:src/pkg/test_bad.py||file:tests/*)&&!file:src/pkg/sub//*"
        );
        assert!(!matches(&pattern, "/p/src/pkg/sub/b.py"));
        assert!(matches(&pattern, "/p/src/pkg/a.py"));
        // Excluding an included node removes its clause rather than negating it.
        assert_eq!(
            apply_tree_action(
                "file:src//*||file:tests/*",
                TreeAction::Exclude,
                "tests",
                true
            ),
            "file:src//*"
        );
        // Root-level files are anchored so a nested file of the same name
        // is unaffected; slash-free patterns still match names anywhere.
        let excluded = apply_tree_action("", TreeAction::Exclude, "readme.md", false);
        assert_eq!(excluded, "!file:/readme.md");
        assert!(!matches(&excluded, "/p/readme.md"));
        assert!(matches(&excluded, "/p/src/readme.md"));
        let included = apply_tree_action("", TreeAction::Include, "readme.md", false);
        assert_eq!(included, "file:/readme.md");
        assert!(matches(&included, "/p/readme.md"));
        assert!(!matches(&included, "/p/src/readme.md"));
        assert!(!matches(&included, "/elsewhere/readme.md"));
        assert!(matches("file:readme.md", "/p/src/readme.md"));
        // Including the node of a lone exclusion ("everything but it") yields
        // everything, not just that node.
        assert_eq!(
            apply_tree_action(&excluded, TreeAction::Include, "readme.md", false),
            "file:*"
        );
        assert_eq!(
            apply_tree_action(
                "file:src//*||!file:/readme.md",
                TreeAction::Include,
                "readme.md",
                false
            ),
            "file:*"
        );
        assert!(matches("file:*", "/p/src/pkg/sub/b.py"));
        assert_eq!(tree_clause("", true, true), "file:*");
        let odd = tree_clause("src/a&b (1).py", false, false);
        assert_eq!(odd, "\"file:src/a&b (1).py\"");
        let pattern =
            apply_tree_action("file:src//*", TreeAction::Exclude, "src/a&b (1).py", false);
        assert!(settings
            .compile(&ScopeSelection::Custom(pattern.clone()))
            .is_ok());
        assert_eq!(
            apply_tree_action(&pattern, TreeAction::Include, "src/a&b (1).py", false),
            format!("file:src//*||{odd}")
        );
    }
    #[test]
    fn shared_scopes_roundtrip_through_project_file_and_stay_out_of_ide_settings() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = ScopeSettings {
            named: vec![
                NamedScope {
                    name: "Local".into(),
                    pattern: "file:*.py".into(),
                    color: None,
                    shared: false,
                },
                NamedScope {
                    name: "Team".into(),
                    pattern: "file:src//*".into(),
                    color: Some([1, 2, 3]),
                    shared: true,
                },
            ],
            ..ScopeSettings::default()
        };
        // IDE settings keep only local scopes, and old configs without the
        // flag still load as local scopes.
        let encoded = serde_json::to_value(&settings).unwrap();
        assert_eq!(encoded["named"].as_array().unwrap().len(), 1);
        assert!(encoded["named"][0].get("shared").is_none());
        let legacy: ScopeSettings = serde_json::from_str(
            r#"{"named":[{"name":"Old","pattern":"file:*.md","color":null}]}"#,
        )
        .unwrap();
        assert!(!legacy.named[0].shared);
        // Nothing is written for a project without shared scopes.
        legacy.save_project(root.path()).unwrap();
        assert!(!ScopeSettings::project_path(root.path()).exists());
        settings.save_project(root.path()).unwrap();
        let mut loaded: ScopeSettings = serde_json::from_value(encoded).unwrap();
        loaded.load_project(root.path()).unwrap();
        assert_eq!(loaded.named, settings.named);
        assert!(loaded
            .compile(&ScopeSelection::Named("Team".into()))
            .is_ok());
        // Reloading (or switching projects) replaces, never duplicates, shared scopes.
        loaded.load_project(root.path()).unwrap();
        assert_eq!(loaded.named.len(), 2);
        let other = tempfile::tempdir().unwrap();
        loaded.load_project(other.path()).unwrap();
        assert_eq!(loaded.named.len(), 1);
        // A local scope of the same name wins and the conflict is reported.
        let mut clash = ScopeSettings::default();
        clash.named.push(NamedScope {
            name: "Team".into(),
            pattern: "file:*".into(),
            color: None,
            shared: false,
        });
        assert!(clash.load_project(root.path()).is_err());
        assert_eq!(clash.named.len(), 1);
        // Unsharing the last scope empties the team file; a corrupt file is kept.
        settings.named[1].shared = false;
        settings.save_project(root.path()).unwrap();
        let mut reloaded = ScopeSettings::default();
        reloaded.load_project(root.path()).unwrap();
        assert!(reloaded.named.is_empty());
        std::fs::write(ScopeSettings::project_path(root.path()), "{broken").unwrap();
        settings.named[1].shared = true;
        assert!(settings.save_project(root.path()).is_err());
        assert!(reloaded.load_project(root.path()).is_err());
    }
}
