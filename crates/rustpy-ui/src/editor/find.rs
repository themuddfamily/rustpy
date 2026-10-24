//! In-file find and replace (roadmap §1.4): plain text or regular
//! expressions, match case, whole words, search in a selection, `$1` /
//! `${name}` groups and `\u` `\l` `\U` `\L` `\E` case conversions in regex
//! replacements, and "preserve case" for plain replacements.

use rustpy_core::{TextRange, TextSize};
use rustpy_text::Edit;

use super::state::{map_offset, Caret, EditorState, LastOp};
#[path = "find_filter.rs"]
mod find_filter;
use find_filter::Contexts;
pub use find_filter::{supports_filter, SearchFilter};

/// How the find bar matches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub regex: bool,
    /// Query editor mode; regex dots can span line breaks when enabled.
    pub multiline: bool,
    pub filter: SearchFilter,
    /// Plain replacements take the case style of each match (`foo`/`Foo`/`FOO`).
    pub preserve_case: bool,
    /// Search only inside this range (find in selection).
    pub scope: Option<TextRange>,
}

/// Highlighting stops after this many matches (huge files, `.` patterns).
pub const MAX_MATCHES: usize = 10_000;

/// Compiles `query`; regex errors come back as a one-line message.
pub fn compile(query: &str, opts: &FindOptions) -> Result<regex::Regex, String> {
    let short = |e: regex::Error| {
        e.to_string()
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("invalid regular expression")
            .trim_start_matches("error: ")
            .to_string()
    };
    let pattern = if opts.regex {
        // Validate the user's pattern alone so the message does not show our wrapping.
        regex::Regex::new(query).map_err(short)?;
        query.to_string()
    } else {
        regex::escape(query)
    };
    // Whole words: a boundary only on the sides where the plain-text query
    // starts or ends with a word character (`foo(` must still find `foo()`).
    let pattern = if opts.whole_words {
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let (left, right) = if opts.regex {
            (true, true)
        } else {
            (word(query.chars().next()), word(query.chars().next_back()))
        };
        format!(
            "{}(?:{pattern}){}",
            if left { r"\b" } else { "" },
            if right { r"\b" } else { "" }
        )
    } else {
        pattern
    };
    // `^` and `$` match at line boundaries, as in a find bar.
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!opts.case_sensitive)
        .multi_line(true)
        .dot_matches_new_line(opts.multiline)
        .build()
        .map_err(short)
}

/// The searched byte range: the scope (on character boundaries), or the whole text.
fn scope_bounds(text: &str, scope: Option<TextRange>) -> (usize, usize) {
    let floor = |i: usize| {
        (0..=i.min(text.len()))
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0)
    };
    match scope {
        Some(r) => {
            let end = floor(usize::from(r.end()));
            (floor(usize::from(r.start())).min(end), end)
        }
        None => (0, text.len()),
    }
}

fn next_char_boundary(text: &str, i: usize) -> usize {
    text[i..]
        .chars()
        .next()
        .map(|c| i + c.len_utf8())
        .unwrap_or(text.len() + 1)
}

/// The matches of a regex inside `[start, end)`, in order and without a limit.
/// The text before `start` still counts for `^` and `\b`; the scope end acts as
/// the end of the text. Like `find_iter`, an empty match right after a match is
/// skipped.
struct Matches<'r, 't> {
    re: &'r regex::Regex,
    hay: &'t str,
    pos: usize,
    last_end: Option<usize>,
}

impl Iterator for Matches<'_, '_> {
    type Item = std::ops::Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos <= self.hay.len() {
            let m = self.re.find_at(self.hay, self.pos)?;
            if m.is_empty() && self.last_end == Some(m.start()) {
                self.pos = next_char_boundary(self.hay, m.start());
                continue;
            }
            // No empty match after the final line break: `^` does
            // not match a line that does not exist.
            if m.is_empty() && m.start() == self.hay.len() && self.hay.ends_with('\n') {
                return None;
            }
            self.pos = if m.is_empty() {
                next_char_boundary(self.hay, m.end())
            } else {
                m.end()
            };
            self.last_end = Some(m.end());
            return Some(m.range());
        }
        None
    }
}

fn match_ranges<'r, 't>(
    re: &'r regex::Regex,
    text: &'t str,
    start: usize,
    end: usize,
) -> Matches<'r, 't> {
    Matches {
        re,
        hay: &text[..end],
        pos: start.min(end),
        last_end: None,
    }
}

fn to_range(r: std::ops::Range<usize>) -> TextRange {
    TextRange::new(TextSize::new(r.start as u32), TextSize::new(r.end as u32))
}

/// All matches of `query` in `text` (inside `opts.scope`), at most [`MAX_MATCHES`].
pub fn find_matches(text: &str, query: &str, opts: &FindOptions) -> Result<Vec<TextRange>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let re = compile(query, opts)?;
    let (start, end) = scope_bounds(text, opts.scope);
    let contexts = Contexts::new(text, opts.filter);
    Ok(match_ranges(&re, text, start, end)
        .map(to_range)
        .filter(|range| contexts.accepts(*range))
        .take(MAX_MATCHES)
        .collect())
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum CaseMode {
    None,
    Upper,
    Lower,
}

/// Expands a regex replacement template for one match: `$0`..`$99`, `${n}`,
/// `${name}`, `$$`, `\n` `\t`, `\$` `\\`, and the case conversions `\u` / `\l`
/// (next character) and `\U` / `\L` … `\E` (until `\E`).
pub fn expand_template(caps: &regex::Captures, template: &str) -> String {
    let mut out = String::new();
    let mut mode = CaseMode::None;
    let mut once = CaseMode::None;
    let emit = |s: &str, mode: CaseMode, once: &mut CaseMode, out: &mut String| {
        for c in s.chars() {
            let m = if *once != CaseMode::None {
                std::mem::replace(once, CaseMode::None)
            } else {
                mode
            };
            match m {
                CaseMode::Upper => out.extend(c.to_uppercase()),
                CaseMode::Lower => out.extend(c.to_lowercase()),
                CaseMode::None => out.push(c),
            }
        }
    };
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            i += 2;
            match chars[i - 1] {
                'n' => emit("\n", mode, &mut once, &mut out),
                't' => emit("\t", mode, &mut once, &mut out),
                'u' => once = CaseMode::Upper,
                'l' => once = CaseMode::Lower,
                'U' => mode = CaseMode::Upper,
                'L' => mode = CaseMode::Lower,
                'E' => mode = CaseMode::None,
                other => emit(&other.to_string(), mode, &mut once, &mut out),
            }
            continue;
        }
        if c == '$' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if next == '$' {
                emit("$", mode, &mut once, &mut out);
                i += 2;
                continue;
            }
            if next == '{' {
                if let Some(close) = chars[i + 2..].iter().position(|c| *c == '}') {
                    let name: String = chars[i + 2..i + 2 + close].iter().collect();
                    let group = match name.parse::<usize>() {
                        Ok(n) => caps.get(n),
                        Err(_) => caps.name(&name),
                    };
                    emit(
                        group.map(|m| m.as_str()).unwrap_or(""),
                        mode,
                        &mut once,
                        &mut out,
                    );
                    i += 3 + close;
                    continue;
                }
            }
            if next.is_ascii_digit() {
                // Java style: take the longest digit run that names an existing group.
                let mut n = next.to_digit(10).unwrap_or(0) as usize;
                let mut j = i + 2;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    let longer = n * 10 + chars[j].to_digit(10).unwrap_or(0) as usize;
                    if longer >= caps.len() {
                        break;
                    }
                    n = longer;
                    j += 1;
                }
                emit(
                    caps.get(n).map(|m| m.as_str()).unwrap_or(""),
                    mode,
                    &mut once,
                    &mut out,
                );
                i = j;
                continue;
            }
        }
        emit(&c.to_string(), mode, &mut once, &mut out);
        i += 1;
    }
    out
}

/// "Preserve case": `replacement` takes the case style of `found`: the case of
/// its first letter, and an all-upper (or all-lower) rest when the
/// replacement's rest is uniformly cased the other way (`foo`→`bar`,
/// `Foo`→`Bar`, `FOO`→`BAR`; `foo`→`barBaz` keeps its humps).
pub fn preserve_case(found: &str, replacement: &str) -> String {
    let mut f = found.chars();
    let mut r = replacement.chars();
    let (Some(f0), Some(r0)) = (f.next(), r.next()) else {
        return replacement.to_string();
    };
    let mut out = String::new();
    if f0.is_uppercase() {
        out.extend(r0.to_uppercase());
    } else if f0.is_lowercase() {
        out.extend(r0.to_lowercase());
    } else {
        out.push(r0);
    }
    let r_tail: String = r.collect();
    let letters =
        |s: &mut dyn Iterator<Item = char>| s.filter(|c| c.is_alphabetic()).collect::<Vec<char>>();
    let f_tail = letters(&mut f);
    let r_letters = letters(&mut r_tail.chars());
    let (f_upper, f_lower) = (
        !f_tail.is_empty() && f_tail.iter().all(|c| c.is_uppercase()),
        !f_tail.is_empty() && f_tail.iter().all(|c| c.is_lowercase()),
    );
    let (r_upper, r_lower) = (
        r_letters.iter().all(|c| c.is_uppercase()),
        r_letters.iter().all(|c| c.is_lowercase()),
    );
    if f_upper && r_lower {
        out.push_str(&r_tail.to_uppercase());
    } else if f_lower && r_upper {
        out.push_str(&r_tail.to_lowercase());
    } else {
        out.push_str(&r_tail);
    }
    out
}

/// The text that replaces the match `range` of `re` in `text`.
fn replacement_for(
    re: &regex::Regex,
    text: &str,
    range: TextRange,
    end: usize,
    template: &str,
    opts: &FindOptions,
) -> String {
    let found = &text[usize::from(range.start())..usize::from(range.end())];
    if opts.regex {
        match re.captures_at(&text[..end], usize::from(range.start())) {
            Some(caps)
                if caps
                    .get(0)
                    .is_some_and(|m| m.start() == usize::from(range.start())) =>
            {
                expand_template(&caps, template)
            }
            _ => template.to_string(),
        }
    } else if opts.preserve_case {
        preserve_case(found, template)
    } else {
        template.to_string()
    }
}

/// One immutable planned replacement (current document byte range).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementOccurrence {
    pub range: TextRange,
    pub original: String,
    pub replacement: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplacementPreview {
    pub rows: Vec<ReplacementOccurrence>,
    /// More changed matches exist, or a large expansion was omitted safely.
    pub has_more: bool,
}
const MAX_PREVIEW_ROWS: usize = 200;
const MAX_PREVIEW_BYTES: usize = 256 * 1024;

impl EditorState {
    fn check_find_filter(&self, opts: &FindOptions) -> Result<(), String> {
        if opts.filter != SearchFilter::Anywhere && !supports_filter(self.language) {
            return Err("Comment/string search filters are available for Python and Cython".into());
        }
        Ok(())
    }
    /// Bounded immutable preview of the same filtered, non-excluded edits used
    /// by Replace All. No document/caret/history or read-only refusal mutation.
    pub fn replacement_preview(
        &self,
        query: &str,
        template: &str,
        opts: &FindOptions,
        excluded: &[TextRange],
        limit: usize,
    ) -> Result<ReplacementPreview, String> {
        self.check_find_filter(opts)?;
        let mut preview = ReplacementPreview::default();
        if query.is_empty() {
            return Ok(preview);
        }
        let re = compile(query, opts)?;
        let text = self.doc.text();
        let (start, end) = scope_bounds(&text, opts.scope);
        let contexts = Contexts::new(&text, opts.filter);
        let limit = limit.min(MAX_PREVIEW_ROWS);
        let mut used = 0usize;
        for range in match_ranges(&re, &text, start, end)
            .map(to_range)
            .filter(|range| contexts.accepts(*range) && !excluded.contains(range))
        {
            let original = &text[usize::from(range.start())..usize::from(range.end())];
            // A template character can reference an entire match. Unicode case
            // expansion is conservatively bounded before allocating output.
            let bound = if opts.regex {
                template
                    .len()
                    .saturating_mul(original.len().max(1).saturating_mul(12).saturating_add(1))
            } else if opts.preserve_case {
                template.len().saturating_mul(12)
            } else {
                template.len()
            };
            if used.saturating_add(original.len()).saturating_add(bound) > MAX_PREVIEW_BYTES {
                preview.has_more = true;
                break;
            }
            let replacement = replacement_for(&re, &text, range, end, template, opts);
            if replacement == original {
                continue;
            }
            if preview.rows.len() == limit {
                preview.has_more = true;
                break;
            }
            used += original.len() + replacement.len();
            preview.rows.push(ReplacementOccurrence {
                range,
                original: original.to_string(),
                replacement,
            });
        }
        Ok(preview)
    }

    /// All matches of `query` in the document (inside `opts.scope`).
    pub fn find_matches(&self, query: &str, opts: &FindOptions) -> Result<Vec<TextRange>, String> {
        self.check_find_filter(opts)?;
        find_matches(&self.doc.text(), query, opts)
    }

    /// Selects the next (or previous) match after (before) the primary caret,
    /// wrapping around. `incremental` keeps a match that starts at the current
    /// selection (search-as-you-type extends it in place). Returns the match.
    pub fn find_with(
        &mut self,
        query: &str,
        opts: &FindOptions,
        backwards: bool,
        incremental: bool,
    ) -> Result<Option<TextRange>, String> {
        if query.is_empty() {
            return Ok(None);
        }
        self.check_find_filter(opts)?;
        let re = compile(query, opts)?;
        let text = self.doc.text();
        let (start, end) = scope_bounds(&text, opts.scope);
        // Every match counts (not only the first MAX_MATCHES that are highlighted).
        let contexts = Contexts::new(&text, opts.filter);
        let all = || {
            match_ranges(&re, &text, start, end)
                .map(to_range)
                .filter(|range| contexts.accepts(*range))
        };
        let k = self.primary().range();
        let found = if backwards {
            // The last match before the selection, else (wrapping around) the last one.
            let (mut before, mut last) = (None, None);
            for m in all() {
                if before.is_some() && m.start() >= k.start() {
                    break;
                }
                if m.end() <= k.start() && m != k {
                    before = Some(m);
                }
                last = Some(m);
            }
            before.or(last)
        } else if incremental {
            all()
                .find(|m| m.start() >= k.start())
                .or_else(|| all().next())
        } else if k.is_empty() {
            // From a plain caret: a match starting at the caret counts, unless it is empty (no progress).
            all()
                .find(|m| m.start() > k.start() || (m.start() == k.start() && !m.is_empty()))
                .or_else(|| all().next())
        } else {
            all()
                .find(|m| m.start() >= k.end() && *m != k)
                .or_else(|| all().next())
        };
        if let Some(m) = found {
            self.reveal_offset(m.start());
            self.set_carets(vec![Caret::new(m.start(), m.end())]);
            self.last_op = LastOp::Other;
        }
        Ok(found)
    }

    /// Replaces the selection when it is a match, then selects the next match.
    /// Returns whether something was replaced.
    pub fn replace_current_with(
        &mut self,
        query: &str,
        template: &str,
        opts: &FindOptions,
    ) -> Result<bool, String> {
        if query.is_empty() {
            return Ok(false);
        }
        self.check_find_filter(opts)?;
        let re = compile(query, opts)?;
        let text = self.doc.text();
        let (start, end) = scope_bounds(&text, opts.scope);
        let k = self.primary().range();
        let contexts = Contexts::new(&text, opts.filter);
        let hit = match_ranges(&re, &text, start, end)
            .map(to_range)
            .filter(|range| contexts.accepts(*range))
            .take_while(|m| m.start() <= k.start())
            .find(|m| *m == k);
        let mut replaced = false;
        if let Some(m) = hit {
            let new_text = replacement_for(&re, &text, m, end, template, opts);
            let caret = m.start() + TextSize::new(new_text.len() as u32);
            let group = self.group_for(LastOp::Other);
            replaced = self.apply(
                vec![Edit::replace(m, new_text)],
                vec![Caret::at(caret)],
                group,
                Some("Replace"),
            );
        }
        // The scope grew or shrank with the replacement.
        let mut next_opts = opts.clone();
        if let (Some(scope), Some(m), true) = (opts.scope, hit, replaced) {
            let delta = usize::from(self.doc.len()) as i64 - text.len() as i64;
            let end =
                (usize::from(scope.end()) as i64 + delta).max(usize::from(m.start()) as i64) as u32;
            next_opts.scope = Some(TextRange::new(scope.start(), TextSize::new(end)));
        }
        self.find_with(query, &next_opts, false, false)?;
        Ok(replaced)
    }

    /// Replaces every match except the `excluded` ones as one undo step;
    /// returns how many were replaced.
    pub fn replace_all_with(
        &mut self,
        query: &str,
        template: &str,
        opts: &FindOptions,
        excluded: &[TextRange],
    ) -> Result<usize, String> {
        if query.is_empty() {
            return Ok(0);
        }
        self.check_find_filter(opts)?;
        let re = compile(query, opts)?;
        let text = self.doc.text();
        let (start, end) = scope_bounds(&text, opts.scope);
        let contexts = Contexts::new(&text, opts.filter);
        let edits: Vec<Edit> = match_ranges(&re, &text, start, end)
            .map(to_range)
            .filter(|range| contexts.accepts(*range))
            .filter(|m| !excluded.contains(m))
            .map(|m| Edit::replace(m, replacement_for(&re, &text, m, end, template, opts)))
            .filter(|e| text[usize::from(e.range.start())..usize::from(e.range.end())] != *e.text)
            .collect();
        if edits.is_empty() {
            return Ok(0);
        }
        let n = edits.len();
        let carets = self
            .carets
            .iter()
            .map(|k| Caret::at(map_offset(&edits, k.head(), true)))
            .collect();
        let group = self.group_for(LastOp::Other);
        if self.apply(edits, carets, group, Some("Replace All")) {
            Ok(n)
        } else {
            Ok(0)
        }
    }

    /// Alt+Enter in the find bar: one selection per match (at most [`MAX_MATCHES`]).
    pub fn select_all_matches(&mut self, query: &str, opts: &FindOptions) -> Result<usize, String> {
        let matches: Vec<TextRange> = self
            .find_matches(query, opts)?
            .into_iter()
            .filter(|m| !m.is_empty())
            .collect();
        if matches.is_empty() {
            return Ok(0);
        }
        // Keep the match at the caret primary.
        let k = self.primary().range();
        let first = matches
            .iter()
            .position(|m| *m == k || m.start() >= k.start())
            .unwrap_or(0);
        let mut carets: Vec<Caret> = vec![Caret::new(matches[first].start(), matches[first].end())];
        carets.extend(
            matches
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != first)
                .map(|(_, m)| Caret::new(m.start(), m.end())),
        );
        let n = carets.len();
        self.set_carets(carets);
        self.last_op = LastOp::Other;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Language;

    fn ts(n: u32) -> TextSize {
        TextSize::new(n)
    }

    fn opts() -> FindOptions {
        FindOptions::default()
    }

    fn texts(text: &str, ranges: &[TextRange]) -> Vec<String> {
        ranges
            .iter()
            .map(|r| text[usize::from(r.start())..usize::from(r.end())].to_string())
            .collect()
    }

    #[test]
    fn plain_case_and_whole_words() {
        let text = "Foo foo food FOO";
        assert_eq!(
            texts(text, &find_matches(text, "foo", &opts()).unwrap()),
            ["Foo", "foo", "foo", "FOO"]
        );
        let cs = FindOptions {
            case_sensitive: true,
            ..opts()
        };
        assert_eq!(
            texts(text, &find_matches(text, "foo", &cs).unwrap()),
            ["foo", "foo"]
        );
        let words = FindOptions {
            whole_words: true,
            ..opts()
        };
        assert_eq!(
            texts(text, &find_matches(text, "foo", &words).unwrap()),
            ["Foo", "foo", "FOO"]
        );
        // Regex metacharacters are literal in plain mode.
        assert_eq!(find_matches("a.b axb", "a.b", &opts()).unwrap().len(), 1);
    }

    #[test]
    fn regex_lines_and_errors() {
        let text = "x = 1\n  y = 22\n";
        let re = FindOptions {
            regex: true,
            ..opts()
        };
        assert_eq!(
            texts(
                text,
                &find_matches(text, r"^\s*(\w+) = (\d+)$", &re).unwrap()
            ),
            ["x = 1", "  y = 22"]
        );
        let err = find_matches(text, "(", &re).unwrap_err();
        assert!(!err.contains('\n') && err.contains("unclosed"), "{err}");
        // `^` matches every line start (empty matches are allowed for replace),
        // but not after the final line break (there is no line there).
        assert_eq!(find_matches("a\nb\n", "^", &re).unwrap().len(), 2);
    }

    #[test]
    fn scope_limits_matches_but_keeps_context() {
        let text = "aba aba aba";
        let scoped = FindOptions {
            scope: Some(TextRange::new(ts(2), ts(11))),
            ..opts()
        };
        let found = find_matches(text, "aba", &scoped).unwrap();
        assert_eq!(
            found,
            vec![TextRange::new(ts(4), ts(7)), TextRange::new(ts(8), ts(11))]
        );
        // `\b` sees the character before the scope: "ba" in "aba" is not a word start.
        let words = FindOptions {
            whole_words: true,
            scope: Some(TextRange::new(ts(1), ts(11))),
            ..opts()
        };
        assert_eq!(find_matches(text, "ba", &words).unwrap(), vec![]);
    }

    #[test]
    fn replacement_groups_and_case_conversions() {
        let re = regex::Regex::new(r"(?P<first>\w+)_(\w+)").unwrap();
        let caps = re.captures("snake_case").unwrap();
        assert_eq!(expand_template(&caps, "$2_$1"), "case_snake");
        assert_eq!(expand_template(&caps, "${first}-${2}"), "snake-case");
        assert_eq!(expand_template(&caps, r"$1\u$2"), "snakeCase");
        assert_eq!(expand_template(&caps, r"\U$1\E_$2"), "SNAKE_case");
        assert_eq!(expand_template(&caps, r"\L\uHELLO\E!"), "Hello!");
        assert_eq!(
            expand_template(&caps, r"$$1 \$ \\ a\tb\nc"),
            "$1 $ \\ a\tb\nc"
        );
        // `$12` with two groups is group 1 followed by "2".
        assert_eq!(expand_template(&caps, "$12"), "snake2");
        assert_eq!(expand_template(&caps, "$0!"), "snake_case!");
    }

    #[test]
    fn preserve_case_styles() {
        assert_eq!(preserve_case("foo", "bar"), "bar");
        assert_eq!(preserve_case("Foo", "bar"), "Bar");
        assert_eq!(preserve_case("FOO", "bar"), "BAR");
        assert_eq!(preserve_case("foo", "BarBaz"), "barBaz");
        assert_eq!(preserve_case("Foo", "BAR"), "Bar");
        assert_eq!(
            preserve_case("FOO", "barBaz"),
            "BarBaz",
            "mixed-case replacement keeps its humps"
        );
        assert_eq!(preserve_case("FooBar", "bazQux"), "BazQux");
        assert_eq!(preserve_case("x", "yz"), "yz");
        assert_eq!(preserve_case("", "yz"), "yz");
    }

    #[test]
    fn editor_replace_with_regex_groups_and_exclusions() {
        let mut e = EditorState::from_text("get_x()\nget_y()\nget_z()\n", Language::Python);
        let re = FindOptions {
            regex: true,
            ..opts()
        };
        let all = e.find_matches(r"get_(\w)", &re).unwrap();
        assert_eq!(all.len(), 3);
        let n = e
            .replace_all_with(r"get_(\w)", r"\u$1_getter", &re, &all[1..2])
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(e.doc.text(), "X_getter()\nget_y()\nZ_getter()\n");
        assert!(e.undo());
        assert_eq!(e.doc.text(), "get_x()\nget_y()\nget_z()\n", "one undo step");
    }

    #[test]
    fn editor_preserve_case_replace_and_navigation() {
        let mut e = EditorState::from_text("Value value VALUE", Language::PlainText);
        let pc = FindOptions {
            preserve_case: true,
            ..opts()
        };
        assert_eq!(e.replace_all_with("value", "item", &pc, &[]).unwrap(), 3);
        assert_eq!(e.doc.text(), "Item item ITEM");
        // Replace one at a time: the selection must be a match first.
        let mut e = EditorState::from_text("a1 a2 a3", Language::PlainText);
        let re = FindOptions {
            regex: true,
            ..opts()
        };
        assert_eq!(
            e.find_with(r"a(\d)", &re, false, false).unwrap(),
            Some(TextRange::new(ts(0), ts(2)))
        );
        assert!(e.replace_current_with(r"a(\d)", "b$1", &re).unwrap());
        assert_eq!(e.doc.text(), "b1 a2 a3");
        assert_eq!(
            e.primary().range(),
            TextRange::new(ts(3), ts(5)),
            "moves to the next match"
        );
        assert_eq!(
            e.find_with(r"a(\d)", &re, true, false).unwrap(),
            Some(TextRange::new(ts(6), ts(8))),
            "backwards wraps around"
        );
    }

    #[test]
    fn each_replace_takes_the_next_occurrence() {
        // Two clicks on Replace change occurrences 1 and 2: Replace moves on by
        // itself, so the bar must not step once more after it (that skipped one).
        let mut e = EditorState::from_text(
            "def f(prefix):\n    self.prefix = prefix\n",
            Language::Python,
        );
        assert!(e
            .find_with("prefix", &opts(), false, true)
            .unwrap()
            .is_some());
        assert!(e.replace_current_with("prefix", "pfx", &opts()).unwrap());
        assert!(e.replace_current_with("prefix", "pfx", &opts()).unwrap());
        assert_eq!(e.doc.text(), "def f(pfx):\n    self.pfx = prefix\n");
        assert_eq!(
            e.doc.slice(e.primary().range()),
            "prefix",
            "the remaining occurrence is next"
        );
        assert_eq!(e.primary().range().start(), ts(27));
    }

    #[test]
    fn in_selection_replace_all_and_select_all_matches() {
        let mut e = EditorState::from_text("x x\nx x\nx x\n", Language::PlainText);
        let scoped = FindOptions {
            scope: Some(TextRange::new(ts(4), ts(7))),
            ..opts()
        };
        assert_eq!(e.replace_all_with("x", "yy", &scoped, &[]).unwrap(), 2);
        assert_eq!(e.doc.text(), "x x\nyy yy\nx x\n");
        let mut e = EditorState::from_text("ab ab ab", Language::PlainText);
        assert_eq!(e.select_all_matches("ab", &opts()).unwrap(), 3);
        assert_eq!(e.carets.len(), 3);
        assert!(e.carets.iter().all(|c| e.doc.slice(c.range()) == "ab"));
    }

    #[test]
    fn navigation_and_replace_past_the_highlight_limit() {
        // More matches than are highlighted: F3 / Shift+F3 / Replace still see all of them.
        let n = MAX_MATCHES + 10;
        let mut e = EditorState::from_text(&"x ".repeat(n), Language::PlainText);
        assert_eq!(e.find_matches("x", &opts()).unwrap().len(), MAX_MATCHES);
        let at = |i: usize| TextRange::new(ts(2 * i as u32), ts(2 * i as u32 + 1));
        e.set_carets(vec![Caret::new(
            at(MAX_MATCHES + 1).start(),
            at(MAX_MATCHES + 1).end(),
        )]);
        assert_eq!(
            e.find_with("x", &opts(), false, false).unwrap(),
            Some(at(MAX_MATCHES + 2)),
            "no wrap to the top"
        );
        assert_eq!(
            e.find_with("x", &opts(), true, false).unwrap(),
            Some(at(MAX_MATCHES + 1))
        );
        e.set_carets(vec![Caret::new(at(n - 1).start(), at(n - 1).end())]);
        assert_eq!(
            e.find_with("x", &opts(), false, false).unwrap(),
            Some(at(0)),
            "wraps after the last match"
        );
        assert_eq!(
            e.find_with("x", &opts(), true, false).unwrap(),
            Some(at(n - 1)),
            "backwards from the first wraps to the last"
        );
        e.set_carets(vec![Caret::new(
            at(MAX_MATCHES + 3).start(),
            at(MAX_MATCHES + 3).end(),
        )]);
        assert!(e.replace_current_with("x", "y", &opts()).unwrap());
        assert_eq!(
            &e.doc.text()[2 * (MAX_MATCHES + 3)..2 * (MAX_MATCHES + 3) + 1],
            "y"
        );
    }

    #[test]
    fn scope_inside_a_multibyte_character_is_safe() {
        let text = "aé b";
        // 2 is inside "é" (bytes 1..3): the scope ends at the character start.
        let scoped = FindOptions {
            scope: Some(TextRange::new(ts(0), ts(2))),
            ..opts()
        };
        assert_eq!(
            find_matches(text, "a", &scoped).unwrap(),
            vec![TextRange::new(ts(0), ts(1))]
        );
        let inner = FindOptions {
            scope: Some(TextRange::new(ts(2), ts(5))),
            ..opts()
        };
        assert_eq!(
            find_matches(text, "b", &inner).unwrap(),
            vec![TextRange::new(ts(4), ts(5))]
        );
    }

    #[test]
    fn incremental_search_extends_in_place() {
        let mut e = EditorState::from_text("foo fool food", Language::PlainText);
        assert_eq!(
            e.find_with("fo", &opts(), false, true)
                .unwrap()
                .map(|r| r.start()),
            Some(ts(0))
        );
        assert_eq!(
            e.find_with("foo", &opts(), false, true)
                .unwrap()
                .map(|r| r.start()),
            Some(ts(0))
        );
        assert_eq!(
            e.find_with("fool", &opts(), false, true)
                .unwrap()
                .map(|r| r.start()),
            Some(ts(4))
        );
        assert_eq!(
            e.find_with("foo", &opts(), false, false)
                .unwrap()
                .map(|r| r.start()),
            Some(ts(9))
        );
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    fn opts() -> FindOptions {
        FindOptions::default()
    }

    fn ts(n: u32) -> TextSize {
        TextSize::new(n)
    }

    #[test]
    fn caret_does_not_match_after_the_final_newline_and_whole_words_allow_punctuation() {
        let opts = FindOptions {
            regex: true,
            ..Default::default()
        };
        assert_eq!(find_matches("ab\ncd\n", "^", &opts).unwrap().len(), 2);
        let opts = FindOptions {
            whole_words: true,
            ..Default::default()
        };
        assert_eq!(
            find_matches("foo(x) foo() xfoo(", "foo(", &opts)
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn all_six_filters_use_python_tokens_including_fstring_expression_code() {
        let source = "needle = \"needle\" # needle\nf\"needle {needle} { 'needle' }\"\n'''needle\nneedle'''\n";
        for (filter, count) in [
            (SearchFilter::Anywhere, 8),
            (SearchFilter::Comments, 1),
            (SearchFilter::Strings, 5),
            (SearchFilter::ExceptComments, 7),
            (SearchFilter::ExceptStrings, 3),
            (SearchFilter::ExceptCommentsAndStrings, 2),
        ] {
            let options = FindOptions { filter, ..opts() };
            assert_eq!(
                find_matches(source, "needle", &options).unwrap().len(),
                count,
                "{filter:?}"
            );
        }
        let options = FindOptions {
            filter: SearchFilter::Strings,
            ..opts()
        };
        assert_eq!(
            find_matches(
                "x = r\"# needle\"; y = b'needle'; z = \"escaped \\\" needle\"\n# needle\n",
                "needle",
                &options
            )
            .unwrap()
            .len(),
            3
        );
        assert_eq!(
            find_matches("x = 'unterminated needle\n# needle\n", "needle", &options)
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn filters_precede_highlight_cap_and_preserve_navigation_and_exclusions() {
        let source = "needle = 0\n".repeat(MAX_MATCHES + 1) + "# needle\n# needle\n";
        let mut state = EditorState::from_text(&source, super::super::Language::Python);
        let options = FindOptions {
            filter: SearchFilter::Comments,
            ..opts()
        };
        let matches = state.find_matches("needle", &options).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(
            state.find_with("needle", &options, false, false).unwrap(),
            Some(matches[0])
        );
        assert_eq!(
            state.find_with("needle", &options, true, false).unwrap(),
            Some(matches[1])
        );
        assert_eq!(state.select_all_matches("needle", &options).unwrap(), 2);
        assert_eq!(
            state
                .replace_all_with("needle", "changed", &options, &[matches[1]])
                .unwrap(),
            1
        );
        assert!(state.doc.text().ends_with("# changed\n# needle\n"));
        assert!(state.doc.text().starts_with("needle = 0\n"));
        assert!(state.undo());
        assert_eq!(state.doc.text(), source);
    }
    #[test]
    fn multiline_dot_and_cross_category_matches_have_explicit_boundaries() {
        let source = "x = \"π\" # comment\ny = \"😀\"\n";
        let regex = FindOptions {
            regex: true,
            ..opts()
        };
        assert!(find_matches(source, "π.*😀", &regex).unwrap().is_empty());
        let multiline = FindOptions {
            multiline: true,
            ..regex.clone()
        };
        assert_eq!(find_matches(source, "π.*😀", &multiline).unwrap().len(), 1);
        let strings = FindOptions {
            filter: SearchFilter::Strings,
            ..multiline.clone()
        };
        assert!(find_matches(source, "π.*😀", &strings).unwrap().is_empty());
        let excluded = FindOptions {
            filter: SearchFilter::ExceptComments,
            ..multiline
        };
        assert!(find_matches(source, "π.*😀", &excluded).unwrap().is_empty());
        assert_eq!(
            find_matches(
                "'π'",
                "$",
                &FindOptions {
                    regex: true,
                    filter: SearchFilter::Strings,
                    ..opts()
                }
            )
            .unwrap()
            .len(),
            0
        );
        assert_eq!(
            find_matches(
                "'π'",
                "$",
                &FindOptions {
                    regex: true,
                    filter: SearchFilter::ExceptStrings,
                    ..opts()
                }
            )
            .unwrap()
            .len(),
            1
        );
    }
    #[test]
    fn immutable_preview_uses_actual_groups_case_scope_and_exclusions() {
        let source = "snake_case snake_case\n# snake_case\n";
        let mut state = EditorState::from_text(source, super::super::Language::Python);
        let options = FindOptions {
            regex: true,
            filter: SearchFilter::ExceptComments,
            ..opts()
        };
        let matches = state.find_matches(r"(\w+)_(\w+)", &options).unwrap();
        let before = state.carets.clone();
        let preview = state
            .replacement_preview(r"(\w+)_(\w+)", r"\U$2\E-$1", &options, &[matches[1]], 3)
            .unwrap();
        assert_eq!(preview.rows.len(), 1);
        assert_eq!(preview.rows[0].original, "snake_case");
        assert_eq!(preview.rows[0].replacement, "CASE-snake");
        assert!(!preview.has_more);
        assert_eq!(state.carets, before);
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
        assert_eq!(
            state
                .replace_all_with(r"(\w+)_(\w+)", r"\U$2\E-$1", &options, &[matches[1]])
                .unwrap(),
            1
        );
        assert_eq!(state.doc.text(), "CASE-snake snake_case\n# snake_case\n");
        let state = EditorState::from_text("foo Foo FOO", super::super::Language::PlainText);
        let options = FindOptions {
            preserve_case: true,
            scope: Some(TextRange::new(ts(4), ts(11))),
            ..opts()
        };
        let preview = state
            .replacement_preview("foo", "bar", &options, &[], 1)
            .unwrap();
        assert_eq!(preview.rows[0].replacement, "Bar");
        assert!(preview.has_more);
    }
    #[test]
    fn readonly_replace_all_reports_zero_and_preview_remains_available() {
        let mut state = EditorState::from_text("foo foo", super::super::Language::PlainText);
        state.doc.set_read_only(true);
        let carets = state.carets.clone();
        assert_eq!(
            state
                .replacement_preview("foo", "bar", &opts(), &[], 3)
                .unwrap()
                .rows
                .len(),
            2
        );
        assert!(!state.read_only_refused);
        assert_eq!(
            state.replace_all_with("foo", "bar", &opts(), &[]).unwrap(),
            0
        );
        assert!(state.read_only_refused);
        assert_eq!(state.doc.text(), "foo foo");
        assert_eq!(state.carets, carets);
        assert!(!state.doc.can_undo());
        assert!(state
            .replacement_preview("", "x", &opts(), &[], 3)
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(state.replace_all_with("", "x", &opts(), &[]).unwrap(), 0);
    }
    #[test]
    fn preview_output_is_bounded_and_unsupported_filter_is_explicit() {
        let state =
            EditorState::from_text("x ".repeat(500).as_str(), super::super::Language::PlainText);
        let preview = state
            .replacement_preview("x", "y", &opts(), &[], usize::MAX)
            .unwrap();
        assert_eq!(preview.rows.len(), MAX_PREVIEW_ROWS);
        assert!(preview.has_more);
        let preview = state
            .replacement_preview("x", &"y".repeat(MAX_PREVIEW_BYTES + 1), &opts(), &[], 3)
            .unwrap();
        assert!(preview.rows.is_empty() && preview.has_more);
        let options = FindOptions {
            filter: SearchFilter::Comments,
            ..opts()
        };
        assert!(state
            .find_matches("x", &options)
            .unwrap_err()
            .contains("Python"));
    }
}
