//! Project/path navigation chrome. Its transient state lives in the window's
//! egui context, so the integration needs no new IdeApp fields. Call keys before
//! shortcut dispatch, draw the panel after the main toolbar, and dispatch the
//! keymap's Alt+Home action to focus_navigation_bar.

use super::*;
use crate::a11y::DialogWindow;

fn state_id() -> egui::Id {
    egui::Id::new("project_navigation_bar_state")
}
fn focus_id() -> egui::Id {
    egui::Id::new("project_navigation_bar_focus")
}

#[derive(Clone, Default)]
struct BarState {
    root: PathBuf,
    focused: bool,
    popup: bool,
    crumb: usize,
    selected: usize,
    // One-shot reveal requests; idle drawing never overrides manual scrolling.
    reveal_choice: bool,
    reveal_crumb: bool,
    query: String,
    browse: Option<PathBuf>,
    entries: Vec<NavEntry>,
    error: Option<String>,
    anchor: egui::Pos2,
}

#[derive(Clone)]
struct NavEntry {
    path: PathBuf,
    name: String,
    directory: bool,
}
#[derive(Clone)]
struct Crumb {
    path: PathBuf,
    name: String,
}

impl BarState {
    fn matches(&self) -> Vec<NavEntry> {
        self.entries
            .iter()
            .filter(|entry| crate::project_tree::speed_search_matches(&entry.name, &self.query))
            .cloned()
            .collect()
    }
    fn save(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(state_id(), self));
    }
}

impl IdeApp {
    fn nav_bar_state(&self, ctx: &egui::Context) -> BarState {
        let mut state = ctx
            .data(|d| d.get_temp::<BarState>(state_id()))
            .unwrap_or_default();
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        if state.root != root {
            state = BarState {
                root,
                ..Default::default()
            };
        }
        state
    }

    fn nav_bar_crumbs(&self, state: &BarState) -> Vec<Crumb> {
        let Some(project) = &self.project else {
            return Vec::new();
        };
        let mut crumbs = vec![Crumb {
            path: project.root.clone(),
            name: path_label(&project.root),
        }];
        let current = state.browse.as_deref().or_else(|| {
            self.active
                .and_then(|i| self.tabs.get(i))
                .and_then(|tab| tab.state.path.as_deref())
        });
        let Some(current) = current else {
            return crumbs;
        };
        // Attached content roots and external editors retain the project crumb,
        // followed by their actual path; selecting the file still lists siblings.
        let base = if current.starts_with(&project.root) {
            project.root.clone()
        } else {
            let roots = project.tree.model().content_roots;
            roots
                .into_iter()
                .filter(|root| current.starts_with(root))
                .max_by_key(|root| root.components().count())
                .unwrap_or_else(|| current.parent().unwrap_or(current).to_path_buf())
        };
        if base != project.root {
            crumbs.push(Crumb {
                name: base.display().to_string(),
                path: base.clone(),
            });
        }
        if let Ok(relative) = current.strip_prefix(&base) {
            let mut path = base;
            for component in relative.components() {
                path.push(component);
                crumbs.push(Crumb {
                    name: path_label(&path),
                    path: path.clone(),
                });
            }
        }
        crumbs
    }

    /// The registered Jump to Navigation Bar action (default Alt+Home) calls this.
    /// When the permanent bar is hidden, the same navigator appears temporarily.
    pub(super) fn focus_navigation_bar(&mut self, ctx: &egui::Context) {
        if self.project.is_none()
            || !self.shows_chrome()
            || !matches!(self.dialog, Dialog::None)
            || self.layout.exit_prompt.is_some()
        {
            return;
        }
        let mut state = self.nav_bar_state(ctx);
        state.focused = true;
        state.popup = false;
        state.browse = None;
        state.query.clear();
        state.crumb = self.nav_bar_crumbs(&state).len().saturating_sub(1);
        state.reveal_crumb = true;
        state.anchor = ctx.content_rect().center_top() + egui::vec2(-180.0, 80.0);
        ctx.memory_mut(|m| m.request_focus(focus_id()));
        self.focus_editor = false;
        state.save(ctx);
    }

    fn nav_bar_choices(&self, state: &mut BarState) {
        let crumbs = self.nav_bar_crumbs(state);
        state.crumb = state.crumb.min(crumbs.len().saturating_sub(1));
        let Some(crumb) = crumbs.get(state.crumb) else {
            return;
        };
        let dir = if crumb.path.is_dir() {
            crumb.path.as_path()
        } else {
            crumb.path.parent().unwrap_or(&crumb.path)
        };
        state.entries.clear();
        state.error = None;
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                let show_hidden = self.project.as_ref().is_some_and(|p| p.tree.show_hidden);
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            let path = entry.path();
                            let directory = path.is_dir();
                            let name = entry.file_name().to_string_lossy().into_owned();
                            // Match ProjectTree's hidden-folder policy, including caches.
                            if !show_hidden
                                && directory
                                && ((name.starts_with('.') && name != ".github")
                                    || matches!(name.as_str(), "__pycache__" | "node_modules"))
                            {
                                continue;
                            }
                            state.entries.push(NavEntry {
                                path,
                                name,
                                directory,
                            });
                        }
                        Err(error) => state.error = Some(error.to_string()),
                    }
                }
                state.entries.sort_by(|a, b| {
                    b.directory
                        .cmp(&a.directory)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
            Err(error) => state.error = Some(format!("Cannot list {}: {error}", dir.display())),
        }
        state.selected = state
            .entries
            .iter()
            .position(|entry| entry.path == crumb.path)
            .unwrap_or(0);
        state.query.clear();
        state.popup = true;
        state.reveal_choice = true;
    }

    fn nav_bar_choose(&mut self, state: &mut BarState, entry: NavEntry) {
        if entry.directory {
            if let Some(project) = &mut self.project {
                project.tree.reveal(&entry.path);
            }
            state.browse = Some(entry.path);
            state.crumb = self.nav_bar_crumbs(state).len().saturating_sub(1);
            state.reveal_crumb = true;
            self.nav_bar_choices(state);
        } else {
            self.mark_nav_jump();
            self.open_file(&entry.path);
            if let Some(project) = &mut self.project {
                project.tree.reveal(&entry.path);
            }
            state.focused = false;
            state.popup = false;
            state.browse = None;
            state.query.clear();
            self.focus_editor = true;
        }
    }

    /// Must run before editor/project/IDE shortcuts to own arrows, typing and Escape.
    /// Returns true while the navigator owns keyboard input.
    pub(super) fn navigation_bar_keys(&mut self, ctx: &egui::Context) -> bool {
        let mut state = self.nav_bar_state(ctx);
        if self.project.is_none()
            || !self.shows_chrome()
            || !matches!(self.dialog, Dialog::None)
            || self.layout.exit_prompt.is_some()
        {
            state.focused = false;
            state.popup = false;
            ctx.memory_mut(|m| m.surrender_focus(focus_id()));
            state.save(ctx);
            return false;
        }
        if !state.focused {
            return false;
        }
        let mut keys = Vec::new();
        let mut text = String::new();
        ctx.input_mut(|input| {
            for key in [
                egui::Key::Escape,
                egui::Key::ArrowLeft,
                egui::Key::ArrowRight,
                egui::Key::ArrowDown,
                egui::Key::ArrowUp,
                egui::Key::Enter,
                egui::Key::Backspace,
                egui::Key::Home,
                egui::Key::End,
            ] {
                if input.consume_key(egui::Modifiers::NONE, key) {
                    keys.push(key);
                }
            }
            input.events.retain(|event| {
                if let egui::Event::Text(value) = event {
                    text.push_str(value);
                    false
                } else {
                    true
                }
            });
        });
        for key in keys {
            match key {
                egui::Key::Escape => {
                    state.focused = false;
                    state.popup = false;
                    state.browse = None;
                    self.focus_editor = true;
                    ctx.memory_mut(|m| m.surrender_focus(focus_id()));
                    break;
                }
                egui::Key::ArrowLeft | egui::Key::ArrowRight => {
                    let count = self.nav_bar_crumbs(&state).len();
                    state.crumb = if key == egui::Key::ArrowLeft {
                        state.crumb.saturating_sub(1)
                    } else {
                        (state.crumb + 1).min(count.saturating_sub(1))
                    };
                    state.reveal_crumb = true;
                    if state.popup {
                        self.nav_bar_choices(&mut state);
                    }
                }
                egui::Key::ArrowDown | egui::Key::ArrowUp => {
                    if !state.popup {
                        self.nav_bar_choices(&mut state);
                    } else {
                        let count = state.matches().len();
                        if count > 0 {
                            state.selected = if key == egui::Key::ArrowDown {
                                (state.selected + 1) % count
                            } else {
                                (state.selected + count - 1) % count
                            };
                            state.reveal_choice = true;
                        }
                    }
                }
                egui::Key::Home => {
                    state.selected = 0;
                    state.reveal_choice = true;
                }
                egui::Key::End => {
                    state.selected = state.matches().len().saturating_sub(1);
                    state.reveal_choice = true;
                }
                egui::Key::Backspace => {
                    state.query.pop();
                    state.selected = 0;
                    state.reveal_choice = true;
                }
                egui::Key::Enter => {
                    if state.popup {
                        if let Some(entry) = state.matches().get(state.selected).cloned() {
                            self.nav_bar_choose(&mut state, entry);
                            if !state.focused {
                                ctx.memory_mut(|m| m.surrender_focus(focus_id()));
                            }
                        }
                    } else {
                        self.nav_bar_choices(&mut state);
                    }
                }
                _ => {}
            }
        }
        if state.focused && !text.is_empty() {
            if !state.popup {
                self.nav_bar_choices(&mut state);
            }
            state.query.push_str(&text);
            state.selected = 0;
            state.reveal_choice = true;
        }
        state.save(ctx);
        true
    }

    /// Draw after the main toolbar, before edge stripes and dock panels.
    pub(super) fn navigation_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut state = self.nav_bar_state(&ctx);
        if self.project.is_none() || !self.shows_chrome() {
            return;
        }
        if !matches!(self.dialog, Dialog::None) || self.layout.exit_prompt.is_some() {
            state.focused = false;
            state.popup = false;
        }
        let mut regions = Vec::new();
        let in_toolbar = self.settings.chrome.navigation_location
            == rustpy_settings::chrome::NavigationLocation::Toolbar
            && self.settings.appearance.show_main_toolbar;
        if self.settings.appearance.show_navigation_bar && !in_toolbar {
            let panel = egui::Panel::top("project_navigation_bar")
                .resizable(false)
                .frame(egui::Frame::new().fill(self.theme.panel_bg))
                .show(ui, |ui| self.navigation_bar_segments(ui, &mut state));
            regions.push(panel.response.rect);
        } else if state.focused && !(self.settings.appearance.show_navigation_bar && in_toolbar) {
            if let Some(window) = egui::Window::new("Navigation Bar")
                .id(egui::Id::new("temporary_navigation_bar"))
                .collapsible(false)
                .resizable(false)
                .default_pos(state.anchor)
                .show_modeless(&ctx, |ui| self.navigation_bar_segments(ui, &mut state))
            {
                regions.push(window.response.rect);
            }
        }
        if state.focused && state.popup {
            let reveal_choice = std::mem::take(&mut state.reveal_choice);
            let mut open = true;
            let window = egui::Window::new("Navigate to…")
                .id(egui::Id::new("navigation_bar_choices"))
                .collapsible(false)
                .resizable(false)
                .default_pos(state.anchor)
                .open(&mut open)
                .show_modeless(&ctx, |ui| {
                    ui.set_min_width(220.0);
                    if !state.query.is_empty() {
                        ui.label(format!("Search: {}", state.query));
                    }
                    if let Some(error) = &state.error {
                        ui.colored_label(self.theme.error, error);
                    }
                    let entries = state.matches();
                    if entries.is_empty() {
                        ui.label("No matching files or folders");
                    }
                    #[cfg(test)]
                    let wheel_diagnostics = ctx.data(|d| {
                        d.get_temp::<bool>(egui::Id::new("nav_bar_test_wheel_diagnostics"))
                            .unwrap_or(false)
                    });
                    #[cfg(test)]
                    if wheel_diagnostics {
                        let pointer = ctx.input(|i| i.pointer.interact_pos());
                        let pointer_layer = pointer.and_then(|pos| ctx.layer_id_at(pos));
                        eprintln!(
                            "NAV_WHEEL before time={} chooser_layer={:?} pointer={:?} pointer_layer={:?} smooth_delta={:?} reveal_choice={} selected={}",
                            ctx.input(|i| i.time),
                            ui.layer_id(),
                            pointer,
                            pointer_layer,
                            ctx.input(|i| i.smooth_scroll_delta()),
                            reveal_choice,
                            state.selected,
                        );
                    }
                    let chooser_scroll =
                        egui::ScrollArea::vertical()
                            .max_height(350.0)
                            .show(ui, |ui| {
                                for (index, entry) in entries.into_iter().enumerate() {
                                    let label = format!(
                                        "{}{}",
                                        entry.name,
                                        if entry.directory { "/" } else { "" }
                                    );
                                    let response = ui
                                        .selectable_label(index == state.selected, label)
                                        .on_hover_text(entry.path.display().to_string());
                                    if reveal_choice && index == state.selected {
                                        response.scroll_to_me(Some(egui::Align::Center));
                                    }
                                    #[cfg(test)]
                                    remember_nav_hit(
                                        &ctx,
                                        entry.path.clone(),
                                        false,
                                        response.rect,
                                    );
                                    if response.clicked() {
                                        self.nav_bar_choose(&mut state, entry);
                                    }
                                }
                            });
                    #[cfg(test)]
                    if wheel_diagnostics {
                        eprintln!(
                            "NAV_WHEEL after time={} chooser_layer={:?} scroll_id={:?} offset={:?} inner_rect={:?} content_size={:?} owns_pointer={} center_layer={:?} smooth_delta={:?}",
                            ctx.input(|i| i.time),
                            ui.layer_id(),
                            chooser_scroll.id,
                            chooser_scroll.state.offset,
                            chooser_scroll.inner_rect,
                            chooser_scroll.content_size,
                            ui.rect_contains_pointer(chooser_scroll.inner_rect),
                            ctx.layer_id_at(chooser_scroll.inner_rect.center()),
                            ctx.input(|i| i.smooth_scroll_delta()),
                        );
                    }
                    #[cfg(test)]
                    ctx.data_mut(|d| {
                        d.insert_temp(
                            egui::Id::new("nav_bar_test_scroll"),
                            (
                                chooser_scroll.state.offset.y,
                                chooser_scroll.inner_rect,
                                chooser_scroll.content_size.y,
                            ),
                        )
                    });
                    #[cfg(not(test))]
                    let _ = chooser_scroll;
                });
            if let Some(window) = window {
                regions.push(window.response.rect);
            }
            if !open {
                state.focused = false;
                state.popup = false;
                state.browse = None;
                self.focus_editor = true;
            }
        }
        if state.focused && ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                if !regions.iter().any(|rect| rect.contains(pos)) {
                    state.focused = false;
                    state.popup = false;
                    state.browse = None;
                    // Let the clicked editor/tool receive focus naturally.
                    self.focus_editor = false;
                }
            }
        }
        if state.focused {
            ctx.memory_mut(|m| m.request_focus(focus_id()));
        } else if ctx.memory(|m| m.has_focus(focus_id())) {
            ctx.memory_mut(|m| m.surrender_focus(focus_id()));
        }
        state.save(&ctx);
    }

    pub(super) fn navigation_toolbar(&mut self, ui: &mut egui::Ui) {
        if self.settings.chrome.navigation_location
            != rustpy_settings::chrome::NavigationLocation::Toolbar
            || !self.settings.appearance.show_navigation_bar
            || self.project.is_none()
            || !self.shows_chrome()
        {
            return;
        }
        let mut state = self.nav_bar_state(ui.ctx());
        self.navigation_bar_segments(ui, &mut state);
        state.save(ui.ctx());
    }

    fn navigation_bar_segments(&mut self, ui: &mut egui::Ui, state: &mut BarState) {
        if !matches!(self.dialog, Dialog::None) || self.layout.exit_prompt.is_some() {
            ui.disable();
        }
        // Register the nonvisual keyboard owner in egui's used widget IDs.
        // This sense does not intercept clicks on breadcrumb/chooser buttons.
        let focus = ui.interact(
            ui.max_rect(),
            focus_id(),
            egui::Sense::focusable_noninteractive(),
        );
        if state.focused {
            focus.request_focus();
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    focus_id(),
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }
        let owner = self
            .project
            .as_ref()
            .map(|project| crate::run_context::ProjectOwner {
                root: project.root.clone(),
                session: self.file_session,
            });
        let mut customized_action = None;
        let crumbs = self.nav_bar_crumbs(state);
        state.crumb = state.crumb.min(crumbs.len().saturating_sub(1));
        let reveal_crumb = std::mem::take(&mut state.reveal_crumb);
        egui::ScrollArea::horizontal()
            .id_salt("navigation_bar_horizontal_scroll")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, crumb) in crumbs.iter().enumerate() {
                        if index > 0 {
                            ui.label("›");
                        }
                        let response = ui
                            .selectable_label(state.focused && state.crumb == index, &crumb.name)
                            .on_hover_text(crumb.path.display().to_string());
                        if let Some(owner) = &owner {
                            // The crumb is stamped only while its menu is open
                            // (a change closes it), not on every frame.
                            let authority =
                                egui::Id::new(("navigation-context", owner, &crumb.path));
                            crate::a11y::bound_context_menu_for_owner(
                                &response,
                                ui.is_enabled(),
                                state.focused && state.crumb == index && focus.has_focus(),
                                authority,
                            )
                            .show(|ui| {
                                if crate::run_context::open_menu_stamp(ui, &crumb.path).is_none() {
                                    return;
                                }
                                if !crate::custom_menus::render(
                                    ui,
                                    "navigation.popup",
                                    |ui, action| {
                                        if crate::custom_menus::action_button(ui, action).clicked()
                                        {
                                            customized_action = crate::custom_menus::path_action(
                                                owner,
                                                &crumb.path,
                                                action,
                                            );
                                            ui.close();
                                        }
                                    },
                                ) {
                                    for action in [
                                        Action::CopyPath,
                                        Action::FindUsages,
                                        Action::Rename,
                                        Action::LocalHistory,
                                    ] {
                                        if crate::custom_menus::action_button(ui, action).clicked()
                                        {
                                            customized_action = crate::custom_menus::path_action(
                                                owner,
                                                &crumb.path,
                                                action,
                                            );
                                            ui.close();
                                        }
                                    }
                                }
                            });
                        }
                        #[cfg(test)]
                        remember_nav_hit(ui.ctx(), crumb.path.clone(), true, response.rect);
                        if reveal_crumb && state.focused && state.crumb == index {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.clicked() {
                            state.focused = true;
                            state.crumb = index;
                            state.anchor = response.rect.left_bottom();
                            self.focus_editor = false;
                            self.nav_bar_choices(state);
                        }
                    }
                    if self.settings.chrome.navigation_members {
                        if let Some(index) = self
                            .active
                            .filter(|index| self.tabs[*index].state.language == Language::Python)
                        {
                            let members = self.tabs[index].state.breadcrumbs();
                            let mut target = None;
                            for (name, range) in members {
                                ui.label("›");
                                if ui
                                    .button(name)
                                    .on_hover_text("Navigate to containing member")
                                    .clicked()
                                {
                                    target = Some(range);
                                }
                            }
                            if let Some(range) = target {
                                self.mark_nav_jump();
                                self.active = Some(index);
                                self.tabs[index].state.reveal_offset(range.start());
                                self.focus_editor = true;
                                state.focused = false;
                                state.popup = false;
                            }
                        }
                    }
                });
            });
        if let (Some(owner), Some(action)) = (owner, customized_action) {
            self.dispatch_custom_path_menu(&owner, action, ui.ctx());
        }
    }
}

fn path_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
fn remember_nav_hit(ctx: &egui::Context, path: PathBuf, crumb: bool, rect: egui::Rect) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<(PathBuf, bool, egui::Rect)>>(egui::Id::new(
            "nav_bar_test_hits",
        ))
        .push((path, crumb, rect))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let root =
                std::env::temp_dir().join(format!("rustpy-nav-bar-{}-{n}", std::process::id()));
            std::fs::create_dir_all(root.join("pkg")).unwrap();
            for file in ["main.py", "sibling.py", "pkg/child.py"] {
                std::fs::write(root.join(file), "value = 1\n").unwrap();
            }
            Self(dunce::canonicalize(&root).unwrap())
        }
        fn app(&self) -> IdeApp {
            let mut app = IdeApp::new(Settings::default(), None);
            app.project = Some(Project {
                root: self.0.clone(),
                tree: ProjectTree::new(self.0.clone()),
                analysis: Arc::new(Analysis::for_project(&self.0)),
                run_configs: RunConfigStore::default(),
                interpreters: Vec::new(),
                interpreter: None,
                interpreter_rx: None,
                git: None,
                git_refreshed: -100.0,
                git_branch: None,
            });
            let mut editor = EditorState::from_text("value = 1\n", Language::Python);
            editor.path = Some(self.0.join("main.py"));
            app.push_tab(editor);
            app
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<(PathBuf, bool, egui::Rect)> {
        ctx.data_mut(|d| {
            d.remove::<Vec<(PathBuf, bool, egui::Rect)>>(egui::Id::new("nav_bar_test_hits"))
        });
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                app.navigation_bar_keys(ui.ctx());
                app.navigation_bar(ui);
                egui::CentralPanel::default().show(ui, |_| {});
            },
        )
        .drop_without_applying_deltas();
        ctx.data(|d| {
            d.get_temp::<Vec<(PathBuf, bool, egui::Rect)>>(egui::Id::new("nav_bar_test_hits"))
        })
        .unwrap_or_default()
    }
    fn key(app: &mut IdeApp, ctx: &egui::Context, key: egui::Key) {
        frame(
            app,
            ctx,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        frame(
            app,
            ctx,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
    }
    fn click(app: &mut IdeApp, ctx: &egui::Context, path: &Path, crumb: bool) {
        let hits = frame(app, ctx, vec![]);
        let pos = hits
            .iter()
            .find(|(p, c, _)| p == path && *c == crumb)
            .unwrap()
            .2
            .center();
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    fn active_path(app: &IdeApp) -> &Path {
        app.tabs[app.active.unwrap()].state.path.as_deref().unwrap()
    }

    #[test]
    fn pointer_opens_file_sibling_and_reveals_it_in_project_tree() {
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        click(&mut app, &ctx, &dir.0.join("main.py"), true);
        click(&mut app, &ctx, &dir.0.join("sibling.py"), false);
        assert_eq!(active_path(&app), dir.0.join("sibling.py"));
        assert_eq!(
            app.project.as_ref().unwrap().tree.selected,
            Some(dir.0.join("sibling.py"))
        );
        assert!(app.focus_editor);
        assert!(!app.nav_bar_state(&ctx).focused);
    }

    #[test]
    fn pointer_drills_into_directory_then_opens_child() {
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        click(&mut app, &ctx, &dir.0, true);
        click(&mut app, &ctx, &dir.0.join("pkg"), false);
        assert_eq!(app.nav_bar_state(&ctx).browse, Some(dir.0.join("pkg")));
        click(&mut app, &ctx, &dir.0.join("pkg/child.py"), false);
        assert_eq!(active_path(&app), dir.0.join("pkg/child.py"));
    }

    #[test]
    fn keyboard_focus_arrows_speed_search_and_enter_open_sibling() {
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        app.focus_navigation_bar(&ctx);
        key(&mut app, &ctx, egui::Key::ArrowLeft);
        assert_eq!(app.nav_bar_state(&ctx).crumb, 0);
        key(&mut app, &ctx, egui::Key::ArrowRight);
        assert_eq!(app.nav_bar_state(&ctx).crumb, 1);
        key(&mut app, &ctx, egui::Key::ArrowDown);
        frame(&mut app, &ctx, vec![egui::Event::Text("sibling".into())]);
        assert_eq!(app.nav_bar_state(&ctx).matches().len(), 1);
        key(&mut app, &ctx, egui::Key::Enter);
        assert_eq!(active_path(&app), dir.0.join("sibling.py"));
    }

    #[test]
    fn hidden_permanent_bar_has_temporary_keyboard_navigator_and_escape() {
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        app.settings.appearance.show_navigation_bar = false;
        assert!(frame(&mut app, &ctx, vec![]).is_empty());
        app.focus_navigation_bar(&ctx);
        assert!(!frame(&mut app, &ctx, vec![]).is_empty());
        key(&mut app, &ctx, egui::Key::ArrowDown);
        key(&mut app, &ctx, egui::Key::Escape);
        assert_eq!(active_path(&app), dir.0.join("main.py"));
        assert!(frame(&mut app, &ctx, vec![]).is_empty());
        assert!(app.focus_editor);
    }

    #[test]
    fn view_modes_and_welcome_suppress_bar_and_focus() {
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        for mode in [
            layout::ViewMode::DistractionFree,
            layout::ViewMode::Presentation,
        ] {
            app.layout.view_mode = mode;
            app.focus_navigation_bar(&ctx);
            assert!(frame(&mut app, &ctx, vec![]).is_empty());
            assert!(!app.nav_bar_state(&ctx).focused);
        }
        app.layout.view_mode = layout::ViewMode::Normal;
        app.project = None;
        app.focus_navigation_bar(&ctx);
        assert!(frame(&mut app, &ctx, vec![]).is_empty());
    }

    #[test]
    fn editor_pointer_click_dismisses_without_intercepting_future_keys() {
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        app.focus_navigation_bar(&ctx);
        key(&mut app, &ctx, egui::Key::ArrowDown);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerButton {
                pos: egui::pos2(900.0, 600.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(!app.nav_bar_state(&ctx).focused);
        assert!(!app.navigation_bar_keys(&ctx));
        assert_eq!(active_path(&app), dir.0.join("main.py"));
    }

    #[test]
    fn manual_wheel_offset_survives_idle_frames_and_keyboard_reveals_selection() {
        let dir = Scratch::new();
        for n in 0..100 {
            std::fs::write(dir.0.join(format!("zfile_{n:03}.py")), "value = 1\n").unwrap();
        }
        let mut app = dir.app();
        let ctx = egui::Context::default();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("nav_bar_test_wheel_diagnostics"), true));
        app.focus_navigation_bar(&ctx);
        key(&mut app, &ctx, egui::Key::ArrowDown);
        for _ in 0..20 {
            frame(&mut app, &ctx, vec![]);
        }
        let scroll = |ctx: &egui::Context| {
            ctx.data(|d| d.get_temp::<(f32, egui::Rect, f32)>(egui::Id::new("nav_bar_test_scroll")))
                .unwrap()
        };
        let (initial_offset, viewport, content_height) = scroll(&ctx);
        assert!(
            content_height > viewport.height() * 3.0,
            "fixture must need scrolling"
        );
        assert!(
            initial_offset < 100.0,
            "selected main.py begins near the top"
        );
        // egui reports the pre-shrink inner rect, while wheel hit testing uses
        // the area shrunk to content width. Use a real visible row's x position.
        let row_x = ctx
            .data(|d| {
                d.get_temp::<Vec<(PathBuf, bool, egui::Rect)>>(egui::Id::new("nav_bar_test_hits"))
            })
            .unwrap()
            .iter()
            .find(|(path, crumb, rect)| {
                !crumb && *path == dir.0.join("main.py") && viewport.intersects(*rect)
            })
            .unwrap()
            .2
            .center()
            .x;
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(egui::pos2(row_x, viewport.center().y)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -500.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        // Let egui's smooth wheel motion finish before measuring persistence.
        for _ in 0..40 {
            frame(&mut app, &ctx, vec![]);
        }
        let manual_offset = scroll(&ctx).0;
        assert!(
            manual_offset > 200.0,
            "wheel must move away from selected row: {manual_offset}"
        );
        for _ in 0..12 {
            frame(&mut app, &ctx, vec![]);
        }
        assert!(
            (scroll(&ctx).0 - manual_offset).abs() < 1.0,
            "idle render must preserve manual browsing offset"
        );
        key(&mut app, &ctx, egui::Key::End);
        for _ in 0..40 {
            frame(&mut app, &ctx, vec![]);
        }
        let (keyboard_offset, viewport, _) = scroll(&ctx);
        let state = app.nav_bar_state(&ctx);
        let entries = state.matches();
        assert_eq!(state.selected, entries.len() - 1);
        assert!(
            keyboard_offset > manual_offset + 200.0,
            "keyboard must reveal distant selection"
        );
        let hits = frame(&mut app, &ctx, vec![]);
        let row = hits
            .iter()
            .find(|(path, crumb, _)| !crumb && path == &entries[state.selected].path)
            .unwrap()
            .2;
        assert!(
            viewport.intersects(row),
            "keyboard-selected row must be visible"
        );
    }

    #[test]
    fn settings_modal_releases_navigation_keyboard_without_consuming_text() {
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        app.focus_navigation_bar(&ctx);
        app.dialog = Dialog::Settings;
        ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Text("settings search".into())],
                ..Default::default()
            },
            |ui| {
                assert!(!app.navigation_bar_keys(ui.ctx()));
                assert!(ui
                    .input(|i| i.events.iter().any(
                        |e| matches!(e, egui::Event::Text(text) if text == "settings search")
                    )));
            },
        )
        .drop_without_applying_deltas();
        assert!(!app.nav_bar_state(&ctx).focused);
        app.focus_navigation_bar(&ctx);
        assert!(!app.nav_bar_state(&ctx).focused);
    }

    #[test]
    fn default_alt_home_dispatch_opens_sibling_and_escape_returns_editor() {
        fn dispatch(
            app: &mut IdeApp,
            ctx: &egui::Context,
            events: Vec<egui::Event>,
            modifiers: egui::Modifiers,
        ) -> Vec<egui::Event> {
            let mut remaining = Vec::new();
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 720.0),
                    )),
                    events: std::iter::once(egui::Event::ModifiersChanged(modifiers))
                        .chain(events)
                        .collect(),
                    ..Default::default()
                },
                |ui| {
                    // Exercise the registered action and default keymap, never call
                    // focus_navigation_bar directly in this integration fixture.
                    app.global_shortcuts(ui);
                    remaining = ui.input(|i| i.events.clone());
                    app.navigation_bar(ui);
                    egui::CentralPanel::default().show(ui, |_| {});
                },
            )
            .drop_without_applying_deltas();
            remaining
        }
        fn dispatch_key(
            app: &mut IdeApp,
            ctx: &egui::Context,
            key: egui::Key,
            modifiers: egui::Modifiers,
        ) -> Vec<egui::Event> {
            let mut remaining = Vec::new();
            for pressed in [true, false] {
                let events = dispatch(
                    app,
                    ctx,
                    vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    }],
                    modifiers,
                );
                if pressed {
                    remaining = events;
                }
            }
            remaining
        }
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        app.settings.appearance.show_navigation_bar = false;
        let bottom_before = app.bottom;
        dispatch(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        dispatch_key(&mut app, &ctx, egui::Key::Home, egui::Modifiers::ALT);
        assert!(
            app.nav_bar_state(&ctx).focused,
            "default Alt+Home must dispatch navigation action"
        );
        assert!(!app.focus_editor);
        dispatch_key(&mut app, &ctx, egui::Key::ArrowDown, egui::Modifiers::NONE);
        dispatch(
            &mut app,
            &ctx,
            vec![egui::Event::Text("sibling".into())],
            egui::Modifiers::NONE,
        );
        dispatch_key(&mut app, &ctx, egui::Key::Enter, egui::Modifiers::NONE);
        assert_eq!(active_path(&app), dir.0.join("sibling.py"));
        assert!(app.focus_editor);
        assert!(!app.nav_bar_state(&ctx).focused);

        dispatch_key(&mut app, &ctx, egui::Key::Home, egui::Modifiers::ALT);
        dispatch_key(&mut app, &ctx, egui::Key::ArrowDown, egui::Modifiers::NONE);
        let events = dispatch_key(&mut app, &ctx, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(
            !events.iter().any(|e| matches!(
                e,
                egui::Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                }
            )),
            "navigation must consume Escape before another IDE action sees it"
        );
        assert!(!app.nav_bar_state(&ctx).focused);
        assert!(app.focus_editor);
        assert_eq!(active_path(&app), dir.0.join("sibling.py"));
        assert_eq!(app.bottom, bottom_before);
        assert!(matches!(app.dialog, Dialog::None));
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            "value = 1\n"
        );
    }

    #[test]
    fn registered_navigation_focus_persists_and_real_editor_ignores_owned_keys() {
        fn editor_frame(
            app: &mut IdeApp,
            ctx: &egui::Context,
            events: Vec<egui::Event>,
            modifiers: egui::Modifiers,
        ) -> Vec<crate::editor::view::ViewEvent> {
            let mut editor_events = Vec::new();
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 720.0),
                    )),
                    events: std::iter::once(egui::Event::ModifiersChanged(modifiers))
                        .chain(events)
                        .collect(),
                    ..Default::default()
                },
                |ui| {
                    app.global_shortcuts(ui);
                    app.navigation_bar(ui);
                    egui::CentralPanel::default().show(ui, |ui| {
                        let opts = crate::editor::state::EditOptions::default();
                        let request_focus = std::mem::take(&mut app.focus_editor);
                        let cfg = crate::editor::view::ViewConfig {
                            scheme: &app.scheme,
                            theme: &app.theme,
                            settings: &app.settings.editor,
                            opts: &opts,
                            keymap: &app.keymap,
                            font_size: 14.0,
                            search_highlights: &[],
                            request_focus,
                            run_lines: &[],
                            line_changes: &[],
                            breakpoints: &[],
                            exec_line: None,
                            inline_values: &[],
                            bookmarks: &[],
                            gutter_markers: &[],
                            gutter_quick_fixes: &[],
                            code_vision: None,
                            annotations: None,
                            coverage: &[],
                            // Keep normal editor keyboard processing enabled: the
                            // registered navigator must own focus itself.
                            keyboard: true,
                        };
                        let tab = &mut app.tabs[app.active.unwrap()];
                        editor_events =
                            crate::editor::view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                    });
                },
            )
            .drop_without_applying_deltas();
            editor_events
        }
        fn event(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
            egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            }
        }
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        editor_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE);
        assert!(
            app.tabs[app.active.unwrap()].view.focused,
            "fixture must begin with a focused real editor"
        );
        let before = app.tabs[app.active.unwrap()].state.doc.text();
        let caret_before = app.tabs[app.active.unwrap()].state.primary_position();
        let events = editor_frame(
            &mut app,
            &ctx,
            vec![event(egui::Key::Home, true, egui::Modifiers::ALT)],
            egui::Modifiers::ALT,
        );
        assert!(events.is_empty());
        editor_frame(
            &mut app,
            &ctx,
            vec![event(egui::Key::Home, false, egui::Modifiers::ALT)],
            egui::Modifiers::ALT,
        );
        for _ in 0..6 {
            assert!(editor_frame(&mut app, &ctx, vec![], egui::Modifiers::NONE).is_empty());
            assert_eq!(
                ctx.memory(|m| m.focused()),
                Some(focus_id()),
                "navigation widget must remain registered after end_pass"
            );
            assert!(!app.tabs[app.active.unwrap()].view.focused);
        }
        for (key, modifiers) in [
            (egui::Key::Delete, egui::Modifiers::NONE),
            (egui::Key::Tab, egui::Modifiers::NONE),
            (egui::Key::Delete, egui::Modifiers::CTRL),
            (egui::Key::A, egui::Modifiers::CTRL),
            (egui::Key::ArrowRight, egui::Modifiers::SHIFT),
            (egui::Key::Enter, egui::Modifiers::CTRL),
        ] {
            for pressed in [true, false] {
                let routed = editor_frame(
                    &mut app,
                    &ctx,
                    vec![event(key, pressed, modifiers)],
                    modifiers,
                );
                assert!(
                    routed.is_empty(),
                    "owned {key:?}/{modifiers:?} must not route editor events: {routed:?}"
                );
                assert_eq!(ctx.memory(|m| m.focused()), Some(focus_id()));
                let tab = &app.tabs[app.active.unwrap()];
                assert!(!tab.view.focused);
                assert_eq!(tab.state.doc.text(), before);
                assert_eq!(tab.state.primary_position(), caret_before);
                assert!(!tab.state.has_selection());
            }
        }
    }

    #[test]
    fn unreadable_directory_reports_error_and_project_switch_resets_state() {
        let dir = Scratch::new();
        let mut app = dir.app();
        let ctx = egui::Context::default();
        app.focus_navigation_bar(&ctx);
        let mut state = app.nav_bar_state(&ctx);
        state.browse = Some(dir.0.join("missing/child.py"));
        state.crumb = 2;
        app.nav_bar_choices(&mut state);
        assert!(state.error.as_deref().unwrap().contains("Cannot list"));
        state.save(&ctx);
        app.project.as_mut().unwrap().root = dir.0.join("pkg");
        assert!(!app.nav_bar_state(&ctx).focused);
    }
}
