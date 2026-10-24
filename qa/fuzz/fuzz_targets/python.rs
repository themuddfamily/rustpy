#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|source: &str| {
    // All editor sources, including unfinished text, must remain lossless.
    let _ = rustpy_syntax::lexer::tokenize(source);
    let parsed = rustpy_syntax::parse(source);
    assert_eq!(parsed.syntax_node().text().to_string(), source);
    // Formatter failure on incomplete syntax is permitted. Successful formatting
    // must remain syntactically valid and stable on a second pass.
    if parsed.errors().is_empty() {
        let options = rustpy_format::FormatOptions::default();
        if let Ok(formatted) = rustpy_format::format_source(source, &options) {
            assert!(rustpy_syntax::parse(&formatted).errors().is_empty());
            let before = rustpy_format::semantic_fingerprint(source);
            assert!(
                before.is_some(),
                "successful formatter input must have a semantic fingerprint"
            );
            assert_eq!(before, rustpy_format::semantic_fingerprint(&formatted));
            assert!(rustpy_format::comments_preserved(source, &formatted));
            let twice = rustpy_format::format_source(&formatted, &options).expect("second format");
            assert_eq!(formatted, twice);
        }
    }
});
