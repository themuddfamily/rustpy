use super::model::{Model, Row};
use crate::a11y::AccessibleCombo;
use crate::theme::UiTheme;
use egui::{text::LayoutJob, Color32, FontId, TextFormat};
use rustpy_settings::diff_merge::{DiffMergeSettings, Highlight, Ignore};
use similar::{ChangeTag, TextDiff};
use std::collections::HashSet;
pub struct Viewer {
    pub left: String,
    pub right: String,
    pub options: DiffMergeSettings,
    pub editable: bool,
    pub apply: bool,
    selected: usize,
    expanded: HashSet<usize>,
    scroll: Option<f32>,
    pub boundary: i8,
    cache: Option<(String, String, Ignore, bool, Model)>,
}
fn display(text: &str, whitespace: bool) -> String {
    let text = text.trim_end_matches(['\r', '\n']);
    if whitespace {
        text.replace(' ', "·").replace('\t', "→   ")
    } else {
        text.into()
    }
}
fn segment(
    job: &mut LayoutJob,
    text: &str,
    color: Color32,
    background: Color32,
    mono: &FontId,
    whitespace: bool,
) {
    job.append(
        &display(text, whitespace),
        0.0,
        TextFormat {
            font_id: mono.clone(),
            color,
            background,
            ..Default::default()
        },
    );
}
/// Translucency applied to diff line/fragment backgrounds so text stays legible.
const DIFF_ALPHA: u8 = 85;
/// Deleted (left) and inserted (right) backgrounds, taken from the active color
/// scheme's `diff.deleted` / `diff.inserted` overrides when present.
fn diff_backgrounds(theme: &UiTheme) -> (Color32, Color32) {
    let tint = |(key, [r, g, b]): (&str, [u8; 3])| {
        let [r, g, b, _] = theme
            .scheme_color(key, Color32::from_rgb(r, g, b))
            .to_array();
        Color32::from_rgba_unmultiplied(r, g, b, DIFF_ALPHA)
    };
    (tint(super::DIFF_DELETED), tint(super::DIFF_INSERTED))
}
fn layout(
    text: &str,
    other: Option<&str>,
    right: bool,
    options: &DiffMergeSettings,
    theme: &UiTheme,
    mono: &FontId,
    changed: bool,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let background = if !changed || options.highlight == Highlight::None {
        Color32::TRANSPARENT
    } else {
        let (deleted, inserted) = diff_backgrounds(theme);
        if right {
            inserted
        } else {
            deleted
        }
    };
    if let Some(other) = other
        .filter(|_| changed && matches!(options.highlight, Highlight::Word | Highlight::Character))
    {
        let mut append = |tag: ChangeTag, value: &str| {
            if (right && tag != ChangeTag::Delete) || (!right && tag != ChangeTag::Insert) {
                segment(
                    &mut job,
                    value,
                    theme.text,
                    if tag == ChangeTag::Equal {
                        Color32::TRANSPARENT
                    } else {
                        background
                    },
                    mono,
                    options.whitespace,
                );
            }
        };
        if options.highlight == Highlight::Character {
            let diff = if right {
                TextDiff::from_chars(other, text)
            } else {
                TextDiff::from_chars(text, other)
            };
            for change in diff.iter_all_changes() {
                append(change.tag(), change.value());
            }
        } else {
            let diff = if right {
                TextDiff::from_words(other, text)
            } else {
                TextDiff::from_words(text, other)
            };
            for change in diff.iter_all_changes() {
                append(change.tag(), change.value());
            }
        }
    } else {
        segment(
            &mut job,
            text,
            theme.text,
            background,
            mono,
            options.whitespace,
        );
    }
    job
}
impl Viewer {
    pub fn new(left: &str, right: &str, options: DiffMergeSettings, model: Model) -> Self {
        Self {
            left: left.into(),
            right: right.into(),
            options: options.clone(),
            editable: false,
            apply: false,
            selected: 0,
            expanded: HashSet::new(),
            scroll: None,
            boundary: 0,
            cache: Some((
                left.into(),
                right.into(),
                options.ignore,
                options.align,
                model,
            )),
        }
    }
    pub fn toolbar(&mut self, ui: &mut egui::Ui, count: usize) {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.options.unified, false, "Side by side");
            ui.selectable_value(&mut self.options.unified, true, "Unified");
            egui::ComboBox::from_id_salt("diff-highlight")
                .selected_text(format!("{:?}", self.options.highlight))
                .show_ui(ui, |ui| {
                    for value in [
                        Highlight::Line,
                        Highlight::Word,
                        Highlight::Character,
                        Highlight::Split,
                        Highlight::None,
                    ] {
                        ui.selectable_value(
                            &mut self.options.highlight,
                            value,
                            format!("{value:?}"),
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Highlighting level"));
            egui::ComboBox::from_id_salt("diff-ignore")
                .selected_text(format!("Ignore {:?}", self.options.ignore))
                .show_ui(ui, |ui| {
                    for value in [Ignore::None, Ignore::Leading, Ignore::Trailing, Ignore::All] {
                        ui.selectable_value(&mut self.options.ignore, value, format!("{value:?}"));
                    }
                })
                .combo_named(crate::i18n::tr_static("Ignore whitespace"));
            ui.checkbox(&mut self.options.synchronize, "Synchronize scrolling");
            ui.checkbox(&mut self.options.collapse, "Collapse unchanged");
            ui.add(
                egui::DragValue::new(&mut self.options.context)
                    .range(0..=100)
                    .prefix("Context "),
            );
            ui.checkbox(&mut self.options.align, "Align changes");
            ui.checkbox(&mut self.options.whitespace, "Show whitespaces");
            ui.checkbox(&mut self.options.line_numbers, "Line numbers");
            let previous = ui.button("Previous (Shift+F7)").clicked()
                || ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::F7));
            let next = ui.button("Next (F7)").clicked()
                || ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F7));
            if previous {
                if self.selected == 0 {
                    self.boundary = -1;
                }
                self.selected = self.selected.saturating_sub(1);
                self.scroll = Some(-1.0);
            }
            if next {
                if self.selected + 1 >= count {
                    self.boundary = 1;
                }
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
                self.scroll = Some(-1.0);
            }
            if ui.button("First").clicked() {
                self.selected = 0;
                self.scroll = Some(-1.0);
            }
            if ui.button("Last").clicked() {
                self.selected = count.saturating_sub(1);
                self.scroll = Some(-1.0);
            }
            ui.label(format!("{} changes", count));
        });
    }
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: FontId,
        left_title: &str,
        right_title: &str,
    ) {
        let align = self.options.align && self.options.highlight != Highlight::Split;
        let mut cached = self
            .cache
            .take()
            .filter(|(left, right, ignore, aligned, _)| {
                left == &self.left
                    && right == &self.right
                    && *ignore == self.options.ignore
                    && *aligned == align
            })
            .unwrap_or_else(|| {
                (
                    self.left.clone(),
                    self.right.clone(),
                    self.options.ignore,
                    align,
                    Model::new(&self.left, &self.right, self.options.ignore, align),
                )
            });
        self.toolbar(ui, cached.4.hunks.len());
        let align = self.options.align && self.options.highlight != Highlight::Split;
        if cached.2 != self.options.ignore || cached.3 != align {
            cached = (
                self.left.clone(),
                self.right.clone(),
                self.options.ignore,
                align,
                Model::new(&self.left, &self.right, self.options.ignore, align),
            );
        }
        let model = &cached.4;
        self.selected = self.selected.min(model.hunks.len().saturating_sub(1));
        let mut apply = None;
        ui.horizontal_wrapped(|ui| {
            if self.editable {
                for (index, _) in model.hunks.iter().enumerate() {
                    if ui
                        .small_button(format!("→ Revert change {}", index + 1))
                        .clicked()
                    {
                        apply = Some(index);
                    }
                }
                if ui.button("Apply edited right side to local file").clicked() {
                    self.apply = true;
                }
            }
        });
        if let Some(index) = apply {
            if let Some(text) = model.apply(&self.right, index) {
                self.right = text;
            }
        }
        if self.editable {
            egui::CollapsingHeader::new("Edit right side (highlighting updates immediately)")
                .default_open(false)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.right)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(8),
                    );
                });
        }
        let mut visible = Vec::new();
        let mut folds = std::collections::HashMap::new();
        let mut index = 0;
        while index < model.rows.len() {
            if self.options.collapse && !model.rows[index].changed {
                let start = index;
                while index < model.rows.len() && !model.rows[index].changed {
                    index += 1;
                }
                let end = index;
                let context = self.options.context;
                if end - start > context * 2 + 1 && !self.expanded.contains(&start) {
                    for row in start..(start + context).min(end) {
                        visible.push(Some(row));
                    }
                    folds.insert(visible.len(), start);
                    visible.push(None);
                    for row in end.saturating_sub(context).max(start)..end {
                        visible.push(Some(row));
                    }
                } else {
                    visible.extend((start..end).map(Some));
                }
            } else {
                visible.push(Some(index));
                index += 1;
            }
        }
        let target = self.scroll.take().map(|_| {
            let row = model
                .hunks
                .get(self.selected)
                .map(|hunk| hunk.row)
                .unwrap_or(0);
            visible
                .iter()
                .position(|candidate| *candidate == Some(row))
                .unwrap_or(row) as f32
                * (mono.size * 1.55)
        });
        ui.horizontal(|ui| {
            ui.label(left_title);
            ui.separator();
            ui.label(right_title);
        });
        let mut gutter_apply = None;
        let mut render = |ui: &mut egui::Ui, side: Option<bool>, expanded: &mut HashSet<usize>| {
            for (item, entry) in visible.iter().enumerate() {
                if let Some(row) = entry {
                    let row = &model.rows[*row];
                    if self.options.unified {
                        if row.left.is_some() && row.changed {
                            paint_row(ui, row, model, false, &self.options, theme, &mono);
                        }
                        if row.right.is_some() {
                            paint_row(ui, row, model, true, &self.options, theme, &mono);
                        }
                    } else if let Some(right) = side {
                        paint_row(ui, row, model, right, &self.options, theme, &mono);
                    } else {
                        ui.horizontal(|ui| {
                            if self.editable {
                                if let Some(hunk) = model
                                    .hunks
                                    .iter()
                                    .position(|hunk| hunk.row == *entry.as_ref().unwrap())
                                {
                                    if ui
                                        .small_button("→")
                                        .on_hover_text("Apply left change / revert right change")
                                        .clicked()
                                    {
                                        gutter_apply = Some(hunk);
                                    }
                                } else {
                                    ui.add_space(20.0);
                                }
                            }
                            ui.columns(2, |columns| {
                                paint_row(
                                    &mut columns[0],
                                    row,
                                    model,
                                    false,
                                    &self.options,
                                    theme,
                                    &mono,
                                );
                                paint_row(
                                    &mut columns[1],
                                    row,
                                    model,
                                    true,
                                    &self.options,
                                    theme,
                                    &mono,
                                );
                            });
                        });
                    }
                } else {
                    if ui.small_button("… Expand unchanged fragment …").clicked() {
                        expanded.insert(*folds.get(&item).unwrap_or(&0));
                    }
                }
            }
        };
        if !self.options.synchronize && !self.options.unified {
            ui.columns(2, |columns| {
                for (side, column) in columns.iter_mut().enumerate() {
                    let mut area = egui::ScrollArea::both()
                        .id_salt(("diff-independent", side))
                        .max_height(500.0);
                    if let Some(offset) = target {
                        area = area.vertical_scroll_offset(offset);
                    }
                    area.show(column, |ui| render(ui, Some(side == 1), &mut self.expanded));
                }
            });
        } else {
            let mut area = egui::ScrollArea::both()
                .id_salt("rich-diff-scroll")
                .max_height(600.0);
            if let Some(offset) = target {
                area = area.vertical_scroll_offset(offset);
            }
            area.show(ui, |ui| render(ui, None, &mut self.expanded));
        }
        if let Some(index) = gutter_apply {
            if let Some(text) = model.apply(&self.right, index) {
                self.right = text;
            }
        }
        self.cache = Some(cached);
    }
}
fn paint_row(
    ui: &mut egui::Ui,
    row: &Row,
    model: &Model,
    right: bool,
    options: &DiffMergeSettings,
    theme: &UiTheme,
    mono: &FontId,
) {
    let index = if right { row.right } else { row.left };
    let source = if right { &model.right } else { &model.left };
    let other_index = if right { row.left } else { row.right };
    let other = if right { &model.left } else { &model.right };
    ui.horizontal(|ui| {
        if options.line_numbers {
            ui.label(
                egui::RichText::new(
                    index
                        .map(|index| (index + 1).to_string())
                        .unwrap_or_default(),
                )
                .monospace()
                .color(theme.text_dim),
            );
        }
        let text = index
            .and_then(|index| source.get(index))
            .map(String::as_str)
            .unwrap_or(" ");
        ui.add(
            egui::Label::new(layout(
                text,
                other_index
                    .and_then(|index| other.get(index))
                    .map(String::as_str),
                right,
                options,
                theme,
                mono,
                row.changed,
            ))
            .wrap_mode(if options.soft_wrap {
                egui::TextWrapMode::Wrap
            } else {
                egui::TextWrapMode::Extend
            }),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_highlight::{ColorScheme, Rgb};

    fn background(job: &LayoutJob) -> Color32 {
        job.sections[0].format.background
    }

    #[test]
    fn scheme_diff_overrides_drive_viewer_backgrounds() {
        let options = DiffMergeSettings {
            highlight: Highlight::Line,
            ..Default::default()
        };
        let mono = FontId::monospace(12.0);
        let mut theme = UiTheme::dark();
        let defaults = (
            background(&layout("a", None, false, &options, &theme, &mono, true)),
            background(&layout("b", None, true, &options, &theme, &mono, true)),
        );
        assert_eq!(defaults.0, Color32::from_rgba_unmultiplied(170, 40, 40, 85));
        assert_eq!(defaults.1, Color32::from_rgba_unmultiplied(30, 130, 60, 85));

        let mut scheme = ColorScheme::by_name("Dark").unwrap();
        scheme
            .context_colors
            .insert("diff.inserted".into(), Rgb(0, 0, 255));
        scheme
            .context_colors
            .insert("diff.deleted".into(), Rgb(255, 255, 0));
        theme.bind_editor_scheme(&scheme);
        assert_eq!(
            background(&layout("a", None, false, &options, &theme, &mono, true)),
            Color32::from_rgba_unmultiplied(255, 255, 0, 85)
        );
        assert_eq!(
            background(&layout("b", Some("a"), true, &options, &theme, &mono, true)),
            Color32::from_rgba_unmultiplied(0, 0, 255, 85)
        );
        // Unchanged rows stay transparent regardless of the scheme.
        assert_eq!(
            background(&layout("b", None, true, &options, &theme, &mono, false)),
            Color32::TRANSPARENT
        );
    }
}
