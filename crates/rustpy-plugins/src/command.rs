//! The command syntax of External Tools, as the IDE reads a tool's program
//! and arguments (`rustpy-ui`, `external_tools::build_invocation`). No shell
//! is involved: whitespace separates words, `"…"` and `'…'` group, nothing
//! is special inside them (there are no escapes, so Windows paths keep their
//! backslashes), and quoted and unquoted parts next to each other form one
//! word (`"$PluginDir$"/bin/x`). The IDE then expands macros such as
//! `$FilePath$` inside each word, so a macro's value is never split or read
//! as syntax. `$PluginDir$` is different: it is filled in when the plugin
//! loads, so [`substitute`] writes its value in this syntax.
//!
//! The program is read as one word when it starts with a quote; otherwise
//! the IDE takes it as it is (also with quotes inside) unless it has spaces
//! and is not a file. [`quote`] writes a word that both rules read back
//! exactly.

/// Whether `c` separates words (outside quotes), as `char::is_whitespace`
/// does for the IDE.
fn separates(c: char) -> bool {
    c.is_whitespace()
}

/// Splits `text` into words the way the IDE does: whitespace separates,
/// `"…"` and `'…'` group, no escapes. Fails on a quote that is not closed
/// (the IDE would take the rest of the line as quoted text).
pub(crate) fn split_words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if separates(c) => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            None => {
                word.push(c);
                in_word = true;
            }
        }
    }
    match quote {
        None => {
            if in_word {
                words.push(word);
            }
            Ok(words)
        }
        Some(q) => Err(format!("a `{q}` quote is not closed")),
    }
}

/// Text that the IDE reads as exactly `word`, one word, wherever it stands:
/// `word` itself when it has no whitespace and no quotes, else quoted:
/// `"…"`, with every `"` in `'…'` (`a"b` is `"a"'"'"b"`).
pub(crate) fn quote(word: &str) -> String {
    if !word.is_empty() && !word.chars().any(|c| separates(c) || c == '"' || c == '\'') {
        return word.to_string();
    }
    let mut out = String::with_capacity(word.len() + 2);
    let mut open: Option<char> = None;
    for c in word.chars() {
        let wanted = if c == '"' { '\'' } else { '"' };
        if open != Some(wanted) {
            out.extend(open);
            out.push(wanted);
            open = Some(wanted);
        }
        out.push(c);
    }
    match open {
        Some(q) => out.push(q),
        None => out.push_str("\"\""),
    }
    out
}

/// Replaces every `token` in command `text` with `value`, written for the
/// place it stands so that the IDE reads exactly `value` there, whatever
/// characters it contains: [`quote`]d outside quotes; inside `"…"` as it is,
/// except that a `"` closes the quote, stands in `'…'` and opens it again
/// (and likewise a `'` inside `'…'`). So `$PluginDir$/bin/x`,
/// `"$PluginDir$/bin/x"` and `'$PluginDir$/bin/x'` all stay one word naming
/// the same file. An empty `value` outside quotes is no text at all.
/// Replacements are not searched for `token` again.
pub(crate) fn substitute(text: &str, token: &str, value: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quote_open: Option<char> = None;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if !token.is_empty() && rest.starts_with(token) {
            out.push_str(&match quote_open {
                None if value.is_empty() => String::new(),
                None => quote(value),
                Some(q) => {
                    let other = if q == '"' { '\'' } else { '"' };
                    value.replace(q, &format!("{q}{other}{q}{other}{q}"))
                }
            });
            rest = &rest[token.len()..];
            continue;
        }
        match quote_open {
            Some(q) if c == q => quote_open = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote_open = Some(c),
            None => {}
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        split_words(text).unwrap()
    }

    #[test]
    fn splits_like_the_ide() {
        assert_eq!(words("  check  --fix x.py "), ["check", "--fix", "x.py"]);
        assert_eq!(words(r#""a b" 'c d' e"#), ["a b", "c d", "e"]);
        assert_eq!(words(r#""$X$"/../.."#), ["$X$/../.."]);
        assert_eq!(words(r#"pre"mid"'post'"#), ["premidpost"]);
        assert_eq!(words(r#""it's" 'say "hi"'"#), ["it's", "say \"hi\""]);
        assert_eq!(words(r#""" ''"#), ["", ""]);
        // No escapes: backslashes stay (Windows paths, regular expressions).
        assert_eq!(
            words(r"C:\Tools\x.exe \d+ a\ b"),
            [r"C:\Tools\x.exe", r"\d+", r"a\", "b"]
        );
        assert_eq!(words(r#""a\" b"#), [r"a\", "b"]);
        // Any whitespace separates outside quotes, also a no-break space.
        assert_eq!(words("a\u{a0}b\tc"), ["a", "b", "c"]);
        assert_eq!(words("\"a\u{a0}b\""), ["a\u{a0}b"]);
        assert!(split_words("'open").unwrap_err().contains("not closed"));
        assert!(split_words("a \"b").unwrap_err().contains("not closed"));
    }

    #[test]
    fn quoted_words_read_back_exactly() {
        assert_eq!(
            quote("plain/path-1.2_x+y:z@w%v=u"),
            "plain/path-1.2_x+y:z@w%v=u"
        );
        assert_eq!(quote(r"C:\Users\me\x.exe"), r"C:\Users\me\x.exe");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("it's"), "\"it's\"");
        assert_eq!(quote("a\"b"), "\"a\"'\"'\"b\"");
        assert_eq!(quote("\"x\""), "'\"'\"x\"'\"'");
        assert_eq!(quote(""), "\"\"");
        for word in [
            "",
            "a",
            "a b",
            "it's \"odd\" $dir \\ `x`",
            "\"",
            "'",
            "\"\"''\"",
            "line\nbreak",
            "tab\there",
            "nbsp\u{a0}x",
            "ünïcode ✓",
        ] {
            assert_eq!(words(&quote(word)), [word], "{word:?} as {}", quote(word));
            // Next to other text it stays part of the same word.
            assert_eq!(
                words(&format!("--x={}/y", quote(word))),
                [format!("--x={word}/y")]
            );
        }
    }

    #[test]
    fn plugin_dir_is_written_for_its_place() {
        let value = "/home/o'brien/My \"Config\"/rustpy/plugins/com.x";
        let text = "$PluginDir$/bin/run.sh --a $PluginDir$/a.toml --b=\"$PluginDir$/b.toml\" '$PluginDir$/c' \"'$PluginDir$'\" '\"$PluginDir$\"'";
        let substituted = substitute(text, "$PluginDir$", value);
        assert_eq!(
            words(&substituted),
            [
                format!("{value}/bin/run.sh"),
                "--a".to_string(),
                format!("{value}/a.toml"),
                format!("--b={value}/b.toml"),
                format!("{value}/c"),
                format!("'{value}'"),
                format!("\"{value}\""),
            ],
            "{substituted}"
        );
        // A plain directory is written as it is.
        assert_eq!(
            substitute("$PluginDir$/x \"$PluginDir$\"", "$PluginDir$", "/p/q"),
            "/p/q/x \"/p/q\""
        );
        // Other macros and the rest of the text stay as written.
        assert_eq!(
            substitute("'$FilePath$' \\$PluginDir$", "$PluginDir$", "/p"),
            "'$FilePath$' \\/p"
        );
        assert_eq!(substitute("[$PluginDir$]", "$PluginDir$", ""), "[]");
        assert_eq!(
            substitute("$PluginDir$", "$PluginDir$", "$PluginDir$"),
            "$PluginDir$"
        );
    }
}
