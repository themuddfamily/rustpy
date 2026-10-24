//! Glob patterns for excluded paths (`.git`, `*.pyc`, `docs/**/*.tmp`).
//!
//! * `*` matches any run of characters except `/`, `?` one character except `/`,
//!   `[abc]` / `[a-z]` / `[!a-z]` (or `[^a-z]`) a character class, `\x` a literal `x`.
//! * `**` forming a whole path segment matches across directories: `**/x`, `a/**/b`, `a/**`.
//! * A pattern **without** `/` is matched against every path component, so `__pycache__`
//!   excludes such directories at any depth and `*.pyc` every such file.
//! * A pattern **with** `/` is anchored at the root (a leading `/` or `./` is optional) and
//!   excludes the matching path and everything below it (`build/lib` excludes `build/lib/x.py`).
//! * A trailing `/` is ignored; blank patterns and `#` comments are skipped. There is no `!`
//!   negation.
//!
//! Matching is case-sensitive and runs in `O(pattern × path)` (no exponential backtracking).

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(char),
    /// `?`
    One,
    /// `*`
    Star,
    /// `**` at the end of a pattern: anything, including `/`.
    Globstar,
    /// `**/`: nothing, or any run of characters ending with `/`.
    GlobstarDir,
    /// `[...]`
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

fn compile(pattern: &str) -> Vec<Tok> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                toks.push(Tok::Lit(chars.get(i + 1).copied().unwrap_or('\\')));
                i += 2;
            }
            '?' => {
                toks.push(Tok::One);
                i += 1;
            }
            '*' => {
                let mut j = i;
                while j < chars.len() && chars[j] == '*' {
                    j += 1;
                }
                let segment_start = i == 0 || chars[i - 1] == '/';
                let segment_end = j == chars.len() || chars[j] == '/';
                if j - i >= 2 && segment_start && segment_end {
                    if j < chars.len() {
                        toks.push(Tok::GlobstarDir);
                        j += 1; // the '/' is part of the token
                    } else {
                        toks.push(Tok::Globstar);
                    }
                } else {
                    toks.push(Tok::Star);
                }
                i = j;
            }
            '[' => match parse_class(&chars, i) {
                Some((tok, next)) => {
                    toks.push(tok);
                    i = next;
                }
                None => {
                    toks.push(Tok::Lit('['));
                    i += 1;
                }
            },
            c => {
                toks.push(Tok::Lit(c));
                i += 1;
            }
        }
    }
    toks
}

/// Parses `[...]` starting at `start` (the `[`); `None` when it is not closed.
fn parse_class(chars: &[char], start: usize) -> Option<(Tok, usize)> {
    let mut i = start + 1;
    let negated = matches!(chars.get(i), Some('!') | Some('^'));
    if negated {
        i += 1;
    }
    let mut ranges = Vec::new();
    let mut first = true;
    while i < chars.len() {
        let c = chars[i];
        if c == ']' && !first {
            return Some((Tok::Class { negated, ranges }, i + 1));
        }
        first = false;
        let lo = if c == '\\' && i + 1 < chars.len() {
            i += 1;
            chars[i]
        } else {
            c
        };
        if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != ']' {
            ranges.push((lo, chars[i + 2]));
            i += 3;
        } else {
            ranges.push((lo, lo));
            i += 1;
        }
    }
    None
}

fn class_matches(negated: bool, ranges: &[(char, char)], c: char) -> bool {
    c != '/' && ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi) != negated
}

/// Dynamic programming over (token, text position): `next[j]` tells whether the tokens after
/// the current one match `text[j..]`.
fn matches(toks: &[Tok], text: &[char]) -> bool {
    let n = text.len();
    let mut next = vec![false; n + 1];
    next[n] = true;
    for tok in toks.iter().rev() {
        let mut cur = vec![false; n + 1];
        // For GlobstarDir: whether some k > j with text[k-1] == '/' has next[k].
        let mut slash_then_rest = false;
        for j in (0..=n).rev() {
            cur[j] = match tok {
                Tok::Lit(c) => j < n && text[j] == *c && next[j + 1],
                Tok::One => j < n && text[j] != '/' && next[j + 1],
                Tok::Class { negated, ranges } => {
                    j < n && class_matches(*negated, ranges, text[j]) && next[j + 1]
                }
                Tok::Star => next[j] || (j < n && text[j] != '/' && cur[j + 1]),
                Tok::Globstar => next[j] || (j < n && cur[j + 1]),
                Tok::GlobstarDir => {
                    if j < n && text[j] == '/' && next[j + 1] {
                        slash_then_rest = true;
                    }
                    next[j] || slash_then_rest
                }
            };
        }
        next = cur;
    }
    next[0]
}

/// Matches `text` against a single glob `pattern` (see the module docs for the syntax).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    matches(&compile(pattern), &text)
}

/// A compiled list of exclusion patterns (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct ExcludeSet {
    /// Patterns without `/`, matched against each component.
    names: Vec<Vec<Tok>>,
    /// Raw name patterns (for `find -name` pruning on SSH servers).
    name_sources: Vec<String>,
    /// Patterns with `/`, matched against each leading sub-path.
    anchored: Vec<Vec<Tok>>,
}

impl ExcludeSet {
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Self {
        let mut set = ExcludeSet::default();
        for raw in patterns {
            let p = raw.as_ref().trim();
            let p = p.strip_suffix('/').unwrap_or(p);
            if p.is_empty() || p.starts_with('#') {
                continue;
            }
            if p.contains('/') {
                let p = p.trim_start_matches("./").trim_start_matches('/');
                if !p.is_empty() {
                    set.anchored.push(compile(p));
                }
            } else {
                set.names.push(compile(p));
                set.name_sources.push(p.to_string());
            }
        }
        set
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.anchored.is_empty()
    }

    /// The patterns that apply to single names (usable with `find -name`).
    pub fn name_patterns(&self) -> &[String] {
        &self.name_sources
    }

    /// True when `rel_path` (relative, `/`-separated) or one of its parent directories is excluded.
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        let rel = normalize_rel(rel_path);
        if rel.is_empty() || self.is_empty() {
            return false;
        }
        if !self.names.is_empty() {
            for comp in rel.split('/') {
                let chars: Vec<char> = comp.chars().collect();
                if self.names.iter().any(|t| matches(t, &chars)) {
                    return true;
                }
            }
        }
        if !self.anchored.is_empty() {
            let chars: Vec<char> = rel.chars().collect();
            let ends = chars
                .iter()
                .enumerate()
                .filter(|(_, c)| **c == '/')
                .map(|(i, _)| i)
                .chain(std::iter::once(chars.len()));
            for end in ends {
                if self.anchored.iter().any(|t| matches(t, &chars[..end])) {
                    return true;
                }
            }
        }
        false
    }
}

/// True when `rel_path` is excluded by `patterns` (compiles them; use [`ExcludeSet`] in loops).
pub fn is_excluded<S: AsRef<str>>(patterns: &[S], rel_path: &str) -> bool {
    ExcludeSet::new(patterns).is_excluded(rel_path)
}

/// `./a//b/` → `a/b` (and `\` → `/` on Windows).
pub(crate) fn normalize_rel(rel: &str) -> String {
    let rel = if cfg!(windows) {
        rel.replace('\\', "/")
    } else {
        rel.to_string()
    };
    rel.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_wildcards() {
        assert!(glob_match("*.pyc", "mod.pyc"));
        assert!(glob_match("*.pyc", ".pyc"));
        assert!(!glob_match("*.pyc", "mod.py"));
        assert!(!glob_match("*.pyc", "pkg/mod.pyc"), "* does not cross /");
        assert!(glob_match("mod?.py", "mod1.py"));
        assert!(!glob_match("mod?.py", "mod.py"));
        assert!(glob_match("[abc]x", "bx"));
        assert!(!glob_match("[abc]x", "dx"));
        assert!(glob_match("[a-c]x", "cx"));
        assert!(glob_match("[!a-c]x", "dx"));
        assert!(glob_match("[^a-c]x", "dx"));
        assert!(!glob_match("[!a-c]x", "ax"));
        assert!(glob_match("[]]", "]"));
        assert!(glob_match("\\*lit", "*lit"));
        assert!(!glob_match("\\*lit", "xlit"));
        assert!(glob_match("[unclosed", "[unclosed"));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
    }

    #[test]
    fn globstar() {
        assert!(glob_match("**/foo", "foo"));
        assert!(glob_match("**/foo", "a/b/foo"));
        assert!(!glob_match("**/foo", "a/xfoo"));
        assert!(glob_match("a/**/b", "a/b"));
        assert!(glob_match("a/**/b", "a/x/y/b"));
        assert!(!glob_match("a/**/b", "ab"));
        assert!(glob_match("a/**", "a/x/y"));
        assert!(!glob_match("a/**", "a"));
        assert!(glob_match("**", "anything/at/all"));
        // `**` inside a segment is an ordinary star.
        assert!(glob_match("a**b", "aXb"));
        assert!(!glob_match("a**b", "a/b"));
    }

    #[test]
    fn no_exponential_blowup() {
        let text = "a".repeat(200);
        assert!(!glob_match("*a*a*a*a*a*a*a*a*a*a*a*b", &text));
    }

    #[test]
    fn exclusion_semantics() {
        let defaults = crate::config::default_excluded();
        let set = ExcludeSet::new(&defaults);
        assert!(set.is_excluded(".git"));
        assert!(set.is_excluded(".git/config"));
        assert!(set.is_excluded("pkg/__pycache__/mod.cpython-312.pyc"));
        assert!(set.is_excluded("pkg/mod.pyc"));
        assert!(set.is_excluded(".venv/lib/site.py"));
        assert!(set.is_excluded(".rustpy/deployment.json"));
        assert!(!set.is_excluded("pkg/mod.py"));
        assert!(!set.is_excluded("my.git/x"));
        assert!(!set.is_excluded(""));

        let set = ExcludeSet::new(&["docs/*.md", "/build/", "**/node_modules", "# comment", "  "]);
        assert!(set.is_excluded("docs/readme.md"));
        assert!(!set.is_excluded("docs/sub/readme.md"));
        assert!(!set.is_excluded("other/docs/readme.md"));
        assert!(set.is_excluded("build"));
        assert!(set.is_excluded("build/lib/x.py"));
        assert!(!set.is_excluded("src/build/x.py"), "anchored pattern");
        assert!(set.is_excluded("node_modules/x.js"));
        assert!(set.is_excluded("web/node_modules/x.js"));
        assert!(set.is_excluded("./docs//readme.md"), "paths are normalized");
        assert_eq!(set.name_patterns().len(), 0);

        assert!(is_excluded(&["*.log"], "logs/today.log"));
        assert!(!is_excluded::<&str>(&[], "anything"));
    }
}
