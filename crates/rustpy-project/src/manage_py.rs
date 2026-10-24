//! Django `manage.py` task console model (Tools › Run manage.py Task…):
//! command / option / app-label completion, `manage.py help` output parsing,
//! custom management command and app label discovery, and per-project command
//! history. The UI lives in `rustpy-ui` (`app/manage_py_console.rs`); nothing
//! here evaluates project code except [`query_help`], which the UI only calls
//! for trusted projects.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::frameworks::Facet;

/// Commands whose positional arguments are application labels.
pub const APP_LABEL_COMMANDS: &[&str] = &[
    "migrate",
    "makemigrations",
    "test",
    "sqlmigrate",
    "showmigrations",
    "sqlsequencereset",
    "squashmigrations",
    "optimizemigration",
    "dumpdata",
];

/// Options every Django management command accepts.
pub const COMMON_OPTIONS: &[&str] = &[
    "--help",
    "--version",
    "--verbosity",
    "--settings",
    "--pythonpath",
    "--traceback",
    "--no-color",
    "--force-color",
    "--skip-checks",
];

/// Built-in option table used until (or when) `manage.py help <cmd>` is not
/// available. Common options are appended by [`fallback_options`].
fn builtin_options(command: &str) -> &'static [&'static str] {
    match command {
        "runserver" | "runserver_plus" | "testserver" => &[
            "--noreload",
            "--nothreading",
            "--insecure",
            "--ipv6",
            "--nostatic",
        ],
        "makemigrations" => &[
            "--dry-run",
            "--check",
            "--empty",
            "--name",
            "--merge",
            "--noinput",
            "--no-header",
            "--scriptable",
            "--update",
        ],
        "migrate" => &[
            "--plan",
            "--check",
            "--fake",
            "--fake-initial",
            "--run-syncdb",
            "--database",
            "--noinput",
            "--prune",
        ],
        "test" => &[
            "--keepdb",
            "--parallel",
            "--failfast",
            "--reverse",
            "--tag",
            "--exclude-tag",
            "--pattern",
            "--noinput",
            "--testrunner",
            "--debug-mode",
            "--debug-sql",
            "--shuffle",
            "--buffer",
            "-k",
        ],
        "shell" => &["--interface", "--command", "--no-startup"],
        "showmigrations" => &["--list", "--plan", "--database"],
        "sqlmigrate" => &["--backwards", "--database"],
        "squashmigrations" => &[
            "--no-optimize",
            "--noinput",
            "--squashed-name",
            "--no-header",
        ],
        "collectstatic" => &[
            "--noinput",
            "--no-post-process",
            "--ignore",
            "--dry-run",
            "--clear",
            "--link",
            "--no-default-ignore",
        ],
        "createsuperuser" => &["--username", "--email", "--noinput", "--database"],
        "changepassword" => &["--database"],
        "startapp" | "startproject" => &["--template", "--extension", "--name", "--exclude"],
        "dumpdata" => &[
            "--format",
            "--indent",
            "--database",
            "--exclude",
            "--natural-foreign",
            "--natural-primary",
            "--all",
            "--pks",
            "--output",
        ],
        "loaddata" => &[
            "--database",
            "--app",
            "--ignorenonexistent",
            "--exclude",
            "--format",
        ],
        "flush" => &["--noinput", "--database"],
        "check" => &[
            "--tag",
            "--list-tags",
            "--deploy",
            "--fail-level",
            "--database",
        ],
        "inspectdb" => &["--database", "--include-partitions", "--include-views"],
        "dbshell" | "sqlflush" | "sqlsequencereset" => &["--database"],
        "diffsettings" => &["--all", "--default", "--output"],
        "sendtestemail" => &["--managers", "--admins"],
        "makemessages" => &[
            "--locale",
            "--exclude",
            "--domain",
            "--all",
            "--extension",
            "--symlinks",
            "--ignore",
            "--no-wrap",
            "--no-location",
        ],
        "compilemessages" => &["--locale", "--exclude", "--use-fuzzy", "--ignore"],
        "createcachetable" => &["--database", "--dry-run"],
        _ => &[],
    }
}

/// Options for `command` without asking the interpreter.
pub fn fallback_options(command: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for option in builtin_options(command)
        .iter()
        .chain(crate::frameworks::options(command))
        .chain(COMMON_OPTIONS)
    {
        if !out.iter().any(|known| known == option) {
            out.push((*option).to_string());
        }
    }
    out
}

/// Built-in Django command names (the Frameworks dialog chips plus the rest of
/// `django.core.management` / `contrib` commands).
pub fn builtin_commands() -> Vec<String> {
    const MORE: &[&str] = &[
        "help",
        "changepassword",
        "compilemessages",
        "createcachetable",
        "makemessages",
        "optimizemigration",
        "remove_stale_contenttypes",
        "sqlflush",
        "sqlsequencereset",
        "squashmigrations",
        "startproject",
        "testserver",
    ];
    let mut out: BTreeSet<String> = crate::frameworks::COMMANDS
        .iter()
        .chain(MORE)
        .map(|c| (*c).to_string())
        .collect();
    // Extensions-only commands appear once discovered from `help --commands`.
    out.remove("runserver_plus");
    out.remove("shell_plus");
    out.into_iter().collect()
}

/// A management command found in `<app>/management/commands/<name>.py`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomCommand {
    pub name: String,
    pub path: PathBuf,
    /// Literal `parser.add_argument("--flag", …)` options of the command.
    pub options: Vec<String>,
}

/// Option strings of the literal `….add_argument("-x", "--flag", …)` calls
/// in a management command source.
pub fn command_arguments(source: &str) -> Vec<String> {
    use rustpy_syntax::ast::{self, AstNode};
    let parse = rustpy_syntax::parse(source);
    let mut out: Vec<String> = Vec::new();
    for call in parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .take(4096)
    {
        if !call.callee().is_some_and(|callee| {
            callee
                .syntax()
                .text()
                .to_string()
                .ends_with(".add_argument")
        }) {
            continue;
        }
        let Some(args) = call.arg_list() else {
            continue;
        };
        for arg in args.args().take(32) {
            if let ast::AnyArg::Positional(arg) = arg {
                if let Some(ast::Expr::String(value)) = arg.expr() {
                    let value = value.value();
                    if value.starts_with('-') && value.len() > 1 && !out.contains(&value) {
                        out.push(value);
                    }
                }
            }
        }
    }
    out
}

fn skipped_dir(name: &str) -> bool {
    matches!(
        name,
        "venv"
            | ".venv"
            | "env"
            | "node_modules"
            | "site-packages"
            | "__pycache__"
            | "build"
            | "dist"
    ) || name.starts_with('.')
}

/// Custom management commands under the project (virtual environments and
/// hidden / ignored directories excluded).
pub fn discover_custom_commands(root: &Path) -> Vec<CustomCommand> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .max_depth(Some(10))
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|t| t.is_dir())
                || !skipped_dir(&entry.file_name().to_string_lossy())
        })
        .build();
    for entry in walker.flatten().take(200_000) {
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file())
            || path.extension().and_then(|e| e.to_str()) != Some("py")
        {
            continue;
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        if parent.file_name().and_then(|n| n.to_str()) != Some("commands")
            || parent
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                != Some("management")
        {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('_') || !is_identifier(name) {
            continue;
        }
        out.push(CustomCommand {
            name: name.to_string(),
            path: path.to_path_buf(),
            options: read_limited(path)
                .map(|source| command_arguments(&source))
                .unwrap_or_default(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(rustpy_core::is_ident_start)
        && chars.all(rustpy_core::is_ident_continue)
}

fn read_limited(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut source = String::new();
    let file = std::fs::File::open(path).ok()?;
    file.take(1024 * 1024 + 1)
        .read_to_string(&mut source)
        .ok()?;
    (source.len() <= 1024 * 1024).then_some(source)
}

/// String entries of every literal `INSTALLED_APPS = [...]` /
/// `INSTALLED_APPS += [...]` in a settings source (never evaluated).
pub fn installed_apps(source: &str) -> Vec<String> {
    use rustpy_syntax::ast::{self, AstNode};
    let parse = rustpy_syntax::parse(source);
    let mut out = Vec::new();
    for stmt in parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Stmt::cast)
    {
        let value = match &stmt {
            ast::Stmt::Assign(assign)
                if assign.targets().iter().any(
                    |t| matches!(t, ast::Expr::Name(n) if n.name_text() == "INSTALLED_APPS"),
                ) =>
            {
                assign.value()
            }
            ast::Stmt::AugAssign(assign) if matches!(assign.target(), Some(ast::Expr::Name(n)) if n.name_text() == "INSTALLED_APPS") => {
                assign.value()
            }
            ast::Stmt::AnnAssign(assign) if matches!(assign.target(), Some(ast::Expr::Name(n)) if n.name_text() == "INSTALLED_APPS") => {
                assign.value()
            }
            _ => None,
        };
        let Some(value) = value else { continue };
        for string in value
            .syntax()
            .descendants()
            .filter_map(ast::StringExpr::cast)
            .take(1024)
        {
            let app = string.value();
            if is_dotted(&app) && !out.contains(&app) {
                out.push(app);
            }
        }
    }
    out
}

fn is_dotted(text: &str) -> bool {
    !text.is_empty() && text.split('.').all(is_identifier)
}

/// An `AppConfig` subclass declared in an `apps.py`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfigInfo {
    pub class: String,
    pub name: Option<String>,
    pub label: Option<String>,
    pub default: bool,
}

impl AppConfigInfo {
    /// The Django app label: `label`, else the last component of `name`.
    pub fn app_label(&self) -> Option<String> {
        self.label.clone().or_else(|| {
            self.name
                .as_deref()
                .and_then(|n| n.rsplit('.').next())
                .map(str::to_string)
        })
    }
}

/// `AppConfig` subclasses of an `apps.py` source with their literal
/// `name` / `label` / `default` attributes.
pub fn app_configs(source: &str) -> Vec<AppConfigInfo> {
    use rustpy_syntax::ast::{self, AstNode};
    let parse = rustpy_syntax::parse(source);
    let mut out = Vec::new();
    for class in parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ClassDef::cast)
        .take(256)
    {
        if !class.bases().iter().any(|base| {
            let text = base.syntax().text().to_string();
            text == "AppConfig" || text.ends_with(".AppConfig") || text.ends_with("Config")
        }) {
            continue;
        }
        let mut info = AppConfigInfo {
            class: class.name_text(),
            name: None,
            label: None,
            default: false,
        };
        for stmt in class
            .body()
            .into_iter()
            .flat_map(|b| b.statements().collect::<Vec<_>>())
        {
            let (target, value) = match &stmt {
                ast::Stmt::Assign(a) => (a.targets().into_iter().next(), a.value()),
                ast::Stmt::AnnAssign(a) => (a.target(), a.value()),
                _ => continue,
            };
            let (Some(ast::Expr::Name(target)), Some(value)) = (target, value) else {
                continue;
            };
            match (target.name_text().as_str(), value) {
                ("name", ast::Expr::String(s)) => info.name = Some(s.value()),
                ("label", ast::Expr::String(s)) => info.label = Some(s.value()),
                ("default", ast::Expr::Literal(l)) => info.default = l.syntax().text() == "True",
                ("default", ast::Expr::Name(n)) => info.default = n.name_text() == "True",
                _ => {}
            }
        }
        out.push(info);
    }
    out
}

/// Python source files of a dotted settings module: `pkg/settings.py`, or
/// every module of a `pkg/settings/` package (split settings).
fn settings_files(root: &Path, module: &str) -> Vec<PathBuf> {
    if !is_dotted(module) {
        return Vec::new();
    }
    let base = root.join(module.replace('.', "/"));
    let file = base.with_extension("py");
    if file.is_file() {
        return vec![file];
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("py"))
        .take(64)
        .collect();
    files.sort();
    files
}

/// The app label of one `INSTALLED_APPS` entry, resolving project `apps.py`
/// AppConfig classes (`blog.apps.BlogConfig`, or `blog` with a default config).
pub fn app_label_for_entry(root: &Path, entry: &str) -> String {
    let parts: Vec<&str> = entry.split('.').collect();
    let last = parts.last().copied().unwrap_or(entry);
    let configs_in = |module: &[&str]| -> Vec<AppConfigInfo> {
        let path = root.join(module.join("/")).with_extension("py");
        read_limited(&path)
            .map(|s| app_configs(&s))
            .unwrap_or_default()
    };
    if last.chars().next().is_some_and(char::is_uppercase) && parts.len() >= 2 {
        // Dotted path to an AppConfig class.
        let module = &parts[..parts.len() - 1];
        if let Some(info) = configs_in(module).into_iter().find(|c| c.class == last) {
            if let Some(label) = info.app_label() {
                return label;
            }
        }
        let package = if module.last() == Some(&"apps") && module.len() >= 2 {
            module[module.len() - 2]
        } else {
            module.last().copied().unwrap_or(last)
        };
        return package.to_string();
    }
    let mut apps_module = parts.clone();
    apps_module.push("apps");
    let configs = configs_in(&apps_module);
    let chosen = configs
        .iter()
        .find(|c| c.default)
        .or_else(|| (configs.len() == 1).then(|| &configs[0]));
    chosen
        .and_then(|c| c.app_label())
        .unwrap_or_else(|| last.to_string())
}

/// App labels for `migrate` / `makemigrations` / `test` / … completion:
/// literal `INSTALLED_APPS` of the settings module (resolved through
/// `apps.py`), then project AppConfig classes not listed there.
pub fn discover_app_labels(root: &Path, facet: &Facet) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    let push = |label: String, labels: &mut Vec<String>| {
        if is_identifier(&label) && !labels.contains(&label) {
            labels.push(label);
        }
    };
    for file in settings_files(root, &facet.settings(root)) {
        let Some(source) = read_limited(&file) else {
            continue;
        };
        for entry in installed_apps(&source) {
            push(app_label_for_entry(root, &entry), &mut labels);
        }
    }
    let walker = ignore::WalkBuilder::new(root)
        .max_depth(Some(6))
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|t| t.is_dir())
                || !skipped_dir(&entry.file_name().to_string_lossy())
        })
        .build();
    for entry in walker.flatten().take(100_000) {
        if entry.file_name() != "apps.py" || !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Some(source) = read_limited(entry.path()) else {
            continue;
        };
        for info in app_configs(&source) {
            if let Some(label) = info.app_label() {
                push(label, &mut labels);
            }
        }
    }
    labels
}

/// Command names from `manage.py help --commands` (one per line) or the
/// grouped `manage.py help` listing (`[app]` headers, indented names).
pub fn parse_command_list(output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let name = line.trim();
        if is_identifier(name) && !out.iter().any(|known| known == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// One option of a command's argparse help.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpOption {
    /// Every spelling, e.g. `["--noinput", "--no-input"]`.
    pub flags: Vec<String>,
    /// The value placeholder (`DATABASE`) when the option takes a value.
    pub metavar: Option<String>,
    pub help: String,
}

/// Parsed `manage.py help <command>` output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandHelp {
    pub options: Vec<HelpOption>,
    pub positionals: Vec<String>,
}

impl CommandHelp {
    /// Every option spelling in help order.
    pub fn flags(&self) -> Vec<String> {
        self.options
            .iter()
            .flat_map(|o| o.flags.iter().cloned())
            .collect()
    }
    /// Whether `flag` consumes the next word as its value.
    pub fn takes_value(&self, flag: &str) -> bool {
        self.options
            .iter()
            .any(|o| o.metavar.is_some() && o.flags.iter().any(|f| f == flag))
    }
}

/// Parses argparse `usage:` / `positional arguments:` / `options:` help.
pub fn parse_command_help(output: &str) -> CommandHelp {
    #[derive(PartialEq)]
    enum Section {
        Other,
        Positional,
        Options,
    }
    let mut help = CommandHelp::default();
    let mut section = Section::Other;
    for line in output.lines() {
        let trimmed = line.trim();
        if !line.starts_with(' ') && trimmed.ends_with(':') {
            let header = trimmed.trim_end_matches(':').to_ascii_lowercase();
            section = if header.starts_with("positional") {
                Section::Positional
            } else if header == "options"
                || header.contains("optional arguments")
                || header.ends_with("options")
            {
                Section::Options
            } else {
                Section::Other
            };
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        // `  --flag META    help` (two or more spaces separate the help text).
        let (spec, text) = match trimmed.find("  ") {
            Some(at) => (&trimmed[..at], trimmed[at..].trim()),
            None => (trimmed, ""),
        };
        match section {
            Section::Options if trimmed.starts_with('-') && indent <= 4 => {
                let mut option = HelpOption {
                    help: text.to_string(),
                    ..Default::default()
                };
                for part in spec.split(", ") {
                    let mut words = part.split_whitespace();
                    let Some(flag) = words.next() else { continue };
                    let flag = flag.split('=').next().unwrap_or(flag);
                    if flag.starts_with('-') && flag.len() > 1 {
                        option.flags.push(flag.to_string());
                    }
                    if let Some(meta) = words
                        .next()
                        .or_else(|| part.split_once('=').map(|(_, m)| m))
                    {
                        option.metavar = Some(meta.to_string());
                    }
                }
                if !option.flags.is_empty() {
                    help.options.push(option);
                }
            }
            Section::Positional if indent <= 4 => {
                let name = spec.trim();
                if is_identifier(name) && !help.positionals.iter().any(|p| p == name) {
                    help.positionals.push(name.to_string());
                }
            }
            Section::Options | Section::Positional => {
                // Wrapped help text of the previous entry.
                if section == Section::Options {
                    if let Some(last) = help.options.last_mut() {
                        if !last.help.is_empty() {
                            last.help.push(' ');
                        }
                        last.help.push_str(trimmed);
                    }
                }
            }
            Section::Other => {}
        }
    }
    help
}

/// Runs `manage.py help [args]` with the project interpreter (bounded time and
/// output). Executes project code, so callers must check project trust.
pub fn query_help(python: &Path, root: &Path, facet: &Facet, args: &[&str]) -> Option<String> {
    let mut command = crate::interpreter::interpreter_command(python);
    command
        .arg(facet.manage(root))
        .arg("help")
        .args(args)
        .current_dir(root)
        .envs(facet.environment(root))
        .env("PYTHONIOENCODING", "utf-8")
        .env("DJANGO_COLORS", "nocolor");
    let bytes = crate::probe::command_output(command, Duration::from_secs(20)).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Completion candidate kind (shown as a type hint in the popup).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    Command,
    Option,
    AppLabel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
}

/// What the console knows for completion.
#[derive(Clone, Debug, Default)]
pub struct CompletionSource {
    pub commands: Vec<String>,
    pub app_labels: Vec<String>,
    /// Parsed `help <command>` output, when queried.
    pub help: BTreeMap<String, CommandHelp>,
    /// Options read from custom command sources (see [`command_arguments`]).
    pub static_options: BTreeMap<String, Vec<String>>,
}

impl CompletionSource {
    pub fn options(&self, command: &str) -> Vec<String> {
        match self.help.get(command) {
            Some(help) if !help.options.is_empty() => help.flags(),
            _ => {
                let mut options = self
                    .static_options
                    .get(command)
                    .cloned()
                    .unwrap_or_default();
                for option in fallback_options(command) {
                    if !options.contains(&option) {
                        options.push(option);
                    }
                }
                options
            }
        }
    }
    fn takes_app_labels(&self, command: &str) -> bool {
        APP_LABEL_COMMANDS.contains(&command)
            || self.help.get(command).is_some_and(|h| {
                h.positionals
                    .iter()
                    .any(|p| p == "app_label" || p == "test_label" || p.starts_with("app_label"))
            })
    }
}

/// The candidates for the word that ends `line` and the byte offset where that
/// word starts (the replacement range is `start..line.len()`).
pub fn complete(line: &str, source: &CompletionSource) -> (usize, Vec<Candidate>) {
    let start = line
        .rfind(char::is_whitespace)
        .map(|i| i + line[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0);
    let word = &line[start..];
    let previous: Vec<&str> = line[..start].split_whitespace().collect();
    let rank = |items: Vec<String>, kind: CandidateKind| -> Vec<Candidate> {
        let lower = word.to_lowercase();
        let mut prefix = Vec::new();
        let mut inner = Vec::new();
        for item in items {
            if previous.iter().skip(1).any(|p| *p == item) {
                continue;
            }
            let candidate = item.to_lowercase();
            if candidate.starts_with(&lower) {
                prefix.push(Candidate { text: item, kind });
            } else if lower.trim_start_matches('-').len() >= 2
                && candidate.contains(lower.trim_start_matches('-'))
            {
                inner.push(Candidate { text: item, kind });
            }
        }
        prefix.extend(inner);
        prefix
    };
    let Some(command) = previous.first().copied() else {
        return (start, rank(source.commands.clone(), CandidateKind::Command));
    };
    if word.starts_with('-') {
        return (start, rank(source.options(command), CandidateKind::Option));
    }
    if command == "help" && previous.len() == 1 {
        return (start, rank(source.commands.clone(), CandidateKind::Command));
    }
    // The value of an option (`--database default`) is not an app label.
    if let Some(last) = previous.last().filter(|_| previous.len() > 1) {
        let takes_value = source
            .help
            .get(command)
            .map(|h| h.takes_value(last))
            .unwrap_or_else(|| {
                [
                    "--database",
                    "--settings",
                    "--pythonpath",
                    "--name",
                    "--verbosity",
                    "--tag",
                    "--pattern",
                    "--format",
                    "--output",
                    "--template",
                ]
                .contains(last)
            });
        if takes_value {
            return (start, Vec::new());
        }
    }
    if source.takes_app_labels(command) {
        let labels = source.app_labels.clone();
        // sqlmigrate / squashmigrations take one app label then a migration name.
        let labels_used = previous[1..].iter().filter(|p| !p.starts_with('-')).count();
        if matches!(
            command,
            "sqlmigrate" | "squashmigrations" | "optimizemigration"
        ) && labels_used >= 1
        {
            return (start, Vec::new());
        }
        return (start, rank(labels, CandidateKind::AppLabel));
    }
    (start, Vec::new())
}

/// Per-project command history (newest last), persisted under `.rustpy/`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub entries: Vec<String>,
}

pub const HISTORY_LIMIT: usize = 200;

impl History {
    pub fn path(root: &Path) -> PathBuf {
        root.join(".rustpy/manage_py_history.json")
    }
    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(Self::path(root))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let path = Self::path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
    /// Records a command (a repeated command moves to the end).
    pub fn push(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        self.entries.retain(|e| e != command);
        self.entries.push(command.to_string());
        if self.entries.len() > HISTORY_LIMIT {
            let excess = self.entries.len() - HISTORY_LIMIT;
            self.entries.drain(..excess);
        }
    }
    /// Up: the entry before `cursor` (`None` = the line being edited).
    pub fn older(&self, cursor: Option<usize>) -> Option<usize> {
        match cursor {
            None => self.entries.len().checked_sub(1),
            Some(i) => Some(i.saturating_sub(1)),
        }
    }
    /// Down: the entry after `cursor`; `None` returns to the edited line.
    pub fn newer(&self, cursor: Option<usize>) -> Option<usize> {
        cursor.and_then(|i| (i + 1 < self.entries.len()).then_some(i + 1))
    }
}

/// `help` output cached per project, interpreter and settings module, so a
/// reopened project completes custom commands before the query finishes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpCache {
    pub key: String,
    pub commands: Vec<String>,
    pub help: BTreeMap<String, CommandHelp>,
}

impl HelpCache {
    pub fn key(python: &Path, root: &Path, facet: &Facet) -> String {
        format!("{}|{}", python.display(), facet.settings(root))
    }
    pub fn path(root: &Path) -> PathBuf {
        root.join(".rustpy/manage_py_help.json")
    }
    /// The cache for `key`, or an empty one when absent or stale.
    pub fn load(root: &Path, key: &str) -> Self {
        std::fs::read_to_string(Self::path(root))
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .filter(|cache| cache.key == key)
            .unwrap_or_else(|| Self {
                key: key.to_string(),
                ..Default::default()
            })
    }
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let path = Self::path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// Built-in + discovered custom + interpreter-reported command names, sorted.
pub fn merged_commands(custom: &[CustomCommand], reported: &[String]) -> Vec<String> {
    let mut all: BTreeSet<String> = builtin_commands().into_iter().collect();
    all.extend(custom.iter().map(|c| c.name.clone()));
    all.extend(reported.iter().cloned());
    all.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn source() -> CompletionSource {
        CompletionSource {
            commands: merged_commands(
                &[CustomCommand {
                    name: "import_feeds".into(),
                    path: PathBuf::new(),
                    options: vec!["--since".into()],
                }],
                &[],
            ),
            app_labels: vec!["auth".into(), "blog".into(), "shop".into()],
            help: BTreeMap::new(),
            static_options: BTreeMap::from([("import_feeds".into(), vec!["--since".into()])]),
        }
    }

    fn texts(candidates: &[Candidate]) -> Vec<&str> {
        candidates.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn completes_command_names_with_custom_commands() {
        let (start, items) = complete("mak", &source());
        assert_eq!(start, 0);
        assert_eq!(texts(&items), ["makemessages", "makemigrations"]);
        let (_, items) = complete("imp", &source());
        assert_eq!(texts(&items), ["import_feeds"]);
        assert!(items.iter().all(|c| c.kind == CandidateKind::Command));
        let (_, items) = complete("", &source());
        assert!(texts(&items).contains(&"migrate"));
    }

    #[test]
    fn completes_options_and_skips_used_ones() {
        let (start, items) = complete("migrate --fa", &source());
        assert_eq!(start, 8);
        assert_eq!(texts(&items), ["--fake", "--fake-initial"]);
        let (_, items) = complete("migrate --fake --fa", &source());
        assert_eq!(texts(&items), ["--fake-initial"]);
        let (_, items) = complete("runserver --", &source());
        assert!(texts(&items).contains(&"--noreload"));
        assert!(texts(&items).contains(&"--settings"));
    }

    #[test]
    fn completes_app_labels_for_label_commands_only() {
        let (start, items) = complete("makemigrations b", &source());
        assert_eq!(start, 15);
        assert_eq!(texts(&items), ["blog"]);
        assert!(items.iter().all(|c| c.kind == CandidateKind::AppLabel));
        let (_, items) = complete("migrate blog ", &source());
        assert_eq!(texts(&items), ["auth", "shop"]);
        let (_, items) = complete("sqlmigrate blog ", &source());
        assert!(items.is_empty(), "migration name, not a label");
        let (_, items) = complete("migrate --database ", &source());
        assert!(items.is_empty(), "option value");
        let (_, items) = complete("runserver ", &source());
        assert!(items.is_empty());
        let (_, items) = complete("help mig", &source());
        assert_eq!(texts(&items)[0], "migrate");
        assert!(items.iter().all(|c| c.kind == CandidateKind::Command));
    }

    #[test]
    fn parsed_help_overrides_builtin_options() {
        let mut source = source();
        let (_, items) = complete("import_feeds --s", &source);
        assert_eq!(texts(&items), ["--since", "--settings", "--skip-checks"]);
        source.help.insert(
            "import_feeds".into(),
            parse_command_help("options:\n  --since DATE  Only newer feeds.\n  --dry-run\n"),
        );
        let (_, items) = complete("import_feeds --", &source);
        assert_eq!(texts(&items), ["--since", "--dry-run"]);
        let (_, items) = complete("import_feeds --since ", &source);
        assert!(items.is_empty());
    }

    const MIGRATE_HELP: &str =
        "usage: manage.py migrate [-h] [--noinput] [--database DATABASE] [--fake]
                         [app_label] [migration_name]

Updates database schema. Manages both apps with migrations and those without.

positional arguments:
  app_label             App label of an application to synchronize the
                        state.
  migration_name        Database state will be brought to the state after that
                        migration. Use the name \"zero\" to unapply all
                        migrations.

options:
  -h, --help            show this help message and exit
  --noinput, --no-input
                        Tells Django to NOT prompt the user for input of any
                        kind.
  --database DATABASE   Nominates a database to synchronize. Defaults to the
                        \"default\" database.
  --fake                Mark migrations as run without actually running them.
  --verbosity {0,1,2,3}, -v {0,1,2,3}
                        Verbosity level
";

    #[test]
    fn parses_argparse_help_output() {
        let help = parse_command_help(MIGRATE_HELP);
        assert_eq!(help.positionals, ["app_label", "migration_name"]);
        assert_eq!(
            help.flags(),
            [
                "-h",
                "--help",
                "--noinput",
                "--no-input",
                "--database",
                "--fake",
                "--verbosity",
                "-v"
            ]
        );
        assert!(help.takes_value("--database"));
        assert!(!help.takes_value("--fake"));
        let noinput = &help.options[1];
        assert_eq!(
            noinput.help,
            "Tells Django to NOT prompt the user for input of any kind."
        );
        // Python < 3.10 heading.
        let old = parse_command_help("optional arguments:\n  --plan  Shows a list.\n");
        assert_eq!(old.flags(), ["--plan"]);
    }

    #[test]
    fn parses_command_lists() {
        assert_eq!(
            parse_command_list("check\nmigrate\nimport_feeds\n"),
            ["check", "migrate", "import_feeds"]
        );
        let grouped = "\nType 'manage.py help <subcommand>' for help on a specific subcommand.\n\nAvailable subcommands:\n\n[auth]\n    changepassword\n    createsuperuser\n\n[django]\n    check\n";
        assert_eq!(
            parse_command_list(grouped),
            ["changepassword", "createsuperuser", "check"]
        );
    }

    #[test]
    fn discovers_custom_commands_outside_virtualenvs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "blog/management/commands/import_feeds.py",
            "class Command(BaseCommand):\n    def add_arguments(self, parser):\n        parser.add_argument('-s', '--since')\n        parser.add_argument('feed')\n",
        );
        write(root, "blog/management/commands/__init__.py", "");
        write(root, "blog/management/commands/_private.py", "");
        write(root, "shop/management/commands/reindex.py", "");
        write(root, "venv/lib/x/management/commands/vendored.py", "");
        write(root, "blog/commands/not_management.py", "");
        let found = discover_custom_commands(root);
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["import_feeds", "reindex"]);
        assert_eq!(found[0].options, ["-s", "--since"]);
    }

    #[test]
    fn discovers_app_labels_from_settings_and_app_configs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "site/settings.py",
            "INSTALLED_APPS = [\n  'django.contrib.auth',\n  \"blog.apps.BlogConfig\",\n  'shop',\n]\nINSTALLED_APPS += ['debug_toolbar']\n",
        );
        write(
            root,
            "blog/apps.py",
            "from django.apps import AppConfig\nclass BlogConfig(AppConfig):\n    name = 'blog'\n    label = 'weblog'\n",
        );
        write(
            root,
            "shop/apps.py",
            "from django.apps import AppConfig\nclass ShopConfig(AppConfig):\n    default_auto_field = 'x'\n    name = 'shop'\n",
        );
        write(
            root,
            "extra/apps.py",
            "import django.apps\nclass ExtraConfig(django.apps.AppConfig):\n    name = 'pkg.extra'\n",
        );
        let facet = Facet {
            settings_module: "site.settings".into(),
            ..serde_json::from_str("{}").unwrap()
        };
        assert_eq!(
            discover_app_labels(root, &facet),
            ["auth", "weblog", "shop", "debug_toolbar", "extra"]
        );
    }

    #[test]
    fn history_persists_dedupes_and_navigates() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::default();
        history.push("migrate");
        history.push("check");
        history.push("  migrate ");
        history.push("");
        assert_eq!(history.entries, ["check", "migrate"]);
        history.save(dir.path()).unwrap();
        let loaded = History::load(dir.path());
        assert_eq!(loaded, history);
        assert_eq!(loaded.older(None), Some(1));
        assert_eq!(loaded.older(Some(1)), Some(0));
        assert_eq!(loaded.older(Some(0)), Some(0));
        assert_eq!(loaded.newer(Some(0)), Some(1));
        assert_eq!(loaded.newer(Some(1)), None);
        assert_eq!(History::default().older(None), None);
        for i in 0..HISTORY_LIMIT + 5 {
            history.push(&format!("cmd{i}"));
        }
        assert_eq!(history.entries.len(), HISTORY_LIMIT);
        assert_eq!(
            history.entries.last().unwrap(),
            &format!("cmd{}", HISTORY_LIMIT + 4)
        );
    }

    #[test]
    fn help_cache_is_keyed_by_interpreter_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = HelpCache::load(dir.path(), "py|a.settings");
        assert!(cache.commands.is_empty());
        cache.commands = vec!["import_feeds".into()];
        cache
            .help
            .insert("migrate".into(), parse_command_help(MIGRATE_HELP));
        cache.save(dir.path()).unwrap();
        assert_eq!(HelpCache::load(dir.path(), "py|a.settings"), cache);
        assert!(HelpCache::load(dir.path(), "other|a.settings")
            .commands
            .is_empty());
    }
}
