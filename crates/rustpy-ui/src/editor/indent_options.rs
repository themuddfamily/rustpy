//! Per-file indentation (roadmap §2.9 "Detect and use existing file indents",
//! §1.6 EditorConfig): the global code style, overridden by the indentation
//! detected in the file, then by `.editorconfig`, then by the per-file choice
//! made in the status bar.

use rustpy_settings::editorconfig::EditorConfigProps;
use rustpy_settings::EditorSettings;
use rustpy_text::util::IndentStyle;

use super::state::{EditOptions, EditorState};

/// Which layer decided a file's indentation.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IndentSource {
    /// Settings › Code Style.
    Settings,
    /// Detected from the file's existing indentation.
    Detected,
    /// An `.editorconfig` file.
    EditorConfig,
    /// Chosen for this file in the status bar.
    File,
}

impl IndentSource {
    pub fn describe(self) -> &'static str {
        match self {
            IndentSource::Settings => "from Settings › Code Style",
            IndentSource::Detected => "detected from the file's indents",
            IndentSource::EditorConfig => "from .editorconfig",
            IndentSource::File => "set for this file",
        }
    }
}

fn set_style(opts: &mut EditOptions, style: IndentStyle) {
    match style {
        IndentStyle::Tabs => {
            opts.use_tabs = true;
            opts.indent_size = opts.tab_size.max(1);
        }
        IndentStyle::Spaces(n) => {
            opts.use_tabs = false;
            opts.indent_size = n.clamp(1, 16);
        }
    }
}

/// Applies the per-file layers on top of `opts` (built from the settings):
/// detected indentation (when `detect` is on), then EditorConfig, then the
/// per-file override. Returns the layer that decided the indentation.
pub fn apply_file_indent(
    opts: &mut EditOptions,
    detect: bool,
    detected: Option<IndentStyle>,
    ec: &EditorConfigProps,
    file_override: Option<IndentStyle>,
) -> IndentSource {
    let mut source = IndentSource::Settings;
    // The tab width first: a detected tab indent is one tab of the file's width.
    if let Some(n) = ec.tab_width {
        opts.tab_size = n.clamp(1, 16);
    }
    if let Some(style) = detected.filter(|_| detect) {
        set_style(opts, style);
        source = IndentSource::Detected;
    }
    if let Some(style) = &ec.indent_style {
        opts.use_tabs = style.eq_ignore_ascii_case("tab");
        source = IndentSource::EditorConfig;
    }
    match ec
        .indent_size
        .or_else(|| ec.indent_size_is_tab.then_some(opts.tab_size))
    {
        Some(n) => {
            opts.indent_size = n.clamp(1, 16);
            source = IndentSource::EditorConfig;
        }
        // `indent_style = tab` without a size: one level is one tab.
        None if ec.indent_style.is_some() && opts.use_tabs => opts.indent_size = opts.tab_size,
        None => {}
    }
    if let Some(n) = ec.max_line_length {
        opts.right_margin = n;
    }
    opts.smart_tabs = ec.ide.smart_tabs;
    opts.visual_guides = ec.ide.visual_guides.clone();
    if let Some(wrap) = ec.ide.wrap_on_typing {
        opts.wrap_on_typing = wrap;
    }
    if ec.max_line_length_off {
        opts.right_margin = 0;
        opts.wrap_on_typing = false;
    }
    if let Some(style) = file_override {
        set_style(opts, style);
        source = IndentSource::File;
    }
    let ide = &ec.ide;
    if ide.continuation_indent_size.is_some()
        || ide.continuation_parameters.is_some()
        || ide.continuation_arguments.is_some()
        || ide.continuation_collections.is_some()
    {
        opts.continuation_indent = Some(rustpy_format::ContinuationIndent {
            width: ide.continuation_indent_size.unwrap_or(opts.indent_size),
            parameters: ide.continuation_parameters.unwrap_or(true),
            arguments: ide.continuation_arguments.unwrap_or(false),
            collections: ide.continuation_collections.unwrap_or(false),
        });
    }
    source
}

/// The indentation `opts` describe, as the status bar shows it.
pub fn style_of(opts: &EditOptions) -> IndentStyle {
    if opts.use_tabs {
        IndentStyle::Tabs
    } else {
        IndentStyle::Spaces(opts.indent_size)
    }
}

/// Edit options for one editor: settings plus its file's indentation layers.
/// EditorConfig is resolved through a short-lived cache (this runs every frame).
pub fn file_edit_options(
    settings: &EditorSettings,
    state: &EditorState,
) -> (EditOptions, IndentSource) {
    let mut opts = EditOptions::from_settings(settings);
    let ec = match &state.path {
        Some(path) if !path.starts_with("<typeshed>") => settings.editorconfig_for(path),
        _ => EditorConfigProps::default(),
    };
    let source = apply_file_indent(
        &mut opts,
        settings.detect_indents,
        state.detected_indent,
        &ec,
        state.indent_override,
    );
    (opts, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ec(style: Option<&str>, size: Option<usize>) -> EditorConfigProps {
        EditorConfigProps {
            indent_style: style.map(str::to_string),
            indent_size: size,
            tab_width: size,
            ..Default::default()
        }
    }

    #[test]
    fn precedence_settings_detected_editorconfig_override() {
        let base = EditOptions::default();
        let none = EditorConfigProps::default();
        let mut o = base.clone();
        assert_eq!(
            apply_file_indent(&mut o, true, None, &none, None),
            IndentSource::Settings
        );
        assert_eq!((o.use_tabs, o.indent_size), (false, 4));

        let mut o = base.clone();
        assert_eq!(
            apply_file_indent(&mut o, true, Some(IndentStyle::Spaces(2)), &none, None),
            IndentSource::Detected
        );
        assert_eq!((o.use_tabs, o.indent_size), (false, 2));

        // "Detect and use existing file indents" off: the settings win.
        let mut o = base.clone();
        assert_eq!(
            apply_file_indent(&mut o, false, Some(IndentStyle::Spaces(2)), &none, None),
            IndentSource::Settings
        );
        assert_eq!(o.indent_size, 4);

        // EditorConfig wins over detection.
        let mut o = base.clone();
        assert_eq!(
            apply_file_indent(
                &mut o,
                true,
                Some(IndentStyle::Spaces(2)),
                &ec(Some("space"), Some(3)),
                None
            ),
            IndentSource::EditorConfig
        );
        assert_eq!((o.use_tabs, o.indent_size, o.tab_size), (false, 3, 3));

        // The status-bar choice wins over everything.
        let mut o = base.clone();
        assert_eq!(
            apply_file_indent(
                &mut o,
                true,
                Some(IndentStyle::Spaces(2)),
                &ec(Some("space"), Some(3)),
                Some(IndentStyle::Tabs)
            ),
            IndentSource::File
        );
        assert!(o.use_tabs);
        assert_eq!(o.indent_size, o.tab_size);
    }

    #[test]
    fn detected_tabs_use_the_tab_size() {
        let mut o = EditOptions {
            tab_size: 8,
            ..EditOptions::default()
        };
        apply_file_indent(
            &mut o,
            true,
            Some(IndentStyle::Tabs),
            &EditorConfigProps::default(),
            None,
        );
        assert_eq!((o.use_tabs, o.indent_size), (true, 8));
        assert_eq!(style_of(&o), IndentStyle::Tabs);
        // An EditorConfig tab_width (without indent properties) is the width of the detected tab.
        let props = EditorConfigProps {
            tab_width: Some(8),
            ..Default::default()
        };
        let mut o = EditOptions::default();
        assert_eq!(
            apply_file_indent(&mut o, true, Some(IndentStyle::Tabs), &props, None),
            IndentSource::Detected
        );
        assert_eq!((o.use_tabs, o.indent_size, o.tab_size), (true, 8, 8));
    }

    #[test]
    fn editorconfig_tab_style_without_size_indents_by_one_tab() {
        let props = EditorConfigProps {
            indent_style: Some("tab".into()),
            tab_width: Some(8),
            ..Default::default()
        };
        let mut o = EditOptions::default();
        assert_eq!(
            apply_file_indent(&mut o, true, None, &props, None),
            IndentSource::EditorConfig
        );
        assert_eq!((o.use_tabs, o.indent_size, o.tab_size), (true, 8, 8));
        // Only the line length: the indentation still comes from detection.
        let props = EditorConfigProps {
            max_line_length: Some(79),
            ..Default::default()
        };
        let mut o = EditOptions::default();
        assert_eq!(
            apply_file_indent(&mut o, true, Some(IndentStyle::Spaces(2)), &props, None),
            IndentSource::Detected
        );
        assert_eq!((o.indent_size, o.right_margin), (2, 79));
    }

    #[test]
    fn file_options_use_detection_from_the_state() {
        let state =
            EditorState::from_text("def f():\n  return 1\n", super::super::Language::Python);
        assert_eq!(state.detected_indent, Some(IndentStyle::Spaces(2)));
        let settings = EditorSettings::default();
        let (o, source) = file_edit_options(&settings, &state);
        assert_eq!((o.indent_size, source), (2, IndentSource::Detected));
        let off = EditorSettings {
            detect_indents: false,
            ..EditorSettings::default()
        };
        assert_eq!(file_edit_options(&off, &state).0.indent_size, 4);
    }
}

#[cfg(test)]
mod ide_tests {
    use super::*;
    #[test]
    fn explicit_off_defeats_typing_wrap_but_not_indent_or_file_choice() {
        let mut opts = EditOptions::default();
        let ec = EditorConfigProps {
            indent_size: Some(2),
            max_line_length_off: true,
            ide: rustpy_settings::editorconfig::IdeCodeStyle {
                wrap_on_typing: Some(true),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            apply_file_indent(&mut opts, false, None, &ec, None),
            IndentSource::EditorConfig
        );
        assert_eq!(opts.indent_size, 2);
        assert_eq!(opts.right_margin, 0);
        assert!(!opts.wrap_on_typing);
        let mut state = EditorState::from_text("if True:", super::super::Language::Python);
        state.move_doc_end(false);
        state.newline(&opts);
        assert_eq!(state.doc.text(), "if True:\n  ");
        assert!(state.undo());
        assert_eq!(state.doc.text(), "if True:");
        assert!(state.redo());
        assert_eq!(state.doc.text(), "if True:\n  ");
        apply_file_indent(&mut opts, false, None, &ec, Some(IndentStyle::Spaces(3)));
        assert_eq!(opts.indent_size, 3);
    }
}
