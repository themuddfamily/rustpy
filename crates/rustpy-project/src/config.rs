//! Run/debug configurations and their persistence (roadmap §3.1).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunConfigKind {
    /// Run a script file.
    Python {
        script: PathBuf,
    },
    /// `python -m module`.
    PythonModule {
        module: String,
    },
    /// pytest / unittest target.
    Pytest {
        target: String,
        keywords: String,
    },
    Unittest {
        target: String,
    },
    ShellScript {
        script: PathBuf,
    },
    /// A program started directly, without a shell, with arguments that are passed
    /// as they are (External Tools and File Watchers): spaces and shell characters
    /// in file names need no quoting, and it works the same on Windows.
    Program {
        program: PathBuf,
        args: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunConfig {
    pub workflow: crate::run_workflow::Workflow,
    pub name: String,
    pub type_options: Option<crate::configuration_types::ConfigurationSpec>,
    pub kind: RunConfigKind,
    pub parameters: String,
    pub working_dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub interpreter_options: String,
    pub interpreter: Option<PathBuf>,
    pub emulate_terminal: bool,
    pub show_console_on_stdout: bool,
    pub show_console_on_stderr: bool,
    pub console_encoding: String,
    pub console_buffer_kib: Option<usize>,
    pub add_content_roots: bool,
    pub add_source_roots: bool,
    pub run_with_console: bool,
    pub temporary: bool,
    /// Shared configurations are committed; private ones stay in workspace.json.
    pub shared: bool,
    /// `.env` files loaded before [`RunConfig::env`] (relative paths are
    /// resolved against the project root); see [`parse_env_file`].
    pub env_files: Vec<PathBuf>,
    /// File fed to the process's standard input (relative paths: project root).
    pub redirect_input: Option<PathBuf>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            workflow: Default::default(),
            name: "Unnamed".into(),
            type_options: None,
            kind: RunConfigKind::Python {
                script: PathBuf::new(),
            },
            parameters: String::new(),
            working_dir: None,
            env: BTreeMap::new(),
            interpreter_options: String::new(),
            interpreter: None,
            emulate_terminal: false,
            show_console_on_stdout: false,
            show_console_on_stderr: true,
            console_encoding: "utf-8".into(),
            console_buffer_kib: None,
            add_content_roots: true,
            add_source_roots: true,
            run_with_console: false,
            temporary: false,
            shared: true,
            env_files: Vec::new(),
            redirect_input: None,
        }
    }
}

impl RunConfig {
    pub fn for_script(script: &Path) -> Self {
        let name = script
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "script".into());
        Self {
            name,
            kind: RunConfigKind::Python {
                script: script.to_path_buf(),
            },
            working_dir: script.parent().map(Path::to_path_buf),
            temporary: true,
            ..Default::default()
        }
    }

    pub fn for_pytest(target: &str, root: &Path) -> Self {
        let name = if target.is_empty() {
            "pytest".to_string()
        } else {
            format!(
                "pytest in {}",
                Path::new(target)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| target.to_string())
            )
        };
        Self {
            name,
            kind: RunConfigKind::Pytest {
                target: target.to_string(),
                keywords: String::new(),
            },
            working_dir: Some(root.to_path_buf()),
            temporary: true,
            ..Default::default()
        }
    }

    /// The script of a Python-script configuration, resolved as a run resolves
    /// it: a relative path against the working directory (else `project_root`).
    pub fn script_path(&self, project_root: &Path) -> Option<PathBuf> {
        let RunConfigKind::Python { script } = &self.kind else {
            return None;
        };
        Some(if script.is_absolute() {
            script.clone()
        } else {
            self.working_dir
                .as_deref()
                .unwrap_or(project_root)
                .join(script)
        })
    }

    /// Icon-ish short kind label.
    pub fn kind_label(&self) -> &'static str {
        if let Some(spec) = &self.type_options {
            return spec.label();
        }
        match self.kind {
            RunConfigKind::Python { .. } => "Python",
            RunConfigKind::PythonModule { .. } => "Python",
            RunConfigKind::Pytest { .. } => "pytest",
            RunConfigKind::Unittest { .. } => "unittest",
            RunConfigKind::ShellScript { .. } => "Shell",
            RunConfigKind::Program { .. } => "Program",
        }
    }

    /// The environment the configuration adds to the process: the `.env`
    /// files in order, then [`RunConfig::env`] (later entries win). The second
    /// value lists files that could not be read.
    pub fn load_env(&self, project_root: &Path) -> (BTreeMap<String, String>, Vec<String>) {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        let mut errors = Vec::new();
        for file in &self.env_files {
            if file.as_os_str().is_empty() {
                continue;
            }
            let path = if file.is_absolute() {
                file.clone()
            } else {
                project_root.join(file)
            };
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let (parsed, problems) = parse_env_file(&text, |k| {
                        env.get(k).cloned().or_else(|| {
                            self.workflow
                                .inherit_environment
                                .then(|| std::env::var(k).ok())
                                .flatten()
                        })
                    });
                    errors.extend(
                        problems
                            .into_iter()
                            .map(|p| format!("Env file {}: {p}", path.display())),
                    );
                    env.extend(parsed);
                }
                Err(e) => errors.push(format!("Cannot read env file {}: {e}", path.display())),
            }
        }
        env.extend(self.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        (env, errors)
    }

    /// `redirect_input` resolved against the project root.
    pub fn input_file(&self, project_root: &Path) -> Option<PathBuf> {
        let f = self
            .redirect_input
            .as_ref()
            .filter(|f| !f.as_os_str().is_empty())?;
        Some(if f.is_absolute() {
            f.clone()
        } else {
            project_root.join(f)
        })
    }

    /// Why the configuration cannot start: its "Redirect input from" file
    /// does not exist (launchers check this before starting the process).
    pub fn input_problem(&self, project_root: &Path) -> Option<String> {
        let path = self.input_file(project_root)?;
        (!path.is_file()).then(|| {
            format!(
                "Cannot redirect input from {}: the file does not exist",
                path.display()
            )
        })
    }

    /// The working directory of the process: relative paths are resolved
    /// against the project root, none means the project root.
    pub fn working_dir_in(&self, project_root: &Path) -> PathBuf {
        match &self.working_dir {
            Some(d) if !d.as_os_str().is_empty() => {
                if d.is_absolute() {
                    d.clone()
                } else {
                    project_root.join(d)
                }
            }
            _ => project_root.to_path_buf(),
        }
    }

    /// Whether both configurations run the same thing: the same script or
    /// shell script, the same module with the same parameters, or the same
    /// test target with the same test selection (see [`RunConfig::test_filters`]:
    /// a `-k` configuration of a test does not run that test as such, it
    /// can deselect it). Paths are compared the way the process resolves
    /// them: scripts against the working directory, test targets against the
    /// project root.
    pub fn same_target(&self, other: &RunConfig, project_root: &Path) -> bool {
        if self.type_options.is_some() || other.type_options.is_some() {
            if self
                .type_options
                .as_ref()
                .is_some_and(|spec| spec.kind.special())
                || other
                    .type_options
                    .as_ref()
                    .is_some_and(|spec| spec.kind.special())
            {
                return self.type_options == other.type_options;
            }
            let lowered = |cfg: &RunConfig| {
                let mut cfg = if let Some(spec) = &cfg.type_options {
                    spec.materialize_at(cfg, project_root).ok()?
                } else {
                    cfg.clone()
                };
                cfg.type_options = None;
                Some(cfg)
            };
            return match (lowered(self), lowered(other)) {
                (Some(a), Some(b)) => a.same_target(&b, project_root),
                _ => false,
            };
        }
        let script = |cfg: &RunConfig, p: &Path| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                cfg.working_dir_in(project_root).join(p)
            }
        };
        let test = |t: &str| {
            let t = t.trim().replace('\\', "/");
            let (file, rest) = match t.split_once("::") {
                Some((f, r)) => (f.to_string(), format!("::{r}")),
                None => (t.clone(), String::new()),
            };
            let full = lexical(&if Path::new(&file).is_absolute() {
                PathBuf::from(&file)
            } else {
                project_root.join(&file)
            });
            format!("{}{rest}", full.to_string_lossy())
        };
        match (&self.kind, &other.kind) {
            (RunConfigKind::Python { script: a }, RunConfigKind::Python { script: b })
            | (
                RunConfigKind::ShellScript { script: a },
                RunConfigKind::ShellScript { script: b },
            ) => same_file(&script(self, a), &script(other, b)),
            (
                RunConfigKind::PythonModule { module: a },
                RunConfigKind::PythonModule { module: b },
            ) => a.trim() == b.trim() && self.parameters.trim() == other.parameters.trim(),
            (RunConfigKind::Pytest { target: a, .. }, RunConfigKind::Pytest { target: b, .. })
            | (RunConfigKind::Unittest { target: a }, RunConfigKind::Unittest { target: b }) => {
                test(a) == test(b) && self.test_filters() == other.test_filters()
            }
            _ => false,
        }
    }

    /// The test selection a pytest / unittest configuration applies to its
    /// target, normalised and sorted: `-k` keywords (the Keywords field or
    /// `-k` in the Parameters) and pytest's `-m` marker expressions.
    pub fn test_filters(&self) -> Vec<String> {
        let (keywords, options): (&str, &[&str]) = match &self.kind {
            RunConfigKind::Pytest { keywords, .. } => (keywords, &["-k", "-m"]),
            RunConfigKind::Unittest { .. } => ("", &["-k"]),
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        if !keywords.trim().is_empty() {
            out.push(format!("-k {}", keywords.trim()));
        }
        let args = Self::split_args(&self.parameters);
        let mut args = args.iter();
        while let Some(a) = args.next() {
            for opt in options {
                if a == opt {
                    if let Some(value) = args.next() {
                        out.push(format!("{opt} {}", value.trim()));
                    }
                    break;
                }
                if let Some(value) = a.strip_prefix(opt).filter(|v| !v.is_empty()) {
                    out.push(format!("{opt} {}", value.trim()));
                    break;
                }
            }
        }
        out.sort();
        out
    }

    /// A name no configuration in `existing` uses: `base`, else `base (1)`, `base (2)`, …
    /// (a trailing ` (N)` of `base` is replaced rather than nested).
    pub fn unique_name(base: &str, existing: &[&str]) -> String {
        let taken = |n: &str| existing.contains(&n);
        if !taken(base) {
            return base.to_string();
        }
        let stem = match base.rsplit_once(" (") {
            Some((s, rest))
                if rest
                    .strip_suffix(')')
                    .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())) =>
            {
                s
            }
            _ => base,
        };
        (1..)
            .map(|i| format!("{stem} ({i})"))
            .find(|n| !taken(n))
            .unwrap_or_else(|| base.to_string())
    }

    /// Splits `parameters` honoring simple quoting.
    pub fn split_args(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut quote: Option<char> = None;
        let mut has = false;
        for c in s.chars() {
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => cur.push(c),
                None if c == '"' || c == '\'' => {
                    quote = Some(c);
                    has = true;
                }
                None if c.is_whitespace() => {
                    if has || !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                        has = false;
                    }
                }
                None => cur.push(c),
            }
        }
        if has || !cur.is_empty() {
            out.push(cur);
        }
        out
    }
}

/// `path` with `.` and `..` removed lexically (no file system access); `/..` is `/`.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Parses a `.env` file into `(key, value)` pairs in file order, and the
/// problems found (`line N: …`).
///
/// Supported syntax (a python-dotenv compatible subset): `KEY=VALUE` lines
/// with an optional `export ` prefix; blank lines and `#` comments (also after
/// an unquoted value when preceded by whitespace); single-quoted values taken
/// literally except escaped quote/backslash; single-quoted keys and a leading
/// UTF-8 BOM are supported. Quoted values reject trailing non-comment text.
/// Double-quoted values support the escapes `\n`, `\t`, `\r`, `\"`,
/// `\\` and `\$`; `${VAR}` / `${VAR:-default}` expansion in unquoted and
/// double-quoted values. Variables defined earlier in the file are visible
/// to later lines, anything else is resolved with `lookup`. Lines without
/// `=` are ignored.
///
/// A quoted value may span lines, but it must be closed, and on a later line
/// only a comment may follow the closing quote. Otherwise the line of the
/// opening quote is skipped and reported, and reading goes on with the next
/// line (python-dotenv likewise skips a value it cannot parse instead of
/// swallowing the rest of the file).
pub fn parse_env_file(
    text: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> (Vec<(String, String)>, Vec<String>) {
    let chars: Vec<char> = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .chars()
        .collect();
    let n = chars.len();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut problems = Vec::new();
    let mut i = 0;
    let skip_line = |i: &mut usize| {
        while *i < n && chars[*i] != '\n' {
            *i += 1;
        }
        *i = (*i + 1).min(n);
    };
    let line_of = |pos: usize| chars[..pos].iter().filter(|c| **c == '\n').count() + 1;
    while i < n {
        while i < n && matches!(chars[i], ' ' | '\t' | '\r' | '\n') {
            i += 1;
        }
        if i >= n {
            break;
        }
        if chars[i] == '#' {
            skip_line(&mut i);
            continue;
        }
        let key_start = i;
        while i < n && chars[i] != '=' && chars[i] != '\n' {
            i += 1;
        }
        if i >= n || chars[i] == '\n' {
            continue; // not an assignment
        }
        let mut key = chars[key_start..i]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        if let Some(rest) = key.strip_prefix("export") {
            if rest.starts_with([' ', '\t']) {
                key = rest.trim().to_string();
            }
        }
        let quoted_key = key.starts_with('\'') && key.ends_with('\'') && key.len() > 2;
        if quoted_key {
            key = key[1..key.len() - 1].to_string();
        }
        i += 1; // '='
        while i < n && matches!(chars[i], ' ' | '\t') {
            i += 1;
        }
        let resolve = |name: &str, out: &[(String, String)]| {
            out.iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .or_else(|| lookup(name))
        };
        let value = if i < n && (chars[i] == '\'' || chars[i] == '"') {
            let (quote, open) = (chars[i], i);
            i += 1;
            let mut raw = String::new();
            while i < n && chars[i] != quote {
                if quote == '\''
                    && chars[i] == '\\'
                    && i + 1 < n
                    && matches!(chars[i + 1], '\\' | '\'')
                {
                    i += 1;
                    raw.push(chars[i]);
                } else if quote == '"' && chars[i] == '\\' && i + 1 < n {
                    i += 1;
                    match chars[i] {
                        'n' => raw.push('\n'),
                        't' => raw.push('\t'),
                        'r' => raw.push('\r'),
                        '"' => raw.push('"'),
                        '\\' => raw.push('\\'),
                        // An escaped `$` must survive expansion: park it on a private-use char.
                        '$' => raw.push('\u{E000}'),
                        other => {
                            raw.push('\\');
                            raw.push(other);
                        }
                    }
                } else {
                    raw.push(chars[i]);
                }
                i += 1;
            }
            let closed = i < n && {
                let mut after = i + 1;
                while after < n && matches!(chars[after], ' ' | '\t' | '\r') {
                    after += 1;
                }
                !chars[open..i].contains(&'\n')
                    || after >= n
                    || chars[after] == '\n'
                    || chars[after] == '#'
            };
            if !closed {
                problems.push(format!(
                    "line {}: the quoted value of {key} is not closed; the line is ignored",
                    line_of(open)
                ));
                i = open;
                skip_line(&mut i);
                continue;
            }
            let mut after = i + 1;
            while after < n && matches!(chars[after], ' ' | '\t' | '\r') {
                after += 1;
            }
            if after < n && chars[after] != '\n' && chars[after] != '#' {
                problems.push(format!(
                    "line {}: unexpected text after the quoted value of {key}; the line is ignored",
                    line_of(open)
                ));
                skip_line(&mut i);
                continue;
            }
            skip_line(&mut i);
            if quote == '"' {
                expand_env_refs(&raw, |name| resolve(name, &out)).replace('\u{E000}', "$")
            } else {
                raw
            }
        } else {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            let mut v: String = chars[start..i].iter().collect();
            if let Some(pos) = v.find(" #").or_else(|| v.find("\t#")) {
                v.truncate(pos);
            }
            i = (i + 1).min(n);
            expand_env_refs(v.trim(), |name| resolve(name, &out))
        };
        if key.is_empty() || (!quoted_key && key.contains(char::is_whitespace)) {
            continue;
        }
        out.push((key, value));
    }
    (out, problems)
}

/// Expands `${NAME}` and `${NAME:-default}`; unknown names expand to the
/// default or to nothing.
fn expand_env_refs(s: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("${") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[pos..]);
            return out;
        };
        let inner = &after[..end];
        let (name, default) = match inner.split_once(":-") {
            Some((name, d)) => (name, Some(d)),
            None => (inner, None),
        };
        match lookup(name.trim()) {
            Some(v) if !(v.is_empty() && default.is_some()) => out.push_str(&v),
            _ => out.push_str(default.unwrap_or("")),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Turns a unittest target into an argument `python -m unittest` accepts:
/// pytest-style node ids (`tests/test_x.py::Case::test_a`) and file paths
/// (`tests/test_x.py`, absolute under `project_root` or relative to it)
/// become dotted names (`tests.test_x.Case.test_a`); dotted names stay as
/// they are.
pub fn unittest_cli_target(target: &str, project_root: &Path) -> String {
    let (file_part, rest) = match target.split_once("::") {
        Some((f, r)) => (f, Some(r)),
        None => (target, None),
    };
    let normalized = file_part.replace('\\', "/");
    if !(normalized.ends_with(".py") || normalized.contains('/') || rest.is_some()) {
        return target.to_string();
    }
    let rel = Path::new(&normalized)
        .strip_prefix(project_root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| normalized.clone());
    let rel = rel.trim_start_matches("./");
    let mut module = rel.strip_suffix(".py").unwrap_or(rel).to_string();
    if let Some(pkg) = module.strip_suffix("/__init__") {
        module = pkg.to_string();
    }
    let module = module.trim_matches('/').replace('/', ".");
    match rest {
        Some(r) if !r.is_empty() => format!("{module}.{}", r.replace("::", ".")),
        _ => module,
    }
}

/// The `python -m unittest` arguments that run a Unittests `target`: no
/// target discovers from the working directory (`discover`); a directory
/// (absolute or relative to `project_root`) is discovered too, as
/// `discover -s DIR`, plus `-t` with the first ancestor that is not a
/// package when DIR is one (its modules then import as the Test Runner
/// imports them); anything else is a test name (see [`unittest_cli_target`]).
/// A directory used to be passed as a module name, which runs no tests.
pub fn unittest_cli_args(target: &str, project_root: &Path) -> Vec<String> {
    let target = target.trim();
    if target.is_empty() {
        return vec!["discover".into()];
    }
    let dir = lexical(&if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        project_root.join(target)
    });
    if !dir.is_dir() {
        return vec![unittest_cli_target(target, project_root)];
    }
    let mut top = dir.clone();
    while top.join("__init__.py").is_file() {
        match top.parent() {
            Some(parent) => top = parent.to_path_buf(),
            None => break,
        }
    }
    let mut args = vec![
        "discover".to_string(),
        "-s".to_string(),
        dir.to_string_lossy().into_owned(),
    ];
    if top != dir {
        args.extend(["-t".to_string(), top.to_string_lossy().into_owned()]);
    }
    args
}

/// Persistent store of a project's run configurations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunConfigStore {
    pub configs: Vec<RunConfig>,
    pub selected: Option<String>,
}

impl RunConfigStore {
    pub fn path_for(project: &Path) -> PathBuf {
        project
            .join(".rustpy")
            .join("runConfigurations/configurations.json")
    }

    pub fn load(project: &Path) -> Self {
        let canonical = Self::path_for(project);
        let path = if canonical.exists() {
            canonical
        } else {
            project.join(".rustpy/runConfigurations.json")
        };
        // Shared configurations write paths with path variables (`$PROJECT_DIR$/main.py`).
        let vars = rustpy_settings::path_variables::active();
        let mut store: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|mut value| {
                vars.expand_json(&mut value, Some(project));
                serde_json::from_value(value).ok()
            })
            .unwrap_or_default();
        if let Ok(snapshot) = rustpy_settings::WorkspaceStore::new(project).load() {
            if let Some(value) = snapshot.run_configurations() {
                if let Ok(private) = serde_json::from_value::<RunConfigStore>(value.clone()) {
                    for mut configuration in private.configs {
                        configuration.shared = false;
                        store
                            .configs
                            .retain(|existing| existing.name != configuration.name);
                        store.configs.push(configuration);
                    }
                    if private.selected.is_some() {
                        store.selected = private.selected;
                    }
                }
            }
        }
        store
    }

    pub fn save(&self, project: &Path) -> anyhow::Result<()> {
        let path = Self::path_for(project);
        let existing = if path.exists() {
            path.clone()
        } else {
            project.join(".rustpy/runConfigurations.json")
        };
        match std::fs::read(&existing) {
            Ok(bytes) => {
                serde_json::from_slice::<RunConfigStore>(&bytes)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        crate::ProjectModel::ensure_gitignore(project)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let persistent = RunConfigStore {
            configs: self
                .configs
                .iter()
                .filter(|c| !c.temporary && c.shared)
                .cloned()
                .collect(),
            selected: self.selected.clone(),
        };
        let workspace_store = rustpy_settings::WorkspaceStore::new(project);
        let workspace_baseline = workspace_store.load()?;
        let private = RunConfigStore {
            configs: self
                .configs
                .iter()
                .filter(|configuration| !configuration.temporary && !configuration.shared)
                .cloned()
                .collect(),
            selected: self.selected.clone(),
        };
        let workspace_patch = rustpy_settings::WorkspacePatch {
            run_configurations: Some(serde_json::to_value(private)?),
            ..Default::default()
        };
        let stage = path.with_extension("json.tmp");
        // The shared file is committed: paths under the project or a path
        // variable's folder are written with the variable, so they work on
        // other machines. Private ones (workspace.json) stay as they are.
        let mut shared = serde_json::to_value(&persistent)?;
        rustpy_settings::path_variables::active().collapse_json(&mut shared, Some(project));
        std::fs::write(&stage, serde_json::to_string_pretty(&shared)?)?;
        std::fs::rename(stage, &path)?;
        workspace_store.commit(&workspace_baseline, &workspace_patch, None)?;
        Ok(())
    }

    /// Adds `cfg` as a new configuration and selects it; returns its name.
    /// Unlike [`upsert`](Self::upsert) it never replaces a configuration: a
    /// name that is taken gets a ` (1)`, ` (2)`… suffix (the Run/Debug
    /// Configurations dialog's "+" buttons).
    pub fn add_new(&mut self, mut cfg: RunConfig) -> String {
        cfg.name = self.unique_name(&cfg.name);
        let name = cfg.name.clone();
        self.configs.push(cfg);
        self.selected = Some(name.clone());
        name
    }

    /// The configuration a Rerun of a Run or Debug tab starts. `name` is the
    /// tab's name and `own` the configuration it was started with, as stored
    /// then (before macro expansion). The stored configuration of that name
    /// when it is still that configuration, so edits apply: one that runs
    /// the same target, or a permanent configuration that the tab was
    /// started from (its target may have been edited too). Otherwise `own`:
    /// its temporary configuration was evicted by newer runs (its name may
    /// now belong to a configuration for another script), or it was deleted.
    /// `own` then runs as a temporary configuration, so a deleted
    /// configuration is not stored for good again.
    pub fn config_for_rerun(&self, name: &str, own: Option<&RunConfig>) -> Option<RunConfig> {
        let stored = self
            .configs
            .iter()
            .find(|c| c.name == name)
            .filter(|c| own.is_none_or(|o| o.kind == c.kind || (!o.temporary && !c.temporary)));
        stored.cloned().or_else(|| {
            own.map(|o| RunConfig {
                temporary: true,
                ..o.clone()
            })
        })
    }

    /// Adds or replaces a configuration by name and selects it.
    pub fn upsert(&mut self, cfg: RunConfig) {
        if let Some(existing) = self.configs.iter_mut().find(|c| c.name == cfg.name) {
            *existing = cfg.clone();
        } else {
            self.configs.push(cfg.clone());
        }
        // Keep at most 5 temporary configurations.
        let temps = self.configs.iter().filter(|c| c.temporary).count();
        if temps > 5 {
            if let Some(i) = self
                .configs
                .iter()
                .position(|c| c.temporary && c.name != cfg.name)
            {
                self.configs.remove(i);
            }
        }
        self.selected = Some(cfg.name);
    }

    /// The selected configuration; `None` stands for "Current File" (there is
    /// no fallback to the first configuration).
    pub fn selected_config(&self) -> Option<&RunConfig> {
        self.selected
            .as_ref()
            .and_then(|n| self.configs.iter().find(|c| &c.name == n))
    }

    /// What to launch (and [`RunConfigStore::upsert`]) for `cfg`. A
    /// permanent configuration is returned as it is. A temporary one made by
    /// the IDE (editor or Project view context, a test, Run Anything) must
    /// not replace another configuration: with `reuse`, an existing
    /// configuration that runs the same target is returned instead (the
    /// selected one first, then one of the same name, then permanent ones),
    /// so its parameters, environment and "Run with Python Console" apply
    /// (context configurations are reused); otherwise `cfg` gets a unique
    /// name (`name (1)`) when a permanent configuration, or one with another
    /// target, has its name.
    pub fn resolve_temporary(
        &self,
        mut cfg: RunConfig,
        reuse: bool,
        project_root: &Path,
    ) -> RunConfig {
        if !cfg.temporary {
            return cfg;
        }
        if reuse {
            let selected = self.selected.as_deref();
            if let Some(existing) = self
                .configs
                .iter()
                .filter(|c| c.same_target(&cfg, project_root))
                .min_by_key(|c| {
                    (
                        Some(c.name.as_str()) != selected,
                        c.name != cfg.name,
                        c.temporary,
                    )
                })
            {
                return existing.clone();
            }
        }
        if self
            .configs
            .iter()
            .any(|c| c.name == cfg.name && (!c.temporary || !c.same_target(&cfg, project_root)))
        {
            cfg.name = self.unique_name(&cfg.name);
        }
        cfg
    }

    /// The configuration that runs `script` from its editor or the Project view
    /// (Ctrl+Shift+F10, Ctrl+Shift+F9, the gutter ▶): the script's existing
    /// configuration, else a new temporary one (see [`Self::resolve_temporary`]).
    pub fn context_config_for_script(&self, script: &Path, project_root: &Path) -> RunConfig {
        self.resolve_temporary(RunConfig::for_script(script), true, project_root)
    }

    /// Configuration for running the tests `target` from the editor
    /// (Ctrl+Shift+F10, gutter ▶): an existing configuration for that target,
    /// else a new temporary one named `name` (see [`Self::resolve_temporary`]).
    pub fn context_config_for_pytest(
        &self,
        target: &str,
        name: Option<&str>,
        project_root: &Path,
    ) -> RunConfig {
        let mut cfg = RunConfig::for_pytest(target, project_root);
        if let Some(name) = name {
            cfg.name = name.to_string();
        }
        self.resolve_temporary(cfg, true, project_root)
    }

    /// `name`, or `name (1)`, `name (2)`… when a configuration already has it.
    pub fn unique_name(&self, name: &str) -> String {
        let names: Vec<&str> = self.configs.iter().map(|c| c.name.as_str()).collect();
        RunConfig::unique_name(name, &names)
    }
}

/// True when `a` and `b` name the same file: equal once `.` and `..` are
/// removed, or after resolving symbolic links when both exist.
fn same_file(a: &Path, b: &Path) -> bool {
    lexical(a) == lexical(b)
        || matches!((dunce::canonicalize(a), dunce::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_run_configurations_stay_in_workspace_and_keep_other_fields() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = rustpy_settings::Workspace::path_for(directory.path());
        std::fs::create_dir_all(workspace.parent().unwrap()).unwrap();
        std::fs::write(
            &workspace,
            r#"{"future_state":{"keep":true},"bookmarks":[]}"#,
        )
        .unwrap();
        let mut private = RunConfig::for_script(Path::new("private.py"));
        private.temporary = false;
        private.shared = false;
        let mut shared = RunConfig::for_script(Path::new("team.py"));
        shared.temporary = false;
        let store = RunConfigStore {
            configs: vec![private.clone(), shared.clone()],
            selected: Some(private.name.clone()),
        };
        store.save(directory.path()).unwrap();
        let committed =
            std::fs::read_to_string(RunConfigStore::path_for(directory.path())).unwrap();
        assert!(committed.contains("team.py"));
        assert!(!committed.contains("private.py"));
        let local: serde_json::Value =
            serde_json::from_slice(&std::fs::read(workspace).unwrap()).unwrap();
        assert_eq!(local["future_state"]["keep"], true);
        let restored = RunConfigStore::load(directory.path());
        assert_eq!(restored.configs.len(), 2);
        assert!(
            !restored
                .configs
                .iter()
                .find(|config| config.name == private.name)
                .unwrap()
                .shared
        );
        assert_eq!(restored.selected, store.selected);
    }

    #[test]
    fn shared_run_configurations_store_paths_with_path_variables() {
        use rustpy_settings::path_variables::{install, PathVariable, PathVariables};
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("app");
        std::fs::create_dir_all(&project).unwrap();
        // Unique folder: the definitions are process-wide.
        let sdks = directory.path().join("pv-config-sdks");
        install(&PathVariables {
            variables: vec![PathVariable {
                name: "PV_CONFIG_SDKS".into(),
                path: sdks.clone(),
            }],
            ignored: Vec::new(),
        });
        let mut config = RunConfig::for_script(&project.join("main.py"));
        config.temporary = false;
        config.interpreter = Some(sdks.join("py311").join("python"));
        let store = RunConfigStore {
            configs: vec![config.clone()],
            selected: None,
        };
        store.save(&project).unwrap();
        let committed = std::fs::read_to_string(RunConfigStore::path_for(&project)).unwrap();
        assert!(committed.contains("$PROJECT_DIR$/main.py"), "{committed}");
        assert!(
            committed.contains("$PV_CONFIG_SDKS$/py311/python"),
            "{committed}"
        );
        assert!(
            !committed.contains(&*project.to_string_lossy()),
            "{committed}"
        );
        let loaded = RunConfigStore::load(&project);
        assert_eq!(loaded.configs, vec![config]);
    }

    #[test]
    fn legacy_shared_run_configuration_file_is_read_before_canonical_migration() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join(".rustpy")).unwrap();
        let mut config = RunConfig::for_script(Path::new("legacy.py"));
        config.temporary = false;
        let store = RunConfigStore {
            configs: vec![config],
            selected: None,
        };
        std::fs::write(
            directory.path().join(".rustpy/runConfigurations.json"),
            serde_json::to_vec(&store).unwrap(),
        )
        .unwrap();
        let loaded = RunConfigStore::load(directory.path());
        assert_eq!(loaded, store);
        loaded.save(directory.path()).unwrap();
        assert!(RunConfigStore::path_for(directory.path()).is_file());
    }

    #[test]
    fn splits_args_and_persists() {
        assert_eq!(
            RunConfig::split_args(r#"a "b c" 'd' e"#),
            vec!["a", "b c", "d", "e"]
        );
        let dir = tempfile::tempdir().unwrap();
        let mut store = RunConfigStore::default();
        let mut cfg = RunConfig::for_script(&dir.path().join("main.py"));
        cfg.temporary = false;
        store.upsert(cfg.clone());
        store.save(dir.path()).unwrap();
        let loaded = RunConfigStore::load(dir.path());
        assert_eq!(loaded.configs.len(), 1);
        assert_eq!(loaded.selected.as_deref(), Some("main"));
        assert_eq!(loaded.selected_config().unwrap().kind_label(), "Python");
    }

    #[test]
    fn no_selection_means_current_file() {
        let mut store = RunConfigStore::default();
        store.configs.push(RunConfig {
            name: "a".into(),
            ..Default::default()
        });
        assert!(
            store.selected_config().is_none(),
            "no fallback to the first configuration"
        );
        store.selected = Some("gone".into());
        assert!(store.selected_config().is_none());
        store.selected = Some("a".into());
        assert_eq!(store.selected_config().map(|c| c.name.as_str()), Some("a"));
    }

    #[test]
    fn old_configuration_files_still_load() {
        let json = r#"{"configs":[{"name":"t","kind":{"type":"pytest","target":"tests","keywords":""},"parameters":"-x","env":{"A":"1"}}],"selected":"t"}"#;
        let store: RunConfigStore = serde_json::from_str(json).unwrap();
        let cfg = &store.configs[0];
        assert!(cfg.env_files.is_empty());
        assert_eq!(cfg.redirect_input, None);
        assert_eq!(cfg.parameters, "-x");
        assert!(cfg.add_source_roots);
    }

    fn parse(text: &str) -> Vec<(String, String)> {
        let (pairs, problems) =
            parse_env_file(text, |k| (k == "HOME_DIR").then(|| "/home/me".to_string()));
        assert!(problems.is_empty(), "{problems:?}");
        pairs
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn env_file_basic_syntax() {
        let text = "# comment\n\nA=1\nexport B = two words  \nC=value # inline comment\nD=has#hash\n  E=  \nnot an assignment\n";
        assert_eq!(
            parse(text),
            pairs(&[
                ("A", "1"),
                ("B", "two words"),
                ("C", "value"),
                ("D", "has#hash"),
                ("E", "")
            ])
        );
    }

    #[test]
    fn env_file_single_quote_escapes_and_quoted_keys_preserve_literal_expansion() {
        assert_eq!(
            parse(
                "\u{feff}'TOKEN'='it\\'s \\\\ ${HOME_DIR}' # comment\n'WITH SPACE'=ok\nNEXT=visible\n"
            ),
            pairs(&[
                ("TOKEN", "it's \\ ${HOME_DIR}"),
                ("WITH SPACE", "ok"),
                ("NEXT", "visible")
            ])
        );
        assert_eq!(
            parse("A='literal\\n\\q'\n"),
            pairs(&[("A", "literal\\n\\q")])
        );
    }

    #[test]
    fn env_file_quoted_trailing_junk_is_reported_and_never_published() {
        let (vars, problems) = parse_env_file(
            "A=\"secret\"junk\nB='also' junk\nC=\"valid\" # comment\nAFTER=ready\n",
            |_| None,
        );
        assert_eq!(vars, pairs(&[("C", "valid"), ("AFTER", "ready")]));
        assert_eq!(problems.len(), 2);
        assert!(problems[0].contains("line 1: unexpected text"));
        assert!(problems[1].contains("line 2: unexpected text"));
    }

    #[test]
    fn actual_env_file_loading_reports_bad_binding_and_preserves_override_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "\u{feff}'TOKEN'='it\\'s'\nBAD=\"lost\"suffix\nNEXT=λ🙂\n",
        )
        .unwrap();
        let mut cfg = RunConfig {
            env_files: vec![PathBuf::from(".env")],
            ..Default::default()
        };
        cfg.env.insert("NEXT".into(), "explicit wins".into());
        let (env, problems) = cfg.load_env(dir.path());
        assert_eq!(env.get("TOKEN").map(String::as_str), Some("it's"));
        assert_eq!(env.get("NEXT").map(String::as_str), Some("explicit wins"));
        assert!(!env.contains_key("BAD"));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains(".env: line 2: unexpected text"));
    }

    #[test]
    fn env_file_quotes_and_escapes() {
        let text = "S='single ${HOME_DIR} \\n kept'\nD=\"tab\\there\\nnew \\\"q\\\" \\$HOME_DIR\" # c\nM=\"line1\nline2\"\nAFTER=x\n";
        assert_eq!(
            parse(text),
            pairs(&[
                ("S", "single ${HOME_DIR} \\n kept"),
                ("D", "tab\there\nnew \"q\" $HOME_DIR"),
                ("M", "line1\nline2"),
                ("AFTER", "x")
            ])
        );
    }

    #[test]
    fn env_file_expansion() {
        let text = "BASE=/srv\nDATA=${BASE}/data\nCFG=\"${HOME_DIR}/cfg\"\nMISSING=${NOPE}\nDEF=${NOPE:-fallback}\nEMPTY=\nDEF2=${EMPTY:-used}\nOPEN=${unterminated\n";
        assert_eq!(
            parse(text),
            pairs(&[
                ("BASE", "/srv"),
                ("DATA", "/srv/data"),
                ("CFG", "/home/me/cfg"),
                ("MISSING", ""),
                ("DEF", "fallback"),
                ("EMPTY", ""),
                ("DEF2", "used"),
                ("OPEN", "${unterminated")
            ])
        );
    }

    #[test]
    fn env_file_unterminated_quotes_skip_only_their_line() {
        let parse_checked = |text: &str| parse_env_file(text, |_| None);
        // B and C used to become part of A's value, without any error.
        assert_eq!(
            parse_checked("A='oops\nB=2\nC=3\n"),
            (
                pairs(&[("B", "2"), ("C", "3")]),
                vec![
                    "line 1: the quoted value of A is not closed; the line is ignored".to_string()
                ]
            )
        );
        let (vars, problems) = parse_checked("X=1\nD=\"open \\\" quote\nE=5");
        assert_eq!(vars, pairs(&[("X", "1"), ("E", "5")]));
        assert_eq!(
            problems,
            vec!["line 2: the quoted value of D is not closed; the line is ignored".to_string()]
        );
        // A quote on a later line does not close the value when more text follows it.
        let (vars, problems) = parse_checked("A='oops\nB='x'\nC=3\n");
        assert_eq!(vars, pairs(&[("B", "x"), ("C", "3")]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        // Closed multi-line values still work (a comment may follow; CRLF line ends).
        assert_eq!(
            parse_checked("M='a\nb' # note\r\nN=\"c\nd\"\r\nO=1"),
            (pairs(&[("M", "a\nb"), ("N", "c\nd"), ("O", "1")]), vec![])
        );
        // A run reports the problem with the file's name and keeps the other variables.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A='oops\nB=2\n").unwrap();
        let cfg = RunConfig {
            env_files: vec![PathBuf::from(".env")],
            ..Default::default()
        };
        let (env, errors) = cfg.load_env(dir.path());
        assert_eq!(
            (env.get("A"), env.get("B").map(String::as_str)),
            (None, Some("2"))
        );
        assert!(
            errors.len() == 1 && errors[0].contains(".env") && errors[0].contains("line 1"),
            "{errors:?}"
        );
    }

    #[test]
    fn load_env_merges_files_then_explicit_variables() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "A=from_file\nB=${A}-b\n").unwrap();
        std::fs::write(dir.path().join("second.env"), "B=second\nC=3\n").unwrap();
        let mut cfg = RunConfig {
            env_files: vec![
                PathBuf::from(".env"),
                dir.path().join("second.env"),
                PathBuf::from("missing.env"),
            ],
            ..Default::default()
        };
        cfg.env.insert("C".into(), "explicit".into());
        let (env, errors) = cfg.load_env(dir.path());
        assert_eq!(env.get("A").map(String::as_str), Some("from_file"));
        assert_eq!(env.get("B").map(String::as_str), Some("second"));
        assert_eq!(env.get("C").map(String::as_str), Some("explicit"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("missing.env"), "{errors:?}");
    }

    #[test]
    fn unique_names() {
        assert_eq!(RunConfig::unique_name("main", &[]), "main");
        assert_eq!(RunConfig::unique_name("main", &["main"]), "main (1)");
        assert_eq!(
            RunConfig::unique_name("main", &["main", "main (1)"]),
            "main (2)"
        );
        // Copying "main (1)" gives "main (2)", not "main (1) (1)".
        assert_eq!(
            RunConfig::unique_name("main (1)", &["main", "main (1)"]),
            "main (2)"
        );
        assert_eq!(RunConfig::unique_name("a (b)", &["a (b)"]), "a (b) (1)");
    }

    #[test]
    fn unittest_targets_for_the_command_line() {
        let root = Path::new("/proj");
        assert_eq!(
            unittest_cli_target("tests/test_x.py::Case::test_a", root),
            "tests.test_x.Case.test_a"
        );
        assert_eq!(
            unittest_cli_target("/proj/tests/test_x.py", root),
            "tests.test_x"
        );
        assert_eq!(
            unittest_cli_target("./test_x.py::Outer::Inner", root),
            "test_x.Outer.Inner"
        );
        assert_eq!(
            unittest_cli_target("pkg/__init__.py::Case", root),
            "pkg.Case"
        );
        assert_eq!(
            unittest_cli_target("tests.test_x.Case", root),
            "tests.test_x.Case"
        );
        assert_eq!(unittest_cli_target("tests", root), "tests");
    }

    #[test]
    fn unittest_directories_are_discovered() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("tests/unit")).unwrap();
        std::fs::create_dir_all(root.join("pkg/tests")).unwrap();
        std::fs::write(root.join("pkg/__init__.py"), "").unwrap();
        std::fs::write(root.join("pkg/tests/__init__.py"), "").unwrap();
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(unittest_cli_args("", root), ["discover"]);
        assert_eq!(
            unittest_cli_args("tests", root),
            ["discover", "-s", &s(&root.join("tests"))]
        );
        assert_eq!(
            unittest_cli_args(&s(&root.join("tests/./unit")), root),
            ["discover", "-s", &s(&root.join("tests/unit"))]
        );
        // A package is discovered from the first directory above it that is not one.
        assert_eq!(
            unittest_cli_args("pkg/tests", root),
            [
                "discover",
                "-s",
                &s(&root.join("pkg/tests")),
                "-t",
                &s(root)
            ]
        );
        // Files, node ids and dotted names are test names.
        assert_eq!(
            unittest_cli_args("tests/test_x.py::Case", root),
            ["tests.test_x.Case"]
        );
        assert_eq!(
            unittest_cli_args("pkg.tests.test_y", root),
            ["pkg.tests.test_y"]
        );
    }

    #[test]
    fn same_target_resolves_paths_like_the_process() {
        let root = Path::new("/proj");
        let script = |s: &str, wd: Option<&str>| RunConfig {
            kind: RunConfigKind::Python { script: s.into() },
            working_dir: wd.map(PathBuf::from),
            ..Default::default()
        };
        // A relative script runs from the working directory (default: the root).
        assert!(script("main.py", None)
            .same_target(&RunConfig::for_script(Path::new("/proj/main.py")), root));
        assert!(script("../app/main.py", Some("/proj/tools"))
            .same_target(&script("/proj/app/./main.py", None), root));
        assert!(
            !script("main.py", Some("/proj/app")).same_target(&script("/proj/main.py", None), root)
        );
        assert!(
            script("../../x.py", Some("/proj/a/b")).same_target(&script("/proj/x.py", None), root)
        );
        // Test targets are relative to the root.
        let pytest = |t: &str, k: &str| RunConfig {
            kind: RunConfigKind::Pytest {
                target: t.into(),
                keywords: k.into(),
            },
            ..Default::default()
        };
        assert!(pytest("tests/test_a.py::TestK::test_x", "")
            .same_target(&pytest("./tests/test_a.py::TestK::test_x", ""), root));
        assert!(pytest("/proj/tests", "").same_target(&pytest("tests", ""), root));
        assert!(!pytest("tests/test_a.py::test_x", "")
            .same_target(&pytest("tests/test_a.py::test_y", ""), root));
        // A `-k` / `-m` selection makes it another run (it can deselect the target's tests).
        assert!(!pytest("tests", "slow").same_target(&pytest("tests", ""), root));
        assert!(pytest("tests", "slow").same_target(&pytest("tests", " slow "), root));
        let with_params = |k: &str, p: &str| RunConfig {
            parameters: p.into(),
            ..pytest("tests", k)
        };
        assert!(!with_params("", "-x -k slow").same_target(&pytest("tests", ""), root));
        assert!(with_params("", "-kslow -x").same_target(&pytest("tests", "slow"), root));
        assert!(!with_params("", "-m 'not slow'").same_target(&pytest("tests", ""), root));
        assert!(
            with_params("", "-x --maxfail 2").same_target(&pytest("tests", ""), root),
            "other options select the same tests"
        );
        let unittest = |p: &str| RunConfig {
            kind: RunConfigKind::Unittest {
                target: "tests".into(),
            },
            parameters: p.into(),
            ..Default::default()
        };
        assert!(!unittest("-k slow").same_target(&unittest(""), root));
        assert!(unittest("-v").same_target(&unittest(""), root));
        // Modules compare their parameters (doctest runs name the file there).
        let module = |m: &str, p: &str| RunConfig {
            kind: RunConfigKind::PythonModule { module: m.into() },
            parameters: p.into(),
            ..Default::default()
        };
        assert!(module("doctest", "-v a.py").same_target(&module("doctest", "-v a.py"), root));
        assert!(!module("doctest", "-v a.py").same_target(&module("doctest", "-v b.py"), root));
        assert!(!pytest("tests", "").same_target(
            &RunConfig {
                kind: RunConfigKind::Unittest {
                    target: "tests".into()
                },
                ..Default::default()
            },
            root
        ));
    }

    #[test]
    fn context_launches_never_replace_other_configurations() {
        let root = Path::new("/proj");
        let mut store = RunConfigStore::default();
        // A permanent configuration of main.py with parameters and environment.
        let mut main = RunConfig {
            name: "main".into(),
            kind: RunConfigKind::Python {
                script: "main.py".into(),
            },
            parameters: "--port 80".into(),
            ..Default::default()
        };
        main.env.insert("MODE".into(), "dev".into());
        store.configs.push(main.clone());
        // A permanent pytest configuration whose name a context launch would also use.
        let custom = RunConfig {
            name: "pytest in tests".into(),
            kind: RunConfigKind::Pytest {
                target: "tests/unit".into(),
                keywords: String::new(),
            },
            ..Default::default()
        };
        store.configs.push(custom.clone());
        // ▶ on main.py reuses the existing configuration of that script.
        assert_eq!(
            store.resolve_temporary(
                RunConfig::for_script(Path::new("/proj/main.py")),
                true,
                root
            ),
            main
        );
        // The Project view's "pytest in tests" (target `tests`) must not replace the
        // permanent "pytest in tests" (target `tests/unit`): it gets its own name.
        let ctx = store.resolve_temporary(RunConfig::for_pytest("tests", root), true, root);
        assert_eq!(
            (ctx.name.as_str(), ctx.temporary),
            ("pytest in tests (1)", true)
        );
        store.upsert(ctx);
        assert!(
            store.configs.contains(&custom),
            "the permanent configuration survives"
        );
        // Without reuse (Run Anything, Debug from the test tree) a same-named
        // permanent configuration is never replaced, even with the same target.
        let same = RunConfig {
            name: "main".into(),
            kind: RunConfigKind::Python {
                script: "/proj/main.py".into(),
            },
            temporary: true,
            ..Default::default()
        };
        assert_eq!(store.resolve_temporary(same, false, root).name, "main (1)");
        // A temporary configuration of the same target is refreshed in place.
        let tmp = RunConfig::for_pytest("tests/test_a.py::test_x", root);
        store.upsert(tmp.clone());
        let mut again = tmp.clone();
        again.env.insert("A".into(), "1".into());
        assert_eq!(store.resolve_temporary(again.clone(), false, root), again);
        // ...but another script of the same file name gets a unique name.
        store.upsert(RunConfig::for_script(Path::new("/proj/a/run.py")));
        assert_eq!(
            store
                .resolve_temporary(
                    RunConfig::for_script(Path::new("/proj/b/run.py")),
                    true,
                    root
                )
                .name,
            "run (1)"
        );
        // Permanent configurations (the dialog's Run) are launched as edited.
        let mut edited = main.clone();
        edited.parameters = "--port 81".into();
        assert_eq!(store.resolve_temporary(edited.clone(), true, root), edited);
    }

    #[test]
    fn input_file_problems() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("in.txt"), "x").unwrap();
        let mut cfg = RunConfig::default();
        assert_eq!(cfg.input_problem(dir.path()), None);
        cfg.redirect_input = Some(PathBuf::new());
        assert_eq!(
            cfg.input_problem(dir.path()),
            None,
            "an empty path is not used"
        );
        cfg.redirect_input = Some(PathBuf::from("in.txt"));
        assert_eq!(cfg.input_problem(dir.path()), None);
        cfg.redirect_input = Some(PathBuf::from("missing.txt"));
        assert!(cfg
            .input_problem(dir.path())
            .is_some_and(|p| p.contains("missing.txt")));
    }

    fn script_cfg(name: &str, script: &str, temporary: bool) -> RunConfig {
        RunConfig {
            name: name.into(),
            kind: RunConfigKind::Python {
                script: script.into(),
            },
            temporary,
            ..Default::default()
        }
    }

    #[test]
    fn context_runs_reuse_the_scripts_configuration() {
        let root = Path::new("/proj");
        let mut store = RunConfigStore::default();
        // A stored configuration that runs job.py in the Python Console with arguments.
        let mut job = script_cfg("Job (console)", "/proj/job.py", false);
        job.run_with_console = true;
        job.parameters = "a b".into();
        store.upsert(job.clone());
        store.upsert(script_cfg("other", "/proj/other.py", false));
        // Ctrl+Shift+F10 in job.py runs that configuration, not a fresh one.
        assert_eq!(
            store.context_config_for_script(Path::new("/proj/job.py"), root),
            job
        );
        assert_eq!(
            store.context_config_for_script(Path::new("/proj/./sub/../job.py"), root),
            job
        );
        // A relative script resolves against the working directory, else the project root.
        store.configs.clear();
        let mut rel = script_cfg("rel", "tools/run.py", false);
        rel.run_with_console = true;
        store.upsert(rel.clone());
        assert!(
            store
                .context_config_for_script(Path::new("/proj/tools/run.py"), root)
                .run_with_console
        );
        rel.working_dir = Some("/proj/tools".into());
        store.upsert(rel.clone());
        assert!(
            !store
                .context_config_for_script(Path::new("/proj/tools/run.py"), root)
                .run_with_console
        );
        assert!(
            store
                .context_config_for_script(Path::new("/proj/tools/tools/run.py"), root)
                .run_with_console
        );
        // The selected configuration wins over other matches; permanent ones over temporary ones.
        store.configs.clear();
        store.upsert(script_cfg("temp", "/proj/main.py", true));
        store.upsert(script_cfg("kept", "/proj/main.py", false));
        store.selected = None;
        assert_eq!(
            store
                .context_config_for_script(Path::new("/proj/main.py"), root)
                .name,
            "kept"
        );
        store.selected = Some("temp".into());
        assert_eq!(
            store
                .context_config_for_script(Path::new("/proj/main.py"), root)
                .name,
            "temp"
        );
        store.selected = Some("other".into());
        assert_eq!(
            store
                .context_config_for_script(Path::new("/proj/main.py"), root)
                .name,
            "kept"
        );
    }

    #[test]
    fn new_context_configurations_do_not_replace_others() {
        let root = Path::new("/proj");
        let mut store = RunConfigStore::default();
        // A permanent configuration named "main" that runs another script.
        let mut other = script_cfg("main", "/proj/app/main.py", false);
        other.parameters = "--port 80".into();
        store.upsert(other.clone());
        let cfg = store.context_config_for_script(Path::new("/proj/main.py"), root);
        assert_eq!((cfg.name.as_str(), cfg.temporary), ("main (1)", true));
        assert_eq!(
            cfg.script_path(root).as_deref(),
            Some(Path::new("/proj/main.py"))
        );
        store.upsert(cfg);
        assert_eq!(
            store.configs.iter().find(|c| c.name == "main"),
            Some(&other),
            "the other configuration is kept"
        );
        assert_eq!(store.unique_name("main"), "main (2)");
        assert_eq!(store.unique_name("fresh"), "fresh");
        // Without a clash the configuration is named after the script.
        assert_eq!(
            RunConfigStore::default().context_config_for_script(Path::new("/proj/main.py"), root),
            RunConfig::for_script(Path::new("/proj/main.py"))
        );
        // Persisting keeps the permanent configuration (temporary ones are not saved).
        let dir = tempfile::tempdir().unwrap();
        store.save(dir.path()).unwrap();
        assert_eq!(RunConfigStore::load(dir.path()).configs, vec![other]);
    }

    #[test]
    fn same_file_compares_normalised_paths() {
        assert!(same_file(Path::new("/a/b/../c.py"), Path::new("/a/c.py")));
        assert!(same_file(Path::new("/a/./c.py"), Path::new("/a/c.py")));
        assert!(same_file(Path::new("/../a.py"), Path::new("/a.py")));
        assert!(!same_file(Path::new("/a/c.py"), Path::new("/b/c.py")));
        assert!(!same_file(Path::new("../../x.py"), Path::new("x.py")));
        // Through a symbolic link.
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("real.py"), "").unwrap();
            std::os::unix::fs::symlink(dir.path().join("real.py"), dir.path().join("link.py"))
                .unwrap();
            assert!(same_file(
                &dir.path().join("link.py"),
                &dir.path().join("real.py")
            ));
        }
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn pytest_context_runs_reuse_and_never_replace_stored_configs() {
        let root = Path::new("/p");
        let mut store = RunConfigStore::default();
        let mut stored = RunConfig::for_pytest("test_x.py", root);
        stored.temporary = false;
        stored.parameters = "-x".into();
        let stored_name = stored.name.clone();
        store.upsert(stored);
        // The same target: the stored configuration (with its parameters) is used.
        let c = store.context_config_for_pytest("test_x.py", None, root);
        assert_eq!(
            (c.name.as_str(), c.parameters.as_str(), c.temporary),
            (stored_name.as_str(), "-x", false)
        );
        // Another target with a clashing name gets a unique name.
        let c = store.context_config_for_pytest("test_y.py", Some(&stored_name), root);
        assert_ne!(c.name, stored_name);
    }

    /// The Run/Debug Configurations dialog's "+ Python" / "+ pytest" add a
    /// configuration each time and never overwrite one (QA: "+ Python" after
    /// a removal replaced "Python 2" and lost its parameters).
    #[test]
    fn added_configurations_never_replace_existing_ones() {
        let root = Path::new("/p");
        let mut store = RunConfigStore::default();
        let python = || RunConfig {
            working_dir: Some(root.to_path_buf()),
            ..Default::default()
        };
        assert_eq!(store.add_new(python()), "Unnamed");
        assert_eq!(store.add_new(python()), "Unnamed (1)");
        store.configs[1].parameters = "--keep-me".into();
        store.configs.remove(0);
        assert_eq!(store.add_new(python()), "Unnamed");
        assert_eq!(store.add_new(python()), "Unnamed (2)");
        assert_eq!(
            store
                .configs
                .iter()
                .find(|c| c.name == "Unnamed (1)")
                .map(|c| c.parameters.as_str()),
            Some("--keep-me")
        );
        let mut pytest = RunConfig::for_pytest("", root);
        pytest.temporary = false;
        assert_eq!(store.add_new(pytest.clone()), "pytest");
        if let Some(RunConfigKind::Pytest { keywords, .. }) =
            store.configs.last_mut().map(|c| &mut c.kind)
        {
            *keywords = "slow".into();
        }
        assert_eq!(store.add_new(pytest), "pytest (1)");
        assert_eq!(store.selected.as_deref(), Some("pytest (1)"));
        assert!(
            matches!(&store.configs.iter().find(|c| c.name == "pytest").map(|c| &c.kind), Some(RunConfigKind::Pytest { keywords, .. }) if keywords == "slow")
        );
        let names: Vec<&str> = store.configs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Unnamed (1)",
                "Unnamed",
                "Unnamed (2)",
                "pytest",
                "pytest (1)"
            ]
        );
        // All of them are stored.
        let dir = tempfile::tempdir().unwrap();
        store.save(dir.path()).unwrap();
        assert_eq!(RunConfigStore::load(dir.path()).configs.len(), 5);
    }

    fn script(name: &str, script: &str) -> RunConfig {
        RunConfig {
            name: name.into(),
            kind: RunConfigKind::Python {
                script: PathBuf::from(script),
            },
            temporary: true,
            ..Default::default()
        }
    }

    /// QA: after six context runs the first Run tab's temporary configuration
    /// was evicted and Rerun did nothing.
    #[test]
    fn rerun_falls_back_to_the_tabs_own_configuration() {
        let own = script("args", "/p/args.py");
        let mut store = RunConfigStore::default();
        store.upsert(own.clone());
        for i in 0..5 {
            store.upsert(script(&format!("s{i}"), &format!("/p/s{i}.py")));
        }
        assert!(
            store.configs.iter().all(|c| c.name != "args"),
            "evicted by newer runs"
        );
        // Evicted: the tab's own configuration runs (as a temporary one).
        assert_eq!(
            store.config_for_rerun("args", Some(&own)),
            Some(own.clone())
        );
        let deleted = RunConfig {
            temporary: false,
            ..own.clone()
        };
        assert_eq!(
            store.config_for_rerun("args", Some(&deleted)),
            Some(own.clone())
        );
        // Still stored, maybe edited: the stored one.
        let edited = RunConfig {
            parameters: "--new".into(),
            ..own.clone()
        };
        store.upsert(edited.clone());
        assert_eq!(
            store.config_for_rerun("args", Some(&own)),
            Some(edited.clone())
        );
        assert_eq!(store.config_for_rerun("args", None), Some(edited));
        // The name now belongs to a configuration for another script.
        store.configs.clear();
        store.upsert(script("args", "/p/sub/args.py"));
        assert_eq!(
            store.config_for_rerun("args", Some(&own)),
            Some(own.clone())
        );
        assert_eq!(
            RunConfigStore::default().config_for_rerun("args", None),
            None
        );
        // A permanent configuration is the same one after any edit, its script included.
        let permanent = RunConfig {
            temporary: false,
            ..own.clone()
        };
        let moved = RunConfig {
            kind: RunConfigKind::Python {
                script: PathBuf::from("/p/v2/args.py"),
            },
            ..permanent.clone()
        };
        store.configs.clear();
        store.upsert(moved.clone());
        assert_eq!(
            store.config_for_rerun("args", Some(&permanent)),
            Some(moved)
        );
        // A temporary configuration with that name is not the deleted permanent one.
        store.configs.clear();
        store.upsert(script("args", "/p/sub/args.py"));
        assert_eq!(store.config_for_rerun("args", Some(&permanent)), Some(own));
    }

    /// A stored `-k` configuration of a test is not what the test's ▶ (or the
    /// Project view's Run on its directory) runs: its keywords could
    /// deselect the very test ("1 deselected", nothing run).
    #[test]
    fn context_runs_do_not_reuse_keyword_configurations() {
        let root = Path::new("/p");
        let mut store = RunConfigStore::default();
        let slow = RunConfig {
            name: "slow only".into(),
            kind: RunConfigKind::Pytest {
                target: "tests/test_a.py::test_x".into(),
                keywords: "slow".into(),
            },
            ..Default::default()
        };
        store.upsert(slow.clone());
        let c = store.context_config_for_pytest(
            "tests/test_a.py::test_x",
            Some("pytest for test_a.test_x"),
            root,
        );
        assert_eq!(
            (c.name.as_str(), &c.kind, c.temporary),
            (
                "pytest for test_a.test_x",
                &RunConfigKind::Pytest {
                    target: "tests/test_a.py::test_x".into(),
                    keywords: String::new()
                },
                true
            )
        );
        // The same with `-k` in the Parameters, for a directory and its default name.
        let mut dir = RunConfig::for_pytest("tests", root);
        dir.temporary = false;
        dir.parameters = "-k 'not slow'".into();
        store.upsert(dir.clone());
        let c = store.resolve_temporary(RunConfig::for_pytest("tests", root), true, root);
        assert_eq!(
            (c.name.as_str(), c.parameters.as_str(), c.temporary),
            ("pytest in tests (1)", "", true)
        );
        store.upsert(c);
        assert!(
            store.configs.contains(&slow) && store.configs.contains(&dir),
            "the stored configurations are kept"
        );
        // Without a selection the stored configuration is reused as before.
        let mut plain = slow.clone();
        plain.name = "x".into();
        plain.kind = RunConfigKind::Pytest {
            target: "tests/test_a.py::test_x".into(),
            keywords: String::new(),
        };
        store.upsert(plain.clone());
        assert_eq!(
            store.context_config_for_pytest(
                "tests/test_a.py::test_x",
                Some("pytest for test_a.test_x"),
                root
            ),
            plain
        );
    }
}
