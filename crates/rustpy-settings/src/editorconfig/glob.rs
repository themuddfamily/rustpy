//! EditorConfig globs compiled to a bounded nondeterministic automaton.
//! Alternatives share graph tails; numeric ranges are compared without expanding
//! them or converting unbounded decimal endpoints to machine integers.
use std::collections::{HashSet, VecDeque};

#[derive(Clone)]
struct Decimal {
    negative: bool,
    digits: String,
}
impl Decimal {
    fn parse(value: &str) -> Option<Self> {
        let (negative, digits) = value
            .strip_prefix('-')
            .map_or((false, value), |digits| (true, digits));
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let digits = digits.trim_start_matches('0');
        Some(Self {
            negative: negative && !digits.is_empty(),
            digits: if digits.is_empty() {
                "0".into()
            } else {
                digits.into()
            },
        })
    }
    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        match (self.negative, other.negative) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => {
                let order = self
                    .digits
                    .len()
                    .cmp(&other.digits.len())
                    .then(self.digits.cmp(&other.digits));
                if self.negative {
                    order.reverse()
                } else {
                    order
                }
            }
        }
    }
}
enum Edge {
    Empty(usize),
    Character(char, usize),
    Any {
        slash: bool,
        to: usize,
    },
    Class {
        values: Vec<char>,
        ranges: Vec<(char, char)>,
        negate: bool,
        to: usize,
    },
    Number {
        low: Decimal,
        high: Decimal,
        to: usize,
    },
}
#[derive(Default)]
pub(super) struct Glob {
    states: Vec<Vec<Edge>>,
    end: usize,
}
impl Glob {
    pub(super) fn compile(pattern: &str) -> Option<Self> {
        if pattern.chars().count() > 8192 {
            return None;
        }
        let mut glob = Self::default();
        glob.states.push(Vec::new());
        glob.end = glob.sequence(&pattern.chars().collect::<Vec<_>>(), 0, 0)?;
        Some(glob)
    }
    fn state(&mut self) -> Option<usize> {
        if self.states.len() >= 16384 {
            return None;
        }
        let index = self.states.len();
        self.states.push(Vec::new());
        Some(index)
    }
    fn sequence(&mut self, chars: &[char], mut from: usize, depth: usize) -> Option<usize> {
        if depth > 64 {
            return None;
        }
        let mut at = 0;
        while at < chars.len() {
            let to = self.state()?;
            match chars[at] {
                '\\' if at + 1 < chars.len() => {
                    at += 1;
                    self.states[from].push(Edge::Character(chars[at], to));
                }
                '*' => {
                    let slash = chars.get(at + 1) == Some(&'*');
                    if slash {
                        at += 1;
                    }
                    self.states[from].push(Edge::Empty(to));
                    self.states[from].push(Edge::Any { slash, to: from });
                    if slash && chars.get(at + 1) == Some(&'/') {
                        // The official core fixtures allow b/**/z at b/z.
                        let after_slash = self.state()?;
                        self.states[to].push(Edge::Character('/', after_slash));
                        self.states[from].push(Edge::Empty(after_slash));
                        from = after_slash;
                        at += 2;
                        continue;
                    }
                }
                '?' => self.states[from].push(Edge::Any { slash: false, to }),
                '[' => {
                    if let Some((end, values, ranges, negate)) = class(chars, at) {
                        self.states[from].push(Edge::Class {
                            values,
                            ranges,
                            negate,
                            to,
                        });
                        at = end;
                    } else {
                        self.states[from].push(Edge::Character('[', to));
                    }
                }
                '{' => {
                    if let Some((end, separators)) = brace(chars, at) {
                        let text: String = chars[at + 1..end].iter().collect();
                        let number = text
                            .split_once("..")
                            .and_then(|(low, high)| {
                                Some((Decimal::parse(low)?, Decimal::parse(high)?))
                            })
                            .filter(|(low, high)| low.compare(high).is_lt());
                        if let Some((low, high)) = number {
                            self.states[from].push(Edge::Number { low, high, to });
                            at = end;
                        } else if !separators.is_empty() {
                            let mut start = at + 1;
                            for stop in separators.into_iter().chain(std::iter::once(end)) {
                                let alternative = self.state()?;
                                self.states[from].push(Edge::Empty(alternative));
                                let tail =
                                    self.sequence(&chars[start..stop], alternative, depth + 1)?;
                                self.states[tail].push(Edge::Empty(to));
                                start = stop + 1;
                            }
                            at = end;
                        } else {
                            self.states[from].push(Edge::Character('{', to));
                        }
                    } else {
                        self.states[from].push(Edge::Character('{', to));
                    }
                }
                ch => self.states[from].push(Edge::Character(ch, to)),
            }
            from = to;
            at += 1;
        }
        Some(from)
    }
    pub(super) fn matches(&self, path: &str) -> bool {
        let chars: Vec<_> = path.chars().collect();
        let mut seen = HashSet::new();
        let mut pending = VecDeque::from([(0usize, 0usize)]);
        while let Some((state, at)) = pending.pop_front() {
            if !seen.insert((state, at)) {
                continue;
            }
            if seen.len() > 2_000_000 || pending.len() > 2_000_000 {
                return false;
            }
            if state == self.end && at == chars.len() {
                return true;
            }
            for edge in &self.states[state] {
                match edge {
                    Edge::Empty(to) => pending.push_back((*to, at)),
                    Edge::Character(ch, to) if chars.get(at) == Some(ch) => {
                        pending.push_back((*to, at + 1))
                    }
                    Edge::Any { slash, to }
                        if chars.get(at).is_some_and(|ch| *slash || *ch != '/') =>
                    {
                        pending.push_back((*to, at + 1))
                    }
                    Edge::Class {
                        values,
                        ranges,
                        negate,
                        to,
                    } if chars.get(at).is_some_and(|ch| {
                        *ch != '/'
                            && (values.contains(ch)
                                || ranges.iter().any(|(low, high)| low <= ch && ch <= high))
                                != *negate
                    }) =>
                    {
                        pending.push_back((*to, at + 1))
                    }
                    Edge::Number { low, high, to } => {
                        let first = at + usize::from(chars.get(at) == Some(&'-'));
                        let mut end = first;
                        while end < chars.len() && chars[end].is_ascii_digit() {
                            if end > first && chars[first] == '0' {
                                break;
                            }
                            end += 1;
                            let value: String = chars[at..end].iter().collect();
                            if let Some(value) = Decimal::parse(&value) {
                                if !value.compare(low).is_lt() && !value.compare(high).is_gt() {
                                    pending.push_back((*to, end));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        false
    }
}
type ParsedClass = (usize, Vec<char>, Vec<(char, char)>, bool);
fn class(chars: &[char], start: usize) -> Option<ParsedClass> {
    let mut at = start + 1;
    let negate = chars.get(at) == Some(&'!');
    at += usize::from(negate);
    let mut values = Vec::new();
    let mut ranges = Vec::new();
    while let Some(&ch) = chars.get(at) {
        if ch == ']' && (!values.is_empty() || !ranges.is_empty()) {
            return Some((at, values, ranges, negate));
        }
        if ch == '/' {
            return None;
        }
        if ch != '\\'
            && chars.get(at + 1) == Some(&'-')
            && chars
                .get(at + 2)
                .is_some_and(|ch| *ch != ']' && *ch != '/' && *ch != '\\')
        {
            ranges.push((ch, chars[at + 2]));
            at += 3;
            continue;
        }
        if ch == '\\' && at + 1 < chars.len() {
            at += 1;
        }
        values.push(chars[at]);
        at += 1;
    }
    None
}
fn brace(chars: &[char], start: usize) -> Option<(usize, Vec<usize>)> {
    let mut depth = 1usize;
    let mut separators = Vec::new();
    let mut at = start + 1;
    while let Some(&ch) = chars.get(at) {
        match ch {
            '\\' => at += 1,
            '[' => {
                if let Some((end, _, _, _)) = class(chars, at) {
                    at = end;
                }
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((at, separators));
                }
            }
            ',' if depth == 1 => separators.push(at),
            _ => {}
        }
        at += 1;
    }
    None
}

pub(super) fn has_separator(pattern: &str) -> bool {
    let chars: Vec<_> = pattern.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        match chars[at] {
            '[' => {
                if let Some((end, _, _, _)) = class(&chars, at) {
                    at = end;
                }
            }
            '/' => return true,
            '\\' => {
                at += 1;
                if chars.get(at) == Some(&'/') {
                    return true;
                }
            }
            _ => {}
        }
        at += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    fn matches(pattern: &str, path: &str) -> bool {
        Glob::compile(pattern).unwrap().matches(path)
    }
    #[test]
    fn nested_choices_escaped_delimiters_and_literal_braces_share_the_same_matcher() {
        for path in ["src/a.py", "lib/a.py", "pkg/tests/a.py"] {
            assert!(matches("{src,lib,pkg/{tests,tools}}/*.py", path), "{path}");
        }
        assert!(!matches(
            "{src,lib,pkg/{tests,tools}}/*.py",
            "pkg/other/a.py"
        ));
        assert!(matches(r"{a\,b,cd}.txt", "a,b.txt"));
        assert!(!matches(r"{a\,b,cd}.txt", "a.txt"));
        assert!(matches("{single}.py", "{single}.py"));
        assert!(!matches("{single}.py", "single.py"));
        assert!(matches(r"test\?.py", "test?.py"));
        assert!(!matches(r"test\?.py", "testa.py"));
    }
    #[test]
    fn number_ranges_handle_negative_and_large_endpoints_without_expanding() {
        for name in ["test-3.py", "test0.py", "test12.py"] {
            assert!(matches("test{-3..12}.py", name));
        }
        for name in ["test-4.py", "test13.py", "test012.py", "test1a.py"] {
            assert!(!matches("test{-3..12}.py", name));
        }
        assert!(matches(
            "n{999999999999999999999..1000000000000000000001}t",
            "n1000000000000000000000t"
        ));
        assert!(!matches(
            "n{999999999999999999999..1000000000000000000001}t",
            "n1000000000000000000002t"
        ));
        assert!(matches("{word..words}", "{word..words}"));
    }
    #[test]
    fn core_fixture_globstar_and_bracket_forms_preserve_path_boundaries() {
        assert!(matches("b/**/z.c", "b/z.c"));
        assert!(matches("b/**/z.c", "b/a/b/z.c"));
        assert!(!matches("b/*/z.c", "b/z.c"));
        assert!(matches("[a-c]*.py", "bx.py"));
        assert!(!matches("[!a-c]*.py", "bx.py"));
        assert!(matches("[*{12}].py", "*.py"));
        assert!(matches("[]a].py", "].py"));
        assert!(matches("ab[e/]cd.i", "ab[e/]cd.i"));
        assert!(!matches("ab[e/]cd.i", "ab/cd.i"));
    }
}
