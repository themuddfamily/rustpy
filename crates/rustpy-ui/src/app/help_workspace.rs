//! Native Help/onboarding UI. Activity is local stable action IDs, never telemetry.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_settings::help::Preferences;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Guide {
    Welcome,
    GettingStarted,
    Accessibility,
    Topics,
}
#[derive(Default)]
pub(super) struct State {
    preferences: Option<Preferences>,
    path: Option<PathBuf>,
    dirty: bool,
    last_save: Option<std::time::Instant>,
    startup: bool,
    scripted: bool,
    guide: Option<Guide>,
    productivity: bool,
    filter: String,
    tour: Option<usize>,
    hint: Option<(String, std::time::Instant)>,
}
impl State {
    fn load(&mut self) {
        if self.preferences.is_none() {
            let path = Settings::system_dir().join("help-productivity.json");
            self.preferences = Some(Preferences::load(&path));
            self.path = Some(path);
        }
    }
    fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.last_save = Some(std::time::Instant::now());
        if let (Some(preferences), Some(path)) = (&mut self.preferences, &self.path) {
            match preferences.save(path) {
                Ok(()) => {
                    self.dirty = false;
                    self.last_save = Some(std::time::Instant::now());
                }
                Err(error) => log::warn!("Cannot save local Help preferences: {error}"),
            }
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.save();
    }
}
struct TourStep {
    title: &'static str,
    text: &'static str,
    panel: &'static str,
    action: Action,
}
const TOUR:&[TourStep]=&[
 TourStep{title:"Find your files",text:"The Project tool window shows the actual project tree. Show Me opens it; select a file to edit. If you have no project yet, use File → Open first.",panel:"project_panel",action:Action::ToggleProjectView},
 TourStep{title:"Find any command",text:"Menus show commands and their current keymap shortcuts. Show Me opens Find Action; search for a feature by name and press Enter.",panel:"menu_bar",action:Action::FindAction},
 TourStep{title:"Run and debug",text:"The main toolbar uses the selected run configuration and interpreter. The Run tool window keeps its real process output. Show Me opens Run; it does not start a program.",panel:"main_toolbar",action:Action::ToggleRunView},
 TourStep{title:"Tool windows",text:"The bottom tools hold Run, Debug, Terminal, Problems and other project views. Show Me opens the owned Terminal tool window.",panel:"bottom_tools",action:Action::ToggleTerminal},
 TourStep{title:"Status and background work",text:"The status bar shows editor and background state. Show Me opens the real background tasks view, where supported tasks expose cancellation.",panel:"status_bar",action:Action::ShowBackgroundTasks},
 TourStep{title:"Practice safely",text:"The Features Trainer opens practice files and checks your real editor actions. Choose a lesson to learn completion, navigation, refactoring or debugging.",panel:"menu_bar",action:Action::LearnIdeFeatures},
];
const WELCOME: &str = r#"# Welcome to RustPy

RustPy uses the Python interpreter and developer tools installed on your computer.

1. Open a project with **File → Open**, or create one with **File → New Project**.
2. Choose **Settings → Project → Python Interpreter**. Create a virtual environment or select an existing interpreter.
3. Open a Python file in the Project tree. Use **Find Action** to discover commands and their shortcuts in your selected keymap.
4. Create a run configuration, or use a file's Run gutter. Run and Debug keep their output in separate tool windows.
5. Save with **File → Save**. Git changes stay visible in the Commit and Git tools.

Use **Start interactive tour** for real UI highlights, or **Learn IDE Features** for checked practice lessons. Nothing in the guide starts a Python program automatically.
"#;
const GETTING_STARTED: &str = r#"# Getting started

## Editor essentials
Use **Search Everywhere** for files, symbols and commands. **Find Action** searches every registered action. Code completion, parameter information, quick documentation and intentions help while editing. Keymap Reference exports the current shortcuts as a PDF.

## Navigate and refactor
Go to Declaration follows a symbol; Find Usages shows references. Rename and other refactorings preview workspace changes. The Features Trainer includes Essentials, Editor basics, Code completion, Navigation, Refactorings, and Run/debug practice.

## Run, test and debug
Select a valid project interpreter first. Use Run configurations for scripts, modules, tests and servers. Add breakpoints in the gutter and choose Debug. Run, Debug and Test Results retain their own sessions and stop controls.

## Project tools
Project shows files; Structure shows document symbols; Terminal runs configured shells; Problems shows inspections. Use Settings to change keymaps, fonts, contrast, inspections and tool behavior. Tool-window actions can always be found with Find Action.

## Help and recovery
Help Topics is bundled and works offline. Collect Logs and Diagnostic Data creates a local export you can inspect before sharing. Contact Support and Submit Feedback only open drafts in your browser; they do not send anything. Edit Custom Properties opens the actual RustPy settings file.
"#;
const ACCESSIBILITY: &str = r#"# Accessibility guide

## Keyboard navigation
Use **Find Action** to discover commands without traversing menus. **Search Everywhere** locates files and symbols. **Focus Tool Window Controls** moves into a tool window's controls; **Escape** returns to the editor where supported. The selected keymap determines every displayed shortcut; open **Keyboard Shortcuts PDF** to export it.

## Visual settings
In Settings, choose the UI and editor font size, color scheme, high contrast, line spacing and caret appearance. Zoom actions change editor text size. Turn off distracting animations or hide unused tool windows through Appearance and View settings.

## Assistive technology
RustPy's native UI exposes accessible names, selected states and keyboard focus through its UI framework. Platform screen-reader behavior depends on the operating system and runtime. Report an inaccessible control using Submit Feedback with the action name and platform; no diagnostic files are attached automatically.

## Learn at your pace
The Getting Started tour highlights actual rendered panels and can be closed at any time. Show Me invokes the named action. If a panel is hidden or absent, the tour says so instead of placing a highlight at guessed coordinates. The Features Trainer uses local practice files and does not upload your work.
"#;
fn guide_text(guide: Guide) -> &'static str {
    match guide {
        Guide::Welcome => WELCOME,
        Guide::GettingStarted => GETTING_STARTED,
        Guide::Accessibility => ACCESSIBILITY,
        Guide::Topics => HELP_TEXT,
    }
}
pub(super) fn about_info() -> String {
    format!("RustPy {}\nBuild: {} ({})\nTarget: {}\nRuntime: native Rust; egui/eframe UI\nOS: {} / {}\nEmbedded web preview: {}\nBuild timestamp (SOURCE_DATE_EPOCH): {}\n",env!("CARGO_PKG_VERSION"),option_env!("RUSTPY_BUILD_REVISION").unwrap_or("development; revision not embedded"),option_env!("RUSTPY_BUILD_PROFILE").unwrap_or("unknown profile"),option_env!("RUSTPY_BUILD_TARGET").unwrap_or(std::env::consts::ARCH),std::env::consts::OS,std::env::consts::ARCH,if cfg!(feature="native-web-preview"){"enabled"}else{"not included"},option_env!("RUSTPY_BUILD_EPOCH").unwrap_or("not embedded"))
}
impl IdeApp {
    pub(super) fn observe_help_action(&mut self, action: Action) {
        if cfg!(test)
            || self.help_workspace.scripted
            || self.screenshot_path.is_some()
            || self.input_player.is_some()
            || !self.actions_on_start.is_empty()
        {
            return;
        }
        self.help_workspace.load();
        let preferences = self.help_workspace.preferences.as_mut().unwrap();
        if self.settings.diagnostics.collect_usage_statistics {
            preferences.observe(action.id());
            self.help_workspace.dirty = true;
        }
        if preferences.shortcut_hints {
            let shortcut = self.keymap.shortcut_text(action);
            if !shortcut.is_empty() {
                self.help_workspace.hint = Some((
                    format!("{}   {}", action.text(), shortcut),
                    std::time::Instant::now(),
                ));
            }
        }
    }
    pub(super) fn help_action(&mut self, action: Action, ctx: &egui::Context) {
        self.help_workspace.load();
        match action {
            Action::WelcomeGuide => {
                self.help_workspace.guide = Some(Guide::Welcome);
                self.help_workspace
                    .preferences
                    .as_mut()
                    .unwrap()
                    .welcome_seen = true;
                self.help_workspace.dirty = true;
            }
            Action::GettingStarted => self.help_workspace.guide = Some(Guide::GettingStarted),
            Action::AccessibilityGuide => self.help_workspace.guide = Some(Guide::Accessibility),
            Action::ProductivityGuide => self.help_workspace.productivity = true,
            Action::StartGuidedTour => {
                self.help_workspace.tour = Some(0);
                self.help_workspace.guide = None;
            }
            Action::EditCustomProperties => {
                let path = Settings::settings_path();
                if !path.exists() {
                    if let Err(error) = self.settings_to_persist().save() {
                        self.set_status(format!("Cannot create settings file: {error}"));
                        return;
                    }
                }
                self.open_file(&path);
            }
            Action::ContactSupport | Action::SubmitFeedback => {
                let title = if action == Action::ContactSupport {
                    "Support request"
                } else {
                    "Feedback"
                };
                let body=format!("{}\n\nDescribe your request, expected behavior, and steps to reproduce. Review this draft before submitting. No project files or logs are attached.",about_info());
                let mut url =
                    url::Url::parse("https://github.com/themuddfamily/rustpy/issues/new").unwrap();
                url.query_pairs_mut()
                    .append_pair("title", title)
                    .append_pair("body", &body);
                self.open_help_url(ctx, url.as_str());
            }
            Action::OnlineHelp => {
                self.open_help_url(ctx, "https://github.com/themuddfamily/rustpy#readme")
            }
            _ => {}
        }
    }
    fn open_help_url(&mut self, ctx: &egui::Context, url: &str) {
        if cfg!(test)
            || self.help_workspace.scripted
            || self.screenshot_path.is_some()
            || self.input_player.is_some()
            || !self.actions_on_start.is_empty()
        {
            self.set_status(format!("Help link: {url}"));
            return;
        }
        ctx.open_url(egui::OpenUrl::new_tab(url));
    }
    pub(super) fn help_workspace_tick(&mut self, ctx: &egui::Context) {
        self.help_workspace.scripted |= cfg!(test)
            || self.screenshot_path.is_some()
            || self.input_player.is_some()
            || !self.actions_on_start.is_empty()
            || self.run_on_start;
        self.help_workspace.load();
        if !self.help_workspace.startup && self.frame_count >= 4 {
            if self.help_workspace.scripted || std::env::var_os("RUSTPY_NO_STARTUP_TIPS").is_some()
            {
                self.help_workspace.startup = true;
            } else if matches!(self.dialog, Dialog::None)
                && self.help_workspace.guide.is_none()
                && self.help_workspace.tour.is_none()
                && !self.help_workspace.productivity
                && !self.exit_questions_open()
                && !self.exit_requested
                && !self.coordinated_quit_active()
            {
                self.help_workspace.startup = true;
                if self.settings.show_tips_on_startup {
                    let preferences = self.help_workspace.preferences.as_mut().unwrap();
                    self.dialog = Dialog::TipOfTheDay(preferences.next_tip % TIPS.len());
                    preferences.next_tip = (preferences.next_tip + 1) % TIPS.len();
                    self.help_workspace.dirty = true;
                } else if self.project.is_none()
                    && !self
                        .help_workspace
                        .preferences
                        .as_ref()
                        .unwrap()
                        .welcome_seen
                {
                    self.help_workspace
                        .preferences
                        .as_mut()
                        .unwrap()
                        .welcome_seen = true;
                    self.help_workspace.guide = Some(Guide::Welcome);
                    self.help_workspace.dirty = true;
                }
            }
        }
        if self.help_workspace.dirty
            && self
                .help_workspace
                .last_save
                .is_none_or(|at| at.elapsed() > std::time::Duration::from_secs(10))
        {
            self.help_workspace.save();
        }
        self.help_guide_window(ctx);
        self.productivity_window(ctx);
        self.tour_window(ctx);
        if let Some((text, at)) = &self.help_workspace.hint {
            if at.elapsed() < std::time::Duration::from_secs(3) {
                egui::Area::new(egui::Id::new("help_shortcut_hint"))
                    .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -38.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(text);
                        });
                    });
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }
    fn help_guide_window(&mut self, ctx: &egui::Context) {
        let Some(guide) = self.help_workspace.guide else {
            return;
        };
        let theme = self.theme.clone();
        let mut open = true;
        let mut action = None;
        let mut export = false;
        egui::Window::new("RustPy Guide")
            .id(egui::Id::new("rustpy_help_guide"))
            .open(&mut open)
            .default_size([780.0, 600.0])
            .show_modeless(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (label, page) in [
                        ("Welcome", Guide::Welcome),
                        ("Getting Started", Guide::GettingStarted),
                        ("Help Topics", Guide::Topics),
                        ("Accessibility", Guide::Accessibility),
                    ] {
                        if ui
                            .selectable_label(guide == page, crate::i18n::tr_static(label))
                            .clicked()
                        {
                            self.help_workspace.guide = Some(page);
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    for (label, command) in [
                        ("Find Action", Action::FindAction),
                        ("Learn IDE Features", Action::LearnIdeFeatures),
                        ("Start interactive tour", Action::StartGuidedTour),
                        ("Keyboard Shortcuts PDF", Action::KeymapReference),
                    ] {
                        if ui.button(crate::i18n::tr_static(label)).clicked() {
                            action = Some(command);
                        }
                    }
                    if ui.button("Export bundled HTML").clicked() {
                        export = true;
                    }
                    if ui.link("Online help / project").clicked() {
                        action = Some(Action::OnlineHelp);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    crate::markdown_view::render_markdown(ui, &theme, guide_text(guide), 14.0);
                });
            });
        if !open {
            self.help_workspace.guide = None;
        }
        if export {
            let path = Settings::system_dir().join("help/rustpy-guide.html");
            let text = format!(
                "{}\n\n{}\n\n{}\n\n{}",
                WELCOME, GETTING_STARTED, HELP_TEXT, ACCESSIBILITY
            );
            let html = crate::markdown_workspace::html_document(
                &text,
                None,
                &self.settings.markdown,
                true,
            );
            let result = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, html)
            })();
            match result {
                Ok(()) => self.open_help_url(ctx, &crate::markdown_workspace::file_url(&path)),
                Err(error) => self.set_status(error.to_string()),
            }
        }
        if let Some(action) = action {
            if action == Action::OnlineHelp {
                self.open_help_url(ctx, "https://github.com/themuddfamily/rustpy#readme");
            } else {
                self.handle_action(action, ctx);
            }
        }
    }
    fn productivity_window(&mut self, ctx: &egui::Context) {
        if !self.help_workspace.productivity {
            return;
        }
        let mut open = true;
        let mut reset = false;
        let mut invoke = None;
        let mut collect_usage = self.settings.diagnostics.collect_usage_statistics;
        let keymap = &self.keymap;
        let preferences = self.help_workspace.preferences.as_mut().unwrap();
        egui::Window::new("Productivity Guide").open(&mut open).default_size([720.0,520.0]).show_modeless(ctx,|ui|{ui.label("Local counts of invoked action IDs. No document content, filenames, keypresses or usage data are sent anywhere.");ui.checkbox(&mut collect_usage,"Record local feature usage (Data Sharing setting)");if ui.checkbox(&mut preferences.shortcut_hints,"Show current keymap shortcut hints on action use").changed(){self.help_workspace.dirty=true;}ui.horizontal(|ui|{ui.label("Filter");ui.text_edit_singleline(&mut self.help_workspace.filter);if ui.button("Reset local counts").clicked(){reset=true;}});let mut counts=Action::ALL.iter().copied().map(|action|(action,preferences.counts.get(action.id()).copied().unwrap_or(0))).collect::<Vec<_>>();counts.sort_by_key(|(action,count)|(std::cmp::Reverse(*count),action.english_text()));egui::ScrollArea::vertical().show(ui,|ui|{for(action,count)in counts{if !action.text().to_lowercase().contains(&self.help_workspace.filter.to_lowercase()){continue;}ui.horizontal(|ui|{ui.label(format!("{count:>6}"));if ui.link(action.text()).clicked(){invoke=Some(action);}ui.label(keymap.shortcut_text(action));});}});});
        self.help_workspace.productivity = open;
        if collect_usage != self.settings.diagnostics.collect_usage_statistics {
            self.settings.diagnostics.collect_usage_statistics = collect_usage;
            if let Some(snapshot) = &mut self.settings_ui.snapshot {
                snapshot.settings.diagnostics.collect_usage_statistics = collect_usage;
            }
            if let Err(error) = self.settings_to_persist().save() {
                self.set_status(format!("Cannot save usage preference: {error}"));
            }
        }
        if reset {
            self.help_workspace
                .preferences
                .as_mut()
                .unwrap()
                .reset_usage();
            self.help_workspace.dirty = true;
        }
        if let Some(action) = invoke {
            self.handle_action(action, ctx);
        }
    }
    fn tour_window(&mut self, ctx: &egui::Context) {
        let Some(index) = self.help_workspace.tour else {
            return;
        };
        let step = &TOUR[index.min(TOUR.len() - 1)];
        let visible = match step.panel {
            "project_panel" => self.layout.left_visible && self.project.is_some(),
            "menu_bar" => self.shows_menu_bar(),
            "main_toolbar" => {
                self.settings.appearance.show_main_toolbar
                    && self.project.is_some()
                    && self.shows_chrome()
            }
            "status_bar" => self.settings.appearance.show_status_bar && self.shows_chrome(),
            "bottom_tools" => {
                self.bottom.is_some() && !self.floating_bottom && !self.bottom_maximized
            }
            _ => false,
        };
        let rect = egui::PanelState::load(ctx, egui::Id::new(step.panel))
            .map(|state| state.outer_rect)
            .filter(|rect| {
                visible
                    && rect.is_finite()
                    && rect.is_positive()
                    && ctx.content_rect().intersects(*rect)
            });
        if let Some(rect) = rect {
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("guided_tour_highlight"),
            ))
            .rect_stroke(
                rect.shrink(2.0),
                4.0,
                egui::Stroke::new(3.0, self.theme.accent),
                egui::StrokeKind::Inside,
            );
        }
        let mut open = true;
        let mut action = None;
        egui::Window::new("Getting Started tour").open(&mut open).default_width(440.0).anchor(Align2::RIGHT_TOP,Vec2::new(-24.0,72.0)).show_modeless(ctx,|ui|{ui.strong(format!("{} / {} — {}",index+1,TOUR.len(),step.title));ui.label(step.text);if rect.is_none(){ui.label("This panel is currently hidden or absent. Show Me opens the feature using its real action.");}ui.horizontal(|ui|{if ui.button("Show Me").clicked(){action=Some(step.action);}
if ui.add_enabled(index>0,egui::Button::new("Back")).clicked(){self.help_workspace.tour=Some(index-1);}
if ui.button(if index+1==TOUR.len(){"Finish"}else{"Next"}).clicked(){self.help_workspace.tour=(index+1<TOUR.len()).then_some(index+1);}});});
        if !open {
            self.help_workspace.tour = None;
        }
        if let Some(action) = action {
            self.handle_action(action, ctx);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_tips_leave_an_owned_dialog_unchanged_in_headless_runs() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                show_tips_on_startup: true,
                ..Settings::default()
            },
            None,
        );
        app.frame_count = 8;
        app.screenshot_path = Some(PathBuf::from("unexecuted-help-fixture.png"));
        app.dialog = Dialog::About;
        app.help_workspace_tick(&egui::Context::default());
        assert!(matches!(app.dialog, Dialog::About));
        assert!(app.help_workspace.startup);
    }
    #[test]
    fn tour_uses_real_panel_ids_and_registered_actions() {
        for step in TOUR {
            assert!([
                "project_panel",
                "menu_bar",
                "main_toolbar",
                "bottom_tools",
                "status_bar"
            ]
            .contains(&step.panel));
            assert!(Action::ALL.contains(&step.action));
        }
    }
    #[test]
    fn about_draft_contains_platform_build_runtime_and_no_project_paths() {
        let info = about_info();
        for label in ["RustPy", "Build:", "Target:", "Runtime:", "OS:"] {
            assert!(info.contains(label));
        }
        assert!(!info.contains("/home/"));
    }
    #[test]
    fn bundled_guides_have_actionable_onboarding_and_accessibility() {
        assert!(WELCOME.contains("Python Interpreter"));
        assert!(ACCESSIBILITY.contains("Keyboard"));
        assert!(GETTING_STARTED.contains("Collect Logs"));
    }
}
