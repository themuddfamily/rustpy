//! Copyright profile editor and the active-document Update Copyright action.
use super::*;
use crate::a11y::AccessibleCombo;
use rustpy_settings::copyright::{
    self, CommentStyle, CopyrightProfile, CopyrightScope, UpdatePolicy, Variables,
};

impl IdeApp {
    pub(super) fn copyright_profiles_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Copyright profiles");
        ui.checkbox(
            &mut self.settings.copyright.enabled,
            "Enable Update Copyright",
        );
        ui.checkbox(
            &mut self.settings.copyright.add_to_new_files,
            "Add copyright to new file headers",
        );
        ui.label("Scopes use project-relative globs (*, **, ?). First matching rule wins; an empty profile excludes a scope.");
        let settings = &mut self.settings.copyright_profiles;
        if settings.is_unconfigured() {
            ui.label("The legacy notice remains active until you create profiles.");
            ui.add(
                egui::TextEdit::multiline(&mut self.settings.copyright.text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            if ui
                .button("Create default profile from current notice")
                .clicked()
            {
                settings
                    .profiles
                    .push(CopyrightProfile::from_legacy(&self.settings.copyright.text));
                settings.default_profile = Some("Default".into());
            }
        }
        ui.horizontal(|ui| {
            let default_profile_label = ui.label("Default profile");
            let current = settings.default_profile.as_deref().unwrap_or("None");
            egui::ComboBox::from_id_salt("copyright-default")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut settings.default_profile, None, "None");
                    for profile in &settings.profiles {
                        ui.selectable_value(
                            &mut settings.default_profile,
                            Some(profile.name.clone()),
                            &profile.name,
                        );
                    }
                })
                .combo_labelled(&default_profile_label);
            if ui.button("Add profile").clicked() {
                let mut index = 1;
                while settings
                    .profiles
                    .iter()
                    .any(|p| p.name == format!("Profile {index}"))
                {
                    index += 1;
                }
                settings.profiles.push(CopyrightProfile {
                    name: format!("Profile {index}"),
                    ..Default::default()
                });
            }
        });
        let mut remove = None;
        for (i, profile) in settings.profiles.iter_mut().enumerate() {
            egui::CollapsingHeader::new(&profile.name).id_salt(("copyright-profile",i)).show(ui,|ui| {
                ui.horizontal(|ui| { ui.label("Name"); ui.text_edit_singleline(&mut profile.name); if ui.button("Delete profile").clicked() {remove=Some(i);} });
                ui.add(egui::TextEdit::multiline(&mut profile.text).desired_rows(5).desired_width(f32::INFINITY));
                ui.label("Variables: $today.year, $today.date, $project, $user, $file; $$ emits $.");
                ui.horizontal(|ui| {ui.label("Ownership marker"); ui.text_edit_singleline(&mut profile.marker);});
                ui.label("Only leading comment headers containing this literal marker can be replaced.");
                egui::ComboBox::from_id_salt(("copyright-policy",i)).selected_text(format!("{:?}",profile.policy)).show_ui(ui,|ui| {
                    ui.selectable_value(&mut profile.policy,UpdatePolicy::ReplaceOrAdd,"Replace existing or add missing");
                    ui.selectable_value(&mut profile.policy,UpdatePolicy::AddMissingOnly,"Add missing only");
                    ui.selectable_value(&mut profile.policy,UpdatePolicy::UpdateExistingOnly,"Update existing only");
                }).combo_named(crate::i18n::tr_static("Update policy"));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut profile.style,CommentStyle::Line,"Line comments");
                    ui.selectable_value(&mut profile.style,CommentStyle::Block,"Block comments");
                });
                ui.label("Python block comments use a boxed # notice; XML uses <!-- -->.");
                ui.checkbox(&mut profile.blank_after,"Blank line after notice");
                ui.checkbox(&mut profile.border,"Top border on line comments");
                ui.horizontal(|ui| {ui.label("Wrap width (0 preserves lines)");ui.add(egui::DragValue::new(&mut profile.width).range(0..=240));});
            });
        }
        if let Some(i) = remove {
            settings.profiles.remove(i);
        }
        ui.separator();
        ui.heading("Scope rules");
        let mut remove = None;
        let mut move_up = None;
        for (i, scope) in settings.scopes.iter_mut().enumerate() {
            ui.push_id(("copyright-scope", i), |ui| {
                ui.horizontal(|ui| {
                    ui.label("Scope");
                    ui.text_edit_singleline(&mut scope.name);
                    if i > 0 && ui.small_button("↑").clicked() {
                        move_up = Some(i);
                    }
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Include");
                    ui.text_edit_singleline(&mut scope.include);
                    ui.label("Exclude");
                    ui.text_edit_singleline(&mut scope.exclude);
                });
                egui::ComboBox::from_id_salt("profile")
                    .selected_text(if scope.profile.is_empty() {
                        "Excluded"
                    } else {
                        &scope.profile
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut scope.profile, String::new(), "Excluded");
                        for profile in &settings.profiles {
                            ui.selectable_value(
                                &mut scope.profile,
                                profile.name.clone(),
                                &profile.name,
                            );
                        }
                    })
                    .combo_named(crate::i18n::tr_static("Profile"));
            });
        }
        if let Some(i) = remove {
            settings.scopes.remove(i);
        } else if let Some(i) = move_up {
            settings.scopes.swap(i, i - 1);
        }
        if ui.button("Add scope rule").clicked() {
            settings.scopes.push(CopyrightScope {
                profile: settings.default_profile.clone().unwrap_or_default(),
                ..Default::default()
            });
        }
        if let Err(error) = settings.validate() {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
    }

    pub(super) fn update_copyright_profile(&mut self) {
        if !self.settings.copyright.enabled {
            self.set_status("Enable copyright in Settings › Editor › Code Style");
            return;
        }
        let Some(i) = self.active else {
            return;
        };
        if self.tabs[i].state.doc.is_read_only()
            || self.tabs[i].image.is_some()
            || self.tabs[i].notebook.is_some()
        {
            self.set_status("Copyright updates require a writable text editor");
            return;
        }
        let Some(path) = self.tabs[i].state.path.clone() else {
            self.set_status("Save the file before applying a copyright scope");
            return;
        };
        let legacy = CopyrightProfile::from_legacy(&self.settings.copyright.text);
        let selection = if self.settings.copyright_profiles.is_unconfigured() {
            Ok(Some(&legacy))
        } else {
            self.settings.copyright_profiles.validate().and_then(|()| {
                self.settings
                    .copyright_profiles
                    .profile_for(&path, self.project.as_ref().map(|p| p.root.as_path()))
            })
        };
        let profile = match selection {
            Ok(Some(p)) => p,
            Ok(None) => {
                self.set_status("No copyright profile applies to this scope");
                return;
            }
            Err(e) => {
                self.set_status(e);
                return;
            }
        };
        let now = chrono::Local::now();
        let year = now.format("%Y").to_string();
        let date = now.format("%Y-%m-%d").to_string();
        let project = self
            .project
            .as_ref()
            .and_then(|p| p.root.file_name())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".into());
        let file = path
            .file_name()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let vars = Variables {
            year: &year,
            date: &date,
            project: &project,
            user: &user,
            file: &file,
        };
        let planned = copyright::plan_update(&self.tabs[i].state.doc.text(), &path, profile, &vars);
        match planned {
            Ok(Some(edit)) => {
                let range = TextRange::new(
                    TextSize::new(edit.start as u32),
                    TextSize::new(edit.end as u32),
                );
                if self.tabs[i]
                    .state
                    .apply_text_edits(vec![(range, edit.replacement)], "Update copyright")
                {
                    self.set_status("Copyright updated");
                } else {
                    self.set_status("Copyright update could not be applied");
                }
            }
            Ok(None) => self.set_status("Copyright is up to date or excluded by update policy"),
            Err(error) => self.set_status(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(path: &Path, profiles: rustpy_settings::copyright::CopyrightProfilesSettings) -> IdeApp {
        let mut settings = Settings::default();
        settings.copyright.enabled = true;
        settings.copyright_profiles = profiles;
        let mut app = IdeApp::new_welcome(settings);
        app.open_file(path);
        app
    }
    #[test]
    fn actual_update_action_preserves_python_header_and_is_one_undo_unit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        let original = "#!/bin/python\n# coding: utf-8\nπ=1\n";
        std::fs::write(&path, original).unwrap();
        let mut app = app(&path, Default::default());
        app.update_copyright_profile();
        let tab = &mut app.tabs[app.active.unwrap()];
        let updated = tab.state.doc.text();
        assert!(updated.starts_with("#!/bin/python\n# coding: utf-8\n# ---\n# Copyright"));
        assert!(updated.ends_with("π=1\n"));
        assert!(tab.state.doc.is_modified());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(tab.state.doc.undo().is_some());
        assert_eq!(tab.state.doc.text(), original);
        assert!(tab.state.doc.redo().is_some());
        assert_eq!(tab.state.doc.text(), updated);
        app.update_copyright_profile();
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), updated);
    }
    #[test]
    fn actual_scope_exclusion_and_read_only_document_remain_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "x=1\n").unwrap();
        let profiles = copyright::CopyrightProfilesSettings {
            profiles: vec![CopyrightProfile::default()],
            default_profile: None,
            scopes: vec![],
        };
        let mut app = app(&path, profiles);
        app.update_copyright_profile();
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), "x=1\n");
        app.settings.copyright_profiles.default_profile = Some("Default".into());
        app.tabs[app.active.unwrap()].state.doc.set_read_only(true);
        app.update_copyright_profile();
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), "x=1\n");
    }
}

#[cfg(test)]
mod missing_profile_tests {
    use super::*;
    #[test]
    fn deleted_last_profile_does_not_apply_legacy_notice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "x=1\n").unwrap();
        let mut settings = Settings::default();
        settings.copyright.enabled = true;
        settings.copyright_profiles.default_profile = Some("Deleted".into());
        let mut app = IdeApp::new_welcome(settings);
        app.open_file(&path);
        app.update_copyright_profile();
        let doc = &app.tabs[app.active.unwrap()].state.doc;
        assert_eq!(doc.text(), "x=1\n");
        assert!(!doc.is_modified());
    }
}

#[cfg(test)]
mod cookie_position_tests {
    use super::*;
    #[test]
    fn actual_action_refuses_pre_cookie_notice_without_document_or_disk_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.py");
        let original = "# Copyright 2000 Old\n# coding: utf-8\nπ=1\n";
        std::fs::write(&path, original).unwrap();
        let mut settings = Settings::default();
        settings.copyright.enabled = true;
        let mut app = IdeApp::new_welcome(settings);
        app.open_file(&path);
        let i = app.active.unwrap();
        let version = app.tabs[i].state.doc.version();
        let encoding = app.tabs[i].state.doc.encoding();
        app.update_copyright_profile();
        assert_eq!(app.tabs[i].state.doc.text(), original);
        assert_eq!(app.tabs[i].state.doc.version(), version);
        assert_eq!(app.tabs[i].state.doc.encoding(), encoding);
        assert!(!app.tabs[i].state.doc.is_modified());
        assert!(app.tabs[i].state.doc.undo_label().as_deref().is_none());
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}
