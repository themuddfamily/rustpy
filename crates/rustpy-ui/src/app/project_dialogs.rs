//! Project lifecycle UI: Welcome screen, Open dialog (file browser), New
//! Project wizard (location, interpreter / virtualenv, main.py, git init),
//! Get from VCS (git clone), Close Project.

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;

/// A small egui directory browser (no native dialogs, works under Xvfb).
#[derive(Clone)]
pub(super) struct FileBrowser {
    pub dir: PathBuf,
    pub path_input: String,
    pub entries: Vec<(PathBuf, bool)>,
    pub selected: Option<PathBuf>,
    pub show_hidden: bool,
    dirty: bool,
}

impl FileBrowser {
    pub fn new(dir: PathBuf) -> Self {
        let dir = if dir.is_dir() { dir } else { dirs_home() };
        Self::for_creation(dir)
    }

    /// Preserve an explicitly chosen parent even before it exists.
    pub fn for_creation(dir: PathBuf) -> Self {
        Self {
            path_input: dir.display().to_string(),
            dir,
            entries: Vec::new(),
            selected: None,
            show_hidden: false,
            dirty: true,
        }
    }

    fn refresh(&mut self) {
        self.dirty = false;
        self.entries.clear();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let p = e.path();
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !self.show_hidden && name.starts_with('.') {
                    continue;
                }
                let is_dir = p.is_dir();
                self.entries.push((p, is_dir));
            }
        }
        self.entries.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.file_name().cmp(&b.0.file_name()))
        });
    }

    fn go(&mut self, dir: PathBuf) {
        if dir.is_dir() {
            self.dir = dir;
            self.path_input = self.dir.display().to_string();
            self.selected = None;
            self.dirty = true;
        }
    }

    /// Draws the browser; returns a path when the user double-clicks a file
    /// (or a directory when `pick_dirs`).
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        pick_files: bool,
        height: f32,
    ) -> Option<PathBuf> {
        if self.dirty {
            self.refresh();
        }
        let mut chosen = None;
        ui.horizontal(|ui| {
            if ui.button("⬆").on_hover_text("Parent directory").clicked() {
                if let Some(p) = self.dir.parent().map(Path::to_path_buf) {
                    self.go(p);
                }
            }
            if ui.button("⌂").on_hover_text("Home").clicked() {
                self.go(dirs_home());
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.path_input)
                    .desired_width(ui.available_width() - 120.0),
            );
            if (r.lost_focus() || r.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let p = PathBuf::from(self.path_input.trim());
                if !pick_files && rustpy_settings::project_directory::validate(&p).is_ok() {
                    self.dir = p;
                    self.path_input = self.dir.display().to_string();
                    self.selected = None;
                    self.dirty = true;
                } else if p.is_dir() {
                    self.go(p);
                } else if p.is_file() && pick_files {
                    chosen = Some(p);
                }
            }
            if ui.checkbox(&mut self.show_hidden, "Hidden").changed() {
                self.dirty = true;
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("file_browser")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let entries = self.entries.clone();
                for (p, is_dir) in entries {
                    if !is_dir && !pick_files {
                        continue;
                    }
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let selected = self.selected.as_ref() == Some(&p);
                    let (rect, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
                    if selected {
                        ui.painter().rect_filled(rect, 3.0, theme.selection_bg);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 3.0, theme.hover_bg);
                    }
                    let icon_rect = Rect::from_center_size(
                        Pos2::new(rect.left() + 14.0, rect.center().y),
                        Vec2::new(14.0, 11.0),
                    );
                    if is_dir {
                        ui.painter().rect_filled(
                            icon_rect,
                            2.0,
                            Color32::from_rgb(0xAF, 0xB1, 0xB3),
                        );
                        ui.painter().rect_filled(
                            Rect::from_min_size(icon_rect.left_top(), Vec2::new(6.0, 3.0))
                                .translate(Vec2::new(0.0, -2.0)),
                            1.0,
                            Color32::from_rgb(0xAF, 0xB1, 0xB3),
                        );
                    } else {
                        let r = Rect::from_center_size(icon_rect.center(), Vec2::new(10.0, 13.0));
                        ui.painter().rect_stroke(
                            r,
                            1.0,
                            Stroke::new(1.0, theme.text_dim),
                            egui::StrokeKind::Inside,
                        );
                        if name.ends_with(".py") {
                            ui.painter().text(
                                r.center(),
                                Align2::CENTER_CENTER,
                                "py",
                                egui::FontId::proportional(7.0),
                                theme.accent,
                            );
                        }
                    }
                    let color = if selected {
                        Color32::WHITE
                    } else if is_dir {
                        theme.text
                    } else {
                        theme.text_dim
                    };
                    ui.painter().text(
                        Pos2::new(rect.left() + 28.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        &name,
                        egui::FontId::proportional(13.0),
                        color,
                    );
                    if resp.double_clicked() {
                        if is_dir {
                            self.go(p.clone());
                        } else {
                            chosen = Some(p.clone());
                        }
                    } else if resp.clicked() {
                        self.selected = Some(p.clone());
                    }
                }
            });
        chosen
    }
}

/// `.gitignore` of a new project with Git: the bundled Python, VirtualEnv
/// and Node templates and the per-user RustPy files. The shared ones stay
/// committable (as the `.rustpy/.gitignore` RustPy writes keeps them):
/// `project.json` (content roots and folder marks, so teammates who clone get
/// the same layout), the run configurations and the data sources.
pub(super) fn new_project_gitignore(existing: &str) -> String {
    rustpy_vcs::gitignore_templates::new_project_gitignore(existing)
}

fn dirs_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub(super) use rustpy_project::setup::ProjectKind;

pub(super) struct NewProjectState {
    pub browser: FileBrowser,
    pub name: String,
    pub kind: ProjectKind,
    pub create_venv: bool,
    /// Create the environment with uv (`uv venv --seed`) instead of `python -m venv`.
    pub use_uv: bool,
    /// `uv` found on PATH.
    pub uv: Option<PathBuf>,
    /// `--system-site-packages`.
    pub inherit_site_packages: bool,
    pub base_interpreter: Option<PathBuf>,
    pub interpreters: Vec<Interpreter>,
    pub interp_rx: Option<crossbeam_channel::Receiver<Vec<Interpreter>>>,
    pub create_main: bool,
    pub git_init: bool,
    pub environment_manager: Option<rustpy_project::environment_manager::Manager>,
    pub environment_location: String,
    pub manager_executable: String,
    pub python_version: String,
    pub make_available_all: bool,
    pub install_requirements: bool,
    pub template_options: rustpy_project::project_templates::TemplateOptions,
    pub error: Option<String>,
}

impl NewProjectState {
    pub fn new(start: PathBuf) -> Self {
        let rx = rustpy_project::interpreter::discovery_receiver(None, true, None);
        let uv = super::project_setup::find_on_path("uv");
        Self {
            browser: FileBrowser::for_creation(start),
            name: "pythonProject".into(),
            kind: ProjectKind::Pure,
            create_venv: true,
            use_uv: false,
            uv,
            inherit_site_packages: false,
            base_interpreter: None,
            interpreters: Vec::new(),
            interp_rx: Some(rx),
            create_main: true,
            git_init: false,
            environment_manager: None,
            environment_location: ".venv".into(),
            manager_executable: String::new(),
            python_version: "3.13".into(),
            make_available_all: false,
            install_requirements: true,
            template_options: Default::default(),
            error: None,
        }
    }
}

pub(super) struct CloneState {
    pub recurse_submodules: bool,
    pub url: String,
    pub dir: String,
    /// The running `git clone`: job id (for ask-pass), process, target directory.
    pub job: Option<(u64, rustpy_vcs::GitJob, PathBuf)>,
    /// What the target directory held before the running clone started.
    prior: ClonePrior,
    pub log: String,
}

impl CloneState {
    pub fn new(dir: PathBuf) -> CloneState {
        CloneState {
            recurse_submodules: true,
            url: String::new(),
            dir: dir.display().to_string(),
            job: None,
            prior: ClonePrior::NotEmpty,
            log: String::new(),
        }
    }
}

/// What a clone's target directory held before the clone started.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ClonePrior {
    Missing,
    Empty,
    /// Files, or not a readable directory: git refuses to clone into it, and
    /// nothing in it is removed.
    NotEmpty,
}

fn clone_prior(target: &Path) -> ClonePrior {
    match std::fs::read_dir(target) {
        Ok(mut entries) => match entries.next() {
            None => ClonePrior::Empty,
            Some(_) => ClonePrior::NotEmpty,
        },
        Err(_) if target.symlink_metadata().is_ok() => ClonePrior::NotEmpty,
        Err(_) => ClonePrior::Missing,
    }
}

/// Removes what a clone that did not complete (cancelled, failed) left in
/// `target`, so cloning again does not fail with "destination path already
/// exists". Git cleans up itself when it can; a clone that had to be killed
/// (on Windows, or after the grace period) leaves its files behind. Only
/// what the clone created goes: the directory when it did not exist, its
/// contents when it was empty.
fn remove_partial_clone(target: &Path, prior: ClonePrior) {
    match prior {
        ClonePrior::Missing => {
            if target.is_dir() {
                let _ = std::fs::remove_dir_all(target);
            }
        }
        ClonePrior::Empty => {
            for entry in std::fs::read_dir(target).into_iter().flatten().flatten() {
                let path = entry.path();
                let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
            }
        }
        ClonePrior::NotEmpty => {}
    }
}

/// The clone dialog was closed while cloning: the clone is stopped and,
/// once it ended, what it left is removed (in the background). A clone
/// already cancelled keeps its grace period to clean up (stopping it again
/// would kill it at once).
const CLONE_CLEANUP_TASK: &str = "Cleaning up cancelled clone";

pub(super) fn abandoned_clone_cleanup_pending() -> Vec<(u64, String)> {
    let mut owners: Vec<_> = rustpy_core::TaskManager::global()
        .running_all()
        .into_iter()
        .filter(|task| task.title == CLONE_CLEANUP_TASK)
        .map(|task| (task.id, task.title))
        .collect();
    owners.sort_by_key(|(id, _)| *id);
    owners
}

fn finish_abandoned_clone(mut job: rustpy_vcs::GitJob, target: PathBuf, prior: ClonePrior) -> u64 {
    if !job.was_stopped() {
        job.stop();
    }
    // This is already cancellation cleanup. A timeout cannot relinquish the
    // job or its destination: descendants and pipe readers may still own them.
    // Keep it visible to shutdown until physical retirement and cleanup finish.
    rustpy_core::TaskManager::global()
        .task(CLONE_CLEANUP_TASK)
        .cancellable(false)
        .spawn(move |_| {
            while !job.wait(std::time::Duration::from_secs(1)) {}
            if !job.succeeded() {
                remove_partial_clone(&target, prior);
            }
            Ok(())
        })
        .id()
}

/// The directory `git clone <url>` creates (`repo` for `…/repo.git`, `git@host:user/repo`).
fn clone_dir_name(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let name = trimmed.rsplit(['/', ':', '\\']).next().unwrap_or("");
    if name.is_empty() {
        "repo".to_string()
    } else {
        name.to_string()
    }
}

/// Does `dir` look like a project root worth auto-opening?
pub(super) fn looks_like_project(dir: &Path) -> bool {
    for marker in [
        ".rustpy",
        ".git",
        "pyproject.toml",
        "setup.py",
        "requirements.txt",
        ".idea",
    ] {
        if dir.join(marker).exists() {
            return true;
        }
    }
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("py"))
        })
        .unwrap_or(false)
}

impl IdeApp {
    pub(super) fn project_creation_directory(&self) -> PathBuf {
        rustpy_settings::project_directory::creation_directory(
            self.settings.default_project_directory.as_deref(),
            self.project.as_ref().map(|project| project.root.as_path()),
            dirs::home_dir(),
        )
    }

    pub(super) fn welcome_screen(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let rect = ui.available_rect_before_wrap();
        ui.painter()
            .rect_filled(rect, 0.0, rgb(self.scheme.editor.background));
        crate::appearance_assets::paint(ui.ctx(), ui.painter(), rect, false);
        let mut open_recent: Option<PathBuf> = None;
        let mut remove_recent: Option<PathBuf> = None;
        egui::Area::new(ui.id().with("welcome"))
            .anchor(Align2::CENTER_CENTER, Vec2::new(0.0, -40.0))
            .show(ui.ctx(), |ui| {
                ui.set_width(620.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Welcome to RustPy")
                            .size(26.0)
                            .color(theme.text),
                    );
                    ui.label(
                        egui::RichText::new("A Python IDE written in Rust").color(theme.text_dim),
                    );
                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::new("New Project").min_size(Vec2::new(150.0, 34.0)))
                            .clicked()
                        {
                            self.dialog = Dialog::NewProject(Box::new(NewProjectState::new(
                                self.project_creation_directory(),
                            )));
                        }
                        if ui
                            .add(egui::Button::new("Open").min_size(Vec2::new(150.0, 34.0)))
                            .clicked()
                        {
                            self.open_path_chooser(dirs_home());
                        }
                        if ui
                            .add(egui::Button::new("Get from VCS").min_size(Vec2::new(150.0, 34.0)))
                            .clicked()
                        {
                            self.dialog = Dialog::CloneRepo(Box::new(CloneState::new(
                                self.project_creation_directory(),
                            )));
                        }
                    });
                    ui.add_space(18.0);
                    ui.horizontal_wrapped(|ui| {
                        for (title, action) in [
                            ("Welcome Guide", Action::WelcomeGuide),
                            ("Learn IDE Features", Action::LearnIdeFeatures),
                            ("Accessibility Guide", Action::AccessibilityGuide),
                        ] {
                            if ui.link(title).clicked() {
                                self.pending_action = Some(action);
                            }
                        }
                    });
                });
                let recents = self.settings.recent_projects.clone();
                if !recents.is_empty() {
                    ui.label(
                        egui::RichText::new("Recent projects")
                            .strong()
                            .color(theme.text),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            for p in recents {
                                let exists = p.is_dir();
                                ui.horizontal(|ui| {
                                    let name = p
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| p.display().to_string());
                                    let label = egui::RichText::new(name)
                                        .color(if exists {
                                            theme.text
                                        } else {
                                            theme.text_disabled
                                        })
                                        .size(15.0);
                                    if ui
                                        .add(egui::Label::new(label).sense(Sense::click()))
                                        .clicked()
                                        && exists
                                    {
                                        open_recent = Some(p.clone());
                                    }
                                    ui.label(
                                        egui::RichText::new(p.display().to_string())
                                            .color(theme.text_dim)
                                            .small(),
                                    );
                                    if ui
                                        .small_button("×")
                                        .on_hover_text("Remove from list")
                                        .clicked()
                                    {
                                        remove_recent = Some(p.clone());
                                    }
                                });
                            }
                        });
                }
            });
        if let Some(p) = open_recent {
            self.request_open_project(&p);
        }
        if let Some(p) = remove_recent {
            self.settings.recent_projects.retain(|r| r != &p);
            let _ = self.settings.save();
        }
    }

    pub(super) fn close_project(&mut self) {
        if !self.save_all() {
            return;
        }
        if !self.workspace_transition(workspace_ui::WorkspaceTransition::Close) {
            return;
        }
        self.restore_dialog_project_base();
        self.reset_documentation();
        self.emmet_ui = Default::default();
        self.cancel_startup_terminal();
        self.cancel_project_tasks();
        self.forget_all_tabs();
        self.project = None;
        self.project_index.files.clear();
        self.bottom = None;
        self.debug = None;
        self.debug_sessions.clear();
        self.split = None;
        self.set_status("Project closed");
        self.report_workspace_error();
        self.workspace_client = None;
    }

    pub(super) fn open_path_dialog(&mut self, ctx: &egui::Context, mut browser: Box<FileBrowser>) {
        if self.native_open_path_dialog(ctx, &browser) {
            return;
        }
        let theme = self.theme.clone();
        let mut open = true;
        let mut chosen: Option<PathBuf> = None;
        let mut cancel = false;
        egui::Window::new("Open File or Project").collapsible(false).resizable(true).default_size([640.0, 480.0]).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            ui.label(egui::RichText::new("Choose a directory to open it as a project, or a file to open it in the editor.").color(theme.text_dim));
            if let Some(p) = browser.ui(ui, &theme, true, 340.0) {
                chosen = Some(p);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let sel = browser.selected.clone();
                let sel_text = sel.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| browser.dir.display().to_string());
                ui.label(egui::RichText::new(sel_text).color(theme.text_dim).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Open").clicked() {
                        chosen = Some(sel.unwrap_or_else(|| browser.dir.clone()));
                    }
                    if ui.button("Open Current Directory as Project").clicked() {
                        chosen = Some(browser.dir.clone());
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
        if let Some(p) = chosen {
            if p.is_dir() {
                self.request_open_project(&p);
            } else if p.is_file() {
                if self.project.is_none() {
                    let root = p
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from("/"));
                    self.open_project(&root);
                }
                self.open_file(&p);
            }
            self.focus_editor = true;
        } else if open && !cancel {
            self.dialog = Dialog::OpenPath(browser);
        } else {
            self.focus_editor = true;
        }
    }

    fn new_project_template_options(&mut self, ui: &mut egui::Ui, state: &mut NewProjectState) {
        use rustpy_project::project_templates::TemplateEngine;
        fn engine(ui: &mut egui::Ui, id: &str, current: &mut TemplateEngine, django: bool) {
            let label = ui.label("Template engine");
            egui::ComboBox::from_id_salt(id)
                .selected_text(format!("{current:?}"))
                .show_ui(ui, |ui| {
                    if django {
                        ui.selectable_value(current, TemplateEngine::Django, "Django templates");
                    }
                    ui.selectable_value(current, TemplateEngine::Jinja2, "Jinja2");
                    ui.selectable_value(current, TemplateEngine::None, "No templates");
                })
                .combo_labelled(&label);
        }
        let options = &mut state.template_options;
        match state.kind {
            ProjectKind::Django => {
                ui.horizontal(|ui| {
                    ui.label("Django application name");
                    ui.text_edit_singleline(&mut options.django_app_name);
                });
                ui.checkbox(&mut options.django_admin, "Enable Django admin");
                ui.horizontal(|ui| {
                    engine(
                        ui,
                        "new_django_engine",
                        &mut options.django_template_engine,
                        true,
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Templates directory");
                    ui.text_edit_singleline(&mut options.django_templates_dir);
                });
            }
            ProjectKind::Flask => {
                ui.horizontal(|ui| {
                    engine(
                        ui,
                        "new_flask_engine",
                        &mut options.flask_template_engine,
                        false,
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Templates directory");
                    ui.text_edit_singleline(&mut options.flask_templates_dir);
                });
            }
            ProjectKind::FastApi => {
                ui.checkbox(&mut options.fastapi_async, "Use async request handlers");
            }
            ProjectKind::AppEngine => {
                ui.horizontal(|ui| {
                    ui.label("App Engine runtime");
                    ui.text_edit_singleline(&mut options.appengine_runtime);
                });
            }
            ProjectKind::Web2py => {
                ui.horizontal(|ui| {
                    ui.label("web2py framework directory");
                    let mut value = options
                        .web2py_framework
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if ui.text_edit_singleline(&mut value).changed() {
                        options.web2py_framework =
                            (!value.trim().is_empty()).then(|| PathBuf::from(value.trim()));
                    }
                });
            }
            ProjectKind::Angular | ProjectKind::React | ProjectKind::Vue | ProjectKind::Html5 => {
                ui.label("The project includes frontend sources and a development-server Run configuration. Node/npm is used for the frontend.");
            }
            _ => {}
        }
    }

    pub(super) fn new_project_dialog(&mut self, ctx: &egui::Context, mut st: Box<NewProjectState>) {
        let theme = self.theme.clone();
        if let Some(rx) = &st.interp_rx {
            if let Ok(found) = rx.try_recv() {
                st.interpreters = found;
                st.interp_rx = None;
                if st.base_interpreter.is_none() {
                    st.base_interpreter = st
                        .interpreters
                        .iter()
                        .find(|i| !i.is_venv)
                        .map(|i| i.path.clone())
                        .or_else(|| st.interpreters.first().map(|i| i.path.clone()));
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
        let mut open = true;
        let mut create = false;
        let mut cancel = false;
        let size = self.dialog_workspace.size("new-project", [680., 520.]);
        let response=egui::Window::new("New Project")
            .collapsible(false)
            .resizable(true)
            .vscroll(true)
            .default_size(size)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                if ui.small_button("Help: project setup").clicked(){self.dialog_workspace.help=Some("# New Project\n\nChoose a writable parent and a unique project name. Interpreter and environment creation are explicit setup steps. Validation errors retain this dialog. Settings for New Projects supplies formatting and run templates; existing project files are never replaced by those defaults.".into());}
                egui::Grid::new("newproj_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Name:");
                        let name =
                            ui.add(egui::TextEdit::singleline(&mut st.name).desired_width(300.0));
                        // Typing goes to the dialog's first field, never to the editor behind it.
                        if ui.memory(|m| m.focused().is_none()) {
                            name.request_focus();
                        }
                        ui.end_row();
                        ui.label("Location:");
                        ui.label(
                            egui::RichText::new(
                                st.browser.dir.join(&st.name).display().to_string(),
                            )
                            .color(theme.text_dim),
                        );
                        ui.end_row();
                    });
                ui.horizontal(|ui| {
                    let kind_label = ui.label(egui::RichText::new("Project type").strong());
                    egui::ComboBox::from_id_salt("new_project_kind")
                        .selected_text(st.kind.label())
                        .show_ui(ui, |ui| {
                            for kind in ProjectKind::all() {
                                ui.selectable_value(&mut st.kind, *kind, kind.label());
                            }
                        })
                        .combo_labelled(&kind_label);
                });
                self.new_project_template_options(ui, &mut st);
                ui.label(egui::RichText::new("Parent directory").strong());
                let _ = st.browser.ui(ui, &theme, false, 170.0);
                ui.separator();
                let interpreter_type = ui.label(egui::RichText::new("Interpreter type").strong());
                use rustpy_project::environment_manager::Manager;
                let previous_manager = st.environment_manager.unwrap_or(if st.use_uv {
                    Manager::Uv
                } else {
                    Manager::Virtualenv
                });
                let mut manager = previous_manager;
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("new_project_environment_manager")
                        .selected_text(manager.name())
                        .show_ui(ui, |ui| {
                            for value in Manager::ALL {
                                ui.selectable_value(&mut manager, value, value.name());
                            }
                        })
                        .combo_labelled(&interpreter_type);
                    if manager != Manager::System {
                        ui.radio_value(&mut st.create_venv, true, "Create new environment");
                        ui.radio_value(&mut st.create_venv, false, "Existing environment");
                    } else {
                        st.create_venv = false;
                    }
                });
                if manager != previous_manager {
                    st.manager_executable.clear();
                }
                st.environment_manager = Some(manager);
                if manager != Manager::System {
                    ui.horizontal(|ui| {
                        ui.label("Environment location");
                        ui.text_edit_singleline(&mut st.environment_location);
                    });
                }
                if !matches!(manager, Manager::Virtualenv | Manager::System) {
                    ui.horizontal(|ui| {
                        ui.label("Manager executable (PATH when empty)");
                        ui.text_edit_singleline(&mut st.manager_executable);
                    });
                }
                if st.create_venv {
                    ui.add_enabled_ui(matches!(manager, Manager::Virtualenv | Manager::Uv), |ui| {
                        ui.checkbox(
                            &mut st.inherit_site_packages,
                            "Inherit global site-packages",
                        );
                    });
                    if manager == Manager::Conda {
                        ui.horizontal(|ui| {
                            ui.label("Python version");
                            ui.text_edit_singleline(&mut st.python_version);
                        });
                    }
                    ui.checkbox(
                        &mut st.install_requirements,
                        "Install project requirements / dependencies",
                    );
                }
                ui.checkbox(
                    &mut st.make_available_all,
                    "Make interpreter available to all projects",
                );
                ui.horizontal(|ui| {
                    let interpreter_label = ui.label(if st.create_venv {
                        "Base interpreter:"
                    } else {
                        "Interpreter:"
                    });
                    let current = st
                        .base_interpreter
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(detecting…)".into());
                    egui::ComboBox::from_id_salt("newproj_interp")
                        .selected_text(current)
                        .width(380.0)
                        .show_ui(ui, |ui| {
                            for i in &st.interpreters {
                                let label = format!(
                                    "Python {} — {} ({})",
                                    i.version,
                                    i.path.display(),
                                    i.kind
                                );
                                if ui
                                    .selectable_label(
                                        st.base_interpreter.as_ref() == Some(&i.path),
                                        label,
                                    )
                                    .clicked()
                                {
                                    st.base_interpreter = Some(i.path.clone());
                                }
                            }
                        })
                        .combo_labelled(&interpreter_label);
                });
                ui.horizontal(|ui| {
                    ui.label("Python executable");
                    let mut value = st
                        .base_interpreter
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if ui.text_edit_singleline(&mut value).changed() {
                        st.base_interpreter =
                            (!value.trim().is_empty()).then(|| PathBuf::from(value.trim()));
                    }
                });
                ui.checkbox(&mut st.create_main, "Create a welcome script (main.py)");
                ui.checkbox(&mut st.git_init, "Create Git repository");
                if let Some(e) = &st.error {
                    ui.colored_label(theme.error, e);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        if ui
                            .add_enabled(!st.name.trim().is_empty(), egui::Button::new("Create"))
                            .clicked()
                        {
                            create = true;
                        }
                    });
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
            });
        if create || cancel || !open {
            if let Some(response) = response {
                self.dialog_workspace
                    .remember_size("new-project", response.response.rect.size().into());
            }
        }
        if create {
            let parent = match rustpy_settings::project_directory::validate(Path::new(
                st.browser.path_input.trim(),
            )) {
                Ok(parent) => parent,
                Err(error) => {
                    st.error = Some(error);
                    self.dialog = Dialog::NewProject(st);
                    return;
                }
            };
            let root = parent.join(st.name.trim());
            match self.create_project(&root, &st) {
                Ok(()) => {
                    self.focus_editor = true;
                    return;
                }
                Err(e) => st.error = Some(e),
            }
        }
        if open && !cancel {
            self.dialog = Dialog::NewProject(st);
        } else {
            self.focus_editor = true;
        }
    }

    fn create_project(&mut self, root: &Path, st: &NewProjectState) -> Result<(), String> {
        use rustpy_project::{
            environment_manager::{Manager, Request},
            project_templates::{self, TemplateFile, TemplateProgram},
        };
        let name = st.name.trim();
        if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\']) {
            return Err("Enter a project name without directory separators".into());
        }
        if root.exists()
            && std::fs::read_dir(root)
                .map_err(|error| error.to_string())?
                .next()
                .is_some()
        {
            return Err(format!(
                "{} already exists and is not empty",
                root.display()
            ));
        }
        let mut template_options = st.template_options.clone();
        template_options.project_name = name.to_string();
        template_options.create_main = st.create_main;
        let mut template = project_templates::plan(st.kind, &template_options)?;
        let frontend = matches!(
            st.kind,
            ProjectKind::Angular | ProjectKind::React | ProjectKind::Vue | ProjectKind::Html5
        );
        let manager = st.environment_manager.unwrap_or(if st.use_uv {
            Manager::Uv
        } else if st.create_venv {
            Manager::Virtualenv
        } else {
            Manager::System
        });
        let base = st.base_interpreter.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                super::project_setup::find_on_path(&path.to_string_lossy())
                    .unwrap_or_else(|| path.clone())
            }
        });
        if st.create_venv && !frontend && base.is_none() {
            return Err("Select a base Python interpreter".into());
        }
        // Manager manifests belong to the captured scaffold, before a process
        // can run. The common setup pipeline installs requirements into the
        // actual interpreter returned by the manager.
        if st.create_venv && !frontend {
            if manager == Manager::Pipenv
                && !template
                    .files
                    .iter()
                    .any(|file| file.path == Path::new("Pipfile"))
            {
                template.files.push(TemplateFile { path: "Pipfile".into(), contents: "[[source]]\nurl = \"https://pypi.org/simple\"\nverify_ssl = true\nname = \"pypi\"\n\n[packages]\n\n[dev-packages]\n".into() });
            }
            if matches!(manager, Manager::Poetry | Manager::Hatch | Manager::Pdm)
                && !template
                    .files
                    .iter()
                    .any(|file| file.path == Path::new("pyproject.toml"))
            {
                let package_name = super::project_setup::django_project_name(name)
                    .replace('_', "-")
                    .to_ascii_lowercase();
                let mut contents = format!("[project]\nname = {package_name:?}\nversion = \"0.1.0\"\nrequires-python = \">=3.10\"\n\n");
                if manager == Manager::Poetry {
                    contents.push_str("[tool.poetry]\npackage-mode = false\n");
                }
                if manager == Manager::Hatch {
                    contents.push_str("[tool.hatch.envs.default]\nskip-install = true\n");
                }
                template.files.push(TemplateFile {
                    path: "pyproject.toml".into(),
                    contents,
                });
            }
        }
        // Saving the current project must succeed before creating the next one.
        if self.tabs.iter().any(|tab| tab.has_unsaved_changes()) {
            self.save_all();
            let unsaved = self
                .tabs
                .iter()
                .filter(|tab| tab.has_unsaved_changes())
                .map(|tab| tab.state.title())
                .collect::<Vec<_>>();
            if !unsaved.is_empty() {
                return Err(format!(
                    "Save or close these files first: {}",
                    unsaved.join(", ")
                ));
            }
        }
        std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
        let root = dunce::canonicalize(root).map_err(|error| error.to_string())?;
        if st.git_init {
            if let Some(file) = template
                .files
                .iter_mut()
                .find(|file| file.path == Path::new(".gitignore"))
            {
                file.contents = new_project_gitignore(&file.contents);
            } else {
                template.files.push(TemplateFile {
                    path: ".gitignore".into(),
                    contents: new_project_gitignore(""),
                });
            }
        }
        template
            .write_new(&root)
            .map_err(|error| error.to_string())?;

        let mut store = RunConfigStore::load(&root);
        for run in &template.runs {
            let (kind, arguments) = match &run.program {
                TemplateProgram::Python
                    if run.args.first().is_some_and(|arg| arg == "-m") && run.args.len() >= 2 =>
                {
                    (
                        rustpy_project::RunConfigKind::PythonModule {
                            module: run.args[1].clone(),
                        },
                        run.args[2..].to_vec(),
                    )
                }
                TemplateProgram::Python => {
                    let script = run
                        .args
                        .first()
                        .ok_or("Template is missing its Python entry point")?;
                    let path = PathBuf::from(script);
                    (
                        rustpy_project::RunConfigKind::Python {
                            script: if path.is_absolute() {
                                path
                            } else {
                                root.join(path)
                            },
                        },
                        run.args[1..].to_vec(),
                    )
                }
                other => {
                    let program = match other {
                        TemplateProgram::Node => super::project_setup::find_on_path("node")
                            .unwrap_or_else(|| "node".into()),
                        TemplateProgram::Npm => super::project_setup::find_on_path("npm")
                            .unwrap_or_else(|| "npm".into()),
                        TemplateProgram::External(path) => path.clone(),
                        TemplateProgram::Python => unreachable!(),
                    };
                    (
                        rustpy_project::RunConfigKind::Program {
                            program,
                            args: run.args.clone(),
                        },
                        Vec::new(),
                    )
                }
            };
            let config = RunConfig {
                name: run.name.clone(),
                kind,
                parameters: arguments
                    .iter()
                    .map(|arg| format!("\"{}\"", arg.replace('"', "\"'\"'\"")))
                    .collect::<Vec<_>>()
                    .join(" "),
                working_dir: Some(root.clone()),
                env: run.environment.clone(),
                ..Default::default()
            };
            if store.selected.is_none() {
                store.selected = Some(config.name.clone());
            }
            store.upsert(config);
        }
        if !template.runs.is_empty() {
            store.save(&root).map_err(|error| error.to_string())?;
        }
        if !st.create_venv && manager == Manager::System {
            if let Some(interpreter) = &base {
                Workspace {
                    interpreter: Some(interpreter.clone()),
                    ..Default::default()
                }
                .save(&root)
                .map_err(|error| error.to_string())?;
            }
        }
        if self.settings.trust_project(&root) {
            self.settings.save().map_err(|error| error.to_string())?;
        }
        self.open_project(&root);
        if !self
            .project
            .as_ref()
            .is_some_and(|project| project.root == root)
        {
            return Err(format!(
                "{} was created but could not be opened",
                root.display()
            ));
        }
        let mut steps = Vec::new();
        if st.git_init {
            steps.push(super::project_setup::Step::Command {
                label: "Create Git repository".into(),
                program: rustpy_vcs::git_program().into(),
                args: vec!["init".into(), "-q".into()],
                cwd: root.clone(),
            });
        }
        let request = if !frontend && (st.create_venv || base.is_some()) {
            let base = base.unwrap_or_else(|| "python3".into());
            let executable = if st.manager_executable.trim().is_empty() {
                if matches!(manager, Manager::Virtualenv | Manager::System) {
                    base.clone()
                } else if manager == Manager::Uv {
                    st.uv
                        .clone()
                        .or_else(|| super::project_setup::find_on_path("uv"))
                        .unwrap_or_else(|| "uv".into())
                } else {
                    super::project_setup::find_on_path(manager.executable())
                        .unwrap_or_else(|| manager.executable().into())
                }
            } else {
                PathBuf::from(st.manager_executable.trim())
            };
            let location = if manager == Manager::System {
                base.clone()
            } else {
                let value = PathBuf::from(st.environment_location.trim());
                if value.is_absolute() {
                    value
                } else {
                    root.join(value)
                }
            };
            if st.create_venv && st.install_requirements && root.join("requirements.txt").is_file()
            {
                steps.push(super::project_setup::Step::InstallRequirements);
            }
            steps.push(super::project_setup::Step::SelectEnv);
            Some(Request {
                root: root.clone(),
                manager,
                existing: !st.create_venv,
                executable,
                base,
                location,
                python_version: st.python_version.trim().into(),
                inherit_site_packages: st.inherit_site_packages,
                install: false,
            })
        } else {
            None
        };
        let open = template
            .entry_files
            .iter()
            .map(|path| root.join(path))
            .collect::<Vec<_>>();
        for file in &open {
            if file.is_file() {
                self.open_file(file);
            }
        }
        if request.is_some() || !steps.is_empty() {
            self.pending_venv = Some(Box::new(super::project_setup::ProjectSetup::managed(
                root,
                request,
                steps,
                Vec::new(),
                st.make_available_all,
            )?));
        }
        Ok(())
    }

    /// Re-runs interpreter discovery for the open project (after creating a venv or
    /// trusting the project). The project's own venvs are probed only once trusted.
    pub(super) fn rediscover_interpreters(&mut self) {
        let project_safe_mode = self.project_safe_mode();
        let preferred = self.effective_workspace().interpreter;
        let trusted = !project_safe_mode;
        let Some(p) = &mut self.project else { return };
        p.interpreter_rx = Some(rustpy_project::interpreter::discovery_receiver(
            Some(p.root.clone()),
            trusted,
            preferred,
        ));
    }

    pub(super) fn clone_dialog(&mut self, ctx: &egui::Context, mut st: Box<CloneState>) {
        let theme = self.theme.clone();
        let mut open = true;
        let mut cancel = false;
        let mut done: Option<PathBuf> = None;
        // The clone runs as a background git job: progress, Cancel, and
        // credential / passphrase prompts through the ask-pass dialog.
        if let Some((_, job, _)) = &mut st.job {
            for line in job.poll() {
                self.git_panel
                    .git_jobs
                    .log(crate::git_jobs::ConsoleKind::Output, line);
            }
            if job.is_running() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            } else if let Some((id, job, target)) = st.job.take() {
                let output = job.output();
                self.git_panel
                    .git_jobs
                    .job_finished(id, job.succeeded(), &output);
                if job.succeeded() {
                    self.git_panel
                        .git_jobs
                        .log(crate::git_jobs::ConsoleKind::Success, "Cloned".into());
                    done = Some(target);
                } else if job.was_stopped() {
                    self.git_panel
                        .git_jobs
                        .log(crate::git_jobs::ConsoleKind::Failure, "Cancelled".into());
                    remove_partial_clone(&target, st.prior);
                    st.log = "Clone cancelled".into();
                } else {
                    self.git_panel
                        .git_jobs
                        .log(crate::git_jobs::ConsoleKind::Failure, "Failed".into());
                    remove_partial_clone(&target, st.prior);
                    st.log = format!(
                        "Clone failed: {}",
                        rustpy_vcs::describe_failure(&job.summary_line())
                    );
                }
            }
        }
        if let Some(p) = done {
            self.request_open_project(&p);
            self.focus_editor = true;
            return;
        }
        let mut start = false;
        let mut github_picker = false;
        egui::Window::new("Get from Version Control").collapsible(false).resizable(false).default_width(560.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            let running = st.job.is_some();
            if ui.add_enabled(!running,egui::Button::new("GitHub repositories…")).clicked(){github_picker=true;}
            ui.add_enabled_ui(!running, |ui| {
                egui::Grid::new("clone_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("URL:");
                    let url = ui.add(egui::TextEdit::singleline(&mut st.url).hint_text("https://github.com/user/repo.git or git@github.com:user/repo.git").desired_width(420.0));
                    if !running && ui.memory(|m| m.focused().is_none()) {
                        url.request_focus();
                    }
                    ui.end_row();
                    ui.label("Directory:");
                    ui.add(egui::TextEdit::singleline(&mut st.dir).desired_width(420.0));
                    ui.end_row();
                });
            });
            ui.add_enabled_ui(!running,|ui|{ui.checkbox(&mut st.recurse_submodules,"Initialize submodules recursively");});
            if let Some((_, job, target)) = &st.job {
                ui.horizontal(|ui| {
                    crate::a11y::spinner(ui);
                    let line = job.progress.clone().or_else(|| job.lines.last().cloned()).unwrap_or_else(|| format!("Cloning into {}…", target.display()));
                    ui.label(egui::RichText::new(line).color(theme.text_dim).small());
                });
            } else if !st.log.is_empty() {
                ui.label(egui::RichText::new(&st.log).color(theme.error).small());
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.add_enabled(!running && !st.url.trim().is_empty(), egui::Button::new("Clone")).clicked() {
                        start = true;
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
        if github_picker {
            self.github.clone_directory = Some(PathBuf::from(&st.dir));
            self.open_github_repositories(true);
            return;
        }
        if start {
            let url = st.url.trim().to_string();
            let parent =
                match rustpy_settings::project_directory::validate(Path::new(st.dir.trim())) {
                    Ok(parent) => parent,
                    Err(error) => {
                        st.log = error;
                        self.dialog = Dialog::CloneRepo(st);
                        return;
                    }
                };
            if let Err(error) = std::fs::create_dir_all(&parent) {
                st.log = format!("Cannot create clone parent: {error}");
                self.dialog = Dialog::CloneRepo(st);
                return;
            }
            let target = parent.join(clone_dir_name(&url));
            let cwd = parent;
            let (id, env) = self.git_panel.git_jobs.new_job();
            let mut args = vec![
                "clone".to_string(),
                "--progress".into(),
                url,
                target.display().to_string(),
            ];
            if st.recurse_submodules {
                args.insert(1, "--recurse-submodules".into());
            }
            let prior = clone_prior(&target);
            if self.settings.vcs.preferences.shallow_depth > 0 {
                args.insert(
                    1,
                    format!("--depth={}", self.settings.vcs.preferences.shallow_depth),
                );
            }
            match rustpy_vcs::GitJob::spawn(&cwd, &args, &env) {
                Ok(job) => {
                    self.git_panel.git_jobs.log(
                        crate::git_jobs::ConsoleKind::Command,
                        crate::git_jobs::copyable_command(&args),
                    );
                    st.job = Some((id, job, target));
                    st.prior = prior;
                    st.log.clear();
                }
                Err(e) => st.log = format!("{e:#}"),
            }
        }
        // Cancel while cloning stops the clone and keeps the dialog.
        if cancel {
            if let Some((_, job, _)) = &mut st.job {
                job.stop();
                cancel = false;
            }
        }
        if open && !cancel {
            self.dialog = Dialog::CloneRepo(st);
        } else {
            if let Some((_, job, target)) = st.job.take() {
                self.git_panel
                    .git_jobs
                    .log(crate::git_jobs::ConsoleKind::Failure, "Cancelled".into());
                finish_abandoned_clone(job, target, st.prior);
            }
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_refuses_unsynced_notebook_when_save_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current");
        std::fs::create_dir_all(&current).unwrap();
        let path = current.join("pending.ipynb");
        let original = rustpy_notebook::new_notebook().to_string();
        std::fs::write(&path, &original).unwrap();
        let mut settings = Settings::default();
        settings.trust_project(&current);
        let mut app = IdeApp::new(settings, Some(current.clone()));
        assert!(app.tabs.is_empty());
        app.open_file(&path);
        assert_eq!(app.tabs.len(), 1);
        let doc_version = app.tabs[0].state.doc.version();
        let history = app.tabs[0].state.doc.current_history();
        let nb = app.tabs[0].notebook.as_mut().unwrap();
        nb.set_cell_source(0, "print('pending unsynced cell')".into());
        nb.set_dirty(true);
        let synced = nb.synced_version();
        assert!(!app.tabs[0].state.doc.is_modified());
        let external = rustpy_notebook::new_notebook().to_string() + "\n";
        std::fs::write(&path, &external).unwrap();
        let next = dir.path().join("new-project");
        let st = NewProjectState {
            browser: FileBrowser::new(dir.path().to_path_buf()),
            name: "new-project".into(),
            kind: ProjectKind::Pure,
            create_venv: false,
            use_uv: false,
            uv: None,
            inherit_site_packages: false,
            base_interpreter: None,
            interpreters: vec![],
            interp_rx: None,
            create_main: false,
            git_init: false,
            environment_manager: None,
            environment_location: ".venv".into(),
            manager_executable: String::new(),
            python_version: "3.13".into(),
            make_available_all: false,
            install_requirements: true,
            template_options: Default::default(),
            error: None,
        };
        let error = app.create_project(&next, &st).unwrap_err();
        assert!(error.starts_with("Save or close these files first:"));
        assert!(error.contains("pending.ipynb"));
        assert!(
            !next.exists(),
            "failed save cannot create or switch projects"
        );
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].state.doc.text(), original);
        assert_eq!(app.tabs[0].state.doc.version(), doc_version);
        assert_eq!(app.tabs[0].state.doc.current_history(), history);
        let nb = app.tabs[0].notebook.as_ref().unwrap();
        assert!(nb.is_dirty());
        assert_eq!(nb.synced_version(), synced);
        assert_eq!(nb.model().cells[0].source, "print('pending unsynced cell')");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), external);
        assert_eq!(app.project.as_ref().unwrap().root, current);
    }

    #[test]
    #[cfg(unix)] // matches against a Unix root path
    fn new_project_gitignore_keeps_the_project_structure_shareable() {
        let mut builder = ignore::gitignore::GitignoreBuilder::new("/p");
        for line in new_project_gitignore("").lines() {
            builder.add_line(None, line).unwrap();
        }
        let gitignore = builder.build().unwrap();
        let ignored = |path: &str, dir: bool| {
            gitignore
                .matched_path_or_any_parents(Path::new("/p").join(path), dir)
                .is_ignore()
        };
        assert!(
            !ignored(".rustpy/project.json", false),
            "the project structure can be committed"
        );
        for shared in rustpy_project::project::SHARED_FILES {
            assert!(
                !ignored(&format!(".rustpy/{shared}"), false),
                "{shared} can be shared with a team"
            );
        }
        for per_user in [
            ".rustpy/workspace.json",
            ".rustpy/deployment.json",
            ".rustpy/tasks.json",
            ".rustpy/http-history.log",
            ".rustpy/coverage/run.coverage",
        ] {
            assert!(ignored(per_user, false), "{per_user}");
        }
        assert!(ignored(".venv", true));
        assert!(ignored("pkg/__pycache__/m.cpython-312.pyc", false));
        assert!(!ignored("main.py", false));
    }

    #[test]
    #[cfg(unix)] // matches against a Unix root path
    fn the_rustpy_gitignore_keeps_only_the_shared_files_committable() {
        // Written with the first project.json into an existing repository.
        let dir = std::env::temp_dir().join(format!("rustpy-dir-gitignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        rustpy_project::project::ProjectModel::default()
            .save(&dir)
            .unwrap();
        let text = std::fs::read_to_string(dir.join(".rustpy/.gitignore")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let mut builder = ignore::gitignore::GitignoreBuilder::new("/p/.rustpy");
        for line in text.lines() {
            builder.add_line(None, line).unwrap();
        }
        let gitignore = builder.build().unwrap();
        let ignored = |name: &str| {
            gitignore
                .matched_path_or_any_parents(Path::new("/p/.rustpy").join(name), false)
                .is_ignore()
        };
        for shared in rustpy_project::project::SHARED_FILES
            .into_iter()
            .chain([".gitignore"])
        {
            assert!(
                !ignored(shared),
                "{shared} can be shared with a team, as documented"
            );
        }
        for per_user in [
            "workspace.json",
            "deployment.json",
            "tasks.json",
            "http-history.log",
            "console_history.txt",
            "runtime_types.json",
            "coverage/run.coverage",
            "a-file-of-a-later-version.json",
        ] {
            assert!(ignored(per_user), "{per_user} stays out of version control");
        }
    }

    fn captured_wizard(parent: &Path, name: &str, kind: ProjectKind) -> NewProjectState {
        NewProjectState {
            browser: FileBrowser::new(parent.into()),
            name: name.into(),
            kind,
            create_venv: false,
            use_uv: false,
            uv: None,
            inherit_site_packages: false,
            base_interpreter: None,
            interpreters: Vec::new(),
            interp_rx: None,
            create_main: false,
            git_init: false,
            environment_manager: Some(rustpy_project::environment_manager::Manager::System),
            environment_location: ".venv".into(),
            manager_executable: String::new(),
            python_version: "3.13".into(),
            make_available_all: false,
            install_requirements: false,
            template_options: Default::default(),
            error: None,
        }
    }
    #[test]
    fn actual_wizard_creates_custom_flask_and_sdk_bound_run_configuration() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("website");
        let mut state = captured_wizard(parent.path(), "website", ProjectKind::Flask);
        state.template_options.flask_templates_dir = "site-templates".into();
        let mut app = IdeApp::new(Settings::default(), None);
        app.create_project(&root, &state).unwrap();
        assert!(root.join("app.py").is_file());
        assert!(root.join("site-templates/index.html").is_file());
        assert!(!root.join("main.py").exists());
        let configs = RunConfigStore::load(&root);
        assert!(configs.configs.iter().any(|config| matches!(&config.kind, rustpy_project::RunConfigKind::PythonModule {module} if module == "flask")));
        assert!(app.pending_venv.is_none());
        assert_eq!(app.project.as_ref().unwrap().root, root);
    }
    #[test]
    fn actual_frontend_wizard_preserves_python_free_setup_and_package_commands() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("frontend");
        let mut state = captured_wizard(parent.path(), "frontend", ProjectKind::React);
        state.create_venv = true;
        let mut app = IdeApp::new(Settings::default(), None);
        app.create_project(&root, &state).unwrap();
        assert!(root.join("package.json").is_file());
        assert!(root.join("src/main.jsx").is_file());
        assert!(!root.join(".venv").exists());
        assert!(app.pending_venv.is_none());
        let configs = RunConfigStore::load(&root);
        assert!(configs.configs.iter().any(|config| matches!(&config.kind, rustpy_project::RunConfigKind::Program {args,..} if args == &["run", "dev"])));
    }
    #[test]
    fn wizard_rejects_escape_names_before_creating_or_replacing_files() {
        let parent = tempfile::tempdir().unwrap();
        let state = captured_wizard(parent.path(), "../outside", ProjectKind::Pure);
        let target = parent.path().join("uncreated");
        let mut app = IdeApp::new(Settings::default(), None);
        assert!(app.create_project(&target, &state).is_err());
        assert!(!target.exists());
        assert!(app.project.is_none());
    }
}

#[cfg(test)]
mod clone_tests {
    use super::{
        clone_dir_name, clone_prior, finish_abandoned_clone, remove_partial_clone, ClonePrior,
    };
    use std::time::{Duration, Instant};

    /// An explicitly selected transport that never answers: the clone waits on it.
    fn slow_ssh(dir: &std::path::Path) -> rustpy_vcs::GitEnv {
        let script = dir.join("slow-ssh");
        let marker = dir.join("ssh-ready.pid");
        let _ = std::fs::remove_file(&marker);
        let quoted_marker = format!("'{}'", marker.to_string_lossy().replace('\'', "'\\''"));
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho $$ > {quoted_marker}\nexec sleep 30\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        rustpy_vcs::GitEnv {
            // The production Git policy installs GIT_SSH_COMMAND. GIT_SSH
            // would be ignored in its presence, accidentally contacting a real
            // remote rather than the fixture's owned waiting transport.
            vars: vec![
                (
                    "GIT_SSH_COMMAND".into(),
                    format!("'{}'", script.to_string_lossy().replace('\'', "'\\''")),
                ),
                ("GIT_SSH_VARIANT".into(), "ssh".into()),
            ],
        }
    }

    fn transport_ready(dir: &std::path::Path) -> bool {
        std::fs::read_to_string(dir.join("ssh-ready.pid"))
            .is_ok_and(|text| text.ends_with('\n') && text.trim().parse::<u32>().is_ok())
    }

    fn start_clone(dir: &std::path::Path, target: &std::path::Path) -> rustpy_vcs::GitJob {
        let args = vec![
            "clone".to_string(),
            "--progress".into(),
            "ssh://example.invalid/repo.git".into(),
            target.display().to_string(),
        ];
        let mut job = rustpy_vcs::GitJob::spawn(dir, &args, &slow_ssh(dir)).unwrap();
        // git creates the directory before it connects.
        let start = Instant::now();
        while (!target.join(".git").exists() || !transport_ready(dir))
            && start.elapsed() < Duration::from_secs(20)
        {
            job.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        let created = target.join(".git").exists();
        let transport_started = transport_ready(dir);
        if !created || !transport_started {
            job.stop();
            while !job.wait(Duration::from_secs(1)) {}
        }
        assert!(
            created,
            "git clone created {}; actual output: {}",
            target.display(),
            job.output()
        );
        assert!(
            transport_started,
            "the owned waiting SSH transport actually started"
        );
        job
    }

    // Retain the real job before any readiness/retirement assertion can unwind.
    struct OwnedClone(Option<rustpy_vcs::GitJob>);
    impl std::ops::Deref for OwnedClone {
        type Target = rustpy_vcs::GitJob;
        fn deref(&self) -> &Self::Target {
            self.0.as_ref().unwrap()
        }
    }
    impl std::ops::DerefMut for OwnedClone {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.0.as_mut().unwrap()
        }
    }
    impl Drop for OwnedClone {
        fn drop(&mut self) {
            if let Some(job) = &mut self.0 {
                if job.is_running() && !job.was_stopped() {
                    job.stop();
                }
                while !job.wait(Duration::from_secs(1)) {}
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn transport_identity(dir: &std::path::Path) -> (u32, String) {
        let pid: u32 = std::fs::read_to_string(dir.join("ssh-ready.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        (
            pid,
            stat.rsplit(") ")
                .next()
                .unwrap()
                .split_whitespace()
                .nth(19)
                .unwrap()
                .into(),
        )
    }
    #[cfg(target_os = "linux")]
    fn transport_retired((pid, start): &(u32, String)) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit(") ")
                    .next()?
                    .split_whitespace()
                    .nth(19)
                    .map(str::to_owned)
            })
            .is_none_or(|actual| &actual != start)
    }

    /// Review: Cancel during Get from VCS left the partial clone behind, and
    /// cloning again failed with "destination path already exists".
    #[test]
    fn a_cancelled_clone_leaves_no_directory_behind() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
            || cfg!(windows)
        {
            return;
        }
        let dir = crate::git_jobs::TestDir::new("clone-cancel");
        struct RetainFiles(Vec<u64>);
        impl Drop for RetainFiles {
            fn drop(&mut self) {
                while self
                    .0
                    .iter()
                    .any(|id| rustpy_core::TaskManager::global().is_running(*id))
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        let mut retain_files = RetainFiles(Vec::new());
        let target = dir.path().join("repo");
        assert_eq!(clone_prior(&target), ClonePrior::Missing);
        // Cancel follows the dialog's completed-job path: Git may clean up
        // itself, and the IDE removes any partial clone left after cancellation.
        let mut job = OwnedClone(Some(start_clone(dir.path(), &target)));
        #[cfg(target_os = "linux")]
        let transport = transport_identity(dir.path());
        job.stop();
        assert!(job.wait(Duration::from_secs(20)));
        assert!(job.was_stopped() && !job.succeeded());
        #[cfg(target_os = "linux")]
        assert!(
            transport_retired(&transport),
            "physical transport retirement precedes destination cleanup"
        );
        remove_partial_clone(&target, ClonePrior::Missing);
        assert!(
            !target.exists(),
            "cancelled clone was cleaned up by the IDE"
        );
        // Closing the dialog while cloning: the same, in the background.
        let mut job = OwnedClone(Some(start_clone(dir.path(), &target)));
        #[cfg(target_os = "linux")]
        let transport = transport_identity(dir.path());
        let cleanup_id =
            finish_abandoned_clone(job.0.take().unwrap(), target.clone(), ClonePrior::Missing);
        retain_files.0.push(cleanup_id);
        let start = Instant::now();
        while target.exists() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!target.exists(), "the abandoned clone was cleaned up");
        #[cfg(target_os = "linux")]
        assert!(
            transport_retired(&transport),
            "background cleanup must not run before owned transport retirement"
        );
    }

    #[test]
    fn leftovers_of_a_killed_clone_are_removed_but_nothing_else() {
        let dir = crate::git_jobs::TestDir::new("clone-leftovers");
        // The clone created the directory: it goes.
        let made = dir.path().join("made");
        assert_eq!(clone_prior(&made), ClonePrior::Missing);
        std::fs::create_dir_all(made.join(".git/objects")).unwrap();
        std::fs::write(made.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        remove_partial_clone(&made, ClonePrior::Missing);
        assert!(!made.exists());
        // An empty directory chosen as the target: emptied again, kept.
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(clone_prior(&empty), ClonePrior::Empty);
        std::fs::create_dir_all(empty.join(".git")).unwrap();
        std::fs::write(empty.join("README"), "x").unwrap();
        remove_partial_clone(&empty, ClonePrior::Empty);
        assert!(empty.is_dir() && std::fs::read_dir(&empty).unwrap().next().is_none());
        // A directory with files (git refuses to clone into it): untouched.
        let full = dir.path().join("full");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::write(full.join("mine.txt"), "keep").unwrap();
        assert_eq!(clone_prior(&full), ClonePrior::NotEmpty);
        remove_partial_clone(&full, ClonePrior::NotEmpty);
        assert!(full.join("mine.txt").exists());
    }

    #[test]
    fn clone_directory_names_follow_git() {
        assert_eq!(clone_dir_name("https://github.com/user/repo.git"), "repo");
        assert_eq!(clone_dir_name("git@github.com:user/repo.git"), "repo");
        assert_eq!(clone_dir_name("git@host:repo"), "repo");
        assert_eq!(clone_dir_name("/srv/git/project.git/"), "project");
        assert_eq!(clone_dir_name(""), "repo");
    }
}

#[cfg(test)]
mod creation_directory_regressions {
    use super::*;

    #[test]
    fn creation_browser_preserves_a_missing_explicit_parent() {
        let parent = PathBuf::from("/explicit/not-created/rustpy-projects");
        let browser = FileBrowser::for_creation(parent.clone());
        assert_eq!(browser.dir, parent);
        assert_eq!(browser.path_input, parent.display().to_string());
        assert!(browser.selected.is_none());
    }
}
