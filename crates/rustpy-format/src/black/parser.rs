//! Tokens and a recursive-descent parser for Black's `blib2to3` grammar
//! (`Grammar.txt`). The tree has exactly the shapes `pgen` produces: one node
//! per rule, nodes with a single child collapsed into that child, comments and
//! whitespace kept in the leaves' prefixes (`driver.py` prefix rules,
//! including how comments before a dedent are split between the closing block
//! and the next statement).

use rustpy_syntax::SyntaxKind as K;

use super::tree::*;

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub line: u32,
}

#[derive(Clone, Debug)]
struct BTok {
    ty: u16,
    value: String,
    prefix: String,
    lineno: u32,
    keep_string: bool,
}

/// Facts the tokenizer learns for target-version detection.
#[derive(Default, Clone, Copy, Debug)]
pub struct TokFeatures {
    pub f_strings: bool,
    pub t_strings: bool,
    pub debug_f_strings: bool,
}

const HARD_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "break", "class", "continue", "def", "del", "elif", "else", "except",
    "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not",
    "or", "pass", "raise", "return", "try", "while", "with", "yield",
];

pub fn is_hard_keyword(s: &str) -> bool {
    HARD_KEYWORDS.contains(&s)
}

fn op_type(k: K) -> Option<u16> {
    Some(match k {
        K::L_PAREN => LPAR,
        K::R_PAREN => RPAR,
        K::L_BRACK => LSQB,
        K::R_BRACK => RSQB,
        K::L_BRACE => LBRACE,
        K::R_BRACE => RBRACE,
        K::COMMA => COMMA,
        K::COLON => COLON,
        K::DOT => DOT,
        K::SEMICOLON => SEMI,
        K::AT => AT,
        K::EQ => EQUAL,
        K::ARROW => RARROW,
        K::PLUS_EQ => PLUSEQUAL,
        K::MINUS_EQ => MINEQUAL,
        K::STAR_EQ => STAREQUAL,
        K::SLASH_EQ => SLASHEQUAL,
        K::SLASH_SLASH_EQ => DOUBLESLASHEQUAL,
        K::PERCENT_EQ => PERCENTEQUAL,
        K::AT_EQ => ATEQUAL,
        K::AMP_EQ => AMPEREQUAL,
        K::PIPE_EQ => VBAREQUAL,
        K::CARET_EQ => CIRCUMFLEXEQUAL,
        K::SHR_EQ => RIGHTSHIFTEQUAL,
        K::SHL_EQ => LEFTSHIFTEQUAL,
        K::STAR_STAR_EQ => DOUBLESTAREQUAL,
        K::PLUS => PLUS,
        K::MINUS => MINUS,
        K::STAR => STAR,
        K::STAR_STAR => DOUBLESTAR,
        K::SLASH => SLASH,
        K::SLASH_SLASH => DOUBLESLASH,
        K::PERCENT => PERCENT,
        K::SHL => LEFTSHIFT,
        K::SHR => RIGHTSHIFT,
        K::AMP => AMPER,
        K::PIPE => VBAR,
        K::CARET => CIRCUMFLEX,
        K::TILDE => TILDE,
        K::COLON_EQ => COLONEQUAL,
        K::LT => LESS,
        K::GT => GREATER,
        K::LT_EQ => LESSEQUAL,
        K::GT_EQ => GREATEREQUAL,
        K::EQ_EQ => EQEQUAL,
        K::NOT_EQ => NOTEQUAL,
        _ => return None,
    })
}

/// `Driver._partially_consume_prefix`: the leading lines of `prefix` that are
/// indented at least `column` deep, and the rest.
fn partially_consume_prefix(prefix: &str, column: usize) -> (String, String) {
    let mut lines = String::new();
    let mut current_line = String::new();
    let mut current_column = 0usize;
    let mut wait_for_nl = false;
    for ch in prefix.chars() {
        current_line.push(ch);
        if wait_for_nl {
            if ch == '\n' {
                if !current_line.trim().is_empty() && current_column < column {
                    let rest = prefix[lines.len()..].to_string();
                    return (lines, rest);
                }
                lines.push_str(&current_line);
                current_line.clear();
                current_column = 0;
                wait_for_nl = false;
            }
        } else if ch == ' ' || ch == '\t' {
            current_column += 1;
        } else if ch == '\n' || ch == '\x0c' {
            current_column = 0;
        } else {
            wait_for_nl = true;
        }
    }
    (lines, current_line)
}

/// The prefix Black's driver builds from the text between two tokens. The
/// driver keeps whitespace, comments and newlines as written, but Black's
/// tokenizer only turns a backslash continuation into an NL token (kept in the
/// prefix) when it follows code on its line; one that follows a newline, such
/// as a line holding nothing but `\`, is dropped together with its line's
/// whitespace, leaving just the line break. `after_newline`: the gap starts
/// after a NEWLINE token or at the start of the file.
fn driver_prefix(gap: &str, mut after_newline: bool) -> String {
    if !gap.contains('\\') {
        return gap.to_string();
    }
    let mut out = String::with_capacity(gap.len());
    let mut line_start = 0;
    let mut rest = gap;
    while let Some(c) = rest.chars().next() {
        if c == '#' {
            let end = rest.find('\n').unwrap_or(rest.len());
            out.push_str(&rest[..end]);
            after_newline = false;
            rest = &rest[end..];
        } else if c == '\n' || rest.starts_with("\\\n") {
            if c == '\\' {
                if after_newline {
                    out.truncate(line_start);
                } else {
                    out.push('\\');
                }
                rest = &rest[1..];
            }
            out.push('\n');
            rest = &rest[1..];
            line_start = out.len();
            after_newline = true;
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

fn tokenize(src: &str) -> Result<(Vec<BTok>, TokFeatures), ParseError> {
    let (toks, diags) = rustpy_syntax::tokenize(src);
    let mut line_starts = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let line_of = |off: usize| -> u32 {
        match line_starts.binary_search(&off) {
            Ok(i) => i as u32 + 1,
            Err(i) => i as u32,
        }
    };
    if let Some(d) = diags.first() {
        return Err(ParseError {
            message: d.message.to_string(),
            line: line_of(usize::from(d.range.start())),
        });
    }
    let mut feats = TokFeatures::default();
    let mut out: Vec<BTok> = Vec::with_capacity(toks.len());
    let mut prev_end = 0usize;
    // Whether the token before `prev_end` is a NEWLINE (or there is none).
    let mut after_newline = true;
    // Prefix text a DEDENT left over for the next token.
    let mut carried = String::new();
    let mut indent_cols: Vec<usize> = Vec::new();
    let next_sig = |mut j: usize| -> usize {
        while j < toks.len() {
            if !matches!(
                toks[j].kind,
                K::WHITESPACE | K::COMMENT | K::NL | K::DEDENT | K::INDENT
            ) {
                return usize::from(toks[j].range.start());
            }
            j += 1;
        }
        src.len()
    };
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        let start = usize::from(t.range.start());
        let end = usize::from(t.range.end());
        match t.kind {
            K::WHITESPACE | K::COMMENT | K::NL => {
                i += 1;
                continue;
            }
            K::INDENT => {
                indent_cols.push(src[start..end].chars().count());
                out.push(BTok {
                    ty: INDENT,
                    value: String::new(),
                    prefix: String::new(),
                    lineno: line_of(start),
                    keep_string: false,
                });
                i += 1;
                continue;
            }
            K::DEDENT => {
                let mut n = 0;
                while i < toks.len() && toks[i].kind == K::DEDENT {
                    n += 1;
                    i += 1;
                }
                let next_start = next_sig(i);
                let mut gap = std::mem::take(&mut carried)
                    + &driver_prefix(&src[prev_end..next_start], after_newline);
                for _ in 0..n {
                    let col = indent_cols.pop().unwrap_or(0);
                    let (consumed, rest) = partially_consume_prefix(&gap, col);
                    out.push(BTok {
                        ty: DEDENT,
                        value: String::new(),
                        prefix: consumed,
                        lineno: line_of(next_start),
                        keep_string: false,
                    });
                    gap = rest;
                }
                carried = gap;
                prev_end = next_start;
                continue;
            }
            K::ERROR_TOKEN | K::BANG => {
                return Err(ParseError {
                    message: "invalid token".into(),
                    line: line_of(start),
                });
            }
            _ => {}
        }
        let prefix =
            std::mem::take(&mut carried) + &driver_prefix(&src[prev_end..start], after_newline);
        let lineno = line_of(start);
        if matches!(t.kind, K::FSTRING_START | K::TSTRING_START) {
            let (start_kind, end_kind) = if t.kind == K::FSTRING_START {
                feats.f_strings = true;
                (K::FSTRING_START, K::FSTRING_END)
            } else {
                feats.t_strings = true;
                (K::TSTRING_START, K::TSTRING_END)
            };
            // Merge the whole literal into one STRING token, noting whether a
            // replacement field contains a backslash (Black then leaves the
            // literal alone) or a self-documenting `=`.
            let mut nest = 0usize;
            let mut brace = 0usize;
            let mut field_start = 0usize;
            let mut keep = false;
            let mut j = i;
            let mut lit_end = end;
            while j < toks.len() {
                let k = toks[j].kind;
                let ks = usize::from(toks[j].range.start());
                let ke = usize::from(toks[j].range.end());
                if k == start_kind || (k == K::FSTRING_START || k == K::TSTRING_START) {
                    nest += 1;
                } else if k == end_kind || k == K::FSTRING_END || k == K::TSTRING_END {
                    nest -= 1;
                    if nest == 0 {
                        lit_end = ke;
                        break;
                    }
                } else if k == K::L_BRACE {
                    if brace == 0 {
                        field_start = ks;
                    }
                    brace += 1;
                } else if k == K::R_BRACE {
                    brace = brace.saturating_sub(1);
                    if brace == 0 && src[field_start..ke].contains('\\') {
                        keep = true;
                    }
                } else if k == K::EQ && brace == 1 {
                    if let Some(nk) = toks[j + 1..]
                        .iter()
                        .find(|x| !matches!(x.kind, K::WHITESPACE | K::NL | K::COMMENT))
                    {
                        if matches!(nk.kind, K::R_BRACE | K::BANG | K::COLON) {
                            feats.debug_f_strings = true;
                        }
                    }
                }
                j += 1;
            }
            out.push(BTok {
                ty: STRING,
                value: src[start..lit_end].to_string(),
                prefix,
                lineno,
                keep_string: keep,
            });
            prev_end = lit_end;
            after_newline = false;
            i = j + 1;
            continue;
        }
        let text = &src[start..end];
        let (ty, value) = match t.kind {
            K::NEWLINE => (NEWLINE, "\n".to_string()),
            K::IDENT => (NAME, text.to_string()),
            K::ASYNC_KW => (ASYNC, text.to_string()),
            K::AWAIT_KW => (AWAIT, text.to_string()),
            k if k.is_keyword() => (NAME, text.to_string()),
            K::INT_NUMBER | K::FLOAT_NUMBER | K::COMPLEX_NUMBER => (NUMBER, text.to_string()),
            K::STRING => (STRING, text.to_string()),
            K::ELLIPSIS => {
                for k in 0..3 {
                    out.push(BTok {
                        ty: DOT,
                        value: ".".into(),
                        prefix: if k == 0 {
                            prefix.clone()
                        } else {
                            String::new()
                        },
                        lineno,
                        keep_string: false,
                    });
                }
                prev_end = end;
                after_newline = false;
                i += 1;
                continue;
            }
            k => match op_type(k) {
                Some(ty) => (ty, text.to_string()),
                None => {
                    return Err(ParseError {
                        message: format!("unexpected token {k:?}"),
                        line: lineno,
                    })
                }
            },
        };
        after_newline = ty == NEWLINE;
        out.push(BTok {
            ty,
            value,
            prefix,
            lineno,
            keep_string: false,
        });
        prev_end = end;
        i += 1;
    }
    out.push(BTok {
        ty: ENDMARKER,
        value: String::new(),
        prefix: carried + &driver_prefix(&src[prev_end..], after_newline),
        lineno: line_starts.len() as u32,
        keep_string: false,
    });
    Ok((out, feats))
}

type PResult = Result<Id, ParseError>;

struct Parser {
    toks: Vec<BTok>,
    pos: usize,
    tree: Tree,
}

/// Parses `src` (which must end with a newline) into a `file_input` tree.
pub fn parse(src: &str) -> Result<(Tree, Id, TokFeatures), ParseError> {
    let (toks, feats) = tokenize(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        tree: Tree::new(),
    };
    let root = p.file_input()?;
    Ok((p.tree, root, feats))
}

impl Parser {
    fn tok(&self) -> &BTok {
        &self.toks[self.pos.min(self.toks.len() - 1)]
    }

    fn tok_at(&self, k: usize) -> &BTok {
        &self.toks[(self.pos + k).min(self.toks.len() - 1)]
    }

    fn ty(&self) -> u16 {
        self.tok().ty
    }

    fn is_kw(&self, kw: &str) -> bool {
        let t = self.tok();
        t.ty == NAME && t.value == kw
    }

    fn err<T>(&self, what: &str) -> Result<T, ParseError> {
        let t = self.tok();
        Err(ParseError {
            message: format!("{what} (got {} {:?})", type_name(t.ty), t.value),
            line: t.lineno,
        })
    }

    fn bump(&mut self) -> Id {
        let t = &self.toks[self.pos];
        let id = self
            .tree
            .leaf_with_prefix(t.ty, t.value.clone(), t.prefix.clone());
        let n = self.tree.nm(id);
        n.lineno = t.lineno;
        n.keep_string = t.keep_string;
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        id
    }

    fn expect(&mut self, ty: u16) -> PResult {
        if self.ty() == ty {
            Ok(self.bump())
        } else {
            self.err(&format!("expected {}", type_name(ty)))
        }
    }

    fn expect_kw(&mut self, kw: &str) -> PResult {
        if self.is_kw(kw) {
            Ok(self.bump())
        } else {
            self.err(&format!("expected '{kw}'"))
        }
    }

    fn expect_name(&mut self) -> PResult {
        if self.ty() == NAME {
            Ok(self.bump())
        } else {
            self.err("expected a name")
        }
    }

    fn make(&mut self, sym: u16, children: Vec<Id>) -> Id {
        if children.len() == 1 {
            children[0]
        } else {
            self.tree.node(sym, children)
        }
    }

    // ---- FIRST sets

    fn starts_atom(&self) -> bool {
        let t = self.tok();
        match t.ty {
            NUMBER | STRING | LPAR | LSQB | LBRACE | DOT => true,
            NAME => !is_hard_keyword(&t.value),
            _ => false,
        }
    }

    fn starts_expr(&self) -> bool {
        self.starts_atom() || matches!(self.ty(), AWAIT | PLUS | MINUS | TILDE)
    }

    fn starts_test(&self) -> bool {
        self.starts_expr() || self.is_kw("not") || self.is_kw("lambda")
    }

    fn at_comp_for(&self) -> bool {
        self.is_kw("for")
            || (self.ty() == ASYNC && {
                let t = self.tok_at(1);
                t.ty == NAME && t.value == "for"
            })
    }

    // ---- statements

    fn file_input(&mut self) -> PResult {
        let mut ch = Vec::new();
        loop {
            match self.ty() {
                ENDMARKER => {
                    ch.push(self.bump());
                    break;
                }
                NEWLINE => ch.push(self.bump()),
                _ => ch.push(self.stmt()?),
            }
        }
        Ok(self.tree.node(FILE_INPUT, ch))
    }

    fn stmt(&mut self) -> PResult {
        let t = self.tok();
        if t.ty == NAME {
            match t.value.as_str() {
                "if" => return self.if_stmt(),
                "while" => return self.while_stmt(),
                "for" => return self.for_stmt(),
                "try" => return self.try_stmt(),
                "with" => return self.with_stmt(),
                "def" => return self.funcdef(),
                "class" => return self.classdef(),
                "match" => {
                    if let Some(m) = self.try_match_stmt() {
                        return Ok(m);
                    }
                }
                _ => {}
            }
        } else if t.ty == AT {
            return self.decorated();
        } else if t.ty == ASYNC {
            return self.async_stmt();
        }
        self.simple_stmt()
    }

    fn simple_stmt(&mut self) -> PResult {
        let mut ch = vec![self.small_stmt()?];
        while self.ty() == SEMI {
            ch.push(self.bump());
            if self.ty() == NEWLINE {
                break;
            }
            ch.push(self.small_stmt()?);
        }
        ch.push(self.expect(NEWLINE)?);
        Ok(self.tree.node(SIMPLE_STMT, ch))
    }

    fn small_stmt(&mut self) -> PResult {
        let t = self.tok();
        if t.ty == NAME {
            match t.value.as_str() {
                "del" => {
                    let kw = self.bump();
                    let target = self.exprlist()?;
                    return Ok(self.tree.node(DEL_STMT, vec![kw, target]));
                }
                "pass" | "break" | "continue" => return Ok(self.bump()),
                "return" => {
                    let kw = self.bump();
                    let mut ch = vec![kw];
                    if self.starts_test() || self.ty() == STAR {
                        ch.push(self.testlist_star_expr()?);
                    }
                    return Ok(self.make(RETURN_STMT, ch));
                }
                "raise" => {
                    let mut ch = vec![self.bump()];
                    if self.starts_test() {
                        ch.push(self.test()?);
                        if self.is_kw("from") {
                            ch.push(self.bump());
                            ch.push(self.test()?);
                        }
                    }
                    return Ok(self.make(RAISE_STMT, ch));
                }
                "yield" => return self.yield_expr(),
                "import" => {
                    let kw = self.bump();
                    let names = self.dotted_as_names()?;
                    return Ok(self.tree.node(IMPORT_NAME, vec![kw, names]));
                }
                "from" => return self.import_from(),
                "global" | "nonlocal" => {
                    let mut ch = vec![self.bump(), self.expect_name()?];
                    while self.ty() == COMMA {
                        ch.push(self.bump());
                        ch.push(self.expect_name()?);
                    }
                    return Ok(self.tree.node(GLOBAL_STMT, ch));
                }
                "assert" => {
                    let mut ch = vec![self.bump(), self.test()?];
                    if self.ty() == COMMA {
                        ch.push(self.bump());
                        ch.push(self.test()?);
                    }
                    return Ok(self.tree.node(ASSERT_STMT, ch));
                }
                "type" => {
                    let n1 = self.tok_at(1);
                    let n2 = self.tok_at(2);
                    if n1.ty == NAME && !is_hard_keyword(&n1.value) && matches!(n2.ty, EQUAL | LSQB)
                    {
                        let mut ch = vec![self.bump(), self.bump()];
                        if self.ty() == LSQB {
                            ch.push(self.typeparams()?);
                        }
                        ch.push(self.expect(EQUAL)?);
                        ch.push(self.test()?);
                        return Ok(self.tree.node(TYPE_STMT, ch));
                    }
                }
                _ => {}
            }
        }
        self.expr_stmt()
    }

    fn expr_stmt(&mut self) -> PResult {
        let first = self.testlist_star_expr()?;
        let mut ch = vec![first];
        let ty = self.ty();
        if ty == COLON {
            let mut a = vec![self.bump(), self.test()?];
            if self.ty() == EQUAL {
                a.push(self.bump());
                a.push(if self.is_kw("yield") {
                    self.yield_expr()?
                } else {
                    self.testlist_star_expr()?
                });
            }
            ch.push(self.tree.node(ANNASSIGN, a));
        } else if matches!(
            ty,
            PLUSEQUAL
                | MINEQUAL
                | STAREQUAL
                | ATEQUAL
                | SLASHEQUAL
                | PERCENTEQUAL
                | AMPEREQUAL
                | VBAREQUAL
                | CIRCUMFLEXEQUAL
                | LEFTSHIFTEQUAL
                | RIGHTSHIFTEQUAL
                | DOUBLESTAREQUAL
                | DOUBLESLASHEQUAL
        ) {
            ch.push(self.bump());
            ch.push(if self.is_kw("yield") {
                self.yield_expr()?
            } else {
                self.testlist()?
            });
        } else {
            while self.ty() == EQUAL {
                ch.push(self.bump());
                ch.push(if self.is_kw("yield") {
                    self.yield_expr()?
                } else {
                    self.testlist_star_expr()?
                });
            }
        }
        Ok(self.make(EXPR_STMT, ch))
    }

    fn yield_expr(&mut self) -> PResult {
        let kw = self.expect_kw("yield")?;
        let mut ch = vec![kw];
        if self.is_kw("from") {
            let f = self.bump();
            let t = self.test()?;
            ch.push(self.tree.node(YIELD_ARG, vec![f, t]));
        } else if self.starts_test() || self.ty() == STAR {
            ch.push(self.testlist_star_expr()?);
        }
        Ok(self.make(YIELD_EXPR, ch))
    }

    fn dotted_name(&mut self) -> PResult {
        let mut ch = vec![self.expect_name()?];
        while self.ty() == DOT {
            ch.push(self.bump());
            ch.push(self.expect_name()?);
        }
        Ok(self.make(DOTTED_NAME, ch))
    }

    fn dotted_as_names(&mut self) -> PResult {
        let mut ch = vec![self.dotted_as_name()?];
        while self.ty() == COMMA {
            ch.push(self.bump());
            ch.push(self.dotted_as_name()?);
        }
        Ok(self.make(DOTTED_AS_NAMES, ch))
    }

    fn dotted_as_name(&mut self) -> PResult {
        let mut ch = vec![self.dotted_name()?];
        if self.is_kw("as") {
            ch.push(self.bump());
            ch.push(self.expect_name()?);
        }
        Ok(self.make(DOTTED_AS_NAME, ch))
    }

    fn import_as_name(&mut self) -> PResult {
        let mut ch = vec![self.expect_name()?];
        if self.is_kw("as") {
            ch.push(self.bump());
            ch.push(self.expect_name()?);
        }
        Ok(self.make(IMPORT_AS_NAME, ch))
    }

    fn import_as_names(&mut self) -> PResult {
        let mut ch = vec![self.import_as_name()?];
        while self.ty() == COMMA {
            ch.push(self.bump());
            if self.ty() != NAME {
                break;
            }
            ch.push(self.import_as_name()?);
        }
        Ok(self.make(IMPORT_AS_NAMES, ch))
    }

    fn import_from(&mut self) -> PResult {
        let mut ch = vec![self.expect_kw("from")?];
        let mut dots = 0;
        while self.ty() == DOT {
            ch.push(self.bump());
            dots += 1;
        }
        if !self.is_kw("import") || dots == 0 {
            ch.push(self.dotted_name()?);
        }
        ch.push(self.expect_kw("import")?);
        match self.ty() {
            STAR => ch.push(self.bump()),
            LPAR => {
                ch.push(self.bump());
                ch.push(self.import_as_names()?);
                ch.push(self.expect(RPAR)?);
            }
            _ => ch.push(self.import_as_names()?),
        }
        Ok(self.tree.node(IMPORT_FROM, ch))
    }

    fn suite(&mut self) -> PResult {
        if self.ty() == NEWLINE {
            let mut ch = vec![self.bump(), self.expect(INDENT)?];
            loop {
                ch.push(self.stmt()?);
                if self.ty() == DEDENT {
                    ch.push(self.bump());
                    break;
                }
                if self.ty() == ENDMARKER {
                    return self.err("expected dedent");
                }
            }
            Ok(self.tree.node(SUITE, ch))
        } else {
            self.simple_stmt()
        }
    }

    fn colon_suite(&mut self, ch: &mut Vec<Id>) -> Result<(), ParseError> {
        ch.push(self.expect(COLON)?);
        ch.push(self.suite()?);
        Ok(())
    }

    fn if_stmt(&mut self) -> PResult {
        let mut ch = vec![self.bump(), self.namedexpr_test()?];
        self.colon_suite(&mut ch)?;
        while self.is_kw("elif") {
            ch.push(self.bump());
            ch.push(self.namedexpr_test()?);
            self.colon_suite(&mut ch)?;
        }
        if self.is_kw("else") {
            ch.push(self.bump());
            self.colon_suite(&mut ch)?;
        }
        Ok(self.tree.node(IF_STMT, ch))
    }

    fn while_stmt(&mut self) -> PResult {
        let mut ch = vec![self.bump(), self.namedexpr_test()?];
        self.colon_suite(&mut ch)?;
        if self.is_kw("else") {
            ch.push(self.bump());
            self.colon_suite(&mut ch)?;
        }
        Ok(self.tree.node(WHILE_STMT, ch))
    }

    fn for_stmt(&mut self) -> PResult {
        let mut ch = vec![self.bump(), self.exprlist()?];
        ch.push(self.expect_kw("in")?);
        ch.push(self.testlist_star_expr()?);
        self.colon_suite(&mut ch)?;
        if self.is_kw("else") {
            ch.push(self.bump());
            self.colon_suite(&mut ch)?;
        }
        Ok(self.tree.node(FOR_STMT, ch))
    }

    fn try_stmt(&mut self) -> PResult {
        let mut ch = vec![self.bump()];
        self.colon_suite(&mut ch)?;
        while self.is_kw("except") {
            let mut ec = vec![self.bump()];
            if self.ty() == STAR {
                ec.push(self.bump());
            }
            if self.starts_test() {
                ec.push(self.testlist()?);
                if self.is_kw("as") {
                    ec.push(self.bump());
                    ec.push(self.test()?);
                }
            }
            ch.push(self.make(EXCEPT_CLAUSE, ec));
            self.colon_suite(&mut ch)?;
        }
        if self.is_kw("else") {
            ch.push(self.bump());
            self.colon_suite(&mut ch)?;
        }
        if self.is_kw("finally") {
            ch.push(self.bump());
            self.colon_suite(&mut ch)?;
        }
        Ok(self.tree.node(TRY_STMT, ch))
    }

    fn with_stmt(&mut self) -> PResult {
        let mut ch = vec![self.bump(), self.asexpr_test()?];
        while self.ty() == COMMA {
            ch.push(self.bump());
            ch.push(self.asexpr_test()?);
        }
        self.colon_suite(&mut ch)?;
        Ok(self.tree.node(WITH_STMT, ch))
    }

    fn funcdef(&mut self) -> PResult {
        let mut ch = vec![self.expect_kw("def")?, self.expect_name()?];
        if self.ty() == LSQB {
            ch.push(self.typeparams()?);
        }
        ch.push(self.parameters()?);
        if self.ty() == RARROW {
            ch.push(self.bump());
            ch.push(self.test()?);
        }
        self.colon_suite(&mut ch)?;
        Ok(self.tree.node(FUNCDEF, ch))
    }

    fn parameters(&mut self) -> PResult {
        let mut ch = vec![self.expect(LPAR)?];
        if self.ty() != RPAR {
            let mut args = Vec::new();
            loop {
                match self.ty() {
                    STAR => {
                        args.push(self.bump());
                        if self.ty() == NAME {
                            args.push(self.tname(true)?);
                        }
                    }
                    DOUBLESTAR => {
                        args.push(self.bump());
                        args.push(self.tname(false)?);
                    }
                    SLASH => args.push(self.bump()),
                    NAME => {
                        args.push(self.tname(false)?);
                        if self.ty() == EQUAL {
                            args.push(self.bump());
                            args.push(self.test()?);
                        }
                    }
                    _ => return self.err("bad parameter"),
                }
                if self.ty() == COMMA {
                    args.push(self.bump());
                    if self.ty() == RPAR {
                        break;
                    }
                } else {
                    break;
                }
            }
            ch.push(self.make(TYPEDARGSLIST, args));
        }
        ch.push(self.expect(RPAR)?);
        Ok(self.tree.node(PARAMETERS, ch))
    }

    fn tname(&mut self, star: bool) -> PResult {
        let mut ch = vec![self.expect_name()?];
        if self.ty() == COLON {
            ch.push(self.bump());
            if star && self.ty() == STAR {
                ch.push(self.star_expr()?);
            } else {
                ch.push(self.test()?);
            }
        }
        Ok(self.make(if star { TNAME_STAR } else { TNAME }, ch))
    }

    fn varargslist(&mut self, old: bool) -> PResult {
        let mut ch = Vec::new();
        loop {
            match self.ty() {
                STAR => {
                    ch.push(self.bump());
                    if self.ty() == NAME {
                        ch.push(self.bump());
                    }
                }
                DOUBLESTAR => {
                    ch.push(self.bump());
                    ch.push(self.expect_name()?);
                }
                SLASH => ch.push(self.bump()),
                NAME if !is_hard_keyword(&self.tok().value) => {
                    ch.push(self.bump());
                    if self.ty() == EQUAL {
                        ch.push(self.bump());
                        ch.push(self.test()?);
                    }
                }
                _ => break,
            }
            if self.ty() == COMMA {
                ch.push(self.bump());
            } else {
                break;
            }
        }
        let _ = old;
        Ok(self.make(VARARGSLIST, ch))
    }

    fn typeparams(&mut self) -> PResult {
        let mut ch = vec![self.expect(LSQB)?];
        loop {
            let item = match self.ty() {
                STAR => {
                    let mut c = vec![self.bump(), self.expect_name()?];
                    if self.ty() == EQUAL {
                        c.push(self.bump());
                        c.push(if self.ty() == STAR {
                            self.star_expr()?
                        } else {
                            self.test()?
                        });
                    }
                    self.tree.node(TYPEVARTUPLE, c)
                }
                DOUBLESTAR => {
                    let mut c = vec![self.bump(), self.expect_name()?];
                    if self.ty() == EQUAL {
                        c.push(self.bump());
                        c.push(self.test()?);
                    }
                    self.tree.node(PARAMSPEC, c)
                }
                _ => {
                    let mut c = vec![self.expect_name()?];
                    if self.ty() == COLON {
                        c.push(self.bump());
                        c.push(self.test()?);
                    }
                    if self.ty() == EQUAL {
                        c.push(self.bump());
                        c.push(self.test()?);
                    }
                    self.make(TYPEVAR, c)
                }
            };
            ch.push(item);
            if self.ty() == COMMA {
                ch.push(self.bump());
                if self.ty() == RSQB {
                    break;
                }
            } else {
                break;
            }
        }
        ch.push(self.expect(RSQB)?);
        Ok(self.tree.node(TYPEPARAMS, ch))
    }

    fn classdef(&mut self) -> PResult {
        let mut ch = vec![self.expect_kw("class")?, self.expect_name()?];
        if self.ty() == LSQB {
            ch.push(self.typeparams()?);
        }
        if self.ty() == LPAR {
            ch.push(self.bump());
            if self.ty() != RPAR {
                ch.push(self.arglist()?);
            }
            ch.push(self.expect(RPAR)?);
        }
        self.colon_suite(&mut ch)?;
        Ok(self.tree.node(CLASSDEF, ch))
    }

    fn decorated(&mut self) -> PResult {
        let mut decs = Vec::new();
        while self.ty() == AT {
            let at = self.bump();
            let e = self.namedexpr_test()?;
            let nl = self.expect(NEWLINE)?;
            decs.push(self.tree.node(DECORATOR, vec![at, e, nl]));
        }
        let d = self.make(DECORATORS, decs);
        let def = if self.is_kw("class") {
            self.classdef()?
        } else if self.is_kw("def") {
            self.funcdef()?
        } else if self.ty() == ASYNC {
            let a = self.bump();
            let f = self.funcdef()?;
            self.tree.node(ASYNC_FUNCDEF, vec![a, f])
        } else {
            return self.err("expected def or class after decorator");
        };
        Ok(self.tree.node(DECORATED, vec![d, def]))
    }

    fn async_stmt(&mut self) -> PResult {
        let a = self.bump();
        let s = if self.is_kw("def") {
            self.funcdef()?
        } else if self.is_kw("with") {
            self.with_stmt()?
        } else if self.is_kw("for") {
            self.for_stmt()?
        } else {
            return self.err("expected def, with or for after async");
        };
        Ok(self.tree.node(ASYNC_STMT, vec![a, s]))
    }

    fn try_match_stmt(&mut self) -> Option<Id> {
        let save_pos = self.pos;
        let save_len = self.tree.nodes.len();
        match self.match_stmt() {
            Ok(id) => Some(id),
            Err(_) => {
                self.pos = save_pos;
                self.tree.nodes.truncate(save_len);
                None
            }
        }
    }

    fn match_stmt(&mut self) -> PResult {
        let mut ch = vec![self.expect_kw("match")?];
        let mut subj = vec![self.namedexpr_or_star()?];
        while self.ty() == COMMA {
            subj.push(self.bump());
            if !(self.starts_test() || self.ty() == STAR) {
                break;
            }
            subj.push(self.namedexpr_or_star()?);
        }
        ch.push(self.make(SUBJECT_EXPR, subj));
        ch.push(self.expect(COLON)?);
        ch.push(self.expect(NEWLINE)?);
        ch.push(self.expect(INDENT)?);
        loop {
            ch.push(self.case_block()?);
            if self.ty() == DEDENT {
                ch.push(self.bump());
                break;
            }
        }
        Ok(self.tree.node(MATCH_STMT, ch))
    }

    fn case_block(&mut self) -> PResult {
        let mut ch = vec![self.expect_kw("case")?];
        let mut pats = vec![self.pattern()?];
        while self.ty() == COMMA {
            pats.push(self.bump());
            if !(self.starts_expr() || self.ty() == STAR) {
                break;
            }
            pats.push(self.pattern()?);
        }
        ch.push(self.make(PATTERNS, pats));
        if self.is_kw("if") {
            let i = self.bump();
            let c = self.namedexpr_test()?;
            ch.push(self.tree.node(GUARD, vec![i, c]));
        }
        self.colon_suite(&mut ch)?;
        Ok(self.tree.node(CASE_BLOCK, ch))
    }

    fn pattern(&mut self) -> PResult {
        let mut ch = vec![if self.ty() == STAR {
            self.star_expr()?
        } else {
            self.expr()?
        }];
        if self.is_kw("as") {
            ch.push(self.bump());
            ch.push(self.expr()?);
        }
        Ok(self.make(PATTERN, ch))
    }

    // ---- expressions

    fn test_or_star(&mut self) -> PResult {
        if self.ty() == STAR {
            self.star_expr()
        } else {
            self.test()
        }
    }

    fn namedexpr_or_star(&mut self) -> PResult {
        if self.ty() == STAR {
            self.star_expr()
        } else {
            self.namedexpr_test()
        }
    }

    fn testlist_star_expr(&mut self) -> PResult {
        let mut ch = vec![self.test_or_star()?];
        while self.ty() == COMMA {
            ch.push(self.bump());
            if !(self.starts_test() || self.ty() == STAR) {
                break;
            }
            ch.push(self.test_or_star()?);
        }
        Ok(self.make(TESTLIST_STAR_EXPR, ch))
    }

    fn testlist(&mut self) -> PResult {
        let mut ch = vec![self.test()?];
        while self.ty() == COMMA {
            ch.push(self.bump());
            if !self.starts_test() {
                break;
            }
            ch.push(self.test()?);
        }
        Ok(self.make(TESTLIST, ch))
    }

    fn exprlist(&mut self) -> PResult {
        let mut ch = vec![if self.ty() == STAR {
            self.star_expr()?
        } else {
            self.expr()?
        }];
        while self.ty() == COMMA {
            ch.push(self.bump());
            if !(self.starts_expr() || self.ty() == STAR) {
                break;
            }
            ch.push(if self.ty() == STAR {
                self.star_expr()?
            } else {
                self.expr()?
            });
        }
        Ok(self.make(EXPRLIST, ch))
    }

    fn namedexpr_test(&mut self) -> PResult {
        let a = self.asexpr_test()?;
        if self.ty() == COLONEQUAL {
            let op = self.bump();
            let b = self.asexpr_test()?;
            return Ok(self.tree.node(NAMEDEXPR_TEST, vec![a, op, b]));
        }
        Ok(a)
    }

    fn asexpr_test(&mut self) -> PResult {
        let a = self.test()?;
        if self.is_kw("as") {
            let op = self.bump();
            let b = self.test()?;
            return Ok(self.tree.node(ASEXPR_TEST, vec![a, op, b]));
        }
        Ok(a)
    }

    fn test(&mut self) -> PResult {
        if self.is_kw("lambda") {
            return self.lambdef(false);
        }
        let a = self.or_test()?;
        if self.is_kw("if") {
            let i = self.bump();
            let c = self.or_test()?;
            let e = self.expect_kw("else")?;
            let b = self.test()?;
            return Ok(self.tree.node(TEST, vec![a, i, c, e, b]));
        }
        Ok(a)
    }

    fn old_test(&mut self) -> PResult {
        if self.is_kw("lambda") {
            return self.lambdef(true);
        }
        self.or_test()
    }

    fn lambdef(&mut self, old: bool) -> PResult {
        let mut ch = vec![self.bump()];
        if self.ty() != COLON {
            ch.push(self.varargslist(old)?);
        }
        ch.push(self.expect(COLON)?);
        ch.push(if old { self.old_test()? } else { self.test()? });
        Ok(self.tree.node(if old { OLD_LAMBDEF } else { LAMBDEF }, ch))
    }

    fn or_test(&mut self) -> PResult {
        let mut ch = vec![self.and_test()?];
        while self.is_kw("or") {
            ch.push(self.bump());
            ch.push(self.and_test()?);
        }
        Ok(self.make(OR_TEST, ch))
    }

    fn and_test(&mut self) -> PResult {
        let mut ch = vec![self.not_test()?];
        while self.is_kw("and") {
            ch.push(self.bump());
            ch.push(self.not_test()?);
        }
        Ok(self.make(AND_TEST, ch))
    }

    fn not_test(&mut self) -> PResult {
        if self.is_kw("not") {
            let n = self.bump();
            let e = self.not_test()?;
            return Ok(self.tree.node(NOT_TEST, vec![n, e]));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> PResult {
        let mut ch = vec![self.expr()?];
        loop {
            let t = self.tok();
            let op = match t.ty {
                LESS | GREATER | EQEQUAL | GREATEREQUAL | LESSEQUAL | NOTEQUAL => self.bump(),
                NAME if t.value == "in" => self.bump(),
                NAME if t.value == "not" && {
                    let n = self.tok_at(1);
                    n.ty == NAME && n.value == "in"
                } =>
                {
                    let a = self.bump();
                    let b = self.bump();
                    self.tree.node(COMP_OP, vec![a, b])
                }
                NAME if t.value == "is" => {
                    let a = self.bump();
                    if self.is_kw("not") {
                        let b = self.bump();
                        self.tree.node(COMP_OP, vec![a, b])
                    } else {
                        a
                    }
                }
                _ => break,
            };
            ch.push(op);
            ch.push(self.expr()?);
        }
        Ok(self.make(COMPARISON, ch))
    }

    fn star_expr(&mut self) -> PResult {
        let s = self.expect(STAR)?;
        let e = self.expr()?;
        Ok(self.tree.node(STAR_EXPR, vec![s, e]))
    }

    fn binary(&mut self, sym: u16, ops: &[u16], next: fn(&mut Self) -> PResult) -> PResult {
        let mut ch = vec![next(self)?];
        while ops.contains(&self.ty()) {
            ch.push(self.bump());
            ch.push(next(self)?);
        }
        Ok(self.make(sym, ch))
    }

    fn expr(&mut self) -> PResult {
        self.binary(EXPR, &[VBAR], Self::xor_expr)
    }

    fn xor_expr(&mut self) -> PResult {
        self.binary(XOR_EXPR, &[CIRCUMFLEX], Self::and_expr)
    }

    fn and_expr(&mut self) -> PResult {
        self.binary(AND_EXPR, &[AMPER], Self::shift_expr)
    }

    fn shift_expr(&mut self) -> PResult {
        self.binary(SHIFT_EXPR, &[LEFTSHIFT, RIGHTSHIFT], Self::arith_expr)
    }

    fn arith_expr(&mut self) -> PResult {
        self.binary(ARITH_EXPR, &[PLUS, MINUS], Self::term)
    }

    fn term(&mut self) -> PResult {
        self.binary(TERM, &[STAR, AT, SLASH, PERCENT, DOUBLESLASH], Self::factor)
    }

    fn factor(&mut self) -> PResult {
        if matches!(self.ty(), PLUS | MINUS | TILDE) {
            let op = self.bump();
            let f = self.factor()?;
            return Ok(self.tree.node(FACTOR, vec![op, f]));
        }
        self.power()
    }

    fn power(&mut self) -> PResult {
        let mut ch = Vec::new();
        if self.ty() == AWAIT {
            ch.push(self.bump());
        }
        ch.push(self.atom()?);
        while matches!(self.ty(), LPAR | LSQB | DOT) {
            ch.push(self.trailer()?);
        }
        if self.ty() == DOUBLESTAR {
            ch.push(self.bump());
            ch.push(self.factor()?);
        }
        Ok(self.make(POWER, ch))
    }

    fn atom(&mut self) -> PResult {
        let t = self.tok();
        match t.ty {
            LPAR => {
                let l = self.bump();
                if self.ty() == RPAR {
                    let r = self.bump();
                    return Ok(self.tree.node(ATOM, vec![l, r]));
                }
                let inner = if self.is_kw("yield") {
                    self.yield_expr()?
                } else {
                    self.testlist_gexp(TESTLIST_GEXP)?
                };
                let r = self.expect(RPAR)?;
                Ok(self.tree.node(ATOM, vec![l, inner, r]))
            }
            LSQB => {
                let l = self.bump();
                if self.ty() == RSQB {
                    let r = self.bump();
                    return Ok(self.tree.node(ATOM, vec![l, r]));
                }
                let inner = self.testlist_gexp(LISTMAKER)?;
                let r = self.expect(RSQB)?;
                Ok(self.tree.node(ATOM, vec![l, inner, r]))
            }
            LBRACE => {
                let l = self.bump();
                if self.ty() == RBRACE {
                    let r = self.bump();
                    return Ok(self.tree.node(ATOM, vec![l, r]));
                }
                let inner = self.dictsetmaker()?;
                let r = self.expect(RBRACE)?;
                Ok(self.tree.node(ATOM, vec![l, inner, r]))
            }
            NUMBER => Ok(self.bump()),
            NAME if !is_hard_keyword(&t.value) => Ok(self.bump()),
            STRING => {
                let mut ch = vec![self.bump()];
                while self.ty() == STRING {
                    ch.push(self.bump());
                }
                Ok(self.make(ATOM, ch))
            }
            DOT => {
                let a = self.bump();
                let b = self.expect(DOT)?;
                let c = self.expect(DOT)?;
                Ok(self.tree.node(ATOM, vec![a, b, c]))
            }
            _ => self.err("expected an expression"),
        }
    }

    /// `testlist_gexp` / `listmaker`.
    fn testlist_gexp(&mut self, sym: u16) -> PResult {
        let mut ch = vec![self.namedexpr_or_star()?];
        if self.at_comp_for() {
            ch.push(self.old_comp_for()?);
        } else {
            while self.ty() == COMMA {
                ch.push(self.bump());
                if !(self.starts_test() || self.ty() == STAR) {
                    break;
                }
                ch.push(self.namedexpr_or_star()?);
            }
        }
        Ok(self.make(sym, ch))
    }

    fn trailer(&mut self) -> PResult {
        match self.ty() {
            LPAR => {
                let l = self.bump();
                if self.ty() == RPAR {
                    let r = self.bump();
                    return Ok(self.tree.node(TRAILER, vec![l, r]));
                }
                let a = self.arglist()?;
                let r = self.expect(RPAR)?;
                Ok(self.tree.node(TRAILER, vec![l, a, r]))
            }
            LSQB => {
                let l = self.bump();
                let s = self.subscriptlist()?;
                let r = self.expect(RSQB)?;
                Ok(self.tree.node(TRAILER, vec![l, s, r]))
            }
            _ => {
                let d = self.expect(DOT)?;
                let n = self.expect_name()?;
                Ok(self.tree.node(TRAILER, vec![d, n]))
            }
        }
    }

    fn subscriptlist(&mut self) -> PResult {
        let mut ch = vec![self.subscript_or_star()?];
        while self.ty() == COMMA {
            ch.push(self.bump());
            if !(self.starts_test() || matches!(self.ty(), STAR | COLON)) {
                break;
            }
            ch.push(self.subscript_or_star()?);
        }
        Ok(self.make(SUBSCRIPTLIST, ch))
    }

    fn subscript_or_star(&mut self) -> PResult {
        if self.ty() == STAR {
            return self.star_expr();
        }
        let mut ch = Vec::new();
        if self.ty() != COLON {
            let t = self.test()?;
            if self.ty() == COLONEQUAL {
                let op = self.bump();
                let v = self.test()?;
                return Ok(self.tree.node(SUBSCRIPT, vec![t, op, v]));
            }
            if self.ty() != COLON {
                return Ok(t);
            }
            ch.push(t);
        }
        ch.push(self.bump());
        if self.starts_test() {
            ch.push(self.test()?);
        }
        if self.ty() == COLON {
            let mut s = vec![self.bump()];
            if self.starts_test() {
                s.push(self.test()?);
            }
            ch.push(self.make(SLICEOP, s));
        }
        Ok(self.make(SUBSCRIPT, ch))
    }

    fn arglist(&mut self) -> PResult {
        let mut ch = vec![self.argument()?];
        while self.ty() == COMMA {
            ch.push(self.bump());
            if !(self.starts_test() || matches!(self.ty(), STAR | DOUBLESTAR)) {
                break;
            }
            ch.push(self.argument()?);
        }
        Ok(self.make(ARGLIST, ch))
    }

    fn argument(&mut self) -> PResult {
        if matches!(self.ty(), STAR | DOUBLESTAR) {
            let s = self.bump();
            let t = self.test()?;
            return Ok(self.tree.node(ARGUMENT, vec![s, t]));
        }
        let t = self.test()?;
        if self.at_comp_for() {
            let c = self.comp_for()?;
            return Ok(self.tree.node(ARGUMENT, vec![t, c]));
        }
        match self.ty() {
            COLONEQUAL => {
                let op = self.bump();
                let v = self.test()?;
                let mut ch = vec![t, op, v];
                if self.at_comp_for() {
                    ch.push(self.comp_for()?);
                }
                Ok(self.tree.node(ARGUMENT, ch))
            }
            EQUAL => {
                let op = self.bump();
                let v = self.asexpr_test()?;
                Ok(self.tree.node(ARGUMENT, vec![t, op, v]))
            }
            NAME if self.tok().value == "as" => {
                let op = self.bump();
                let v = self.test()?;
                Ok(self.tree.node(ARGUMENT, vec![t, op, v]))
            }
            _ => Ok(t),
        }
    }

    fn comp_for_common(&mut self, old: bool) -> PResult {
        let mut ch = Vec::new();
        if self.ty() == ASYNC {
            ch.push(self.bump());
        }
        ch.push(self.expect_kw("for")?);
        ch.push(self.exprlist()?);
        ch.push(self.expect_kw("in")?);
        if old {
            // testlist_safe: old_test [(',' old_test)+ [',']]
            let first = self.old_test()?;
            let mut items = vec![first];
            if self.ty() == COMMA && {
                let save = self.pos;
                self.pos += 1;
                let ok = self.starts_test();
                self.pos = save;
                ok
            } {
                while self.ty() == COMMA {
                    items.push(self.bump());
                    if !self.starts_test() {
                        break;
                    }
                    items.push(self.old_test()?);
                }
            }
            ch.push(self.make(TESTLIST_SAFE, items));
        } else {
            ch.push(self.or_test()?);
        }
        if self.at_comp_for() {
            ch.push(self.comp_for_common(old)?);
        } else if self.is_kw("if") {
            ch.push(self.comp_if(old)?);
        }
        Ok(self
            .tree
            .node(if old { OLD_COMP_FOR } else { COMP_FOR }, ch))
    }

    fn comp_for(&mut self) -> PResult {
        self.comp_for_common(false)
    }

    fn old_comp_for(&mut self) -> PResult {
        self.comp_for_common(true)
    }

    fn comp_if(&mut self, old: bool) -> PResult {
        let mut ch = vec![self.bump(), self.old_test()?];
        if self.at_comp_for() {
            ch.push(self.comp_for_common(old)?);
        } else if self.is_kw("if") {
            ch.push(self.comp_if(old)?);
        }
        Ok(self.tree.node(if old { OLD_COMP_IF } else { COMP_IF }, ch))
    }

    fn dictsetmaker(&mut self) -> PResult {
        let mut ch = Vec::new();
        let is_dict;
        if self.ty() == DOUBLESTAR {
            ch.push(self.bump());
            ch.push(self.expr()?);
            is_dict = true;
        } else if self.ty() == STAR {
            ch.push(self.star_expr()?);
            is_dict = false;
        } else {
            let t = self.test()?;
            ch.push(t);
            if self.ty() == COLON {
                ch.push(self.bump());
                ch.push(self.asexpr_test()?);
                is_dict = true;
            } else {
                if self.ty() == COLONEQUAL {
                    ch.push(self.bump());
                    ch.push(self.test()?);
                }
                is_dict = false;
            }
        }
        if self.at_comp_for() {
            ch.push(self.comp_for()?);
        } else {
            while self.ty() == COMMA {
                ch.push(self.bump());
                if is_dict {
                    if self.ty() == DOUBLESTAR {
                        ch.push(self.bump());
                        ch.push(self.expr()?);
                    } else if self.starts_test() {
                        ch.push(self.test()?);
                        ch.push(self.expect(COLON)?);
                        ch.push(self.asexpr_test()?);
                    } else {
                        break;
                    }
                } else if self.ty() == STAR {
                    ch.push(self.star_expr()?);
                } else if self.starts_test() {
                    ch.push(self.test()?);
                    if self.ty() == COLONEQUAL {
                        ch.push(self.bump());
                        ch.push(self.test()?);
                    }
                } else {
                    break;
                }
            }
        }
        Ok(self.make(DICTSETMAKER, ch))
    }
}
