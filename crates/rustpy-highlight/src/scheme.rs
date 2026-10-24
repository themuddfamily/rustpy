//! Editor color schemes (Dark, Light, High Contrast) — roadmap §2.3.

use std::collections::BTreeMap;

use crate::HighlightTag;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Rgb(
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        )
    }
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct TextStyle {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    #[serde(default)]
    pub wave: bool,
    #[serde(default)]
    pub bold_underline: bool,
    #[serde(default)]
    pub strikeout: bool,
    #[serde(default)]
    pub border: bool,
    #[serde(default)]
    pub effect_color: Option<Rgb>,
}

impl TextStyle {
    pub const fn fg(c: u32) -> Self {
        Self {
            fg: Some(Rgb::hex(c)),
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            wave: false,
            bold_underline: false,
            strikeout: false,
            border: false,
            effect_color: None,
        }
    }
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
}

/// Non-token colors of the editor surface.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditorColors {
    pub background: Rgb,
    pub foreground: Rgb,
    pub caret: Rgb,
    pub caret_row: Rgb,
    pub selection: Rgb,
    pub selection_inactive: Rgb,
    pub line_number: Rgb,
    pub line_number_current: Rgb,
    pub gutter_background: Rgb,
    pub gutter_separator: Rgb,
    pub indent_guide: Rgb,
    pub indent_guide_active: Rgb,
    pub right_margin: Rgb,
    pub whitespace: Rgb,
    pub matched_brace_bg: Rgb,
    pub search_result_bg: Rgb,
    pub identifier_under_caret_bg: Rgb,
    pub identifier_under_caret_write_bg: Rgb,
    pub error_stripe_error: Rgb,
    pub error_stripe_warning: Rgb,
    pub error_underline: Rgb,
    pub warning_underline: Rgb,
    pub weak_warning_underline: Rgb,
    pub typo_underline: Rgb,
    pub folded_text_bg: Rgb,
    pub folded_text_fg: Rgb,
    pub sticky_lines_bg: Rgb,
    pub inlay_hint_bg: Rgb,
    pub inlay_hint_fg: Rgb,
    pub added_line: Rgb,
    pub modified_line: Rgb,
    pub deleted_line: Rgb,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ColorScheme {
    pub name: String,
    pub dark: bool,
    pub editor: EditorColors,
    pub styles: BTreeMap<HighlightTag, TextStyle>,
    /// Explicit attribute inheritance; absent entries use language defaults.
    #[serde(default)]
    pub inherits: BTreeMap<HighlightTag, HighlightTag>,
    /// Per-scheme context overrides. Keys are stable UI/consumer roles.
    #[serde(default)]
    pub context_colors: BTreeMap<String, Rgb>,
}

impl ColorScheme {
    /// Resolves the public Python external attribute name through the same style
    /// table used to paint actual spans, including inherited defaults.
    pub fn python_attribute_style(&self, key: &str) -> Option<TextStyle> {
        crate::PYTHON_ATTRIBUTE_KEYS
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, tag)| self.style(*tag))
    }
    pub fn set_python_attribute_style(&mut self, key: &str, style: TextStyle) -> bool {
        if let Some((_, tag)) = crate::PYTHON_ATTRIBUTE_KEYS
            .iter()
            .find(|(name, _)| *name == key)
        {
            self.inherits.remove(tag);
            self.styles.insert(*tag, style);
            if key == "PY.STRING.U" {
                self.inherits.remove(&HighlightTag::FString);
                self.styles.insert(HighlightTag::FString, style);
            }
            if key == "PY.KEYWORD" {
                self.inherits.remove(&HighlightTag::SoftKeyword);
                self.styles.insert(HighlightTag::SoftKeyword, style);
            }
            true
        } else {
            false
        }
    }

    pub fn inherit_style(&mut self, tag: HighlightTag, parent: HighlightTag) -> bool {
        if tag == parent {
            return false;
        }
        let mut current = parent;
        let mut seen = vec![tag];
        for _ in 0..32 {
            if seen.contains(&current) {
                return false;
            }
            seen.push(current);
            match self.inherits.get(&current) {
                Some(next) => current = *next,
                None => {
                    self.inherits.insert(tag, parent);
                    return true;
                }
            }
        }
        false
    }
    pub fn reset_style(&mut self, tag: HighlightTag) {
        self.inherits.remove(&tag);
        let base = Self::by_name(&self.name).unwrap_or_else(|| {
            if self.dark {
                Self::dark()
            } else {
                Self::light()
            }
        });
        if let Some(style) = base.styles.get(&tag) {
            self.styles.insert(tag, *style);
        } else {
            self.styles.remove(&tag);
        }
    }
    pub fn context_color(&self, key: &str, fallback: Rgb) -> Rgb {
        self.context_colors.get(key).copied().unwrap_or(fallback)
    }
    fn named_palette(name: &str) -> Self {
        use HighlightTag as T;
        let (bg, fg, keyword, string, number, comment) = match name {
            "Monokai" => (0x272822, 0xf8f8f2, 0xf92672, 0xe6db74, 0xae81ff, 0x75715e),
            "Twilight" => (0x141414, 0xf8f8f8, 0xcda869, 0x8f9d6a, 0xcf6a4c, 0x5f5a60),
            "Slate Dark" => (0x2b2b2b, 0xbcbec4, 0xcf8e6d, 0x6aab73, 0x2aacb8, 0x7a7e85),
            "WarmNeon" => (0x0b161d, 0xf0efeb, 0xffc600, 0x26a98b, 0xff73fd, 0x6c7a89),
            "Solarized Light" => (0xfdf6e3, 0x657b83, 0x859900, 0x2aa198, 0xd33682, 0x93a1a1),
            _ => (0x002b36, 0x839496, 0x859900, 0x2aa198, 0xd33682, 0x586e75),
        };
        let mut scheme = if name == "Solarized Light" {
            Self::light()
        } else {
            Self::dark()
        };
        scheme.name = name.into();
        scheme.editor.background = Rgb::hex(bg);
        scheme.editor.foreground = Rgb::hex(fg);
        for (tag, color) in [
            (T::Default, fg),
            (T::Keyword, keyword),
            (T::SoftKeyword, keyword),
            (T::String, string),
            (T::Bytes, string),
            (T::FString, string),
            (T::Number, number),
            (T::Comment, comment),
            (T::Docstring, comment),
        ] {
            scheme.styles.insert(tag, TextStyle::fg(color));
        }
        scheme
    }
    pub fn style(&self, tag: HighlightTag) -> TextStyle {
        let mut tag = tag;
        let mut seen = Vec::new();
        while let Some(parent) = self.inherits.get(&tag) {
            if seen.len() >= 32 || seen.contains(parent) {
                return TextStyle::default();
            }
            seen.push(tag);
            tag = *parent;
        }
        match self.styles.get(&tag) {
            Some(style) => *style,
            // Schemes saved before the tag existed gray unused code like comments.
            None if tag == HighlightTag::Unused => TextStyle {
                fg: self.styles.get(&HighlightTag::Comment).and_then(|c| c.fg),
                ..Default::default()
            },
            None if tag == HighlightTag::DocCommentTag => {
                let mut value = self.style(HighlightTag::Docstring);
                value.bold = true;
                value.italic = self.dark;
                value.underline = true;
                value.effect_color = value.fg;
                value
            }
            None if tag == HighlightTag::InvalidEscapeSequence => {
                let mut value = self.style(HighlightTag::String);
                if self.dark {
                    value.wave = true;
                    value.effect_color = Some(Rgb::hex(0xff0000));
                } else {
                    value.bg = Some(Rgb::hex(0xffcccc));
                }
                value
            }
            None if matches!(
                tag,
                HighlightTag::PredefinedDefinition | HighlightTag::PredefinedUsage
            ) =>
            {
                let mut value = TextStyle::fg(
                    ((self.editor.foreground.0 as u32) << 16)
                        | ((self.editor.foreground.1 as u32) << 8)
                        | (self.editor.foreground.2 as u32),
                );
                value.italic = true;
                value
            }
            None => {
                use HighlightTag::*;
                let fallback = match tag {
                    NestedFunctionDefinition => FunctionDefinition,
                    MethodCall => FunctionCall,
                    PredefinedDefinition | PredefinedUsage => Dunder,
                    DocCommentTag => Docstring,
                    InvalidEscapeSequence => Error,
                    TypeParameter => Parameter,
                    FStringColon | FStringConversion | FStringFormatCharacter => EscapeSequence,
                    FStringFormatNumber => Number,
                    _ => return TextStyle::default(),
                };
                self.style(fallback)
            }
        }
    }

    pub fn by_name(name: &str) -> Option<ColorScheme> {
        match name {
            // Legacy names written by earlier builds are accepted as aliases.
            "Dark" | "Darcula" => Some(Self::dark()),
            "Light" | "IntelliJ Light" | "Default" => Some(Self::light()),
            "High Contrast" => Some(Self::high_contrast()),
            "Slate Dark" | "Rider Dark" => Some(Self::named_palette("Slate Dark")),
            "Monokai" | "Twilight" | "WarmNeon" | "Solarized Dark" | "Solarized Light" => {
                Some(Self::named_palette(name))
            }
            _ => None,
        }
    }

    pub fn builtin_names() -> &'static [&'static str] {
        &[
            "Dark",
            "Light",
            "High Contrast",
            "Monokai",
            "Twilight",
            "Slate Dark",
            "WarmNeon",
            "Solarized Dark",
            "Solarized Light",
        ]
    }

    pub fn dark() -> ColorScheme {
        use HighlightTag as T;
        let styles = [
            (T::Default, TextStyle::fg(0xA9B7C6)),
            (T::Rainbow1, TextStyle::fg(0x9ACD68)),
            (T::Rainbow2, TextStyle::fg(0x6AB7FF)),
            (T::Rainbow3, TextStyle::fg(0xE2A96B)),
            (T::Rainbow4, TextStyle::fg(0xD28CDB)),
            (T::Rainbow5, TextStyle::fg(0x5BD4C8)),
            (T::Rainbow6, TextStyle::fg(0xF2D96B)),
            (T::Keyword, TextStyle::fg(0xCC7832)),
            (T::SoftKeyword, TextStyle::fg(0xCC7832)),
            (T::Builtin, TextStyle::fg(0x8888C6)),
            (T::SelfParameter, TextStyle::fg(0x94558D)),
            (T::Decorator, TextStyle::fg(0xBBB529)),
            (T::FunctionDefinition, TextStyle::fg(0xFFC66D)),
            (T::ClassDefinition, TextStyle::fg(0xA9B7C6).bold()),
            (T::FunctionCall, TextStyle::fg(0xA9B7C6)),
            (T::Number, TextStyle::fg(0x6897BB)),
            (T::String, TextStyle::fg(0x6A8759)),
            (T::Bytes, TextStyle::fg(0x6A8759)),
            (T::FString, TextStyle::fg(0x6A8759)),
            (T::FStringBrace, TextStyle::fg(0xCC7832)),
            (T::EscapeSequence, TextStyle::fg(0xCC7832)),
            (T::Docstring, TextStyle::fg(0x629755).italic()),
            (T::Comment, TextStyle::fg(0x808080)),
            (T::TodoComment, TextStyle::fg(0xA8C023).italic()),
            (T::Operator, TextStyle::fg(0xA9B7C6)),
            (T::Punctuation, TextStyle::fg(0xA9B7C6)),
            (T::Brace, TextStyle::fg(0xA9B7C6)),
            (T::Bracket, TextStyle::fg(0xA9B7C6)),
            (T::Parenthesis, TextStyle::fg(0xA9B7C6)),
            (T::Comma, TextStyle::fg(0xCC7832)),
            (T::Dot, TextStyle::fg(0xA9B7C6)),
            (T::Dunder, TextStyle::fg(0xB200B2)),
            (T::KeywordArgument, TextStyle::fg(0xAA4926)),
            (T::Parameter, TextStyle::fg(0xA9B7C6)),
            (T::TypeAnnotation, TextStyle::fg(0xA9B7C6)),
            (T::LocalVariable, TextStyle::fg(0xA9B7C6)),
            (
                T::Error,
                TextStyle {
                    fg: Some(Rgb::hex(0xBC3F3C)),
                    underline: true,
                    ..Default::default()
                },
            ),
            (T::Key, TextStyle::fg(0x5CA7A7)),
            (T::Tag, TextStyle::fg(0xE8BF6A)),
            (T::Heading, TextStyle::fg(0xFFC66D).bold()),
            (
                T::Link,
                TextStyle {
                    fg: Some(Rgb::hex(0x287BDE)),
                    underline: true,
                    ..Default::default()
                },
            ),
            (
                T::Emphasis,
                TextStyle {
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Strong,
                TextStyle {
                    bold: true,
                    ..Default::default()
                },
            ),
            (T::Variable, TextStyle::fg(0x9876AA)),
            (T::Unused, TextStyle::fg(0x72737A)),
        ];
        let mut scheme = ColorScheme {
            name: "Dark".into(),
            dark: true,
            editor: EditorColors {
                background: Rgb::hex(0x2B2B2B),
                foreground: Rgb::hex(0xA9B7C6),
                caret: Rgb::hex(0xBBBBBB),
                caret_row: Rgb::hex(0x323232),
                selection: Rgb::hex(0x214283),
                selection_inactive: Rgb::hex(0x3A3D41),
                line_number: Rgb::hex(0x606366),
                line_number_current: Rgb::hex(0xA4A3A3),
                gutter_background: Rgb::hex(0x313335),
                gutter_separator: Rgb::hex(0x3C3F41),
                indent_guide: Rgb::hex(0x373737),
                indent_guide_active: Rgb::hex(0x545454),
                right_margin: Rgb::hex(0x404040),
                whitespace: Rgb::hex(0x505050),
                matched_brace_bg: Rgb::hex(0x3B514D),
                search_result_bg: Rgb::hex(0x32593D),
                identifier_under_caret_bg: Rgb::hex(0x344134),
                identifier_under_caret_write_bg: Rgb::hex(0x40332B),
                error_stripe_error: Rgb::hex(0x9E2927),
                error_stripe_warning: Rgb::hex(0xBE9117),
                error_underline: Rgb::hex(0xBC3F3C),
                warning_underline: Rgb::hex(0xBE9117),
                weak_warning_underline: Rgb::hex(0x756D56),
                typo_underline: Rgb::hex(0x659C6B),
                folded_text_bg: Rgb::hex(0x3A3A3A),
                folded_text_fg: Rgb::hex(0x8C8C8C),
                sticky_lines_bg: Rgb::hex(0x2E2E2E),
                inlay_hint_bg: Rgb::hex(0x3D3F41),
                inlay_hint_fg: Rgb::hex(0x787878),
                added_line: Rgb::hex(0x629755),
                modified_line: Rgb::hex(0x6897BB),
                deleted_line: Rgb::hex(0x6E6E6E),
            },
            styles: styles.into_iter().collect(),
            inherits: BTreeMap::new(),
            context_colors: BTreeMap::new(),
        };
        crate::python_defaults::apply(&mut scheme, "Dark");
        scheme
    }

    pub fn light() -> ColorScheme {
        use HighlightTag as T;
        let styles = [
            (T::Default, TextStyle::fg(0x080808)),
            (T::Rainbow1, TextStyle::fg(0x3C8C2E)),
            (T::Rainbow2, TextStyle::fg(0x1F6FBF)),
            (T::Rainbow3, TextStyle::fg(0xB0581E)),
            (T::Rainbow4, TextStyle::fg(0x8A3FA8)),
            (T::Rainbow5, TextStyle::fg(0x1B8C84)),
            (T::Rainbow6, TextStyle::fg(0x9A7A00)),
            (T::Keyword, TextStyle::fg(0x0033B3)),
            (T::SoftKeyword, TextStyle::fg(0x0033B3)),
            (T::Builtin, TextStyle::fg(0x000080)),
            (T::SelfParameter, TextStyle::fg(0x94558D)),
            (T::Decorator, TextStyle::fg(0x9E880D)),
            (T::FunctionDefinition, TextStyle::fg(0x00627A)),
            (T::ClassDefinition, TextStyle::fg(0x000000).bold()),
            (T::FunctionCall, TextStyle::fg(0x080808)),
            (T::Number, TextStyle::fg(0x1750EB)),
            (T::String, TextStyle::fg(0x067D17)),
            (T::Bytes, TextStyle::fg(0x067D17)),
            (T::FString, TextStyle::fg(0x067D17)),
            (T::FStringBrace, TextStyle::fg(0x0037A6)),
            (T::EscapeSequence, TextStyle::fg(0x0037A6)),
            (T::Docstring, TextStyle::fg(0x8C8C8C).italic()),
            (T::Comment, TextStyle::fg(0x8C8C8C)),
            (T::TodoComment, TextStyle::fg(0x008DDE).italic()),
            (T::Operator, TextStyle::fg(0x080808)),
            (T::Punctuation, TextStyle::fg(0x080808)),
            (T::Brace, TextStyle::fg(0x080808)),
            (T::Bracket, TextStyle::fg(0x080808)),
            (T::Parenthesis, TextStyle::fg(0x080808)),
            (T::Comma, TextStyle::fg(0x080808)),
            (T::Dot, TextStyle::fg(0x080808)),
            (T::Dunder, TextStyle::fg(0xB200B2)),
            (T::KeywordArgument, TextStyle::fg(0x1750EB)),
            (T::Parameter, TextStyle::fg(0x080808)),
            (T::TypeAnnotation, TextStyle::fg(0x080808)),
            (T::LocalVariable, TextStyle::fg(0x080808)),
            (
                T::Error,
                TextStyle {
                    fg: Some(Rgb::hex(0xFF0000)),
                    underline: true,
                    ..Default::default()
                },
            ),
            (T::Key, TextStyle::fg(0x007070)),
            (T::Tag, TextStyle::fg(0x000080)),
            (T::Heading, TextStyle::fg(0x0033B3).bold()),
            (
                T::Link,
                TextStyle {
                    fg: Some(Rgb::hex(0x006DCC)),
                    underline: true,
                    ..Default::default()
                },
            ),
            (
                T::Emphasis,
                TextStyle {
                    italic: true,
                    ..Default::default()
                },
            ),
            (
                T::Strong,
                TextStyle {
                    bold: true,
                    ..Default::default()
                },
            ),
            (T::Variable, TextStyle::fg(0x871094)),
            (T::Unused, TextStyle::fg(0x8C8C8C)),
        ];
        let mut scheme = ColorScheme {
            name: "Light".into(),
            dark: false,
            editor: EditorColors {
                background: Rgb::hex(0xFFFFFF),
                foreground: Rgb::hex(0x000000),
                caret: Rgb::hex(0x000000),
                caret_row: Rgb::hex(0xFCFAED),
                selection: Rgb::hex(0xA6D2FF),
                selection_inactive: Rgb::hex(0xE4E4E4),
                line_number: Rgb::hex(0xADADAD),
                line_number_current: Rgb::hex(0x6E6E6E),
                gutter_background: Rgb::hex(0xF2F2F2),
                gutter_separator: Rgb::hex(0xEBECF0),
                indent_guide: Rgb::hex(0xEBEBEB),
                indent_guide_active: Rgb::hex(0xC5C5C5),
                right_margin: Rgb::hex(0xE1E1E1),
                whitespace: Rgb::hex(0xC0C0C0),
                matched_brace_bg: Rgb::hex(0xDDF0FF),
                search_result_bg: Rgb::hex(0xE5F1E5),
                identifier_under_caret_bg: Rgb::hex(0xEDEBFC),
                identifier_under_caret_write_bg: Rgb::hex(0xFCE8F4),
                error_stripe_error: Rgb::hex(0xF24B4B),
                error_stripe_warning: Rgb::hex(0xF2CB4B),
                error_underline: Rgb::hex(0xFF0000),
                warning_underline: Rgb::hex(0xEBC700),
                weak_warning_underline: Rgb::hex(0xB0B0B0),
                typo_underline: Rgb::hex(0x3CB371),
                folded_text_bg: Rgb::hex(0xE9E9E9),
                folded_text_fg: Rgb::hex(0x6E6E6E),
                sticky_lines_bg: Rgb::hex(0xF7F8FA),
                inlay_hint_bg: Rgb::hex(0xEEEEEE),
                inlay_hint_fg: Rgb::hex(0x8C8C8C),
                added_line: Rgb::hex(0xC9DEC1),
                modified_line: Rgb::hex(0xB9CEE4),
                deleted_line: Rgb::hex(0xD6D6D6),
            },
            styles: styles.into_iter().collect(),
            inherits: BTreeMap::new(),
            context_colors: BTreeMap::new(),
        };
        crate::python_defaults::apply(&mut scheme, "Light");
        scheme
    }

    pub fn high_contrast() -> ColorScheme {
        use HighlightTag as T;
        let mut scheme = Self::dark();
        scheme.name = "High Contrast".into();
        scheme.editor.background = Rgb::hex(0x000000);
        scheme.editor.foreground = Rgb::hex(0xFFFFFF);
        scheme.editor.gutter_background = Rgb::hex(0x000000);
        scheme.editor.caret_row = Rgb::hex(0x1C1C1C);
        scheme.editor.line_number = Rgb::hex(0xC0C0C0);
        scheme.editor.selection = Rgb::hex(0x1A3B6E);
        let overrides = [
            (T::Default, TextStyle::fg(0xFFFFFF)),
            (T::Keyword, TextStyle::fg(0xFF8C00).bold()),
            (T::SoftKeyword, TextStyle::fg(0xFF8C00).bold()),
            (T::Builtin, TextStyle::fg(0xC792EA)),
            (T::FunctionDefinition, TextStyle::fg(0xFFFF00)),
            (T::ClassDefinition, TextStyle::fg(0xFFFFFF).bold()),
            (T::Number, TextStyle::fg(0x00AAFF)),
            (T::String, TextStyle::fg(0x1AEBFF)),
            (T::FString, TextStyle::fg(0x1AEBFF)),
            (T::Bytes, TextStyle::fg(0x1AEBFF)),
            (T::Docstring, TextStyle::fg(0x5FD75F).italic()),
            (T::Comment, TextStyle::fg(0xB3B3B3)),
            (T::Operator, TextStyle::fg(0xFFFFFF)),
            (T::Punctuation, TextStyle::fg(0xFFFFFF)),
            (T::Brace, TextStyle::fg(0xFFFFFF)),
            (T::Bracket, TextStyle::fg(0xFFFFFF)),
            (T::Parenthesis, TextStyle::fg(0xFFFFFF)),
            (T::Comma, TextStyle::fg(0xFFFFFF)),
            (T::Dot, TextStyle::fg(0xFFFFFF)),
            (T::Key, TextStyle::fg(0x5FE0E0)),
            (T::Tag, TextStyle::fg(0xFFB454)),
            (T::Heading, TextStyle::fg(0xFFFF00).bold()),
            (
                T::Link,
                TextStyle {
                    fg: Some(Rgb::hex(0x4FC3F7)),
                    underline: true,
                    ..Default::default()
                },
            ),
            (T::Variable, TextStyle::fg(0xC792EA)),
            (T::Unused, TextStyle::fg(0x9A9A9A)),
            (T::Rainbow1, TextStyle::fg(0xB4FF7A)),
            (T::Rainbow2, TextStyle::fg(0x7AC8FF)),
            (T::Rainbow3, TextStyle::fg(0xFFB37A)),
            (T::Rainbow4, TextStyle::fg(0xE59AFF)),
            (T::Rainbow5, TextStyle::fg(0x7AFFF0)),
            (T::Rainbow6, TextStyle::fg(0xFFF27A)),
        ];
        for (t, s) in overrides {
            scheme.styles.insert(t, s);
        }
        scheme.editor.foreground = Rgb::hex(0xebebeb);
        crate::python_defaults::apply(&mut scheme, "High Contrast");
        scheme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_code_is_gray_in_every_scheme() {
        for name in ColorScheme::builtin_names() {
            let s = ColorScheme::by_name(name).unwrap();
            let fg = s
                .style(HighlightTag::Unused)
                .fg
                .unwrap_or_else(|| panic!("{name}"));
            assert_ne!(fg, s.editor.foreground, "{name}");
        }
        // A custom scheme saved before the tag existed falls back to the comment color.
        let mut s = ColorScheme::dark();
        s.styles.remove(&HighlightTag::Unused);
        assert_eq!(
            s.style(HighlightTag::Unused).fg,
            s.style(HighlightTag::Comment).fg
        );
    }

    #[test]
    fn every_python_attribute_has_a_style_in_every_builtin_scheme() {
        use HighlightTag::*;
        let attributes = [
            Keyword,
            SoftKeyword,
            Builtin,
            Decorator,
            String,
            Bytes,
            FString,
            FStringBrace,
            EscapeSequence,
            Number,
            Operator,
            Parameter,
            SelfParameter,
            ClassDefinition,
            FunctionDefinition,
            Dunder,
            Comment,
            TodoComment,
            Docstring,
            TypeAnnotation,
            KeywordArgument,
        ];
        for name in ColorScheme::builtin_names() {
            let scheme = ColorScheme::by_name(name).unwrap();
            for attribute in attributes {
                assert!(
                    scheme.styles.contains_key(&attribute),
                    "{name}: {attribute:?}"
                );
                assert!(
                    scheme.style(attribute).fg.is_some(),
                    "{name}: {attribute:?}"
                );
            }
        }
    }

    #[test]
    fn actual_python_spans_select_custom_context_styles() {
        use HighlightTag::*;
        let mut scheme = ColorScheme::dark();
        let parameter = TextStyle::fg(0x123456).bold();
        let annotation = TextStyle::fg(0x654321).italic();
        let keyword = TextStyle::fg(0xABCDEF);
        scheme.styles.insert(Parameter, parameter);
        scheme.styles.insert(TypeAnnotation, annotation);
        scheme.styles.insert(KeywordArgument, keyword);
        let source = "def f(value: User = 1): pass\nf(argument=1)\n";
        let spans = crate::highlight_python(source);
        for (needle, expected) in [
            ("value", parameter),
            ("User", annotation),
            ("argument", keyword),
        ] {
            let offset = rustpy_core::TextSize::new(source.find(needle).unwrap() as u32);
            let tag = crate::python::tag_at(&spans, offset).unwrap();
            assert_eq!(scheme.style(tag), expected);
        }
    }
}
