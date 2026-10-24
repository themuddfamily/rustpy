//! Bounded finite-width proof for lookbehind plus semantics-preserving escape hints.
use crate::{
    tokenizer::{tokenize, RegexTokenKind as K},
    RegexError,
};
use rustpy_core::{TextRange, TextSize};
#[derive(Clone)]
struct Frame {
    range: TextRange,
    behind: bool,
    assertion: bool,
    current: Option<(u64, u64)>,
    last: Option<(u64, u64)>,
    branches: Vec<Option<(u64, u64)>>,
}
impl Frame {
    fn atom(&mut self, width: Option<(u64, u64)>) {
        self.current = self
            .current
            .zip(width)
            .map(|((a, b), (c, d))| (a.saturating_add(c), b.saturating_add(d)));
        self.last = width;
    }
    fn finish(&self) -> Option<(u64, u64)> {
        let mut widths = self.branches.clone();
        widths.push(self.current);
        let widths = widths.into_iter().collect::<Option<Vec<_>>>()?;
        Some((
            widths.iter().map(|w| w.0).min()?,
            widths.iter().map(|w| w.1).max()?,
        ))
    }
}
pub fn lookbehind_errors(pattern: &str, verbose: bool) -> Vec<RegexError> {
    if pattern.len() > 65536 {
        return vec![];
    }
    let tokens = tokenize(pattern, verbose);
    if tokens.len() > 4096 || tokens.iter().any(|t| t.kind == K::Error) {
        return vec![];
    }
    let mut stack = vec![Frame {
        range: TextRange::empty(TextSize::new(0)),
        behind: false,
        assertion: false,
        current: Some((0, 0)),
        last: None,
        branches: vec![],
    }];
    let mut class = false;
    let mut errors = Vec::new();
    for token in tokens {
        let text = &pattern[usize::from(token.range.start())..usize::from(token.range.end())];
        if class {
            if token.kind == K::CharClass && text == "]" {
                class = false;
            }
            continue;
        }
        if token.kind == K::CharClass && text.starts_with('[') {
            class = true;
            stack.last_mut().unwrap().atom(Some((1, 1)));
            continue;
        }
        match token.kind {
            K::GroupOpen => {
                if text == ">" {
                    continue;
                }
                if stack.len() >= 32 {
                    return vec![];
                }
                stack.push(Frame {
                    range: token.range,
                    behind: matches!(text, "(?<=" | "(?<!"),
                    assertion: matches!(text, "(?<=" | "(?<!" | "(?=" | "(?!"),
                    current: if text == "(?(" { None } else { Some((0, 0)) },
                    last: None,
                    branches: vec![],
                });
            }
            K::GroupClose => {
                if stack.len() == 1 {
                    return vec![];
                }
                let frame = stack.pop().unwrap();
                let width = frame.finish();
                if frame.behind && width.is_some_and(|(a, b)| a != b) {
                    errors.push(RegexError {
                        range: frame.range,
                        message: "look-behind requires fixed-width pattern".into(),
                    });
                }
                stack
                    .last_mut()
                    .unwrap()
                    .atom(if frame.assertion { Some((0, 0)) } else { width });
            }
            K::Alternation => {
                let frame = stack.last_mut().unwrap();
                frame.branches.push(frame.current);
                frame.current = Some((0, 0));
                frame.last = None;
            }
            K::Quantifier => {
                let frame = stack.last_mut().unwrap();
                let repeats = repeat(text);
                if let (Some((a, b)), Some((c, d)), Some((lo, hi))) =
                    (frame.current, frame.last, repeats)
                {
                    let width = (c.saturating_mul(lo), d.saturating_mul(hi));
                    frame.current = Some((
                        a.saturating_sub(c).saturating_add(width.0),
                        b.saturating_sub(d).saturating_add(width.1),
                    ));
                    frame.last = Some(width);
                } else {
                    frame.current = None;
                    frame.last = None;
                }
            }
            K::Literal | K::Dot | K::ClassRange => stack.last_mut().unwrap().atom(Some((1, 1))),
            K::Escape => {
                let reference = text.starts_with("(?P=")
                    || text.strip_prefix('\\').is_some_and(|s| {
                        s.starts_with(|c: char| c.is_ascii_digit() && c != '0')
                            || s.starts_with("g<")
                    });
                stack
                    .last_mut()
                    .unwrap()
                    .atom(if reference { None } else { Some((1, 1)) });
            }
            K::GroupName => {
                if text.starts_with("(?P=") {
                    stack.last_mut().unwrap().atom(None);
                }
            }
            K::Anchor | K::Flag | K::Comment => {}
            K::CharClass | K::Error => {}
        }
    }
    errors
}
fn repeat(text: &str) -> Option<(u64, u64)> {
    match text.as_bytes().first()? {
        b'*' => Some((0, u64::MAX)),
        b'+' => Some((1, u64::MAX)),
        b'?' => Some((0, 1)),
        b'{' => {
            let body = &text[1..text.find('}')?];
            if let Some((left, right)) = body.split_once(',') {
                Some((
                    if left.is_empty() {
                        0
                    } else {
                        left.parse().ok()?
                    },
                    if right.is_empty() {
                        u64::MAX
                    } else {
                        right.parse().ok()?
                    },
                ))
            } else {
                let value = body.parse().ok()?;
                Some((value, value))
            }
        }
        _ => None,
    }
}
pub fn redundant_escapes(pattern: &str, verbose: bool) -> Vec<RegexError> {
    if pattern.len() > 65536 {
        return vec![];
    }
    let mut class = false;
    let mut out = Vec::new();
    for token in tokenize(pattern, verbose).into_iter().take(4096) {
        let text = &pattern[usize::from(token.range.start())..usize::from(token.range.end())];
        if token.kind == K::CharClass {
            if text.starts_with('[') {
                class = true;
            } else if text == "]" {
                class = false;
            }
        }
        if !class
            && token.kind == K::Escape
            && text.len() == 2
            && text.as_bytes()[1].is_ascii_punctuation()
            && !b".\\+*?[^]$(){}|#".contains(&text.as_bytes()[1])
        {
            out.push(RegexError {
                range: token.range,
                message: "redundant escape of a literal character".into(),
            });
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lookbehind_widths_preserve_equal_alternatives_and_assertions() {
        for source in [
            "(?<=ab|cd)x",
            "(?<=a{2})x",
            "(?<=(?=a+)b)x",
            "(?<=\\d[ab])x",
        ] {
            assert!(lookbehind_errors(source, false).is_empty(), "{source}");
        }
        for source in ["(?<=a+)x", "(?<=a|bc)x", "(?<!a{1,2})x"] {
            assert_eq!(lookbehind_errors(source, false).len(), 1, "{source}");
        }
    }
    #[test]
    fn redundant_escapes_do_not_change_class_or_verbose_semantics() {
        assert_eq!(redundant_escapes(r"a\_b", false).len(), 1);
        assert!(redundant_escapes(r"[a\-z]\#\ ", true).is_empty());
    }
}
