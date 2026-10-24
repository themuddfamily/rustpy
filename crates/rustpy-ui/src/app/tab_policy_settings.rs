//! Actual Editor Tabs settings controls; no per-frame project file reads.
use rustpy_settings::tab_policy::{TabFileColor, TabOpeningOrder, TabPlacement, TabPolicySettings};
pub(super) fn show(ui: &mut egui::Ui, settings: &mut TabPolicySettings) {
    ui.horizontal(|ui| {
        ui.label("Tab placement");
        for (value, name) in [
            (TabPlacement::Top, "Top"),
            (TabPlacement::Bottom, "Bottom"),
            (TabPlacement::Left, "Left"),
            (TabPlacement::Right, "Right"),
            (TabPlacement::None, "None"),
        ] {
            ui.selectable_value(&mut settings.placement, value, crate::i18n::tr_static(name));
        }
    });
    ui.checkbox(&mut settings.multiple_rows, "Show tabs in multiple rows");
    ui.checkbox(&mut settings.show_file_icons, "Show file icons");
    ui.checkbox(&mut settings.show_full_path, "Show full path in tab title");
    ui.checkbox(&mut settings.sort_by_name, "Sort tabs by file name");
    ui.horizontal(|ui| {
        ui.label("Open new tabs");
        for (value, name) in [
            (TabOpeningOrder::End, "At the end"),
            (TabOpeningOrder::AfterCurrent, "After current tab"),
            (TabOpeningOrder::Alphabetical, "In alphabetical order"),
        ] {
            ui.selectable_value(
                &mut settings.opening_order,
                value,
                crate::i18n::tr_static(name),
            );
        }
    });
    ui.checkbox(
        &mut settings.reuse_unmodified,
        "Reuse unmodified normal tabs",
    );
    ui.label("File colors by directory scope (most specific directory wins)");
    let mut removed = None;
    for (index, scope) in settings.file_colors.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut scope.name)
                        .hint_text("Scope name")
                        .desired_width(110.0),
                );
                let mut path = scope.directory.to_string_lossy().to_string();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut path)
                            .hint_text("Project-relative or absolute directory")
                            .desired_width(220.0),
                    )
                    .changed()
                {
                    scope.directory = path.into();
                }
                ui.color_edit_button_srgb(&mut scope.rgb);
                if ui.button("Remove scope").clicked() {
                    removed = Some(index);
                }
            });
        });
    }
    if let Some(index) = removed {
        settings.file_colors.remove(index);
    }
    if ui.button("Add file color scope").clicked() {
        settings.file_colors.push(TabFileColor {
            name: "New scope".into(),
            directory: ".".into(),
            rgb: [35, 55, 75],
        });
    }
}
