//! File and Code Templates in the application (roadmap §1.7): the Project
//! view's *New* menu creates files from templates (`${NAME}`, `${DATE}`, …,
//! and the *File Header* include with the copyright notice), Tools › Save
//! File as Template, and Settings › Editor › File and Code Templates.

use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use std::collections::BTreeMap;

use super::*;
use rustpy_templates::file::{self as ft, FileTemplate, Origin, TemplateSet, TemplateStore};

/// New › <template> and File › New File…: the template and the name of the file to create.
pub(super) struct NewFromTemplateState {
    pub dir: PathBuf,
    /// Template name; empty: an empty file.
    pub template: String,
    pub name: String,
    /// The name proposed for the template (replaced when another template is picked, unless edited).
    pub proposed: String,
    /// Templates of the New menu, to pick another one.
    pub templates: Vec<String>,
    pub error: Option<String>,
    /// The name field was focused and its text selected.
    pub focused: bool,
}

/// Which templates the settings page edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Scope {
    /// `<config>/fileTemplates.json`, for every project.
    #[default]
    Ide,
    /// `<project>/.rustpy/fileTemplates.json`, wins over the IDE's.
    Project,
}

/// State of Settings › Editor › File and Code Templates.
#[derive(Default)]
pub(super) struct FileTemplatesPage {
    pub scope: Scope,
    /// The Includes tab (else Files).
    pub includes: bool,
    pub selected: Option<String>,
    /// The template being edited and the name it had when loaded (`None`: a new one).
    pub draft: Option<(FileTemplate, Option<String>)>,
    /// The scheme and project the draft was made in: elsewhere it is dropped,
    /// so it is never saved into another store.
    draft_for: Option<(Scope, Option<PathBuf>)>,
    pub message: Option<String>,
    /// The IDE-level and the project's stores, with the store generation and
    /// project they were loaded for.
    cache: Option<(u64, Option<PathBuf>, TemplateStore, Option<TemplateStore>)>,
}

/// The templates the settings page shows.
struct Schemes {
    /// Those of the scheme being edited (see [`scheme_set`]).
    shown: TemplateSet,
    /// The Default scheme's: what removing a project template brings back.
    default: TemplateSet,
    /// What this project gets (the project's templates win).
    effective: TemplateSet,
}

impl FileTemplatesPage {
    fn schemes(&mut self, project: Option<&Path>) -> Schemes {
        let generation = ft::generation();
        let stale = !matches!(&self.cache, Some((g, p, _, _)) if *g == generation && p.as_deref() == project);
        if stale {
            let ide = ft::load_store(&ft::ide_store_path(&Settings::config_dir()));
            let proj = project.map(|r| ft::load_store(&ft::project_store_path(r)));
            self.cache = Some((generation, project.map(Path::to_path_buf), ide, proj));
        }
        let (_, _, ide, proj) = self.cache.as_ref().expect("loaded above");
        Schemes {
            shown: scheme_set(self.scope, ide, proj.as_ref()),
            default: scheme_set(Scope::Ide, ide, None),
            effective: project_new_ui::generation_defaults(TemplateSet::merged(ide, proj.as_ref())),
        }
    }
}

/// The templates the page lists for `scope`. The Default scheme holds the
/// builtins and the IDE-level store only: showing the project's version there
/// would let Save copy a project template into every project, over the
/// user's own. The Project scheme is what this project gets.
fn scheme_set(scope: Scope, ide: &TemplateStore, project: Option<&TemplateStore>) -> TemplateSet {
    match scope {
        Scope::Ide => project_new_ui::generation_defaults(TemplateSet::merged(ide, None)),
        Scope::Project => project_new_ui::generation_defaults(TemplateSet::merged(ide, project)),
    }
}

/// What Reset / Delete does to a template of `origin` in `scope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Removal {
    /// The scheme does not store it: nothing to remove.
    None,
    /// Removing it brings back the builtin (Default scheme) or the Default
    /// scheme's version (Project scheme).
    Reset,
    Delete,
}

fn removal(
    scope: Scope,
    name: &str,
    origin: Origin,
    default: &TemplateSet,
    includes: bool,
) -> Removal {
    match (scope, origin) {
        (Scope::Ide, Origin::Modified) => Removal::Reset,
        (Scope::Ide, Origin::Custom) => Removal::Delete,
        (Scope::Project, Origin::Project) => {
            let list = if includes {
                &default.includes
            } else {
                &default.templates
            };
            if list.iter().any(|(t, _)| t.name == name) {
                Removal::Reset
            } else {
                Removal::Delete
            }
        }
        _ => Removal::None,
    }
}

/// Saves `t`, loaded as `original` (`None`: a new template), into the store at
/// `path`; a renamed template replaces its old entry (a builtin's name stays
/// builtin). Returns the saved name.
fn save_template(
    path: &Path,
    includes: bool,
    original: Option<&str>,
    t: FileTemplate,
) -> std::io::Result<String> {
    let mut store = ft::load_store(path);
    let name = t.name.trim().to_string();
    // A new name that another template already has would replace it silently.
    if original != Some(name.as_str()) {
        let list = if includes {
            &store.includes
        } else {
            &store.templates
        };
        if list.iter().any(|x| x.name == name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("a template named \"{name}\" already exists; choose another name"),
            ));
        }
    }
    if let Some(old) = original.filter(|o| *o != name) {
        store.remove(includes, old);
    }
    store.upsert(
        includes,
        FileTemplate {
            name: name.clone(),
            ..t
        },
    );
    ft::save_store(path, &store)?;
    Ok(name)
}

/// The Preview shows at most this many characters.
const PREVIEW_CHARS: usize = 20_000;

fn is_python(file_name: &str) -> bool {
    file_name.ends_with(".py") || file_name.ends_with(".pyw")
}

/// The file name the New dialog proposes for `template` (empty: an empty file).
fn proposed_name(set: &TemplateSet, template: &str) -> String {
    if template.is_empty() {
        return "file.txt".into();
    }
    set.template(template)
        .map(FileTemplate::default_file_name)
        .unwrap_or_else(|| "untitled".into())
}

/// Template variables for a new file at `path` in the project at `root`, with
/// the copyright notice (Code Style › Copyright) when it goes into new files.
fn file_vars(
    path: &Path,
    root: Option<&Path>,
    copyright: &rustpy_settings::CopyrightSettings,
    user: &str,
    now: chrono::NaiveDateTime,
) -> BTreeMap<String, String> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = match (path.parent(), root) {
        (Some(d), Some(r)) => d.strip_prefix(r).map(Path::to_path_buf).unwrap_or_default(),
        _ => PathBuf::new(),
    };
    let project_name = root
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let c = copyright;
    let copyright = (c.enabled && c.add_to_new_files)
        .then(|| ft::expand_copyright(&c.text, now, &project_name, user, &file_name))
        .and_then(|notice| ft::copyright_comment(&notice, &file_name));
    ft::variables(
        &ft::NewFile {
            file_name,
            dir,
            project_name,
            user: user.to_string(),
            copyright,
        },
        now,
    )
}

/// Creates `typed` in `dir` from template `template` of `set` (empty: an empty
/// file): the template's extension is added when `typed` has none, a name with
/// another extension is refused (no Python boilerplate in `notes.txt`), an
/// existing file is never overwritten, a template that runs away is refused,
/// and Python templates marked *Reformat according to style* go through the
/// formatter.
#[cfg(test)]
fn create_file(
    set: &TemplateSet,
    dir: &Path,
    template: &str,
    typed: &str,
    vars: impl Fn(&Path) -> BTreeMap<String, String>,
    format: &rustpy_format::FormatOptions,
) -> Result<PathBuf, String> {
    create_file_checked(set, dir, template, typed, |path| Ok(vars(path)), format)
}

fn profile_file_vars(
    path: &Path,
    root: Option<&Path>,
    legacy: &rustpy_settings::CopyrightSettings,
    profiles: &rustpy_settings::copyright::CopyrightProfilesSettings,
    user: &str,
    now: chrono::NaiveDateTime,
) -> Result<BTreeMap<String, String>, String> {
    let mut vars = file_vars(path, root, legacy, user, now);
    if !legacy.enabled || !legacy.add_to_new_files {
        return Ok(vars);
    }
    profiles.validate()?;
    if profiles.is_unconfigured() {
        return Ok(vars);
    }
    if !rustpy_settings::copyright::supports_comments(path) {
        vars.insert("COPYRIGHT".into(), String::new());
        return Ok(vars);
    }
    let notice = match profiles.profile_for(path, root)? {
        None => String::new(),
        Some(profile) => {
            let year = now.format("%Y").to_string();
            let date = now.format("%Y-%m-%d").to_string();
            let project = root
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            rustpy_settings::copyright::plan_update(
                "",
                path,
                profile,
                &rustpy_settings::copyright::Variables {
                    year: &year,
                    date: &date,
                    project: &project,
                    user,
                    file: &file,
                },
            )?
            .map(|edit| edit.replacement.trim_end_matches('\n').to_owned())
            .unwrap_or_default()
        }
    };
    vars.insert("COPYRIGHT".into(), notice);
    Ok(vars)
}

#[cfg(test)]
fn create_file_checked(
    set: &TemplateSet,
    dir: &Path,
    template: &str,
    typed: &str,
    vars: impl Fn(&Path) -> Result<BTreeMap<String, String>, String>,
    format: &rustpy_format::FormatOptions,
) -> Result<PathBuf, String> {
    create_file_checked_with_header(
        set,
        dir,
        template,
        typed,
        vars,
        |_, text| Ok(text.to_string()),
        format,
    )
}

#[cfg(test)]
fn create_file_checked_with_header(
    set: &TemplateSet,
    dir: &Path,
    template: &str,
    typed: &str,
    vars: impl Fn(&Path) -> Result<BTreeMap<String, String>, String>,
    header: impl Fn(&Path, &str) -> Result<String, String>,
    format: &rustpy_format::FormatOptions,
) -> Result<PathBuf, String> {
    create_prepared_file(
        FileTemplateRequest {
            set,
            dir,
            template,
            typed,
        },
        vars,
        header,
        |_| format.clone(),
        |_, text| Ok(text.as_bytes().to_vec()),
    )
    .map(|created| created.path)
}

struct CreatedFile {
    path: PathBuf,
    expansion: Option<rustpy_templates::Expansion>,
}
/// Captured template/destination identity stays together while the per-file
/// expansion, copyright, formatter and encoding policies run in their original order.
struct FileTemplateRequest<'a> {
    set: &'a TemplateSet,
    dir: &'a Path,
    template: &'a str,
    typed: &'a str,
}
fn create_prepared_file(
    request: FileTemplateRequest<'_>,
    vars: impl Fn(&Path) -> Result<BTreeMap<String, String>, String>,
    header: impl Fn(&Path, &str) -> Result<String, String>,
    format: impl Fn(&Path) -> rustpy_format::FormatOptions,
    encode: impl Fn(&Path, &str) -> Result<Vec<u8>, String>,
) -> Result<CreatedFile, String> {
    let FileTemplateRequest {
        set,
        dir,
        template,
        typed,
    } = request;
    let t = if template.is_empty() {
        None
    } else {
        Some(
            set.template(template)
                .cloned()
                .ok_or_else(|| format!("No file template named \"{template}\""))?,
        )
    };
    let file_name = match &t {
        Some(t) => t.file_name_for(typed),
        None => typed.trim().to_string(),
    };
    if file_name.is_empty() {
        return Err("Enter a file name".into());
    }
    if let Some(t) = t.as_ref().filter(|t| !t.accepts(&file_name)) {
        return Err(format!(
            "{file_name} is not a .{} file: choose \"File (empty)\" or another template",
            t.extension.trim().trim_start_matches('.')
        ));
    }
    let path = dir.join(&file_name);
    if path.exists() {
        return Err(format!("{file_name} already exists"));
    }
    let variables = if t.is_some() {
        vars(&path)?
    } else {
        BTreeMap::new()
    };
    let live = t
        .as_ref()
        .filter(|t| t.live_templates)
        .map(|t| rustpy_templates::file_live::PreparedLive::render(set, t, &variables))
        .transpose()
        .map_err(|e| format!("Cannot prepare live fields: {e}"))?;
    let mut text = match (&t, &live) {
        (_, Some(live)) => live.text.clone(),
        (Some(t), None) => set
            .try_render(t, &variables)
            .map_err(|e| format!("Cannot use \"{}\": {e}", t.name))?,
        _ => String::new(),
    };
    text = header(&path, &text)?;
    if t.as_ref().is_some_and(|t| t.reformat) && is_python(&file_name) {
        if let Some(live) = &live {
            text = live
                .format_python(&text, |source| {
                    rustpy_format::format_source(source, &format(&path))
                })
                .map_err(|error| format!("Cannot reformat live template: {error}"))?;
        } else if let Ok(formatted) = rustpy_format::format_source(&text, &format(&path)) {
            text = formatted;
        }
    }
    let expansion = live
        .map(|live| live.finish(&text.replace("\r\n", "\n").replace('\r', "\n")))
        .transpose()?;
    if let Some(expansion) = &expansion {
        text = expansion.text.clone();
    }
    let encoded = encode(&path, &text)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
    if let Err(error) = file.write_all(&encoded).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(format!("Cannot create {}: {error}", path.display()));
    }
    Ok(CreatedFile { path, expansion })
}

impl IdeApp {
    fn template_set(&self) -> TemplateSet {
        project_new_ui::generation_templates(
            &Settings::config_dir(),
            self.project.as_ref().map(|p| p.root.as_path()),
        )
    }

    pub(super) fn invalidate_file_template_page(&mut self) {
        self.settings_ui.file_templates.cache = None;
    }

    /// New › <template> (or New › File for an empty `template`) in `dir`.
    pub(super) fn open_new_file_dialog(&mut self, dir: PathBuf, template: String) {
        let set = self.template_set();
        let name = proposed_name(&set, &template);
        self.dialog = Dialog::NewFromTemplate(Box::new(NewFromTemplateState {
            dir,
            template,
            proposed: name.clone(),
            name,
            templates: set.new_menu(),
            error: None,
            focused: false,
        }));
    }

    /// File › New File… (Ctrl+Alt+Insert): in the Project view's selected
    /// folder (a selected file's folder, else the project root). The typed
    /// name is created as it is, empty ("File (empty)"), unless a template is
    /// picked in the dialog.
    pub(super) fn new_file_from_action(&mut self) {
        let dir = match &self.project {
            Some(p) => match &p.tree.selected {
                Some(s) if s.is_dir() => s.clone(),
                Some(s) => s
                    .parent()
                    .filter(|d| d.starts_with(&p.root))
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| p.root.clone()),
                None => p.root.clone(),
            },
            None => PathBuf::from("."),
        };
        self.open_new_file_dialog(dir, String::new());
    }

    /// Template variables for a new file at `path`.
    fn new_file_vars(&self, path: &Path) -> Result<BTreeMap<String, String>, String> {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".into());
        let mut vars = profile_file_vars(
            path,
            self.project.as_ref().map(|p| p.root.as_path()),
            &self.settings.copyright,
            &self.settings.copyright_profiles,
            &user,
            chrono::Local::now().naive_local(),
        )?;
        // Render the template first, then place the notice around its actual
        // shebang/cookie/declaration. Legacy templates retain their old include.
        if !self.settings.copyright_profiles.is_unconfigured() {
            vars.insert("COPYRIGHT".into(), String::new());
        }
        Ok(vars)
    }

    fn new_file_copyright(&self, path: &Path, text: &str) -> Result<String, String> {
        if !self.settings.copyright.enabled || !self.settings.copyright.add_to_new_files {
            return Ok(text.to_string());
        }
        let profiles = &self.settings.copyright_profiles;
        profiles.validate()?;
        if profiles.is_unconfigured() || !rustpy_settings::copyright::supports_comments(path) {
            return Ok(text.to_string());
        }
        let Some(profile) =
            profiles.profile_for(path, self.project.as_ref().map(|p| p.root.as_path()))?
        else {
            return Ok(text.to_string());
        };
        let now = chrono::Local::now();
        let year = now.format("%Y").to_string();
        let date = now.format("%Y-%m-%d").to_string();
        let project = self
            .project
            .as_ref()
            .and_then(|p| p.root.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".into());
        match rustpy_settings::copyright::plan_update(
            text,
            path,
            profile,
            &rustpy_settings::copyright::Variables {
                year: &year,
                date: &date,
                project: &project,
                user: &user,
                file: &file,
            },
        )? {
            Some(edit) => Ok(format!(
                "{}{}{}",
                &text[..edit.start],
                edit.replacement,
                &text[edit.end..]
            )),
            None => Ok(text.to_string()),
        }
    }

    /// The code style's formatter options (Reformat according to style).
    fn template_format_options(&self) -> rustpy_format::FormatOptions {
        let cs = &self.settings.code_style;
        let mut opts = rustpy_format::FormatOptions::pep8();
        opts.max_line_length = self.settings.editor.right_margin.max(40);
        opts.indent = self.settings.editor.indent_size.max(1);
        opts.quote = match cs.quote_style.as_str() {
            "double" => rustpy_format::QuoteStyle::Double,
            "single" => rustpy_format::QuoteStyle::Single,
            _ => rustpy_format::QuoteStyle::Preserve,
        };
        opts.magic_trailing_comma = cs.magic_trailing_comma;
        opts.blank_lines_top_level = cs.blank_lines_top_level;
        opts.blank_lines_in_class = cs.blank_lines_in_class;
        opts
    }

    /// Creates `typed` in `dir` from `template` (empty: an empty file).
    fn create_from_template(
        &self,
        dir: &Path,
        template: &str,
        typed: &str,
    ) -> Result<CreatedFile, String> {
        create_prepared_file(
            FileTemplateRequest {
                set: &self.template_set(),
                dir,
                template,
                typed,
            },
            |p| self.new_file_vars(p),
            |p, text| self.new_file_copyright(p, text),
            |path| {
                let mut options = self.template_format_options();
                let mut edits =
                    crate::editor::state::EditOptions::from_settings(&self.settings.editor);
                crate::editor::indent_options::apply_file_indent(
                    &mut edits,
                    false,
                    None,
                    &self.settings.editor.editorconfig_for(path),
                    None,
                );
                options.indent = edits.indent_size.max(1);
                options.max_line_length = edits.right_margin.max(40);
                options
            },
            |path, text| {
                let prefs = &self.settings.editor.preferences;
                let codec = prefs.encoding(path, self.project.as_ref().map(|p| p.root.as_path()));
                let encoding = match codec {
                    Some(codec) => rustpy_text::Encoding::from_key(codec)
                        .ok_or_else(|| format!("Unknown encoding {codec}"))?,
                    None => {
                        if prefs.new_utf8_bom {
                            rustpy_text::Encoding::Utf8Bom
                        } else {
                            rustpy_text::Encoding::Utf8
                        }
                    }
                };
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                let ending = match prefs.new_line_separator.as_str() {
                    "CRLF" => normalized.replace('\n', "\r\n"),
                    "CR" => normalized.replace('\n', "\r"),
                    "LF" => normalized,
                    _ => text.to_owned(),
                };
                encoding
                    .encode_with_policy(&ending, rustpy_text::SourcePolicy::for_path(path))
                    .map_err(|e| e.to_string())
            },
        )
    }

    fn start_file_template_fields(&mut self, created: &CreatedFile) {
        let Some(expansion) = &created.expansion else {
            return;
        };
        let Some(index) = self
            .active
            .filter(|&i| self.tabs[i].state.path.as_deref() == Some(created.path.as_path()))
        else {
            return;
        };
        let tab = &mut self.tabs[index];
        if tab.state.doc.text() != expansion.text || tab.state.doc.is_read_only() {
            return;
        }
        let mut vars: Vec<(String, Vec<TextRange>)> = Vec::new();
        for (start, end, name) in &expansion.tab_stops {
            let range = TextRange::new(TextSize::new(*start as u32), TextSize::new(*end as u32));
            if let Some((_, ranges)) = vars.iter_mut().find(|(n, _)| n == name) {
                ranges.push(range)
            } else {
                vars.push((name.clone(), vec![range]))
            }
        }
        let end = TextSize::new(expansion.end_offset.unwrap_or(expansion.text.len()) as u32);
        let caret = vars
            .first()
            .and_then(|(_, ranges)| ranges.first())
            .map_or(crate::editor::Caret::at(end), |r| {
                crate::editor::Caret::new(r.start(), r.end())
            });
        tab.state.set_carets(vec![caret]);
        self.template_session = (!vars.is_empty()).then_some(TemplateSession {
            tab_id: tab.view.id,
            vars,
            idx: 0,
            end,
            version: tab.state.doc.version(),
        });
    }

    pub(super) fn new_from_template_dialog(
        &mut self,
        ctx: &egui::Context,
        mut st: Box<NewFromTemplateState>,
    ) {
        let mut open = true;
        let (mut create, mut cancel) = (false, false);
        let theme = self.theme.clone();
        let title = if st.template.is_empty() {
            "New File".to_string()
        } else {
            format!("New {}", st.template)
        };
        let mut picked: Option<String> = None;
        egui::Window::new(title)
            .id(egui::Id::new("new_from_template"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label(
                    egui::RichText::new(format!("in {}", st.dir.display()))
                        .color(theme.text_dim)
                        .small(),
                );
                if !st.templates.is_empty() {
                    ui.horizontal(|ui| {
                        let template_label = ui.label("Template");
                        let shown = if st.template.is_empty() {
                            "File (empty)".to_string()
                        } else {
                            st.template.clone()
                        };
                        egui::ComboBox::from_id_salt("new_file_template")
                            .selected_text(shown)
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for name in std::iter::once(String::new())
                                    .chain(st.templates.iter().cloned())
                                {
                                    let label = if name.is_empty() {
                                        "File (empty)".to_string()
                                    } else {
                                        name.clone()
                                    };
                                    if ui.selectable_label(st.template == name, label).clicked() {
                                        picked = Some(name);
                                    }
                                }
                            })
                            .combo_labelled(&template_label);
                    });
                }
                let r = ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(320.0));
                if !st.focused {
                    r.request_focus();
                    crate::theme::select_all_text(ui, &r, &st.name);
                    st.focused = true;
                }
                if r.changed() {
                    st.error = None;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    create = true;
                }
                if let Some(e) = &st.error {
                    ui.colored_label(theme.error, e);
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        create = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
        if let Some(name) = picked.filter(|n| *n != st.template) {
            let proposed = proposed_name(&self.template_set(), &name);
            // A name the user typed stays; the proposed one follows the template.
            if st.name == st.proposed {
                st.name = proposed.clone();
            }
            st.proposed = proposed;
            st.template = name;
            st.error = None;
        }
        if create {
            match self.create_from_template(&st.dir, &st.template, &st.name) {
                Ok(created) => {
                    let path = &created.path;
                    if let Some(p) = &mut self.project {
                        p.tree.invalidate_all();
                        p.tree.expanded.insert(st.dir.clone());
                    }
                    self.open_file(path);
                    self.vcs_file_created(path);
                    self.start_file_template_fields(&created);
                    self.focus_editor = true;
                }
                Err(e) => {
                    st.error = Some(e);
                    self.dialog = Dialog::NewFromTemplate(st);
                }
            }
        } else if open && !cancel {
            self.dialog = Dialog::NewFromTemplate(st);
        } else {
            self.focus_editor = true;
        }
    }

    /// Tools › Save File as Template: the active file becomes an IDE-level template.
    pub(super) fn save_file_as_template(&mut self) {
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            self.set_status("Open a file to save it as a template");
            return;
        };
        let path = tab
            .state
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("untitled.py"));
        let text = tab.state.doc.text();
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let set = self.template_set();
        let mut name = stem.clone();
        let mut n = 2;
        while set.template(&name).is_some() {
            name = format!("{stem} {n}");
            n += 1;
        }
        let store_path = ft::ide_store_path(&Settings::config_dir());
        let mut store = ft::load_store(&store_path);
        store.upsert(
            false,
            FileTemplate {
                name: name.clone(),
                extension,
                file_name: stem,
                body: ft::escape_literal(&text),
                description: format!("Saved from {}", path.display()),
                ..Default::default()
            },
        );
        if let Err(e) = ft::save_store(&store_path, &store) {
            self.set_status(format!("Cannot save the template: {e}"));
            return;
        }
        self.settings_ui.file_templates.scope = Scope::Ide;
        self.settings_ui.file_templates.includes = false;
        self.settings_ui.file_templates.selected = Some(name.clone());
        self.settings_ui.file_templates.draft = None;
        self.settings_ui.page = settings_ui::SettingsPage::FileTemplates;
        self.dialog = Dialog::Settings;
        self.set_status(format!("Saved as file template \"{name}\""));
    }

    /// Settings › Editor › File and Code Templates.
    pub(super) fn page_file_templates(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let root = self.project.as_ref().map(|p| p.root.clone());
        let page = &mut self.settings_ui.file_templates;
        if root.is_none() {
            page.scope = Scope::Ide;
        }
        // A draft from another scheme or project (closed, switched) is not saved here.
        let here = (page.scope, root.clone());
        if page.draft.is_none() {
            page.draft_for = None;
        } else if page.draft_for.as_ref().is_some_and(|f| *f != here) {
            page.draft = None;
            page.draft_for = None;
            page.message = None;
        }
        ui.horizontal(|ui| {
            ui.label("Scheme:");
            let scope = page.scope;
            ui.selectable_value(&mut page.scope, Scope::Ide, "Default").on_hover_text("Templates for every project (stored with the IDE settings)");
            ui.add_enabled_ui(root.is_some(), |ui| {
                ui.selectable_value(&mut page.scope, Scope::Project, "Project").on_hover_text("Templates of this project (.rustpy/fileTemplates.json); they win over the default ones");
            });
            if scope != page.scope {
                // The draft came from the other scheme: never save it into this one.
                page.draft = None;
                page.message = None;
            }
            ui.separator();
            let before = page.includes;
            ui.selectable_value(&mut page.includes, false, "Files");
            ui.selectable_value(&mut page.includes, true, "Includes");
            if before != page.includes {
                page.selected = None;
                page.draft = None;
            }
        });
        let store_path = match (page.scope, &root) {
            (Scope::Project, Some(r)) => ft::project_store_path(r),
            _ => ft::ide_store_path(&Settings::config_dir()),
        };
        ui.label(
            egui::RichText::new(format!("Saved to {}", store_path.display()))
                .color(theme.text_dim)
                .small(),
        );
        ui.separator();
        let includes = page.includes;
        let Schemes {
            shown: set,
            default,
            effective,
        } = page.schemes(root.as_deref());
        let list: Vec<(FileTemplate, Origin)> = if includes {
            set.includes.clone()
        } else {
            set.templates.clone()
        };
        // In the Default scheme: the templates this project replaces with its own.
        let project_owned: Vec<String> = match page.scope {
            Scope::Ide => if includes {
                &effective.includes
            } else {
                &effective.templates
            }
            .iter()
            .filter(|(_, o)| *o == Origin::Project)
            .map(|(t, _)| t.name.clone())
            .collect(),
            Scope::Project => Vec::new(),
        };
        let mut save: Option<FileTemplate> = None;
        let mut remove: Option<String> = None;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(230.0);
                ui.horizontal(|ui| {
                    if ui.button("+").on_hover_text(if includes { "New include" } else { "New template" }).clicked() {
                        let t = if includes { FileTemplate::new("New Include", "py", "", "", "") } else { FileTemplate::new("New Template", "py", "untitled", "#parse(\"File Header.py\")\n", "") };
                        page.selected = None;
                        page.draft = Some((t, None));
                    }
                    let selected = page.selected.clone().and_then(|s| list.iter().find(|(t, _)| t.name == s).cloned());
                    if ui.add_enabled(selected.is_some(), egui::Button::new("Copy")).clicked() {
                        if let Some((mut t, _)) = selected.clone() {
                            t.name = format!("{} copy", t.name);
                            page.selected = None;
                            page.draft = Some((t, None));
                        }
                    }
                    // Only what this scheme stores can be removed: a builtin (or, for a
                    // project template, the Default scheme's version) comes back.
                    let what = selected.as_ref().filter(|(t, _)| !t.name.is_empty()).map_or(Removal::None, |(t, origin)| removal(page.scope, &t.name, *origin, &default, includes));
                    let (label, hint) = match (what, page.scope) {
                        (Removal::Reset, Scope::Ide) => ("Reset", "Back to the builtin template"),
                        (Removal::Reset, Scope::Project) => ("Reset", "Back to the Default scheme's template"),
                        (Removal::None, _) if selected.as_ref().is_some_and(|(_, o)| *o != Origin::Custom && *o != Origin::Project) => ("Reset", "Not changed in this scheme"),
                        _ => ("Delete", "Delete this template"),
                    };
                    if ui.add_enabled(what != Removal::None, egui::Button::new(label)).on_hover_text(hint).clicked() {
                        remove = selected.map(|(t, _)| t.name);
                    }
                });
                egui::ScrollArea::vertical().id_salt("file_templates_list").max_height(420.0).show(ui, |ui| {
                    for (t, origin) in &list {
                        let selected = page.selected.as_deref() == Some(t.name.as_str());
                        let mut text = egui::RichText::new(&t.name);
                        // Colors mark changed and project templates (not on the selection, where they would be hard to read).
                        text = match origin {
                            Origin::Builtin => text,
                            Origin::Modified if !selected => text.color(theme.accent),
                            Origin::Custom => text.italics(),
                            Origin::Project if !selected => text.color(theme.success),
                            Origin::Modified | Origin::Project => text,
                        };
                        if !includes && !t.enabled && !selected {
                            text = text.color(theme.text_disabled);
                        }
                        let hint = match origin {
                            Origin::Builtin => "Builtin",
                            Origin::Modified => "Builtin, changed",
                            Origin::Custom => "Your template",
                            Origin::Project => "Project template",
                        };
                        if ui.selectable_label(selected, text).on_hover_text(hint).clicked() {
                            page.selected = Some(t.name.clone());
                            page.draft = Some((t.clone(), Some(t.name.clone())));
                        }
                    }
                });
            });
            ui.separator();
            ui.vertical(|ui| {
                if page.draft.is_none() {
                    if let Some((t, _)) = page.selected.as_ref().and_then(|s| list.iter().find(|(t, _)| &t.name == s)) {
                        page.draft = Some((t.clone(), Some(t.name.clone())));
                    }
                }
                let Some((draft, original)) = page.draft.as_mut() else {
                    ui.label(egui::RichText::new("Select a template, or + to add one.").color(theme.text_dim));
                    return;
                };
                egui::Grid::new("file_template_edit").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(260.0));
                    ui.end_row();
                    ui.label("Extension");
                    ui.add(egui::TextEdit::singleline(&mut draft.extension).desired_width(80.0).hint_text("none"));
                    ui.end_row();
                    if !includes {
                        ui.label("File name");
                        ui.add(egui::TextEdit::singleline(&mut draft.file_name).desired_width(200.0).hint_text("proposed in the New dialog"));
                        ui.end_row();
                    }
                    ui.label("Description");
                    ui.add(egui::TextEdit::singleline(&mut draft.description).desired_width(360.0));
                    ui.end_row();
                });
                if !includes {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut draft.enabled, "Show in the New menu");
                        ui.checkbox(&mut draft.reformat, "Reformat according to style");
                        ui.checkbox(&mut draft.live_templates, "Enable live templates").on_hover_text("$FIELD$ creates a linked editable field; $END$ marks the final caret. ${NAME} and other file variables still expand before creation.");
                    });
                }
                ui.add(egui::TextEdit::multiline(&mut draft.body).font(egui::TextStyle::Monospace).code_editor().desired_rows(14).desired_width(f32::INFINITY));
                if original.as_ref().is_some_and(|o| project_owned.contains(o)) {
                    ui.label(egui::RichText::new("This project has its own version (Project scheme), which it uses instead of this one.").color(theme.text_dim).small());
                }
                let stored = original.as_ref().and_then(|o| list.iter().find(|(t, _)| t.name == *o)).map(|(t, _)| t.clone());
                let dirty = stored.as_ref() != Some(&*draft);
                let mut discard = false;
                ui.horizontal(|ui| {
                    if ui.add_enabled(dirty && !draft.name.trim().is_empty(), egui::Button::new("Save")).clicked() {
                        save = Some(draft.clone());
                    }
                    if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                        match &stored {
                            Some(t) => *draft = t.clone(),
                            // A new template that was never saved: drop it.
                            None => discard = true,
                        }
                    }
                    if dirty {
                        ui.label(egui::RichText::new("not saved").color(theme.warning).small());
                    }
                });
                if discard {
                    page.draft = None;
                    return;
                }
                ui.collapsing("Variables", |ui| {
                    egui::Grid::new("file_template_vars").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                        for (name, what) in ft::VARIABLES {
                            ui.label(egui::RichText::new(format!("${{{name}}}")).monospace());
                            ui.label(egui::RichText::new(*what).color(theme.text_dim));
                            ui.end_row();
                        }
                    });
                    ui.label(egui::RichText::new("#parse(\"File Header.py\") inserts an include; #if (${VAR}) … #else … #end, #set ($VAR = \"…\"), ## comment. ${DS} or \\$ writes a $.").color(theme.text_dim).small());
                });
            });
        });
        if let Some(m) = &page.message {
            ui.label(egui::RichText::new(m).color(theme.text_dim));
        }
        // Preview with this project's values (outside the borrow of the page state).
        let preview = self
            .settings_ui
            .file_templates
            .draft
            .as_ref()
            .map(|(t, _)| t.clone());
        if let Some(t) = preview {
            ui.collapsing("Preview", |ui| {
                let file_name = if includes {
                    "example.py".to_string()
                } else {
                    t.default_file_name()
                };
                let path = root.clone().unwrap_or_default().join(&file_name);
                let vars = self.new_file_vars(&path);
                let mut with_draft = set.clone();
                let slot = if includes {
                    &mut with_draft.includes
                } else {
                    &mut with_draft.templates
                };
                match slot.iter_mut().find(|(x, _)| x.name == t.name) {
                    Some(x) => x.0 = t.clone(),
                    None => slot.push((t.clone(), Origin::Custom)),
                }
                ui.label(
                    egui::RichText::new(format!("{file_name}:"))
                        .color(theme.text_dim)
                        .small(),
                );
                match vars
                    .and_then(|vars| {
                        if t.live_templates {
                            let live = rustpy_templates::file_live::PreparedLive::render(
                                &with_draft,
                                &t,
                                &vars,
                            )
                            .map_err(|e| e.to_string())?;
                            let text = self.new_file_copyright(&path, &live.text)?;
                            live.finish(&text).map(|e| e.text)
                        } else {
                            with_draft.try_render(&t, &vars).map_err(|e| e.to_string())
                        }
                    })
                    .and_then(|text| {
                        if t.live_templates {
                            Ok(text)
                        } else {
                            self.new_file_copyright(&path, &text)
                        }
                    }) {
                    Ok(text) => {
                        let mut shown: String = text.chars().take(PREVIEW_CHARS).collect();
                        if shown.len() < text.len() {
                            shown.push_str("\n…");
                        }
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(if text.is_empty() {
                                    "(empty)".to_string()
                                } else {
                                    shown
                                })
                                .monospace(),
                            )
                            .wrap(),
                        );
                    }
                    Err(e) => {
                        ui.colored_label(theme.error, format!("Cannot render: {e}"));
                    }
                }
            });
        }
        let page = &mut self.settings_ui.file_templates;
        if let Some(name) = remove {
            let what = list
                .iter()
                .find(|(t, _)| t.name == name)
                .map_or(Removal::None, |(_, origin)| {
                    removal(page.scope, &name, *origin, &default, includes)
                });
            let mut store: TemplateStore = ft::load_store(&store_path);
            store.remove(includes, &name);
            page.message = Some(match ft::save_store(&store_path, &store) {
                Ok(()) => match (what, page.scope) {
                    (Removal::Reset, Scope::Ide) => format!("{name}: back to the builtin"),
                    (Removal::Reset, Scope::Project) => {
                        format!("{name}: back to the Default scheme's template")
                    }
                    _ => format!("{name}: deleted"),
                },
                Err(e) => format!("Cannot save {}: {e}", store_path.display()),
            });
            page.draft = None;
            if what != Removal::Reset {
                page.selected = None;
            }
        }
        if let Some(t) = save {
            let original = page.draft.as_ref().and_then(|(_, o)| o.clone());
            match save_template(&store_path, includes, original.as_deref(), t) {
                Ok(name) => {
                    page.message = Some(format!("Saved \"{name}\" to {}", store_path.display()));
                    page.selected = Some(name);
                    page.draft = None;
                }
                Err(e) => page.message = Some(format!("Cannot save {}: {e}", store_path.display())),
            }
        }
        if page.draft.is_some() && page.draft_for.is_none() {
            page.draft_for = Some((page.scope, root));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 10)
            .unwrap()
            .and_hms_opt(9, 30, 0)
            .unwrap()
    }

    /// A fresh directory, removed again when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "rustpy-file-templates-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn new_files_get_the_project_variables_and_the_copyright_header() {
        let dir = TempDir::new("vars");
        let root = dir.path().join("shop");
        let pkg = root.join("orders").join("api");
        std::fs::create_dir_all(&pkg).unwrap();
        let mut copyright = rustpy_settings::CopyrightSettings {
            enabled: true,
            text: "(c) $today.year $user, $project".into(),
            add_to_new_files: false,
        };
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let opts = rustpy_format::FormatOptions::pep8();
        // Without "Add to new files" there is no header; the extension is added.
        let path = create_file(
            &set,
            &pkg,
            "Python Unit Test",
            "test_refunds",
            |p| file_vars(p, Some(&root), &copyright, "jane", now()),
            &opts,
        )
        .unwrap();
        assert_eq!(path, pkg.join("test_refunds.py"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("import unittest\n\n\nclass TestRefunds(unittest.TestCase):"),
            "{text}"
        );
        // An existing file is not overwritten.
        let again = create_file(
            &set,
            &pkg,
            "Python Unit Test",
            "test_refunds.py",
            |p| file_vars(p, Some(&root), &copyright, "jane", now()),
            &opts,
        );
        assert_eq!(again, Err("test_refunds.py already exists".to_string()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        // With it, the File Header include writes the notice as a comment of the file's language.
        copyright.add_to_new_files = true;
        let vars = |p: &Path| file_vars(p, Some(&root), &copyright, "jane", now());
        let py = std::fs::read_to_string(
            create_file(&set, &pkg, "Python File", "views.py", vars, &opts).unwrap(),
        )
        .unwrap();
        assert!(
            py.starts_with("# (c) 2026 jane, shop\n\n\"\"\"views.\n\nCreated on 2026-10-10."),
            "{py}"
        );
        let docker = std::fs::read_to_string(
            create_file(&set, &root, "Dockerfile", "Dockerfile", vars, &opts).unwrap(),
        )
        .unwrap();
        assert!(
            docker.starts_with("# (c) 2026 jane, shop\n\nFROM python:"),
            "{docker}"
        );
        // JSON has no comments: no header.
        let json = std::fs::read_to_string(
            create_file(&set, &root, "JSON File", "data", vars, &opts).unwrap(),
        )
        .unwrap();
        assert_eq!(json, "{\n}\n");
        let v = file_vars(
            &pkg.join("models.py"),
            Some(&root),
            &copyright,
            "jane",
            now(),
        );
        assert_eq!(
            (
                v["PACKAGE_NAME"].as_str(),
                v["PROJECT_NAME"].as_str(),
                v["DIR_PATH"].as_str()
            ),
            ("orders.api", "shop", "orders/api")
        );
        // An empty template name creates an empty file (New › File); subdirectories are created.
        let empty = create_file(&set, &root, "", "notes/todo.txt", vars, &opts).unwrap();
        assert_eq!(std::fs::read_to_string(empty).unwrap(), "");
        assert!(create_file(&set, &root, "", "  ", vars, &opts).is_err());
        assert!(create_file(&set, &root, "No Such Template", "x", vars, &opts).is_err());
    }

    #[test]
    fn reformat_according_to_style_runs_the_formatter_on_python_templates() {
        let dir = TempDir::new("reformat");
        let mut store = TemplateStore::default();
        let mut t = FileTemplate::new("Messy", "py", "messy", "x=[1,2 ,3]\n", "");
        store.upsert(false, t.clone());
        t.name = "Messy Formatted".into();
        t.reformat = true;
        store.upsert(false, t);
        let set = TemplateSet::merged(&store, None);
        let opts = rustpy_format::FormatOptions::pep8();
        let copyright = rustpy_settings::CopyrightSettings::default();
        let vars = |p: &Path| file_vars(p, Some(dir.path()), &copyright, "jane", now());
        let raw = std::fs::read_to_string(
            create_file(&set, dir.path(), "Messy", "a", vars, &opts).unwrap(),
        )
        .unwrap();
        assert_eq!(raw, "x=[1,2 ,3]\n");
        let formatted = std::fs::read_to_string(
            create_file(&set, dir.path(), "Messy Formatted", "b", vars, &opts).unwrap(),
        )
        .unwrap();
        assert_eq!(formatted, "x = [1, 2, 3]\n");
    }

    fn store_with(name: &str, body: &str) -> TemplateStore {
        let mut store = TemplateStore::default();
        store.upsert(false, FileTemplate::new(name, "py", "script", body, ""));
        store
    }

    #[test]
    fn the_default_scheme_edits_the_ide_template_not_the_projects() {
        let dir = TempDir::new("schemes");
        let ide_path = ft::ide_store_path(&dir.path().join("config"));
        let project_path = ft::project_store_path(&dir.path().join("proj"));
        ft::save_store(
            &ide_path,
            &store_with("Python File", "# MY IDE-LEVEL BODY\n"),
        )
        .unwrap();
        ft::save_store(
            &project_path,
            &store_with("Python File", "# PROJECT-ONLY BODY (company X)\n"),
        )
        .unwrap();
        let (ide, project) = (ft::load_store(&ide_path), ft::load_store(&project_path));
        let default = scheme_set(Scope::Ide, &ide, None);
        // The Default scheme lists the user's own version, which Reset can remove.
        let shown = scheme_set(Scope::Ide, &ide, Some(&project));
        let (t, origin) = shown
            .templates
            .iter()
            .find(|(t, _)| t.name == "Python File")
            .cloned()
            .unwrap();
        assert_eq!(
            (t.body.as_str(), origin),
            ("# MY IDE-LEVEL BODY\n", Origin::Modified)
        );
        assert_eq!(
            removal(Scope::Ide, &t.name, origin, &default, false),
            Removal::Reset
        );
        assert!(
            shown.templates.iter().all(|(_, o)| *o != Origin::Project),
            "no project template in the Default scheme"
        );
        // Editing it there and saving keeps the user's body and leaves the project's alone.
        let mut draft = t.clone();
        draft.body = "# MY IDE-LEVEL BODY!\n".into();
        assert_eq!(
            save_template(&ide_path, false, Some("Python File"), draft).unwrap(),
            "Python File"
        );
        assert_eq!(
            ft::load_store(&ide_path),
            store_with("Python File", "# MY IDE-LEVEL BODY!\n")
        );
        assert_eq!(ft::load_store(&project_path), project);
        // The Project scheme shows (and saves to) the project's version; its Reset
        // goes back to the Default scheme's template.
        let ide = ft::load_store(&ide_path);
        let shown = scheme_set(Scope::Project, &ide, Some(&project));
        let (t, origin) = shown
            .templates
            .iter()
            .find(|(t, _)| t.name == "Python File")
            .cloned()
            .unwrap();
        assert_eq!(
            (t.body.as_str(), origin),
            ("# PROJECT-ONLY BODY (company X)\n", Origin::Project)
        );
        assert_eq!(
            removal(
                Scope::Project,
                &t.name,
                origin,
                &scheme_set(Scope::Ide, &ide, None),
                false
            ),
            Removal::Reset
        );
        let mut project_only = project.clone();
        project_only.upsert(
            false,
            FileTemplate::new("Fixture", "json", "fixture", "[]\n", ""),
        );
        let shown = scheme_set(Scope::Project, &ide, Some(&project_only));
        let (f, origin) = shown
            .templates
            .iter()
            .find(|(t, _)| t.name == "Fixture")
            .cloned()
            .unwrap();
        assert_eq!(
            removal(Scope::Project, &f.name, origin, &default, false),
            Removal::Delete
        );
        // Templates a scheme does not store cannot be removed there.
        let pytest = shown
            .templates
            .iter()
            .find(|(t, _)| t.name == "Python pytest")
            .unwrap();
        assert_eq!(
            removal(Scope::Project, &pytest.0.name, pytest.1, &default, false),
            Removal::None
        );
        assert_eq!(
            removal(Scope::Ide, &pytest.0.name, pytest.1, &default, false),
            Removal::None
        );
    }

    #[test]
    fn file_new_file_creates_the_typed_name_and_templates_keep_their_extension() {
        let dir = TempDir::new("names");
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let opts = rustpy_format::FormatOptions::pep8();
        let copyright = rustpy_settings::CopyrightSettings::default();
        let vars = |p: &Path| file_vars(p, Some(dir.path()), &copyright, "jane", now());
        // File › New File… starts on "File (empty)": the name as typed, empty.
        for name in ["notes.txt", "Makefile"] {
            let path = create_file(&set, dir.path(), "", name, vars, &opts).unwrap();
            assert_eq!(
                (path.clone(), std::fs::read_to_string(&path).unwrap()),
                (dir.path().join(name), String::new())
            );
        }
        // A template does not write its text into a file of another type.
        let err =
            create_file(&set, dir.path(), "Python File", "todo.txt", vars, &opts).unwrap_err();
        assert!(err.starts_with("todo.txt is not a .py file"), "{err}");
        assert!(!dir.path().join("todo.txt").exists());
        assert!(create_file(&set, dir.path(), "Markdown File", "notes.rst", vars, &opts).is_err());
        // A bare name gets the extension, and .pyw is a Python script too.
        assert_eq!(
            create_file(&set, dir.path(), "Python File", "build", vars, &opts).unwrap(),
            dir.path().join("build.py")
        );
        assert!(std::fs::read_to_string(
            create_file(&set, dir.path(), "Python File", "gui.pyw", vars, &opts).unwrap()
        )
        .unwrap()
        .contains("def main()"));
        // Templates without an extension take any name.
        assert!(create_file(
            &set,
            dir.path(),
            "Dockerfile",
            "Dockerfile.dev",
            vars,
            &opts
        )
        .is_ok());
    }

    #[test]
    fn a_runaway_template_is_refused_and_creates_no_file() {
        let dir = TempDir::new("runaway");
        let mut store = TemplateStore::default();
        store.upsert(
            false,
            FileTemplate::new("Bomb", "py", "bomb", "#parse(\"A\")\n", ""),
        );
        store.upsert(
            true,
            FileTemplate::new("A", "", "", &format!("{}x", "#parse(\"A\")".repeat(10)), ""),
        );
        let set = TemplateSet::merged(&TemplateStore::default(), Some(&store));
        let opts = rustpy_format::FormatOptions::pep8();
        let copyright = rustpy_settings::CopyrightSettings::default();
        let start = std::time::Instant::now();
        let err = create_file(
            &set,
            dir.path(),
            "Bomb",
            "boom",
            |p| file_vars(p, Some(dir.path()), &copyright, "jane", now()),
            &opts,
        )
        .unwrap_err();
        assert!(err.contains("too much work"), "{err}");
        assert!(!dir.path().join("boom.py").exists());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn saving_under_another_templates_name_is_refused() {
        // Review: Save silently replaced a custom template with the same name.
        let dir = std::env::temp_dir().join(format!("rustpy-ft-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fileTemplates.json");
        let t = |name: &str, body: &str| FileTemplate::new(name, "py", "x", body, "");
        assert_eq!(
            save_template(&path, false, None, t("Mine", "# mine\n")).unwrap(),
            "Mine"
        );
        assert!(
            save_template(&path, false, None, t("Mine", "# other\n")).is_err(),
            "a new template does not replace it"
        );
        save_template(&path, false, None, t("Other", "# o\n")).unwrap();
        assert!(
            save_template(&path, false, Some("Other"), t("Mine", "# o\n")).is_err(),
            "nor does renaming another one onto it"
        );
        save_template(&path, false, Some("Mine"), t("Mine", "# updated\n")).unwrap();
        let store = ft::load_store(&path);
        assert_eq!(
            store
                .templates
                .iter()
                .find(|x| x.name == "Mine")
                .map(|x| x.body.as_str()),
            Some("# updated\n")
        );
        assert_eq!(store.templates.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod copyright_profile_tests {
    use super::*;
    use rustpy_settings::copyright::{
        CommentStyle, CopyrightProfile, CopyrightProfilesSettings, CopyrightScope,
    };
    #[test]
    fn checked_creation_selects_scoped_profile_and_exclusion() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = rustpy_settings::CopyrightSettings {
            enabled: true,
            add_to_new_files: true,
            ..Default::default()
        };
        let profiles = CopyrightProfilesSettings {
            profiles: vec![CopyrightProfile {
                text: "Copyright $project $file".into(),
                style: CommentStyle::Block,
                ..Default::default()
            }],
            default_profile: Some("Default".into()),
            scopes: vec![CopyrightScope {
                include: "generated/**".into(),
                profile: String::new(),
                ..Default::default()
            }],
        };
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 12)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let created = create_file_checked(
            &set,
            dir.path(),
            "Python File",
            "a",
            |path| profile_file_vars(path, Some(dir.path()), &legacy, &profiles, "author", now),
            &rustpy_format::FormatOptions::pep8(),
        )
        .unwrap();
        let text = std::fs::read_to_string(created).unwrap();
        assert!(text.contains("# ----------------"));
        assert!(text.contains("a.py"));
        let vars = profile_file_vars(
            &dir.path().join("generated/a.py"),
            Some(dir.path()),
            &legacy,
            &profiles,
            "author",
            now,
        )
        .unwrap();
        assert_eq!(vars["COPYRIGHT"], "");
        let json = profile_file_vars(
            &dir.path().join("data.json"),
            Some(dir.path()),
            &legacy,
            &profiles,
            "author",
            now,
        )
        .unwrap();
        assert_eq!(json["COPYRIGHT"], "");
    }
    #[test]
    fn invalid_profile_fails_before_file_or_parent_publication() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested");
        let legacy = rustpy_settings::CopyrightSettings {
            enabled: true,
            add_to_new_files: true,
            ..Default::default()
        };
        let profiles = CopyrightProfilesSettings {
            profiles: vec![CopyrightProfile {
                text: "Copyright $unknown".into(),
                ..Default::default()
            }],
            default_profile: Some("Default".into()),
            scopes: vec![],
        };
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 12)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let result = create_file_checked(
            &set,
            &target,
            "Python File",
            "a",
            |path| profile_file_vars(path, Some(dir.path()), &legacy, &profiles, "author", now),
            &rustpy_format::FormatOptions::pep8(),
        );
        assert!(result.is_err());
        assert!(!target.exists());
    }
}

#[cfg(test)]
mod copyright_post_render_tests {
    use super::*;
    use rustpy_settings::copyright::{CopyrightProfile, CopyrightProfilesSettings};
    fn app() -> IdeApp {
        let mut settings = Settings::default();
        settings.copyright.enabled = true;
        settings.copyright.add_to_new_files = true;
        settings.copyright_profiles = CopyrightProfilesSettings {
            profiles: vec![CopyrightProfile::default()],
            default_profile: Some("Default".into()),
            scopes: vec![],
        };
        IdeApp::new_welcome(settings)
    }
    #[test]
    fn actual_profile_creation_uses_rendered_shebang_cookie_and_xml_declaration() {
        let dir = tempfile::tempdir().unwrap();
        let app = app();
        let mut set = TemplateSet::merged(&TemplateStore::default(), None);
        set.templates.push((
            FileTemplate::new(
                "Executable",
                "py",
                "run",
                "#parse(\"File Header.py\")\n#!/bin/python\n# coding: utf-8\nprint('ok')\n",
                "",
            ),
            Origin::Custom,
        ));
        set.templates.push((
            FileTemplate::new(
                "XML",
                "xml",
                "data",
                "<?xml version=\"1.0\"?>\n<root/>\n",
                "",
            ),
            Origin::Custom,
        ));
        for (template, typed, prefix) in [
            (
                "Executable",
                "run",
                "#!/bin/python\n# coding: utf-8\n# ---\n# Copyright",
            ),
            ("XML", "data", "<?xml version=\"1.0\"?>\n<!--"),
        ] {
            let path = create_file_checked_with_header(
                &set,
                dir.path(),
                template,
                typed,
                |p| app.new_file_vars(p),
                |p, text| app.new_file_copyright(p, text),
                &rustpy_format::FormatOptions::pep8(),
            )
            .unwrap();
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.starts_with(prefix), "{text}");
            assert_eq!(text.matches("Copyright").count(), 1);
        }
    }
    #[test]
    fn dangling_profile_refuses_template_and_empty_file_before_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app();
        app.settings.copyright_profiles.profiles.clear();
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        for (template, typed) in [("Python File", "a"), ("", "b.py")] {
            let result = create_file_checked_with_header(
                &set,
                dir.path(),
                template,
                typed,
                |p| app.new_file_vars(p),
                |p, text| app.new_file_copyright(p, text),
                &rustpy_format::FormatOptions::pep8(),
            );
            assert!(result.is_err());
            assert!(!dir
                .path()
                .join(if template.is_empty() { "b.py" } else { "a.py" })
                .exists());
        }
    }
}

#[cfg(test)]
#[path = "file_templates_connected_tests.rs"]
mod connected_tests;

#[cfg(test)]
mod editor_encoding_page_fixture {
    use super::*;
    #[test]
    fn new_file_encoding_and_separator_preferences_write_actual_bytes() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.editor.preferences.new_utf8_bom = true;
        settings.editor.preferences.new_line_separator = "CRLF".into();
        let app = IdeApp::new(settings, None);
        let result = app
            .create_from_template(root.path(), "HTML File", "created.html")
            .unwrap();
        let bytes = std::fs::read(result.path).unwrap();
        assert!(bytes.starts_with(&[0xef, 0xbb, 0xbf]));
        assert!(bytes.windows(2).any(|pair| pair == b"\r\n"));
        assert!(!bytes
            .windows(2)
            .any(|pair| pair[1] == b'\n' && pair[0] != b'\r'));
    }
}
