//! Grammar checking for comments, docstrings and prose, limited to
//! high-precision rules so code-heavy comments don't drown in noise:
//!
//! * `repeated-word` — "the the"
//! * `article` — "a apple" / "an car" (with pronunciation exceptions)
//! * `could-of` — "could of" → "could have"
//! * `alot` and other common one-word mistakes
//! * `lowercase-i` — "i think" → "I think"
//! * `sentence-start` — a sentence after ". " starting in lowercase
//! * `space-before-punct` — "word ," → "word,"

pub mod policy;

use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// Byte range in the checked text.
    pub range: Range<usize>,
    pub rule: &'static str,
    pub message: String,
    pub replacements: Vec<String>,
}

#[derive(Clone, Debug)]
struct Word {
    start: usize,
    end: usize,
    text: String,
}

fn words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        let part = c.is_alphanumeric() || c == '\'' || c == '_';
        match (part, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push(Word {
                    start: s,
                    end: i,
                    text: text[s..i].trim_end_matches('\'').to_string(),
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(Word {
            start: s,
            end: text.len(),
            text: text[s..].to_string(),
        });
    }
    out
}

/// True when only whitespace (no newline-paragraph break, no punctuation) separates two words.
fn only_spaces_between(text: &str, a: &Word, b: &Word) -> bool {
    let gap = &text[a.end..b.start];
    !gap.is_empty() && gap.chars().all(|c| c == ' ' || c == '\t') && !gap.contains("  ")
}

fn is_code_like(w: &str) -> bool {
    w.contains('_')
        || w.chars().any(|c| c.is_ascii_digit())
        || (w.chars().skip(1).any(|c| c.is_uppercase()) && w.chars().any(|c| c.is_lowercase()))
}

/// "an" is right before a vowel *sound*.
fn wants_an(next: &str) -> Option<bool> {
    let lower = next.to_lowercase();
    let first = lower.chars().next()?;
    if !first.is_alphabetic() {
        return None;
    }
    // Acronyms are read letter by letter: an SQL, an HTTP, a URL.
    if next.len() >= 2 && next.chars().all(|c| c.is_ascii_uppercase()) {
        return Some(matches!(
            next.chars().next()?,
            'A' | 'E' | 'F' | 'H' | 'I' | 'L' | 'M' | 'N' | 'O' | 'R' | 'S' | 'X'
        ));
    }
    const CONSONANT_SOUND: &[&str] = &[
        "uni", "use", "usu", "uti", "ubi", "ure", "eu", "ewe", "one", "once", "ouija",
    ];
    const VOWEL_SOUND: &[&str] = &["hour", "honest", "honor", "honour", "heir"];
    if VOWEL_SOUND.iter().any(|p| lower.starts_with(p)) {
        return Some(true);
    }
    if CONSONANT_SOUND.iter().any(|p| lower.starts_with(p)) {
        return Some(false);
    }
    Some(matches!(first, 'a' | 'e' | 'i' | 'o' | 'u'))
}

const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "etc", "vs", "cf", "mr", "mrs", "dr", "no", "fig", "approx", "al",
];

/// Checks a run of prose.
pub fn check_prose(text: &str) -> Vec<Issue> {
    let mut out = Vec::new();
    let ws = words(text);
    for (i, w) in ws.iter().enumerate() {
        let lower = w.text.to_lowercase();
        if let Some(next) = ws.get(i + 1) {
            let next_lower = next.text.to_lowercase();
            // repeated-word
            if lower == next_lower
                && only_spaces_between(text, w, next)
                && lower.chars().all(char::is_alphabetic)
                && !matches!(
                    lower.as_str(),
                    "had" | "that" | "is" | "bye" | "very" | "ha" | "no" | "so" | "really"
                )
            {
                out.push(Issue {
                    range: w.start..next.end,
                    rule: "repeated-word",
                    message: format!("Duplicated word '{}'", w.text),
                    replacements: vec![w.text.clone()],
                });
            }
            // article
            if (lower == "a" || lower == "an")
                && only_spaces_between(text, w, next)
                && !is_code_like(&next.text)
            {
                if let Some(an) = wants_an(&next.text) {
                    let wrong = (lower == "a" && an) || (lower == "an" && !an);
                    if wrong {
                        let fixed = match (w.text.as_str(), an) {
                            ("A", true) => "An",
                            ("An" | "AN", false) => "A",
                            (_, true) => "an",
                            (_, false) => "a",
                        };
                        out.push(Issue {
                            range: w.start..w.end,
                            rule: "article",
                            message: format!("Use '{fixed}' before '{}'", next.text),
                            replacements: vec![fixed.to_string()],
                        });
                    }
                }
            }
            // could-of
            if matches!(
                lower.as_str(),
                "could" | "would" | "should" | "must" | "might"
            ) && next_lower == "of"
                && only_spaces_between(text, w, next)
            {
                out.push(Issue {
                    range: next.start..next.end,
                    rule: "could-of",
                    message: format!("Did you mean '{} have'?", w.text),
                    replacements: vec!["have".into()],
                });
            }
            // lowercase-i
            if w.text == "i"
                && only_spaces_between(text, w, next)
                && matches!(
                    next_lower.as_str(),
                    "am" | "have"
                        | "think"
                        | "was"
                        | "will"
                        | "would"
                        | "can"
                        | "could"
                        | "should"
                        | "guess"
                        | "believe"
                        | "hope"
                        | "need"
                        | "want"
                        | "know"
                        | "don't"
                        | "didn't"
                        | "do"
                )
            {
                out.push(Issue {
                    range: w.start..w.end,
                    rule: "lowercase-i",
                    message: "The pronoun 'I' is capitalized".into(),
                    replacements: vec!["I".into()],
                });
            }
        }
        if matches!(w.text.as_str(), "i'm" | "i've" | "i'd" | "i'll") {
            let fixed = format!("I{}", &w.text[1..]);
            out.push(Issue {
                range: w.start..w.end,
                rule: "lowercase-i",
                message: "The pronoun 'I' is capitalized".into(),
                replacements: vec![fixed],
            });
        }
        // single-word mistakes
        // Plain misspellings are left to the spell checker; these are usage mistakes.
        let fix = match lower.as_str() {
            "alot" => Some("a lot"),
            "irregardless" => Some("regardless"),
            _ => None,
        };
        if let Some(f) = fix {
            out.push(Issue {
                range: w.start..w.end,
                rule: "common-mistake",
                message: format!("Did you mean '{f}'?"),
                replacements: vec![f.to_string()],
            });
        }
        // sentence-start: "... end. next" with lowercase next word
        if i > 0 {
            let prev = &ws[i - 1];
            let gap = &text[prev.end..w.start];
            let first = w.text.chars().next().unwrap_or(' ');
            if (gap == ". " || gap == "! " || gap == "? ")
                && first.is_lowercase()
                && !is_code_like(&w.text)
                && !ABBREVIATIONS.contains(&prev.text.to_lowercase().as_str())
                && !(prev.text.len() == 1)
                && prev.text.chars().all(char::is_alphabetic)
                && !w.text.contains('.')
            {
                // Skip "foo.bar" (dotted names) — gap check above already requires ". ".
                let mut upper = first.to_uppercase().collect::<String>();
                upper.push_str(&w.text[first.len_utf8()..]);
                out.push(Issue {
                    range: w.start..w.end,
                    rule: "sentence-start",
                    message: "A sentence should start with a capital letter".into(),
                    replacements: vec![upper],
                });
            }
        }
    }
    // space-before-punct: "word ," / "word ."
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(b, b',' | b';' | b'!' | b'?')
            && i >= 2
            && bytes[i - 1] == b' '
            && bytes[i - 2].is_ascii_alphabetic()
        {
            let after_ok = i + 1 >= bytes.len() || bytes[i + 1] == b' ' || bytes[i + 1] == b'\n';
            if after_ok {
                out.push(Issue {
                    range: i - 1..i + 1,
                    rule: "space-before-punct",
                    message: format!("Remove the space before '{}'", b as char),
                    replacements: vec![(b as char).to_string()],
                });
            }
        }
    }
    out.sort_by_key(|i| i.range.start);
    out
}

/// Checks `#` comments and docstrings (triple-quoted strings) of Python source.
pub fn check_python(src: &str) -> Vec<Issue> {
    use rustpy_syntax::SyntaxKind;
    let mut out = Vec::new();
    for tok in rustpy_syntax::tokenize(src).0 {
        let r = tok.range;
        let (s, e) = (usize::from(r.start()), usize::from(r.end()));
        let text = &src[s..e];
        let (inner_start, inner) = match tok.kind {
            SyntaxKind::COMMENT => {
                let body = text.trim_start_matches('#');
                // Skip commented-out code and directives.
                let t = body.trim();
                if t.starts_with("noqa")
                    || t.starts_with("type:")
                    || t.starts_with("pragma")
                    || t.starts_with("TODO")
                    || t.ends_with(':')
                    || t.contains("()")
                    || t.contains(" = ")
                {
                    continue;
                }
                (s + (text.len() - body.len()), body)
            }
            SyntaxKind::STRING => {
                let prefix_len = text.find(['"', '\'']).unwrap_or(0);
                let rest = &text[prefix_len..];
                if !(rest.starts_with("\"\"\"") || rest.starts_with("'''"))
                    || text[..prefix_len].to_lowercase().contains('f')
                    || rest.len() < 6
                {
                    continue;
                }
                (s + prefix_len + 3, &rest[3..rest.len() - 3])
            }
            _ => continue,
        };
        for mut issue in check_prose(inner) {
            issue.range = issue.range.start + inner_start..issue.range.end + inner_start;
            out.push(issue);
        }
    }
    out
}

/// Checks Markdown prose, skipping fenced code blocks, inline code and link targets.
pub fn check_markdown(src: &str) -> Vec<Issue> {
    let mut masked: Vec<u8> = src.as_bytes().to_vec();
    let mut in_fence = false;
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        let fence = t.starts_with("```") || t.starts_with("~~~");
        if fence || in_fence || line.starts_with("    ") || line.starts_with('\t') {
            for b in &mut masked[offset..offset + line.len()] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
        }
        if fence {
            in_fence = !in_fence;
        }
        offset += line.len();
    }
    // Inline code spans and link targets.
    let mut i = 0;
    while i < masked.len() {
        if masked[i] == b'`' {
            if let Some(j) = masked[i + 1..].iter().position(|&b| b == b'`') {
                for b in &mut masked[i..=i + 1 + j] {
                    *b = b' ';
                }
                i += j + 2;
                continue;
            }
        }
        if masked[i] == b']' && masked.get(i + 1) == Some(&b'(') {
            if let Some(j) = masked[i + 2..].iter().position(|&b| b == b')') {
                for b in &mut masked[i + 1..=i + 2 + j] {
                    *b = b' ';
                }
                i += j + 3;
                continue;
            }
        }
        i += 1;
    }
    // Masking only replaces ASCII bytes with spaces, so the text stays valid UTF-8.
    let text = String::from_utf8(masked).unwrap_or_default();
    check_prose(&text)
}

/// Applies the first replacement of each issue (non-overlapping, back to front).
pub fn apply_first_fixes(text: &str, issues: &[Issue]) -> String {
    let mut out = text.to_string();
    let mut sorted: Vec<&Issue> = issues
        .iter()
        .filter(|i| !i.replacements.is_empty())
        .collect();
    sorted.sort_by_key(|i| std::cmp::Reverse(i.range.start));
    let mut last_start = usize::MAX;
    for i in sorted {
        if i.range.end <= last_start {
            out.replace_range(i.range.clone(), &i.replacements[0]);
            last_start = i.range.start;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Vec<&'static str> {
        check_prose(text).into_iter().map(|i| i.rule).collect()
    }

    #[test]
    fn repeated_words() {
        assert_eq!(rules("This is the the answer."), vec!["repeated-word"]);
        assert!(rules("I said that that was fine.").is_empty());
        assert!(
            rules("the\n\nthe").is_empty(),
            "paragraph breaks are not duplicates"
        );
    }

    #[test]
    fn articles() {
        assert_eq!(
            apply_first_fixes(
                "Take a apple and an car.",
                &check_prose("Take a apple and an car.")
            ),
            "Take an apple and a car."
        );
        assert!(
            rules("an hour, a user, a unique id, an SQL query, a URL, an HTTP call").is_empty()
        );
        assert_eq!(rules("a SQL query"), vec!["article"]);
        assert!(
            rules("a my_var value").is_empty(),
            "code-like words are skipped"
        );
    }

    #[test]
    fn could_of_and_pronoun_i() {
        let t = "i think we could of done it, i'm sure.";
        let fixed = apply_first_fixes(t, &check_prose(t));
        assert_eq!(fixed, "I think we could have done it, I'm sure.");
        assert!(rules("for i in range(3)").is_empty());
    }

    #[test]
    fn sentence_start_and_abbreviations() {
        assert_eq!(rules("Done. then we exit."), vec!["sentence-start"]);
        assert!(rules("Use tools, e.g. black or ruff.").is_empty());
        assert!(rules("See os.path for details.").is_empty());
    }

    #[test]
    fn space_before_punctuation_and_common_mistakes() {
        let t = "Hello , world. It happens alot.";
        assert_eq!(
            apply_first_fixes(t, &check_prose(t)),
            "Hello, world. It happens a lot."
        );
        assert!(rules("x = f(a , b)").len() <= 1);
    }

    #[test]
    fn python_comments_and_docstrings() {
        let src = "def f():\n    \"\"\"Return the the value.\"\"\"\n    x = 1  # this is a error\n    # y = a an_thing()\n    return x\n";
        let issues = check_python(src);
        let found: Vec<(&str, &str)> = issues
            .iter()
            .map(|i| (i.rule, &src[i.range.clone()]))
            .collect();
        assert_eq!(found, vec![("repeated-word", "the the"), ("article", "a")]);
    }

    #[test]
    fn markdown_skips_code() {
        let src = "# Title\n\nThis is a example.\n\n```\nthe the\n```\n\nRun `a apple` and [the the](http://x/the the).\n";
        let issues = check_markdown(src);
        assert_eq!(
            issues.iter().map(|i| i.rule).collect::<Vec<_>>(),
            vec!["article", "repeated-word"]
        );
        assert_eq!(&src[issues[0].range.clone()], "a");
    }
}
