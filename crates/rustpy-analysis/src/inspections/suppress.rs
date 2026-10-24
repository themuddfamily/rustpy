//! Suppression comments (roadmap §2.5 "suppression", §2.6 "Suppress for …"):
//!
//! * `# noinspection Id[,Id…]` on its own line covers the statement below it,
//!   or the whole definition when it sits above a `def`/`class` (or its
//!   decorators); at the end of a line it covers that line. `ALL` matches
//!   every inspection.
//! * `# noinspection file: Id[,Id…]` on its own comment line covers the file,
//!   without suppressing syntax errors or changing ordinary marker scopes.
//! * `# noqa` / `# noqa: E501,F401` silence a line (flake8 codes, matched by
//!   prefix, or inspection ids); as in flake8, a comment after a multi-line
//!   string or a backslash-continued line covers the lines it ends.
//! * `# type: ignore` / `# type: ignore[attr-defined, arg-type]` silence type
//!   checks, argument checks and unresolved attributes on a line.
//!
//! Syntax errors are never suppressed.

use rustc_hash::FxHashMap;
use rustpy_core::{Diagnostic, TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use super::{indent_at, line_end_incl, line_start, ts, QuickFix};
use rustpy_semantic::db::FileInfo;

/// Codes a line marker silences (`None`: all of them).
type Codes = Option<Vec<String>>;

/// The suppression comments of one file.
pub struct Suppressions {
    /// `# noinspection` comments: covered range and inspection ids.
    regions: Vec<(TextRange, Vec<String>)>,
    file_ids: Vec<String>,
    noqa: FxHashMap<usize, Codes>,
    type_ignore: FxHashMap<usize, Codes>,
    /// Byte offset of every line start.
    lines: Vec<usize>,
    /// Last line → first line of the physical lines a comment there ends
    /// (only for groups longer than one line).
    groups: FxHashMap<usize, usize>,
    /// The same groups as `(first, last)`, ordered by line.
    spans: Vec<(usize, usize)>,
}

impl Suppressions {
    pub fn new(file: &FileInfo) -> Self {
        let text: &str = &file.text;
        let mut lines = vec![0];
        lines.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let mut s = Suppressions {
            regions: Vec::new(),
            file_ids: Vec::new(),
            noqa: FxHashMap::default(),
            type_ignore: FxHashMap::default(),
            lines,
            groups: FxHashMap::default(),
            spans: Vec::new(),
        };
        let root = file.parse.syntax_node();
        s.spans = line_groups(&root, &s.lines)
            .into_iter()
            .filter(|(first, last)| first < last)
            .collect();
        s.groups = s.spans.iter().map(|&(first, last)| (last, first)).collect();
        for tok in root
            .descendants_with_tokens()
            .filter_map(|e| e.into_token())
            .filter(|t| t.kind() == SyntaxKind::COMMENT)
        {
            let start = usize::from(tok.text_range().start());
            let line = s.line_of(start);
            let own_line = text[s.lines[line]..start].trim().is_empty();
            for marker in markers(tok.text()) {
                match marker {
                    Marker::FileNoInspection(ids) => {
                        if own_line {
                            s.file_ids.extend(ids);
                        }
                    }
                    Marker::NoInspection(ids) => {
                        let covered = if own_line {
                            covered_statement(text, &tok)
                        } else {
                            Some(TextRange::new(
                                ts(s.lines[line]),
                                ts(line_end_incl(text, start)),
                            ))
                        };
                        if let Some(r) = covered {
                            s.regions.push((r, ids));
                        }
                    }
                    Marker::Noqa(codes) => {
                        let first = s.groups.get(&line).copied().unwrap_or(line);
                        for l in first..=line {
                            merge(&mut s.noqa, l, codes.clone());
                        }
                    }
                    Marker::TypeIgnore(codes) => merge(&mut s.type_ignore, line, codes),
                }
            }
        }
        s
    }

    /// Last line of the line group containing `line` (`line` when alone).
    fn group_end(&self, line: usize) -> usize {
        let index = self.spans.partition_point(|&(first, _)| first <= line);
        match index.checked_sub(1).map(|index| self.spans[index]) {
            Some((_, last)) if line <= last => last,
            _ => line,
        }
    }

    fn line_of(&self, off: usize) -> usize {
        self.lines.partition_point(|&l| l <= off).saturating_sub(1)
    }

    /// Whether a comment silences `d`.
    pub fn is_suppressed(&self, d: &Diagnostic) -> bool {
        if matches!(d.code.as_str(), "syntax" | "PySyntaxError") {
            return false;
        }
        let ids = inspection_ids(d);
        if self
            .file_ids
            .iter()
            .any(|id| id.eq_ignore_ascii_case("all") || ids.contains(&id.as_str()))
        {
            return true;
        }
        if self.regions.iter().any(|(r, rids)| {
            (r.contains(d.range.start()) || r.start() == d.range.start())
                && rids
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case("all") || ids.contains(&id.as_str()))
        }) {
            return true;
        }
        let first = self.line_of(usize::from(d.range.start()));
        let last = self.line_of(usize::from(d.range.end()).max(usize::from(d.range.start())));
        (first..=last).any(|line| {
            let noqa = self.noqa.get(&line).is_some_and(|codes| match codes {
                None => true,
                Some(cs) => cs.iter().any(|c| noqa_matches(c, d, &ids)),
            });
            noqa || self
                .type_ignore
                .get(&line)
                .is_some_and(|codes| type_ignore_matches(codes, d))
        })
    }
}

fn merge(map: &mut FxHashMap<usize, Codes>, line: usize, codes: Codes) {
    match (map.get_mut(&line), codes) {
        (Some(Some(old)), Some(new)) => old.extend(new),
        (Some(old), None) => *old = None,
        (Some(None), Some(_)) => {}
        (None, codes) => {
            map.insert(line, codes);
        }
    }
}

enum Marker {
    FileNoInspection(Vec<String>),
    NoInspection(Vec<String>),
    Noqa(Codes),
    TypeIgnore(Codes),
}

/// Suppression markers in a comment (`# type: ignore  # noqa: E501` has two).
fn markers(comment: &str) -> Vec<Marker> {
    let mut out = Vec::new();
    for seg in comment.split('#').map(str::trim) {
        if let Some(rest) = seg.strip_prefix("noinspection") {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                let (rest, file) = rest
                    .trim_start()
                    .strip_prefix("file:")
                    .map_or((rest, false), |ids| (ids, true));
                let ids: Vec<String> = rest
                    .split([',', ' ', '\t'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if !ids.is_empty() {
                    out.push(if file {
                        Marker::FileNoInspection(ids)
                    } else {
                        Marker::NoInspection(ids)
                    });
                }
            }
            continue;
        }
        let lower = seg.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("noqa") {
            if rest.is_empty() || !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                let rest = &seg[4..];
                let codes: Vec<String> = match rest.trim_start().strip_prefix(':') {
                    Some(list) => list
                        .split([',', ' ', '\t'])
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect(),
                    None => Vec::new(),
                };
                out.push(Marker::Noqa(if codes.is_empty() {
                    None
                } else {
                    Some(codes)
                }));
            }
            continue;
        }
        if let Some(rest) = seg.strip_prefix("type:") {
            let rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix("ignore") {
                let codes = after
                    .trim_start()
                    .strip_prefix('[')
                    .and_then(|l| l.split(']').next())
                    .map(|l| {
                        l.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    });
                if after.is_empty() || after.starts_with(['[', ' ', '\t']) {
                    out.push(Marker::TypeIgnore(codes.filter(|c| !c.is_empty())));
                }
            }
        }
    }
    out
}

/// Physical line groups `(first, last)` (0-based) the way flake8 matches
/// `# noqa`: the lines from the first token after a line break token (`NL`
/// or `NEWLINE`) to the next one. A multi-line string, or lines joined with
/// a backslash, end on the last line of their group, the only place a
/// comment can go. Line breaks inside f-string replacement fields do not
/// end a group (a comment there needs Python 3.12).
fn line_groups(root: &SyntaxNode, lines: &[usize]) -> Vec<(usize, usize)> {
    let line_of = |off: TextSize| {
        lines
            .partition_point(|&l| l <= usize::from(off))
            .saturating_sub(1)
    };
    let mut out = Vec::new();
    let mut first: Option<usize> = None;
    let mut fstrings = 0usize;
    for tok in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        match tok.kind() {
            SyntaxKind::FSTRING_START | SyntaxKind::TSTRING_START => fstrings += 1,
            SyntaxKind::FSTRING_END | SyntaxKind::TSTRING_END => {
                fstrings = fstrings.saturating_sub(1)
            }
            SyntaxKind::NL | SyntaxKind::NEWLINE if fstrings == 0 => {
                let last = line_of(tok.text_range().start());
                out.push((first.take().unwrap_or(last).min(last), last));
                continue;
            }
            SyntaxKind::INDENT | SyntaxKind::DEDENT => continue,
            _ => {}
        }
        first.get_or_insert_with(|| line_of(tok.text_range().start()));
    }
    out
}

/// The statement(s) a `# noinspection` comment on its own line applies to:
/// the next statement (a whole `def`/`class` including its decorators) plus
/// any other statements sharing its first line. A comment indented deeper
/// than that statement ends the block before it (the last line of a
/// function body) and applies to nothing.
fn covered_statement(text: &str, comment: &SyntaxToken) -> Option<TextRange> {
    let mut tok = comment.next_token()?;
    while matches!(
        tok.kind(),
        SyntaxKind::WHITESPACE
            | SyntaxKind::COMMENT
            | SyntaxKind::NL
            | SyntaxKind::NEWLINE
            | SyntaxKind::INDENT
            | SyntaxKind::DEDENT
    ) {
        tok = tok.next_token()?;
    }
    let start = tok.text_range().start();
    let column = |off: usize| off - line_start(text, off);
    if column(usize::from(comment.text_range().start())) > column(usize::from(start)) {
        return None;
    }
    let stmt = tok
        .parent_ancestors()
        .filter(|n| ast::Stmt::can_cast(n.kind()) && n.text_range().start() == start)
        .last()?;
    let first_line_end = line_end_incl(text, usize::from(start));
    let mut end = stmt.text_range().end();
    let mut sib = stmt.next_sibling();
    // `a = 1; b = 2`: statements on the same line.
    while let Some(s) = sib {
        if !ast::Stmt::can_cast(s.kind()) || usize::from(s.text_range().start()) >= first_line_end {
            break;
        }
        end = s.text_range().end();
        sib = s.next_sibling();
    }
    Some(TextRange::new(start, end))
}

/// Inspection ids a problem answers to.
fn inspection_ids(d: &Diagnostic) -> Vec<&str> {
    let code = d.code.as_str();
    if code.starts_with("PEP8:") {
        return vec!["PyPep8", code];
    }
    match code {
        // `# noinspection PyUnresolvedReferences` also suppresses unused imports,
        // the id they are traditionally reported under.
        "PyUnusedImports" => vec!["PyUnusedImports", "PyUnresolvedReferences"],
        other => vec![other],
    }
}

/// flake8 codes of a problem (for `# noqa: CODE`).
fn flake8_codes(d: &Diagnostic) -> Vec<&str> {
    let code = d.code.as_str();
    if let Some(c) = code.strip_prefix("PEP8:") {
        return vec![c];
    }
    match code {
        "PyUnusedImports" => vec!["F401"],
        "PyUnusedLocal" => vec!["F841"],
        "PyRedeclaration" => vec!["F811"],
        "PyUnresolvedReferences" if d.message.starts_with("Unresolved reference") => vec!["F821"],
        _ => Vec::new(),
    }
}

fn noqa_matches(code: &str, d: &Diagnostic, ids: &[&str]) -> bool {
    ids.contains(&code) || flake8_codes(d).iter().any(|c| c.starts_with(code))
}

/// Problems a `# type: ignore` comment covers: type and argument checks and
/// unresolved attributes.
fn type_ignore_matches(codes: &Codes, d: &Diagnostic) -> bool {
    let attribute = d.code == "PyUnresolvedReferences"
        && (d.message.starts_with("Unresolved attribute reference")
            || d.message.starts_with("Cannot find reference"));
    let kind = if attribute {
        "attr"
    } else if d.code == "PyTypeChecker" {
        "type"
    } else if d.code == "PyArgumentList" {
        "args"
    } else {
        return false;
    };
    match codes {
        None => true,
        Some(cs) => cs.iter().any(|c| {
            c == d.code.as_str()
                || match c.as_str() {
                    "attr-defined" | "union-attr" => kind == "attr",
                    "call-arg" | "call-overload" => kind == "args",
                    "arg-type" | "assignment" | "return-value" | "index" | "operator" | "misc"
                    | "list-item" | "dict-item" | "type-var" | "override" => kind == "type",
                    _ => false,
                }
        }),
    }
}

/// The inspection id written into a `# noinspection` comment for `d`.
fn suppression_id(d: &Diagnostic) -> &str {
    let code = d.code.as_str();
    if code.starts_with("PEP8:") {
        "PyPep8"
    } else if code == "PyUnusedImports" {
        "PyUnresolvedReferences"
    } else {
        code
    }
}

/// "Suppress for statement / function / class" (and `# noqa` for PEP 8)
/// fixes for a problem. Titles start with "Suppress " (see
/// [`QuickFix::is_suppression`]).
/// Quick fixes adding a suppression comment for `d`. Takes the file's
/// [`Suppressions`] so line groups are computed once per file, not once per
/// problem (which was quadratic in the file size).
pub fn fixes(suppressions: &Suppressions, file: &FileInfo, d: &Diagnostic) -> Vec<QuickFix> {
    if matches!(d.code.as_str(), "syntax" | "PySyntaxError") || file.is_stub {
        return Vec::new();
    }
    let text: &str = &file.text;
    let id = suppression_id(d);
    let root = file.parse.syntax_node();
    // Called once per problem: the logarithmic lookup keeps files with
    // thousands of top-level statements (and problems) linear overall.
    let at = rustpy_syntax::token_at_offset(&root, d.range.start());
    let Some(tok) = at.clone().right_biased().or_else(|| at.left_biased()) else {
        return Vec::new();
    };
    // A dedicated file directive differs from the existing next-statement marker.
    // Keep both initial comment/blank lines ahead of it so shebang/cookie semantics
    // remain intact; never insert into an actual string token.
    let mut offset = 0;
    for line in text.split_inclusive('\n').take(2) {
        if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            break;
        }
        offset += line.len();
    }
    let prefix = if offset > 0 && !text[..offset].ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let mut out = vec![QuickFix {
        title: format!("Suppress '{id}' for file"),
        edits: vec![(
            TextRange::empty(ts(offset)),
            format!("{prefix}# noinspection file: {id}\n"),
        )],
    }];
    // The statement owning the problem, or the first statement of its line
    // (`x = 1; y = …`: a comment above the line covers both).
    let stmt = tok
        .parent_ancestors()
        .filter(|n| ast::Stmt::can_cast(n.kind()))
        .find_map(|n| line_statement(text, n));
    if let Some(stmt) = &stmt {
        if let Some(fix) = comment_fix(text, stmt, id, format!("Suppress '{id}' for statement")) {
            out.push(fix);
        }
        if let Some(func) = stmt
            .ancestors()
            .skip(1)
            .find(|n| n.kind() == SyntaxKind::FUNC_DEF)
        {
            if let Some(fix) = comment_fix(text, &func, id, format!("Suppress '{id}' for function"))
            {
                out.push(fix);
            }
        }
        if let Some(class) = stmt
            .ancestors()
            .skip(1)
            .find(|n| n.kind() == SyntaxKind::CLASS_DEF)
        {
            if let Some(fix) = comment_fix(text, &class, id, format!("Suppress '{id}' for class")) {
                out.push(fix);
            }
        }
    }
    if let Some(code) = d.code.strip_prefix("PEP8:") {
        if !matches!(code, "W291" | "W292" | "W293") {
            // The comment goes where the problem's line group ends: after a
            // multi-line string or a backslash continuation, not inside it.
            let problem_line = suppressions.line_of(usize::from(d.range.start()));
            let last = suppressions.group_end(problem_line);
            let ls = suppressions.lines[last];
            let le = line_end_incl(text, ls);
            let line = text[ls..le].trim_end_matches(['\n', '\r']);
            if !line.to_ascii_lowercase().contains("# noqa") {
                let content_end = ls + line.trim_end().len();
                out.push(QuickFix {
                    title: format!("Suppress with '# noqa: {code}'"),
                    edits: vec![(
                        TextRange::new(ts(content_end), ts(ls + line.len())),
                        format!("  # noqa: {code}"),
                    )],
                });
            }
        }
    }
    out
}

fn starts_line(text: &str, n: &SyntaxNode) -> bool {
    let s = usize::from(n.text_range().start());
    text[line_start(text, s)..s].trim().is_empty()
}

/// `n` when it starts its line, else the statement before it on the same
/// line that does (`x = 1; y = 2`); `None` for the body of `if c: y = 2`
/// (the caller then takes the enclosing statement).
fn line_statement(text: &str, n: SyntaxNode) -> Option<SyntaxNode> {
    if starts_line(text, &n) {
        return Some(n);
    }
    let ls = line_start(text, usize::from(n.text_range().start()));
    let mut prev = n.prev_sibling();
    while let Some(p) = prev {
        if !ast::Stmt::can_cast(p.kind()) || usize::from(p.text_range().start()) < ls {
            return None;
        }
        if starts_line(text, &p) {
            return Some(p);
        }
        prev = p.prev_sibling();
    }
    None
}

/// Inserts `# noinspection id` above `node` (or adds `id` to a
/// `# noinspection` comment already there).
fn comment_fix(text: &str, node: &SyntaxNode, id: &str, title: String) -> Option<QuickFix> {
    let start = usize::from(node.text_range().start());
    let ls = line_start(text, start);
    let indent = indent_at(text, start);
    if ls > 0 {
        let prev_start = line_start(text, ls - 1);
        let prev = text[prev_start..ls].trim_end_matches(['\n', '\r']);
        if let Some(ids) = prev.trim_start().strip_prefix("# noinspection ") {
            if ids.split(',').any(|x| x.trim() == id) {
                return None;
            }
            let end = prev_start + prev.len();
            return Some(QuickFix {
                title,
                edits: vec![(
                    TextRange::empty(TextSize::new(end as u32)),
                    format!(",{id}"),
                )],
            });
        }
    }
    Some(QuickFix {
        title,
        edits: vec![(
            TextRange::empty(ts(ls)),
            format!("{indent}# noinspection {id}\n"),
        )],
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::{inspect, InspectionOptions, Problem};
    use rustpy_semantic::db::Analysis;

    fn run(src: &str) -> Vec<Problem> {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        inspect(&db, &f, &InspectionOptions::default())
    }

    fn codes(src: &str) -> Vec<String> {
        run(src)
            .into_iter()
            .map(|p| format!("{} {}", p.diagnostic.code, p.diagnostic.message))
            .collect()
    }

    fn apply(text: &str, edits: &[(rustpy_core::TextRange, String)]) -> String {
        let mut edits = edits.to_vec();
        edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start()));
        let mut out = text.to_string();
        for (r, rep) in edits {
            out.replace_range(usize::from(r.start())..usize::from(r.end()), &rep);
        }
        out
    }

    #[test]
    fn noinspection_above_a_statement_targets_one_inspection() {
        let src = "# noinspection PyUnresolvedReferences\nx = undefined_a\ny = undefined_b\n";
        let c = codes(src);
        assert!(!c.iter().any(|m| m.contains("undefined_a")), "{c:?}");
        assert!(c.iter().any(|m| m.contains("undefined_b")), "{c:?}");
        // Another id leaves the problem alone.
        let c = codes("# noinspection PyTypeChecker\nx = undefined_a\n");
        assert!(c.iter().any(|m| m.contains("undefined_a")), "{c:?}");
        // Same-line comments only silence the listed ids.
        let c = codes("x = undefined_a  # noinspection PyTypeChecker\ny = undefined_b  # noinspection PyUnresolvedReferences\n");
        assert!(c.iter().any(|m| m.contains("undefined_a")), "{c:?}");
        assert!(!c.iter().any(|m| m.contains("undefined_b")), "{c:?}");
    }

    #[test]
    fn noinspection_above_def_or_class_covers_the_definition() {
        let src = "import functools\n\n\n# noinspection PyUnresolvedReferences\n@functools.cache\ndef f():\n    return undefined_a\n\n\ndef g():\n    return undefined_b\n\n\n# noinspection ALL\nclass C:\n    def m(self):\n        return undefined_c\n";
        let c = codes(src);
        assert!(
            !c.iter()
                .any(|m| m.contains("undefined_a") || m.contains("undefined_c")),
            "{c:?}"
        );
        assert!(c.iter().any(|m| m.contains("undefined_b")), "{c:?}");
    }

    #[test]
    fn noqa_and_type_ignore_markers() {
        let src = "import os  # noqa: F401\nimport sys  # noqa\nimport json\nx = 1 # noqa: E261\n";
        let c = codes(src);
        assert!(
            !c.iter()
                .any(|m| m.contains("'import os'") || m.contains("'import sys'")),
            "{c:?}"
        );
        assert!(c.iter().any(|m| m.contains("'import json'")), "{c:?}");
        assert!(!c.iter().any(|m| m.contains("E261")), "{c:?}");
        // `# type: ignore` silences attribute and type checks, not unresolved names.
        let src = "import os\nos.nope()  # type: ignore\nos.nope2()  # type: ignore[arg-type]\nundefined_a  # type: ignore\n";
        let c = codes(src);
        assert!(!c.iter().any(|m| m.contains("'nope'")), "{c:?}");
        assert!(c.iter().any(|m| m.contains("'nope2'")), "{c:?}");
        assert!(c.iter().any(|m| m.contains("undefined_a")), "{c:?}");
        let c = codes("def f(a: int): ...\n\n\nf('x')  # type: ignore[arg-type]\nf(1, 2)  # type: ignore[call-arg]\nf()\n");
        assert_eq!(
            c.iter()
                .filter(|m| m.starts_with("PyArgumentList") || m.starts_with("PyTypeChecker"))
                .count(),
            1,
            "{c:?}"
        );
    }

    #[test]
    fn unused_imports_answer_to_unresolved_references() {
        let c = codes("# noinspection PyUnresolvedReferences\nimport os\nimport sys\n");
        assert!(!c.iter().any(|m| m.contains("'import os'")), "{c:?}");
        assert!(c.iter().any(|m| m.contains("'import sys'")), "{c:?}");
    }

    #[test]
    fn suppress_fixes_insert_comments() {
        let src = "class C:\n    @staticmethod\n    def m():\n        if True:\n            return undefined_a\n";
        let probs = run(src);
        let p = probs
            .iter()
            .find(|p| p.diagnostic.message.contains("undefined_a"))
            .unwrap();
        let titles: Vec<&str> = p.fixes.iter().map(|f| f.title.as_str()).collect();
        assert!(
            titles.contains(&"Suppress 'PyUnresolvedReferences' for statement"),
            "{titles:?}"
        );
        let stmt = p
            .fixes
            .iter()
            .find(|f| f.title.ends_with("for statement"))
            .unwrap();
        assert_eq!(apply(src, &stmt.edits), "class C:\n    @staticmethod\n    def m():\n        if True:\n            # noinspection PyUnresolvedReferences\n            return undefined_a\n");
        let func = p
            .fixes
            .iter()
            .find(|f| f.title.ends_with("for function"))
            .unwrap();
        let fixed = apply(src, &func.edits);
        assert_eq!(fixed, "class C:\n    # noinspection PyUnresolvedReferences\n    @staticmethod\n    def m():\n        if True:\n            return undefined_a\n");
        // The inserted comment suppresses the problem.
        assert!(!codes(&fixed).iter().any(|m| m.contains("undefined_a")));
        let class = p
            .fixes
            .iter()
            .find(|f| f.title.ends_with("for class"))
            .unwrap();
        assert!(apply(src, &class.edits)
            .starts_with("# noinspection PyUnresolvedReferences\nclass C:\n"));
        // Real fixes stay first; suppressions are never the primary fix.
        assert!(p
            .fixes
            .iter()
            .all(|f| f.is_suppression() || !f.title.starts_with("Suppress")));
        assert!(p.primary_fix().is_none_or(|f| !f.is_suppression()));
        // An existing comment above gets the id appended.
        let src2 = "# noinspection PyTypeChecker\nx = undefined_b\n";
        let probs = run(src2);
        let p = probs
            .iter()
            .find(|p| p.diagnostic.message.contains("undefined_b"))
            .unwrap();
        let stmt = p
            .fixes
            .iter()
            .find(|f| f.title.ends_with("for statement"))
            .unwrap();
        assert_eq!(
            apply(src2, &stmt.edits),
            "# noinspection PyTypeChecker,PyUnresolvedReferences\nx = undefined_b\n"
        );
    }

    #[test]
    fn pep8_problems_offer_noqa() {
        let src = "x = [1,2]\n";
        let probs = run(src);
        let p = probs
            .iter()
            .find(|p| p.diagnostic.code == "PEP8:E231")
            .unwrap();
        let fix = p
            .fixes
            .iter()
            .find(|f| f.title == "Suppress with '# noqa: E231'")
            .unwrap();
        let fixed = apply(src, &fix.edits);
        assert_eq!(fixed, "x = [1,2]  # noqa: E231\n");
        assert!(!codes(&fixed).iter().any(|m| m.starts_with("PEP8:E231")));
        // PyPep8 silences all pycodestyle codes for the statement.
        assert!(!codes("# noinspection PyPep8\nx = [1,2]\n")
            .iter()
            .any(|m| m.starts_with("PEP8:")));
    }

    #[test]
    fn noqa_fix_goes_after_multiline_strings_and_continuations() {
        let long = "x".repeat(130);
        // A long line inside a docstring: after the closing quotes, not in the string.
        let src = format!("def f():\n    s = \"\"\"\n{long}\n\"\"\"\n    return s\n");
        let probs = run(&src);
        let p = probs
            .iter()
            .find(|p| p.diagnostic.code == "PEP8:E501")
            .unwrap();
        let fix = p
            .fixes
            .iter()
            .find(|f| f.title == "Suppress with '# noqa: E501'")
            .unwrap();
        let fixed = apply(&src, &fix.edits);
        assert_eq!(
            fixed,
            format!("def f():\n    s = \"\"\"\n{long}\n\"\"\"  # noqa: E501\n    return s\n")
        );
        assert!(
            !codes(&fixed).iter().any(|m| m.starts_with("PEP8:E501")),
            "{:?}",
            codes(&fixed)
        );
        assert!(!codes(&fixed).iter().any(|m| m.starts_with("syntax")));
        // A long line continued with a backslash: after the continuation line.
        let src = format!("x = '{long}' + \\\n    'y'\n");
        let probs = run(&src);
        let p = probs
            .iter()
            .find(|p| p.diagnostic.code == "PEP8:E501")
            .unwrap();
        let fix = p
            .fixes
            .iter()
            .find(|f| f.title == "Suppress with '# noqa: E501'")
            .unwrap();
        let fixed = apply(&src, &fix.edits);
        assert_eq!(fixed, format!("x = '{long}' + \\\n    'y'  # noqa: E501\n"));
        assert!(
            !codes(&fixed)
                .iter()
                .any(|m| m.starts_with("PEP8:E501") || m.starts_with("syntax")),
            "{:?}",
            codes(&fixed)
        );
        // Inside brackets each physical line takes its own comment.
        let src = format!("y = max(\n    '{long}',\n    'z')\n");
        let probs = run(&src);
        let p = probs
            .iter()
            .find(|p| p.diagnostic.code == "PEP8:E501")
            .unwrap();
        let fix = p
            .fixes
            .iter()
            .find(|f| f.title == "Suppress with '# noqa: E501'")
            .unwrap();
        assert_eq!(
            apply(&src, &fix.edits),
            format!("y = max(\n    '{long}',  # noqa: E501\n    'z')\n")
        );
        // A `# noqa` after a string covers its lines, but not the lines of the next statement.
        let src = format!("s = '''\n{long}\n'''  # noqa: E501\nt = '{long}'\n");
        assert_eq!(
            codes(&src)
                .iter()
                .filter(|m| m.starts_with("PEP8:E501"))
                .count(),
            1,
            "{:?}",
            codes(&src)
        );
    }

    #[test]
    fn statement_suppression_on_a_semicolon_line_stays_on_that_line() {
        let src = "def f():\n    x = 1; y = undefined_q\n    return x, y\n\n\ndef g():\n    return undefined_r\n";
        let probs = run(src);
        let p = probs
            .iter()
            .find(|p| p.diagnostic.message.contains("undefined_q"))
            .unwrap();
        let stmt = p
            .fixes
            .iter()
            .find(|f| f.title.ends_with("for statement"))
            .unwrap();
        let fixed = apply(src, &stmt.edits);
        assert_eq!(fixed, "def f():\n    # noinspection PyUnresolvedReferences\n    x = 1; y = undefined_q\n    return x, y\n\n\ndef g():\n    return undefined_r\n");
        let c = codes(&fixed);
        assert!(!c.iter().any(|m| m.contains("undefined_q")), "{c:?}");
        assert!(c.iter().any(|m| m.contains("undefined_r")), "{c:?}");
        // `if c: y = …` on one line: the comment goes above the `if`.
        let src = "def f(c):\n    if c: y = undefined_q\n";
        let probs = run(src);
        let p = probs
            .iter()
            .find(|p| p.diagnostic.message.contains("undefined_q"))
            .unwrap();
        let stmt = p
            .fixes
            .iter()
            .find(|f| f.title.ends_with("for statement"))
            .unwrap();
        assert_eq!(
            apply(src, &stmt.edits),
            "def f(c):\n    # noinspection PyUnresolvedReferences\n    if c: y = undefined_q\n"
        );
    }

    #[test]
    fn noinspection_left_at_the_end_of_a_body_covers_nothing_after_it() {
        let src = "def f():\n    x = 1\n    return x\n    # noinspection PyUnresolvedReferences\n\n\ndef g():\n    return undefined_a\n\n\nclass C:\n    def m(self):\n        pass\n        # noinspection PyUnresolvedReferences\n    def n(self):\n        return undefined_b\n";
        let c = codes(src);
        assert!(c.iter().any(|m| m.contains("undefined_a")), "{c:?}");
        assert!(c.iter().any(|m| m.contains("undefined_b")), "{c:?}");
        // At the statement's own indentation it still applies.
        let src = "class C:\n    def m(self):\n        pass\n\n    # noinspection PyUnresolvedReferences\n    def n(self):\n        return undefined_b\n";
        assert!(!codes(src).iter().any(|m| m.contains("undefined_b")));
    }

    #[test]
    fn docstring_problems_go_through_inspect_and_suppression() {
        let src = "def f(a):\n    \"\"\"Do.\n\n    :param b: nope\n    \"\"\"\n    return a\n";
        assert!(
            codes(src)
                .iter()
                .any(|m| m.starts_with("PyIncorrectDocstring")),
            "{:?}",
            codes(src)
        );
        let src2 = format!("# noinspection PyIncorrectDocstring\n{src}");
        assert!(!codes(&src2)
            .iter()
            .any(|m| m.starts_with("PyIncorrectDocstring")));
    }
}

#[cfg(test)]
mod file_directive_tests {
    use super::*;
    #[test]
    fn actual_file_comment_covers_nested_and_later_definitions_but_not_syntax() {
        let db = rustpy_semantic::Analysis::for_project(std::path::Path::new("/project"));
        let source = "# noinspection file: PyUnresolvedReferences\nπ = missing_one\nclass A:\n    def f(self):\n        return missing_two\n";
        let id = db.set_file_text(std::path::Path::new("/project/π.py"), source);
        let file = db.file(id).unwrap();
        let suppressions = Suppressions::new(&file);
        for name in ["missing_one", "missing_two"] {
            let offset = source.find(name).unwrap();
            let range = TextRange::new(ts(offset), ts(offset + name.len()));
            assert!(suppressions.is_suppressed(&Diagnostic::error(
                range,
                "PyUnresolvedReferences",
                "missing"
            )));
            assert!(!suppressions.is_suppressed(&Diagnostic::error(range, "syntax", "bad syntax")));
            assert!(!suppressions.is_suppressed(&Diagnostic::warning(
                range,
                "PyUnusedLocal",
                "unused"
            )));
        }
    }
    #[test]
    fn file_directive_in_string_or_inline_comment_does_not_become_global() {
        for source in [
            "text = '# noinspection file: ALL'\nmissing\n",
            "value = 1 # noinspection file: ALL\nmissing\n",
        ] {
            let db = rustpy_semantic::Analysis::for_project(std::path::Path::new("/project"));
            let id = db.set_file_text(std::path::Path::new("/project/a.py"), source);
            let file = db.file(id).unwrap();
            let offset = source.find("missing").unwrap();
            assert!(!Suppressions::new(&file).is_suppressed(&Diagnostic::error(
                TextRange::new(ts(offset), ts(offset + 7)),
                "PyUnresolvedReferences",
                "missing"
            )));
        }
    }
}
