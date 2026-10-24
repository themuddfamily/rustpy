//! Python lexer-backed Find contexts. Token categories, not color or regex guesses.
use crate::editor::Language;
use rustpy_core::TextRange;
use rustpy_syntax::SyntaxKind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    Anywhere,
    Comments,
    Strings,
    ExceptComments,
    ExceptStrings,
    ExceptCommentsAndStrings,
}
impl SearchFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::Anywhere => "Anywhere",
            Self::Comments => "Comments",
            Self::Strings => "Strings",
            Self::ExceptComments => "Except comments",
            Self::ExceptStrings => "Except strings",
            Self::ExceptCommentsAndStrings => "Except comments & strings",
        }
    }
}
pub fn supports_filter(language: Language) -> bool {
    matches!(language, Language::Python | Language::Cython)
}

pub(super) struct Contexts {
    filter: SearchFilter,
    comments: Vec<TextRange>,
    strings: Vec<TextRange>,
}
impl Contexts {
    pub fn new(text: &str, filter: SearchFilter) -> Self {
        let mut out = Self {
            filter,
            comments: Vec::new(),
            strings: Vec::new(),
        };
        if filter == SearchFilter::Anywhere {
            return out;
        }
        let (tokens, _) = rustpy_syntax::tokenize(text);
        for token in tokens {
            let ranges = match token.kind {
                SyntaxKind::COMMENT => &mut out.comments,
                SyntaxKind::STRING
                | SyntaxKind::FSTRING_START
                | SyntaxKind::FSTRING_MIDDLE
                | SyntaxKind::FSTRING_END
                | SyntaxKind::TSTRING_START
                | SyntaxKind::TSTRING_MIDDLE
                | SyntaxKind::TSTRING_END => &mut out.strings,
                _ => continue,
            };
            if let Some(last) = ranges
                .last_mut()
                .filter(|last| last.end() == token.range.start())
            {
                *last = TextRange::new(last.start(), token.range.end());
            } else {
                ranges.push(token.range);
            }
        }
        out
    }
    /// Nonempty include matches must fit wholly within one category interval.
    /// Exclusions reject any overlap. Zero-width matches belong to the token on
    /// their right (half-open intervals), so closing/EOF boundaries are code.
    pub fn accepts(&self, range: TextRange) -> bool {
        let intersects = |ranges: &[TextRange]| {
            let index = ranges.partition_point(|token| token.end() <= range.start());
            ranges.get(index).is_some_and(|token| {
                if range.is_empty() {
                    token.start() <= range.start() && range.start() < token.end()
                } else {
                    token.start() < range.end()
                }
            })
        };
        let contained = |ranges: &[TextRange]| {
            let index = ranges.partition_point(|token| token.end() <= range.start());
            ranges.get(index).is_some_and(|token| {
                token.start() <= range.start()
                    && range.end() <= token.end()
                    && (!range.is_empty() || range.start() < token.end())
            })
        };
        match self.filter {
            SearchFilter::Anywhere => true,
            SearchFilter::Comments => contained(&self.comments),
            SearchFilter::Strings => contained(&self.strings),
            SearchFilter::ExceptComments => !intersects(&self.comments),
            SearchFilter::ExceptStrings => !intersects(&self.strings),
            SearchFilter::ExceptCommentsAndStrings => {
                !intersects(&self.comments) && !intersects(&self.strings)
            }
        }
    }
}
