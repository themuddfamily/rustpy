//! Static Jinja2 template checks: unknown filters/tests and undefined imported
//! macros. Project registrations are read from source text; nothing executes.
use super::super::Problem;
use super::documents::push;
use super::*;
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, OnceLock},
};

const CODE: &str = "JinjaTemplate";

/// Filters/tests a project registers on its Jinja environments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Registry {
    /// Python sources use Jinja2 (directly or through Flask/Quart/Starlette).
    jinja: bool,
    filters: HashSet<String>,
    tests: HashSet<String>,
    /// Registrations that cannot be enumerated statically.
    dynamic_filters: bool,
    dynamic_tests: bool,
}
/// Flask extensions known not to install template filters or tests.
const QUIET_EXTENSIONS: &[&str] = &[
    "flask_sqlalchemy",
    "flask_login",
    "flask_wtf",
    "flask_migrate",
    "flask_cors",
    "flask_restful",
    "flask_restx",
    "flask_mail",
    "flask_bcrypt",
    "flask_jwt_extended",
    "flask_caching",
    "flask_limiter",
    "flask_session",
    "flask_marshmallow",
    "flask_socketio",
    "flask_talisman",
    "flask_compress",
    "flask_testing",
];
fn literal(text: &str) -> Option<&str> {
    let text = text.trim_start();
    let quote = text.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let end = text[1..].find(quote)?;
    Some(&text[1..1 + end])
}
fn identifier(text: &str) -> Option<&str> {
    let text = text.trim_start();
    let end = text
        .find(|c: char| !rustpy_core::is_ident_continue(c))
        .unwrap_or(text.len());
    let name = &text[..end];
    name.starts_with(rustpy_core::is_ident_start)
        .then_some(name)
}
impl Registry {
    pub(super) fn scan_source(&mut self, source: &str) {
        if [
            "jinja2",
            "from flask",
            "import flask",
            "from quart",
            "Jinja2Templates",
        ]
        .iter()
        .any(|needle| source.contains(needle))
        {
            self.jinja = true;
        }
        let mut pending: Option<bool> = None; // Some(true) = filter decorator awaiting `def`.
        for line in source.lines().take(100_000) {
            let line = line.trim();
            if let Some(filter) = pending {
                if let Some(rest) = line
                    .strip_prefix("def ")
                    .or_else(|| line.strip_prefix("async def "))
                {
                    if let Some(name) = identifier(rest) {
                        self.add(filter, name);
                    }
                    pending = None;
                } else if !line.starts_with('@') && !line.is_empty() {
                    pending = None;
                }
            }
            for module in ["from ", "import "] {
                if let Some(rest) = line.strip_prefix(module) {
                    let name = rest.split([' ', '.', ',']).next().unwrap_or("");
                    if (name.starts_with("flask_") && !QUIET_EXTENSIONS.contains(&name))
                        || name.starts_with("flaskext")
                        || name == "django_jinja"
                        || name.starts_with("jinja2_")
                    {
                        self.dynamic_filters = true;
                        self.dynamic_tests = true;
                    }
                }
            }
            if line.contains("add_extension(") || line.contains("extensions") {
                let mut values = line.split(['\'', '"']).skip(1).step_by(2).peekable();
                if values.peek().is_none() && line.contains("add_extension(")
                    || values.any(|value| !value.starts_with("jinja2.ext"))
                {
                    self.dynamic_filters = true;
                    self.dynamic_tests = true;
                }
            }
            for (filter, plural, singular) in [
                (true, "filters", "template_filter"),
                (false, "tests", "template_test"),
            ] {
                let mut rest = line;
                while let Some(at) = rest.find(plural) {
                    let after = &rest[at + plural.len()..];
                    let attribute = rest[..at].ends_with('.');
                    if attribute {
                        if let Some(index) = after.strip_prefix('[') {
                            match literal(index) {
                                Some(name) => self.add(filter, name),
                                None => self.dynamic(filter),
                            }
                        } else if after.starts_with(".update")
                            || after.starts_with(".setdefault")
                            || after.trim_start().starts_with("=")
                                && !after.trim_start().starts_with("==")
                            || after.trim_start().starts_with("|=")
                        {
                            self.dynamic(filter);
                        }
                    }
                    rest = after;
                }
                if let Some(at) = line.find(singular) {
                    let after = &line[at + singular.len()..];
                    let add = line[..at].ends_with("add_") || line[..at].ends_with("add_app_");
                    let arguments = after.strip_prefix('(');
                    let named = arguments.and_then(|arguments| {
                        if add {
                            let (_, rest) = arguments.split_once(',')?;
                            literal(rest.trim_start().trim_start_matches("name=")).or_else(|| {
                                arguments
                                    .split_once(',')
                                    .and_then(|(function, _)| identifier(function))
                            })
                        } else {
                            literal(arguments.trim_start().trim_start_matches("name="))
                        }
                    });
                    match (named, add) {
                        (Some(name), _) => self.add(filter, name),
                        (None, true) => match arguments.and_then(identifier) {
                            Some(name) => self.add(filter, name),
                            None => self.dynamic(filter),
                        },
                        (None, false) if line.starts_with('@') => pending = Some(filter),
                        _ => {}
                    }
                }
            }
        }
    }
    fn add(&mut self, filter: bool, name: &str) {
        if filter {
            self.filters.insert(name.to_string());
        } else {
            self.tests.insert(name.to_string());
        }
    }
    fn dynamic(&mut self, filter: bool) {
        if filter {
            self.dynamic_filters = true;
        } else {
            self.dynamic_tests = true;
        }
    }
}
/// Bounded scan of the project's Python sources, cached per project revision.
fn registry(root: &Path, revision: u64) -> std::sync::Arc<Registry> {
    type Cache = HashMap<(PathBuf, u64), std::sync::Arc<Registry>>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (root.to_path_buf(), revision);
    if let Some(found) = cache.lock().ok().and_then(|cache| cache.get(&key).cloned()) {
        return found;
    }
    let mut registry = Registry::default();
    let mut pending = vec![root.to_path_buf()];
    let (mut entries, mut files, mut bytes) = (0usize, 0usize, 0usize);
    while let Some(directory) = pending.pop() {
        let Ok(children) = std::fs::read_dir(&directory) else {
            continue;
        };
        for child in children.flatten() {
            entries += 1;
            if entries > 8192 || files > 1024 || bytes > 16 * 1024 * 1024 {
                break;
            }
            let path = child.path();
            let name = child.file_name();
            let name = name.to_string_lossy();
            let Ok(kind) = child.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.')
                    && !matches!(
                        name.as_ref(),
                        "node_modules"
                            | "site-packages"
                            | "venv"
                            | "__pycache__"
                            | "build"
                            | "dist"
                    )
                {
                    pending.push(path);
                }
            } else if kind.is_file() && name.ends_with(".py") {
                if let Some(source) = super::read(&path) {
                    files += 1;
                    bytes += source.len();
                    registry.scan_source(&source);
                }
            }
        }
    }
    let registry = std::sync::Arc::new(registry);
    if let Ok(mut cache) = cache.lock() {
        if cache.len() > 16 {
            cache.clear();
        }
        cache.insert(key, registry.clone());
    }
    registry
}

/// Identifier and single-character symbol tokens outside string literals,
/// with offsets relative to `text`.
fn tokens(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut chars = text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        if out.len() >= 4096 {
            break;
        }
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == open {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if rustpy_core::is_ident_start(ch) {
            let mut end = at + ch.len_utf8();
            while let Some(&(next, c)) = chars.peek() {
                if !rustpy_core::is_ident_continue(c) {
                    break;
                }
                end = next + c.len_utf8();
                chars.next();
            }
            out.push((at, &text[at..end]));
        } else if ch.is_ascii_digit() {
            while chars
                .peek()
                .is_some_and(|(_, c)| c.is_ascii_alphanumeric() || *c == '.')
            {
                chars.next();
            }
        } else if !ch.is_whitespace() {
            out.push((at, &text[at..at + ch.len_utf8()]));
        }
    }
    out
}
fn span(offset: usize, name: &str) -> TextRange {
    TextRange::new(
        TextSize::new(offset as u32),
        TextSize::new((offset + name.len()) as u32),
    )
}

pub(super) fn inspect(source: &str, path: &Path, inputs: &Inputs, out: &mut Vec<Problem>) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let in_templates = path.components().any(|p| p.as_os_str() == "templates");
    let jinja_extension = matches!(ext, "j2" | "jinja" | "jinja2");
    let configured = inputs.template_engine == "Jinja2";
    let django = inputs.django_template_engine && !configured;
    let markup = matches!(ext, "html" | "htm" | "xml" | "txt") && in_templates;
    if !(jinja_extension || markup && (configured || !django)) {
        return;
    }
    let registry = inputs
        .root
        .as_deref()
        .map(|root| registry(root, inputs.project_revision))
        .unwrap_or_default();
    // Unknown filters/tests only when the project is known to render with
    // Jinja2 (not e.g. Ansible `.j2` files with their own filter plugins).
    let names = configured || registry.jinja && !django;
    macros(source, inputs, out);
    if !names {
        return;
    }
    let mut filters: HashSet<&str> = rustpy_semantic::template_jinja::FILTERS
        .iter()
        .copied()
        .collect();
    filters.extend(registry.filters.iter().map(String::as_str));
    filters.extend(inputs.template_filters.iter().map(String::as_str));
    let mut tests: HashSet<&str> = rustpy_semantic::template_jinja::TESTS
        .iter()
        .copied()
        .collect();
    tests.extend(registry.tests.iter().map(String::as_str));
    use rustpy_syntax::template::{self, Dialect, Kind};
    let document = template::parse(source, Dialect::Jinja);
    for region in document.regions.iter().take(10000) {
        if !matches!(region.kind, Kind::Statement | Kind::Expression) {
            continue;
        }
        let base = region.content.start;
        let tokens = tokens(&source[region.content.clone()]);
        let statement = region.kind == Kind::Statement;
        let tag = tokens
            .iter()
            .find(|(_, t)| !matches!(*t, "-" | "+"))
            .map(|(_, t)| *t);
        for (index, (_, token)) in tokens.iter().enumerate() {
            let next = tokens.get(index + 1);
            // A filter name follows `|`, or the `filter` tag of a filter block.
            let filter_tag =
                statement && tag == Some("filter") && Some(*token) == tag && index <= 1;
            let filter = if *token == "|" || filter_tag {
                next
            } else {
                None
            };
            if let Some((at, name)) =
                filter.filter(|(_, name)| name.starts_with(rustpy_core::is_ident_start))
            {
                if !registry.dynamic_filters && !filters.contains(name) {
                    push(
                        out,
                        span(base + at, name),
                        CODE,
                        format!("Unknown Jinja2 filter '{name}'"),
                    );
                }
            }
            if *token == "is" && index > 0 && tokens[index - 1].1 != "." {
                let mut test = next;
                if test.is_some_and(|(_, t)| *t == "not") {
                    test = tokens.get(index + 2);
                }
                if let Some((at, name)) =
                    test.filter(|(_, name)| name.starts_with(rustpy_core::is_ident_start))
                {
                    if !registry.dynamic_tests && !tests.contains(name) {
                        push(
                            out,
                            span(base + at, name),
                            CODE,
                            format!("Unknown Jinja2 test '{name}'"),
                        );
                    }
                }
            }
        }
    }
}

/// `{% from 'x' import m %}` and `alias.m` where the resolved template defines
/// no such macro or variable.
fn macros(source: &str, inputs: &Inputs, out: &mut Vec<Problem>) {
    let Some(root) = &inputs.root else { return };
    let imports = rustpy_semantic::template_jinja::imports(source);
    if imports.is_empty() {
        return;
    }
    let mut aliases: Vec<(String, String, Vec<String>)> = Vec::new();
    for import in imports.iter().take(64) {
        let Some(path) = rustpy_project::frameworks::resource(
            root,
            &inputs.template_directories,
            &import.template,
            "templates",
        ) else {
            continue;
        };
        let Some(text) = super::read(&path) else {
            continue;
        };
        // Inherited or included definitions are not enumerated statically.
        if text.contains("extends") || text.contains("include") {
            continue;
        }
        let exports = rustpy_semantic::template_jinja::exports(&text);
        for name in &import.names {
            if !exports.contains(&name.name) {
                push(
                    out,
                    name.range,
                    CODE,
                    format!("Undefined macro '{}' in '{}'", name.name, import.template),
                );
            }
        }
        if let Some(alias) = &import.alias {
            aliases.push((alias.clone(), import.template.clone(), exports));
        }
    }
    if aliases.is_empty() {
        return;
    }
    use rustpy_syntax::template::{self, Dialect, Kind};
    let document = template::parse(source, Dialect::Jinja);
    for region in document.regions.iter().take(10000) {
        if !matches!(region.kind, Kind::Statement | Kind::Expression) {
            continue;
        }
        let base = region.content.start;
        let tokens = tokens(&source[region.content.clone()]);
        for window in tokens.windows(3) {
            let [(start, alias), (_, "."), (at, name)] = window else {
                continue;
            };
            if *start > 0 && source[base..base + start].ends_with('.') {
                continue;
            }
            if let Some((_, template, exports)) = aliases.iter().find(|(a, _, _)| a == alias) {
                if name.starts_with(rustpy_core::is_ident_start)
                    && !exports.iter().any(|export| export == name)
                {
                    push(
                        out,
                        span(base + at, name),
                        CODE,
                        format!("Undefined macro '{name}' in '{template}'"),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod jinja_fixtures {
    use super::*;
    fn codes(problems: &[Problem]) -> Vec<String> {
        problems
            .iter()
            .filter(|p| p.diagnostic.code.as_str() == CODE)
            .map(|p| p.diagnostic.message.clone())
            .collect()
    }
    #[test]
    fn registry_reads_static_filter_and_test_registrations() {
        let mut registry = Registry::default();
        registry.scan_source(
            "from flask import Flask\napp = Flask(__name__)\n@app.template_filter('money')\ndef fmt(v): return v\n\
@app.template_test()\ndef prime(n): return n\napp.jinja_env.filters[\"shout\"] = str.upper\n\
app.add_template_filter(reverse_words)\napp.add_template_test(check, 'palindrome')\n",
        );
        assert!(registry.jinja);
        assert!(!registry.dynamic_filters && !registry.dynamic_tests);
        for name in ["money", "shout", "reverse_words"] {
            assert!(registry.filters.contains(name), "{name}: {registry:?}");
        }
        for name in ["prime", "palindrome"] {
            assert!(registry.tests.contains(name), "{name}: {registry:?}");
        }
        let mut dynamic = Registry::default();
        dynamic.scan_source(
            "import jinja2\nenv = jinja2.Environment()\nenv.filters.update(FILTERS)\n",
        );
        assert!(dynamic.dynamic_filters && !dynamic.dynamic_tests);
        let mut extension = Registry::default();
        extension.scan_source("from flask_babel import Babel\n");
        assert!(extension.dynamic_filters);
        let mut quiet = Registry::default();
        quiet.scan_source("from flask_sqlalchemy import SQLAlchemy\nenv = Environment(extensions=['jinja2.ext.do'])\n");
        assert!(!quiet.dynamic_filters);
    }
    #[test]
    fn unknown_filters_tests_and_undefined_macros_are_reported() {
        let root = tempfile::tempdir().unwrap();
        let templates = root.path().join("templates");
        std::fs::create_dir_all(&templates).unwrap();
        std::fs::write(
            root.path().join("app.py"),
            "from flask import Flask\napp = Flask(__name__)\n@app.template_filter('money')\ndef money(v): return v\n",
        )
        .unwrap();
        std::fs::write(
            templates.join("forms.html"),
            "{% macro input(name) %}{% endmacro %}{% set version = 1 %}",
        )
        .unwrap();
        let inputs = Inputs {
            root: Some(root.path().to_path_buf()),
            project_revision: 7,
            ..Default::default()
        };
        let source =
            "{% import 'forms.html' as forms %}{% from 'forms.html' import input, missing %}\
{{ forms.input('a') }}{{ forms.version }}{{ forms.absent() }}\
{{ price|money|upper }}{{ name | shout }}{% filter bogus %}x{% endfilter %}\
{% if n is odd %}{% endif %}{% if n is not prime %}{% endif %}{{ 'a|fake is weird' }}";
        let path = templates.join("page.html");
        let mut out = Vec::new();
        inspect(source, &path, &inputs, &mut out);
        let messages = codes(&out);
        assert_eq!(
            messages,
            [
                "Undefined macro 'missing' in 'forms.html'",
                "Undefined macro 'absent' in 'forms.html'",
                "Unknown Jinja2 filter 'shout'",
                "Unknown Jinja2 filter 'bogus'",
                "Unknown Jinja2 test 'prime'",
            ],
            "{out:?}"
        );
        let shout = out
            .iter()
            .find(|p| p.diagnostic.message.contains("shout"))
            .unwrap()
            .diagnostic
            .range;
        assert_eq!(
            &source[usize::from(shout.start())..usize::from(shout.end())],
            "shout"
        );
        // Without Python evidence of Jinja2 (e.g. Ansible `.j2`), filter names
        // are not judged; unresolved imports are not either.
        let bare = tempfile::tempdir().unwrap();
        let inputs = Inputs {
            root: Some(bare.path().to_path_buf()),
            ..Default::default()
        };
        let mut out = Vec::new();
        inspect(
            "{{ x|to_yaml }}{% from 'nowhere.html' import m %}",
            &bare.path().join("site.j2"),
            &inputs,
            &mut out,
        );
        assert!(codes(&out).is_empty(), "{out:?}");
        // Django-configured projects keep their own template inspection.
        let inputs = Inputs {
            root: Some(root.path().to_path_buf()),
            django_template_engine: true,
            project_revision: 7,
            ..Default::default()
        };
        let mut out = Vec::new();
        inspect("{{ x|bogus }}", &path, &inputs, &mut out);
        assert!(codes(&out).is_empty(), "{out:?}");
    }
}
