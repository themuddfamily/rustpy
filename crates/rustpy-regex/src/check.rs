//! Validation of Python regular expressions on top of the tokenizer.

use std::collections::HashMap;

use rustpy_core::TextRange;

use crate::tokenizer::{tokenize, RegexToken, RegexTokenKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexError {
    /// Byte range inside the pattern.
    pub range: TextRange,
    pub message: String,
}

fn text<'a>(pattern: &'a str, t: &RegexToken) -> &'a str {
    &pattern[usize::from(t.range.start())..usize::from(t.range.end())]
}

/// Reports structural problems: unbalanced groups/classes, bad escapes,
/// trailing backslash, nothing to repeat, multiple repeat, bad ranges,
/// unknown group names and invalid group references.
pub fn check(pattern: &str, verbose: bool) -> Vec<RegexError> {
    let tokens = tokenize(pattern, verbose);
    let mut errors = Vec::new();
    let push = |errors: &mut Vec<RegexError>, t: &RegexToken, msg: String| {
        errors.push(RegexError {
            range: t.range,
            message: msg,
        })
    };

    // Collect defined group names / count numbered groups (in order).
    let mut names: HashMap<&str, usize> = HashMap::new();
    let mut group_count = 0usize;
    for (i, t) in tokens.iter().enumerate() {
        if t.kind == RegexTokenKind::GroupOpen {
            let s = text(pattern, t);
            if s == "("
                || ((s == "(?P<" || s == "(?<")
                    && tokens
                        .get(i + 1)
                        .is_some_and(|n| n.kind == RegexTokenKind::GroupName))
            {
                group_count += 1;
            }
        }
    }

    // Numeric conditions can reference later captures, unlike ordinary backreferences.
    let mut seen_groups = 0usize;
    let mut open_groups: Vec<Option<usize>> = Vec::new();
    let mut in_class = false;
    let mut prev_atom = false; // something a quantifier may apply to
    let mut prev_quantifier = false;
    for (i, t) in tokens.iter().enumerate() {
        let s = text(pattern, t);
        match t.kind {
            RegexTokenKind::Error => {
                let msg = if s == "\\" {
                    "bad escape (end of pattern)".to_string()
                } else if s.chars().count() >= 3 && find_range_dash(s).is_some() {
                    format!("bad character range {s}")
                } else if s.starts_with('\\') {
                    format!("bad escape {s}")
                } else if s.starts_with('(') {
                    if is_known_extension(s) {
                        "missing ), unterminated subpattern".to_string()
                    } else {
                        format!("unknown extension {s}")
                    }
                } else if s == ")" {
                    "unbalanced parenthesis".to_string()
                } else if s.starts_with('[') {
                    "unterminated character set".to_string()
                } else {
                    format!("invalid syntax {s}")
                };
                push(&mut errors, t, msg);
                prev_atom = !s.starts_with('(') && s != ")";
                prev_quantifier = false;
            }
            RegexTokenKind::Quantifier => {
                if prev_quantifier {
                    push(&mut errors, t, "multiple repeat".into());
                } else if !prev_atom {
                    push(&mut errors, t, "nothing to repeat".into());
                }
                // `{m,n}` with m > n.
                if s.starts_with('{') {
                    let body = s.trim_start_matches('{').split('}').next().unwrap_or("");
                    if let Some((lo, hi)) = body.split_once(',') {
                        if let (Ok(lo), Ok(hi)) = (lo.parse::<u64>(), hi.parse::<u64>()) {
                            if lo > hi {
                                push(&mut errors, t, "min repeat greater than max repeat".into());
                            }
                        }
                    }
                }
                prev_atom = false;
                prev_quantifier = true;
            }
            RegexTokenKind::ClassRange => {
                let (lo, hi) = class_range_bounds(s);
                if let (Some(lo), Some(hi)) = (lo, hi) {
                    if lo > hi {
                        push(&mut errors, t, format!("bad character range {s}"));
                    }
                }
                prev_atom = false;
                prev_quantifier = false;
            }
            RegexTokenKind::CharClass => {
                // The closing `]` makes the class an atom.
                in_class = s != "]";
                prev_atom = s == "]";
                prev_quantifier = false;
            }
            RegexTokenKind::GroupClose => {
                open_groups.pop();
                prev_atom = true;
                prev_quantifier = false;
            }
            RegexTokenKind::GroupOpen => {
                // Named openers are split into opener/name/">" tokens; only the
                // initial token owns a stack entry. Conditional ")" is also a fragment.
                if s.starts_with('(') {
                    let capture = if s == "(" || s == "(?P<" || s == "(?<" {
                        seen_groups += 1;
                        if s != "(" {
                            if let Some(name) = tokens
                                .get(i + 1)
                                .filter(|t| t.kind == RegexTokenKind::GroupName)
                            {
                                names.insert(text(pattern, name), seen_groups);
                            }
                        }
                        Some(seen_groups)
                    } else {
                        None
                    };
                    open_groups.push(capture);
                }
                prev_atom = false;
                prev_quantifier = false;
            }
            RegexTokenKind::Alternation => {
                prev_atom = false;
                prev_quantifier = false;
            }
            RegexTokenKind::Anchor | RegexTokenKind::Flag | RegexTokenKind::Comment => {
                prev_atom = false;
                prev_quantifier = false;
            }
            RegexTokenKind::GroupName => {
                // References: `(?P=name)`, `\g<name>`, `(?(name)`.
                let prev = i
                    .checked_sub(1)
                    .and_then(|p| tokens.get(p))
                    .map(|p| text(pattern, p))
                    .unwrap_or("");
                let is_reference = prev == "(?P=" || prev == "\\g<" || prev == "(?(";
                if is_reference {
                    let conditional = prev == "(?(";
                    let reference = if let Ok(n) = s.parse::<usize>() {
                        if n == 0
                            || n > if conditional {
                                group_count
                            } else {
                                seen_groups
                            }
                        {
                            push(&mut errors, t, format!("invalid group reference {n}"));
                            None
                        } else {
                            Some(n)
                        }
                    } else if let Some(&n) = names.get(s) {
                        Some(n)
                    } else {
                        push(&mut errors, t, format!("unknown group name '{s}'"));
                        None
                    };
                    if !conditional && reference.is_some_and(|n| open_groups.contains(&Some(n))) {
                        push(&mut errors, t, "cannot refer to an open group".into());
                    }
                } else if s.is_empty()
                    || !s.chars().enumerate().all(|(k, c)| {
                        c == '_' || c.is_alphanumeric() && (k > 0 || !c.is_ascii_digit())
                    })
                {
                    push(&mut errors, t, format!("bad character in group name '{s}'"));
                }
                if is_reference {
                    prev_atom = true;
                    prev_quantifier = false;
                }
            }
            RegexTokenKind::Escape => {
                // Numeric back-reference validity.
                if let Some(num) = s.strip_prefix('\\').filter(|r| {
                    !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()) && !r.starts_with('0')
                }) {
                    if let Ok(n) = num.parse::<usize>() {
                        let octal = num.len() == 3 && num.chars().all(|c| ('0'..='7').contains(&c));
                        if !in_class && !octal {
                            if n > seen_groups {
                                push(&mut errors, t, format!("invalid group reference {n}"));
                            } else if open_groups.contains(&Some(n)) {
                                push(&mut errors, t, "cannot refer to an open group".into());
                            }
                        }
                    }
                }
                prev_atom = s != "\\g<" && s != ">" && s != "(?P=";
                prev_quantifier = false;
            }
            RegexTokenKind::Literal | RegexTokenKind::Dot => {
                prev_atom = true;
                prev_quantifier = false;
            }
        }
    }
    if errors.is_empty() {
        errors.extend(crate::inspection_checks::lookbehind_errors(
            pattern, verbose,
        ));
    }
    errors
}

/// True for group openers the tokenizer recognizes (an unclosed one is reported
/// as "missing )"); anything else starting with `(?` is an unknown extension.
fn is_known_extension(s: &str) -> bool {
    if [
        "(", "(?:", "(?=", "(?!", "(?<=", "(?<!", "(?P<", "(?<", "(?(",
    ]
    .contains(&s)
    {
        return true;
    }
    // Scoped flags `(?i-s:`.
    s.len() > 3
        && s.starts_with("(?")
        && s.ends_with(':')
        && s[2..s.len() - 1].chars().all(|c| "aiLmsux-".contains(c))
}

/// Code points of the two ends of a `lo-hi` class range (escapes decoded when simple).
fn class_range_bounds(s: &str) -> (Option<u32>, Option<u32>) {
    let Some(dash) = find_range_dash(s) else {
        return (None, None);
    };
    let lo = decode_class_char(&s[..dash]);
    let hi = decode_class_char(&s[dash + 1..]);
    (lo, hi)
}

/// Position of the `-` separating the two ends (skipping an escaped first char).
fn find_range_dash(s: &str) -> Option<usize> {
    let lo_len = if let Some(rest) = s.strip_prefix('\\') {
        // Escape token text: `\x41`, `\n`, `\-`, …
        let c = rest.chars().next()?;
        match c {
            'x' => 4,
            'u' => 6,
            'U' => 10,
            _ => 1 + c.len_utf8(),
        }
    } else {
        s.chars().next()?.len_utf8()
    };
    (s.len() > lo_len && s.as_bytes()[lo_len] == b'-').then_some(lo_len)
}

fn decode_class_char(s: &str) -> Option<u32> {
    if let Some(rest) = s.strip_prefix('\\') {
        let mut chars = rest.chars();
        let c = chars.next()?;
        return match c {
            'n' => Some(10),
            't' => Some(9),
            'r' => Some(13),
            'f' => Some(12),
            'v' => Some(11),
            'a' => Some(7),
            'b' => Some(8),
            '0' => u32::from_str_radix(rest, 8).ok().or(Some(0)),
            'x' | 'u' | 'U' => u32::from_str_radix(chars.as_str(), 16).ok(),
            'd' | 'w' | 's' | 'D' | 'W' | 'S' => None,
            other => Some(other as u32),
        };
    }
    s.chars().next().map(|c| c as u32)
}

/// Convenience: the first error, if any.
pub fn first_error(pattern: &str, verbose: bool) -> Option<RegexError> {
    check(pattern, verbose).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(p: &str) -> Vec<String> {
        check(p, false).into_iter().map(|e| e.message).collect()
    }

    #[test]
    fn valid_patterns_have_no_errors() {
        for p in [
            r"^\d{3}-\d{4}$",
            r"(?P<year>\d{4})-(?P=year)",
            r"[a-zA-Z_][a-zA-Z0-9_]*",
            r"a|b|c",
            r"(?i:hello)\s+world",
            r"(?#c)x+?",
            r"(a)\1",
            r"[\d_-]",
            r"a{2,5}",
            r"(?(1)a|b)(x)?",
            r"(?P<n>a)(?(n)b|c)",
            r"\x41-\x5a",
            r"[\x41-\x5a]",
            r"(?i-s:ab)",
            r"(?aiLmsux)x",
        ] {
            let errs = msgs(p);
            assert!(errs.is_empty(), "{p}: {errs:?}");
        }
    }

    #[test]
    fn unbalanced() {
        assert_eq!(msgs("(a"), vec!["missing ), unterminated subpattern"]);
        assert_eq!(msgs("a)"), vec!["unbalanced parenthesis"]);
        assert_eq!(msgs("[ab"), vec!["unterminated character set"]);
        assert_eq!(msgs("(?:a"), vec!["missing ), unterminated subpattern"]);
        assert_eq!(msgs("(?Qa)")[0], "unknown extension (?Q");
    }

    #[test]
    fn escapes() {
        assert_eq!(msgs(r"\q"), vec![r"bad escape \q"]);
        assert_eq!(msgs("abc\\"), vec!["bad escape (end of pattern)"]);
        assert_eq!(msgs(r"\x4g"), vec![r"bad escape \x4"]);
    }

    #[test]
    fn repeats() {
        assert_eq!(msgs("*a"), vec!["nothing to repeat"]);
        assert_eq!(msgs("a**"), vec!["multiple repeat"]);
        assert_eq!(msgs("(*)"), vec!["nothing to repeat"]);
        assert_eq!(msgs("a|*"), vec!["nothing to repeat"]);
        assert_eq!(msgs("^*"), vec!["nothing to repeat"]);
        assert_eq!(msgs("a{5,2}"), vec!["min repeat greater than max repeat"]);
        assert!(msgs("a*?").is_empty());
        assert!(msgs("(ab)+").is_empty());
        assert!(msgs("[ab]+").is_empty());
        assert!(msgs(r"\d+").is_empty());
    }

    #[test]
    fn ranges_and_names() {
        assert_eq!(msgs("[z-a]"), vec!["bad character range z-a"]);
        assert_eq!(msgs(r"[\d-z]"), vec![r"bad character range \d-z"]);
        assert!(msgs("[a-z]").is_empty());
        assert_eq!(msgs(r"(?P<x>a)(?P=y)"), vec!["unknown group name 'y'"]);
        assert_eq!(msgs(r"(a)\g<b>"), vec!["unknown group name 'b'"]);
        assert_eq!(msgs(r"(a)\2"), vec!["invalid group reference 2"]);
        assert_eq!(msgs(r"(?P<1x>a)"), vec!["bad character in group name '1x'"]);
        assert!(msgs(r"(?P<a_1>a)(?P=a_1)").is_empty());
        assert!(msgs(r"(a)\g<1>").is_empty());
    }

    #[test]
    fn error_ranges_point_into_pattern() {
        let e = check("ab(c", false);
        assert_eq!(e.len(), 1);
        assert_eq!(usize::from(e[0].range.start()), 2);
        assert_eq!(usize::from(e[0].range.end()), 3);
    }

    #[test]
    fn verbose_patterns() {
        assert!(check("(?P<a> \\d+ )  # digits\n \\s* (?P=a)", true).is_empty());
        assert_eq!(check("a b *", true).len(), 0);
    }
    #[test]
    fn ordinary_references_require_an_earlier_closed_capture() {
        for (pattern, message) in [
            (r"\1(a)", "invalid group reference 1"),
            (r"(a\1)", "cannot refer to an open group"),
            (r"(?P=x)(?P<x>a)", "unknown group name 'x'"),
            (r"(?P<x>(?P=x))", "cannot refer to an open group"),
            (r"((a)\1)", "cannot refer to an open group"),
        ] {
            assert_eq!(msgs(pattern), vec![message], "{pattern}");
        }
        for pattern in [
            r"(a)\1",
            r"(?P<x>a)(?P=x)",
            r"((a)\2)",
            r"(?:a)(b)\1",
            r"(?i:(a))\1",
            r"(?P<x>a)\g<x>",
        ] {
            assert!(msgs(pattern).is_empty(), "{pattern}");
        }
    }
    #[test]
    fn conditionals_classes_octal_and_verbose_keep_their_distinct_authority() {
        for pattern in [
            r"(?(1)a|b)(x)",
            r"(?P<x>(?(x)a|b))",
            r"[\1](a)",
            r"\111(a)",
            r"\01",
            r"(a)\\1",
        ] {
            assert!(msgs(pattern).is_empty(), "{pattern}");
        }
        assert_eq!(msgs(r"(?(x)a|b)(?P<x>x)"), vec!["unknown group name 'x'"]);
        assert!(check("(?P<x>π) # 🙂\n(?P=x)", true).is_empty());
    }
    #[test]
    fn unicode_reference_error_range_is_the_actual_reference() {
        let pattern = r"π🙂\1(a)";
        let errors = check(pattern, false);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            &pattern[usize::from(errors[0].range.start())..usize::from(errors[0].range.end())],
            r"\1"
        );
    }
}
