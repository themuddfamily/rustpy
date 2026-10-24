//! Project-scoped framework policy. Paths remain project relative on disk.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Facet {
    pub django: bool,
    pub auto_detect: bool,
    pub settings_module: String,
    pub manage_script: PathBuf,
    pub track_models: bool,
    pub use_django_test_runner: bool,
    pub template_engine: String,
    pub template_data_language: String,
    pub static_directories: Vec<PathBuf>,
    pub django_template_debug: bool,
    pub jinja_debug: bool,
    pub host: String,
    pub port: u16,
    pub reload: bool,
    pub runserver_plus: bool,
    pub flask_app: String,
    pub flask_environment: String,
    pub flask_debug: bool,
    pub fastapi_app: String,
    pub additional_options: String,
}
impl Default for Facet {
    fn default() -> Self {
        Self {
            django: false,
            auto_detect: true,
            settings_module: String::new(),
            manage_script: "manage.py".into(),
            track_models: true,
            use_django_test_runner: true,
            template_engine: "Auto".into(),
            template_data_language: "HTML".into(),
            static_directories: vec!["static".into()],
            django_template_debug: true,
            jinja_debug: true,
            host: "127.0.0.1".into(),
            port: 8000,
            reload: true,
            runserver_plus: false,
            flask_app: "app".into(),
            flask_environment: "development".into(),
            flask_debug: true,
            fastapi_app: "main:app".into(),
            additional_options: String::new(),
        }
    }
}
impl Facet {
    pub fn manage(&self, root: &Path) -> PathBuf {
        if self.manage_script.is_absolute() {
            self.manage_script.clone()
        } else {
            root.join(&self.manage_script)
        }
    }
    pub fn settings(&self, root: &Path) -> String {
        if !self.settings_module.trim().is_empty() {
            return self.settings_module.trim().into();
        }
        detect_settings(&self.manage(root)).unwrap_or_default()
    }
    pub fn environment(&self, root: &Path) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        let module = self.settings(root);
        if !module.is_empty() {
            env.insert("DJANGO_SETTINGS_MODULE".into(), module);
        }
        env
    }
    pub fn enabled(&self, root: &Path) -> bool {
        self.django || (self.auto_detect && self.manage(root).is_file())
    }
}
pub fn detect_settings(manage: &Path) -> Option<String> {
    use std::io::Read;
    let mut source = String::new();
    std::fs::File::open(manage)
        .ok()?
        .take(262145)
        .read_to_string(&mut source)
        .ok()?;
    if source.len() > 262144 {
        return None;
    }
    let parse = rustpy_syntax::parse(&source);
    use rustpy_syntax::ast::{self, AstNode};
    for call in parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .take(4096)
    {
        if call
            .callee()
            .is_some_and(|expr| expr.syntax().text().to_string().ends_with(".setdefault"))
        {
            let args = call
                .arg_list()?
                .args()
                .filter_map(|arg| match arg {
                    ast::AnyArg::Positional(arg) => arg.expr(),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let [ast::Expr::String(key), ast::Expr::String(value), ..] = args.as_slice() {
                if key.value() == "DJANGO_SETTINGS_MODULE" {
                    return Some(value.value());
                }
            }
        }
    }
    None
}
pub const COMMANDS: &[&str] = &[
    "runserver",
    "runserver_plus",
    "check",
    "shell",
    "shell_plus",
    "test",
    "makemigrations",
    "migrate",
    "showmigrations",
    "sqlmigrate",
    "collectstatic",
    "createsuperuser",
    "startapp",
    "dumpdata",
    "loaddata",
    "inspectdb",
    "dbshell",
    "clearsessions",
    "flush",
    "diffsettings",
    "sendtestemail",
];
pub fn options(command: &str) -> &'static [&'static str] {
    match command {
        "runserver" | "runserver_plus" => &["--noreload", "--nothreading", "--insecure", "--ipv6"],
        "makemigrations" => &["--dry-run", "--check", "--empty", "--name", "--merge"],
        "migrate" => &[
            "--plan",
            "--check",
            "--fake",
            "--fake-initial",
            "--run-syncdb",
            "--database",
        ],
        "test" => &[
            "--keepdb",
            "--parallel",
            "--failfast",
            "--reverse",
            "--tag",
            "--exclude-tag",
            "--pattern",
        ],
        "shell" => &["--interface", "--command", "--no-startup"],
        _ => &[
            "--settings",
            "--pythonpath",
            "--verbosity",
            "--traceback",
            "--no-color",
        ],
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn defaults_are_compatible_with_existing_project() {
        let facet: Facet = serde_json::from_str("{}").unwrap();
        assert_eq!(
            facet.manage(Path::new("/project")),
            Path::new("/project/manage.py")
        );
        assert!(facet.use_django_test_runner);
        assert!(options("migrate").contains(&"--plan"));
    }
}

/// Literal-only Django DATABASES discovery. Dynamic settings require user entry.
pub fn django_databases(root: &Path, facet: &Facet) -> Vec<(String, BTreeMap<String, String>)> {
    use std::io::Read;
    let module = facet.settings(root);
    if module.is_empty()
        || !module.split('.').all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(rustpy_core::is_ident_start)
                && chars.all(rustpy_core::is_ident_continue)
        })
    {
        return Vec::new();
    }
    let path = root.join(module.replace('.', "/")).with_extension("py");
    let mut source = String::new();
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if file
        .take(1024 * 1024 + 1)
        .read_to_string(&mut source)
        .is_err()
        || source.len() > 1024 * 1024
    {
        return Vec::new();
    }
    database_literals(&source)
}
pub fn database_literals(source: &str) -> Vec<(String, BTreeMap<String, String>)> {
    use rustpy_syntax::ast::{self, AstNode};
    let parse = rustpy_syntax::parse(source);
    let mut out = Vec::new();
    for assign in parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AssignStmt::cast)
        .take(4096)
    {
        if !assign
            .targets()
            .iter()
            .any(|target| matches!(target,ast::Expr::Name(name) if name.name_text()=="DATABASES"))
        {
            continue;
        }
        let Some(ast::Expr::Dict(databases)) = assign.value() else {
            continue;
        };
        for item in databases.items().take(64) {
            let (Some(ast::Expr::String(alias)), Some(ast::Expr::Dict(settings))) =
                (item.key(), item.value())
            else {
                continue;
            };
            let mut values = BTreeMap::new();
            for setting in settings.items().take(128) {
                let (Some(ast::Expr::String(key)), Some(value)) = (setting.key(), setting.value())
                else {
                    continue;
                };
                let key = key.value();
                if !["ENGINE", "NAME", "HOST", "PORT", "USER", "PASSWORD"].contains(&key.as_str()) {
                    continue;
                }
                let value = match value {
                    ast::Expr::String(value) => Some(value.value()),
                    ast::Expr::Literal(value) => Some(value.syntax().text().to_string()),
                    _ => None,
                };
                if let Some(value) = value {
                    values.insert(key, value);
                }
            }
            if values.contains_key("ENGINE") && values.contains_key("NAME") {
                out.push((alias.value(), values));
            }
        }
    }
    out
}
#[cfg(test)]
mod database_fixtures {
    use super::*;
    #[test]
    fn settings_imports_are_never_evaluated() {
        let values=database_literals("DATABASES={'default':{'ENGINE':'django.db.backends.sqlite3','NAME':'local.sqlite3','PASSWORD':get_secret()}}\n");
        assert_eq!(values[0].1["NAME"], "local.sqlite3");
        assert!(!values[0].1.contains_key("PASSWORD"));
    }
}

/// Resolve a project template/static file without evaluating settings or following traversal.
pub fn resource(root: &Path, configured: &[PathBuf], name: &str, folder: &str) -> Option<PathBuf> {
    if name.is_empty()
        || !Path::new(name)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    resource_directories(root, configured, folder)
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}
/// Lookup roots for `folder` resources: configured directories, then
/// `<root>/<folder>` and each top-level app's `<app>/<folder>`.
pub fn resource_directories(root: &Path, configured: &[PathBuf], folder: &str) -> Vec<PathBuf> {
    let mut directories = configured.to_vec();
    directories.push(root.join(folder));
    if let Ok(children) = std::fs::read_dir(root) {
        for child in children.take(128).flatten() {
            if child.file_type().is_ok_and(|kind| kind.is_dir()) {
                directories.push(child.path().join(folder));
            }
        }
    }
    directories
}
/// Project-relative `TEMPLATES[...]["DIRS"]` entries of a Django settings
/// module, read statically: each entry's string literals are joined as path
/// segments (`BASE_DIR / "templates"`, `os.path.join(BASE_DIR, "a", "b")`).
pub fn settings_template_dirs(root: &Path, module: &str) -> Vec<PathBuf> {
    use std::io::Read;
    if module.is_empty()
        || !module
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphanumeric() || c == '_'))
    {
        return Vec::new();
    }
    let base = root.join(module.replace('.', "/"));
    let Some(source) = [base.with_extension("py"), base.join("__init__.py")]
        .iter()
        .find_map(|path| {
            let mut source = String::new();
            std::fs::File::open(path)
                .ok()?
                .take(262145)
                .read_to_string(&mut source)
                .ok()?;
            (source.len() <= 262144).then_some(source)
        })
    else {
        return Vec::new();
    };
    let mut directories = Vec::new();
    let mut rest = source.as_str();
    while let Some(at) = rest.find("DIRS") {
        let before = &rest[..at];
        let after = &rest[at + 4..];
        rest = after;
        // `STATICFILES_DIRS` and other settings are not template roots.
        if before.ends_with(|c: char| c.is_alphanumeric() || c == '_')
            && !before.ends_with("TEMPLATE_")
        {
            continue;
        }
        let after = after.trim_start_matches(['\'', '"']).trim_start();
        let Some(after) = after.strip_prefix(':').or_else(|| after.strip_prefix('=')) else {
            continue;
        };
        let Some(list) = after.trim_start().strip_prefix(['[', '(']) else {
            continue;
        };
        let (mut depth, mut quote, mut literal, mut segments) =
            (0usize, None, String::new(), Vec::new());
        for ch in list.chars().take(4096) {
            if let Some(open) = quote {
                if ch == open {
                    segments.push(std::mem::take(&mut literal));
                    quote = None;
                } else {
                    literal.push(ch);
                }
                continue;
            }
            match ch {
                '\'' | '"' => quote = Some(ch),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => break,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    directories.push(std::mem::take(&mut segments));
                }
                _ => {}
            }
        }
        directories.push(segments);
    }
    directories
        .into_iter()
        .filter(|segments| !segments.is_empty())
        .map(|segments| {
            let path = PathBuf::from(segments.join("/"));
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .filter(|path| {
            path.components()
                .all(|part| !matches!(part, std::path::Component::ParentDir))
        })
        .take(64)
        .collect()
}
/// Template names (forward-slash paths relative to a template root) that
/// [`resource`] resolves, bounded for interactive completion.
pub fn template_names(root: &Path, configured: &[PathBuf]) -> Vec<String> {
    let mut names = Vec::new();
    let mut entries = 0usize;
    for directory in resource_directories(root, configured, "templates") {
        let mut pending = vec![directory.clone()];
        while let Some(current) = pending.pop() {
            let Ok(children) = std::fs::read_dir(&current) else {
                continue;
            };
            for child in children.flatten() {
                entries += 1;
                if entries > 8192 || names.len() >= 4096 {
                    break;
                }
                let path = child.path();
                let Ok(kind) = child.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if !child.file_name().to_string_lossy().starts_with('.') {
                        pending.push(path);
                    }
                } else if kind.is_file() {
                    if let Ok(relative) = path.strip_prefix(&directory) {
                        let parts: Vec<_> = relative
                            .components()
                            .map(|part| part.as_os_str().to_string_lossy())
                            .collect();
                        names.push(parts.join("/"));
                    }
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}
#[cfg(test)]
mod template_name_fixtures {
    use super::*;
    #[test]
    fn settings_template_dirs_read_django_dirs_entries_only() {
        let dir = tempfile::TempDir::with_prefix("rustpy-settings-dirs").unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("site")).unwrap();
        std::fs::write(
            root.join("site/settings.py"),
            "import os\nfrom pathlib import Path\nBASE_DIR = Path(__file__).resolve().parent.parent\nSTATICFILES_DIRS = [BASE_DIR / 'static']\nTEMPLATES = [{\n    'BACKEND': 'django.template.backends.django.DjangoTemplates',\n    'DIRS': [BASE_DIR / 'frontend' / 'html', os.path.join(BASE_DIR, \"mail\")],\n    'APP_DIRS': True,\n}]\n",
        )
        .unwrap();
        assert_eq!(
            settings_template_dirs(root, "site.settings"),
            [root.join("frontend/html"), root.join("mail")]
        );
        assert!(settings_template_dirs(root, "missing.settings").is_empty());
        assert!(settings_template_dirs(root, "../site").is_empty());
    }
    #[test]
    fn template_names_are_relative_to_every_resolvable_template_root() {
        let dir = tempfile::TempDir::with_prefix("rustpy-template-names").unwrap();
        let root = dir.path();
        for file in [
            "templates/base.html",
            "blog/templates/blog/post.html",
            "custom/emails/welcome.txt",
            "templates/.cache/skip.html",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
        let names = template_names(root, &[root.join("custom")]);
        assert_eq!(names, ["base.html", "blog/post.html", "emails/welcome.txt"]);
        for name in &names {
            assert!(resource(root, &[root.join("custom")], name, "templates").is_some());
        }
    }
}
