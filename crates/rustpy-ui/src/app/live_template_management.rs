//! Connected management of live-template definitions; expansion stays in the
//! existing editor/session pipeline.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use rustpy_templates::management::{self, ExpandKey};
use rustpy_templates::TemplateVariable;

#[derive(Default)]
pub(super) struct LiveTemplateManagement {
    draft: Option<LiveTemplate>,
    original: Option<String>,
    selected: Option<String>,
    transfer_path: String,
    message: Option<String>,
    pub expand_key: ExpandKey,
    loaded: bool,
    pub choices: Vec<management::VariableChoices>,
    pub choice_tab: Option<egui::Id>,
    pub dynamic_variables: Vec<(String, String)>,
    choice_index: usize,
    choice_variable: Option<String>,
}

impl LiveTemplateManagement {
    pub(super) fn load() -> Self {
        Self {
            expand_key: std::fs::read(Settings::config_dir().join("live_template_expansion.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default(),
            loaded: true,
            ..Default::default()
        }
    }
}

impl IdeApp {
    pub(super) fn save_selection_as_live_template(&mut self) {
        let Some(i) = self.active else { return };
        let selection = self.tabs[i].state.primary().range();
        if selection.is_empty() {
            self.set_status("Select text to save as a live template");
            return;
        }
        let text = self.tabs[i].state.doc.slice(selection);
        let context = self.template_context(i);
        self.live_template_management.draft = Some(management::from_selection(&text, context));
        self.live_template_management.original = None;
        self.dialog = Dialog::Settings;
        self.settings_ui.page = settings_ui::SettingsPage::LiveTemplates;
    }

    pub(super) fn live_template_management_page(&mut self, ui: &mut egui::Ui) {
        let path = Settings::config_dir().join("live_templates.json");
        let prefs = Settings::config_dir().join("live_template_expansion.json");
        let mut page = std::mem::take(&mut self.live_template_management);
        if !page.loaded {
            page.expand_key = std::fs::read(&prefs)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            page.loaded = true;
        }
        ui.horizontal(|ui| {
            let expand_label = ui.label("Expand with");
            let old = page.expand_key;
            egui::ComboBox::from_id_salt("live-template-expand-key")
                .selected_text(page.expand_key.label())
                .show_ui(ui, |ui| {
                    for key in ExpandKey::ALL {
                        ui.selectable_value(&mut page.expand_key, key, key.label());
                    }
                })
                .combo_labelled(&expand_label);
            if page.expand_key != old {
                match serde_json::to_vec(&page.expand_key)
                    .map_err(std::io::Error::other)
                    .and_then(|bytes| publish(&prefs, &bytes))
                {
                    Ok(()) => {}
                    Err(e) => {
                        page.expand_key = old;
                        page.message = Some(format!("Cannot save expansion key: {e}"));
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Add").clicked() {
                page.draft = Some(LiveTemplate::new(
                    "abbr",
                    "New template",
                    "$END$",
                    &[TemplateContext::Any],
                    "user",
                ));
                page.original = None;
            }
            if ui
                .add_enabled(page.selected.is_some(), egui::Button::new("Edit"))
                .clicked()
            {
                page.draft = self
                    .templates
                    .iter()
                    .find(|t| Some(&t.abbreviation) == page.selected.as_ref())
                    .cloned();
                page.original = page.selected.clone();
            }
            if ui
                .add_enabled(
                    page.selected.is_some(),
                    egui::Button::new("Remove / Restore builtin"),
                )
                .clicked()
            {
                let result = update_store(&path, |user| {
                    user.retain(|t| Some(&t.abbreviation) != page.selected.as_ref());
                });
                match result {
                    Ok(()) => {
                        self.templates = super::load_templates();
                        page.selected = None;
                    }
                    Err(e) => page.message = Some(e),
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Import/export file");
            ui.text_edit_singleline(&mut page.transfer_path);
            if ui.button("Import").clicked() {
                let result = read_import(Path::new(&page.transfer_path)).and_then(|imported| {
                    update_store(&path, |user| {
                        *user = management::merge_import(user, &imported);
                    })
                });
                match result {
                    Ok(()) => {
                        self.templates = super::load_templates();
                        page.message = Some("Templates imported".into());
                    }
                    Err(e) => page.message = Some(e),
                }
            }
            if ui.button("Export").clicked() {
                let result = read_user(&path)
                    .and_then(|user| management::export_json(&user))
                    .and_then(|text| {
                        publish(Path::new(&page.transfer_path), text.as_bytes())
                            .map_err(|e| e.to_string())
                    });
                page.message = Some(match result {
                    Ok(()) => "User templates exported".into(),
                    Err(e) => e,
                });
            }
        });
        if let Some(message) = &page.message {
            ui.label(message);
        }
        if let Some(mut draft) = page.draft.take() {
            ui.separator();
            egui::Grid::new("managed-live-template")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Abbreviation");
                    ui.text_edit_singleline(&mut draft.abbreviation);
                    ui.end_row();
                    ui.label("Description");
                    ui.text_edit_singleline(&mut draft.description);
                    ui.end_row();
                    ui.label("Group");
                    ui.text_edit_singleline(&mut draft.group);
                    ui.end_row();
                    ui.label("Template text");
                    ui.add(
                        egui::TextEdit::multiline(&mut draft.template)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(6),
                    );
                    ui.end_row();
                });
            ui.label("Applicable contexts");
            ui.horizontal_wrapped(|ui| {
                for (label, context) in contexts() {
                    let mut checked = draft.contexts.contains(&context);
                    if ui.checkbox(&mut checked, label).changed() {
                        draft.contexts.retain(|c| *c != context);
                        if checked {
                            draft.contexts.push(context);
                        }
                    }
                }
            });
            ui.collapsing("Edit variables",|ui| {
                let names=draft.variable_names();
                draft.variables.retain(|v|names.contains(&v.name));
                for name in names { if !draft.variables.iter().any(|v|v.name==name){draft.variables.push(TemplateVariable::new(&name,"",""));} }
                egui::Grid::new("live-template-variables").striped(true).show(ui,|ui| {
                    for heading in ["Name","Expression","Default value","Always stop"]{ui.label(crate::i18n::tr_static(heading));}ui.end_row();
                    for variable in &mut draft.variables {ui.label(&variable.name);ui.text_edit_singleline(&mut variable.expression);ui.text_edit_singleline(&mut variable.default);ui.checkbox(&mut variable.always_stop_at,"");ui.end_row();}
                });
                ui.small("Functions include complete(), enum(\"first\", \"second\"), regularExpression(TEXT, \"pattern\", \"replacement\"), pyIterableVariable(), suggestVariableName(), fileName(), className(), date(), time(), user() and clipboard().");
            });
            let validation = management::validate(&draft);
            if let Err(error) = &validation {
                ui.colored_label(egui::Color32::RED, error);
            }
            let mut keep = true;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(validation.is_ok(), egui::Button::new("Save template"))
                    .clicked()
                {
                    let result = update_store(&path, |user| {
                        user.retain(|t| {
                            Some(&t.abbreviation) != page.original.as_ref()
                                && t.abbreviation != draft.abbreviation
                        });
                        user.push(draft.clone());
                    });
                    match result {
                        Ok(()) => {
                            self.templates = super::load_templates();
                            page.selected = Some(draft.abbreviation.clone());
                            keep = false;
                        }
                        Err(e) => page.message = Some(e),
                    }
                }
                if ui.button("Cancel editing").clicked() {
                    keep = false;
                }
            });
            if keep {
                page.draft = Some(draft);
            }
        }
        ui.separator();
        let mut list: Vec<_> = self.templates.iter().collect();
        list.sort_by(|a, b| (&a.group, &a.abbreviation).cmp(&(&b.group, &b.abbreviation)));
        let mut group = None;
        for template in list {
            if group != Some(template.group.as_str()) {
                ui.heading(&template.group);
                group = Some(&template.group);
            }
            if ui
                .selectable_label(
                    page.selected.as_deref() == Some(template.abbreviation.as_str()),
                    format!("{} — {}", template.abbreviation, template.description),
                )
                .clicked()
            {
                page.selected = Some(template.abbreviation.clone());
            }
        }
        self.live_template_management = page;
    }
}
fn contexts() -> [(&'static str, TemplateContext); 10] {
    [
        ("Any", TemplateContext::Any),
        ("Python statement", TemplateContext::Statement),
        ("Python expression", TemplateContext::Expression),
        ("Python class", TemplateContext::ClassBody),
        ("Python module", TemplateContext::Module),
        ("Python comment", TemplateContext::Comment),
        ("Python string", TemplateContext::String),
        ("HTML", TemplateContext::Html),
        ("JavaScript", TemplateContext::JavaScript),
        ("Django templates", TemplateContext::Django),
    ]
}
fn read_import(path: &Path) -> Result<Vec<LiveTemplate>, String> {
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 8 * management::MAX_TEMPLATE_BYTES as u64 {
        return Err("Import must be a regular file smaller than 8 MiB".into());
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    management::import_json(&text)
}
fn store(path: &Path, user: &[LiveTemplate]) -> Result<(), String> {
    management::export_json(user)
        .and_then(|text| publish(path, text.as_bytes()).map_err(|e| e.to_string()))
}
/// Same-directory, exclusive temporary publication: a failed write never
/// truncates the definitions currently used by the editor.
fn publish(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let existing = match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.permissions().readonly() =>
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Template destination must be a writable regular file",
            ))
        }
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    if let Some(metadata) = existing {
        temp.as_file().set_permissions(metadata.permissions())?;
    }
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_save_selection_opens_editor_without_changing_unicode_source_or_history() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("π = '$PRICE$'\n", Language::Python));
        let original = app.tabs[0].state.doc.text();
        app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
            TextSize::new(0),
            TextSize::of(&original),
        )]);
        app.save_selection_as_live_template();
        assert!(matches!(app.dialog, Dialog::Settings));
        assert_eq!(
            app.settings_ui.page,
            settings_ui::SettingsPage::LiveTemplates
        );
        let draft = app.live_template_management.draft.as_ref().unwrap();
        assert_eq!(
            templates::expand(draft, &ExpandContext::default()).text,
            original
        );
        assert_eq!(app.tabs[0].state.doc.text(), original);
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn exported_variables_and_group_import_into_real_app_expansion_and_undo() {
        let dir = tempfile::tempdir().unwrap();
        let export = dir.path().join("export.json");
        let store_path = dir.path().join("live_templates.json");
        let t = LiveTemplate::new(
            "greeting",
            "Unicode greeting",
            "$WHO$ = $WHO$$END$",
            &[TemplateContext::Statement],
            "Team",
        )
        .with_variables(vec![TemplateVariable::new("WHO", "capitalize(\"π\")", "")]);
        publish(
            &export,
            management::export_json(std::slice::from_ref(&t))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let imported = read_import(&export).unwrap();
        store(&store_path, &imported).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.templates = templates::load_user_templates(&store_path);
        app.push_tab(EditorState::from_text("greeting", Language::Python));
        app.expand_template(
            &app.templates[0].clone(),
            TextRange::new(TextSize::new(0), TextSize::new(8)),
            None,
        );
        assert_eq!(app.tabs[0].state.doc.text(), "Π = Π");
        assert!(app.template_session.is_some());
        app.tabs[0].state.undo();
        assert_eq!(app.tabs[0].state.doc.text(), "greeting");
        assert_eq!(
            std::fs::read_to_string(export).unwrap(),
            management::export_json(&imported).unwrap()
        );
    }
    #[test]
    fn invalid_import_does_not_replace_existing_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live_templates.json");
        let t = LiveTemplate::new("valid", "", "$END$", &[TemplateContext::Any], "Team");
        store(&path, &[t]).unwrap();
        let original = std::fs::read(&path).unwrap();
        let invalid = dir.path().join("bad.json");
        std::fs::write(&invalid, "[{\"abbreviation\":\"bad space\"}]").unwrap();
        assert!(read_import(&invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}

impl IdeApp {
    pub(super) fn reset_live_template_choices(&mut self) {
        self.live_template_management.choices.clear();
        self.live_template_management.dynamic_variables.clear();
        self.live_template_management.choice_index = 0;
        self.live_template_management.choice_variable = None;
    }
    /// Refresh at explicit expansion/Tab-stop transitions using the inserted
    /// source and selected variable offset, never during popup painting.
    pub(super) fn refresh_live_template_choices(&mut self) {
        let (Some(session), Some(i)) = (&self.template_session, self.active) else {
            return;
        };
        let Some((name, ranges)) = session.vars.get(session.idx) else {
            return;
        };
        let Some((_, expression)) = self
            .live_template_management
            .dynamic_variables
            .iter()
            .find(|(variable, _)| variable == name)
        else {
            return;
        };
        let name = name.clone();
        let expression = expression.clone();
        let inputs = self.live_template_inputs(i, ranges[0].start());
        let values = if expression == "complete()" {
            inputs.completion
        } else {
            inputs.iterable_variables
        };
        let mut seen = std::collections::HashSet::new();
        let values: Vec<_> = values
            .into_iter()
            .filter(|value| seen.insert(value.clone()))
            .take(256)
            .collect();
        self.live_template_management
            .choices
            .retain(|choice| choice.name != name);
        if !values.is_empty() {
            self.live_template_management
                .choices
                .push(management::VariableChoices { name, values });
        }
        self.sync_live_template_choice_index();
    }
    fn sync_live_template_choice_index(&mut self) {
        let name = self
            .template_session
            .as_ref()
            .and_then(|session| session.vars.get(session.idx))
            .map(|(name, _)| name.clone());
        if self.live_template_management.choice_variable != name {
            self.live_template_management.choice_variable = name;
            self.live_template_management.choice_index = 0;
        }
    }
    /// Snapshot semantic suggestions only when the user requests expansion.
    /// No completion query or filesystem work runs while painting the chooser.
    pub(super) fn live_template_inputs(
        &self,
        i: usize,
        at: TextSize,
    ) -> management::ExpressionInputs {
        let Some((analysis, file)) = self.analysis_file(i) else {
            return Default::default();
        };
        // A one-off snapshot when expansion is requested: bounded like a lookup.
        let items = features::complete_bounded(
            &analysis,
            &file,
            at,
            super::completion_worker::INFERENCE_BUDGET,
            None,
        );
        let iterable_variables: Vec<String> = items
            .iter()
            .filter(|item| {
                matches!(
                    item.kind,
                    CompletionKind::Variable | CompletionKind::Parameter
                ) && [
                    "list",
                    "tuple",
                    "dict",
                    "set",
                    "str",
                    "bytes",
                    "Sequence",
                    "Iterable",
                    "Iterator",
                    "Generator",
                ]
                .iter()
                .any(|ty| item.detail == *ty || item.detail.starts_with(&format!("{ty}[")))
            })
            .map(|item| item.label.to_string())
            .collect();
        let occupied: Vec<String> = items.iter().map(|item| item.label.to_string()).collect();
        let suggested_names = management::suggest_names(&iterable_variables, &occupied);
        management::ExpressionInputs {
            completion: items.into_iter().map(|item| item.insert).collect(),
            iterable_variables,
            suggested_names,
        }
    }
    pub(super) fn live_template_choice_keys(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.live_template_choice_owned() {
            self.live_template_management.choices.clear();
            return false;
        }
        if !self.parameter_info_keyboard(ui) {
            return false;
        }
        self.sync_live_template_choice_index();
        let Some(i) = self.active else {
            return false;
        };
        if !ui.memory(|memory| memory.has_focus(self.tabs[i].view.id)) {
            return false;
        }
        let Some(values) = self.current_live_template_values() else {
            return false;
        };
        if values.is_empty() {
            return false;
        }
        let mut handled = false;
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
            self.live_template_management.choice_index =
                (self.live_template_management.choice_index + 1) % values.len();
            handled = true;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
            self.live_template_management.choice_index =
                (self.live_template_management.choice_index + values.len() - 1) % values.len();
            handled = true;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            let index = self.live_template_management.choice_index;
            self.apply_live_template_value(index);
            handled = true;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.live_template_management.choices.clear();
            handled = true;
        }
        handled
    }
    fn live_template_choice_owned(&self) -> bool {
        if !self.editor_keyboard()
            || !matches!(self.dialog, Dialog::None)
            || self.completion.visible
            || self.intentions.is_some()
            || self.quick_doc.is_some()
            || self.find_in_files.visible
            || self.nav.popup_visible()
            || self.inplace_rename.is_some()
            || self.coordinated_quit_active()
        {
            return false;
        }
        let (Some(session), Some(i)) = (&self.template_session, self.active) else {
            return false;
        };
        let Some(tab) = self.tabs.get(i) else {
            return false;
        };
        self.live_template_management.choice_tab == Some(tab.view.id)
            && session.tab_id == tab.view.id
            && session.version == tab.state.doc.version()
            && !tab.state.doc.is_read_only()
            && self.caret_in_template_variable()
    }
    fn current_live_template_values(&self) -> Option<Vec<String>> {
        let session = self.template_session.as_ref()?;
        let name = &session.vars.get(session.idx)?.0;
        self.live_template_management
            .choices
            .iter()
            .find(|choice| &choice.name == name)
            .map(|choice| choice.values.clone())
    }
    pub(super) fn live_template_choices_ui(&mut self, ctx: &egui::Context) {
        self.sync_live_template_choice_index();
        let focused = self
            .active
            .and_then(|i| self.tabs.get(i))
            .is_some_and(|tab| ctx.memory(|memory| memory.has_focus(tab.view.id)));
        if !focused || egui::Popup::is_any_open(ctx) || !self.live_template_choice_owned() {
            return;
        }
        let Some(values) = self.current_live_template_values() else {
            return;
        };
        if values.is_empty() {
            return;
        }
        let mut selected = None;
        // Editor-attached chooser: its keys are routed through the editor, so
        // the editor must stay in the focus cycle.
        egui::Window::new("Choose template variable")
            .id(egui::Id::new("live-template-variable-choices"))
            .collapsible(false)
            .resizable(false)
            .show_modeless(ctx, |ui| {
                for (index, value) in values.iter().enumerate() {
                    if ui
                        .selectable_label(
                            index == self.live_template_management.choice_index,
                            value,
                        )
                        .clicked()
                    {
                        selected = Some(index);
                    }
                }
            });
        if let Some(index) = selected {
            self.apply_live_template_value(index);
        }
    }
    fn apply_live_template_value(&mut self, index: usize) {
        if !self.live_template_choice_owned() {
            return;
        }
        let Some(value) = self
            .current_live_template_values()
            .and_then(|values| values.get(index).cloned())
        else {
            return;
        };
        let session = self.template_session.as_ref().unwrap();
        let ranges = session.vars[session.idx].1.clone();
        let name = session.vars[session.idx].0.clone();
        let i = self.active.unwrap();
        let caret = self.tabs[i].state.primary();
        let edits = ranges
            .iter()
            .map(|range| rustpy_text::Edit::replace(*range, value.clone()))
            .collect();
        if self.tabs[i].state.apply(
            edits,
            vec![caret],
            rustpy_text::EditGroup::Separate,
            Some("Live template variable"),
        ) {
            self.sync_template_session();
            if let Some(session) = &self.template_session {
                if let Some((_, ranges)) = session.vars.get(session.idx) {
                    self.tabs[i]
                        .state
                        .set_carets(vec![crate::editor::Caret::new(
                            ranges[0].start(),
                            ranges[0].end(),
                        )]);
                }
            }
            self.live_template_management
                .choices
                .retain(|choice| choice.name != name);
            self.live_template_management.choice_index = 0;
            self.focus_editor = true;
        }
    }
}

impl IdeApp {
    pub(super) fn available_surround_templates(&self) -> Vec<LiveTemplate> {
        let mut templates = templates::surround_templates();
        for custom in self.templates.iter().filter(|t| t.is_surround()) {
            templates.retain(|old| old.abbreviation != custom.abbreviation);
            templates.push(custom.clone());
        }
        if let Some(i) = self.active {
            let tab = &self.tabs[i];
            let selection = tab.state.primary().sel.range();
            let language = match tab.state.language {
                Language::Python => "python",
                Language::Cython => "cython",
                Language::Html => "html",
                Language::JavaScript => "javascript",
                Language::Template => "django",
                _ => "other",
            };
            let contexts = templates::management::selection_contexts(
                language,
                &tab.state.doc.text(),
                usize::from(selection.start()),
                usize::from(selection.end()),
            );
            templates.retain(|t| contexts.iter().any(|context| t.applies_in(*context)));
        }
        templates
    }
    pub(super) fn live_template_trigger(
        &mut self,
        ui: &mut egui::Ui,
        editor_focused: bool,
    ) -> bool {
        if !editor_focused
            || !self.parameter_info_keyboard(ui)
            || self.completion.visible
            || self.template_session.is_some()
            || self.choice.is_some()
        {
            return false;
        }
        if self.live_template_management.expand_key == ExpandKey::Tab {
            return false;
        }
        let key = match self.live_template_management.expand_key {
            ExpandKey::Tab => egui::Key::Tab,
            ExpandKey::Enter => egui::Key::Enter,
            ExpandKey::Space => egui::Key::Space,
        };
        if !ui.input(|i| i.key_pressed(key) && i.modifiers.is_none()) {
            return false;
        }
        if !self.try_expand_abbreviation() {
            return false;
        }
        ui.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, key);
            if key == egui::Key::Space {
                i.events
                    .retain(|event| !matches!(event,egui::Event::Text(text)if text==" "));
            }
        });
        true
    }
}

#[cfg(test)]
mod connected_tests {
    use super::*;
    pub(super) fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
    ) {
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                app.draw_choice_popup(ui.ctx());
                app.live_template_choices_ui(ui.ctx());
            },
        )
        .drop_without_applying_deltas();
    }
    pub(super) fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }
    pub(super) fn fixture(source: &str) -> (IdeApp, egui::Context) {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text(source, Language::Python));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        frame(&mut app, &ctx, 0.0, vec![]);
        let end = TextSize::of(source);
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(end)]);
        (app, ctx)
    }
    #[test]
    fn all_three_expansion_keys_route_through_global_shortcuts_and_real_editor_without_extra_space()
    {
        for (trigger, keycode) in [
            (ExpandKey::Tab, egui::Key::Tab),
            (ExpandKey::Enter, egui::Key::Enter),
            (ExpandKey::Space, egui::Key::Space),
        ] {
            let (mut app, ctx) = fixture("greet");
            app.live_template_management.expand_key = trigger;
            app.templates = vec![LiveTemplate::new(
                "greet",
                "",
                "π🙂$END$",
                &[TemplateContext::Module],
                "Team",
            )];
            let mut events = key(keycode);
            if keycode == egui::Key::Space {
                events.push(egui::Event::Text(" ".into()));
            }
            frame(&mut app, &ctx, 0.1, events);
            assert_eq!(app.tabs[0].state.doc.text(), "π🙂", "{trigger:?}");
            assert_eq!(app.tabs[0].state.primary().head(), TextSize::of("π🙂"));
            assert!(app.tabs[0].state.undo());
            assert_eq!(app.tabs[0].state.doc.text(), "greet");
            assert!(app.tabs[0].state.redo());
            assert_eq!(app.tabs[0].state.doc.text(), "π🙂");
        }
    }
    #[test]
    fn enum_choices_use_actual_arrow_enter_route_and_replace_all_linked_occurrences() {
        let (mut app, ctx) = fixture("choose");
        app.live_template_management.expand_key = ExpandKey::Tab;
        app.templates = vec![LiveTemplate::new(
            "choose",
            "",
            "$NAME$ = $NAME$$END$",
            &[TemplateContext::Module],
            "Team",
        )
        .with_variables(vec![TemplateVariable::new(
            "NAME",
            "enum(\"α\", \"β🙂\")",
            "",
        )])];
        frame(&mut app, &ctx, 0.1, key(egui::Key::Tab));
        assert_eq!(app.tabs[0].state.doc.text(), "α = α");
        frame(&mut app, &ctx, 0.2, key(egui::Key::ArrowDown));
        frame(&mut app, &ctx, 0.3, key(egui::Key::Enter));
        assert_eq!(app.tabs[0].state.doc.text(), "β🙂 = β🙂");
        assert!(app.template_session.is_some());
        assert!(app.live_template_management.choices.is_empty());
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), "α = α");
    }
    #[test]
    fn surround_selection_context_is_direction_independent_at_eof_and_keeps_custom_guards() {
        for (source, abbreviation) in [("print('π🙂')\nkeep()", "if"), ("π + value", "(")] {
            let (mut app, ctx) = fixture(source);
            app.templates.push(LiveTemplate::new(
                "only_comment",
                "",
                "$SELECTION$",
                &[TemplateContext::Comment],
                "Team",
            ));
            for reversed in [false, true] {
                let end = TextSize::of(source);
                app.tabs[0].state.set_carets(vec![if reversed {
                    crate::editor::Caret::new(end, TextSize::new(0))
                } else {
                    crate::editor::Caret::new(TextSize::new(0), end)
                }]);
                app.handle_action(Action::SurroundWith, &ctx);
                let choices = &app.choice.as_ref().unwrap().items;
                assert!(choices.iter().any(|(key, _)| key == abbreviation));
                assert!(!choices.iter().any(|(key, _)| key == "only_comment"));
                assert_eq!(app.tabs[0].state.doc.text(), source);
                assert!(!app.tabs[0].state.doc.can_undo());
                app.choice = None;
            }
        }
    }
    #[test]
    fn user_surround_definition_uses_existing_actual_choice_and_undo_pipeline() {
        let (mut app, ctx) = fixture("π🙂");
        app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
            TextSize::new(0),
            TextSize::of("π🙂"),
        )]);
        app.templates = vec![LiveTemplate::new(
            "custom",
            "Custom surround",
            "wrap($SELECTION$)$END$",
            &[TemplateContext::Any],
            "Team",
        )];
        app.handle_action(Action::SurroundWith, &ctx);
        let idx = app
            .choice
            .as_ref()
            .unwrap()
            .items
            .iter()
            .position(|(key, _)| key == "custom")
            .unwrap();
        app.apply_choice(idx);
        assert_eq!(app.tabs[0].state.doc.text(), "wrap(π🙂)");
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), "π🙂");
    }
    #[test]
    fn save_selection_action_and_readonly_expansion_preserve_actual_document() {
        let (mut app, ctx) = fixture("greet");
        app.tabs[0].state.set_carets(vec![crate::editor::Caret::new(
            TextSize::new(0),
            TextSize::of("greet"),
        )]);
        app.handle_action(Action::SaveSelectionAsLiveTemplate, &ctx);
        assert!(matches!(app.dialog, Dialog::Settings));
        assert!(app.live_template_management.draft.is_some());
        assert!(!app.tabs[0].state.doc.can_undo());
        app.dialog = Dialog::None;
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::of("greet"))]);
        app.tabs[0].state.doc.set_read_only(true);
        app.templates = vec![LiveTemplate::new(
            "greet",
            "",
            "replacement$END$",
            &[TemplateContext::Module],
            "Team",
        )];
        frame(&mut app, &ctx, 0.1, key(egui::Key::Tab));
        assert_eq!(app.tabs[0].state.doc.text(), "greet");
        assert!(app.template_session.is_none());
        assert!(app.live_template_management.choices.is_empty());
        assert!(!app.tabs[0].state.doc.can_undo());
    }
}

fn read_user(path: &Path) -> Result<Vec<LiveTemplate>, String> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
        Ok(_) => read_import(path),
    }
}
/// Serialize read/modify/publish across GUI owners; failed acquisition never
/// holds the main thread waiting for another window's settings transaction.
fn update_store(path: &Path, update: impl FnOnce(&mut Vec<LiveTemplate>)) -> Result<(), String> {
    use std::fs::OpenOptions;
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let lock = options
        .open(parent.join(".live_templates.lock"))
        .map_err(|e| format!("Cannot lock template definitions: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "Cannot lock template definitions: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    let result = (|| {
        let mut user = read_user(path)?;
        update(&mut user);
        store(path, &user)
    })();
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    }
    drop(lock);
    result
}

#[cfg(test)]
mod ownership_regression_tests {
    use super::connected_tests::{fixture, frame, key};
    use super::*;
    fn enum_template() -> LiveTemplate {
        LiveTemplate::new("choose", "", "$V$$END$", &[TemplateContext::Module], "Team")
            .with_variables(vec![TemplateVariable::new(
                "V",
                "enum(\"one\", \"two\")",
                "",
            )])
    }
    #[test]
    fn actual_intention_popup_enter_applies_fix_instead_of_configured_template_trigger() {
        let (mut app, ctx) = fixture("choose");
        app.live_template_management.expand_key = ExpandKey::Enter;
        app.templates = vec![enum_template()];
        let range = TextRange::new(TextSize::new(0), TextSize::of("choose"));
        app.tabs[0].state.semantic_version = Some(app.tabs[0].state.doc.version());
        app.tabs[0].state.quick_fixes = vec![(
            range,
            vec![QuickFix {
                title: "Use fixed value".into(),
                edits: vec![(range, "fixed".into())],
            }],
        )];
        app.handle_action(Action::ShowIntentionActions, &ctx);
        assert!(app.intentions.is_some());
        frame(&mut app, &ctx, 0.1, vec![]);
        frame(&mut app, &ctx, 0.2, key(egui::Key::Enter));
        assert_eq!(app.tabs[0].state.doc.text(), "fixed");
        assert!(app.template_session.is_none());
        assert!(app.intentions.is_none());
    }
    #[test]
    fn new_expansion_and_new_variable_reset_previous_cancelled_choice_index() {
        let (mut app, ctx) = fixture("choose");
        app.templates = vec![enum_template()];
        app.live_template_management.expand_key = ExpandKey::Tab;
        frame(&mut app, &ctx, 0.1, key(egui::Key::Tab));
        frame(&mut app, &ctx, 0.2, key(egui::Key::ArrowDown));
        assert_eq!(app.live_template_management.choice_index, 1);
        frame(&mut app, &ctx, 0.3, key(egui::Key::Escape));
        assert!(app.tabs[0].state.undo());
        app.template_session = None;
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::Caret::at(TextSize::of("choose"))]);
        frame(&mut app, &ctx, 0.4, key(egui::Key::Tab));
        assert_eq!(app.live_template_management.choice_index, 0);
        assert_eq!(app.tabs[0].state.doc.text(), "one");
        frame(&mut app, &ctx, 0.5, key(egui::Key::Enter));
        assert_eq!(app.tabs[0].state.doc.text(), "one");
    }
    #[test]
    fn current_completion_widget_keeps_its_arrow_keys_when_template_choices_exist() {
        let (mut app, ctx) = fixture("choose");
        app.templates = vec![enum_template()];
        app.live_template_management.expand_key = ExpandKey::Tab;
        frame(&mut app, &ctx, 0.1, key(egui::Key::Tab));
        app.completion.visible = true;
        app.completion.selected = 0;
        app.completion.items = vec![
            CompletionItem {
                label: "first".into(),
                kind: CompletionKind::Variable,
                detail: String::new(),
                insert: "first".into(),
                caret_offset: None,
                priority: 0,
            },
            CompletionItem {
                label: "second".into(),
                kind: CompletionKind::Variable,
                detail: String::new(),
                insert: "second".into(),
                caret_offset: None,
                priority: 0,
            },
        ];
        frame(&mut app, &ctx, 0.2, key(egui::Key::ArrowDown));
        assert_eq!(app.completion.selected, 1);
        assert_eq!(app.tabs[0].state.doc.text(), "one");
        assert!(app.live_template_management.choices.is_empty());
    }
}

#[cfg(test)]
mod actual_semantic_choice_tests {
    use super::*;
    #[test]
    fn complete_queries_inserted_member_context_and_iterable_queries_current_scope() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.py");
        let source = "word = 'text'\nvalues = [1, 2]\nmember\n";
        std::fs::write(&file, source).unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.open_project(dir.path());
        app.open_file(&file);
        let i = app.active.unwrap();
        let start = TextSize::new(source.find("member").unwrap() as u32);
        let range = TextRange::new(start, start + TextSize::new(6));
        app.tabs[i]
            .state
            .set_carets(vec![crate::editor::Caret::at(range.end())]);
        let template = LiveTemplate::new(
            "member",
            "",
            "word.$MEMBER$$END$",
            &[TemplateContext::Module],
            "Team",
        )
        .with_variables(vec![TemplateVariable::new("MEMBER", "complete()", "")]);
        app.expand_template(&template, range, None);
        assert!(app.template_session.is_some());
        let choices = app
            .live_template_management
            .choices
            .iter()
            .find(|choice| choice.name == "MEMBER")
            .unwrap();
        assert!(choices
            .values
            .iter()
            .any(|value| value.starts_with("upper")));
        assert!(!choices.values.iter().any(|value| value == "values"));
        let inputs = app.live_template_inputs(i, start);
        assert!(inputs.iterable_variables.contains(&"values".into()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), source);
    }
}

#[cfg(test)]
mod lookup_trigger_regression_tests {
    use super::connected_tests::{fixture, frame, key};
    use super::*;
    #[test]
    fn lookup_tab_respects_enter_space_preferences_and_retains_default_tab_template_priority() {
        for trigger in ExpandKey::ALL {
            let (mut app, ctx) = fixture("greet");
            app.live_template_management.expand_key = trigger;
            app.templates = vec![LiveTemplate::new(
                "greet",
                "",
                "template_expanded$END$",
                &[TemplateContext::Module],
                "Team",
            )];
            app.completion.visible = true;
            app.completion.explicit = true;
            app.completion.prefix_start = TextSize::new(0);
            app.completion.doc_version = app.tabs[0].state.doc.version();
            app.completion.caret = app.tabs[0].state.primary().head();
            app.completion.items = vec![CompletionItem {
                label: "completed".into(),
                kind: CompletionKind::Variable,
                detail: String::new(),
                insert: "completed".into(),
                caret_offset: None,
                priority: 0,
            }];
            frame(&mut app, &ctx, 0.1, key(egui::Key::Tab));
            let expected = if trigger == ExpandKey::Tab {
                "template_expanded"
            } else {
                "completed"
            };
            assert_eq!(app.tabs[0].state.doc.text(), expected, "{trigger:?}");
            assert!(app.template_session.is_none());
            assert!(!app.completion.visible);
            assert!(app.tabs[0].state.undo());
            assert_eq!(app.tabs[0].state.doc.text(), "greet");
        }
    }
}
