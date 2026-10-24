//! Plugins in the running IDE (roadmap §5 "Plugins"): the plugins installed
//! under `<config>/plugins` (crate `rustpy-plugins`) and what the enabled ones
//! contribute, merged into the IDE's own lists:
//!
//! - live templates: builtin < plugin < user (by abbreviation, which the
//!   editor uses as the key),
//! - color schemes: builtin, then the user's `<config>/colors`, then plugin
//!   schemes (a user scheme with the same name wins),
//! - keymaps: the bundled presets, then plugin keymaps (layered over Default),
//! - external tools: the user's tools, then plugin tools (grouped in
//!   submenus). Plugin tools run like the user's (`crate::external_tools`):
//!   the program directly, without a shell, with the macros filled in inside
//!   each argument; `$PluginDir$` was filled in when the plugin loaded,
//! - file templates: offered in File › New from Plugin Template and the
//!   project view's New menu,
//! - TextMate grammars ([`textmate_grammars`]): loaded with the TextMate
//!   bundles (the user's bundles win), for files without a built-in
//!   highlighter whose extension the plugin declares.
//!
//! [`tool_macro_values`] fills in `$FilePath$` and the other macros of run
//! configurations (parameters and working directory).
//!
//! The registry is loaded once per process, on first use, and replaced after
//! every change made in Settings › Plugins, so changes apply without a
//! restart. Readers get cheap `Arc` snapshots; the merge functions are pure
//! and take their inputs explicitly so they can be tested.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use rustpy_highlight::ColorScheme;
use rustpy_plugins::{
    Contributions, FileTemplateContribution, GrammarContribution, KeymapContribution,
    PluginRegistry,
};
use rustpy_settings::{ExternalTool, Settings};
use rustpy_templates::LiveTemplate;

use crate::actions::Keymap;

/// The installed plugins of one configuration directory and their contributions.
struct Host {
    config_dir: PathBuf,
    registry: Arc<PluginRegistry>,
    contributions: Arc<Contributions>,
    generation: u64,
    runtime_styles: BTreeMap<String, Contributions>,
}

impl Host {
    fn load(config_dir: PathBuf, generation: u64) -> Host {
        let registry = PluginRegistry::load(&config_dir);
        for e in registry.errors() {
            log::warn!("plugins: {e}");
        }
        for p in registry.plugins() {
            for e in &p.errors {
                log::warn!("plugin {}: {e}", p.id);
            }
        }
        let contributions = Arc::new(registry.contributions());
        log::info!(
            "plugins: {} installed, {} active ({})",
            registry.plugins().len(),
            registry.plugins().iter().filter(|p| p.is_active()).count(),
            config_dir.join(rustpy_plugins::PLUGINS_DIR).display()
        );
        Host {
            config_dir,
            registry: Arc::new(registry),
            contributions,
            generation,
            runtime_styles: BTreeMap::new(),
        }
    }
}

static HOST: RwLock<Option<Host>> = RwLock::new(None);

/// Runs `f` on the host of the current configuration directory, loading it first when needed.
fn with_host<R>(f: impl FnOnce(&Host) -> R) -> R {
    let config_dir = Settings::config_dir();
    {
        let guard = HOST.read().unwrap_or_else(|e| e.into_inner());
        if let Some(host) = guard.as_ref().filter(|h| h.config_dir == config_dir) {
            return f(host);
        }
    }
    let mut guard = HOST.write().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_none_or(|h| h.config_dir != config_dir) {
        let generation = guard.as_ref().map_or(0, |h| h.generation + 1);
        *guard = Some(Host::load(config_dir, generation));
    }
    f(guard.as_ref().expect("loaded above"))
}

/// What the enabled, valid plugins contribute.
pub fn contributions() -> Arc<Contributions> {
    with_host(|h| h.contributions.clone())
}

/// The installed plugins (a snapshot; see [`update`] to change them).
pub fn registry() -> Arc<PluginRegistry> {
    with_host(|h| h.registry.clone())
}

/// Increases whenever the plugins change (for caches built from contributions).
pub fn generation() -> u64 {
    with_host(|h| h.generation)
}

/// The configuration directory the plugins live in.
pub fn config_dir() -> PathBuf {
    with_host(|h| h.config_dir.clone())
}

/// Changes the installed plugins (enable, disable, install, uninstall) and
/// publishes the result, so every reader sees the new contributions.
pub fn update<R>(f: impl FnOnce(&mut PluginRegistry) -> R) -> R {
    with_host(|_| ());
    let mut guard = HOST.write().unwrap_or_else(|e| e.into_inner());
    let host = guard.as_mut().expect("loaded by with_host");
    let registry = Arc::make_mut(&mut host.registry);
    let result = f(registry);
    host.runtime_styles.clear();
    host.contributions = Arc::new(registry.contributions());
    host.generation += 1;
    result
}

/// Re-reads the plugins from disk (after an install by a background job, or
/// whenever the settings are re-read; settings bundles leave plugins alone).
pub fn reload() {
    update(PluginRegistry::reload);
}

/// Publish styles already validated by the capability host. No executable code
/// runs under this global lock; disabling/updating plugins clears their styles.
pub fn set_runtime_styles(
    id: &str,
    response: &rustpy_plugins::runtime::Response,
) -> Result<(), String> {
    let contributions = rustpy_plugins::runtime_styles(id, response)?;
    with_host(|_| ());
    let mut guard = HOST.write().map_err(|_| "Plugin host is unavailable")?;
    let host = guard.as_mut().ok_or("Plugin host is unavailable")?;
    if !host
        .registry
        .plugins()
        .iter()
        .any(|plugin| plugin.id == id && plugin.is_active())
    {
        return Err("Plugin is disabled or no longer valid".into());
    }
    host.runtime_styles.insert(id.into(), contributions);
    let mut merged = host.registry.contributions();
    for contributions in host.runtime_styles.values() {
        merged.append(contributions);
    }
    host.contributions = Arc::new(merged);
    host.generation += 1;
    Ok(())
}
pub fn clear_runtime_styles() {
    with_host(|_| ());
    if let Ok(mut guard) = HOST.write() {
        if let Some(host) = guard.as_mut() {
            host.runtime_styles.clear();
            host.contributions = Arc::new(host.registry.contributions());
            host.generation += 1;
        }
    }
}

// ----- live templates ----------------------------------------------------------

/// The live templates the editor uses: builtin ones, replaced by plugin
/// templates with the same abbreviation (the first plugin wins among plugins),
/// replaced in turn by the user's own.
pub fn merge_live_templates(
    builtin: Vec<LiveTemplate>,
    plugins: &Contributions,
    user: Vec<LiveTemplate>,
) -> Vec<LiveTemplate> {
    let mut all = builtin;
    let mut from_plugins: HashSet<String> = HashSet::new();
    for t in plugins.all_live_templates() {
        if !from_plugins.insert(t.abbreviation.clone()) {
            continue;
        }
        all.retain(|b| b.abbreviation != t.abbreviation);
        all.push(t);
    }
    for t in user {
        all.retain(|b| b.abbreviation != t.abbreviation);
        all.push(t);
    }
    all
}

/// The id of the plugin `template` comes from (same abbreviation and text).
pub fn template_plugin(template: &LiveTemplate, plugins: &Contributions) -> Option<String> {
    plugins
        .live_templates
        .iter()
        .find(|file| {
            file.templates
                .iter()
                .any(|t| t.abbreviation == template.abbreviation && t.template == template.template)
        })
        .map(|file| file.plugin_id.clone())
}

/// The Group column of Settings › Live Templates: plugin templates name their plugin.
pub fn template_group_label(template: &LiveTemplate) -> String {
    match template_plugin(template, &contributions()) {
        Some(id) => {
            let name = plugin_name(&id);
            if template.group == name {
                format!("{name} (plugin)")
            } else {
                format!("{} (plugin {name})", template.group)
            }
        }
        None => template.group.clone(),
    }
}

/// Display name of an installed plugin (its id when unknown).
pub fn plugin_name(id: &str) -> String {
    registry()
        .get(id)
        .map(|p| p.name().to_string())
        .unwrap_or_else(|| id.to_string())
}

// ----- color schemes -----------------------------------------------------------

/// A color scheme contributed by an enabled plugin.
pub fn color_scheme(name: &str) -> Option<ColorScheme> {
    contributions().color_scheme(name).cloned()
}

/// Scheme names for the pickers after the builtin ones: the user's schemes,
/// then plugin schemes whose name is not taken (the user's file wins).
pub fn merge_scheme_names(user: Vec<String>, plugins: &Contributions) -> Vec<String> {
    let mut out = user;
    for scheme in &plugins.color_schemes {
        let name = &scheme.scheme.name;
        if !out.contains(name) && !ColorScheme::builtin_names().contains(&name.as_str()) {
            out.push(name.clone());
        }
    }
    out
}

// ----- keymaps -----------------------------------------------------------------

/// Keymap names for Settings › Keymap: the bundled presets, then plugin
/// keymaps (one named like a bundled preset is shadowed by it).
pub fn keymap_names_with(plugins: &Contributions) -> Vec<String> {
    let mut out: Vec<String> = Keymap::preset_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    for k in &plugins.keymaps {
        if !out.contains(&k.name) {
            out.push(k.name.clone());
        }
    }
    out
}

/// [`keymap_names_with`] the current plugins.
pub fn keymap_names() -> Vec<String> {
    keymap_names_with(&contributions())
}

/// Builds keymap `name`: a bundled preset, else a plugin keymap layered over
/// Default, else Default.
pub fn keymap_with(name: &str, plugins: &Contributions) -> Keymap {
    let builtin = Keymap::preset_names().contains(&name) || name.is_empty();
    match plugins.keymap(name).filter(|_| !builtin) {
        Some(k) => keymap_from_contribution(k).0,
        None => Keymap::preset(name),
    }
}

/// [`keymap_with`] the current plugins.
pub fn keymap(name: &str) -> Keymap {
    keymap_with(name, &contributions())
}

/// A plugin keymap and what could not be applied (unknown actions, bad shortcuts).
pub fn keymap_from_contribution(k: &KeymapContribution) -> (Keymap, Vec<String>) {
    Keymap::from_bindings(
        &k.name,
        k.bindings
            .iter()
            .map(|(id, shortcuts)| (id.as_str(), shortcuts.as_slice())),
    )
}

/// Whether `name` is a keymap a plugin contributes (not a bundled preset).
pub fn is_plugin_keymap(name: &str, plugins: &Contributions) -> bool {
    !Keymap::preset_names().contains(&name) && plugins.keymap(name).is_some()
}

// ----- external tools ----------------------------------------------------------

/// One entry of Tools › External Tools.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolEntry {
    pub tool: ExternalTool,
    /// The plugin's display name, for plugin tools.
    pub plugin: Option<String>,
}

/// The user's tools, then the plugins' tools.
pub fn merge_external_tools(
    user: &[ExternalTool],
    plugins: &Contributions,
    plugin_name: impl Fn(&str) -> String,
) -> Vec<ToolEntry> {
    let mut out: Vec<ToolEntry> = user
        .iter()
        .map(|t| ToolEntry {
            tool: t.clone(),
            plugin: None,
        })
        .collect();
    out.extend(plugins.external_tools.iter().map(|c| ToolEntry {
        tool: c.tool.clone(),
        plugin: Some(plugin_name(&c.plugin_id)),
    }));
    out
}

/// The macros of run configurations (`IdeApp::expand_macros`) with their
/// values for a run on `file`: `("FilePath", "/p/x.py")` stands for
/// `$FilePath$`. They are the macros [`rustpy_plugins::TOOL_MACROS`] allows
/// plugin tools to use (besides `$PluginDir$`, which is filled in when a
/// plugin loads); External Tools and File Watchers fill in theirs with
/// `crate::external_tools`.
pub fn tool_macro_values(
    file: Option<&Path>,
    project_root: &Path,
    line: usize,
    interpreter_dir: Option<&Path>,
) -> Vec<(&'static str, String)> {
    let text = |p: Option<&std::ffi::OsStr>| {
        p.map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    vec![
        (
            "FilePath",
            file.map(|f| f.display().to_string()).unwrap_or_default(),
        ),
        (
            "FileDir",
            file.and_then(Path::parent)
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
        ),
        ("FileName", text(file.and_then(Path::file_name))),
        (
            "FileNameWithoutExtension",
            text(file.and_then(Path::file_stem)),
        ),
        ("ProjectFileDir", project_root.display().to_string()),
        ("LineNumber", line.to_string()),
        (
            "PyInterpreterDirectory",
            interpreter_dir
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
        ),
    ]
}

/// `text` with the macros replaced by their values as they are.
pub fn expand_macros_verbatim(text: &str, values: &[(&'static str, String)]) -> String {
    values.iter().fold(text.to_string(), |text, (name, value)| {
        text.replace(&format!("${name}$"), value)
    })
}

/// The command line a tool runs (macros unexpanded), for tooltips.
pub fn tool_command_line(tool: &ExternalTool) -> String {
    let mut text = tool.program.clone();
    if !tool.arguments.trim().is_empty() {
        text.push(' ');
        text.push_str(tool.arguments.trim());
    }
    text
}

/// The External Tools submenu of the Tools menu: the user's tools, then
/// plugin tools in a submenu per group. Returns the clicked tool. Shows
/// nothing when there are no tools.
pub fn external_tools_menu(ui: &mut egui::Ui, user: &[ExternalTool]) -> Option<ExternalTool> {
    let entries: Vec<_> = merge_external_tools(user, &contributions(), plugin_name)
        .into_iter()
        .filter(|entry| entry.tool.options.main_menu)
        .collect();
    if entries.is_empty() {
        return None;
    }
    let mut picked = None;
    ui.menu_button("External Tools", |ui| {
        let mut groups: BTreeMap<String, Vec<&ToolEntry>> = BTreeMap::new();
        for entry in &entries {
            groups
                .entry(entry.tool.group.trim().to_string())
                .or_default()
                .push(entry);
        }
        for (group, entries) in groups {
            let mut draw = |ui: &mut egui::Ui| {
                for entry in &entries {
                    let hover = match &entry.plugin {
                        Some(plugin) => format!(
                            "{}\nFrom the plugin {plugin}",
                            tool_command_line(&entry.tool)
                        ),
                        None => tool_command_line(&entry.tool),
                    };
                    if ui.button(&entry.tool.name).on_hover_text(hover).clicked() {
                        picked = Some(entry.tool.clone());
                        ui.close();
                    }
                }
            };
            if group.is_empty() {
                draw(ui);
            } else {
                ui.menu_button(group, draw);
            }
        }
    });
    picked
}

// ----- file templates ----------------------------------------------------------

/// Prefix of the file-kind strings of plugin file templates (`plugin:<id>/<name>`).
const KIND_PREFIX: &str = "plugin:";

/// `&'static str` for a file kind: the project view and the New File dialog
/// pass kinds as static strings. Each distinct kind is allocated once per
/// process (a handful of plugin templates), never again.
fn intern(kind: String) -> &'static str {
    static KINDS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut kinds = KINDS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = kinds.iter().find(|k| **k == kind) {
        return k;
    }
    let k: &'static str = Box::leak(kind.into_boxed_str());
    kinds.push(k);
    k
}

/// The file kind of a plugin file template.
pub fn file_template_kind(t: &FileTemplateContribution) -> String {
    format!("{KIND_PREFIX}{}/{}", t.plugin_id, t.name)
}

/// New-menu entries for the plugins' file templates: (label, kind).
pub fn file_template_menu_entries() -> Vec<(String, &'static str)> {
    contributions()
        .file_templates
        .iter()
        .map(|t| (t.name.clone(), intern(file_template_kind(t))))
        .collect()
}

/// The plugin file template of `kind`.
pub fn file_template_for(kind: &str, plugins: &Contributions) -> Option<FileTemplateContribution> {
    kind.strip_prefix(KIND_PREFIX)?;
    plugins
        .file_templates
        .iter()
        .find(|t| file_template_kind(t) == kind)
        .cloned()
}

/// Suggested name for a new file from `kind`: the template file's own name
/// (`conftest.py`, `test_module.py`), else `new_file.<ext>`.
pub fn default_file_name(kind: &str) -> Option<String> {
    let t = file_template_for(kind, &contributions())?;
    Some(suggested_file_name(&t))
}

fn suggested_file_name(t: &FileTemplateContribution) -> String {
    let own = t.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if own
        .to_ascii_lowercase()
        .ends_with(&format!(".{}", t.extension.to_ascii_lowercase()))
    {
        own.to_string()
    } else {
        format!("new_file.{}", t.extension)
    }
}

/// The file name to create for `name` typed by the user: the template's
/// extension is added when the name has none.
pub fn file_name_for(name: &str, kind: &str) -> Option<String> {
    let t = file_template_for(kind, &contributions())?;
    Some(with_extension(name, &t.extension))
}

fn with_extension(name: &str, extension: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name);
    if base.contains('.') {
        name.to_string()
    } else {
        format!("{name}.{extension}")
    }
}

/// The text of a new file `file_name` from a plugin template, with
/// `${NAME}` (file name without extension), `${FILE_NAME}`, `${PROJECT_NAME}`,
/// `${USER}`, `${DATE}`, `${TIME}`, `${YEAR}`, `${MONTH}` and `${DAY}` filled.
pub fn render_file_template(kind: &str, file_name: &str, project_name: &str) -> Option<String> {
    let t = file_template_for(kind, &contributions())?;
    Some(render_with_now(
        &t,
        file_name,
        project_name,
        chrono::Local::now().naive_local(),
    ))
}

fn render_with_now(
    t: &FileTemplateContribution,
    file_name: &str,
    project_name: &str,
    now: chrono::NaiveDateTime,
) -> String {
    let base = Path::new(file_name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_name);
    let stem = Path::new(base)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(base);
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let date = now.format("%Y-%m-%d").to_string();
    let time = now.format("%H:%M").to_string();
    let (year, month, day) = (
        now.format("%Y").to_string(),
        now.format("%m").to_string(),
        now.format("%d").to_string(),
    );
    t.render(&[
        ("NAME", stem),
        ("FILE_NAME", base),
        ("PROJECT_NAME", project_name),
        ("USER", &user),
        ("DATE", &date),
        ("TIME", &time),
        ("YEAR", &year),
        ("MONTH", &month),
        ("DAY", &day),
    ])
}

/// Enabled, validated CSV-table viewer registrations, in deterministic plugin order.
pub fn table_editors() -> Vec<rustpy_plugins::TableEditorContribution> {
    contributions().table_editors.clone()
}

// ----- TextMate grammars -------------------------------------------------------

/// TextMate grammars of the enabled plugins (language, extensions, file,
/// scope name). The editor's grammar registry (`app/textmate_bundles.rs`)
/// loads them before the user's TextMate bundles whenever [`generation`]
/// changes: files without a built-in highlighter whose extension a plugin
/// declares are highlighted with its grammar.
pub fn textmate_grammars() -> Vec<GrammarContribution> {
    contributions().textmate_grammars.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_plugins::{
        ColorSchemeContribution, ExternalToolContribution, LiveTemplateContribution,
    };
    use rustpy_templates::TemplateContext;

    fn template(abbr: &str, text: &str, group: &str) -> LiveTemplate {
        LiveTemplate::new(abbr, "", text, &[TemplateContext::Any], group)
    }

    fn plugin_templates(id: &str, templates: Vec<LiveTemplate>) -> LiveTemplateContribution {
        LiveTemplateContribution {
            plugin_id: id.into(),
            path: PathBuf::from(format!("/p/{id}/t.json")),
            templates,
        }
    }

    #[test]
    fn live_templates_merge_builtin_then_plugins_then_user() {
        let builtin = vec![
            template("main", "builtin main", "Python"),
            template("iter", "builtin iter", "Python"),
        ];
        let plugins = Contributions {
            live_templates: vec![
                plugin_templates(
                    "com.a",
                    vec![
                        template("main", "plugin a main", "A"),
                        template("bp", "breakpoint()", "A"),
                    ],
                ),
                plugin_templates(
                    "com.b",
                    vec![
                        template("bp", "plugin b bp", "B"),
                        template("fx", "fixture", "B"),
                    ],
                ),
            ],
            ..Contributions::default()
        };
        let user = vec![template("fx", "my fixture", "user")];
        let merged = merge_live_templates(builtin, &plugins, user);
        let by_abbr = |a: &str| {
            merged
                .iter()
                .filter(|t| t.abbreviation == a)
                .map(|t| t.template.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            by_abbr("main"),
            ["plugin a main"],
            "a plugin replaces a builtin template"
        );
        assert_eq!(by_abbr("iter"), ["builtin iter"]);
        assert_eq!(
            by_abbr("bp"),
            ["breakpoint()"],
            "the first plugin wins among plugins"
        );
        assert_eq!(
            by_abbr("fx"),
            ["my fixture"],
            "the user's template wins over plugins"
        );
        assert_eq!(merged.len(), 4, "abbreviations stay unique: {merged:?}");
        assert_eq!(
            template_plugin(
                &merged
                    .iter()
                    .find(|t| t.abbreviation == "main")
                    .unwrap()
                    .clone(),
                &plugins
            )
            .as_deref(),
            Some("com.a")
        );
        assert_eq!(
            template_plugin(&template("iter", "builtin iter", "Python"), &plugins),
            None
        );
        assert_eq!(
            template_plugin(&template("fx", "my fixture", "user"), &plugins),
            None,
            "a user override is not the plugin's"
        );
    }

    fn scheme(name: &str) -> ColorSchemeContribution {
        let mut s = ColorScheme::dark();
        s.name = name.into();
        ColorSchemeContribution {
            plugin_id: "com.x".into(),
            path: PathBuf::from("/p/x.json"),
            scheme: s,
        }
    }

    #[test]
    fn scheme_names_list_user_schemes_before_plugin_schemes() {
        let plugins = Contributions {
            color_schemes: vec![
                scheme("Plugin Night"),
                scheme("Solarized Dark"),
                scheme("Mine"),
                scheme("Dark"),
            ],
            ..Contributions::default()
        };
        assert!(ColorScheme::builtin_names().contains(&"Solarized Dark"));
        assert_eq!(
            merge_scheme_names(vec!["Mine".into(), "Other".into()], &plugins),
            ["Mine", "Other", "Plugin Night"]
        );
        assert_eq!(
            merge_scheme_names(Vec::new(), &Contributions::default()),
            Vec::<String>::new()
        );
    }

    fn keymap_contribution(name: &str, bindings: &[(&str, &[&str])]) -> KeymapContribution {
        KeymapContribution {
            plugin_id: "com.keys".into(),
            path: PathBuf::from("/p/k.json"),
            name: name.into(),
            bindings: bindings
                .iter()
                .map(|(id, s)| (id.to_string(), s.iter().map(|x| x.to_string()).collect()))
                .collect(),
        }
    }

    #[test]
    fn plugin_keymaps_are_presets_layered_over_default() {
        use crate::actions::{Action, KeyChord};
        let plugins = Contributions {
            keymaps: vec![
                keymap_contribution(
                    "Plugin Navigation",
                    &[
                        ("$Copy", &["Alt+W"]),
                        ("GotoAction", &["Alt+X"]),
                        ("NoSuchAction", &["F1"]),
                        ("GotoLine", &["Ctrl+Bogus"]),
                    ],
                ),
                keymap_contribution("VS Code", &[("GotoFile", &["F4"])]),
            ],
            ..Contributions::default()
        };
        let names = keymap_names_with(&plugins);
        assert_eq!(
            names.iter().filter(|n| *n == "VS Code").count(),
            1,
            "{names:?}"
        );
        assert_eq!(names.last().map(String::as_str), Some("Plugin Navigation"));
        let km = keymap_with("Plugin Navigation", &plugins);
        assert_eq!(km.name, "Plugin Navigation");
        let chord = |s: &str| KeyChord::parse(s).unwrap();
        assert_eq!(km.lookup(chord("Alt+W")), Some(Action::Copy));
        assert_eq!(
            km.lookup(chord("Ctrl+C")),
            None,
            "the listed action gets exactly the plugin's shortcuts"
        );
        assert_eq!(km.lookup(chord("Alt+X")), Some(Action::FindAction));
        assert_eq!(
            km.lookup(chord("Ctrl+S")),
            Some(Action::Save),
            "everything else stays Default"
        );
        let (_, problems) = keymap_from_contribution(&plugins.keymaps[0]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems.iter().any(|p| p.contains("NoSuchAction"))
                && problems.iter().any(|p| p.contains("Ctrl+Bogus")),
            "{problems:?}"
        );
        // A bundled preset keeps its own bindings even when a plugin keymap has its name.
        assert_eq!(
            keymap_with("VS Code", &plugins).lookup(chord("Ctrl+P")),
            Some(Action::GoToFile)
        );
        assert!(
            is_plugin_keymap("Plugin Navigation", &plugins)
                && !is_plugin_keymap("VS Code", &plugins)
        );
        assert_eq!(keymap_with("Gone", &plugins).name, "Default");
    }

    #[test]
    fn plugin_tools_follow_the_users_tools() {
        let mine = ExternalTool {
            name: "Mine".into(),
            program: "echo".into(),
            ..Default::default()
        };
        let theirs = ExternalTool {
            name: "Lint".into(),
            group: "Linters".into(),
            program: "ruff".into(),
            arguments: "check $FilePath$".into(),
            ..Default::default()
        };
        let plugins = Contributions {
            external_tools: vec![ExternalToolContribution {
                plugin_id: "com.lint".into(),
                tool: theirs.clone(),
            }],
            ..Contributions::default()
        };
        let entries = merge_external_tools(std::slice::from_ref(&mine), &plugins, |id| {
            format!("name of {id}")
        });
        assert_eq!(
            entries,
            vec![
                ToolEntry {
                    tool: mine,
                    plugin: None
                },
                ToolEntry {
                    tool: theirs.clone(),
                    plugin: Some("name of com.lint".into())
                }
            ]
        );
        assert_eq!(tool_command_line(&theirs), "ruff check $FilePath$");
    }

    fn file_template(name: &str, ext: &str, path: &str, text: &str) -> FileTemplateContribution {
        FileTemplateContribution {
            plugin_id: "com.kit".into(),
            path: PathBuf::from(path),
            name: name.into(),
            extension: ext.into(),
            text: text.into(),
        }
    }

    #[test]
    fn file_templates_have_kinds_names_and_variables() {
        let module = file_template("Pytest Module", "py", "/p/file-templates/test_module.py", "\"\"\"${NAME} in ${PROJECT_NAME} (${FILE_NAME}) ${DATE} ${TIME} ${YEAR}-${MONTH}-${DAY} ${UNKNOWN}\"\"\"\n");
        let odd = file_template("Odd", "toml", "/p/file-templates/template.txt", "");
        let plugins = Contributions {
            file_templates: vec![module.clone(), odd.clone()],
            ..Contributions::default()
        };
        let kind = file_template_kind(&module);
        assert_eq!(kind, "plugin:com.kit/Pytest Module");
        assert_eq!(file_template_for(&kind, &plugins), Some(module.clone()));
        assert_eq!(
            file_template_for("python", &plugins),
            None,
            "builtin kinds are not plugin kinds"
        );
        assert_eq!(file_template_for("plugin:com.kit/Gone", &plugins), None);
        assert_eq!(suggested_file_name(&module), "test_module.py");
        assert_eq!(suggested_file_name(&odd), "new_file.toml");
        assert_eq!(with_extension("test_api", "py"), "test_api.py");
        assert_eq!(with_extension("test_api.py", "py"), "test_api.py");
        assert_eq!(with_extension("pkg.d/test_x", "py"), "pkg.d/test_x.py");
        let now = chrono::NaiveDate::from_ymd_opt(2026, 3, 4)
            .unwrap()
            .and_hms_opt(5, 6, 7)
            .unwrap();
        assert_eq!(
            render_with_now(&module, "tests/test_api.py", "shop", now),
            "\"\"\"test_api in shop (test_api.py) 2026-03-04 05:06 2026-03-04 ${UNKNOWN}\"\"\"\n"
        );
        assert!(
            std::ptr::eq(intern(kind.clone()), intern(kind)),
            "kinds are interned once"
        );
    }

    /// Plugin tools may use the macros of [`rustpy_plugins::TOOL_MACROS`]
    /// (besides `$PluginDir$`, which is filled in when the plugin loads). The
    /// External Tools runner must know each of them, or a plugin tool would
    /// pass `$X$` on as written; run configurations fill in the same ones.
    #[test]
    fn the_ide_fills_every_plugin_tool_macro() {
        let allowed: Vec<&str> = rustpy_plugins::TOOL_MACROS
            .iter()
            .copied()
            .filter(|m| *m != "PluginDir")
            .collect();
        let known: Vec<&str> = crate::external_tools::MACROS
            .iter()
            .map(|(name, _)| *name)
            .collect();
        for m in &allowed {
            assert!(known.contains(m), "${m}$ is not an External Tools macro");
        }
        let ctx = crate::external_tools::MacroContext {
            file: Some(PathBuf::from("/p/my pkg/test_x.py")),
            project_root: Some(PathBuf::from("/p")),
            line: Some(7),
            interpreter: Some(PathBuf::from("/venv/bin/python")),
            ..Default::default()
        };
        let all: String = allowed.iter().map(|m| format!("[${m}$]")).collect();
        let expected = "[/p/my pkg/test_x.py][/p/my pkg][test_x.py][test_x][/p][7][/venv/bin]";
        assert_eq!(crate::external_tools::expand_macros(&all, &ctx), expected);
        let values = tool_macro_values(
            Some(Path::new("/p/my pkg/test_x.py")),
            Path::new("/p"),
            7,
            Some(Path::new("/venv/bin")),
        );
        assert_eq!(
            values.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            allowed
        );
        assert_eq!(expand_macros_verbatim(&all, &values), expected);
        assert_eq!(
            expand_macros_verbatim("$FileDir$/out", &values),
            "/p/my pkg/out"
        );
        // Without a file the file macros are empty.
        let none = tool_macro_values(None, Path::new("/p"), 1, None);
        assert_eq!(expand_macros_verbatim("[$FilePath$][$FileDir$][$FileName$][$FileNameWithoutExtension$][$PyInterpreterDirectory$]", &none), "[][][][][]");
    }

    /// Runs `tool` on `file` the way the IDE does: the command line of
    /// [`crate::external_tools::tool_invocation`], spawned as the Run tool
    /// window spawns it (`app/tools_ui.rs`, `run_invocation`). Returns stdout.
    #[cfg(unix)]
    fn run_like_the_ide(tool: &ExternalTool, file: &Path, project: &Path) -> String {
        use rustpy_project::{RunConfig, RunConfigKind};
        use rustpy_run::RunSession;
        let ctx = crate::external_tools::MacroContext {
            file: Some(file.to_path_buf()),
            project_root: Some(project.to_path_buf()),
            ..Default::default()
        };
        let inv = crate::external_tools::tool_invocation(tool, &ctx).unwrap();
        let working_dir = inv
            .working_dir
            .map(|d| project.join(d))
            .unwrap_or_else(|| project.to_path_buf());
        let cfg = RunConfig {
            name: tool.name.clone(),
            kind: RunConfigKind::Program {
                program: inv.program,
                args: inv.args,
            },
            working_dir: Some(working_dir),
            temporary: true,
            add_content_roots: false,
            add_source_roots: false,
            ..Default::default()
        };
        let (program, args, cwd, env) = RunSession::command_for(&cfg, None, project);
        let out = rustpy_core::interpreter_launch::interpreter_command(&program)
            .args(&args)
            .current_dir(&cwd)
            .envs(&env)
            .output()
            .unwrap_or_else(|e| panic!("{} ({:?}): {e}", program.display(), tool.program));
        assert!(
            out.status.success(),
            "{program:?} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Review finding (security): a file watcher `printf '[%s]\n' "$(basename
    /// $FilePath$)"` on `x;touch PWNED;.py` created PWNED while tools ran as
    /// `sh` scripts. Tools run their program directly: a file name is an
    /// argument (or part of one), and the IDE never reads it as a command.
    #[cfg(unix)]
    #[test]
    fn file_names_never_run_as_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        let watcher = ExternalTool {
            name: "Watcher".into(),
            program: "printf".into(),
            arguments: "'[%s]\\n' \"$(basename $FilePath$)\" $FileName$".into(),
            ..Default::default()
        };
        // A line break stays one too (a script for `sh` once lost it: inside
        // "…" it was written as a backslash-newline, a line continuation).
        for name in [
            "x;touch PWNED;.py",
            "a\"$(touch PWNED)\".py",
            "b `touch PWNED` c.py",
            "d'$(touch PWNED)'.py",
            "line\nbreak.py",
        ] {
            let file = project.join(name);
            assert_eq!(
                run_like_the_ide(&watcher, &file, project),
                format!("[$(basename {})]\n[{name}]\n", file.display())
            );
        }
        assert!(
            !project.join("PWNED").exists(),
            "a file name ran as a command"
        );
    }

    /// Plugin tools run through the same runner as the user's tools. Their
    // Only the exact helper process ever writable-opens source or installed
    // executable inodes. The default-parallel parent cannot pass such writers
    // to another concurrently spawned pre-exec child.
    #[cfg(unix)]
    const TOOL_FIXTURE_SCRIPT: &str = "#!/bin/sh\nprintf '[%s]\\n' \"$0\" \"$@\"\n";

    #[cfg(unix)]
    #[test]
    fn plugin_tool_writer_child() {
        use std::io::Write;
        use std::os::unix::fs::MetadataExt;
        let Some(source) = std::env::var_os("RUSTPY_PLUGIN_TOOL_WRITER_SOURCE") else {
            return;
        };
        let source = PathBuf::from(source);
        let config = PathBuf::from(std::env::var_os("RUSTPY_PLUGIN_TOOL_WRITER_CONFIG").unwrap());
        let script = source.join("bin/run.sh");
        if !script.exists() {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&script)
                .unwrap();
            file.write_all(TOOL_FIXTURE_SCRIPT.as_bytes()).unwrap();
            drop(file);
        }
        assert_eq!(
            std::fs::read(&script).unwrap(),
            TOOL_FIXTURE_SCRIPT.as_bytes()
        );
        let installed = rustpy_plugins::PluginRegistry::load(&config)
            .install_from_dir(&source)
            .unwrap()
            .dir;
        let meta = std::fs::metadata(installed.join("bin/run.sh")).unwrap();
        assert_eq!(
            std::fs::read(installed.join("bin/run.sh")).unwrap(),
            TOOL_FIXTURE_SCRIPT.as_bytes()
        );
        println!(
            "PLUGIN_WRITER_ACK {}",
            serde_json::json!({
                "pid": std::process::id(), "nonce": std::env::var("RUSTPY_PLUGIN_TOOL_WRITER_NONCE").unwrap(),
                "path": installed, "dev": meta.dev(), "ino": meta.ino(), "len": meta.len(), "mode": meta.mode()
            })
        );
    }

    #[cfg(unix)]
    fn install_tool_fixture_in_owned_child(source: &Path, config: &Path) -> PathBuf {
        use std::os::unix::fs::MetadataExt;
        struct Owned(std::process::Child);
        impl Drop for Owned {
            fn drop(&mut self) {
                if !matches!(self.0.try_wait(), Ok(Some(_))) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
        }
        let nonce = format!("{}:{}", std::process::id(), config.display());
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .args([
                "--exact",
                "plugin_host::tests::plugin_tool_writer_child",
                "--nocapture",
            ])
            .env("RUSTPY_PLUGIN_TOOL_WRITER_SOURCE", source)
            .env("RUSTPY_PLUGIN_TOOL_WRITER_CONFIG", config)
            .env("RUSTPY_PLUGIN_TOOL_WRITER_NONCE", &nonce)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut owned = Owned(command.spawn().unwrap());
        let pid = owned.0.id();
        drop(owned.0.stdin.take());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while owned.0.try_wait().unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "owned plugin writer exceeded three seconds"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut stdout = String::new();
        let mut stderr = String::new();
        use std::io::Read;
        owned
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        owned
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(owned.0.wait().unwrap().success(), "{stdout}\n{stderr}");
        let ack: serde_json::Value = serde_json::from_str(
            stdout
                .lines()
                .find_map(|line| line.split_once("PLUGIN_WRITER_ACK ").map(|(_, ack)| ack))
                .expect("original writer ACK"),
        )
        .unwrap();
        assert_eq!(ack["pid"], pid);
        assert_eq!(ack["nonce"], nonce);
        let installed = config.join("plugins/com.example.paths");
        assert_eq!(ack["path"], installed.to_str().unwrap());
        let meta = std::fs::metadata(installed.join("bin/run.sh")).unwrap();
        assert_eq!(ack["dev"], meta.dev());
        assert_eq!(ack["ino"], meta.ino());
        assert_eq!(ack["len"], meta.len());
        assert_eq!(ack["mode"], meta.mode());
        assert_ne!(meta.mode() & 0o111, 0);
        assert_eq!(
            std::fs::read(installed.join("bin/run.sh")).unwrap(),
            TOOL_FIXTURE_SCRIPT.as_bytes()
        );
        installed
    }

    /// `$PluginDir$` is filled in when the plugin loads, so it must reach the
    /// program exactly, whatever the configuration directory is called (`'`,
    /// `"`, `$`, `\`, spaces), and a `$PluginDir$` program must be the file
    /// that was checked and made executable.
    #[cfg(unix)]
    #[test]
    fn plugin_tools_get_their_exact_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::create_dir_all(src.join("data")).unwrap();
        let manifest = serde_json::json!({"id": "com.example.paths", "name": "Paths", "version": "1.0.0", "contributes": {"external_tools": [
            {"name": "Bare", "program": "$PluginDir$/bin/run.sh", "arguments": "$PluginDir$/data \"$PluginDir$/data/a b.txt\" '$PluginDir$/data/plain.txt' --file=$FilePath$"},
            {"name": "Quoted", "program": "\"$PluginDir$\"/bin/'run'.sh", "arguments": "--dir=\"$PluginDir$\" \"it's\" 'say \"hi\"'"},
            {"name": "Inner", "program": "$PluginDir$/bin/'run'.sh"},
            {"name": "Single", "program": "'$PluginDir$'/bin/run.sh", "arguments": "\"$FileName$\" '' $FileDir$/out"},
            {"name": "Whole", "program": "\"$PluginDir$/bin/run.sh\"", "arguments": "-x", "working_dir": "$PluginDir$"}
        ]}});
        std::fs::write(src.join("plugin.json"), manifest.to_string()).unwrap();
        std::fs::write(src.join("data/a b.txt"), "").unwrap();
        std::fs::write(src.join("data/plain.txt"), "").unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let file = project.join("x;touch PWNED;.py");
        for config in [
            "plain",
            "it's \"my\" $config \\ `dir`",
            "$FilePath$ $ProjectFileDir$ $Prompt$ $$ line\nbreak",
        ] {
            let config_path = tmp.path().join(config);
            let installed = install_tool_fixture_in_owned_child(&src, &config_path);
            let registry = rustpy_plugins::PluginRegistry::load(&config_path);
            let p = installed.display().to_string();
            let tools = registry.contributions().external_tools;
            let out = |name: &str| {
                run_like_the_ide(
                    &tools.iter().find(|c| c.tool.name == name).unwrap().tool,
                    &file,
                    &project,
                )
            };
            assert_eq!(out("Bare"), format!("[{p}/bin/run.sh]\n[{p}/data]\n[{p}/data/a b.txt]\n[{p}/data/plain.txt]\n[--file={}]\n", file.display()), "{config}");
            assert_eq!(
                out("Quoted"),
                format!("[{p}/bin/run.sh]\n[--dir={p}]\n[it's]\n[say \"hi\"]\n"),
                "{config}"
            );
            assert_eq!(out("Inner"), format!("[{p}/bin/run.sh]\n"), "{config}");
            assert_eq!(
                out("Single"),
                format!(
                    "[{p}/bin/run.sh]\n[x;touch PWNED;.py]\n[]\n[{}/out]\n",
                    project.display()
                ),
                "{config}"
            );
            assert_eq!(
                out("Whole"),
                format!("[{p}/bin/run.sh]\n[-x]\n"),
                "{config}"
            );
        }
        assert!(!project.join("PWNED").exists());
    }
}
