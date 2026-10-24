//! Basic completion context wiring, document schemas, templates and PyPI jobs.
use super::*;
use std::io::Read;

#[derive(Default)]
pub(super) struct BasicCompletionUi {
    packages: Option<PackageCompletionJob>,
    worker: Option<PackageWorker>,
}
struct PackageCompletionJob {
    editor: egui::Id,
    version: u64,
    caret: TextSize,
    start: TextSize,
    explicit: bool,
    receiver: crossbeam_channel::Receiver<Result<Vec<CompletionItem>, String>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
impl Drop for PackageCompletionJob {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
#[derive(Clone)]
enum PackageQuery {
    Names(String),
    Versions(String, String),
}
struct QueuedPackageQuery {
    query: PackageQuery,
    /// `(name, version)` installed in the project interpreter: offered first,
    /// and still offered when PyPI cannot be reached.
    installed: Vec<(String, String)>,
    reply: crossbeam_channel::Sender<Result<Vec<CompletionItem>, String>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
struct PackageWorker {
    pending: Arc<std::sync::Mutex<Option<QueuedPackageQuery>>>,
    wake: crossbeam_channel::Sender<()>,
}
impl PackageWorker {
    fn new() -> Self {
        let pending = Arc::new(std::sync::Mutex::new(None::<QueuedPackageQuery>));
        let work = pending.clone();
        let (wake, receiver) = crossbeam_channel::bounded(1);
        let _ = std::thread::Builder::new()
            .name("rustpy-package-completion".into())
            .spawn(move || {
                while receiver.recv().is_ok() {
                    loop {
                        let next = work
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take();
                        let Some(next) = next else {
                            break;
                        };
                        if next.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                            continue;
                        }
                        let result = package_items(next.query, &next.installed);
                        if !next.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                            let _ = next.reply.send(result);
                        }
                    }
                }
            });
        Self { pending, wake }
    }
    fn submit(&self, query: QueuedPackageQuery) {
        *self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(query);
        let _ = self.wake.try_send(());
    }
}
#[derive(Default)]
struct PackageIndex {
    loaded: Option<std::time::Instant>,
    names: Arc<Vec<String>>,
}
static PACKAGE_NAMES: std::sync::OnceLock<std::sync::Mutex<PackageIndex>> =
    std::sync::OnceLock::new();
fn lookup_item(label: &str, detail: &str) -> CompletionItem {
    CompletionItem {
        label: label.into(),
        kind: CompletionKind::Variable,
        detail: detail.into(),
        insert: label.into(),
        caret_offset: None,
        priority: 0,
    }
}
fn package_query(text: &str, caret: usize) -> Option<(usize, PackageQuery)> {
    let before = text.get(..caret)?;
    let start = before.rfind('\n').map_or(0, |at| at + 1);
    let line = &before[start..];
    let trimmed = line.trim_start();
    if trimmed.starts_with(['#', '-']) || line.contains([';', '@']) || line.contains("://") {
        return None;
    }
    let leading = line.len() - trimmed.len();
    let name_end = trimmed
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')))
        .unwrap_or(trimmed.len());
    let package = &trimmed[..name_end];
    if name_end == trimmed.len() {
        return Some((start + leading, PackageQuery::Names(package.into())));
    }
    let rest = &trimmed[name_end..];
    let operator = rest.rfind(['=', '>', '<', '~', '!'])?;
    let tail = &rest[operator + 1..];
    let whitespace = tail.len() - tail.trim_start().len();
    let prefix = tail.trim_start();
    if !prefix
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '+' | '-' | '_' | '*'))
    {
        return None;
    }
    Some((
        start + leading + name_end + operator + 1 + whitespace,
        PackageQuery::Versions(package.into(), prefix.into()),
    ))
}
fn pypi_names() -> Result<Arc<Vec<String>>, String> {
    let mut cache = PACKAGE_NAMES
        .get_or_init(|| std::sync::Mutex::new(PackageIndex::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache
        .loaded
        .is_some_and(|time| time.elapsed() < std::time::Duration::from_secs(3600))
    {
        return Ok(cache.names.clone());
    }
    #[derive(serde::Deserialize)]
    struct Project {
        name: String,
    }
    fn names<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
        struct Names;
        impl<'de> serde::de::Visitor<'de> for Names {
            type Value = Vec<String>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded list of PyPI project names")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut names = Vec::new();
                let mut count = 0usize;
                while let Some(project) = sequence.next_element::<Project>()? {
                    count += 1;
                    if count > 1_500_000 {
                        return Err(serde::de::Error::custom(
                            "PyPI completion catalogue exceeds its entry limit",
                        ));
                    }
                    if project.name.len() <= 256 {
                        names.push(project.name);
                    }
                }
                Ok(names)
            }
        }
        deserializer.deserialize_seq(Names)
    }
    #[derive(serde::Deserialize)]
    struct Index {
        #[serde(deserialize_with = "names")]
        projects: Vec<String>,
    }
    let url = "https://pypi.org/simple/";
    let response = rustpy_net::agent_builder(url)
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(20))
        .build()
        .get(url)
        .set("Accept", "application/vnd.pypi.simple.v1+json")
        .call()
        .map_err(|error| error.to_string())?;
    // Only the public Simple API names are read; arbitrary metadata values
    // are skipped by serde. The byte cap also bounds a broken proxy response.
    let parsed: Index = serde_json::from_reader(response.into_reader().take(128 * 1024 * 1024))
        .map_err(|error| error.to_string())?;
    let mut names = parsed.projects;
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup();
    cache.names = Arc::new(names);
    cache.loaded = Some(std::time::Instant::now());
    Ok(cache.names.clone())
}
fn package_items(
    query: PackageQuery,
    installed: &[(String, String)],
) -> Result<Vec<CompletionItem>, String> {
    match &query {
        PackageQuery::Names(_) => {
            let names = pypi_names();
            merge_package_items(query, installed, names.as_deref().map(Vec::as_slice))
        }
        PackageQuery::Versions(package, _) => {
            let versions =
                rustpy_packages::pypi_versions(package).map_err(|error| error.to_string());
            merge_package_items(query, installed, versions.as_deref())
        }
    }
}
/// Installed distributions first, then PyPI. An unreachable PyPI is only an
/// error when the interpreter offers nothing for the query either.
fn merge_package_items(
    query: PackageQuery,
    installed: &[(String, String)],
    remote: Result<&[String], &String>,
) -> Result<Vec<CompletionItem>, String> {
    let mut items = Vec::new();
    match &query {
        PackageQuery::Names(prefix) => {
            let prefix = rustpy_packages::normalize_name(prefix);
            for (name, version) in installed {
                if rustpy_packages::normalize_name(name).starts_with(&prefix) {
                    items.push(lookup_item(name, &format!("Installed package {version}")));
                }
            }
        }
        PackageQuery::Versions(package, prefix) => {
            let package = rustpy_packages::normalize_name(package);
            for (name, version) in installed {
                if rustpy_packages::normalize_name(name) == package && version.starts_with(prefix) {
                    items.push(lookup_item(version, "Installed version"));
                }
            }
        }
    }
    let remote = match remote {
        Ok(remote) => remote,
        Err(error) if items.is_empty() => return Err(error.clone()),
        Err(_) => &[][..],
    };
    let known = items
        .iter()
        .map(|item| rustpy_packages::normalize_name(&item.label))
        .collect::<std::collections::HashSet<_>>();
    match query {
        PackageQuery::Names(prefix) => {
            let prefix = rustpy_packages::normalize_name(&prefix);
            items.extend(
                remote
                    .iter()
                    .filter(|name| {
                        let normalized = rustpy_packages::normalize_name(name);
                        normalized.starts_with(&prefix) && !known.contains(&normalized)
                    })
                    .take(200)
                    .map(|name| lookup_item(name, "PyPI package")),
            );
        }
        PackageQuery::Versions(package, prefix) => items.extend(
            remote
                .iter()
                .filter(|version| {
                    version.starts_with(&prefix)
                        && !known.contains(&rustpy_packages::normalize_name(version))
                })
                .take(200)
                .map(|version| lookup_item(version, &format!("{package} release on PyPI"))),
        ),
    }
    Ok(items)
}
fn pyproject_items(text: &str, caret: usize) -> Option<(usize, Vec<CompletionItem>)> {
    let before = text.get(..caret)?;
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    let line = &before[line_start..];
    if line.trim_start().starts_with('#') || line.contains('=') {
        return None;
    }
    let section = before[..line_start]
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('['))
        .map(str::trim)
        .unwrap_or("");
    let prefix = line.trim_start();
    let start = line_start + line.len() - prefix.len();
    if prefix.starts_with('[') {
        let names = [
            "[build-system]",
            "[project]",
            "[project.urls]",
            "[project.scripts]",
            "[project.gui-scripts]",
            "[project.entry-points]",
            "[project.optional-dependencies]",
            "[tool.pytest.ini_options]",
            "[tool.ruff]",
            "[tool.black]",
            "[tool.mypy]",
        ];
        return Some((
            start,
            names
                .into_iter()
                .filter(|name| name.starts_with(prefix))
                .map(|name| lookup_item(name, "pyproject.toml table"))
                .collect(),
        ));
    }
    let fields: &[(&str, &str)] = match section {
        "[build-system]" => &[
            ("requires", "[]"),
            ("build-backend", "\"\""),
            ("backend-path", "[]"),
        ],
        "[project]" => &[
            ("name", "\"\""),
            ("version", "\"\""),
            ("description", "\"\""),
            ("readme", "\"README.md\""),
            ("requires-python", "\">=3.10\""),
            ("license", "\"\""),
            ("license-files", "[]"),
            ("authors", "[]"),
            ("maintainers", "[]"),
            ("keywords", "[]"),
            ("classifiers", "[]"),
            ("dependencies", "[]"),
            ("optional-dependencies", "{}"),
            ("urls", "{}"),
            ("scripts", "{}"),
            ("gui-scripts", "{}"),
            ("entry-points", "{}"),
            ("dynamic", "[]"),
        ],
        "[tool.pytest.ini_options]" => &[
            ("testpaths", "[]"),
            ("python_files", "[]"),
            ("python_classes", "[]"),
            ("python_functions", "[]"),
            ("addopts", "\"\""),
            ("markers", "[]"),
            ("filterwarnings", "[]"),
            ("asyncio_mode", "\"auto\""),
        ],
        "[tool.ruff]" => &[
            ("line-length", "88"),
            ("target-version", "\"py310\""),
            ("src", "[]"),
            ("exclude", "[]"),
            ("extend-exclude", "[]"),
            ("lint", "{}"),
            ("format", "{}"),
        ],
        "[tool.black]" => &[
            ("line-length", "88"),
            ("target-version", "[]"),
            ("include", "\"\\\\.pyi?$\""),
            ("exclude", "\"\""),
        ],
        "[tool.mypy]" => &[
            ("python_version", "\"3.10\""),
            ("strict", "true"),
            ("files", "[]"),
            ("exclude", "[]"),
            ("ignore_missing_imports", "false"),
            ("warn_unused_ignores", "true"),
        ],
        "" => &[("build-system", "{}"), ("project", "{}"), ("tool", "{}")],
        _ => &[],
    };
    Some((
        start,
        fields
            .iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(name, value)| {
                let mut item = lookup_item(name, section);
                item.insert = format!("{name} = {value}");
                item.caret_offset = Some(name.len() + 4);
                item
            })
            .collect(),
    ))
}
/// True inside `{{ … }}` or after a statement's tag name, where a macro call fits.
fn template_expression_position(text: &str, start: usize) -> bool {
    let before = &text[..start];
    match (before.rfind("{{"), before.rfind("{%")) {
        (Some(a), b) if b.is_none_or(|b| a > b) => true,
        (_, Some(b)) => !before[b + 2..]
            .trim_start_matches(['-', '+'])
            .trim()
            .is_empty(),
        _ => false,
    }
}
fn template_items(text: &str, caret: usize) -> Option<(usize, Vec<CompletionItem>)> {
    let before = text.get(..caret)?;
    let (open, statement) = match (before.rfind("{{"), before.rfind("{%")) {
        (Some(a), Some(b)) => {
            if a > b {
                (a, false)
            } else {
                (b, true)
            }
        }
        (Some(a), None) => (a, false),
        (None, Some(b)) => (b, true),
        _ => return None,
    };
    let context = &before[open + 2..];
    if context.contains("}}") || context.contains("%}") {
        return None;
    }
    let start = before
        .rfind(|ch: char| !rustpy_core::is_ident_continue(ch))
        .map_or(open + 2, |at| {
            at + before[at..].chars().next().map_or(0, char::len_utf8)
        });
    let prefix = &before[start..];
    if rustpy_semantic::template_jinja::test_position(&before[open + 2..start]) {
        let items = rustpy_semantic::template_jinja::TESTS
            .iter()
            .filter(|name| name.starts_with(prefix))
            .map(|name| lookup_item(name, "Jinja test"))
            .collect();
        return Some((start, items));
    }
    let names: &[&str] = if statement {
        &[
            "autoescape",
            "block",
            "endblock",
            "break",
            "continue",
            "cache",
            "endcache",
            "call",
            "endcall",
            "comment",
            "endcomment",
            "csrf_token",
            "debug",
            "do",
            "elif",
            "else",
            "endif",
            "empty",
            "extends",
            "filter",
            "endfilter",
            "firstof",
            "for",
            "endfor",
            "from",
            "if",
            "ifchanged",
            "endifchanged",
            "import",
            "include",
            "load",
            "macro",
            "endmacro",
            "now",
            "regroup",
            "set",
            "endset",
            "spaceless",
            "endspaceless",
            "trans",
            "endtrans",
            "url",
            "verbatim",
            "endverbatim",
            "with",
            "endwith",
        ]
    } else if context.contains('|') {
        &[
            "abs",
            "add",
            "addslashes",
            "attr",
            "batch",
            "capitalize",
            "center",
            "count",
            "cut",
            "date",
            "default",
            "default_if_none",
            "dictsort",
            "divisibleby",
            "escape",
            "filesizeformat",
            "first",
            "float",
            "forceescape",
            "format",
            "groupby",
            "indent",
            "int",
            "items",
            "join",
            "json_script",
            "last",
            "length",
            "linebreaks",
            "list",
            "lower",
            "map",
            "max",
            "min",
            "pprint",
            "random",
            "reject",
            "rejectattr",
            "replace",
            "reverse",
            "round",
            "safe",
            "select",
            "selectattr",
            "slice",
            "slugify",
            "sort",
            "string",
            "striptags",
            "sum",
            "title",
            "tojson",
            "truncate",
            "unique",
            "upper",
            "urlencode",
            "urlize",
            "wordcount",
            "wordwrap",
            "yesno",
        ]
    } else {
        &[
            "True",
            "False",
            "None",
            "true",
            "false",
            "none",
            "loop",
            "super",
            "self",
            "range",
            "dict",
            "cycler",
            "joiner",
            "namespace",
            "request",
            "session",
            "g",
            "config",
            "url_for",
            "get_flashed_messages",
            "user",
            "perms",
            "messages",
            "csrf_token",
            "STATIC_URL",
            "MEDIA_URL",
        ]
    };
    let mut items: Vec<_> = names
        .iter()
        .filter(|name| name.starts_with(prefix))
        .map(|name| {
            lookup_item(
                name,
                if statement {
                    "template tag"
                } else {
                    "template value/filter"
                },
            )
        })
        .collect();
    // The local loop/assignment context is source-derived, not a hard-coded
    // application model. Split only complete identifier tokens.
    let mut scopes: Vec<Vec<String>> = vec![Vec::new()];
    for segment in before.split("{%").skip(1).take(256) {
        let Some(end) = segment.find("%}") else {
            continue;
        };
        let words: Vec<_> = segment[..end].split_whitespace().collect();
        let tag = words.first().copied();
        if matches!(tag, Some("endfor" | "endwith")) {
            if scopes.len() > 1 {
                scopes.pop();
            }
        } else if matches!(tag, Some("for" | "set" | "with")) {
            if matches!(tag, Some("for" | "with")) {
                scopes.push(Vec::new());
            }
            for word in words
                .iter()
                .skip(1)
                .take_while(|word| !matches!(**word, "in" | "="))
            {
                let word = word.trim_end_matches(',');
                if !word.is_empty() && word.chars().all(rustpy_core::is_ident_continue) {
                    scopes.last_mut().unwrap().push(word.into());
                }
            }
        }
    }
    for word in scopes.into_iter().flatten() {
        if word.starts_with(prefix) && !items.iter().any(|item| item.label == word) {
            items.push(lookup_item(&word, "template local"));
        }
    }
    Some((start, items))
}
impl IdeApp {
    pub(super) fn cancel_basic_completion(&mut self) {
        self.basic_completion_ui.packages = None;
        self.completion_ui.due = None;
    }
    pub(super) fn open_basic_completion(&mut self, explicit: bool) -> bool {
        if self.open_database_completion(explicit) {
            return true;
        }
        if self.open_document_completion(explicit) {
            return true;
        }
        let Some(index) = self.active else {
            return false;
        };
        if self.tabs[index].state.language != Language::Python {
            return false;
        }
        // Typing in a large file: the providers run off the UI thread.
        if !explicit && self.completes_in_background(index) {
            let state = &self.tabs[index].state;
            let caret = state.primary().head();
            let prefix_start = identifier_start(&state.doc, caret);
            let Some(lookup) = self.large_file_lookup(index, prefix_start) else {
                self.show_loading_lookup(TextSize::new(prefix_start as u32), false);
                return true;
            };
            return match &lookup.found {
                super::completion_worker::Found::TemplateArgument(start) => {
                    self.python_template_names_at(*start, caret, false)
                }
                super::completion_worker::Found::Basic(context) => {
                    // The identifier may have grown since the request.
                    let typed = self.tabs[index]
                        .state
                        .doc
                        .slice(TextRange::new(context.prefix_start.min(caret), caret));
                    let typed = typed.to_lowercase();
                    let (items, imports) = context
                        .items
                        .iter()
                        .cloned()
                        .zip(context.imports.iter().cloned())
                        .filter(|(item, _)| features::matches_prefix(&item.label, &typed))
                        .unzip();
                    let context = rustpy_semantic::basic_completion::BasicCompletion {
                        items,
                        imports,
                        prefix_start: context.prefix_start,
                        replace_segment: context.replace_segment,
                        import_classes: context.import_classes,
                    };
                    self.show_basic_completion(index, caret, false, context);
                    true
                }
                super::completion_worker::Found::Items(_) => false,
            };
        }
        let Some((analysis, file)) = self.analysis_file(index) else {
            return false;
        };
        let caret = self.tabs[index].state.primary().head();
        if self.python_template_name_completion(&file, caret, explicit) {
            return true;
        }
        let Some(context) = rustpy_semantic::basic_completion::complete(&analysis, &file, caret)
        else {
            return false;
        };
        let import_classes = context.import_classes;
        let prefix_start = context.prefix_start;
        self.show_basic_completion(index, caret, explicit, context);
        // Annotated class names outside the current module use the same
        // captured, cancellable project/library import lane as second press.
        if explicit && import_classes && caret > prefix_start {
            self.start_import_completion(analysis, file, caret, true, true);
        }
        true
    }

    /// Shows the lookup of an exclusive context provider.
    fn show_basic_completion(
        &mut self,
        index: usize,
        caret: TextSize,
        explicit: bool,
        context: rustpy_semantic::basic_completion::BasicCompletion,
    ) {
        self.basic_completion_ui.packages = None;
        self.completion.visible = false;
        self.import_completion = None;
        self.completion_ui.due = None;
        self.completion_ui.paths = context.replace_segment;
        self.completion = CompletionPopup {
            visible: !context.items.is_empty(),
            items: context.items,
            imports: context.imports,
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: context.prefix_start,
            doc_version: self.tabs[index].state.doc.version(),
            caret,
            explicit,
            rect: None,
        };
        self.refresh_completion_order();
    }
    fn open_document_completion(&mut self, explicit: bool) -> bool {
        let Some(index) = self.active else {
            return false;
        };
        let tab = &self.tabs[index];
        let text = tab.state.doc.text();
        let caret = tab.state.primary().head();
        let end = usize::from(caret);
        let package = tab.state.language == Language::Requirements;
        let pyproject = tab.state.language == Language::Toml
            && tab
                .state
                .path
                .as_ref()
                .and_then(|path| path.file_name())
                .is_some_and(|name| name == "pyproject.toml");
        let template = matches!(tab.state.language, Language::Template | Language::Html);
        if !package && !pyproject && !template {
            return false;
        }
        self.basic_completion_ui.packages = None;
        self.completion.visible = false;
        self.import_completion = None;
        self.completion_ui.due = None;
        self.completion_ui.paths = true;
        self.completion_ui.reset_help();
        if package {
            let Some((start, query)) = package_query(&text, end) else {
                return false;
            };
            if !explicit && matches!(&query,PackageQuery::Names(prefix) if prefix.len()<2) {
                return true;
            }
            let editor = tab.view.id;
            let version = tab.state.doc.version();
            let installed = self
                .framework_catalog_snapshot()
                .installed
                .unwrap_or_default();
            let (tx, rx) = crossbeam_channel::bounded(1);
            let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let token = cancelled.clone();
            self.basic_completion_ui
                .worker
                .get_or_insert_with(PackageWorker::new)
                .submit(QueuedPackageQuery {
                    query,
                    installed,
                    reply: tx,
                    cancelled: token,
                });
            self.basic_completion_ui.packages = Some(PackageCompletionJob {
                editor,
                version,
                caret,
                start: TextSize::new(start as u32),
                explicit,
                receiver: rx,
                cancelled,
            });
            self.completion.visible = false;
            self.set_status("Looking up PyPI suggestions…");
            return true;
        }
        // Inside a `{% url %}`/`{% load %}`/template-name/`url_for` argument
        // only catalogue names apply, not the tag/value vocabulary.
        let slot = if template {
            self.template_slot_items(&text, end)
        } else {
            None
        };
        let in_slot = slot.is_some();
        // Jinja2 macro imports, `alias.` macros and render-context members.
        let jinja = if template && !in_slot {
            self.jinja_template_items(index, &text, end, explicit)
        } else {
            None
        };
        let member = jinja.is_some();
        let found = if pyproject {
            pyproject_items(&text, end)
        } else if slot.is_some() {
            slot
        } else if jinja.is_some() {
            jinja
        } else {
            template_items(&text, end)
        };
        let Some((start, mut items)) = found else {
            return false;
        };
        if template && !in_slot && !member {
            self.merge_catalog_template_items(&text, end, &mut items);
            // Render-context names come from inferring the project's views:
            // computed off the UI thread, they join the lookup when ready.
            if let Some(path) = self.tabs[index].state.path.clone() {
                let prefix = &text[start..end];
                for item in self
                    .template_context_candidates(
                        &path,
                        start,
                        explicit,
                        super::completion_worker::TemplateQuery::Names,
                    )
                    .unwrap_or_default()
                {
                    if item.label.starts_with(prefix)
                        && !items.iter().any(|known| known.label == item.label)
                    {
                        items.push(item);
                    }
                }
            }
            if template_expression_position(&text, start) {
                let prefix = &text[start..end];
                let mut load = |name: &str| {
                    self.template_path(name)
                        .and_then(|path| self.template_text(&path))
                };
                for item in rustpy_semantic::template_jinja::scope_items(&text, &mut load) {
                    if item.label.starts_with(prefix)
                        && !items.iter().any(|known| known.label == item.label)
                    {
                        let mut entry = lookup_item(&item.label, &item.detail);
                        entry.kind = CompletionKind::Function;
                        items.push(entry);
                    }
                }
            }
        }
        // An empty lookup still waiting for view-derived candidates shows
        // its loading row.
        let loading = template && self.completion_worker.template_pending();
        self.completion = CompletionPopup {
            visible: !items.is_empty() || loading,
            imports: vec![None; items.len()],
            items,
            selected: if self.settings.completion.preselect_first {
                0
            } else {
                usize::MAX
            },
            prefix_start: TextSize::new(start as u32),
            doc_version: self.tabs[index].state.doc.version(),
            caret,
            explicit,
            rect: None,
        };
        self.refresh_completion_order();
        true
    }
    /// Python sources (with unsaved buffers) for static render-context inference.
    pub(super) fn template_context_analysis(&self) -> Option<(Analysis, Vec<PathBuf>)> {
        let project = self.project.as_ref()?;
        let scope = self.project_scope()?;
        let analysis = &project.analysis;
        let resolver = rustpy_semantic::module::ModuleResolver::new(
            analysis.resolver.roots.clone(),
            analysis.resolver.search_paths.clone(),
            analysis.resolver.python_version,
        )
        .with_stub_roots(analysis.resolver.stub_roots.iter().cloned())
        .with_excluded(analysis.resolver.excluded_paths().iter().cloned());
        let query = Analysis::new(resolver);
        let mut files = Vec::new();
        let mut pending = scope.walk_roots();
        let mut entries = 0usize;
        while let Some(directory) = pending.pop() {
            if entries >= 4096 || files.len() >= 256 {
                break;
            }
            let Ok(children) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in children.take(4096 - entries).flatten() {
                entries += 1;
                let path = entry.path();
                if !scope.contains(&path) {
                    continue;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    pending.push(path)
                } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "py") {
                    files.push(path);
                    if files.len() == 256 {
                        break;
                    }
                }
            }
        }
        let mut bytes = 0usize;
        for buffer in self
            .tabs
            .iter()
            .filter(|buffer| buffer.state.language == Language::Python)
        {
            if let Some(path) = buffer
                .state
                .path
                .as_ref()
                .filter(|path| scope.contains(path))
            {
                let source = buffer.state.doc.text();
                bytes += source.len();
                if bytes > 8 * 1024 * 1024 {
                    break;
                }
                query.set_file_text(path, &source);
                if !files.contains(path) && files.len() < 256 {
                    files.push(path.clone());
                }
            }
        }
        Some((query, files))
    }
    /// Jinja2 macro imports, `alias.` macros and render-context members.
    fn jinja_template_items(
        &mut self,
        index: usize,
        text: &str,
        end: usize,
        explicit: bool,
    ) -> Option<(usize, Vec<CompletionItem>)> {
        let mut load = |name: &str| {
            self.template_path(name)
                .and_then(|path| self.template_text(&path))
        };
        let macro_entry = |item: rustpy_semantic::template_jinja::Item| {
            let mut entry = lookup_item(&item.label, &item.detail);
            entry.kind = CompletionKind::Function;
            entry
        };
        if let Some((start, found)) =
            rustpy_semantic::template_jinja::from_import_items(text, end, &mut load)
        {
            let prefix = &text[start..end];
            let items = found
                .into_iter()
                .filter(|item| item.label.starts_with(prefix))
                .map(macro_entry)
                .collect();
            return Some((start, items));
        }
        let (start, root, steps) = rustpy_semantic::template_context::member_path(text, end)?;
        let prefix = &text[start..end];
        if steps.is_empty() {
            if let Some(found) =
                rustpy_semantic::template_jinja::alias_items(text, &root, &mut load)
            {
                let items = found
                    .into_iter()
                    .filter(|item| item.label.starts_with(prefix))
                    .map(macro_entry)
                    .collect();
                return Some((start, items));
            }
        }
        let path = self.tabs[index].state.path.clone()?;
        self.project_scope()?;
        // Members come from inferring the project's views: computed off the
        // UI thread (the lookup shows its loading row meanwhile).
        let items = self
            .template_context_candidates(
                &path,
                start,
                explicit,
                super::completion_worker::TemplateQuery::Members(root, steps),
            )
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.label.starts_with(prefix))
            .collect();
        Some((start, items))
    }
    pub(super) fn poll_basic_completion(&mut self, ctx: &egui::Context) {
        let Some(job) = self.basic_completion_ui.packages.as_ref() else {
            return;
        };
        if !self
            .active
            .and_then(|index| self.tabs.get(index))
            .is_some_and(|tab| {
                tab.view.id == job.editor
                    && tab.state.doc.version() == job.version
                    && tab.state.primary().head() == job.caret
            })
        {
            self.basic_completion_ui.packages = None;
            return;
        }
        let result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(crossbeam_channel::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            Err(_) => {
                self.basic_completion_ui.packages = None;
                return;
            }
        };
        let job = self.basic_completion_ui.packages.take().unwrap();
        match result {
            Ok(items) => {
                self.completion = CompletionPopup {
                    visible: !items.is_empty(),
                    imports: vec![None; items.len()],
                    items,
                    selected: if self.settings.completion.preselect_first {
                        0
                    } else {
                        usize::MAX
                    },
                    prefix_start: job.start,
                    doc_version: job.version,
                    caret: job.caret,
                    explicit: job.explicit,
                    rect: None,
                };
                self.completion_ui.paths = true;
                self.refresh_completion_order();
            }
            Err(error) => self.set_status(format!("Package completion: {error}")),
        }
    }
    pub(super) fn document_completion_frame(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        typed: &str,
        caret_rect: Option<Rect>,
        focused: bool,
        now: f64,
    ) {
        let tab = &self.tabs[index];
        let version = tab.state.doc.version();
        let caret = tab.state.primary().head();
        let editor = tab.view.id;
        if !focused {
            self.completion_ui.due = None;
            if !self.completion.rect.is_some_and(|rect| {
                ui.input(|input| {
                    input
                        .pointer
                        .hover_pos()
                        .is_some_and(|position| rect.contains(position))
                })
            }) {
                self.completion.visible = false;
            }
            return;
        }
        if !typed.is_empty() && self.background_features().completion_popup {
            if self.completion.visible {
                self.open_completion(false);
            } else {
                self.completion_ui.due = Some((
                    editor,
                    version,
                    caret,
                    now + self.settings.completion.auto_popup_delay_ms.min(10000) as f64 / 1000.0,
                ));
            }
        } else if self.completion.visible && self.completion.doc_version != version {
            self.open_completion(self.completion.explicit);
        }
        if let Some(wait) = self
            .completion_ui
            .popup_wait(editor, version, caret, focused, now)
        {
            if wait.is_zero() {
                self.open_completion(false);
            } else {
                ui.ctx().request_repaint_after(wait);
            }
        }
        if self.completion.visible
            && self.completion.doc_version == version
            && self.completion.caret != caret
        {
            self.completion.visible = false;
        }
        if self.completion.visible {
            if let Some(rect) = caret_rect {
                self.draw_completion(ui, rect);
            }
        }
    }
}

/// Start of the identifier the caret at `caret` of `doc` ends.
pub(super) fn identifier_start(doc: &rustpy_text::Document, caret: TextSize) -> usize {
    let line = doc.line_range(doc.line_of(caret));
    let before = doc.slice(TextRange::new(line.start(), caret.max(line.start())));
    let column = before
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map(|at| at + before[at..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0);
    usize::from(line.start()) + column
}

#[cfg(test)]
mod basic_document_tests {
    use super::*;
    #[test]
    fn requirements_completion_preserves_name_extras_and_version_segments() {
        let text = "  requests[security]>=2.3";
        assert!(
            matches!(package_query(text, text.len()), Some((start, PackageQuery::Versions(name, prefix))) if &text[start..]=="2.3" && name=="requests" && prefix=="2.3")
        );
        assert!(
            matches!(package_query("django-res", 10), Some((0, PackageQuery::Names(prefix))) if prefix=="django-res")
        );
        assert!(package_query("# requests", 10).is_none());
        assert!(package_query("thing @ https://example.org/pkg", 30).is_none());
    }
    #[test]
    fn requirement_names_and_versions_fall_back_to_installed_distributions() {
        let installed = vec![
            ("requests".to_string(), "2.31.0".to_string()),
            ("Django".to_string(), "5.0.1".to_string()),
        ];
        let offline = "PyPI unreachable".to_string();
        let labels = |items: Vec<CompletionItem>| {
            items
                .into_iter()
                .map(|item| item.label.to_string())
                .collect::<Vec<_>>()
        };
        let names = merge_package_items(
            PackageQuery::Names("requ".into()),
            &installed,
            Err(&offline),
        )
        .expect("installed names without PyPI");
        assert_eq!(labels(names), ["requests"]);
        let versions = merge_package_items(
            PackageQuery::Versions("django".into(), "5".into()),
            &installed,
            Err(&offline),
        )
        .unwrap();
        assert_eq!(labels(versions), ["5.0.1"]);
        assert!(
            merge_package_items(PackageQuery::Names("zzz".into()), &installed, Err(&offline))
                .is_err()
        );
        let remote = ["requests".to_string(), "requests-oauthlib".to_string()];
        let merged = merge_package_items(
            PackageQuery::Names("requ".into()),
            &installed,
            Ok(&remote[..]),
        )
        .unwrap();
        assert_eq!(labels(merged), ["requests", "requests-oauthlib"]);
    }
    #[test]
    fn document_schema_and_template_providers_are_context_specific() {
        let text = "[project]\nrequ";
        let (start, items) = pyproject_items(text, text.len()).expect("project fields");
        assert_eq!(&text[start..], "requ");
        assert!(items
            .iter()
            .any(|item| item.label.starts_with("requires-python")));
        let text = "{% for customer in customers %}{{ cust";
        let (_, items) = template_items(text, text.len()).expect("template expression");
        assert!(items.iter().any(|item| item.label == "customer"));
        assert!(template_items("ordinary prose", 14).is_none());
    }
    #[test]
    fn jinja_tests_follow_is_and_macros_fit_expression_positions() {
        let text = "{% if loop.index is not div";
        let (start, items) = template_items(text, text.len()).expect("test names");
        assert_eq!(&text[start..], "div");
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["divisibleby"]
        );
        let text = "{{ value is ";
        let (_, items) = template_items(text, text.len()).expect("test names");
        assert!(items.iter().any(|item| item.label == "defined"));
        assert!(items.iter().all(|item| item.detail == "Jinja test"));
        let text = "{{ this";
        let (_, items) = template_items(text, text.len()).unwrap();
        assert!(!items.iter().any(|item| item.detail == "Jinja test"));
        assert!(template_expression_position("{{ fo", 3));
        assert!(template_expression_position("{% call fo", 8));
        assert!(!template_expression_position("{%- ca", 4));
        assert!(!template_expression_position("plain", 2));
    }
}
