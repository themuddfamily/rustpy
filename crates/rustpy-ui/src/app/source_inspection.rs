//! Test-only inspection of the unit-variant arms in `handle_action`.
//!
//! Comments and literals are opaque; delimiter groups isolate signatures,
//! guards and nested bodies. This deliberately supports the actual dispatch's
//! unit patterns and fails loudly if that shape changes.
use std::collections::HashSet;

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    Punct(u8),
    Arrow,
    Literal,
    Group(u8, Vec<Token<'a>>),
}

pub(super) fn handled_actions(source: &str) -> HashSet<String> {
    let mut offset = 0;
    let tokens = tokens(source, &mut offset, None);
    let mut functions = Vec::new();
    find_function(&tokens, &mut functions);
    assert_eq!(
        functions.len(),
        1,
        "expected exactly one handle_action function"
    );
    let matches: Vec<_> = functions[0]
        .windows(3)
        .filter_map(|window| match window {
            [Token::Word("match"), Token::Word("action"), Token::Group(b'{', arms)] => Some(arms),
            _ => None,
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected one top-level match action in handle_action"
    );
    arms(matches[0])
}

fn find_function<'a, 'b>(tokens: &'b [Token<'a>], functions: &mut Vec<&'b [Token<'a>]>) {
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token, Token::Word("fn"))
            && matches!(tokens.get(index + 1), Some(Token::Word("handle_action")))
        {
            assert!(
                matches!(tokens.get(index + 2), Some(Token::Group(b'(', _))),
                "handle_action signature changed"
            );
            let Some(Token::Group(b'{', body)) = tokens.get(index + 3) else {
                panic!("handle_action body/signature changed");
            };
            functions.push(body);
        }
        if let Token::Group(_, inner) = token {
            find_function(inner, functions);
        }
    }
}

fn arms(tokens: &[Token<'_>]) -> HashSet<String> {
    let mut result = HashSet::new();
    let mut at = 0;
    while at < tokens.len() {
        let arrow = tokens[at..]
            .iter()
            .position(|token| matches!(token, Token::Arrow))
            .map(|offset| at + offset)
            .expect("match arm missing =>");
        let pattern = &tokens[at..arrow];
        let guard = pattern
            .iter()
            .position(|token| matches!(token, Token::Word("if")));
        if let Some(guard) = guard {
            assert!(guard + 1 < pattern.len(), "empty match guard");
        }
        let pattern = &pattern[..guard.unwrap_or(pattern.len())];
        let catchall = matches!(pattern, [Token::Word(name)] if *name == "_" || name.as_bytes().first().is_some_and(u8::is_ascii_lowercase));
        if !catchall {
            let mut index = usize::from(matches!(pattern.first(), Some(Token::Punct(b'|'))));
            loop {
                // handle_action uses both imported variants and explicitly
                // qualified Action variants, including mixed or-patterns.
                if matches!(
                    pattern.get(index..index + 3),
                    Some([
                        Token::Word("Action"),
                        Token::Punct(b':'),
                        Token::Punct(b':')
                    ])
                ) {
                    index += 3;
                }
                let Some(Token::Word(name)) = pattern.get(index) else {
                    panic!("unsupported action pattern: {pattern:?}");
                };
                assert!(
                    name.as_bytes().first().is_some_and(u8::is_ascii_uppercase),
                    "unsupported action pattern: {pattern:?}"
                );
                result.insert((*name).to_owned());
                index += 1;
                if index == pattern.len() {
                    break;
                }
                assert!(
                    matches!(pattern.get(index), Some(Token::Punct(b'|'))),
                    "unsupported action pattern: {pattern:?}"
                );
                index += 1;
            }
        }
        at = arrow + 1;
        assert!(at < tokens.len(), "match arm missing body");
        if matches!(tokens[at], Token::Group(b'{', _)) {
            // Block arms may omit their comma; their nested tokens never count.
            at += 1;
            if matches!(tokens.get(at), Some(Token::Punct(b','))) {
                at += 1;
            }
        } else {
            // Calls, closures and other expressions end at a top-level comma.
            // Commas inside delimiter groups are opaque.
            let end = tokens[at..]
                .iter()
                .position(|token| matches!(token, Token::Punct(b',')))
                .map_or(tokens.len(), |offset| at + offset);
            assert!(end > at, "match arm missing body");
            assert!(
                !tokens[at..end]
                    .iter()
                    .any(|token| matches!(token, Token::Arrow)),
                "expression arm missing comma"
            );
            at = (end + 1).min(tokens.len());
        }
    }
    assert!(!result.is_empty(), "no explicit action arms found");
    result
}

fn tokens<'a>(source: &'a str, at: &mut usize, closing: Option<u8>) -> Vec<Token<'a>> {
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    while *at < bytes.len() {
        let start = *at;
        let byte = bytes[start];
        if byte.is_ascii_whitespace() {
            *at += 1;
            continue;
        }
        if bytes[start..].starts_with(b"//") {
            *at += 2;
            while *at < bytes.len() && bytes[*at] != b'\n' {
                *at += 1;
            }
            continue;
        }
        if bytes[start..].starts_with(b"/*") {
            *at += 2;
            let mut depth = 1;
            while *at < bytes.len() && depth > 0 {
                if bytes[*at..].starts_with(b"/*") {
                    depth += 1;
                    *at += 2;
                } else if bytes[*at..].starts_with(b"*/") {
                    depth -= 1;
                    *at += 2;
                } else {
                    *at += 1;
                }
            }
            assert_eq!(depth, 0, "unterminated block comment");
            continue;
        }
        if let Some(end) = literal_end(source, start) {
            *at = end;
            result.push(Token::Literal);
            continue;
        }
        if byte == b'_' || byte.is_ascii_alphabetic() {
            *at += 1;
            while *at < bytes.len() && (bytes[*at] == b'_' || bytes[*at].is_ascii_alphanumeric()) {
                *at += 1;
            }
            result.push(Token::Word(&source[start..*at]));
        } else if matches!(byte, b'(' | b'[' | b'{') {
            *at += 1;
            let close = match byte {
                b'(' => b')',
                b'[' => b']',
                _ => b'}',
            };
            result.push(Token::Group(byte, tokens(source, at, Some(close))));
        } else if matches!(byte, b')' | b']' | b'}') {
            assert_eq!(closing, Some(byte), "unbalanced Rust delimiter");
            *at += 1;
            return result;
        } else if bytes[start..].starts_with(b"=>") {
            *at += 2;
            result.push(Token::Arrow);
        } else {
            *at += 1;
            result.push(Token::Punct(byte));
        }
    }
    assert!(closing.is_none(), "unterminated Rust delimiter group");
    result
}

fn literal_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut at = start;
    if matches!(bytes[at], b'b' | b'c') {
        at += 1;
    }
    if bytes.get(at) == Some(&b'r') {
        at += 1;
        let hashes = at;
        while bytes.get(at) == Some(&b'#') {
            at += 1;
        }
        let hashes = at - hashes;
        if bytes.get(at) != Some(&b'"') {
            return None;
        }
        at += 1;
        while at < bytes.len() {
            if bytes[at] == b'"'
                && bytes
                    .get(at + 1..at + 1 + hashes)
                    .is_some_and(|tail| tail.iter().all(|b| *b == b'#'))
            {
                return Some(at + 1 + hashes);
            }
            at += 1;
        }
        panic!("unterminated raw string");
    }
    if bytes.get(at) == Some(&b'"') {
        at += 1;
        while at < bytes.len() {
            match bytes[at] {
                b'\\' => at += 2,
                b'"' => return Some(at + 1),
                _ => at += 1,
            }
        }
        panic!("unterminated string");
    }
    if bytes.get(at) == Some(&b'\'') {
        at += 1;
        if bytes.get(at) == Some(&b'\\') {
            at += 1;
            if bytes.get(at) == Some(&b'u') && bytes.get(at + 1) == Some(&b'{') {
                at += 2;
                while at < bytes.len() && bytes[at] != b'}' {
                    at += 1;
                }
                at += 1;
            } else if bytes.get(at) == Some(&b'x') {
                at += 3;
            } else {
                at += 1;
            }
        } else if at < bytes.len() {
            at += source[at..].chars().next().unwrap().len_utf8();
        }
        // A quote without a closing quote is a lifetime, not a char literal.
        if bytes.get(at) == Some(&b'\'') {
            return Some(at + 1);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inspect(body: &str) -> HashSet<String> {
        handled_actions(&format!("impl App {{ fn handle_action(&mut self, action: Action) {{ match action {{ {body} }} }} }}"))
    }
    #[test]
    fn compact_and_wrapped_alternatives_have_identical_coverage() {
        let compact = "Cut | Copy | Paste if enabled(action) => call(action), Save => {}, other => fallback(),";
        let wrapped = "| Cut\n | Copy /* layout */\n | Paste\n if\n enabled(action)\n => call(action),\n Save => {}\n other => fallback(),";
        let expected = HashSet::from(["Cut".into(), "Copy".into(), "Paste".into(), "Save".into()]);
        assert_eq!(inspect(compact), expected);
        assert_eq!(inspect(wrapped), expected);
        let qualified = "Action::Cut | Copy | Action::Paste if enabled(action) => call(action), Action::Save => {}, other => fallback(),";
        assert_eq!(inspect(qualified), expected);
    }
    #[test]
    fn comments_literals_nested_bodies_and_guards_do_not_invent_arms() {
        let body = r####"
            // FakeLine => {}
            /* FakeBlock => {} /* NestedComment => {} */ */
            Cut | /* comment across alternative */ Copy
                if matches!(action, FakeGuard | OtherGuard) && check("=> }", '{') => {
                match nested { FakeNested => {}, _ => {} }
                let normal = "FakeString => { escaped \" }";
                let raw = r###"FakeRaw => { \" # ##"###;
                let bytes = br#"FakeBytes => {}"#;
                let chars = ['}', '\'', '\u{7b}', b'{'];
                call(("comma, =>", ['{', '}']), || { FakeClosure; });
            }
            Save => call(|| { match x { OtherNested => {}, _ => {} } }, ","),
            _ => { let ignored = "FakeCatchall => {}"; }
        "####;
        assert_eq!(
            inspect(body),
            HashSet::from(["Cut".into(), "Copy".into(), "Save".into()])
        );
    }
    #[test]
    fn function_boundaries_ignore_fake_source_and_other_methods() {
        let source = r###"
            const FAKE: &str = "fn handle_action() { match action { Fake => {} } }";
            impl App<'static> {
                fn before(&self) { match action { Before => {} } }
                fn handle_action<'a>(&'a mut self, action: Action) {
                    match action { Real => {}, _ => {} }
                }
                fn after(&self) { match action { After => {} } }
            }
        "###;
        // Actual handle_action has no generic parameter list; fail rather than
        // accidentally looking at a neighbouring method's body.
        assert!(std::panic::catch_unwind(|| handled_actions(source)).is_err());
        assert_eq!(
            handled_actions(&source.replace("handle_action<'a>", "handle_action")),
            HashSet::from(["Real".into()])
        );
    }
    #[test]
    fn changed_or_malformed_dispatch_fails_instead_of_reporting_coverage() {
        for source in [
            "fn other() { match action { Cut => {} } }",
            "fn handle_action() { match different { Cut => {} } }",
            "fn handle_action() { match action { _ => {} } }",
            "fn handle_action() { match action { other => {} } }",
            "fn handle_action() { match action { Other::Cut => {} } }",
            "fn handle_action() { match action { Action::Cut(value) => {} } }",
            "fn handle_action() { match action { Action:: => {} } }",
            "fn handle_action() { match action { Cut(value) => {} } }",
            "fn handle_action() { match action { Cut => call() Copy => call(), } }",
            "fn handle_action() { match action { Cut => { /* unclosed } } }",
            "fn handle_action() { match action { Cut => { let s = \"unclosed; } } }",
        ] {
            assert!(
                std::panic::catch_unwind(|| handled_actions(source)).is_err(),
                "{source}"
            );
        }
    }
    #[test]
    fn actual_dispatch_includes_every_wrapped_debugger_alternative() {
        let handled = handled_actions(include_str!("../app.rs"));
        for name in [
            "ViewBreakpoints",
            "RunToCursor",
            "JumpToCursor",
            "SmartStepInto",
            "ShowExecutionPoint",
            "QuickEvaluateExpression",
            "ToggleTemporaryLineBreakpoint",
            "MuteBreakpoints",
            "RestartDebug",
        ] {
            assert!(handled.contains(name), "missing {name}");
        }
    }
}
