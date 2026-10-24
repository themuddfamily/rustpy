//! Mapping TextMate scopes to rustpy-highlight tag names.

/// Every tag name [`scope_to_tag`] can return: names of
/// `rustpy_highlight::HighlightTag` variants.
pub const TAG_NAMES: [&str; 14] = [
    "Keyword",
    "String",
    "Comment",
    "Number",
    "FunctionDefinition",
    "ClassDefinition",
    "Decorator",
    "Builtin",
    "Operator",
    "Tag",
    "Key",
    "Variable",
    "Link",
    "Heading",
];

/// Standard TextMate scope prefixes, most specific first. A prefix matches
/// a scope equal to it or continuing with `.` (`keyword` matches
/// `keyword.control.rust`, not `keywords`).
const RULES: &[(&str, &str)] = &[
    // Decorators and annotations, before the generic entity/punctuation
    // and storage rules.
    ("meta.decorator", "Decorator"),
    ("entity.name.function.decorator", "Decorator"),
    ("punctuation.definition.decorator", "Decorator"),
    ("storage.type.annotation", "Decorator"),
    ("punctuation.definition.annotation", "Decorator"),
    // Word-like operators read as keywords (`new`, `typeof`, `sizeof`, …).
    ("keyword.operator.new", "Keyword"),
    ("keyword.operator.expression", "Keyword"),
    ("keyword.operator.word", "Keyword"),
    ("keyword.operator.wordlike", "Keyword"),
    ("keyword.operator.sizeof", "Keyword"),
    ("keyword.operator", "Operator"),
    ("keyword", "Keyword"),
    // `fn`, `class`, `pub`, `static` … are themed like keywords.
    ("storage", "Keyword"),
    ("constant.numeric", "Number"),
    // `true`, `false`, `null`, `None`.
    ("constant.language", "Keyword"),
    ("string", "String"),
    ("comment", "Comment"),
    ("entity.name.function", "FunctionDefinition"),
    ("entity.name.type", "ClassDefinition"),
    ("entity.name.class", "ClassDefinition"),
    ("entity.name.struct", "ClassDefinition"),
    ("entity.name.enum", "ClassDefinition"),
    ("entity.name.trait", "ClassDefinition"),
    ("entity.name.interface", "ClassDefinition"),
    ("entity.name.tag", "Tag"),
    ("support.type.property-name", "Key"),
    ("entity.other.attribute-name", "Key"),
    ("meta.object-literal.key", "Key"),
    ("support", "Builtin"),
    ("variable", "Variable"),
    ("markup.underline.link", "Link"),
    ("markup.heading", "Heading"),
    ("entity.name.section", "Heading"),
];

/// Whether `scope` is `prefix` or a sub-scope of it.
fn has_prefix(scope: &str, prefix: &str) -> bool {
    scope
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// The rustpy-highlight tag name for a token's scopes (outermost first, as
/// in [`crate::Token::scopes`]), or `None` for plain text.
///
/// The innermost scope with a known meaning decides, so punctuation inside
/// a string is `String` and an interpolated variable inside it `Variable`.
/// Besides the standard prefixes (`keyword.`, `string.`, `comment.`,
/// `constant.numeric`, `entity.name.function`, `entity.name.type`,
/// `meta.decorator`, `support.`, `keyword.operator`, `entity.name.tag`,
/// `support.type.property-name`, `variable.`, `markup.underline.link`,
/// `markup.heading`) a few common ones are recognized: `storage.` and
/// `constant.language` read as keywords, word operators such as
/// `keyword.operator.new` too, decorator punctuation and annotations as
/// decorators, other `entity.name.<kind>` type names as classes, attribute
/// and object keys as keys, and `entity.name.section` as a heading.
pub fn scope_to_tag(scopes: &[String]) -> Option<&'static str> {
    scopes.iter().rev().find_map(|scope| {
        RULES
            .iter()
            .find(|(prefix, _)| has_prefix(scope, prefix))
            .map(|(_, tag)| *tag)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(scopes: &[&str]) -> Option<&'static str> {
        scope_to_tag(&scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn standard_prefixes() {
        assert_eq!(
            tag(&["source.rust", "keyword.control.rust"]),
            Some("Keyword")
        );
        assert_eq!(
            tag(&["source.rust", "keyword.operator.arithmetic.rust"]),
            Some("Operator")
        );
        assert_eq!(
            tag(&["source.js", "keyword.operator.new.js"]),
            Some("Keyword")
        );
        assert_eq!(
            tag(&["source.rust", "string.quoted.double.rust"]),
            Some("String")
        );
        assert_eq!(
            tag(&["source.rust", "comment.line.double-slash.rust"]),
            Some("Comment")
        );
        assert_eq!(
            tag(&["source.rust", "constant.numeric.decimal.rust"]),
            Some("Number")
        );
        assert_eq!(
            tag(&["source.rust", "entity.name.function.rust"]),
            Some("FunctionDefinition")
        );
        assert_eq!(
            tag(&["source.rust", "entity.name.type.struct.rust"]),
            Some("ClassDefinition")
        );
        assert_eq!(
            tag(&["source.python", "meta.decorator.python"]),
            Some("Decorator")
        );
        assert_eq!(
            tag(&["source.python", "entity.name.function.decorator.python"]),
            Some("Decorator")
        );
        assert_eq!(
            tag(&["source.python", "support.function.builtin.python"]),
            Some("Builtin")
        );
        assert_eq!(
            tag(&["text.html.basic", "entity.name.tag.html"]),
            Some("Tag")
        );
        assert_eq!(
            tag(&["source.json", "support.type.property-name.json"]),
            Some("Key")
        );
        assert_eq!(
            tag(&["source.shell", "variable.other.normal.shell"]),
            Some("Variable")
        );
        assert_eq!(
            tag(&["text.html.markdown", "markup.underline.link.markdown"]),
            Some("Link")
        );
        assert_eq!(
            tag(&["text.html.markdown", "markup.heading.markdown"]),
            Some("Heading")
        );
        assert_eq!(tag(&["source.rust", "storage.type.rust"]), Some("Keyword"));
        assert_eq!(
            tag(&["source.python", "constant.language.python"]),
            Some("Keyword")
        );
    }

    #[test]
    fn innermost_meaningful_scope_wins() {
        let s = [
            "source.rust",
            "string.quoted.double.rust",
            "punctuation.definition.string.begin.rust",
        ];
        assert_eq!(tag(&s), Some("String"));
        let s = [
            "source.js",
            "string.template.js",
            "meta.template.expression.js",
            "variable.other.readwrite.js",
        ];
        assert_eq!(tag(&s), Some("Variable"));
        let s = [
            "text.html.markdown",
            "markup.heading.markdown",
            "entity.name.section.markdown",
        ];
        assert_eq!(tag(&s), Some("Heading"));
        let s = [
            "source.c",
            "comment.block.c",
            "punctuation.definition.comment.c",
        ];
        assert_eq!(tag(&s), Some("Comment"));
    }

    #[test]
    fn unknown_and_partial_segments() {
        assert_eq!(tag(&["source.rust"]), None);
        assert_eq!(tag(&[]), None);
        assert_eq!(
            tag(&[
                "source.x",
                "meta.block.x",
                "punctuation.section.block.begin.x"
            ]),
            None
        );
        assert_eq!(tag(&["source.x", "keywords.x"]), None);
        assert_eq!(tag(&["source.x", "strings"]), None);
        assert_eq!(tag(&["source.x", "keyword"]), Some("Keyword"));
        assert_eq!(tag(&["source.x", "invalid.illegal.x"]), None);
    }

    #[test]
    fn every_rule_maps_to_a_listed_tag() {
        for (prefix, tag) in RULES {
            assert!(TAG_NAMES.contains(tag), "{prefix} -> {tag}");
        }
    }
}
