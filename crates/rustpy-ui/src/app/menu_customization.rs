//! Ordered menu layouts use the existing command registry and contextual providers.
use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_settings::menus::{Entry, LOCATIONS};
#[derive(Default)]
pub(super) struct MenuCustomization {
    location: String,
    selected: Vec<usize>,
    filter: String,
    action: String,
    group: String,
    background: crate::appearance_assets::BackgroundAsset,
}
fn group_mut<'a>(entries: &'a mut Vec<Entry>, path: &[usize]) -> Option<&'a mut Vec<Entry>> {
    if path.is_empty() {
        return Some(entries);
    }
    match entries.get_mut(path[0])? {
        Entry::Group { children, .. } => group_mut(children, &path[1..]),
        _ => None,
    }
}
fn entry<'a>(entries: &'a [Entry], path: &[usize]) -> Option<&'a Entry> {
    let (first, rest) = path.split_first()?;
    let item = entries.get(*first)?;
    if rest.is_empty() {
        Some(item)
    } else if let Entry::Group { children, .. } = item {
        entry(children, rest)
    } else {
        None
    }
}
fn insert(entries: &mut Vec<Entry>, selected: &[usize], value: Entry) -> Vec<usize> {
    let (mut parent, index) = if matches!(entry(entries, selected), Some(Entry::Group { .. })) {
        (selected.to_vec(), usize::MAX)
    } else if let Some((index, parent)) = selected.split_last() {
        (parent.to_vec(), index + 1)
    } else {
        (Vec::new(), usize::MAX)
    };
    let Some(items) = group_mut(entries, &parent) else {
        return Vec::new();
    };
    let index = index.min(items.len());
    items.insert(index, value);
    parent.push(index);
    parent
}
fn reparent(entries: &mut Vec<Entry>, selected: &[usize], inward: bool) -> Vec<usize> {
    let Some((&index, parent)) = selected.split_last() else {
        return selected.to_vec();
    };
    if inward {
        let Some(items) = group_mut(entries, parent) else {
            return selected.to_vec();
        };
        if index == 0 || index >= items.len() || !matches!(items[index - 1], Entry::Group { .. }) {
            return selected.to_vec();
        }
        let item = items.remove(index);
        let Entry::Group { children, .. } = &mut items[index - 1] else {
            unreachable!()
        };
        let child = children.len();
        children.push(item);
        let mut path = parent.to_vec();
        path.extend([index - 1, child]);
        path
    } else {
        let Some((&group, outer)) = parent.split_last() else {
            return selected.to_vec();
        };
        let Some(items) = group_mut(entries, outer) else {
            return selected.to_vec();
        };
        let Some(Entry::Group { children, .. }) = items.get_mut(group) else {
            return selected.to_vec();
        };
        if index >= children.len() {
            return selected.to_vec();
        }
        let item = children.remove(index);
        items.insert(group + 1, item);
        let mut path = outer.to_vec();
        path.push(group + 1);
        path
    }
}
fn rows(
    ui: &mut egui::Ui,
    entries: &[Entry],
    selected: &mut Vec<usize>,
    path: &mut Vec<usize>,
    budget: &mut usize,
) {
    if path.len() > 8 {
        return;
    }
    for (index, item) in entries.iter().enumerate() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        path.push(index);
        let label = match item {
            Entry::Action { id, icon } => Keymap::action_by_id(id)
                .map(|a| format!("{icon} {}", a.text()))
                .unwrap_or_else(|| format!("Unavailable: {id}")),
            Entry::Separator => "────────".into(),
            Entry::Group { label, .. } => format!("▸ {label}"),
        };
        ui.indent(egui::Id::new(("menu-layout-row", path.clone())), |ui| {
            if ui.selectable_label(*selected == *path, label).clicked() {
                *selected = path.clone();
            }
            if let Entry::Group { children, .. } = item {
                rows(ui, children, selected, path, budget);
            }
        });
        path.pop();
    }
}
impl IdeApp {
    pub(super) fn appearance_asset_jobs(&self) -> Vec<(u64, String)> {
        self.menu_customization.background.jobs()
    }
    pub(super) fn stop_appearance_asset(&mut self, id: u64) {
        self.menu_customization.background.stop(id);
    }
    pub(super) fn cancel_appearance_asset(&mut self) {
        self.menu_customization.background.cancel();
    }
    pub(super) fn retire_appearance_asset(&mut self, ctx: &egui::Context) -> bool {
        self.menu_customization.background.retire(ctx)
    }
    pub(super) fn menus_frame(&mut self, ctx: &egui::Context) {
        let aa = &self.settings.menus.appearance;
        eframe::text_rendering::begin_frame(
            ctx,
            aa.ide_text_antialiasing.coverage(aa.subpixel_bgr),
            aa.editor_text_antialiasing.coverage(aa.subpixel_bgr),
        );
        if self.settings.menus.appearance.sync_with_os {
            if let Some(theme) = ctx.system_theme() {
                let name = if theme == egui::Theme::Dark {
                    "Dark"
                } else {
                    "Light"
                };
                if self.settings.appearance.theme != name {
                    self.set_ui_theme(name, ctx);
                }
            }
        }
        if !self.exit_requested {
            self.menu_customization
                .background
                .update(ctx, &self.settings.menus.appearance.background);
        }
        let merge_key = egui::Id::new(("native-project-window-merge", ctx.viewport_id()));
        let merge_available = self.native_project_window_merge_available();
        ctx.data_mut(|data| data.insert_temp(merge_key, merge_available));
        crate::custom_menus::configure(
            ctx,
            &self.settings.menus,
            &self.keymap,
            self.component_menu_owner_available(false),
        );
        if ctx
            .with_plugin::<crate::custom_menus::AppearancePlugin, _>(|_| ())
            .is_none()
        {
            ctx.add_plugin(crate::custom_menus::AppearancePlugin);
        }
    }
    pub(super) fn page_menus_toolbars(&mut self, ui: &mut egui::Ui) {
        ui.heading(crate::i18n::tr_static("Menus and Toolbars"));
        if ui
            .button(crate::i18n::tr_static(
                "Configure main toolbar and navigation bar…",
            ))
            .clicked()
        {
            self.settings_ui.page = settings_ui::SettingsPage::WindowChrome;
            return;
        }
        let state = &mut self.menu_customization;
        if state.location.is_empty() {
            state.location = "main.file".into();
        }
        egui::ComboBox::from_id_salt("menu-location")
            .selected_text(crate::i18n::tr(
                LOCATIONS
                    .iter()
                    .find(|(id, _)| *id == state.location)
                    .map_or(state.location.as_str(), |(_, label)| *label),
            ))
            .show_ui(ui, |ui| {
                for (id, label) in LOCATIONS {
                    if ui
                        .selectable_value(&mut state.location, (*id).into(), crate::i18n::tr(label))
                        .changed()
                    {
                        state.selected.clear();
                        state.action.clear();
                    }
                }
            })
            .combo_named(crate::i18n::tr_static("Menu location"));
        let location = state.location.clone();
        let custom = self.settings.menus.overrides.contains_key(&location);
        if !custom {
            ui.label(crate::i18n::tr_static(
                "This menu uses its original provider, including dynamic project and plugin items.",
            ));
            if ui
                .button(crate::i18n::tr_static("Customize this menu"))
                .clicked()
            {
                self.settings
                    .menus
                    .overrides
                    .insert(location.clone(), crate::custom_menus::defaults(&location));
            }
            return;
        }
        ui.small(crate::i18n::tr_static("A custom layout replaces this menu. Restore defaults to include dynamic provider items."));
        ui.horizontal(|ui| {
            if ui
                .button(crate::i18n::tr_static("Restore defaults"))
                .clicked()
            {
                self.settings.menus.overrides.remove(&location);
                state.selected.clear();
            }
            if ui
                .button(crate::i18n::tr_static("Restore all menu defaults"))
                .clicked()
            {
                self.settings.menus.overrides.clear();
                state.selected.clear();
            }
        });
        let Some(entries) = self.settings.menus.overrides.get_mut(&location) else {
            return;
        };
        if let Err(error) = rustpy_settings::menus::validate(entries) {
            ui.colored_label(self.theme.error, error);
        }
        egui::ScrollArea::vertical()
            .id_salt("menu-layout-tree")
            .max_height(280.0)
            .show(ui, |ui| {
                rows(ui, entries, &mut state.selected, &mut Vec::new(), &mut 512)
            });
        ui.horizontal(|ui| {
            if ui
                .button(crate::i18n::tr_static("Select menu root"))
                .clicked()
            {
                state.selected.clear();
            }
            for (direction, label) in [(-1, "Move up"), (1, "Move down")] {
                if ui.button(crate::i18n::tr(label)).clicked() {
                    if let Some((index, parent)) = state.selected.split_last() {
                        let index = *index;
                        let parent = parent.to_vec();
                        if let Some(items) = group_mut(entries, &parent) {
                            let target = if direction < 0 {
                                index.saturating_sub(1)
                            } else {
                                index.saturating_add(1)
                            };
                            if index < items.len() && target < items.len() {
                                items.swap(index, target);
                                state.selected = parent;
                                state.selected.push(target);
                            }
                        }
                    }
                }
            }
            for (inward, label) in [
                (true, "Move into previous group"),
                (false, "Move out of group"),
            ] {
                if ui.button(crate::i18n::tr(label)).clicked() {
                    state.selected = reparent(entries, &state.selected, inward);
                }
            }
            if ui
                .button(crate::i18n::tr_static("Remove selected item"))
                .clicked()
            {
                if let Some((index, parent)) = state.selected.split_last() {
                    let index = *index;
                    let parent = parent.to_vec();
                    if let Some(items) = group_mut(entries, &parent) {
                        if index < items.len() {
                            items.remove(index);
                        }
                    }
                    state.selected = parent;
                }
            }
        });
        if let Some((index, parent)) = state.selected.split_last() {
            if let Some(items) = group_mut(entries, parent) {
                if let Some(item) = items.get_mut(*index) {
                    match item {
                        Entry::Action { id, icon } => {
                            ui.monospace(id.as_str());
                            ui.horizontal(|ui| {
                                ui.label(crate::i18n::tr_static("Icon (up to eight characters)"));
                                ui.text_edit_singleline(icon);
                            });
                        }
                        Entry::Group { label, .. } => {
                            ui.label(crate::i18n::tr_static("Group name"));
                            ui.text_edit_singleline(label);
                        }
                        _ => {}
                    }
                }
            }
        }
        ui.separator();
        ui.label(crate::i18n::tr_static("Find action"));
        ui.text_edit_singleline(&mut state.filter);
        egui::ComboBox::from_id_salt("menu-action-picker")
            .selected_text(
                Keymap::action_by_id(&state.action)
                    .map_or_else(|| "Choose action".into(), |a| a.text().to_string()),
            )
            .height(240.0)
            .show_ui(ui, |ui| {
                for &action in Action::ALL {
                    if action.id() == "MergeAllProjectWindows" && !cfg!(target_os = "macos") {
                        continue;
                    }
                    if location == "run.popup" && !crate::custom_menus::run_action_allowed(action) {
                        continue;
                    }
                    if crate::i18n::matches_filter(action.english_text(), &state.filter) {
                        ui.selectable_value(&mut state.action, action.id().into(), action.text());
                    }
                }
            })
            .combo_named(crate::i18n::tr_static("Action"));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    Keymap::action_by_id(&state.action).is_some(),
                    egui::Button::new(crate::i18n::tr_static("Add action")),
                )
                .clicked()
            {
                state.selected = insert(
                    entries,
                    &state.selected,
                    Entry::Action {
                        id: state.action.clone(),
                        icon: String::new(),
                    },
                );
            }
            if ui.button(crate::i18n::tr_static("Add separator")).clicked() {
                state.selected = insert(entries, &state.selected, Entry::Separator);
            }
        });
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut state.group);
            if ui
                .add_enabled(
                    !state.group.trim().is_empty(),
                    egui::Button::new(crate::i18n::tr_static("Add group")),
                )
                .clicked()
            {
                state.selected = insert(
                    entries,
                    &state.selected,
                    Entry::Group {
                        label: state.group.trim().into(),
                        children: Vec::new(),
                    },
                );
                state.group.clear();
            }
        });
        if let Err(error) = rustpy_settings::menus::validate(entries) {
            ui.colored_label(self.theme.error, error);
        }
    }
    pub(super) fn appearance_extras_page(&mut self, ui: &mut egui::Ui) {
        use crate::i18n::tr_static as tr;
        let p = &mut self.settings.menus.appearance;
        ui.heading(tr("Theme and tooltips"));
        ui.checkbox(&mut p.sync_with_os, tr("Sync theme with operating system"));
        if p.sync_with_os && ui.ctx().system_theme().is_none() {
            ui.small(tr(
                "The operating system has not provided a theme; the current theme is retained.",
            ));
        }
        ui.add(
            egui::Slider::new(&mut p.tooltip_delay, 0.0..=5.0)
                .text(tr("Tooltip initial delay (seconds)")),
        );
        ui.checkbox(&mut p.tooltip_shortcuts, tr("Show tooltips with shortcuts"));
        ui.checkbox(
            &mut p.menu_mnemonics,
            tr("Enable main-menu mnemonics (Alt+F, Alt+E, …)"),
        );
        ui.checkbox(&mut p.menu_icons, tr("Display configured icons in menus"));
        ui.checkbox(&mut p.smooth_shapes, tr("Anti-alias interface shapes"));
        ui.checkbox(
            &mut p.pixel_aligned_text,
            tr("Align text to physical pixels"),
        );
        use rustpy_settings::menus::TextAntialiasing;
        for (id, label, value) in [
            (
                "ide-text-aa",
                "IDE text anti-aliasing",
                &mut p.ide_text_antialiasing,
            ),
            (
                "editor-text-aa",
                "Editor text anti-aliasing",
                &mut p.editor_text_antialiasing,
            ),
        ] {
            egui::ComboBox::from_id_salt(id)
                .selected_text(tr(match value {
                    TextAntialiasing::Grayscale => "Grayscale",
                    TextAntialiasing::None => "None",
                    TextAntialiasing::Subpixel => "Subpixel",
                }))
                .show_ui(ui, |ui| {
                    ui.label(tr(label));
                    for (mode, label) in [
                        (TextAntialiasing::Grayscale, "Grayscale"),
                        (TextAntialiasing::None, "None"),
                        (TextAntialiasing::Subpixel, "Subpixel"),
                    ] {
                        ui.selectable_value(value, mode, tr(label));
                    }
                })
                .combo_named(tr(label).as_ref());
        }
        ui.checkbox(&mut p.subpixel_bgr, tr("BGR horizontal subpixel order"));
        ui.small(tr("Subpixel uses filtered RGB/BGR coverage from the font atlas. Use only with a matching horizontal-stripe display. Rotated, translucent text and transparent windows use grayscale; color emoji keep their colors."));
        if !ui.ctx().data(|data| {
            data.get_temp::<bool>(egui::Id::new("rustpy-glow-text-coverage"))
                .unwrap_or(false)
        }) {
            ui.small(tr("Text coverage modes require the native Glow renderer. Other renderers retain grayscale."));
        }
        ui.checkbox(
            &mut p.tree_indent_guides,
            tr("Show project-tree indent guides"),
        );
        ui.checkbox(
            &mut p.compact_tree_indents,
            tr("Use compact project-tree indents"),
        );
        ui.add(
            egui::Slider::new(&mut p.presentation_font_size, 16.0..=60.0)
                .text(tr("Presentation mode font size")),
        );
        ui.collapsing(tr("Background image"), |ui| {
            ui.label(tr("Local image path"));
            ui.text_edit_singleline(&mut p.background.path);
            ui.add(egui::Slider::new(&mut p.background.opacity, 0.0..=1.0).text(tr("Opacity")));
            egui::ComboBox::from_id_salt("background-placement")
                .selected_text(&p.background.placement)
                .show_ui(ui, |ui| {
                    for placement in ["Fit", "Fill", "Stretch", "Tile"] {
                        ui.selectable_value(
                            &mut p.background.placement,
                            placement.into(),
                            crate::i18n::tr(placement),
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Image placement"));
            ui.checkbox(&mut p.background.editor, tr("Editor background"));
            ui.checkbox(&mut p.background.empty_frame, tr("Empty-frame background"));
            if self.menu_customization.background.loading() {
                crate::a11y::spinner(ui);
                if ui.button(tr("Cancel image loading")).clicked() {
                    self.menu_customization.background.cancel();
                }
            }
            if let Some(error) = &self.menu_customization.background.error {
                ui.colored_label(self.theme.error, error);
            }
            if ui.button(tr("Reload background image")).clicked() {
                self.menu_customization.background.retry();
            }
        });
        self.native_ui_settings(ui);
        self.platform_settings(ui);
        if ui.button(tr("Customize menus and toolbars…")).clicked() {
            self.settings_ui.page = settings_ui::SettingsPage::MenusToolbars;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn insertion_and_reordering_keep_exact_parent_and_ids() {
        let mut entries = vec![Entry::Group {
            label: "Tests".into(),
            children: Vec::new(),
        }];
        let selected = insert(
            &mut entries,
            &[0],
            Entry::Action {
                id: Action::Run.id().into(),
                icon: String::new(),
            },
        );
        assert_eq!(selected, vec![0, 0]);
        let next = insert(&mut entries, &selected, Entry::Separator);
        assert_eq!(next, vec![0, 1]);
        assert!(matches!(entry(&entries,&[0,0]),Some(Entry::Action{id,..})if id==Action::Run.id()));
    }
}
impl IdeApp {
    pub(super) fn custom_run_menu_action(&mut self, id: u64, action: Action) {
        if !self
            .run_panel
            .sessions
            .iter()
            .any(|session| session.id == id)
        {
            return;
        }
        let event = match action {
            Action::Run | Action::Rerun => RunPanelEvent::Rerun(id),
            Action::Stop => RunPanelEvent::Stop(id),
            Action::CloseEditor => RunPanelEvent::Close(id),
            Action::ToggleServicesView => RunPanelEvent::Services(id),
            Action::HideActiveToolWindow => RunPanelEvent::Hide,
            Action::Settings => {
                self.open_settings_page("appearance");
                return;
            }
            _ => return,
        };
        self.handle_run_panel_event(event);
    }
}

impl IdeApp {
    pub(super) fn dispatch_custom_path_menu(
        &mut self,
        owner: &crate::run_context::ProjectOwner,
        action: crate::project_tree::TreeAction,
        ctx: &egui::Context,
    ) {
        if self.file_session != owner.session
            || self
                .project
                .as_ref()
                .is_none_or(|project| project.root != owner.root)
        {
            return;
        }
        match action {
            crate::project_tree::TreeAction::Invoke(path, action) => {
                self.project_path_action(path, action, ctx)
            }
            crate::project_tree::TreeAction::ContextRun(request) => {
                self.project_context_request(*request)
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    #[test]
    fn reparenting_preserves_exact_items_and_selection() {
        let mut items = vec![
            Entry::Group {
                label: "Tools".into(),
                children: vec![],
            },
            Entry::Action {
                id: "Save".into(),
                icon: String::new(),
            },
        ];
        let inside = reparent(&mut items, &[1], true);
        assert_eq!(inside, vec![0, 0]);
        let outside = reparent(&mut items, &inside, false);
        assert_eq!(outside, vec![1]);
        assert!(matches!(&items[1], Entry::Action { id, .. } if id == "Save"));
    }
    #[test]
    fn retired_project_cannot_dispatch_a_captured_navigation_action() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        let owner = crate::run_context::ProjectOwner {
            root: PathBuf::from("/retired/project"),
            session: app.file_session.wrapping_sub(1),
        };
        let previous = app.settings_ui.page;
        app.dispatch_custom_path_menu(
            &owner,
            crate::project_tree::TreeAction::Invoke(
                PathBuf::from("/retired/project/main.py"),
                Action::Settings,
            ),
            &egui::Context::default(),
        );
        assert_eq!(app.settings_ui.page, previous);
        assert!(matches!(app.dialog, Dialog::None));
    }
}
