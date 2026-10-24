//! Conservative embedded-language formatting. Each engine changes layout only,
//! then checks its own lossless-token or structured semantic identity. No tool,
//! loader, entity resolver, database or YAML constructor is executed.
mod html;
mod sql;
mod yaml;

const MAX_INPUT: usize = 128 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_TOKENS: usize = 40_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Sql,
    Html,
    Yaml,
}
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub indent: usize,
    pub line_width: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            indent: 2,
            line_width: 100,
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid fragment syntax: {0}")]
    Syntax(String),
    #[error("fragment unchanged: {0}")]
    Unsafe(String),
    #[error("fragment formatting exceeds its {0} budget")]
    Budget(&'static str),
}
pub fn format(language: Language, source: &str, options: Options) -> Result<String, Error> {
    if source.len() > MAX_INPUT {
        return Err(Error::Budget("128 KiB input"));
    }
    if source.contains('\0') {
        return Err(Error::Syntax("NUL is unsupported".into()));
    }
    let options = Options {
        indent: options.indent.clamp(1, 16),
        line_width: options.line_width.clamp(40, 4096),
    };
    let output = match language {
        Language::Sql => sql::format(source, options)?,
        Language::Html => html::format(source, options)?,
        Language::Yaml => yaml::format(source)?,
    };
    if output.len() > MAX_OUTPUT {
        return Err(Error::Budget("1 MiB output"));
    }
    Ok(output)
}
fn checked_growth(output: &str) -> Result<(), Error> {
    if output.len() > MAX_OUTPUT {
        Err(Error::Budget("1 MiB output"))
    } else {
        Ok(())
    }
}
fn newline(source: &str) -> &'static str {
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            return if index > 0 && source.as_bytes()[index - 1] == b'\r' {
                "\r\n"
            } else {
                "\n"
            };
        }
        if byte == b'\r' {
            return if source.as_bytes().get(index + 1) == Some(&b'\n') {
                "\r\n"
            } else {
                "\r"
            };
        }
    }
    "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    fn formatted(language: Language, source: &str) -> String {
        let output = format(language, source, Options::default()).unwrap();
        assert_eq!(
            format(language, &output, Options::default()).unwrap(),
            output
        );
        output
    }
    #[test]
    fn sql_layout_preserves_literals_comments_and_parameters() {
        let source = "select a,b from things where a=1 and note=U&'d\\0061t' -- keep\n and body=$tag$a  > b$tag$";
        let output = formatted(Language::Sql, source);
        assert!(output.contains("a, b"));
        assert!(output.contains("a = 1"));
        assert!(output.contains("U&'d\\0061t'"));
        assert!(output.contains("-- keep\n"));
        assert!(output.contains("$tag$a  > b$tag$"));
        assert!(format(Language::Sql, "select 'unterminated", Options::default()).is_err());
        assert!(format(Language::Sql, "DELIMITER $$", Options::default()).is_err());
    }
    #[test]
    fn html_only_changes_tag_trivia_not_dom_text_or_raw_text() {
        let source = "<div  class = \"π\"> a  <span title = 'a > b'> b </span><script>const x = '<b>';  </script><img src=x /></div>";
        let output = formatted(Language::Html, source);
        assert_eq!(output, "<div class=\"π\"> a  <span title='a > b'> b </span><script>const x = '<b>';  </script><img src=x /></div>");
        assert_eq!(formatted(Language::Html, "<img src=x/>"), "<img src=x/>");
        assert!(format(
            Language::Html,
            "<div title='unfinished>",
            Options::default()
        )
        .is_err());
        assert!(format(Language::Html, "<p>{{ template }}</p>", Options::default()).is_err());
    }
    #[test]
    fn yaml_preserves_unicode_aliases_block_scalars_and_duplicate_keys() {
        let source = "π: { first: [ 1 , 2 ], second: 'a:  b' }\nanchor: &x [3,4]\ncopy: *x\nπ: |\n  keep:  spaces\n";
        let output = formatted(Language::Yaml, source);
        assert_ne!(output, source);
        assert!(output.contains("[1, 2]"));
        assert!(output.contains("'a:  b'"));
        assert!(output.contains("  keep:  spaces\n"));
        assert!(format(Language::Yaml, "key: [unfinished", Options::default()).is_err());
    }
}
