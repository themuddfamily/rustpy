use super::{parse, sexp};

fn check(src: &str, expected: &str) {
    let p = parse(src);
    let got = sexp(&p.syntax_node());
    assert_eq!(got, expected, "\nsource: {src:?}\nerrors: {:?}", p.errors());
    assert!(p.ok(), "unexpected errors for {src:?}: {:?}", p.errors());
}

fn errors(src: &str) -> Vec<String> {
    parse(src)
        .errors()
        .iter()
        .map(|e| e.message.clone())
        .collect()
}

#[test]
fn roundtrip_preserves_text() {
    for src in [
        "x = 1\n",
        "if a:\n    b\n",
        "def f(a, *, b=1, **kw) -> int:\n    '''doc'''\n    return a\n",
        "f'{x!r:>{w}}' 'y'\n",
        "class C(Base, metaclass=M):\n  pass\n",
        "a = (1,\n  2)  # c\n\n",
        "   \n",
        "x = \"unterminated\n",
        ")",
        "def f(\n",
        "match x:\n  case [1, *r]:\n    pass\n",
    ] {
        let p = std::panic::catch_unwind(|| parse(src))
            .unwrap_or_else(|_| panic!("parser panicked on {src:?}"));
        assert_eq!(
            p.syntax_node().text().to_string(),
            src,
            "roundtrip for {src:?}"
        );
    }
}

#[test]
fn simple_statements() {
    check(
        "x = 1\n",
        "(MODULE (ASSIGN_STMT (NAME_EXPR x) = (LITERAL_EXPR 1)))",
    );
    check(
        "a = b = c\n",
        "(MODULE (ASSIGN_STMT (NAME_EXPR a) = (NAME_EXPR b) = (NAME_EXPR c)))",
    );
    check("x: int = 1\n", "(MODULE (ANN_ASSIGN_STMT (NAME_EXPR x) : (ANNOTATION (NAME_EXPR int)) = (LITERAL_EXPR 1)))");
    check(
        "x += 1; y\n",
        "(MODULE (AUG_ASSIGN_STMT (NAME_EXPR x) += (LITERAL_EXPR 1)) ; (EXPR_STMT (NAME_EXPR y)))",
    );
    check(
        "pass\nbreak\ncontinue\n",
        "(MODULE (PASS_STMT pass) (BREAK_STMT break) (CONTINUE_STMT continue))",
    );
    check("return\n", "(MODULE (RETURN_STMT return))");
    check(
        "return a, b\n",
        "(MODULE (RETURN_STMT return (TUPLE_EXPR (NAME_EXPR a) , (NAME_EXPR b))))",
    );
    check(
        "raise E from e\n",
        "(MODULE (RAISE_STMT raise (NAME_EXPR E) from (NAME_EXPR e)))",
    );
    check("global a, b\n", "(MODULE (GLOBAL_STMT global a , b))");
    check("del x[0], y\n", "(MODULE (DEL_STMT del (TUPLE_EXPR (SUBSCRIPT_EXPR (NAME_EXPR x) [ (LITERAL_EXPR 0) ]) , (NAME_EXPR y))))");
    check(
        "assert x, 'm'\n",
        "(MODULE (ASSERT_STMT assert (NAME_EXPR x) , (STRING_EXPR 'm')))",
    );
    check(
        "x = yield\n",
        "(MODULE (ASSIGN_STMT (NAME_EXPR x) = (YIELD_EXPR yield)))",
    );
    check("type Alias[T] = list[T]\n", "(MODULE (TYPE_ALIAS_STMT type Alias (TYPE_PARAM_LIST [ (TYPE_PARAM T) ]) = (SUBSCRIPT_EXPR (NAME_EXPR list) [ (NAME_EXPR T) ])))");
    check(
        "type = 3\n",
        "(MODULE (ASSIGN_STMT (NAME_EXPR type) = (LITERAL_EXPR 3)))",
    );
}

#[test]
fn imports() {
    check("import a.b as c, d\n", "(MODULE (IMPORT_STMT import (IMPORT_ALIAS (DOTTED_NAME a . b) as c) , (IMPORT_ALIAS (DOTTED_NAME d))))");
    check(
        "from . import x\n",
        "(MODULE (IMPORT_FROM_STMT from (RELATIVE_LEVEL .) import (IMPORT_ALIAS x)))",
    );
    check("from ..pkg.mod import (a as b,\n c)\n", "(MODULE (IMPORT_FROM_STMT from (RELATIVE_LEVEL . .) (DOTTED_NAME pkg . mod) import ( (IMPORT_ALIAS a as b) , (IMPORT_ALIAS c) )))");
    check(
        "from m import *\n",
        "(MODULE (IMPORT_FROM_STMT from (DOTTED_NAME m) import *))",
    );
}

#[test]
fn definitions() {
    check(
        "@dec\n@a.b(1)\nasync def f(a, b: int = 2, /, c=3, *args: int, d, **kw) -> None:\n    pass\n",
        "(MODULE (FUNC_DEF (DECORATORS (DECORATOR @ (NAME_EXPR dec)) (DECORATOR @ (CALL_EXPR (ATTRIBUTE_EXPR (NAME_EXPR a) . b) (ARG_LIST ( (ARG (LITERAL_EXPR 1)) ))))) async def f (PARAM_LIST ( (PARAM a) , (PARAM b : (ANNOTATION (NAME_EXPR int)) = (DEFAULT_VALUE (LITERAL_EXPR 2))) , (SLASH_SEPARATOR /) , (PARAM c = (DEFAULT_VALUE (LITERAL_EXPR 3))) , (STAR_PARAM * args : (ANNOTATION (NAME_EXPR int))) , (PARAM d) , (STAR_STAR_PARAM ** kw) )) -> (RETURN_ANNOTATION (LITERAL_EXPR None)) : (BLOCK (PASS_STMT pass))))",
    );
    check("def g[T: int, *Ts, **P](x): return x\n", "(MODULE (FUNC_DEF def g (TYPE_PARAM_LIST [ (TYPE_PARAM T : (ANNOTATION (NAME_EXPR int))) , (TYPE_VAR_TUPLE_PARAM * Ts) , (PARAM_SPEC_PARAM ** P) ]) (PARAM_LIST ( (PARAM x) )) : (BLOCK (RETURN_STMT return (NAME_EXPR x)))))");
    check("class C(B, metaclass=M):\n    x = 1\n", "(MODULE (CLASS_DEF class C (ARG_LIST ( (ARG (NAME_EXPR B)) , (KEYWORD_ARG metaclass = (NAME_EXPR M)) )) : (BLOCK (ASSIGN_STMT (NAME_EXPR x) = (LITERAL_EXPR 1)))))");
    check("lambda a, *b, c=1: a\n", "(MODULE (EXPR_STMT (LAMBDA_EXPR lambda (PARAM_LIST (PARAM a) , (STAR_PARAM * b) , (PARAM c = (DEFAULT_VALUE (LITERAL_EXPR 1)))) : (NAME_EXPR a))))");
}

#[test]
fn compound_statements() {
    check("if a:\n  b\nelif c:\n  d\nelse:\n  e\n", "(MODULE (IF_STMT if (NAME_EXPR a) : (BLOCK (EXPR_STMT (NAME_EXPR b))) (ELIF_CLAUSE elif (NAME_EXPR c) : (BLOCK (EXPR_STMT (NAME_EXPR d)))) (ELSE_CLAUSE else : (BLOCK (EXPR_STMT (NAME_EXPR e))))))");
    check("while x: pass\nelse: pass\n", "(MODULE (WHILE_STMT while (NAME_EXPR x) : (BLOCK (PASS_STMT pass)) (ELSE_CLAUSE else : (BLOCK (PASS_STMT pass)))))");
    check("for i, j in x, y: pass\n", "(MODULE (FOR_STMT for (TUPLE_EXPR (NAME_EXPR i) , (NAME_EXPR j)) in (TUPLE_EXPR (NAME_EXPR x) , (NAME_EXPR y)) : (BLOCK (PASS_STMT pass))))");
    check("try:\n  a\nexcept* E as e:\n  b\nexcept:\n  c\nelse:\n  d\nfinally:\n  e\n", "(MODULE (TRY_STMT try : (BLOCK (EXPR_STMT (NAME_EXPR a))) (EXCEPT_CLAUSE except * (NAME_EXPR E) as e : (BLOCK (EXPR_STMT (NAME_EXPR b)))) (EXCEPT_CLAUSE except : (BLOCK (EXPR_STMT (NAME_EXPR c)))) (ELSE_CLAUSE else : (BLOCK (EXPR_STMT (NAME_EXPR d)))) (FINALLY_CLAUSE finally : (BLOCK (EXPR_STMT (NAME_EXPR e))))))");
    check("with open(f) as a, b: pass\n", "(MODULE (WITH_STMT with (WITH_ITEM (CALL_EXPR (NAME_EXPR open) (ARG_LIST ( (ARG (NAME_EXPR f)) ))) as (NAME_EXPR a)) , (WITH_ITEM (NAME_EXPR b)) : (BLOCK (PASS_STMT pass))))");
    check("with (a as b, c):\n  pass\n", "(MODULE (WITH_STMT with ( (WITH_ITEM (NAME_EXPR a) as (NAME_EXPR b)) , (WITH_ITEM (NAME_EXPR c)) ) : (BLOCK (PASS_STMT pass))))");
    check("with (a, b):\n  pass\n", "(MODULE (WITH_STMT with ( (WITH_ITEM (NAME_EXPR a)) , (WITH_ITEM (NAME_EXPR b)) ) : (BLOCK (PASS_STMT pass))))");
    check(
        "async with a: pass\n",
        "(MODULE (WITH_STMT async with (WITH_ITEM (NAME_EXPR a)) : (BLOCK (PASS_STMT pass))))",
    );
}

#[test]
fn expressions_precedence() {
    check("a + b * c ** -d\n", "(MODULE (EXPR_STMT (BINARY_EXPR (NAME_EXPR a) + (BINARY_EXPR (NAME_EXPR b) * (BINARY_EXPR (NAME_EXPR c) ** (UNARY_EXPR - (NAME_EXPR d)))))))");
    check("a < b <= c is not d not in e\n", "(MODULE (EXPR_STMT (COMPARE_EXPR (NAME_EXPR a) (COMPARE_OP <) (NAME_EXPR b) (COMPARE_OP <=) (NAME_EXPR c) (COMPARE_OP is not) (NAME_EXPR d) (COMPARE_OP not in) (NAME_EXPR e))))");
    check("not a or b and c\n", "(MODULE (EXPR_STMT (BOOL_OP_EXPR (NOT_EXPR not (NAME_EXPR a)) or (BOOL_OP_EXPR (NAME_EXPR b) and (NAME_EXPR c)))))");
    check("x if c else y if d else z\n", "(MODULE (EXPR_STMT (CONDITIONAL_EXPR (NAME_EXPR x) if (NAME_EXPR c) else (CONDITIONAL_EXPR (NAME_EXPR y) if (NAME_EXPR d) else (NAME_EXPR z)))))");
    check("a | b ^ c & d << e\n", "(MODULE (EXPR_STMT (BINARY_EXPR (NAME_EXPR a) | (BINARY_EXPR (NAME_EXPR b) ^ (BINARY_EXPR (NAME_EXPR c) & (BINARY_EXPR (NAME_EXPR d) << (NAME_EXPR e)))))))");
    check(
        "await f()\n",
        "(MODULE (EXPR_STMT (AWAIT_EXPR await (CALL_EXPR (NAME_EXPR f) (ARG_LIST ( ))))))",
    );
    check("(y := f(x))\n", "(MODULE (EXPR_STMT (PAREN_EXPR ( (NAMED_EXPR (NAME_EXPR y) := (CALL_EXPR (NAME_EXPR f) (ARG_LIST ( (ARG (NAME_EXPR x)) )))) ))))");
    check("a.b.c[1:2, ::3][x]\n", "(MODULE (EXPR_STMT (SUBSCRIPT_EXPR (SUBSCRIPT_EXPR (ATTRIBUTE_EXPR (ATTRIBUTE_EXPR (NAME_EXPR a) . b) . c) [ (SLICE_LIST (SLICE_EXPR (LITERAL_EXPR 1) : (LITERAL_EXPR 2)) , (SLICE_EXPR : : (LITERAL_EXPR 3))) ]) [ (NAME_EXPR x) ])))");
    check("f(a, *b, k=1, **c)\n", "(MODULE (EXPR_STMT (CALL_EXPR (NAME_EXPR f) (ARG_LIST ( (ARG (NAME_EXPR a)) , (STAR_ARG * (NAME_EXPR b)) , (KEYWORD_ARG k = (LITERAL_EXPR 1)) , (STAR_STAR_ARG ** (NAME_EXPR c)) )))))");
    check("f(x for x in y if x)\n", "(MODULE (EXPR_STMT (CALL_EXPR (NAME_EXPR f) (ARG_LIST ( (GENERATOR_EXPR (NAME_EXPR x) (COMP_FOR for (NAME_EXPR x) in (NAME_EXPR y)) (COMP_IF if (NAME_EXPR x))) )))))");
}

#[test]
fn displays_and_comprehensions() {
    check("[]\n", "(MODULE (EXPR_STMT (LIST_EXPR [ ])))");
    check(
        "[1, *a,]\n",
        "(MODULE (EXPR_STMT (LIST_EXPR [ (LITERAL_EXPR 1) , (STAR_EXPR * (NAME_EXPR a)) , ])))",
    );
    check("[x async for x in y]\n", "(MODULE (EXPR_STMT (LIST_COMP [ (NAME_EXPR x) (COMP_FOR async for (NAME_EXPR x) in (NAME_EXPR y)) ])))");
    check("{}\n", "(MODULE (EXPR_STMT (DICT_EXPR { })))");
    check("{**a, **b, 1: 2}\n", "(MODULE (EXPR_STMT (DICT_EXPR { (DICT_UNPACK ** (NAME_EXPR a)) , (DICT_UNPACK ** (NAME_EXPR b)) , (DICT_ITEM (LITERAL_EXPR 1) : (LITERAL_EXPR 2)) })))");
    check("{1: 2, **d}\n", "(MODULE (EXPR_STMT (DICT_EXPR { (DICT_ITEM (LITERAL_EXPR 1) : (LITERAL_EXPR 2)) , (DICT_UNPACK ** (NAME_EXPR d)) })))");
    check("{k: v for k, v in d}\n", "(MODULE (EXPR_STMT (DICT_COMP { (DICT_ITEM (NAME_EXPR k) : (NAME_EXPR v)) (COMP_FOR for (TUPLE_EXPR (NAME_EXPR k) , (NAME_EXPR v)) in (NAME_EXPR d)) })))");
    check(
        "{1, 2}\n",
        "(MODULE (EXPR_STMT (SET_EXPR { (LITERAL_EXPR 1) , (LITERAL_EXPR 2) })))",
    );
    check("{x for x in y}\n", "(MODULE (EXPR_STMT (SET_COMP { (NAME_EXPR x) (COMP_FOR for (NAME_EXPR x) in (NAME_EXPR y)) })))");
    check("()\n", "(MODULE (EXPR_STMT (TUPLE_EXPR ( ))))");
    check(
        "(1,)\n",
        "(MODULE (EXPR_STMT (TUPLE_EXPR ( (LITERAL_EXPR 1) , ))))",
    );
    check(
        "(1)\n",
        "(MODULE (EXPR_STMT (PAREN_EXPR ( (LITERAL_EXPR 1) ))))",
    );
    check("(x for x in y)\n", "(MODULE (EXPR_STMT (GENERATOR_EXPR ( (NAME_EXPR x) (COMP_FOR for (NAME_EXPR x) in (NAME_EXPR y)) ))))");
}

#[test]
fn strings_and_fstrings() {
    check(
        "'a' \"b\"\n",
        "(MODULE (EXPR_STMT (CONCAT_STRING_EXPR (STRING_EXPR 'a') (STRING_EXPR \"b\"))))",
    );
    check("f'a{x!r:>{w}}b'\n", "(MODULE (EXPR_STMT (FSTRING_EXPR f' a (FSTRING_REPLACEMENT { (NAME_EXPR x) (FSTRING_CONVERSION ! r) (FSTRING_FORMAT_SPEC : > (FSTRING_REPLACEMENT { (NAME_EXPR w) })) }) b ')))");
    check(
        "f'{x=}'\n",
        "(MODULE (EXPR_STMT (FSTRING_EXPR f' (FSTRING_REPLACEMENT { (NAME_EXPR x) = }) ')))",
    );
    check("f'{a, b}'\n", "(MODULE (EXPR_STMT (FSTRING_EXPR f' (FSTRING_REPLACEMENT { (TUPLE_EXPR (NAME_EXPR a) , (NAME_EXPR b)) }) ')))");
}

#[test]
fn match_statement() {
    check(
        "match p:\n  case [1, *rest] | (2, 3):\n    pass\n  case {'k': v, **r}:\n    pass\n  case Point(x=0, y) as pt if pt:\n    pass\n  case -1 | 2+3j | None | a.b:\n    pass\n  case _:\n    pass\n",
        "(MODULE (MATCH_STMT match (NAME_EXPR p) : (CASE_CLAUSE case (MATCH_OR_PATTERN (MATCH_SEQUENCE_PATTERN [ (MATCH_VALUE_PATTERN 1) , (MATCH_STAR_PATTERN * rest) ]) | (MATCH_SEQUENCE_PATTERN ( (MATCH_VALUE_PATTERN 2) , (MATCH_VALUE_PATTERN 3) ))) : (BLOCK (PASS_STMT pass))) (CASE_CLAUSE case (MATCH_MAPPING_PATTERN { (MATCH_MAPPING_ITEM (STRING_EXPR 'k') : (MATCH_CAPTURE_PATTERN v)) , (MATCH_MAPPING_REST ** r) }) : (BLOCK (PASS_STMT pass))) (CASE_CLAUSE case (MATCH_AS_PATTERN (MATCH_CLASS_PATTERN Point ( (MATCH_KEYWORD_PATTERN x = (MATCH_VALUE_PATTERN 0)) , (MATCH_CLASS_ARG (MATCH_CAPTURE_PATTERN y)) )) as pt) (GUARD if (NAME_EXPR pt)) : (BLOCK (PASS_STMT pass))) (CASE_CLAUSE case (MATCH_OR_PATTERN (MATCH_VALUE_PATTERN - 1) | (MATCH_VALUE_PATTERN 2 + 3j) | (MATCH_SINGLETON_PATTERN None) | (MATCH_VALUE_PATTERN a . b)) : (BLOCK (PASS_STMT pass))) (CASE_CLAUSE case (MATCH_WILDCARD_PATTERN _) : (BLOCK (PASS_STMT pass)))))",
    );
    check("match = 1\nmatch(x)\nmatch.y\n", "(MODULE (ASSIGN_STMT (NAME_EXPR match) = (LITERAL_EXPR 1)) (EXPR_STMT (CALL_EXPR (NAME_EXPR match) (ARG_LIST ( (ARG (NAME_EXPR x)) )))) (EXPR_STMT (ATTRIBUTE_EXPR (NAME_EXPR match) . y)))");
    check("match x, y:\n  case a, b:\n    pass\n", "(MODULE (MATCH_STMT match (TUPLE_EXPR (NAME_EXPR x) , (NAME_EXPR y)) : (CASE_CLAUSE case (MATCH_SEQUENCE_PATTERN (MATCH_CAPTURE_PATTERN a) , (MATCH_CAPTURE_PATTERN b)) : (BLOCK (PASS_STMT pass)))))");
}

#[test]
fn error_recovery() {
    let e = errors("def f(:\n  pass\n");
    assert!(e.contains(&"expected parameter".to_string()), "{e:?}");
    assert!(e.contains(&"'(' was never closed".to_string()), "{e:?}");
    let e = errors("if x\n  y\n");
    assert!(e.iter().any(|m| m.contains("expected ':'")), "{e:?}");
    let e = errors("x = (1, 2\ny = 3\n");
    assert!(!e.is_empty());
    let p = parse("def f():\npass\n");
    assert!(p
        .errors()
        .iter()
        .any(|e| e.message.contains("indented block")));
    // Subsequent statements still parse after an error.
    let p = parse("x = = 1\ny = 2\n");
    assert!(!p.ok());
    assert!(sexp(&p.syntax_node()).contains("(ASSIGN_STMT (NAME_EXPR y) = (LITERAL_EXPR 2))"));
    // Never panics on garbage.
    for src in [
        "(((",
        "def",
        "class :",
        "@\n",
        "if if if",
        "x[",
        "f(a=",
        "'''",
        "match x:\n  case",
        "lambda",
        "a.",
        "yield from",
        "for in x: pass",
        "try:\n pass\n",
        "\t\t",
        "}",
    ] {
        let _ = parse(src);
    }
}

#[test]
fn unexpected_indent_inside_a_block_is_recovered() {
    // A block's statement loop used to spin on the INDENT until the parser's
    // step limit panicked; it now recovers like the module-level loop.
    for src in [
        "if a:\n    x\n        y\n",
        "def f():\n    x = 1\n        y = 2\n            z = 3\n    w = 4\nv = 5\n",
    ] {
        let p = std::panic::catch_unwind(|| parse(src))
            .unwrap_or_else(|_| panic!("parser panicked on {src:?}"));
        assert_eq!(p.syntax_node().text().to_string(), src);
        // One error per over-indented level (`z` is over-indented again).
        let expected = src.matches("\n        ").count().clamp(1, 2);
        assert_eq!(
            p.errors()
                .iter()
                .filter(|e| e.message == "unexpected indent")
                .count(),
            expected,
            "{:?}",
            p.errors()
        );
    }
    // The over-indented statement is parsed as a statement; the block goes on.
    let src = "def f():\n    x = 1\n        y = 2\n    z = 3\n";
    let tree = sexp(&parse(src).syntax_node());
    assert!(tree.contains("(ASSIGN_STMT (NAME_EXPR y) = (LITERAL_EXPR 2)) (ASSIGN_STMT (NAME_EXPR z) = (LITERAL_EXPR 3))"), "{tree}");
}

#[test]
fn decorated_class_and_nested_blocks() {
    let src = "class A:\n    @property\n    def x(self):\n        if self:\n            return 1\n        return 2\n\n    y = 3\n";
    let p = parse(src);
    assert!(p.ok(), "{:?}", p.errors());
    assert_eq!(p.syntax_node().text().to_string(), src);
    let s = sexp(&p.syntax_node());
    assert!(s.starts_with("(MODULE (CLASS_DEF class A : (BLOCK (FUNC_DEF (DECORATORS (DECORATOR @ (NAME_EXPR property))) def x"));
    assert!(s.ends_with("(ASSIGN_STMT (NAME_EXPR y) = (LITERAL_EXPR 3)))))"));
}

/// Parses the local CPython standard library (if present) as a smoke corpus.
#[test]
fn parses_stdlib_without_panics() {
    let dirs = [
        "/usr/lib/python3.12",
        "/usr/lib/python3.13",
        "/usr/lib/python3.11",
    ];
    let Some(dir) = dirs.iter().find(|d| std::path::Path::new(d).is_dir()) else {
        return;
    };
    let mut files = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if p.is_dir() {
                    if !matches!(
                        name,
                        "test"
                            | "tests"
                            | "lib2to3"
                            | "__pycache__"
                            | "site-packages"
                            | "idlelib"
                            | "badsyntax"
                    ) {
                        walk(&p, out);
                    }
                } else if p.extension().and_then(|e| e.to_str()) == Some("py")
                    && !name.starts_with("bad")
                {
                    out.push(p);
                }
            }
        }
    }
    walk(std::path::Path::new(dir), &mut files);
    let mut with_errors = Vec::new();
    for f in &files {
        let Ok(bytes) = std::fs::read(f) else {
            continue;
        };
        let Ok(src) = String::from_utf8(bytes) else {
            continue;
        };
        let p = parse(&src);
        assert_eq!(
            p.syntax_node().text().to_string(),
            src,
            "roundtrip {}",
            f.display()
        );
        if !p.ok() {
            with_errors.push(format!("{}: {}", f.display(), p.errors()[0].message));
        }
    }
    eprintln!(
        "stdlib corpus: {} files, {} with errors",
        files.len(),
        with_errors.len()
    );
    for e in with_errors.iter().take(60) {
        eprintln!("  {e}");
    }
    let ratio = with_errors.len() as f64 / files.len().max(1) as f64;
    assert!(
        ratio < 0.01,
        "{} of {} stdlib files had parse errors:\n{}",
        with_errors.len(),
        files.len(),
        with_errors
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// Regressions found by the parser corpus tests (tests/corpus.rs: CPython's
// stdlib and test suite, site-packages) and by differential testing against
// CPython 3.12 / 3.14: one test per class of construct that used to fail.

/// Asserts that `src` parses without errors and losslessly.
fn ok(src: &str) {
    let p = parse(src);
    assert!(p.ok(), "unexpected errors for {src:?}: {:?}", p.errors());
    assert_eq!(
        p.syntax_node().text().to_string(),
        src,
        "roundtrip for {src:?}"
    );
}

#[test]
fn raw_fstrings_keep_escaped_quotes() {
    // A backslash keeps the next character from ending a raw f-string too
    // (yt-dlp/pandas/setuptools regexes, CPython's test_fstring.py).
    for src in [
        r#"fr"\'""#,
        r#"fr'\"\'\"'"#,
        r#"rf'<iframe[^>]+src=[\'"](?P<url>{_VALID_URL})[\'"]'"#,
        r#"rf"\"\[{re.escape(name)}\] not found in axis\"""#,
        r#"re.compile(rf'[^\\\'\"{string.whitespace} ]*')"#,
        r#"rf'\{x}\{{'"#,
        r#"Rf'''\''''"#,
        r#"rt'\'{x}'"#,
    ] {
        ok(&format!("{src}\n"));
    }
    check(
        "fr'\\'' rf\"\\\"{x}\\\"\"\n",
        "(MODULE (EXPR_STMT (CONCAT_STRING_EXPR (FSTRING_EXPR fr' \\' ') (FSTRING_EXPR rf\" \\\" (FSTRING_REPLACEMENT { (NAME_EXPR x) }) \\\" \"))))",
    );
}

#[test]
fn fstrings_pep701() {
    for src in [
        "f'{x!r:>{width}.{prec}}' f'{x!s}' f'{x!a:^10}'\n",
        "f\"{d[\"key\"]}\"\n",
        "f'{f'{f'{1}'}'}'\n",
        "f'{\"\\n\".join(lines)}'\n",
        "f'''{\n    x  # comment\n}'''\n",
        "f'___{\n    x\n}___'\n",
        "f'{x = !r:^20}' f'{x=}' f'{x.y()=}'\n",
        "f'{x:{\"a\"}}' f'{x:%Y-%m-%d}' f'{x:=10}' f'{(x:=10)}'\n",
        "f'\\N{BULLET} {x}' rf'\\N{x}'\n",
        "f'{x}}}{{y}}' f'{{{x}}}'\n",
        "f'{ {\"a\": 1}[\"a\"] }' f'{x[1:2]}' f'{a!=b}'\n",
        "f'{x:{y:{z}}}' f'{x:a{y}b{z}c}'\n",
        "f'{x}' 'y' f'{z}' \"w\"\n",
        "t'{x!r:>{w}}' rt'\\d{x}' T'{x=}'\n",
    ] {
        ok(src);
    }
}

#[test]
fn string_and_number_literals() {
    ok("x = [r'a', R'a', b'a', B'a', u'a', U'a', rb'a', Rb'a', bR'a', BR'a', br'a', Br'a', f'a', F'a', rf'a', fR'a', Rf'a', FR'a']\n");
    ok("x = ['''it's''', \"\"\"a\"\"b\"\"\", '\\'', r'\\'', r'\\\\', \"\\N{SNOWMAN}\", 'a\\\nb']\n");
    ok("x = [1_000, 0x_FF, 0o_17, 0b_1, 0XdeadBEEF, 1e10, 1E-5, 1.5e+3, .5, 5., 5.e3, 1j, 1.5J, 1e3j, 00, 0_0, 09.5, 0777e1, 1_0.0_1e1_0j]\n");
    // `1.` is a float even before another dot; hex/octal/binary have no fraction.
    ok("1..real; 1 .real; 0x1.real; 0b1.real; 1.0.hex()\n");
    // CPython accepts (with a warning) keywords glued to numbers.
    ok("x = 1if y else 2\nw = 1or 2\nv = [i for i in range(3)if i]\n");
    check(
        "[0x1for x in y]\n",
        "(MODULE (EXPR_STMT (LIST_EXPR [ (BOOL_OP_EXPR (LITERAL_EXPR 0x1f) or (COMPARE_EXPR (NAME_EXPR x) (COMPARE_OP in) (NAME_EXPR y))) ])))",
    );
    for bad in [
        "0777\n", "1__0\n", "1_\n", "0b2\n", "0o8\n", "0x\n", "1.real\n", "0x1g\n", "1e\n",
    ] {
        assert!(!errors(bad).is_empty(), "{bad:?} must be an error");
    }
}

#[test]
fn unicode_identifiers_and_bom() {
    // XID_Start / XID_Continue, including a variation selector (CPython's
    // test_unicode_identifiers.py) and Other_ID_Start (℘).
    ok("café = 1\nℕ = 2\n变量 = 3\n℘ = 4\nx\u{e0100} = 5\nµ = 6\nΩ = 7\n");
    // A UTF-8 byte order mark at the start of a file is an encoding
    // signature, not code (CPython skips it when it decodes the file).
    ok("\u{feff}import os\n");
    assert!(!errors("x² = 1\n").is_empty());
    assert!(!errors("゛ = 1\n").is_empty());
}

#[test]
fn bracket_recovery_never_fires_on_valid_code() {
    // Continuation lines that start with keywords at the indentation of the
    // opening line (sympy, pygments) are valid: no "never closed" error.
    ok("result = (a\nif b else c)\n");
    ok("raises(ValueError,\nlambda: x)\n");
    ok("x = [y\nfor y in z\nif y]\n");
    ok("tokens = {\n'root': [\nnot_a_keyword,\n],\n}\nif x:\n    pass\n");
    ok("f(a,\nnot b,\nawait c)\n");
    // A bracket that really is unclosed is still cut off at the next statement.
    let e = errors("x = foo(\ndef f(): pass\n");
    assert!(e.contains(&"'(' was never closed".to_string()), "{e:?}");
    let p = parse("x = foo(\ndef f(): pass\n");
    assert!(
        sexp(&p.syntax_node()).contains("(FUNC_DEF def f"),
        "{}",
        sexp(&p.syntax_node())
    );
}

#[test]
fn except_groups_and_unparenthesized_except() {
    ok("try:\n    pass\nexcept* ValueError:\n    pass\nexcept *(A, B) as e:\n    pass\n");
    // Python 3.14 (PEP 758): several exception types without parentheses.
    ok("try:\n    pass\nexcept A, B:\n    pass\nexcept C, D,:\n    pass\n");
    ok("try:\n    pass\nexcept* A, B:\n    pass\n");
    check(
        "try:\n    pass\nexcept A, B:\n    pass\n",
        "(MODULE (TRY_STMT try : (BLOCK (PASS_STMT pass)) (EXCEPT_CLAUSE except (TUPLE_EXPR (NAME_EXPR A) , (NAME_EXPR B)) : (BLOCK (PASS_STMT pass)))))",
    );
    let e = errors("try:\n    pass\nexcept A, B as e:\n    pass\n");
    assert!(
        e.iter().any(|m| m.contains("must be parenthesized")),
        "{e:?}"
    );
}

#[test]
fn backslash_continuation_in_indentation() {
    // CPython 3.12+: the first backslash found at a non-zero column inside
    // the indentation sets the indentation of the logical line.
    check(
        "if x:\n    \\\n    pass\n",
        "(MODULE (IF_STMT if (NAME_EXPR x) : (BLOCK (PASS_STMT pass))))",
    );
    check(
        "if x:\n\\\n    pass\ny\n",
        "(MODULE (IF_STMT if (NAME_EXPR x) : (BLOCK (PASS_STMT pass))) (EXPR_STMT (NAME_EXPR y)))",
    );
    ok("class A:\n    \\\n    x = 1\n");
    ok("if x:\n  \\\n        pass\n  y\n");
    ok("x = 1\n\\\ny = 2\n");
    ok("if x:\n    \\\n\n    pass\n");
    assert_eq!(errors("x = 1\n    \\\ny = 2\n"), ["unexpected indent"]);
    // A line continuation right before the end of the file is an error.
    assert_eq!(
        errors("x = 1 \\\n"),
        ["unexpected end of file after line continuation"]
    );
    assert_eq!(
        errors("if x:\n    pass\n\\\n"),
        ["unexpected end of file after line continuation"]
    );
    // INDENT only covers the whitespace in front of the first token.
    let (tokens, _) = crate::tokenize("if x:\n    \\\n  pass\n");
    let indent = tokens
        .iter()
        .find(|t| t.kind == crate::SyntaxKind::INDENT)
        .expect("INDENT");
    assert_eq!(
        u32::from(indent.range.start())..u32::from(indent.range.end()),
        12..14
    );
}

#[test]
fn soft_keywords_as_names() {
    ok("match = case = type = _ = print = 1\nmatch(x)\nmatch[x] = case\nmatch.x, case.y\nmatch: int = 1\nmatch -x\nprint(match, case, type, _)\n");
    ok("def match(case, type, _): pass\nclass case: pass\nimport match\nfrom type import case as _\nx.match = x.type = x.case = x._\n");
    ok("match -x:\n    case 1: pass\nmatch (x):\n    case _: pass\nmatch [x]:\n    case _: pass\nmatch *a, b:\n    case _: pass\nmatch not x:\n    case _: pass\n");
    ok("match match:\n    case case:\n        type type = type\n");
    ok("print >> sys.stderr, 'x'\nexec('x')\nprint\n");
}

#[test]
fn template_strings_in_patterns() {
    // CPython 3.14's parser accepts t-strings (and f-strings) as literal
    // patterns; the compiler rejects them later.
    check(
        "match x:\n    case t'{y}':\n        pass\n",
        "(MODULE (MATCH_STMT match (NAME_EXPR x) : (CASE_CLAUSE case (MATCH_VALUE_PATTERN (FSTRING_EXPR t' (FSTRING_REPLACEMENT { (NAME_EXPR y) }) ')) : (BLOCK (PASS_STMT pass)))))",
    );
    ok("match x:\n    case {t'k': v, f'j': w}:\n        pass\n    case f'a' | 'b':\n        pass\n");
}

#[test]
fn type_alias_is_a_simple_statement() {
    check(
        "class A: type X = int\n",
        "(MODULE (CLASS_DEF class A : (BLOCK (TYPE_ALIAS_STMT type X = (NAME_EXPR int)))))",
    );
    check(
        "x = 1; type X[T] = list[T]\n",
        "(MODULE (ASSIGN_STMT (NAME_EXPR x) = (LITERAL_EXPR 1)) ; (TYPE_ALIAS_STMT type X (TYPE_PARAM_LIST [ (TYPE_PARAM T) ]) = (SUBSCRIPT_EXPR (NAME_EXPR list) [ (NAME_EXPR T) ])))",
    );
    ok("async def f(): type X = (yield)\n");
    ok("type X[T: (int, str) = int, *Ts = *tuple[int], **P = [int]] = Callable[P, T]\n");
    ok("type(x); type = 1; type.x = 2; type[x] = 3\n");
}

#[test]
fn not_equal_spelling_follows_barry_as_flufl() {
    assert_eq!(errors("1 <> 2\n"), ["use '!=' instead of '<>'"]);
    // The diagnostic covers the operator: the IDE offers to replace it.
    assert_eq!(
        parse("a <> b\n").errors()[0].range,
        crate::TextRange::new(2.into(), 4.into())
    );
    ok("from __future__ import barry_as_FLUFL\n1 <> 2\n");
    assert_eq!(
        errors("from __future__ import barry_as_FLUFL\n1 != 2\n"),
        ["with Barry as BDFL, use '<>' instead of '!='"]
    );
    assert_eq!(
        errors("from __future__ import annotations\n1 <> 2\n"),
        ["use '!=' instead of '<>'"]
    );
}

#[test]
fn indentation_errors_recover() {
    assert_eq!(errors("  x = 1\ny = 2\n"), ["unexpected indent"]);
    // An over-indented line inside a block: reported once, and the rest of the
    // block still belongs to the function.
    let p = parse("def f():\n    x = 1\n        y = 2\n    z = 3\nw = 4\n");
    assert_eq!(
        p.errors()
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>(),
        ["unexpected indent"]
    );
    let s = sexp(&p.syntax_node());
    assert!(
        s.contains("(ASSIGN_STMT (NAME_EXPR z) = (LITERAL_EXPR 3)))) (ASSIGN_STMT (NAME_EXPR w)"),
        "{s}"
    );
    assert_eq!(
        errors("if x:\n    pass\n  y\n"),
        ["unindent does not match any outer indentation level"]
    );
}

#[test]
fn line_endings_and_whitespace() {
    ok("if x:\r\n    y = (1,\r\n         2)\r\n    z = '''a\r\nb'''\r\n");
    ok("if x:\r    pass\rw = 1\r");
    ok("\x0cdef f():\n\tif x:\n\t\treturn 1\n\x0c\nclass A: pass\n");
    ok("x = 1 \x0c+ \t2\nif x:\n    \x0c    pass\n");
    ok("x = 1 + \\\r\n    2\r\n");
}

#[test]
fn checklist_of_constructs() {
    // One module touching every construct class of the corpus hardening task.
    ok(concat!(
        "@x[0].y\n@(lambda f: f)\n@a if b else c\n@x := y\nasync def f[T: int, *Ts, **P = [int]](a, /, b: int = 1, *args: *Ts, c, d=2, **kw) -> T: ...\n",
        "g = lambda: 0; h = lambda *a, b=1, **k: (a, b, k); i = lambda a, /, b, *, c: 0; j = lambda x=(lambda: 1): x\n",
        "a[*b]; a[1, *b]; a[::2, ::-1]; a[1:2, ...]; a[()]; a[:, None]; a[x if y else z:]\n",
        "def gen():\n    x = yield *a, b\n    return *a, *b\n",
        "for *a, b in c: pass\nfor x in *a, *b: pass\n",
        "[y := f(x), y ** 2]; [x for x in data if (y := f(x))]; f(x := 1); a[x := 1]\n",
        "async def h():\n    [x async for x in y]; {k: v async for k, v in z}; (await x for x in y)\n    async with a as b, c as d: pass\n    async for x in y: pass\n",
        "def scope():\n    global g\n    def inner():\n        nonlocal h\n",
        "x = ...; a[...]; Callable[..., int]\n",
        "class A[T](Base[T], metaclass=M): pass\n",
        "with (open(a) as f, open(b) as g,): pass\nwith (a, b): pass\nwith (a, b) as c: pass\n",
        "x = a < b <= c is not d not in e != f; y = not a in b\n",
        "x = {**a, 'b': 1, **c}; y = {*a, *b}; z = {k: v for k, v in w}\n",
        "match p:\n    case Point(x=0, y=yy) | [1, 2, *rest] if yy: pass\n    case {'k': v, **others}: pass\n    case (1 | 2) as n: pass\n    case -1 | 1+2j | None | a.b: pass\n    case str() | bytes(): pass\n    case _: pass\n",
    ));
}

#[test]
fn deep_nesting_is_bounded() {
    // Nesting CPython 3.12 accepts parses without errors.
    ok(&format!("x = {}1{}\n", "(".repeat(200), ")".repeat(200)));
    ok(&format!("x = {}1\n", "-".repeat(5_900)));
    ok(&format!("x = {}1\n", "not ".repeat(5_900)));
    ok(&format!("x = 1{}\n", " if 1 else 1".repeat(5_900)));
    ok(&format!("x = 1{}\n", " + 1".repeat(9_995)));
    ok(&format!("x = a{}\n", ".b".repeat(9_995)));
    ok(&((0..98)
        .map(|i| format!("{}if x:\n", " ".repeat(i)))
        .collect::<String>()
        + &" ".repeat(98)
        + "pass\n"));
    // Absurd nesting gives a diagnostic or two (not one per level) and a
    // lossless tree, without overflowing the 2 MiB test thread stack while
    // parsing, walking or dropping the tree.
    for src in [
        format!("x = {}1{}\n", "(".repeat(100_000), ")".repeat(100_000)),
        format!("x = {}1{}\n", "f(".repeat(100_000), ")".repeat(100_000)),
        format!("x = {}1\n", "-".repeat(100_000)),
        format!("x = 1{}\n", " ** 1".repeat(100_000)),
        format!("x = 1{}\n", " + 1".repeat(100_000)),
        format!("x = a{}\n", "()".repeat(100_000)),
        format!("x = a{}\n", "[0]".repeat(100_000)),
        format!("x = f'{}{}'\n", "{x:".repeat(100_000), "}".repeat(100_000)),
        format!(
            "match x:\n    case {}a{}:\n        pass\n",
            "[".repeat(100_000),
            "]".repeat(100_000)
        ),
    ] {
        let p = parse(&src);
        let n = p.errors().len();
        assert!(
            (1..=3).contains(&n),
            "{n} errors for {:?}...: {:?}",
            &src[..20],
            &p.errors()[..n.min(5)]
        );
        assert!(
            p.errors()
                .iter()
                .any(|e| e.message == "too many nested expressions or blocks"),
            "{:?}",
            p.errors()
        );
        assert_eq!(p.syntax_node().text(), src.as_str());
        assert!(p.syntax_node().descendants().count() > 1);
    }
}

#[test]
fn unclosed_bracket_before_many_blank_lines_is_linear() {
    // Every newline inside an unclosed bracket used to rescan all following
    // blank or comment lines (minutes for this input).
    for filler in ["\n", "# comment\n", "   \n"] {
        let src = format!("x = (\n{}y = 1\n", filler.repeat(100_000));
        let started = std::time::Instant::now();
        let p = parse(&src);
        assert!(!p.ok());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{filler:?}: {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn parse_expression_reports_trailing_tokens() {
    let p = super::parse_expression("a + b c");
    assert_eq!(p.errors().len(), 1);
    assert_eq!(p.errors()[0].message, "unexpected token after expression");
    assert_eq!(u32::from(p.errors()[0].range.start()), 6);
    assert!(super::parse_expression("f(x)[1:2].y if z else w").ok());
}
