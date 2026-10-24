//! Lossless CST printing and transactional structured rewrites.
//!
//! Untouched spans are emitted verbatim. Replacement fragments retain their own
//! trivia; outside trivia stays at its original site. Statement moves can attach
//! physical lines and contiguous same-indent leading comments. Blank lines,
//! shebangs and first-two-line coding cookies never attach. No implicit syntax
//! repair, indentation or reformatting occurs; callers choose transformations.
use crate::ast::{AstNode, Stmt};
use crate::{SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize};
use thiserror::Error;

pub const MAX_EDITS: usize = 4096;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
pub type TextEdit = (TextRange, String);
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RewriteError {
    #[error("the element belongs to another syntax tree")]
    ForeignElement,
    #[error("a rewrite requires a complete root tree")]
    NotRoot,
    #[error("a rewrite cannot snapshot a mutable syntax tree")]
    MutableRoot,
    #[error("edit range {0:?} does not fit the source or splits a Unicode character")]
    InvalidRange(TextRange),
    #[error("overlapping edits at {0:?}")]
    Conflict(TextRange),
    #[error("rewrite exceeds its edit or output budget")]
    Budget,
    #[error("replacement does not lex as one token of the original kind")]
    InvalidToken,
    #[error("statement trivia attachment requires a standalone physical line")]
    NotStandaloneStatement,
    #[error("a move requires two distinct sibling statements")]
    NotSiblings,
    #[error("moving an unterminated final line here would join two statements")]
    MissingLineBreak,
}
/// Re-emits every token, including trivia and error nodes. Invalid/incomplete
/// input round-trips too; this is not an AST pretty-printer.
pub fn print(root: &SyntaxNode) -> String {
    let mut output = String::with_capacity(usize::from(root.text_range().len()));
    for token in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        output.push_str(token.text());
    }
    output
}
fn prepare_edits(source: &str, edits: &[TextEdit]) -> Result<(Vec<TextEdit>, usize), RewriteError> {
    if edits.len() > MAX_EDITS || source.len() > MAX_OUTPUT_BYTES {
        return Err(RewriteError::Budget);
    }
    let total_replacements = edits
        .iter()
        .try_fold(0usize, |total, (_, text)| total.checked_add(text.len()));
    if total_replacements.is_none_or(|total| total > MAX_OUTPUT_BYTES + source.len()) {
        return Err(RewriteError::Budget);
    }
    let mut sorted = edits.to_vec();
    sorted.sort_by(|(a, x), (b, y)| {
        (a.start(), a.end())
            .cmp(&(b.start(), b.end()))
            .then_with(|| y.cmp(x))
    });
    sorted.dedup();
    let mut cursor = 0;
    let mut size = source.len();
    for (range, text) in &sorted {
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        if end > source.len() || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(RewriteError::InvalidRange(*range));
        }
        if start < cursor {
            return Err(RewriteError::Conflict(*range));
        }
        cursor = end;
        size = size
            .checked_sub(end - start)
            .and_then(|n| n.checked_add(text.len()))
            .filter(|n| *n <= MAX_OUTPUT_BYTES)
            .ok_or(RewriteError::Budget)?;
    }
    Ok((sorted, size))
}
/// Shared checked emitter. Duplicate edits coalesce; distinct insertions at the
/// same offset retain historical reverse lexical order. An insertion at a
/// replacement's start precedes it; interior insertions conflict. All edits use
/// original-source coordinates and validate before any output is emitted.
pub fn apply_text_edits(source: &str, edits: &[TextEdit]) -> Result<String, RewriteError> {
    let (sorted, size) = prepare_edits(source, edits)?;
    let mut output = String::with_capacity(size);
    let mut cursor = 0;
    for (range, text) in sorted {
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        output.push_str(&source[cursor..start]);
        output.push_str(&text);
        cursor = end;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriviaAttachment {
    /// Exactly the node's CST range; outside trivia remains at its original site.
    NodeOnly,
    /// Physical statement lines including indentation and inline comment.
    StatementLines,
    /// StatementLines plus contiguous same-indent preceding comment lines.
    LeadingComments,
}
fn trivia(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::WHITESPACE
            | SyntaxKind::COMMENT
            | SyntaxKind::NL
            | SyntaxKind::NEWLINE
            | SyntaxKind::INDENT
            | SyntaxKind::DEDENT
            | SyntaxKind::EOF
    )
}
// Match actual formatter directives, including compound #/semicolon lists.
// Editor regions also delimit a larger span rather than one declaration.
fn boundary_comment(comment: &str) -> bool {
    comment
        .split('#')
        .flat_map(|part| part.split(';'))
        .any(|part| {
            let part = part.trim();
            matches!(
                part,
                "fmt: off"
                    | "fmt:off"
                    | "fmt: on"
                    | "fmt:on"
                    | "fmt: skip"
                    | "fmt:skip"
                    | "yapf: disable"
                    | "yapf: enable"
            ) || part.starts_with("<editor-fold")
                || part.starts_with("</editor-fold")
                || ["region", "endregion"].iter().any(|name| {
                    part.strip_prefix(name).is_some_and(|tail| {
                        tail.is_empty() || tail.starts_with(char::is_whitespace)
                    })
                })
        })
}
fn line_start(source: &str, at: usize) -> usize {
    source[..at].rfind(['\r', '\n']).map_or(0, |n| n + 1)
}
fn line_end(source: &str, at: usize) -> usize {
    source[at..].find(['\r', '\n']).map_or(source.len(), |n| {
        let end = at + n + 1;
        end + usize::from(
            source.as_bytes()[end - 1] == b'\r' && source.as_bytes().get(end) == Some(&b'\n'),
        )
    })
}
/// Computes attachment from actual CST token boundaries. `source` must equal
/// the complete tree text. Trailing blank/comment-only lines consumed before a
/// DEDENT are left behind, while comments inside the statement remain intact.
pub fn attached_range(
    source: &str,
    node: &SyntaxNode,
    attachment: TriviaAttachment,
) -> Result<TextRange, RewriteError> {
    let root = node
        .ancestors()
        .last()
        .ok_or(RewriteError::ForeignElement)?;
    if print(&root) != source {
        return Err(RewriteError::ForeignElement);
    }
    if attachment == TriviaAttachment::NodeOnly {
        return Ok(node.text_range());
    }
    if !Stmt::can_cast(node.kind()) {
        return Err(RewriteError::NotStandaloneStatement);
    }
    let mut tokens = node
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !trivia(t.kind()) && !t.text_range().is_empty());
    let first = tokens.next().ok_or(RewriteError::NotStandaloneStatement)?;
    let last = tokens.last().unwrap_or_else(|| first.clone());
    let first_start = usize::from(first.text_range().start());
    let mut start = line_start(source, first_start);
    let indent = &source[start..first_start];
    if !indent.chars().all(|c| matches!(c, ' ' | '\t' | '\x0c')) {
        return Err(RewriteError::NotStandaloneStatement);
    }
    let last_end = usize::from(last.text_range().end());
    let mut end = line_end(source, last_end);
    let tail = source[last_end..end].trim();
    if !tail.is_empty() && !tail.starts_with('#') {
        return Err(RewriteError::NotStandaloneStatement);
    }
    // The CST can own trailing body comments before a DEDENT. Include only
    // actual owned comment tokens at deeper indentation, never documentation
    // for the next same-indent sibling. Interior blank lines follow that body
    // comment; boundary blank lines remain at the original site.
    for token in node
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|token| token.kind() == SyntaxKind::COMMENT)
    {
        let at = usize::from(token.text_range().start());
        if at < end {
            continue;
        }
        let comment_start = line_start(source, at);
        let comment_indent = &source[comment_start..at];
        if comment_start < end
            || !source[end..comment_start].chars().all(char::is_whitespace)
            || !comment_indent.starts_with(indent)
            || comment_indent.len() <= indent.len()
            || !comment_indent
                .chars()
                .all(|c| matches!(c, ' ' | '\t' | '\x0c'))
        {
            break;
        }
        end = line_end(source, usize::from(token.text_range().end()));
    }
    if attachment == TriviaAttachment::LeadingComments {
        while start > 0 {
            let mut previous_end = start;
            if source.as_bytes()[previous_end - 1] == b'\n' {
                previous_end -= 1;
            }
            if previous_end > 0 && source.as_bytes()[previous_end - 1] == b'\r' {
                previous_end -= 1;
            }
            let previous = line_start(source, previous_end);
            let line = source[previous..start].trim_end_matches(['\r', '\n']);
            let Some(comment) = line.strip_prefix(indent) else {
                break;
            };
            if !comment.starts_with('#')
                || boundary_comment(comment)
                || comment.starts_with("#!")
                || (indent.is_empty()
                    && previous <= line_end(source, 0)
                    && (comment.contains("coding:") || comment.contains("coding=")))
            {
                break;
            }
            start = previous;
        }
    }
    Ok(TextRange::new(
        TextSize::new(start as u32),
        TextSize::new(end as u32),
    ))
}
/// Immutable-tree rewrite plan; ownership is exact root identity, not matching
/// text. A same-text node from a different parse is rejected as stale/foreign.
#[derive(Debug)]
pub struct Rewrite {
    root: SyntaxNode,
    source: String,
    edits: Vec<TextEdit>,
}
impl Rewrite {
    pub fn new(root: SyntaxNode) -> Result<Self, RewriteError> {
        if root.is_mutable() {
            return Err(RewriteError::MutableRoot);
        }
        if root.parent().is_some() || root.text_range().start() != TextSize::new(0) {
            return Err(RewriteError::NotRoot);
        }
        if usize::from(root.text_range().len()) > MAX_OUTPUT_BYTES {
            return Err(RewriteError::Budget);
        }
        let source = print(&root);
        if source.len() > MAX_OUTPUT_BYTES {
            return Err(RewriteError::Budget);
        }
        Ok(Self {
            root,
            source,
            edits: Vec::new(),
        })
    }
    fn owns(&self, node: &SyntaxNode) -> Result<(), RewriteError> {
        if node.ancestors().last().as_ref() != Some(&self.root) {
            return Err(RewriteError::ForeignElement);
        }
        Ok(())
    }
    pub fn replace_node(
        &mut self,
        node: &SyntaxNode,
        replacement: &SyntaxNode,
    ) -> Result<(), RewriteError> {
        self.owns(node)?;
        self.replace_range(node.text_range(), print(replacement))
    }
    /// Typed facade: replacement must belong to the target's AST family.
    pub fn replace_ast<N: AstNode>(
        &mut self,
        node: &N,
        replacement: &N,
    ) -> Result<(), RewriteError> {
        self.replace_node(node.syntax(), replacement.syntax())
    }
    pub fn remove_node(&mut self, node: &SyntaxNode) -> Result<(), RewriteError> {
        self.owns(node)?;
        self.replace_range(node.text_range(), String::new())
    }
    /// Inserts a CST fragment at the exact target boundary. The caller supplies
    /// any grammatical separator as part of the fragment, not the printer.
    pub fn insert_before(
        &mut self,
        target: &SyntaxNode,
        fragment: &SyntaxNode,
    ) -> Result<(), RewriteError> {
        self.owns(target)?;
        self.replace_range(
            TextRange::empty(target.text_range().start()),
            print(fragment),
        )
    }
    pub fn replace_token(
        &mut self,
        token: &SyntaxToken,
        replacement: &str,
    ) -> Result<(), RewriteError> {
        self.owns(&token.parent().ok_or(RewriteError::ForeignElement)?)?;
        let (tokens, errors) = crate::tokenize(replacement);
        let nonempty: Vec<_> = tokens.into_iter().filter(|t| !t.range.is_empty()).collect();
        if !errors.is_empty()
            || nonempty.len() != 1
            || nonempty[0].kind != token.kind()
            || usize::from(nonempty[0].range.len()) != replacement.len()
        {
            return Err(RewriteError::InvalidToken);
        }
        self.replace_range(token.text_range(), replacement.to_string())
    }
    pub fn replace_range(&mut self, range: TextRange, text: String) -> Result<(), RewriteError> {
        self.extend_edits(&[(range, text)])
    }
    /// Failed operations leave all earlier planned edits intact.
    pub fn extend_edits(&mut self, edits: &[TextEdit]) -> Result<(), RewriteError> {
        if self.edits.len().saturating_add(edits.len()) > MAX_EDITS {
            return Err(RewriteError::Budget);
        }
        let mut candidate = self.edits.clone();
        candidate.extend_from_slice(edits);
        prepare_edits(&self.source, &candidate)?;
        self.edits = candidate;
        Ok(())
    }
    pub fn move_before(
        &mut self,
        node: &SyntaxNode,
        before: &SyntaxNode,
        attachment: TriviaAttachment,
    ) -> Result<(), RewriteError> {
        self.owns(node)?;
        self.owns(before)?;
        if node == before
            || node.parent() != before.parent()
            || !Stmt::can_cast(node.kind())
            || !Stmt::can_cast(before.kind())
        {
            return Err(RewriteError::NotSiblings);
        }
        let from = attached_range(&self.source, node, attachment)?;
        let to = attached_range(&self.source, before, attachment)?.start();
        if to >= from.start() && to <= from.end() {
            return Ok(());
        }
        let text = self.source[usize::from(from.start())..usize::from(from.end())].to_string();
        if attachment != TriviaAttachment::NodeOnly && !text.ends_with(['\r', '\n']) {
            return Err(RewriteError::MissingLineBreak);
        }
        self.extend_edits(&[(from, String::new()), (TextRange::empty(to), text)])
    }
    /// Relocates a statement across scopes. The caller explicitly supplies a
    /// source placeholder (e.g. pass) and transformation (indentation/newlines).
    /// Untouched spans remain exact; deletion+insertion validate atomically.
    pub fn relocate_statement(
        &mut self,
        node: &SyntaxNode,
        destination: TextSize,
        attachment: TriviaAttachment,
        source_placeholder: String,
        transform: impl FnOnce(&str) -> String,
    ) -> Result<(), RewriteError> {
        self.owns(node)?;
        let range = attached_range(&self.source, node, attachment)?;
        if destination >= range.start() && destination < range.end() {
            return Err(RewriteError::Conflict(range));
        }
        let moved = transform(&self.source[usize::from(range.start())..usize::from(range.end())]);
        self.extend_edits(&[
            (range, source_placeholder),
            (TextRange::empty(destination), moved),
        ])
    }
    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }
    pub fn finish(self) -> Result<String, RewriteError> {
        apply_text_edits(&self.source, &self.edits)
    }
}
#[cfg(test)]
#[path = "printer_tests.rs"]
mod tests;
