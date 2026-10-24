//! Token and node kinds of the Python syntax tree (roadmap §2.1).
//!
//! Tokens come first, then composite node kinds. The numeric values are only
//! used to bridge to `rowan`; never persist them.

#![allow(non_camel_case_types, clippy::upper_case_acronyms)]

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum SyntaxKind {
    // ---- trivia --------------------------------------------------------
    /// Spaces, tabs, form feeds and backslash line continuations.
    WHITESPACE,
    COMMENT,
    /// A newline that does not end a logical line (blank line, inside brackets).
    NL,
    // ---- structural tokens ---------------------------------------------
    /// End of a logical line.
    NEWLINE,
    /// Increase of indentation (token text is the indentation whitespace).
    INDENT,
    /// Decrease of indentation (zero-width).
    DEDENT,
    /// Unknown or invalid character(s).
    ERROR_TOKEN,
    EOF,
    // ---- literals & names ----------------------------------------------
    IDENT,
    INT_NUMBER,
    FLOAT_NUMBER,
    COMPLEX_NUMBER,
    /// A complete non-f string or bytes literal, including prefix and quotes.
    STRING,
    FSTRING_START,
    FSTRING_MIDDLE,
    FSTRING_END,
    TSTRING_START,
    TSTRING_MIDDLE,
    TSTRING_END,
    // ---- keywords ------------------------------------------------------
    FALSE_KW,
    NONE_KW,
    TRUE_KW,
    AND_KW,
    AS_KW,
    ASSERT_KW,
    ASYNC_KW,
    AWAIT_KW,
    BREAK_KW,
    CLASS_KW,
    CONTINUE_KW,
    DEF_KW,
    DEL_KW,
    ELIF_KW,
    ELSE_KW,
    EXCEPT_KW,
    FINALLY_KW,
    FOR_KW,
    FROM_KW,
    GLOBAL_KW,
    IF_KW,
    IMPORT_KW,
    IN_KW,
    IS_KW,
    LAMBDA_KW,
    NONLOCAL_KW,
    NOT_KW,
    OR_KW,
    PASS_KW,
    RAISE_KW,
    RETURN_KW,
    TRY_KW,
    WHILE_KW,
    WITH_KW,
    YIELD_KW,
    // soft keywords are lexed as IDENT and re-tagged by the parser
    MATCH_KW,
    CASE_KW,
    TYPE_KW,
    UNDERSCORE_KW,
    // ---- punctuation ---------------------------------------------------
    L_PAREN,
    R_PAREN,
    L_BRACK,
    R_BRACK,
    L_BRACE,
    R_BRACE,
    COMMA,
    COLON,
    DOT,
    SEMICOLON,
    AT,
    EQ,
    ARROW,
    ELLIPSIS,
    PLUS_EQ,
    MINUS_EQ,
    STAR_EQ,
    SLASH_EQ,
    SLASH_SLASH_EQ,
    PERCENT_EQ,
    AT_EQ,
    AMP_EQ,
    PIPE_EQ,
    CARET_EQ,
    SHR_EQ,
    SHL_EQ,
    STAR_STAR_EQ,
    PLUS,
    MINUS,
    STAR,
    STAR_STAR,
    SLASH,
    SLASH_SLASH,
    PERCENT,
    SHL,
    SHR,
    AMP,
    PIPE,
    CARET,
    TILDE,
    COLON_EQ,
    LT,
    GT,
    LT_EQ,
    GT_EQ,
    EQ_EQ,
    NOT_EQ,
    /// `!` — only valid as an f-string conversion marker.
    BANG,
    // ---- nodes: module & statements -------------------------------------
    MODULE,
    /// A sequence of statements (an indented block or the body of a module).
    BLOCK,
    EXPR_STMT,
    ASSIGN_STMT,
    AUG_ASSIGN_STMT,
    ANN_ASSIGN_STMT,
    RETURN_STMT,
    PASS_STMT,
    BREAK_STMT,
    CONTINUE_STMT,
    DEL_STMT,
    ASSERT_STMT,
    GLOBAL_STMT,
    NONLOCAL_STMT,
    RAISE_STMT,
    IMPORT_STMT,
    IMPORT_FROM_STMT,
    IMPORT_ALIAS,
    DOTTED_NAME,
    RELATIVE_LEVEL,
    IF_STMT,
    ELIF_CLAUSE,
    ELSE_CLAUSE,
    WHILE_STMT,
    FOR_STMT,
    TRY_STMT,
    EXCEPT_CLAUSE,
    FINALLY_CLAUSE,
    WITH_STMT,
    WITH_ITEM,
    FUNC_DEF,
    CLASS_DEF,
    DECORATOR,
    DECORATORS,
    PARAM_LIST,
    PARAM,
    STAR_PARAM,
    STAR_STAR_PARAM,
    SLASH_SEPARATOR,
    STAR_SEPARATOR,
    DEFAULT_VALUE,
    ANNOTATION,
    RETURN_ANNOTATION,
    TYPE_PARAM_LIST,
    TYPE_PARAM,
    TYPE_VAR_TUPLE_PARAM,
    PARAM_SPEC_PARAM,
    TYPE_ALIAS_STMT,
    MATCH_STMT,
    CASE_CLAUSE,
    GUARD,
    ARG_LIST,
    ARG,
    KEYWORD_ARG,
    STAR_ARG,
    STAR_STAR_ARG,
    // ---- nodes: expressions --------------------------------------------
    NAME_EXPR,
    LITERAL_EXPR,
    STRING_EXPR,
    FSTRING_EXPR,
    FSTRING_REPLACEMENT,
    FSTRING_FORMAT_SPEC,
    FSTRING_CONVERSION,
    /// Implicitly concatenated string literals.
    CONCAT_STRING_EXPR,
    PAREN_EXPR,
    TUPLE_EXPR,
    LIST_EXPR,
    SET_EXPR,
    DICT_EXPR,
    DICT_ITEM,
    DICT_UNPACK,
    LIST_COMP,
    SET_COMP,
    DICT_COMP,
    GENERATOR_EXPR,
    COMP_FOR,
    COMP_IF,
    ATTRIBUTE_EXPR,
    SUBSCRIPT_EXPR,
    SLICE_EXPR,
    SLICE_LIST,
    CALL_EXPR,
    UNARY_EXPR,
    BINARY_EXPR,
    BOOL_OP_EXPR,
    COMPARE_EXPR,
    COMPARE_OP,
    NOT_EXPR,
    LAMBDA_EXPR,
    CONDITIONAL_EXPR,
    NAMED_EXPR,
    STAR_EXPR,
    DOUBLE_STAR_EXPR,
    AWAIT_EXPR,
    YIELD_EXPR,
    YIELD_FROM_EXPR,
    // ---- nodes: patterns -----------------------------------------------
    MATCH_VALUE_PATTERN,
    MATCH_SINGLETON_PATTERN,
    MATCH_CAPTURE_PATTERN,
    MATCH_WILDCARD_PATTERN,
    MATCH_SEQUENCE_PATTERN,
    MATCH_MAPPING_PATTERN,
    MATCH_MAPPING_ITEM,
    MATCH_MAPPING_REST,
    MATCH_CLASS_PATTERN,
    MATCH_CLASS_ARG,
    MATCH_KEYWORD_PATTERN,
    MATCH_STAR_PATTERN,
    MATCH_OR_PATTERN,
    MATCH_AS_PATTERN,
    MATCH_GROUP_PATTERN,
    // ---- error node ----------------------------------------------------
    ERROR,
    #[doc(hidden)]
    __LAST,
}

use SyntaxKind::*;

impl SyntaxKind {
    #[inline]
    pub fn from_raw(raw: u16) -> SyntaxKind {
        assert!(raw < __LAST as u16);
        // SAFETY: repr(u16) enum with contiguous discriminants below __LAST.
        unsafe { std::mem::transmute(raw) }
    }

    pub fn is_trivia(self) -> bool {
        matches!(self, WHITESPACE | COMMENT | NL)
    }

    pub fn is_keyword(self) -> bool {
        (self >= FALSE_KW && self <= YIELD_KW)
            || matches!(self, MATCH_KW | CASE_KW | TYPE_KW | UNDERSCORE_KW)
    }

    pub fn is_soft_keyword(self) -> bool {
        matches!(self, MATCH_KW | CASE_KW | TYPE_KW | UNDERSCORE_KW)
    }

    pub fn is_literal_token(self) -> bool {
        matches!(
            self,
            INT_NUMBER
                | FLOAT_NUMBER
                | COMPLEX_NUMBER
                | STRING
                | TRUE_KW
                | FALSE_KW
                | NONE_KW
                | ELLIPSIS
        )
    }

    /// Maps an identifier to its hard keyword kind, if any.
    pub fn from_keyword(ident: &str) -> Option<SyntaxKind> {
        Some(match ident {
            "False" => FALSE_KW,
            "None" => NONE_KW,
            "True" => TRUE_KW,
            "and" => AND_KW,
            "as" => AS_KW,
            "assert" => ASSERT_KW,
            "async" => ASYNC_KW,
            "await" => AWAIT_KW,
            "break" => BREAK_KW,
            "class" => CLASS_KW,
            "continue" => CONTINUE_KW,
            "def" => DEF_KW,
            "del" => DEL_KW,
            "elif" => ELIF_KW,
            "else" => ELSE_KW,
            "except" => EXCEPT_KW,
            "finally" => FINALLY_KW,
            "for" => FOR_KW,
            "from" => FROM_KW,
            "global" => GLOBAL_KW,
            "if" => IF_KW,
            "import" => IMPORT_KW,
            "in" => IN_KW,
            "is" => IS_KW,
            "lambda" => LAMBDA_KW,
            "nonlocal" => NONLOCAL_KW,
            "not" => NOT_KW,
            "or" => OR_KW,
            "pass" => PASS_KW,
            "raise" => RAISE_KW,
            "return" => RETURN_KW,
            "try" => TRY_KW,
            "while" => WHILE_KW,
            "with" => WITH_KW,
            "yield" => YIELD_KW,
            _ => return None,
        })
    }

    /// Maps an identifier to a soft keyword kind, if any.
    pub fn from_soft_keyword(ident: &str) -> Option<SyntaxKind> {
        Some(match ident {
            "match" => MATCH_KW,
            "case" => CASE_KW,
            "type" => TYPE_KW,
            "_" => UNDERSCORE_KW,
            _ => return None,
        })
    }

    /// Source text of punctuation and keyword tokens.
    pub fn text(self) -> Option<&'static str> {
        Some(match self {
            L_PAREN => "(",
            R_PAREN => ")",
            L_BRACK => "[",
            R_BRACK => "]",
            L_BRACE => "{",
            R_BRACE => "}",
            COMMA => ",",
            COLON => ":",
            DOT => ".",
            SEMICOLON => ";",
            AT => "@",
            EQ => "=",
            ARROW => "->",
            ELLIPSIS => "...",
            PLUS_EQ => "+=",
            MINUS_EQ => "-=",
            STAR_EQ => "*=",
            SLASH_EQ => "/=",
            SLASH_SLASH_EQ => "//=",
            PERCENT_EQ => "%=",
            AT_EQ => "@=",
            AMP_EQ => "&=",
            PIPE_EQ => "|=",
            CARET_EQ => "^=",
            SHR_EQ => ">>=",
            SHL_EQ => "<<=",
            STAR_STAR_EQ => "**=",
            PLUS => "+",
            MINUS => "-",
            STAR => "*",
            STAR_STAR => "**",
            SLASH => "/",
            SLASH_SLASH => "//",
            PERCENT => "%",
            SHL => "<<",
            SHR => ">>",
            AMP => "&",
            PIPE => "|",
            CARET => "^",
            TILDE => "~",
            COLON_EQ => ":=",
            LT => "<",
            GT => ">",
            LT_EQ => "<=",
            GT_EQ => ">=",
            EQ_EQ => "==",
            NOT_EQ => "!=",
            BANG => "!",
            FALSE_KW => "False",
            NONE_KW => "None",
            TRUE_KW => "True",
            AND_KW => "and",
            AS_KW => "as",
            ASSERT_KW => "assert",
            ASYNC_KW => "async",
            AWAIT_KW => "await",
            BREAK_KW => "break",
            CLASS_KW => "class",
            CONTINUE_KW => "continue",
            DEF_KW => "def",
            DEL_KW => "del",
            ELIF_KW => "elif",
            ELSE_KW => "else",
            EXCEPT_KW => "except",
            FINALLY_KW => "finally",
            FOR_KW => "for",
            FROM_KW => "from",
            GLOBAL_KW => "global",
            IF_KW => "if",
            IMPORT_KW => "import",
            IN_KW => "in",
            IS_KW => "is",
            LAMBDA_KW => "lambda",
            NONLOCAL_KW => "nonlocal",
            NOT_KW => "not",
            OR_KW => "or",
            PASS_KW => "pass",
            RAISE_KW => "raise",
            RETURN_KW => "return",
            TRY_KW => "try",
            WHILE_KW => "while",
            WITH_KW => "with",
            YIELD_KW => "yield",
            MATCH_KW => "match",
            CASE_KW => "case",
            TYPE_KW => "type",
            UNDERSCORE_KW => "_",
            _ => return None,
        })
    }

    pub fn is_augmented_assign_op(self) -> bool {
        self >= PLUS_EQ && self <= STAR_STAR_EQ
    }

    pub fn is_comparison_op(self) -> bool {
        matches!(
            self,
            LT | GT | LT_EQ | GT_EQ | EQ_EQ | NOT_EQ | IN_KW | NOT_KW | IS_KW
        )
    }

    pub fn is_statement(self) -> bool {
        self >= EXPR_STMT
            && self <= MATCH_STMT
            && !matches!(
                self,
                IMPORT_ALIAS
                    | DOTTED_NAME
                    | RELATIVE_LEVEL
                    | ELIF_CLAUSE
                    | ELSE_CLAUSE
                    | EXCEPT_CLAUSE
                    | FINALLY_CLAUSE
                    | WITH_ITEM
                    | DECORATOR
                    | DECORATORS
                    | PARAM_LIST
                    | PARAM
                    | STAR_PARAM
                    | STAR_STAR_PARAM
                    | SLASH_SEPARATOR
                    | STAR_SEPARATOR
                    | DEFAULT_VALUE
                    | ANNOTATION
                    | RETURN_ANNOTATION
                    | TYPE_PARAM_LIST
                    | TYPE_PARAM
                    | TYPE_VAR_TUPLE_PARAM
                    | PARAM_SPEC_PARAM
                    | CASE_CLAUSE
                    | GUARD
            )
    }

    pub fn is_expression(self) -> bool {
        self >= NAME_EXPR
            && self <= YIELD_FROM_EXPR
            && !matches!(
                self,
                DICT_ITEM
                    | DICT_UNPACK
                    | COMP_FOR
                    | COMP_IF
                    | SLICE_LIST
                    | COMPARE_OP
                    | FSTRING_REPLACEMENT
                    | FSTRING_FORMAT_SPEC
                    | FSTRING_CONVERSION
            )
    }

    pub fn is_pattern(self) -> bool {
        self >= MATCH_VALUE_PATTERN
            && self <= MATCH_GROUP_PATTERN
            && !matches!(
                self,
                MATCH_MAPPING_ITEM | MATCH_MAPPING_REST | MATCH_CLASS_ARG
            )
    }
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        rowan::SyntaxKind(kind as u16)
    }
}

/// Marker type tying `rowan` to our [`SyntaxKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PythonLanguage {}

impl rowan::Language for PythonLanguage {
    type Kind = SyntaxKind;
    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        SyntaxKind::from_raw(raw.0)
    }
    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub type SyntaxNode = rowan::SyntaxNode<PythonLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<PythonLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<PythonLanguage>;
pub type SyntaxNodeChildren = rowan::SyntaxNodeChildren<PythonLanguage>;
pub type SyntaxElementChildren = rowan::SyntaxElementChildren<PythonLanguage>;
