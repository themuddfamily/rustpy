//! Lexical context of the completion caret (roadmap §2.4 "Completion UI").
//!
//! Code completion only makes sense in code: inside comments, string and
//! docstring text, numeric literals and at positions where a new name is being
//! introduced (`def name`, `class Name`, `... as name`) the lookup must stay
//! closed, otherwise Enter/Tab accept a builtin instead of ending the line.
//! After `def` in a class body the lookup offers the base-class methods that
//! can be overridden, inserted with their parameter list.

use std::collections::HashSet;
use std::sync::Arc;

use rustpy_core::TextSize;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode, Token};
use smol_str::SmolStr;

use crate::classes::MemberKind;
use crate::db::{Analysis, FileInfo};
use crate::features::{CompletionItem, CompletionKind};
use crate::infer::Inferer;
use crate::types::ClassRef;

/// Where the caret is, as far as code completion is concerned.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CompletionContext {
    /// Python code, including f-string replacement fields (`f"{na|}"`).
    Code,
    /// A `#` comment.
    Comment,
    /// Literal text of a string, bytes, docstring or f-string (also format specs).
    StringLiteral,
    /// The name being defined after `def`, `class` or a `type` alias statement.
    DefinitionName,
    /// The new name after `as` (imports, `with`, `except`, `case` patterns).
    AliasName,
    /// Inside a numeric literal (`1e5`, `0x1f`, `1.`).
    Number,
}

impl CompletionContext {
    /// Whether names, keywords, live templates and postfix templates apply.
    pub fn is_code(self) -> bool {
        self == CompletionContext::Code
    }

    /// Whether the lookup may open by itself while typing: in code, and after
    /// `def` (where only overridable base-class methods are offered).
    pub fn allows_auto_popup(self) -> bool {
        matches!(
            self,
            CompletionContext::Code | CompletionContext::DefinitionName
        )
    }
}

/// Classifies the caret position `offset` in `text` (the current editor text).
pub fn completion_context(text: &str, offset: TextSize) -> CompletionContext {
    use SyntaxKind::*;
    let mut end = usize::from(offset).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return CompletionContext::Code;
    }
    // Lexing only the text before the caret is enough: a literal or comment
    // that continues past the caret shows up as an unterminated one.
    let (tokens, diags) = rustpy_syntax::tokenize(&text[..end]);
    let Some(idx) = tokens.iter().rposition(|t| !t.range.is_empty()) else {
        return CompletionContext::Code;
    };
    let tok = tokens[idx];
    if usize::from(tok.range.end()) != end {
        return CompletionContext::Code;
    }
    match tok.kind {
        COMMENT => CompletionContext::Comment,
        STRING => {
            let unterminated = diags.iter().any(|d| {
                d.range.start() == tok.range.start() && d.message.starts_with("unterminated")
            });
            // A complete literal that ends at the caret: the caret is after the closing quote.
            if unterminated {
                CompletionContext::StringLiteral
            } else {
                CompletionContext::Code
            }
        }
        // `f"…` (caret right after the opening quote) and literal text / format specs.
        FSTRING_START | FSTRING_MIDDLE | TSTRING_START | TSTRING_MIDDLE => {
            CompletionContext::StringLiteral
        }
        INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER => CompletionContext::Number,
        _ => name_position(text, &tokens, idx),
    }
}

/// `Code`, or the new-name contexts after `def` / `class` / `as` / `type`.
fn name_position(text: &str, tokens: &[Token], idx: usize) -> CompletionContext {
    use SyntaxKind::*;
    let tok = tokens[idx];
    let mut j = idx;
    // Step over the word being typed, then the spaces before it.
    if tok.kind == IDENT || tok.kind.is_keyword() {
        let Some(prev) = j.checked_sub(1) else {
            return CompletionContext::Code;
        };
        j = prev;
        // Letters glued to a number are part of a literal being typed (`1e`, `2j`).
        if matches!(tokens[j].kind, INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER)
            && tokens[j].range.end() == tok.range.start()
        {
            return CompletionContext::Number;
        }
    }
    while tokens[j].kind == WHITESPACE {
        let Some(prev) = j.checked_sub(1) else {
            return CompletionContext::Code;
        };
        j = prev;
    }
    let anchor = tokens[j];
    match anchor.kind {
        DEF_KW | CLASS_KW => CompletionContext::DefinitionName,
        AS_KW => CompletionContext::AliasName,
        // PEP 695 `type Alias = ...` (`type` is a soft keyword, lexed as a name).
        IDENT if &text[anchor.range] == "type" && at_statement_start(tokens, j) => {
            CompletionContext::DefinitionName
        }
        _ => CompletionContext::Code,
    }
}

fn at_statement_start(tokens: &[Token], idx: usize) -> bool {
    use SyntaxKind::*;
    let mut j = idx;
    loop {
        let Some(prev) = j.checked_sub(1) else {
            return true;
        };
        j = prev;
        match tokens[j].kind {
            WHITESPACE => continue,
            NEWLINE | NL | INDENT | DEDENT | SEMICOLON | COLON | COMMENT => return true,
            _ => return false,
        }
    }
}

/// `(byte offset of the identifier being typed, its text)`.
fn typed_prefix(text: &str, offset: TextSize) -> (usize, &str) {
    let off = usize::from(offset).min(text.len());
    let before = &text[..off];
    let start = before
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map(|i| {
            i + before[i..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1)
        })
        .unwrap_or(0);
    (start, &before[start..])
}

/// The class whose body directly contains the `def` keyword token `def_kw`.
fn class_of_def(def_kw: &rustpy_syntax::SyntaxToken) -> Option<ast::ClassDef> {
    for anc in def_kw.parent_ancestors() {
        match anc.kind() {
            // The (possibly incomplete) definition being typed.
            SyntaxKind::FUNC_DEF
                if anc
                    .children_with_tokens()
                    .any(|e| e.as_token() == Some(def_kw)) =>
            {
                continue
            }
            SyntaxKind::CLASS_DEF => return ast::ClassDef::cast(anc),
            SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR | SyntaxKind::MODULE => return None,
            _ => continue,
        }
    }
    None
}

/// Parameter list of `f` as source text without annotations: names,
/// defaults and the `/` and `*` markers (`self, key, default=None, *, strict`).
pub fn override_params_text(f: &ast::FuncDef) -> String {
    let Some(list) = f.params() else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for p in list.params() {
        let part = match &p {
            ast::AnyParam::Slash(_) => "/".to_string(),
            ast::AnyParam::StarSep(_) => "*".to_string(),
            ast::AnyParam::Star(s) => format!(
                "*{}",
                s.name().map(|t| t.text().to_string()).unwrap_or_default()
            ),
            ast::AnyParam::StarStar(s) => format!(
                "**{}",
                s.name().map(|t| t.text().to_string()).unwrap_or_default()
            ),
            ast::AnyParam::Plain(plain) => {
                let mut s = plain.name_text();
                if let Some(d) = plain.default() {
                    s.push('=');
                    s.push_str(&d.syntax().text().to_string());
                }
                s
            }
        };
        parts.push(part);
    }
    parts.join(", ")
}

fn func_def_named(file: &FileInfo, name_range: rustpy_core::TextRange) -> Option<ast::FuncDef> {
    let root: SyntaxNode = file.parse.syntax_node();
    let tok = root.token_at_offset(name_range.start()).right_biased()?;
    tok.parent_ancestors()
        .find_map(ast::FuncDef::cast)
        .filter(|f| f.name().is_some_and(|n| n.text_range() == name_range))
}

/// `def <caret>` inside a class body: the base-class methods not yet defined
/// in the class, inserted as `name(params):`.
pub fn override_completions(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Vec<CompletionItem> {
    let offset = crate::clamp_offset(file, offset);
    let text: &str = &file.text;
    let (prefix_start, prefix) = typed_prefix(text, offset);
    let root = file.parse.syntax_node();
    let anchor = TextSize::new(prefix_start as u32);
    let Some(mut kw) = root.token_at_offset(anchor).left_biased() else {
        return Vec::new();
    };
    while kw.kind() == SyntaxKind::WHITESPACE
        || (kw.kind() == SyntaxKind::IDENT && kw.text_range().start() >= anchor)
    {
        let Some(prev) = kw.prev_token() else {
            return Vec::new();
        };
        kw = prev;
    }
    if kw.kind() != SyntaxKind::DEF_KW {
        return Vec::new();
    }
    let Some(class) = class_of_def(&kw) else {
        return Vec::new();
    };
    let Some(name) = class.name() else {
        return Vec::new();
    };
    let cref = ClassRef {
        file: file.id,
        range: class.syntax().text_range(),
        name: SmolStr::new(name.text()),
    };
    let inf = Inferer::new(db);
    let Some(info) = inf.class_info(&cref) else {
        return Vec::new();
    };
    // Methods the class already has (except the name being typed right now).
    let own: HashSet<SmolStr> = info
        .members
        .values()
        .filter(|m| m.range.start() != anchor)
        .map(|m| m.name.clone())
        .collect();
    let mut seen: HashSet<SmolStr> = HashSet::new();
    let mut items = Vec::new();
    for base in inf.mro(&cref).iter().skip(1) {
        let (Some(binfo), Some(bfile)) = (inf.class_info(base), db.file(base.file)) else {
            continue;
        };
        for m in binfo.members.values() {
            let overridable = match m.kind {
                MemberKind::Method | MemberKind::ClassMethod | MemberKind::StaticMethod => true,
                MemberKind::Property => !m.name.starts_with("__"),
                _ => false,
            };
            let mangled = m.name.starts_with("__") && !m.name.ends_with("__");
            if !overridable || mangled || own.contains(&m.name) || !seen.insert(m.name.clone()) {
                continue;
            }
            let Some(func) = func_def_named(&bfile, m.range) else {
                continue;
            };
            let insert = format!("{}({}):", m.name, override_params_text(&func));
            items.push(CompletionItem {
                label: m.name.clone(),
                kind: CompletionKind::Method,
                detail: base.name.to_string(),
                insert,
                caret_offset: None,
                priority: if base.name == "object" { 20 } else { 10 },
            });
        }
    }
    crate::features::finish(items, prefix)
}

/// Text to insert for `item` and the caret offset inside it. The `()` that
/// completion appends to function and method names is dropped when
/// "Insert parentheses when completing callables" is off, or when the text
/// after the caret already starts with `(` (the caret then ends up before it).
pub fn insert_text_for(
    item: &CompletionItem,
    insert_parentheses: bool,
    next_char: Option<char>,
) -> (String, Option<usize>) {
    let appended_parens = item
        .insert
        .strip_suffix("()")
        .is_some_and(|name| name == item.label.as_str());
    // With `(` already after the caret only the name goes in: also for an
    // override's `name(params):` when renaming an existing `def sa|(self):`.
    let has_arguments = item
        .insert
        .strip_prefix(item.label.as_str())
        .is_some_and(|rest| rest.starts_with('('));
    if (appended_parens && !insert_parentheses) || (has_arguments && next_char == Some('(')) {
        return (item.label.to_string(), None);
    }
    (item.insert.clone(), item.caret_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use CompletionContext::*;

    #[test]
    fn override_completion_before_existing_parentheses_inserts_the_name() {
        let item = CompletionItem {
            label: "save".into(),
            kind: CompletionKind::Method,
            detail: "Model".into(),
            insert: "save(self, commit=True):".into(),
            caret_offset: None,
            priority: 10,
        };
        assert_eq!(insert_text_for(&item, true, Some('(')).0, "save");
        assert_eq!(
            insert_text_for(&item, true, None).0,
            "save(self, commit=True):"
        );
        let call = CompletionItem {
            label: "run".into(),
            kind: CompletionKind::Function,
            detail: String::new(),
            insert: "run()".into(),
            caret_offset: Some(4),
            priority: 0,
        };
        assert_eq!(
            insert_text_for(&call, true, Some('(')),
            ("run".into(), None)
        );
        assert_eq!(insert_text_for(&call, false, None), ("run".into(), None));
        assert_eq!(
            insert_text_for(&call, true, None),
            ("run()".into(), Some(4))
        );
    }

    /// Context at the `|` marker.
    fn ctx(marked: &str) -> CompletionContext {
        let at = marked.find('|').expect("caret marker");
        let text = marked.replacen('|', "", 1);
        completion_context(&text, TextSize::new(at as u32))
    }

    #[test]
    fn comments_strings_and_numbers_are_not_code() {
        assert_eq!(ctx("x = 1  # the value|\n"), Comment);
        assert_eq!(ctx("# print the list|"), Comment);
        assert_eq!(ctx("#|"), Comment);
        assert_eq!(ctx("s = 'some value|'\n"), StringLiteral);
        assert_eq!(ctx("s = \"unterminated pri|\n"), StringLiteral);
        assert_eq!(
            ctx("def f():\n    \"\"\"Return the list|\n    of values.\"\"\"\n"),
            StringLiteral
        );
        assert_eq!(ctx("b = b'raw byt|'"), StringLiteral);
        assert_eq!(ctx("f\"hello wor|ld {name}\""), StringLiteral);
        assert_eq!(ctx("f\"{name} and val|\""), StringLiteral);
        assert_eq!(ctx("f\"{x:%Y-%m|}\""), StringLiteral);
        assert_eq!(ctx("f\"|\""), StringLiteral);
        assert_eq!(ctx("x = 1e|5"), Number);
        assert_eq!(ctx("x = 0x1f|"), Number);
        assert_eq!(ctx("x = 1.|"), Number);
    }

    #[test]
    fn code_positions() {
        assert_eq!(ctx("pri|"), Code);
        assert_eq!(ctx("|"), Code);
        assert_eq!(ctx("x = 'abc'|"), Code);
        assert_eq!(ctx("x = 'abc'.up|"), Code);
        assert_eq!(ctx("x = obj.|"), Code);
        assert_eq!(ctx("f\"{na|}\""), Code);
        assert_eq!(ctx("f\"text {obj.val|} more\""), Code);
        assert_eq!(ctx("x = foo(1).|"), Code);
        assert_eq!(ctx("import js|"), Code);
        assert_eq!(ctx("from os import pa|"), Code);
        assert_eq!(ctx("class A(Ba|"), Code);
        assert_eq!(ctx("def|"), Code);
        assert_eq!(ctx("de|"), Code);
        assert_eq!(ctx("x = type|"), Code);
        assert_eq!(ctx("x = type(o|"), Code);
        assert_eq!(ctx("value1 = 3\nvalue1.|"), Code);
        assert_eq!(ctx("# comment\nva|"), Code);
        assert_eq!(ctx("s = '''doc'''\nva|"), Code);
    }

    #[test]
    fn new_name_positions() {
        assert_eq!(ctx("def val|"), DefinitionName);
        assert_eq!(ctx("def |"), DefinitionName);
        assert_eq!(ctx("async def fetch_li|"), DefinitionName);
        assert_eq!(ctx("class Valu|"), DefinitionName);
        assert_eq!(ctx("class A:\n    def __in|"), DefinitionName);
        assert_eq!(ctx("type Lis|"), DefinitionName);
        assert_eq!(ctx("import numpy as n|"), AliasName);
        assert_eq!(ctx("from os import path as p|"), AliasName);
        assert_eq!(ctx("with open('f') as fi|"), AliasName);
        assert_eq!(ctx("try:\n    pass\nexcept ValueError as er|"), AliasName);
        assert!(!AliasName.allows_auto_popup());
        assert!(DefinitionName.allows_auto_popup());
        assert!(!Comment.is_code() && Code.is_code());
    }

    fn setup(src: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        (db, f)
    }

    #[test]
    fn def_in_class_body_offers_base_methods() {
        let src = "class Base:\n    def save(self, commit=True, *, using=None):\n        pass\n\n    def _hook(self, x, /):\n        pass\n\n    def __private(self):\n        pass\n\n\nclass Child(Base):\n    def _hook(self, x, /):\n        pass\n\n    def sa\n";
        let (db, f) = setup(src);
        let items = override_completions(
            &db,
            &f,
            TextSize::new(src.rfind("sa\n").unwrap() as u32 + 2),
        );
        let save = items
            .iter()
            .find(|i| i.label == "save")
            .expect("save offered");
        assert_eq!(save.insert, "save(self, commit=True, *, using=None):");
        assert_eq!(save.detail, "Base");
        assert_eq!(save.kind, CompletionKind::Method);
        // Already overridden and name-mangled methods are not offered.
        assert!(!items
            .iter()
            .any(|i| i.label == "_hook" || i.label == "__private"));
        // `object` dunders are offered with their stub parameters.
        let src2 = src.replace("    def sa\n", "    def __in\n");
        let (db, f) = setup(&src2);
        let items = override_completions(
            &db,
            &f,
            TextSize::new(src2.rfind("__in\n").unwrap() as u32 + 4),
        );
        let init = items
            .iter()
            .find(|i| i.label == "__init__")
            .expect("__init__ offered");
        assert_eq!(init.insert, "__init__(self):");
        assert_eq!(init.detail, "object");
    }

    #[test]
    fn parentheses_follow_the_setting_and_the_next_character() {
        let item = |label: &str, insert: &str, caret: Option<usize>| CompletionItem {
            label: label.into(),
            kind: CompletionKind::Function,
            detail: String::new(),
            insert: insert.into(),
            caret_offset: caret,
            priority: 0,
        };
        let print = item("print", "print()", Some(6));
        assert_eq!(
            insert_text_for(&print, true, None),
            ("print()".to_string(), Some(6))
        );
        assert_eq!(
            insert_text_for(&print, true, Some(' ')),
            ("print()".to_string(), Some(6))
        );
        assert_eq!(
            insert_text_for(&print, false, None),
            ("print".to_string(), None)
        );
        assert_eq!(
            insert_text_for(&print, true, Some('(')),
            ("print".to_string(), None)
        );
        // Items whose parentheses are part of the label (smart `Point()`) or of
        // an override signature keep their text.
        let ctor = item("Point()", "Point()", Some(6));
        assert_eq!(
            insert_text_for(&ctor, false, Some('(')),
            ("Point()".to_string(), Some(6))
        );
        let over = item("save", "save(self):", None);
        assert_eq!(
            insert_text_for(&over, false, None),
            ("save(self):".to_string(), None)
        );
        let var = item("value", "value", None);
        assert_eq!(
            insert_text_for(&var, false, Some('(')),
            ("value".to_string(), None)
        );
    }

    #[test]
    fn completion_is_empty_in_comments_strings_and_new_names() {
        let cases = [
            "value = 1\nx = 2  # va",
            "value = 1\ns = 'va",
            "value = 1\ndef f():\n    \"\"\"Return va",
            "value = 1\nimport os as va",
            "value = 1\ndef va",
            "value = 1\nclass Va",
            "value = 1\nn = 1e5",
        ];
        for src in cases {
            let (db, f) = setup(src);
            let off = TextSize::new(src.len() as u32);
            assert!(
                crate::features::complete(&db, &f, off).is_empty(),
                "{src:?}"
            );
            assert!(
                crate::completion2::smart_complete(&db, &f, off).is_empty(),
                "{src:?}"
            );
            assert!(
                crate::completion2::second_press_complete(&db, &f, off, &[]).is_empty(),
                "{src:?}"
            );
        }
        // Code positions (also inside f-string replacement fields) still complete.
        for src in ["value = 1\nx = va", "value = 1\ns = f\"{va"] {
            let (db, f) = setup(src);
            let items = crate::features::complete(&db, &f, TextSize::new(src.len() as u32));
            assert!(items.iter().any(|i| i.label == "value"), "{src:?}");
        }
    }

    #[test]
    fn def_outside_class_and_class_names_offer_nothing() {
        let src = "def sa\n";
        let (db, f) = setup(src);
        assert!(override_completions(&db, &f, TextSize::new(6)).is_empty());
        let src2 = "class Base:\n    def save(self): ...\n\nclass Child(Base):\n    def method(self):\n        def sa\n";
        let (db, f) = setup(src2);
        assert!(override_completions(&db, &f, TextSize::new(src2.len() as u32 - 1)).is_empty());
    }
}
