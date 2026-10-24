//! Bounded shared matcher for editor lexical comments and TODO tool rows.
use regex::{Regex, RegexBuilder};
use rustpy_core::{TextRange, TextSize};
use rustpy_settings::todo::TodoSettings;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub range: TextRange,
    pub line: usize,
    pub text: String,
    pub color: [u8; 3],
}
#[derive(Debug)]
pub(crate) struct Patterns {
    regex: Vec<(Regex, [u8; 3])>,
    multiline: bool,
}
impl Patterns {
    pub fn compile(settings: &TodoSettings) -> Result<Self, String> {
        if settings.patterns.len() > 64 {
            return Err("At most 64 TODO patterns are supported".into());
        }
        let mut regex = Vec::new();
        for (index, pattern) in settings.patterns.iter().enumerate() {
            if pattern.regex.is_empty() || pattern.regex.len() > 4096 {
                return Err(format!("Pattern {} must contain 1–4096 bytes", index + 1));
            }
            let compiled = RegexBuilder::new(&pattern.regex)
                .case_insensitive(!pattern.case_sensitive)
                .size_limit(1024 * 1024)
                .build()
                .map_err(|error| format!("Pattern {}: {error}", index + 1))?;
            if compiled.is_match("") {
                return Err(format!("Pattern {} matches empty text", index + 1));
            }
            regex.push((compiled, pattern.color));
        }
        Ok(Self {
            regex,
            multiline: settings.multiline,
        })
    }
    /// Caller supplies actual lexical comment ranges, sorted in source order.
    /// No matches in strings/code; continuation requires consecutive comment
    /// lines and greater indentation AFTER the comment delimiter than marker.
    pub fn scan(&self, source: &str, comments: &[TextRange]) -> Vec<Item> {
        if source.len() > 1024 * 1024 {
            return Vec::new();
        }
        let mut out: Vec<Item> = Vec::new();
        let mut text_budget = 1024 * 1024;
        let mut previous: Option<(usize, usize, usize)> = None;
        let mut source_cursor = 0;
        let mut source_line = 0;
        for range in comments {
            let start = usize::from(range.start());
            if start < source_cursor {
                continue;
            }
            let Some(comment) = source.get(start..usize::from(range.end())) else {
                continue;
            };
            let Some(gap) = source.get(source_cursor..start) else {
                continue;
            };
            source_line += gap.bytes().filter(|byte| *byte == b'\n').count();
            let mut offset = usize::from(range.start());
            for raw in comment.split_inclusive('\n') {
                let line = source_line;
                let content = raw.trim_end_matches(['\r', '\n']);
                let content = content
                    .strip_prefix("<!--")
                    .or_else(|| content.strip_prefix("//"))
                    .or_else(|| content.strip_prefix("/*"))
                    .or_else(|| content.strip_prefix('#'))
                    .or_else(|| content.strip_prefix('*'))
                    .unwrap_or(content);
                let base = offset + (raw.trim_end_matches(['\r', '\n']).len() - content.len());
                let indent = content.len() - content.trim_start().len();
                let mut matches = Vec::new();
                let mut iterators = self
                    .regex
                    .iter()
                    .map(|(regex, color)| (regex.find_iter(content).peekable(), *color))
                    .collect::<Vec<_>>();
                // Merge bounded iterator heads: a dense earlier configured regex
                // cannot hide an earlier source match from a later regex.
                while matches.len() < 5000 {
                    let next = iterators
                        .iter_mut()
                        .enumerate()
                        .filter_map(|(priority, (iter, color))| {
                            while iter.peek().is_some_and(|found| found.is_empty()) {
                                iter.next();
                            }
                            iter.peek()
                                .map(|found| (found.start(), found.end(), priority, *color))
                        })
                        .min_by_key(|entry| (entry.0, entry.2));
                    let Some(candidate) = next else {
                        break;
                    };
                    iterators[candidate.2].0.next();
                    matches.push(candidate);
                }
                matches.sort_by_key(|entry| (entry.0, entry.2));
                matches.dedup_by_key(|entry| entry.0);
                if matches.is_empty() {
                    if self.multiline && !content.trim().is_empty() {
                        if let Some((index, last_line, marker_indent)) = previous {
                            if line == last_line + 1 && indent > marker_indent {
                                let remaining = 4096usize.saturating_sub(out[index].text.len());
                                if remaining > 1 && text_budget > 1 {
                                    out[index].text.push('\n');
                                    text_budget -= 1;
                                    let continuation = bounded_text(
                                        content.trim(),
                                        (remaining - 1).min(text_budget),
                                    );
                                    text_budget -= continuation.len();
                                    out[index].text.push_str(&continuation);
                                }
                                out[index].range = TextRange::new(
                                    out[index].range.start(),
                                    TextSize::new((base + content.len()) as u32),
                                );
                                previous = Some((index, line, marker_indent));
                            } else {
                                previous = None;
                            }
                        }
                    } else {
                        previous = None;
                    }
                } else {
                    for (start, _end, _priority, color) in matches {
                        if out.len() >= 5000 || text_budget < 4 {
                            return out;
                        }
                        let text = bounded_text(content[start..].trim(), 4096.min(text_budget));
                        text_budget -= text.len();
                        out.push(Item {
                            range: TextRange::new(
                                TextSize::new((base + start) as u32),
                                TextSize::new((base + content.len()) as u32),
                            ),
                            line,
                            text,
                            color,
                        });
                        previous = Some((out.len() - 1, line, start));
                    }
                }
                offset += raw.len();
                source_line += raw.bytes().filter(|byte| *byte == b'\n').count();
            }
            source_cursor = offset;
        }
        out
    }
}
/// Display text is capped independently of the authoritative full source range.
fn bounded_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    if limit < 3 {
        return String::new();
    }
    let mut end = limit - 3;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut display = text[..end].to_owned();
    display.push('…');
    display
}
/// Python tokenization distinguishes literal '# TODO' strings from comments.
#[cfg(test)]
pub(crate) fn python_comments(source: &str) -> Vec<TextRange> {
    rustpy_syntax::lexer::tokenize(source)
        .0
        .into_iter()
        .filter(|token| token.kind == rustpy_syntax::SyntaxKind::COMMENT)
        .map(|token| token.range)
        .collect()
}
/// Lexical contexts come from the same token/highlight authority as the editor.
pub(crate) fn contexts(
    path: &std::path::Path,
    source: &str,
    settings: &TodoSettings,
) -> Vec<TextRange> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("txt");
    if matches!(extension, "py" | "pyi" | "pyx") {
        return rustpy_syntax::lexer::tokenize(source)
            .0
            .into_iter()
            .filter(|token| {
                (settings.comments && token.kind == rustpy_syntax::SyntaxKind::COMMENT)
                    || (settings.strings
                        && matches!(
                            token.kind,
                            rustpy_syntax::SyntaxKind::STRING
                                | rustpy_syntax::SyntaxKind::FSTRING_MIDDLE
                                | rustpy_syntax::SyntaxKind::TSTRING_MIDDLE
                        ))
            })
            .map(|token| token.range)
            .collect();
    }
    if matches!(extension, "md" | "txt" | "rst") {
        return if settings.comments {
            vec![TextRange::new(
                TextSize::new(0),
                TextSize::new(source.len() as u32),
            )]
        } else {
            Vec::new()
        };
    }
    rustpy_highlight::highlight_for(extension, source)
        .into_iter()
        .filter(|span| {
            (settings.comments
                && matches!(
                    span.tag,
                    rustpy_highlight::HighlightTag::Comment
                        | rustpy_highlight::HighlightTag::TodoComment
                ))
                || (settings.strings
                    && matches!(
                        span.tag,
                        rustpy_highlight::HighlightTag::String
                            | rustpy_highlight::HighlightTag::Docstring
                            | rustpy_highlight::HighlightTag::FString
                    ))
        })
        .map(|span| span.range)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_settings::todo::TodoPattern;
    #[test]
    fn custom_case_color_unicode_and_lexical_comment_authority() {
        let settings = TodoSettings {
            patterns: vec![TodoPattern {
                regex: r"\bREVIEW\b".into(),
                case_sensitive: true,
                color: [2, 4, 8],
            }],
            multiline: true,
            ..Default::default()
        };
        let source="x = '# REVIEW false'\n# review ignored\n# REVIEW π😀\n#  continue Ω\n# no continuation\n";
        let items = Patterns::compile(&settings)
            .unwrap()
            .scan(source, &python_comments(source));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].line, 2);
        assert_eq!(items[0].color, [2, 4, 8]);
        assert_eq!(items[0].text, "REVIEW π😀\ncontinue Ω");
        assert_eq!(
            &source[usize::from(items[0].range.start())..usize::from(items[0].range.end())],
            "REVIEW π😀\n#  continue Ω"
        );
    }
    #[test]
    fn continuation_stops_on_code_blank_or_new_marker_and_option_off() {
        let source="# TODO first\n#  detail\nx=1\n#  unrelated\n# FIXME second\n#  more\n\n#   unrelated\n";
        let patterns = Patterns::compile(&TodoSettings::default()).unwrap();
        let items = patterns.scan(source, &python_comments(source));
        assert_eq!(
            items
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            ["TODO first\ndetail", "FIXME second\nmore"]
        );
        let settings = TodoSettings {
            multiline: false,
            ..Default::default()
        };
        assert_eq!(
            Patterns::compile(&settings)
                .unwrap()
                .scan(source, &python_comments(source))[0]
                .text,
            "TODO first"
        );
    }
    #[test]
    fn malformed_comment_ranges_are_ignored_without_losing_valid_comments() {
        let source = "π\n# TODO valid";
        let mut comments = vec![TextRange::new(TextSize::new(1), TextSize::new(2))];
        comments.extend(python_comments(source));
        comments.push(TextRange::new(TextSize::new(100), TextSize::new(101)));
        let items = Patterns::compile(&TodoSettings::default())
            .unwrap()
            .scan(source, &comments);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].line, 1);
        assert_eq!(items[0].text, "TODO valid");
    }
    #[test]
    fn dense_patterns_on_large_unicode_comment_have_bounded_candidates_and_text() {
        let settings = TodoSettings {
            patterns: (0..64)
                .map(|_| TodoPattern {
                    regex: ".".into(),
                    ..Default::default()
                })
                .collect(),
            multiline: true,
            ..Default::default()
        };
        let source = format!("#{}", "😀".repeat(250_000));
        let items = Patterns::compile(&settings)
            .unwrap()
            .scan(&source, &python_comments(&source));
        assert!(!items.is_empty());
        assert!(items.len() <= 5000);
        assert!(items.iter().map(|item| item.text.len()).sum::<usize>() <= 1024 * 1024);
        assert!(items.iter().all(|item| item.text.len() <= 4096));
        assert!(items[0].text.ends_with('…'));
        assert_eq!(usize::from(items[0].range.end()), source.len());
        assert_eq!(
            &source[usize::from(items[0].range.start())..usize::from(items[0].range.start()) + 4],
            "😀"
        );
    }
    #[test]
    fn dense_late_first_pattern_cannot_hide_early_later_pattern() {
        let settings = TodoSettings {
            patterns: vec![
                TodoPattern {
                    regex: "z".into(),
                    color: [1, 2, 3],
                    ..Default::default()
                },
                TodoPattern {
                    regex: "^ TODO".into(),
                    color: [4, 5, 6],
                    ..Default::default()
                },
            ],
            multiline: false,
            ..Default::default()
        };
        let source = format!("# TODO {}", "z".repeat(6000));
        let items = Patterns::compile(&settings)
            .unwrap()
            .scan(&source, &python_comments(&source));
        assert_eq!(items[0].color, [4, 5, 6]);
        assert_eq!(items[0].range.start(), TextSize::new(1));
        assert!(items[0].text.starts_with("TODO "));
        assert!(items.len() <= 5000);
    }
    #[test]
    fn invalid_patterns_fail_and_empty_configuration_disables_matches() {
        let mut settings = TodoSettings::default();
        settings.patterns[0].regex = "[".into();
        assert!(Patterns::compile(&settings).is_err());
        settings.patterns[0].regex = ".*".into();
        assert!(Patterns::compile(&settings).is_err());
        settings.patterns.clear();
        assert!(Patterns::compile(&settings)
            .unwrap()
            .scan("# TODO", &python_comments("# TODO"))
            .is_empty());
    }
}

#[cfg(test)]
mod context_fixtures {
    use super::*;
    #[test]
    fn strings_comments_and_multiline_share_lexical_authority() {
        let source="value = '# TODO literal'\n# TODO comment\n#        continued λ\nother = 1 # FIXME end\n";
        let mut settings = TodoSettings::default();
        let scan = |settings: &TodoSettings| {
            Patterns::compile(settings).unwrap().scan(
                source,
                &contexts(std::path::Path::new("source.py"), source, settings),
            )
        };
        let comments = scan(&settings);
        assert_eq!(comments.len(), 2);
        assert!(comments[0].text.contains("continued λ"));
        assert!(!comments.iter().any(|item| item.text.contains("literal")));
        settings.comments = false;
        settings.strings = true;
        let strings = scan(&settings);
        assert_eq!(strings.len(), 1);
        assert!(strings[0].text.contains("literal"));
        settings.strings = false;
        assert!(scan(&settings).is_empty());
        let js = "const title = 'TODO string'; // FIXME comment\n";
        settings.comments = true;
        let rows = Patterns::compile(&settings).unwrap().scan(
            js,
            &contexts(std::path::Path::new("source.js"), js, &settings),
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].text.starts_with("FIXME"));
    }
}
