//! Integration tests with two grammars written for these tests:
//! `grammars/toy.tmLanguage.json` (a toy language, JSON) and
//! `grammars/rust-lite.tmLanguage` (a reduced Rust grammar, XML plist).
//!
//! `golden/*.tokens` were produced by vscode-textmate (with Oniguruma) from
//! the same grammars and samples; the tokenizer must reproduce them exactly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustpy_textmate::{
    scope_to_tag, tokenize_line, Grammar, GrammarError, RawGrammar, Registry, StackState, Token,
    MAX_LINE_LENGTH, TAG_NAMES,
};

fn dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(sub)
}

fn read(path: &str) -> String {
    std::fs::read_to_string(dir(path))
        .unwrap()
        .replace("\r\n", "\n")
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    let load = registry.load_bundle(&dir("grammars"));
    assert!(load.errors.is_empty(), "{:?}", load.errors);
    assert_eq!(load.loaded, ["source.rust-lite", "source.toy"]);
    registry
}

type Line = Vec<(String, Vec<String>)>;

/// Per line: each token's text and its scopes without the root scope.
fn lines(grammar: &Grammar, text: &str) -> Vec<Line> {
    let mut state = StackState::default();
    text.split('\n')
        .map(|line| {
            tokenize_line(grammar, line, &mut state)
                .into_iter()
                .map(|t| (line[t.range.clone()].to_string(), t.scopes[1..].to_vec()))
                .collect()
        })
        .collect()
}

/// Scopes (root excluded) of the first token whose text is `piece`.
fn scopes<'a>(line: &'a Line, piece: &str) -> Vec<&'a str> {
    let (_, scopes) = line
        .iter()
        .find(|(text, _)| text == piece)
        .unwrap_or_else(|| panic!("{piece:?} in {line:?}"));
    scopes.iter().map(String::as_str).collect()
}

/// Tokens are non-empty, contiguous and cover the line (without its line
/// break) exactly.
fn assert_covers(line: &str, tokens: &[Token]) {
    let content = line
        .strip_suffix('\n')
        .map_or(line, |l| l.strip_suffix('\r').unwrap_or(l));
    let mut pos = 0;
    for t in tokens {
        assert_eq!(t.range.start, pos, "gap or overlap in {line:?}: {tokens:?}");
        assert!(t.range.end > t.range.start, "empty token in {line:?}");
        assert!(
            content.is_char_boundary(t.range.end),
            "token splits a character in {line:?}"
        );
        assert!(!t.scopes.is_empty());
        pos = t.range.end;
    }
    assert_eq!(
        pos,
        content.len(),
        "tokens do not cover {line:?}: {tokens:?}"
    );
}

fn check_golden(registry: &Registry, scope: &str, sample: &str) {
    let grammar = registry.grammar(scope).unwrap();
    let text = read(&format!("golden/{sample}"));
    let mut expected: Vec<Vec<(usize, usize, Vec<String>)>> = Vec::new();
    for line in read(&format!("golden/{sample}.tokens")).lines() {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("@ ") {
            expected.push(Vec::new());
            continue;
        }
        let mut parts = line.split(' ');
        let start = parts.next().unwrap().parse().unwrap();
        let end = parts.next().unwrap().parse().unwrap();
        expected
            .last_mut()
            .unwrap()
            .push((start, end, parts.map(str::to_string).collect()));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.len(), expected.len());
    let mut state = StackState::default();
    for (n, (line, want)) in lines.iter().zip(&expected).enumerate() {
        let tokens = tokenize_line(&grammar, line, &mut state);
        assert_covers(line, &tokens);
        let got: Vec<_> = tokens
            .into_iter()
            .map(|t| (t.range.start, t.range.end, t.scopes))
            .collect();
        assert_eq!(&got, want, "{sample} line {n}: {line:?}");
    }
}

#[test]
fn loads_json_and_plist_grammars() {
    let registry = registry();
    let toy = registry.raw("source.toy").unwrap();
    assert_eq!(toy.name(), Some("Toy"));
    assert_eq!(toy.file_types(), ["toy"]);
    assert_eq!(toy.external_scopes(), ["source.rust-lite"]);
    assert_eq!(
        registry.raw("source.rust-lite").unwrap().name(),
        Some("Rust Lite")
    );
    assert_eq!(
        registry.scope_for_file_name("src/main.rlite"),
        Some("source.rust-lite")
    );
    assert_eq!(registry.scope_for_file_name("demo.toy"), Some("source.toy"));
    assert_eq!(registry.scope_for_file_name("demo.py"), None);
    assert_eq!(
        registry.scope_for_first_line("#!/usr/bin/env toy"),
        Some("source.toy")
    );

    // The documents also load on their own.
    let rust = Grammar::from_plist(&read("grammars/rust-lite.tmLanguage")).unwrap();
    assert_eq!(rust.scope_name(), "source.rust-lite");
    assert_eq!(rust.name(), Some("Rust Lite"));
    assert!(rust.unsupported_patterns().is_empty());
    let toy = Grammar::from_json(&read("grammars/toy.tmLanguage.json")).unwrap();
    assert_eq!(toy.file_types(), ["toy"]);
    assert!(toy.unsupported_patterns().is_empty());
    assert_eq!(
        RawGrammar::parse(&read("grammars/rust-lite.tmLanguage"))
            .unwrap()
            .scope_name(),
        "source.rust-lite"
    );

    assert_eq!(
        Grammar::from_json("{}").unwrap_err(),
        GrammarError::MissingScopeName
    );
    assert!(matches!(
        Grammar::from_json("{"),
        Err(GrammarError::Json(_))
    ));
    assert!(matches!(
        Grammar::from_plist("<plist><dict>"),
        Err(GrammarError::Plist(_))
    ));
    let mut empty = Registry::new();
    assert!(matches!(
        empty.load_file(&dir("grammars/missing.json")),
        Err(GrammarError::Io { .. })
    ));
    let load = empty.load_bundle(&dir("no-such-dir"));
    assert!(load.loaded.is_empty() && load.errors.len() == 1);
}

#[test]
fn toy_sample_matches_vscode_textmate() {
    check_golden(&registry(), "source.toy", "sample.toy");
}

#[test]
fn rust_lite_sample_matches_vscode_textmate() {
    check_golden(&registry(), "source.rust-lite", "sample.rlite");
}

#[test]
fn begin_end_nesting_across_lines() {
    let registry = registry();
    let toy = registry.grammar("source.toy").unwrap();
    let mut state = StackState::default();
    assert!(state.is_initial());
    tokenize_line(&toy, "fn f() {", &mut state);
    assert_eq!(state.scopes(), ["source.toy", "meta.block.toy"]);
    let line = "  if x { /* open TODO";
    let tokens = tokenize_line(&toy, line, &mut state);
    assert_covers(line, &tokens);
    assert_eq!(state.depth(), 4);
    assert_eq!(
        state.scopes(),
        [
            "source.toy",
            "meta.block.toy",
            "meta.block.toy",
            "comment.block.toy"
        ]
    );
    let todo = tokens
        .iter()
        .find(|t| &line[t.range.clone()] == "TODO")
        .unwrap();
    assert_eq!(todo.scopes.last().unwrap(), "keyword.other.todo.toy");
    let line = "  still */ }";
    let tokens = tokenize_line(&toy, line, &mut state);
    assert_eq!(&line[tokens[0].range.clone()], "  still ");
    assert_eq!(
        tokens[0].scopes,
        [
            "source.toy",
            "meta.block.toy",
            "meta.block.toy",
            "comment.block.toy"
        ]
    );
    let close = tokens.last().unwrap();
    assert_eq!(
        close.scopes,
        [
            "source.toy",
            "meta.block.toy",
            "meta.block.toy",
            "punctuation.section.block.end.toy"
        ]
    );
    assert_eq!(state.scopes(), ["source.toy", "meta.block.toy"]);
    tokenize_line(&toy, "}", &mut state);
    assert_eq!(state.depth(), 1);

    // Rust Lite block comments nest (a rule including itself).
    let rust = registry.grammar("source.rust-lite").unwrap();
    let l = lines(&rust, "/* a /* b\n*/ c */ d");
    assert_eq!(
        scopes(&l[0], " b"),
        ["comment.block.rust-lite", "comment.block.rust-lite"]
    );
    assert_eq!(scopes(&l[1], " c "), ["comment.block.rust-lite"]);
    assert_eq!(scopes(&l[1], "d"), ["variable.other.rust-lite"]);
}

#[test]
fn captures_names_and_content_names() {
    let registry = registry();
    let toy = registry.grammar("source.toy").unwrap();
    let l = lines(
        &toy,
        "if x { return \"a\\n${y}\"; }\nimport a.bc\n@inline fn f(p: Point) {}",
    );
    // `$1` in a rule name.
    assert_eq!(scopes(&l[0], "if"), ["keyword.control.if.toy"]);
    assert_eq!(
        scopes(&l[0], "return"),
        ["meta.block.toy", "keyword.control.return.toy"]
    );
    // beginCaptures / endCaptures, nested patterns and contentName.
    assert_eq!(
        scopes(&l[0], "\""),
        [
            "meta.block.toy",
            "string.quoted.double.toy",
            "punctuation.definition.string.begin.toy"
        ]
    );
    assert_eq!(
        scopes(&l[0], "\\n"),
        [
            "meta.block.toy",
            "string.quoted.double.toy",
            "constant.character.escape.toy"
        ]
    );
    assert_eq!(
        scopes(&l[0], "y"),
        [
            "meta.block.toy",
            "string.quoted.double.toy",
            "meta.interpolation.toy",
            "source.toy.embedded",
            "variable.other.toy"
        ]
    );
    // A capture retokenized with its own patterns.
    assert_eq!(
        scopes(&l[1], "a"),
        ["meta.path.toy", "entity.name.namespace.toy"]
    );
    assert_eq!(
        scopes(&l[1], "."),
        ["meta.path.toy", "punctuation.separator.namespace.toy"]
    );
    assert_eq!(
        scopes(&l[1], "bc"),
        ["meta.path.toy", "entity.name.namespace.toy"]
    );
    // Nested captures inside a named match.
    assert_eq!(
        scopes(&l[2], "@"),
        ["meta.decorator.toy", "punctuation.definition.decorator.toy"]
    );
    assert_eq!(scopes(&l[2], "inline"), ["meta.decorator.toy"]);
    assert_eq!(scopes(&l[2], "f"), ["entity.name.function.toy"]);
    // `\G`: only the type right after the colon.
    assert_eq!(
        scopes(&l[2], "Point"),
        ["meta.type.annotation.toy", "entity.name.type.toy"]
    );

    // `$1` in capture names (Rust Lite, loaded from plist).
    let rust = registry.grammar("source.rust-lite").unwrap();
    let l = lines(&rust, "enum Shape {}");
    assert_eq!(scopes(&l[0], "enum"), ["storage.type.enum.rust-lite"]);
    assert_eq!(scopes(&l[0], "Shape"), ["entity.name.type.enum.rust-lite"]);
}

#[test]
fn end_patterns_with_back_references_and_while_rules() {
    let registry = registry();
    let toy = registry.grammar("source.toy").unwrap();
    let l = lines(
        &toy,
        "t = <<EOT\nbody { here\nEOTX\nEOT\nafter\n> one *b*\n> two\nthree",
    );
    assert_eq!(
        scopes(&l[0], "EOT"),
        ["string.unquoted.heredoc.toy", "entity.name.tag.heredoc.toy"]
    );
    assert_eq!(
        scopes(&l[1], "body { here"),
        [
            "string.unquoted.heredoc.toy",
            "string.unquoted.heredoc.body.toy"
        ]
    );
    assert_eq!(
        scopes(&l[2], "EOTX"),
        [
            "string.unquoted.heredoc.toy",
            "string.unquoted.heredoc.body.toy"
        ]
    );
    assert_eq!(
        scopes(&l[3], "EOT"),
        ["string.unquoted.heredoc.toy", "entity.name.tag.heredoc.toy"]
    );
    assert_eq!(scopes(&l[4], "after"), ["variable.other.toy"]);
    // begin/while: continues while lines start with `>`.
    assert_eq!(
        scopes(&l[5], ">"),
        ["markup.quote.toy", "punctuation.definition.quote.toy"]
    );
    assert_eq!(
        scopes(&l[5], "*b*"),
        ["markup.quote.toy", "markup.bold.toy"]
    );
    assert_eq!(scopes(&l[6], "two"), ["markup.quote.toy"]);
    assert_eq!(scopes(&l[7], "three"), ["variable.other.toy"]);

    // Raw strings: `"#` ends `r#"`, `"##` ends `r##"`.
    let rust = registry.grammar("source.rust-lite").unwrap();
    let l = lines(&rust, "r##\"a \"# b\"##; x");
    assert_eq!(
        scopes(&l[0], "a \"# b"),
        ["string.quoted.double.raw.rust-lite"]
    );
    assert_eq!(scopes(&l[0], "x"), ["variable.other.rust-lite"]);
}

#[test]
fn apply_end_pattern_last() {
    let toy = registry().grammar("source.toy").unwrap();
    let l = lines(&toy, "[[a]]b] [a]]b]");
    // With applyEndPatternLast the nested `]]` wins over the `]` end…
    assert_eq!(
        scopes(&l[0], "]]"),
        [
            "meta.brackets.escaped.toy",
            "constant.character.escape.bracket.toy"
        ]
    );
    assert_eq!(scopes(&l[0], "b"), ["meta.brackets.escaped.toy"]);
    // …without it the end wins.
    let plain: Vec<_> = l[0].iter().skip_while(|(t, _)| t != " ").collect();
    assert_eq!(plain[2].0, "a");
    assert_eq!(
        plain[3],
        &("]".to_string(), vec!["meta.brackets.toy".to_string()])
    );
    assert_eq!(plain[4], &("]".to_string(), vec![]));
}

#[test]
fn includes_resolve_self_base_and_other_grammars() {
    let registry = registry();
    let toy = registry.grammar("source.toy").unwrap();
    let rust = registry.grammar("source.rust-lite").unwrap();
    // `$self`: blocks nest.
    let l = lines(&toy, "{ { let } }");
    assert_eq!(
        scopes(&l[0], "let"),
        [
            "meta.block.toy",
            "meta.block.toy",
            "keyword.control.let.toy"
        ]
    );
    // An included grammar: rust-lite inside `rust { … }`, whose blocks
    // include `$base` = the embedding toy grammar…
    let l = lines(&toy, "rust {\n  fn main() { let y = 1; }\n}");
    assert_eq!(
        scopes(&l[1], "fn"),
        ["meta.embedded.block.rust-lite", "storage.type.fn.rust-lite"]
    );
    assert_eq!(
        scopes(&l[1], "let"),
        [
            "meta.embedded.block.rust-lite",
            "meta.block.rust-lite",
            "keyword.control.let.toy"
        ]
    );
    assert_eq!(scopes(&l[2], "}"), ["punctuation.section.embedded.end.toy"]);
    // …while for rust-lite itself `$base` is rust-lite.
    let l = lines(&rust, "{ let y = 1; }");
    assert_eq!(
        scopes(&l[0], "let"),
        ["meta.block.rust-lite", "storage.modifier.rust-lite"]
    );
    // `scope#rule`: toy uses rust-lite's raw strings (with their back-reference).
    let l = lines(&toy, "f(r#\"x\"#)");
    assert_eq!(
        scopes(&l[0], "r"),
        [
            "string.quoted.double.raw.rust-lite",
            "storage.type.string.rust-lite"
        ]
    );
    assert_eq!(scopes(&l[0], "\"#"), ["string.quoted.double.raw.rust-lite"]);

    // Without rust-lite, the rule embedding it (all of whose patterns are
    // missing) is dropped, as in vscode-textmate; other rules still work.
    let alone = Grammar::from_json(&read("grammars/toy.tmLanguage.json")).unwrap();
    let l = lines(&alone, "rust { x }");
    assert_eq!(scopes(&l[0], "rust"), ["variable.other.toy"]);
    assert_eq!(
        scopes(&l[0], "{"),
        ["meta.block.toy", "punctuation.section.block.begin.toy"]
    );
    // Grammars linked later see grammars added since.
    let mut partial = Registry::new();
    partial
        .add_json(&read("grammars/toy.tmLanguage.json"))
        .unwrap();
    let before = partial.grammar("source.toy").unwrap();
    partial
        .load_file(&dir("grammars/rust-lite.tmLanguage"))
        .unwrap();
    let after = partial.grammar("source.toy").unwrap();
    assert_eq!(
        scopes(&lines(&before, "rust {")[0], "rust"),
        ["variable.other.toy"]
    );
    assert_eq!(
        scopes(&lines(&after, "rust {")[0], "rust"),
        ["keyword.other.embedded.toy"]
    );
}

#[test]
fn search_anchor_after_a_begin_that_consumed_the_line_break() {
    // Verified against vscode-textmate: the `begin` ate the newline, so `\G`
    // matches at the start of the next line.
    let grammar = Grammar::from_json(
        r#"{ "scopeName": "source.eol", "patterns": [
            { "begin": "start\\n", "end": "(?!\\G)", "name": "meta.eol",
              "patterns": [ { "match": "\\Gx", "name": "first.x" }, { "match": "x", "name": "other.x" } ] } ] }"#,
    )
    .unwrap();
    let l = lines(&grammar, "start\nxx");
    assert_eq!(l[0], [("start".to_string(), vec!["meta.eol".to_string()])]);
    assert_eq!(
        l[1],
        [
            (
                "x".to_string(),
                vec!["meta.eol".to_string(), "first.x".to_string()]
            ),
            ("x".to_string(), vec![])
        ]
    );
}

#[test]
fn grammars_that_never_advance_terminate() {
    let grammar = Grammar::from_json(
        r#"{ "scopeName": "source.loop", "patterns": [
            { "begin": "(?=y)", "end": "(?=y)", "name": "loop.y" },
            { "match": "(?=z)", "name": "zero.z" },
            { "begin": "\\b", "end": "\\b", "name": "edge", "patterns": [ { "include": "$self" } ] },
            { "begin": "(?=w)", "while": "(?=w)", "name": "while.w", "patterns": [ { "begin": "(?=w)", "end": "" } ] },
            { "begin": "", "end": "(?=q)", "patterns": [ { "include": "$self" } ] }
        ] }"#,
    )
    .unwrap();
    let start = Instant::now();
    for text in ["zz q", "abc def", "w w\nww", "q", "", "yy\nzz q"] {
        let mut state = StackState::default();
        for line in text.split('\n') {
            assert_covers(line, &tokenize_line(&grammar, line, &mut state));
        }
    }
    assert!(start.elapsed() < Duration::from_secs(10));

    // vscode-textmate: a rule entered and left without consuming anything
    // stays open for the rest of the line (and the next lines).
    let grammar = Grammar::from_json(
        r#"{ "scopeName": "source.y", "patterns": [ { "begin": "(?=y)", "end": "(?=y)", "name": "loop.y" } ] }"#,
    )
    .unwrap();
    let l = lines(&grammar, "yy\nzz q");
    assert_eq!(l[0], [("yy".to_string(), vec!["loop.y".to_string()])]);
    assert_eq!(l[1], [("zz q".to_string(), vec!["loop.y".to_string()])]);
}

#[test]
fn hostile_input_never_panics_and_tokens_cover_each_line() {
    let registry = registry();
    let mut samples: Vec<String> = [
        "",
        "\r\n",
        "\n",
        "fn f() {\r\n",
        "\0\u{1}\u{7f}\u{fffd}é漢字😀\u{200d}\u{2028}",
        "\"unterminated ${ { /* \\",
        "/* /* /* r##\"",
        "}}}} ]]]] )))",
        "<<EOT",
        "EOT",
        "> > >",
        "${${${}}}",
        "[[[[]]]]]]",
        "\t\t  \u{a0}x",
        "'''\"\"\"'a'b'",
        "@@ fn fn class class : : :",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Pseudo-random lines over an alphabet of syntax characters.
    let alphabet: Vec<char> = "abcxyzEOTr0123456789_ \t{}[]()<>\"'#/*\\!@$:;,.=+-|&é漢😀\r"
        .chars()
        .collect();
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..400 {
        let len = (seed >> 33) as usize % 60;
        let mut line = String::new();
        for _ in 0..len {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            line.push(alphabet[(seed >> 33) as usize % alphabet.len()]);
        }
        samples.push(line);
    }
    for scope in ["source.toy", "source.rust-lite"] {
        let grammar = registry.grammar(scope).unwrap();
        let mut state = StackState::default();
        for line in &samples {
            let tokens = tokenize_line(&grammar, line, &mut state);
            assert_covers(line, &tokens);
            assert!(tokens.iter().all(|t| t.scopes[0] == scope));
        }
    }
}

#[test]
fn random_patterns_are_rejected_or_run_without_panicking() {
    let pieces = [
        "a",
        "\\",
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        "^",
        "$",
        "?",
        "*",
        "+",
        "|",
        ".",
        "\\d",
        "\\h",
        "\\G",
        "\\A",
        "\\z",
        "\\b",
        "\\K",
        "\\1",
        "\\k<n>",
        "\\g<1>",
        "(?=",
        "(?!",
        "(?<=",
        "(?<!",
        "(?i)",
        "(?x)",
        "(?:",
        "(?<n>",
        "[[:alpha:]]",
        "\\p{L}",
        "\\x{41 42}",
        "{2,}",
        "{,3}",
        "{3,1}",
        "++",
        "*?",
        "#",
        " ",
        "é",
        "-",
        "&&",
        "[^",
        "\\n",
    ];
    let mut seed: u64 = 42;
    let mut next = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as usize
    };
    for _ in 0..600 {
        let pattern: String = (0..1 + next() % 12)
            .map(|_| pieces[next() % pieces.len()])
            .collect();
        let grammar = serde_json::json!({ "scopeName": "source.fuzz", "patterns": [
            { "match": pattern, "name": "m" },
            { "begin": pattern, "end": pattern, "name": "b", "patterns": [ { "include": "$self" } ] },
            { "begin": "x", "while": pattern }
        ] });
        let grammar = Grammar::from_raw(&RawGrammar::from_value(&grammar).unwrap());
        let _ = grammar.unsupported_patterns();
        let mut state = StackState::default();
        for line in ["aé1 (x) [y] {z} #", "\\K ab ba", "", "xx yy"] {
            assert_covers(line, &tokenize_line(&grammar, line, &mut state));
        }
    }
}

#[test]
fn overlong_lines_get_one_token_and_keep_the_state() {
    let toy = registry().grammar("source.toy").unwrap();
    let mut state = StackState::default();
    tokenize_line(&toy, "/* open", &mut state);
    let before = state.clone();
    let long = "x".repeat(MAX_LINE_LENGTH + 1);
    let tokens = tokenize_line(&toy, &long, &mut state);
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].range, 0..long.len());
    assert_eq!(tokens[0].scopes, ["source.toy", "comment.block.toy"]);
    assert_eq!(state, before);
}

#[test]
fn states_are_comparable_values() {
    let registry = registry();
    let toy = registry.grammar("source.toy").unwrap();
    let mut a = StackState::default();
    tokenize_line(&toy, "fn f() {", &mut a);
    let mut b = a.clone();
    assert_eq!(
        tokenize_line(&toy, "  let x = 1;", &mut a),
        tokenize_line(&toy, "  let x = 1;", &mut b)
    );
    assert_eq!(a, b);
    // Different histories converge once the block is closed.
    let mut c = StackState::default();
    tokenize_line(&toy, "x", &mut c);
    tokenize_line(&toy, "}", &mut a);
    assert_eq!(a, c);
    assert_ne!(a, StackState::default());
    // A state from another grammar starts over.
    let rust = registry.grammar("source.rust-lite").unwrap();
    let tokens = tokenize_line(&rust, "let", &mut b);
    assert_eq!(
        tokens[0].scopes,
        ["source.rust-lite", "storage.modifier.rust-lite"]
    );
    // `\r\n` is not tokenized.
    let tokens = tokenize_line(&toy, "x\r\n", &mut c);
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].range, 0..1);
}

#[test]
fn grammars_are_shared_between_threads() {
    let toy = registry().grammar("source.toy").unwrap();
    let text = read("golden/sample.toy");
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let toy = Arc::clone(&toy);
            let text = text.clone();
            std::thread::spawn(move || {
                let mut state = StackState::default();
                text.split('\n')
                    .map(|line| tokenize_line(&toy, line, &mut state))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn unsupported_patterns_are_listed_and_skipped() {
    let grammar = Grammar::from_json(
        r#"{ "scopeName": "source.u", "patterns": [
            { "match": "\\b(\\w+)\\s+\\1\\b", "name": "invalid.repeated-word" },
            { "match": "(['\"]).*?\\1", "name": "string.quoted" },
            { "match": "\\w+", "name": "word" } ] }"#,
    )
    .unwrap();
    // A back-reference to a group with many possible texts is unsupported…
    let unsupported = grammar.unsupported_patterns();
    assert_eq!(unsupported.len(), 1);
    assert_eq!(unsupported[0].pattern, r"\b(\w+)\s+\1\b");
    assert!(
        unsupported[0].reason.contains("back-reference"),
        "{}",
        unsupported[0].reason
    );
    // …and skipped; one to a group with few texts (the quote) is expanded.
    let l = lines(&grammar, "the the 'a\" b' c");
    assert_eq!(scopes(&l[0], "the"), ["word"]);
    assert_eq!(scopes(&l[0], "'a\" b'"), ["string.quoted"]);
    assert_eq!(scopes(&l[0], "c"), ["word"]);
}

#[test]
fn scopes_map_to_highlight_tags() {
    let toy = registry().grammar("source.toy").unwrap();
    let text = "@inline\nfn add(a: Int) {\n  let s = \"x\" + 0x1F; // done\n}\nclass Point {}\nput(<<EOT\nEOT";
    let mut state = StackState::default();
    let mut tags = Vec::new();
    for line in text.split('\n') {
        for token in tokenize_line(&toy, line, &mut state) {
            tags.push((line[token.range.clone()].to_string(), token.tag()));
        }
    }
    let tag = |piece: &str| {
        tags.iter()
            .find(|(text, _)| text == piece)
            .unwrap_or_else(|| panic!("{piece}"))
            .1
    };
    assert_eq!(tag("@"), Some("Decorator"));
    assert_eq!(tag("inline"), Some("Decorator"));
    assert_eq!(tag("fn"), Some("Keyword"));
    assert_eq!(tag("add"), Some("FunctionDefinition"));
    assert_eq!(tag("Int"), Some("ClassDefinition"));
    assert_eq!(tag("let"), Some("Keyword"));
    assert_eq!(tag("s"), Some("Variable"));
    assert_eq!(tag("\""), Some("String"));
    assert_eq!(tag("+"), Some("Operator"));
    assert_eq!(tag("0x1F"), Some("Number"));
    assert_eq!(tag("//"), Some("Comment"));
    assert_eq!(tag(" done"), Some("Comment"));
    assert_eq!(tag("Point"), Some("ClassDefinition"));
    assert_eq!(tag("put"), Some("Builtin"));
    assert_eq!(tag("EOT"), Some("Tag"));
    assert_eq!(tag("{"), None);

    let tag_of =
        |scopes: &[&str]| scope_to_tag(&scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(
        tag_of(&[
            "source.json",
            "meta.structure.dictionary.json",
            "support.type.property-name.json"
        ]),
        Some("Key")
    );
    assert_eq!(
        tag_of(&[
            "text.html.markdown",
            "meta.link.inline.markdown",
            "markup.underline.link.markdown"
        ]),
        Some("Link")
    );
    assert_eq!(
        tag_of(&[
            "text.html.markdown",
            "markup.heading.markdown",
            "entity.name.section.markdown"
        ]),
        Some("Heading")
    );
    assert_eq!(
        tag_of(&[
            "text.html.basic",
            "meta.tag.structure.div.start.html",
            "entity.name.tag.html"
        ]),
        Some("Tag")
    );
    assert_eq!(tag_of(&["source.toy"]), None);
}

#[test]
fn tag_names_are_rustpy_highlight_tags() {
    for name in TAG_NAMES {
        let tag: rustpy_highlight::HighlightTag =
            serde_json::from_value(serde_json::json!(name)).unwrap();
        assert_eq!(format!("{tag:?}"), name);
    }
}
