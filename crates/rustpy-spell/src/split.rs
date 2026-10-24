//! Splitting identifiers into words: `camelCase`, `PascalCase`, `snake_case`,
//! `SCREAMING_CASE`, acronym humps (`parseHTTPResponse` → `parse`, `HTTP`,
//! `Response`), plural acronyms (`getURLs` → `get`, `URLs`) and digits
//! (`utf8` → `utf`).

use std::ops::Range;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Class {
    Lower,
    Upper,
    Digit,
    Other,
}

fn classify(c: char) -> Class {
    if c.is_uppercase() {
        Class::Upper
    } else if c.is_lowercase() {
        Class::Lower
    } else if c.is_numeric() {
        Class::Digit
    } else if c.is_alphabetic() {
        // Caseless scripts (CJK, …) behave like lowercase letters.
        Class::Lower
    } else {
        Class::Other
    }
}

/// Byte ranges (into `ident`) of the word parts of an identifier, in order.
///
/// Underscores, digits and any other non-letter characters separate parts and
/// are not part of any range; case changes split camel humps. The parts keep
/// their original spelling, so `split_identifier_ranges("getHTTPResponse2x")`
/// yields the ranges of `get`, `HTTP`, `Response` and `x`.
pub fn split_identifier_ranges(ident: &str) -> Vec<Range<usize>> {
    let chars: Vec<(usize, char, Class)> = ident
        .char_indices()
        .map(|(i, c)| (i, c, classify(c)))
        .collect();
    let mut out = Vec::new();
    // Index (into `chars`) of the first character of the part being built.
    let mut start: Option<usize> = None;
    for i in 0..chars.len() {
        let (offset, _, class) = chars[i];
        match class {
            Class::Digit | Class::Other => {
                if let Some(s) = start.take() {
                    out.push(chars[s].0..offset);
                }
            }
            Class::Lower => {
                if start.is_none() {
                    start = Some(i);
                }
            }
            Class::Upper => match start {
                None => start = Some(i),
                Some(s) => {
                    let prev = chars[i - 1].2;
                    let next_is_lower = chars.get(i + 1).is_some_and(|c| c.2 == Class::Lower);
                    // A plural acronym (`URLs`, `IDs`) keeps its `s`.
                    let plural = chars.get(i + 1).is_some_and(|c| c.1 == 's')
                        && chars.get(i + 2).is_none_or(|c| c.2 != Class::Lower);
                    // `getValue` → `get` | `Value`; `HTMLParser` → `HTML` | `Parser`.
                    if prev == Class::Lower || (prev == Class::Upper && next_is_lower && !plural) {
                        out.push(chars[s].0..offset);
                        start = Some(i);
                    }
                }
            },
        }
    }
    if let Some(s) = start {
        out.push(chars[s].0..ident.len());
    }
    out
}

/// Splits an identifier into its words, keeping their original case:
/// `camelCase`, `PascalCase`, `snake_case`, `SCREAMING_CASE`, acronym humps
/// and digits are all honoured (`parse_json2xml` → `parse`, `json`, `xml`).
pub fn split_identifier(ident: &str) -> Vec<String> {
    split_identifier_ranges(ident)
        .into_iter()
        .map(|r| ident[r].to_string())
        .collect()
}

/// Returns true when the part at `range` directly touches a digit in `text`
/// (`utf8`, `x86`, `md5sum`): such parts are treated as code, not prose.
pub(crate) fn touches_digit(text: &str, range: &Range<usize>) -> bool {
    let before = text[..range.start]
        .chars()
        .next_back()
        .is_some_and(char::is_numeric);
    let after = text[range.end..]
        .chars()
        .next()
        .is_some_and(char::is_numeric);
    before || after
}

/// True when `word` is written entirely in uppercase letters (`HTTP`, `ID`).
pub(crate) fn is_all_caps(word: &str) -> bool {
    let mut letters = 0;
    for c in word.chars() {
        if c.is_lowercase() {
            return false;
        }
        if c.is_uppercase() {
            letters += 1;
        }
    }
    letters > 0
}

/// True for acronyms: all-caps words (`HTTP`) and their plurals (`URLs`).
pub(crate) fn is_acronym(word: &str) -> bool {
    if is_all_caps(word) {
        return true;
    }
    word.strip_suffix('s')
        .is_some_and(|stem| stem.chars().count() >= 2 && stem.chars().all(char::is_uppercase))
}

/// True when `word` looks like a hexadecimal digest written with letters only
/// (`deadbeef`, `cafebabe`): eight or more characters, all in `a`-`f`.
pub(crate) fn looks_like_hex(word: &str) -> bool {
    word.len() >= 8
        && word
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_digit())
}

/// Byte offset and length of the first apostrophe (`'` or `’`) in `word`.
pub(crate) fn find_apostrophe(word: &str) -> Option<(usize, usize)> {
    word.char_indices()
        .find(|&(_, c)| matches!(c, '\'' | '’'))
        .map(|(i, c)| (i, c.len_utf8()))
}

/// What follows the apostrophe of a word such as `user's` or `isn't`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Contraction {
    /// `'s`, `'re`, `'ll`, `'ve`, `'d`, `'m`: only the part before matters.
    Suffix,
    /// `n't`: the part before (`isn`, `can`, `won`) ends with the `n`.
    Negation,
    /// Anything else (`o'clock`, `rock'n'roll`): both parts are words.
    Other,
}

/// Classifies the apostrophe between `head` and `tail` (`tail` may hold more
/// apostrophes; it is never copied, so long runs stay linear).
pub(crate) fn contraction(head: &str, tail: &str) -> Contraction {
    let is = |suffix: &str| tail.eq_ignore_ascii_case(suffix);
    if ["s", "re", "ll", "ve", "d", "m"].into_iter().any(is) {
        Contraction::Suffix
    } else if is("t") && head.ends_with(['n', 'N']) {
        Contraction::Negation
    } else {
        Contraction::Other
    }
}

/// Re-cases `word` (normally lowercase) like `template`: `Recieve` →
/// `Receive`, `RECIEVE` → `RECEIVE`, `recieve` → `receive`.
pub fn match_case(template: &str, word: &str) -> String {
    let mut chars = template.chars();
    let Some(first) = chars.next() else {
        return word.to_string();
    };
    if !first.is_uppercase() {
        return word.to_string();
    }
    let rest: Vec<char> = chars.collect();
    let all_caps = !rest.is_empty()
        && rest.iter().all(|c| !c.is_lowercase())
        && rest.iter().any(|c| c.is_uppercase());
    if all_caps {
        return word.to_uppercase();
    }
    let mut out = String::with_capacity(word.len());
    let mut wc = word.chars();
    if let Some(c) = wc.next() {
        out.extend(c.to_uppercase());
    }
    out.extend(wc);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_camel_and_pascal_case() {
        assert_eq!(split_identifier("getValue"), ["get", "Value"]);
        assert_eq!(
            split_identifier("HttpRequestHandler"),
            ["Http", "Request", "Handler"]
        );
        assert_eq!(split_identifier("iPhone"), ["i", "Phone"]);
        assert_eq!(split_identifier("value"), ["value"]);
        assert_eq!(split_identifier("Value"), ["Value"]);
    }

    #[test]
    fn splits_acronym_humps() {
        assert_eq!(
            split_identifier("parseHTTPResponse"),
            ["parse", "HTTP", "Response"]
        );
        assert_eq!(split_identifier("HTMLParser"), ["HTML", "Parser"]);
        assert_eq!(split_identifier("getID"), ["get", "ID"]);
        assert_eq!(split_identifier("IOError"), ["IO", "Error"]);
        assert_eq!(
            split_identifier("XMLHttpRequest"),
            ["XML", "Http", "Request"]
        );
        assert_eq!(split_identifier("HTTPSServer"), ["HTTPS", "Server"]);
    }

    #[test]
    fn keeps_plural_acronyms_together() {
        assert_eq!(split_identifier("getURLs"), ["get", "URLs"]);
        assert_eq!(split_identifier("IDsAndNames"), ["IDs", "And", "Names"]);
        assert_eq!(split_identifier("all_APIs"), ["all", "APIs"]);
        assert_eq!(split_identifier("URLs2"), ["URLs"]);
        // Not a plural: the `s` starts a longer lowercase hump.
        assert_eq!(split_identifier("HTMLsplit"), ["HTM", "Lsplit"]);
    }

    #[test]
    fn splits_snake_and_screaming_case() {
        assert_eq!(split_identifier("recieve_data"), ["recieve", "data"]);
        assert_eq!(split_identifier("MAX_RETRIES"), ["MAX", "RETRIES"]);
        assert_eq!(split_identifier("__init__"), ["init"]);
        assert_eq!(split_identifier("_private__name_"), ["private", "name"]);
        assert_eq!(
            split_identifier("kebab-case-name"),
            ["kebab", "case", "name"]
        );
    }

    #[test]
    fn digits_separate_parts() {
        assert_eq!(split_identifier("utf8"), ["utf"]);
        assert_eq!(split_identifier("parse_json2xml"), ["parse", "json", "xml"]);
        assert_eq!(split_identifier("md5sum"), ["md", "sum"]);
        assert_eq!(split_identifier("123"), Vec::<String>::new());
        assert_eq!(split_identifier(""), Vec::<String>::new());
        assert_eq!(split_identifier("_"), Vec::<String>::new());
    }

    #[test]
    fn ranges_are_byte_offsets() {
        assert_eq!(
            split_identifier_ranges("getHTTPResponse2x"),
            vec![0..3, 3..7, 7..15, 16..17]
        );
        // Non-ASCII letters keep correct byte offsets.
        assert_eq!(split_identifier("naïveValue"), ["naïve", "Value"]);
        assert_eq!(split_identifier_ranges("éa_b"), vec![0..3, 4..5]);
        assert_eq!(split_identifier("数据Value"), ["数据", "Value"]);
        assert_eq!(split_identifier("ÉtéValeur"), ["Été", "Valeur"]);
    }

    #[test]
    fn digit_adjacency() {
        let text = "utf8 and x86 and md5sum";
        assert!(touches_digit(text, &(0..3)));
        assert!(!touches_digit(text, &(5..8)));
        assert!(touches_digit(text, &(9..10)));
        assert!(!touches_digit(text, &(13..16)));
        assert!(touches_digit(text, &(17..19)));
        assert!(touches_digit(text, &(20..23)));
    }

    #[test]
    fn caps_acronym_and_hex_detection() {
        assert!(is_all_caps("HTTP"));
        assert!(is_all_caps("ID"));
        assert!(!is_all_caps("Http"));
        assert!(!is_all_caps("http"));
        assert!(!is_all_caps(""));
        assert!(is_acronym("HTTP"));
        assert!(is_acronym("URLs"));
        assert!(is_acronym("IDs"));
        assert!(!is_acronym("Is"));
        assert!(!is_acronym("Https"));
        assert!(looks_like_hex("deadbeef"));
        assert!(looks_like_hex("CAFEBABEcafe"));
        assert!(!looks_like_hex("facade"));
        assert!(!looks_like_hex("deadbeefg"));
        assert!(!looks_like_hex("12345678"));
    }

    #[test]
    fn contractions() {
        assert_eq!(find_apostrophe("isn't"), Some((3, 1)));
        assert_eq!(find_apostrophe("don’t"), Some((3, 3)));
        assert_eq!(find_apostrophe("plain"), None);
        assert_eq!(contraction("user", "s"), Contraction::Suffix);
        assert_eq!(contraction("we", "RE"), Contraction::Suffix);
        assert_eq!(contraction("isn", "t"), Contraction::Negation);
        assert_eq!(contraction("o", "clock"), Contraction::Other);
        assert_eq!(contraction("bit", "t"), Contraction::Other);
    }

    #[test]
    fn case_matching() {
        assert_eq!(match_case("recieve", "receive"), "receive");
        assert_eq!(match_case("Recieve", "receive"), "Receive");
        assert_eq!(match_case("RECIEVE", "receive"), "RECEIVE");
        assert_eq!(match_case("R", "receive"), "Receive");
        assert_eq!(match_case("", "receive"), "receive");
        assert_eq!(match_case("rEcieve", "receive"), "receive");
    }
}
