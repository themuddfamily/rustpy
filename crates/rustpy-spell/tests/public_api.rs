//! End-to-end use of the public API, as the IDE uses it.

use rustpy_spell::{
    add_user_word, check_plain_text, check_text, load_user_words, remove_user_word,
    split_identifier, CheckOptions, Dictionary, TextRange, TextSize, Typo, TypoKind,
};

fn slice<'a>(text: &'a str, typo: &Typo) -> &'a str {
    &text[usize::from(typo.range.start())..usize::from(typo.range.end())]
}

#[test]
fn identifiers_split_into_words() {
    assert_eq!(
        split_identifier("parseHTTPResponse"),
        ["parse", "HTTP", "Response"]
    );
    assert_eq!(
        split_identifier("recieve_data_v2"),
        ["recieve", "data", "v"]
    );
    assert_eq!(split_identifier("MAX_RETRIES"), ["MAX", "RETRIES"]);
}

#[test]
fn dictionary_lookups_and_suggestions() {
    let dict = Dictionary::bundled();
    assert!(dict.contains("Receive"));
    assert!(dict.contains("getUserName"));
    assert!(dict.contains("kwargs"));
    assert!(!dict.contains("recieve"));
    assert_eq!(dict.suggest("recieve", 3)[0], "receive");
    assert_eq!(dict.suggest("Recieve", 3)[0], "Receive");
}

#[test]
fn checks_a_python_file() {
    let src = "\
# Recieve the payload.
def handle_mesage(raw_bytes):
    \"\"\"Decode the paylaod; see https://exmaple.com/recieve for details.\"\"\"
    return raw_bytes.decode('utf8')  # noqa: recieve
";
    let dict = Dictionary::bundled();
    let typos = check_text(src, &dict, CheckOptions::default());
    let found: Vec<(&str, TypoKind)> = typos.iter().map(|t| (slice(src, t), t.kind)).collect();
    assert_eq!(
        found,
        [
            ("Recieve", TypoKind::Comment),
            ("mesage", TypoKind::Identifier),
            ("paylaod", TypoKind::String)
        ]
    );
    for typo in &typos {
        assert_eq!(slice(src, typo), typo.word);
        assert!(!typo.suggestions.is_empty(), "{typo:?}");
    }
    assert_eq!(
        typos[0].range,
        TextRange::new(TextSize::new(2), TextSize::new(9))
    );
    assert_eq!(typos[0].suggestions[0], "Receive");
    assert_eq!(typos[1].suggestions[0], "message");
    assert_eq!(typos[2].suggestions[0], "payload");
    // Options select what is checked.
    let comments_only = CheckOptions {
        check_identifiers: false,
        check_comments: true,
        check_strings: false,
        min_word_len: 4,
    };
    assert_eq!(check_text(src, &dict, comments_only).len(), 1);
}

#[test]
fn checks_a_commit_message() {
    let message = "Fix the recieve loop\n\nRun `pytest -k recieve` to check.\n\nSigned-off-by: A Contribtor <a@exmaple.com>\n";
    let typos = check_plain_text(message, &Dictionary::bundled());
    let words: Vec<&str> = typos.iter().map(|t| t.word.as_str()).collect();
    assert_eq!(words, ["recieve"]);
    assert_eq!(typos[0].kind, TypoKind::PlainText);
}

#[test]
fn user_dictionary_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spelling").join("user.dic");
    assert!(load_user_words(&path).unwrap().is_empty());
    assert!(add_user_word(&path, "Frobnicate").unwrap());
    assert!(!add_user_word(&path, "frobnicate").unwrap());
    let words = load_user_words(&path).unwrap();
    assert_eq!(words, ["Frobnicate"]);

    let src = "def frobnicate_all(): pass  # frobnicate everything\n";
    let bundled = Dictionary::bundled();
    assert_eq!(check_text(src, &bundled, CheckOptions::default()).len(), 2);
    let dict = Dictionary::bundled().with_user_words(&words);
    assert!(dict.contains("FROBNICATE"));
    assert!(check_text(src, &dict, CheckOptions::default()).is_empty());
    assert_eq!(dict.suggest("frobnicat", 1), ["Frobnicate"]);

    assert!(remove_user_word(&path, "FROBNICATE").unwrap());
    assert!(load_user_words(&path).unwrap().is_empty());
}
