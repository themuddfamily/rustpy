//! Finding the abbreviation that ends at the caret, and the bits of
//! document context `expand_at` needs (inside a tag? inside `<style>`?
//! which element encloses the caret? inside a CSS rule block?).

/// Characters an abbreviation may contain outside `[...]` and `{...}`.
fn is_abbr_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"#.*:$-_!@%^+>/".contains(&c)
}

/// Finds the abbreviation ending at byte `offset` of `text`, scanning back
/// on the current line: the result is `(start, abbreviation)` with the
/// abbreviation occupying `start..start + abbreviation.len()`.
///
/// Markup rules apply (they also cover typical CSS abbreviations such as
/// `m10`, `bd1-s-red` or `pos:a`): everything inside `[...]` and `{...}` is
/// taken (`a[title="x y"]`, `p{Hello world}`), scanning stops at whitespace
/// and other non-abbreviation characters, at the `>` that closes an HTML
/// tag (`<div>ul>li`), at the end of a template tag opened earlier on the
/// line (`{% if x %}`, `{{ name }}`, `{# note #}`), and leading `*+>^`
/// operators are dropped. When the editor has auto-inserted
/// closing brackets/quotes right after the caret (`p{Hello|}`), they are
/// included, so the abbreviation can extend past `offset`.
///
/// Returns `None` when there is nothing to expand (caret after whitespace,
/// unbalanced brackets) or `offset` is not a char boundary of `text`.
pub fn abbreviation_before(text: &str, offset: usize) -> Option<(usize, &str)> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut end = offset;
    if matches!(bytes.get(end), Some(b'"' | b'\'')) {
        end += 1;
    }
    while matches!(bytes.get(end), Some(b']' | b'}' | b')')) {
        end += 1;
    }
    if end > offset {
        // A bare `{...}` is a text node: in CSS it is the rule body, so prefer
        // the abbreviation inside it.
        if let Some(found) =
            scan_markup(text, end).filter(|(start, abbr)| *start < offset && !abbr.starts_with('{'))
        {
            return Some(found);
        }
    }
    scan_markup(text, offset)
}

/// Like [`abbreviation_before`] with stylesheet rules: only the characters
/// of CSS abbreviations (`a-z 0-9 # . : ! @ % - + $ _ /`) are taken, so
/// braces, brackets and parentheses always end the abbreviation, and the
/// result never extends past `offset`.
pub fn css_abbreviation_before(text: &str, offset: usize) -> Option<(usize, &str)> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut start = offset;
    while start > 0
        && (bytes[start - 1].is_ascii_alphanumeric() || b"#.:!@%-+$_/".contains(&bytes[start - 1]))
    {
        start -= 1;
    }
    let abbr = text[start..offset].trim_start_matches(['+', '/']);
    (!abbr.is_empty()).then(|| (offset - abbr.len(), abbr))
}

fn scan_markup(text: &str, pos: usize) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    let mut stack: Vec<u8> = Vec::new();
    let mut i = pos;
    while i > line_start {
        let c = bytes[i - 1];
        if stack.contains(&b'}') {
            // Inside a text block everything goes, nested braces balance.
            if c == b'}' {
                stack.push(c);
                i -= 1;
                continue;
            }
            if c != b'{' {
                i -= 1;
                continue;
            }
        }
        match c {
            b'}' if stack.is_empty() && closes_template_tag(text, line_start, i) => break,
            b']' | b'}' | b')' => stack.push(c),
            b'[' | b'{' | b'(' => {
                let close = match c {
                    b'[' => b']',
                    b'{' => b'}',
                    _ => b')',
                };
                if stack.pop() != Some(close) {
                    break;
                }
            }
            _ if stack.contains(&b']') => {}
            b'>' if closes_tag(text, i - 1) => break,
            _ if is_abbr_char(c) => {}
            _ => break,
        }
        i -= 1;
    }
    if !stack.is_empty() {
        return None;
    }
    let abbr = text[i..pos].trim_start_matches(['*', '+', '>', '^']);
    (!abbr.is_empty()).then(|| (pos - abbr.len(), abbr))
}

/// Whether the `}` just before byte `end` closes a template tag opened
/// earlier on the line: `{% if x %}`, `{{ name }}`, `{# note #}`. A text
/// block such as `p{50%}` has no opener and is not one.
fn closes_template_tag(text: &str, line_start: usize, end: usize) -> bool {
    if end < line_start + 2 {
        return false;
    }
    let opener = match text.as_bytes()[end - 2] {
        b'%' => "{%",
        b'}' => "{{",
        b'#' => "{#",
        _ => return false,
    };
    text[line_start..end - 2].contains(opener)
}

/// Whether the `>` at byte `gt` ends an HTML tag (`<div class="x">`,
/// `</p>`, `<br/>`), a comment (`-->`) or a processing instruction (`?>`).
fn closes_tag(text: &str, gt: usize) -> bool {
    let b = text.as_bytes();
    if (gt >= 1 && b[gt - 1] == b'?') || (gt >= 2 && &b[gt - 2..gt] == b"--") {
        return true;
    }
    let mut quote: Option<u8> = None;
    let mut i = gt;
    while i > 0 {
        i -= 1;
        let c = b[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            b'"' | b'\'' => quote = Some(c),
            b'>' => return false,
            b'<' => {
                let inner = &b[i + 1..gt];
                let inner = inner.strip_prefix(b"/").unwrap_or(inner);
                return inner
                    .first()
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'!');
            }
            _ => {}
        }
    }
    false
}

/// Whether `offset` is inside an unfinished tag (`<a href="|`), going by the
/// nearest `<` or `>` before it.
pub(crate) fn inside_tag(text: &str, offset: usize) -> bool {
    let b = text.as_bytes();
    let Some(i) = text[..offset].rfind(['<', '>']) else {
        return false;
    };
    b[i] == b'<'
        && b.get(i + 1)
            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'/' | b'!' | b'?'))
}

/// If `offset` lies in the content of a `<style>` or `<script>` element,
/// that element's name and the offset where its content starts.
pub(crate) fn raw_text_element(text: &str, offset: usize) -> Option<(&'static str, usize)> {
    let before = text[..offset].to_ascii_lowercase();
    let mut best: Option<(&'static str, usize)> = None;
    for tag in ["style", "script"] {
        let Some(open) = before.rfind(&format!("<{tag}")) else {
            continue;
        };
        let after_name = before.as_bytes().get(open + 1 + tag.len()).copied();
        if !matches!(after_name, Some(b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/')) {
            continue;
        }
        if before
            .rfind(&format!("</{tag}"))
            .is_some_and(|close| close > open)
        {
            continue;
        }
        let Some(gt) = before[open..].find('>') else {
            continue;
        };
        if best.is_none_or(|(_, start)| open > start) {
            best = Some((tag, open + gt + 1));
        }
    }
    best
}

#[rustfmt::skip]
const VOID: &[&str] = &[
    "area", "base", "basefont", "br", "col", "command", "embed", "frame", "hr", "img", "input", "isindex", "keygen",
    "link", "meta", "param", "source", "track", "wbr",
];

/// The innermost element still open at `offset` (lower-cased), from a
/// lightweight scan of the tags before it. Comments, doctypes, void and
/// self-closed elements are skipped; a closing tag pops back to its opener.
pub(crate) fn enclosing_element(text: &str, offset: usize) -> Option<String> {
    let b = &text.as_bytes()[..offset];
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &text[i..offset];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(b.len(), |j| i + j + 3);
            continue;
        }
        let closing = rest.starts_with("</");
        let name_start = i + if closing { 2 } else { 1 };
        let name_len = b[name_start..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b':' | b'_'))
            .count();
        if name_len == 0 || !b[name_start].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let name = text[name_start..name_start + name_len].to_ascii_lowercase();
        // Find the end of the tag, skipping quoted attribute values.
        let mut j = name_start + name_len;
        let mut quote: Option<u8> = None;
        while j < b.len() {
            match (quote, b[j]) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, c @ (b'"' | b'\'')) => quote = Some(c),
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        if j >= b.len() {
            break; // unterminated tag: the caret is inside it
        }
        let self_closed = b[j - 1] == b'/';
        if closing {
            if let Some(pos) = stack.iter().rposition(|n| *n == name) {
                stack.truncate(pos);
            }
        } else if !self_closed && !VOID.contains(&name.as_str()) {
            if name == "script" || name == "style" {
                // Raw text: skip to the matching close tag.
                let close = format!("</{name}");
                match text[j..offset].to_ascii_lowercase().find(&close) {
                    Some(k) => {
                        i = j + k;
                        continue;
                    }
                    None => return Some(name),
                }
            }
            stack.push(name);
        }
        i = j + 1;
    }
    stack.pop()
}

/// Unclosed `{` count in CSS source, ignoring comments and strings.
pub(crate) fn brace_depth(css: &str) -> usize {
    let b = css.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = css[i + 2..].find("*/").map_or(b.len(), |j| i + 2 + j + 2);
                continue;
            }
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q && b[i] != b'\n' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    depth
}

/// Whether a CSS abbreviation at `start` begins a declaration (only
/// whitespace since the previous `{`, `;`, `}` or comment) inside a rule
/// block of `css` (at-rules may also start at the top level). With
/// `indented` (Sass, Stylus) an abbreviation that opens an indented line
/// also qualifies, as rule bodies there need no braces.
pub(crate) fn css_declaration_start(css: &str, start: usize, abbr: &str, indented: bool) -> bool {
    let before = css[..start].trim_end();
    let at_start = before.is_empty() || before.ends_with(['{', ';', '}']) || before.ends_with("*/");
    if at_start && (abbr.starts_with('@') || brace_depth(&css[..start]) > 0) {
        return true;
    }
    let line_start = css[..start].rfind('\n').map_or(0, |i| i + 1);
    let lead = &css[line_start..start];
    indented
        && lead.bytes().all(|b| b == b' ' || b == b'\t')
        && (!lead.is_empty() || abbr.starts_with('@'))
}

/// Whether CSS text `before` an abbreviation (inside a `style` attribute
/// value) leaves it at the start of a declaration.
pub(crate) fn inline_declaration_start(before: &str) -> bool {
    let before = before.trim_end();
    before.is_empty() || before.ends_with(';')
}

/// If `offset` lies in the quoted value of a `style` attribute of an
/// unfinished tag (`<div style="m10|`), the offset where that value starts.
pub(crate) fn style_attribute_start(text: &str, offset: usize) -> Option<usize> {
    let b = text.as_bytes();
    let lt = text[..offset].rfind(['<', '>'])?;
    if b[lt] != b'<' || !b.get(lt + 1).is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    let is_space = |c: u8| c.is_ascii_whitespace() || c == b'/';
    let mut i = lt + 1;
    while i < offset && !is_space(b[i]) {
        i += 1; // the tag name
    }
    loop {
        while i < offset && is_space(b[i]) {
            i += 1;
        }
        let name_start = i;
        while i < offset && !is_space(b[i]) && !matches!(b[i], b'=' | b'"' | b'\'') {
            i += 1;
        }
        let name = &text[name_start..i];
        while i < offset && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= offset {
            return None;
        }
        if b[i] != b'=' {
            if i == name_start {
                i += 1; // a stray quote: skip it so the scan advances
            }
            continue; // a valueless attribute
        }
        i += 1;
        while i < offset && b[i].is_ascii_whitespace() {
            i += 1;
        }
        match b.get(i) {
            Some(&q @ (b'"' | b'\'')) if i < offset => {
                let value_start = i + 1;
                match text[value_start..offset].find(char::from(q)) {
                    Some(len) => i = value_start + len + 1,
                    None => return name.eq_ignore_ascii_case("style").then_some(value_start),
                }
            }
            _ => {
                while i < offset && !b[i].is_ascii_whitespace() {
                    i += 1; // an unquoted value
                }
            }
        }
    }
}

/// Whether `offset` is inside a template tag that is still open:
/// `{{ name|`, `{% if x|`, `{# note|` (Django, Jinja, Vue, Handlebars).
pub(crate) fn inside_template_tag(text: &str, offset: usize) -> bool {
    let before = &text[..offset];
    [("{{", "}}"), ("{%", "%}"), ("{#", "#}")]
        .iter()
        .any(|(open, close)| {
            before
                .rfind(open)
                .is_some_and(|i| !before[i + open.len()..].contains(close))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn before(text_with_caret: &str) -> Option<(usize, String)> {
        let caret = text_with_caret.find('|').expect("caret marker");
        let text = text_with_caret.replacen('|', "", 1);
        abbreviation_before(&text, caret).map(|(s, a)| (s, a.to_string()))
    }

    #[test]
    fn markup_extraction() {
        assert_eq!(before("  ul>li*3|"), Some((2, "ul>li*3".into())));
        assert_eq!(before("<div>ul>li|</div>"), Some((5, "ul>li".into())));
        assert_eq!(before("<p class=\"x\">a>b|"), Some((13, "a>b".into())));
        assert_eq!(
            before("x p{Hello, world}|"),
            Some((2, "p{Hello, world}".into()))
        );
        assert_eq!(
            before("a[title=\"a b\" href]|"),
            Some((0, "a[title=\"a b\" href]".into()))
        );
        assert_eq!(before("text >p|"), Some((6, "p".into())));
        assert_eq!(before("{% if x %}div|"), Some((10, "div".into())));
        assert_eq!(before("{{ name }}.x|"), Some((10, ".x".into())));
        assert_eq!(before("{# n #}p|"), Some((7, "p".into())));
        // Text blocks that merely end like a template tag.
        assert_eq!(
            before("p{Discount: 50%}|"),
            Some((0, "p{Discount: 50%}".into()))
        );
        assert_eq!(before("a{#}|"), Some((0, "a{#}".into())));
        assert_eq!(before("p{a {b}}|"), Some((0, "p{a {b}}".into())));
        assert_eq!(before("<!-- c -->ul|"), Some((10, "ul".into())));
        assert_eq!(before("line one\nhtml:5|"), Some((9, "html:5".into())));
        assert_eq!(
            before("(header>nav)+footer|"),
            Some((0, "(header>nav)+footer".into()))
        );
        assert_eq!(before("word |"), None);
        assert_eq!(before("|"), None);
        assert_eq!(before("a]|"), None);
    }

    #[test]
    fn auto_closed_brackets_after_caret() {
        // `p{Hello|}`: the auto-inserted `}` belongs to the abbreviation.
        let (start, abbr) = before("p{Hello|}").unwrap();
        assert_eq!((start, abbr.as_str()), (0, "p{Hello}"));
        let (start, abbr) = before("a[href=\"x|\"]").unwrap();
        assert_eq!((start, abbr.as_str()), (0, "a[href=\"x\"]"));
        // In a CSS rule body the braces are not part of the abbreviation.
        assert_eq!(before("a {m10|}"), Some((3, "m10".into())));
        // An unrelated `)` after the caret is ignored.
        assert_eq!(before("(see p|)"), Some((5, "p".into())));
    }

    #[test]
    fn css_extraction() {
        assert_eq!(abbreviation_before("a { m10", 7), Some((4, "m10")));
        assert_eq!(
            abbreviation_before("color: red; bd1-s-red", 21),
            Some((12, "bd1-s-red"))
        );
        assert_eq!(css_abbreviation_before("a{m10", 5), Some((2, "m10")));
        assert_eq!(css_abbreviation_before("a{m10}", 5), Some((2, "m10")));
        assert_eq!(css_abbreviation_before("  c#f00!", 8), Some((2, "c#f00!")));
        assert_eq!(css_abbreviation_before("x: ", 3), None);
    }

    #[test]
    fn rejects_bad_offsets() {
        assert_eq!(abbreviation_before("abc", 4), None);
        assert_eq!(abbreviation_before("é", 1), None);
        assert_eq!(css_abbreviation_before("é", 1), None);
    }

    #[test]
    fn document_context() {
        assert!(inside_tag("<a href=\"x", 10));
        assert!(!inside_tag("<p>x", 4));
        assert!(!inside_tag("plain", 5));
        let html = "<style>\na { }\n</style><ul>\n<li><!-- <p> -->x</li><br>";
        assert_eq!(enclosing_element(html, html.len()), Some("ul".into()));
        assert_eq!(
            enclosing_element("<div><p>a</p><img src=x>", 24),
            Some("div".into())
        );
        assert_eq!(enclosing_element("<ul><li/>", 9), Some("ul".into()));
        assert_eq!(enclosing_element("text", 4), None);
        let doc = "<style>\n  a { m";
        assert_eq!(raw_text_element(doc, doc.len()), Some(("style", 7)));
        assert_eq!(raw_text_element("<style></style>x", 16), None);
        assert_eq!(raw_text_element("<script src=x>", 14), Some(("script", 14)));
        assert_eq!(brace_depth("a { b { /* { */ \"{\" c"), 2);
        assert!(css_declaration_start("a {\n  m10", 6, "m10", false));
        assert!(css_declaration_start(
            "a { color: red; m10",
            16,
            "m10",
            false
        ));
        assert!(!css_declaration_start("m10", 0, "m10", false));
        assert!(!css_declaration_start("a { margin: 0 p", 14, "p", false));
        assert!(css_declaration_start("@m", 0, "@m", false));
        // Indented syntaxes: an indented line is a rule body.
        assert!(css_declaration_start(".a\n  m10", 5, "m10", true));
        assert!(!css_declaration_start(".a\n  m10", 5, "m10", false));
        assert!(!css_declaration_start("m10", 0, "m10", true));
        assert!(!css_declaration_start(".a\n  b: c m10", 10, "m10", true));
        assert!(inline_declaration_start(""));
        assert!(inline_declaration_start("color: red; "));
        assert!(!inline_declaration_start("margin: "));
    }

    #[test]
    fn style_attributes() {
        let at = |s: &str| {
            let caret = s.find('|').expect("caret marker");
            style_attribute_start(&s.replacen('|', "", 1), caret)
        };
        assert_eq!(at("<div style=\"m10|\">"), Some(12));
        assert_eq!(at("<p class=a STYLE='c:r; m|"), Some(18));
        assert_eq!(at("<a title=\"x\" style=\"c|"), Some(20));
        assert_eq!(at("<div title=\"m10|\">"), None);
        assert_eq!(at("<div style=\"x\" m|"), None);
        assert_eq!(at("<div hidden style=m|"), None);
        assert_eq!(at("<div \"\" style=\"m|"), Some(15));
        assert_eq!(at("<p>style=\"m|"), None);
        assert_eq!(at("plain|"), None);
    }

    #[test]
    fn template_tags() {
        assert!(inside_template_tag("{{ name", 7));
        assert!(inside_template_tag("<p>{% if x", 10));
        assert!(inside_template_tag("{# todo", 7));
        assert!(!inside_template_tag("{{ a }} b", 9));
        assert!(!inside_template_tag("{% if x %}p", 11));
        assert!(!inside_template_tag("p{x}", 4));
    }
}
