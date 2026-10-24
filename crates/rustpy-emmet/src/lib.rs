//! RustPy IDE — Emmet abbreviation expansion for HTML and CSS (roadmap §1.7
//! "Emmet"). Pure logic, no dependencies.
//!
//! * [`expand_html`] expands markup abbreviations: elements, `#id`,
//!   `.class`, `[attr=value]`, `{text}`, `>` `+` `^` operators, `*N`
//!   repetition with `$` numbering (`$$` padding, `@-` reverse, `@N` start),
//!   `(...)` groups, implicit tag names, `lorem`/`loremN` filler text and
//!   snippets (`!`, `html:5`, `link:css`, `input:email`, `btn`, ...). Output
//!   is pretty-printed with the caret at the first edit point.
//! * [`expand_css`] expands stylesheet abbreviations with the Emmet CSS
//!   vocabulary: `m10` -> `margin: 10px;`, `bd1-s-red` -> `border: 1px
//!   solid red;`, `pos:a`, `d:ib`, `w100p`, `c#f00`, `m10!`.
//! * [`abbreviation_before`] / [`css_abbreviation_before`] find the
//!   abbreviation that ends at a caret, [`is_css_context`] /
//!   [`is_html_context`] classify a language, and [`expand_at`] performs the
//!   whole "Tab expands the abbreviation" step on an editor buffer, using
//!   the document around the caret (enclosing element, `<style>` blocks,
//!   `style="..."` attributes, template tags, CSS rule bodies, Sass/Stylus
//!   indentation).
//!
//! The behaviour follows Emmet 2 with a few deliberate differences: output
//! is HTML5-style (`<br>`, `<input disabled>`), one caret plus a list of
//! edit points replaces numbered tab stops, `!` puts the caret in `<body>`,
//! lorem text is deterministic, and in CSS a `-` after a
//! value with a unit is still a separator (`m10p-5p` -> `10% 5%`).

mod configured;
mod css;
mod expansion;
mod extract;
mod html;
mod lorem;

use std::ops::Range;

pub use configured::{configured_at, configured_before, stylesheet_context};
use css::Syntax;
pub use css::{expand_css, expand_css_with_caret};
pub use expansion::Expansion;
pub use extract::{abbreviation_before, css_abbreviation_before};
pub use html::{expand_html, expand_html_in};

/// Normalises a language id or file extension: `"CSS"`, `"css"`, `".scss"`.
fn language_key(language: &str) -> String {
    language.trim().trim_start_matches('.').to_ascii_lowercase()
}

/// Whether `language` (a language id, display name or file extension, case
/// insensitive) is a stylesheet language where CSS abbreviations apply.
pub fn is_css_context(language: &str) -> bool {
    matches!(
        language_key(language).as_str(),
        "css" | "scss" | "sass" | "less" | "stylus" | "styl" | "postcss" | "pcss"
    )
}

/// Whether `language` is a markup language where HTML abbreviations apply
/// (HTML/XML and the usual template languages).
pub fn is_html_context(language: &str) -> bool {
    matches!(
        language_key(language).as_str(),
        "html"
            | "jsx"
            | "tsx"
            | "javascriptreact"
            | "typescriptreact"
            | "svelte"
            | "htm"
            | "xhtml"
            | "xml"
            | "xsl"
            | "xslt"
            | "svg"
            | "vue"
            | "django"
            | "django-html"
            | "django template"
            | "jinja"
            | "jinja2"
            | "j2"
            | "twig"
            | "php"
            | "erb"
            | "handlebars"
            | "hbs"
            | "mustache"
            | "jsp"
    )
}

/// Expands the abbreviation that ends at byte `offset` of `text` — the
/// editor's "Tab expands Emmet abbreviation" action. `language` selects the
/// syntax (see [`is_css_context`] / [`is_html_context`]; other languages
/// never expand) and `indent` is the indentation unit.
///
/// Returns the byte range of `text` to replace and the expansion to insert
/// there, already re-based onto the indentation of the abbreviation's line
/// (the new caret is `range.start + expansion.caret`). Context is taken into
/// account:
///
/// * HTML: nothing expands inside a tag (`<a href="x|`), inside `<script>`
///   or inside a template tag (`{{ name|`, `{% if x|`); `<style>` content
///   and `style="..."` attribute values use CSS rules (the latter on one
///   line); top-level nameless elements take their implicit tag from the
///   enclosing element (`.item` inside `<ul>` becomes an `<li>`).
/// * CSS: only declaration positions expand (not selectors or values); in
///   Sass and Stylus an indented line is a declaration position and the
///   output follows the dialect (`margin: 10px` / `margin 10px`).
pub fn expand_at(
    text: &str,
    offset: usize,
    language: &str,
    indent: &str,
) -> Option<(Range<usize>, Expansion)> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let (start, abbr, expansion) = if is_css_context(language) {
        let syntax = Syntax::for_language(&language_key(language));
        let (start, abbr) = css_abbreviation_before(text, offset)?;
        if !extract::css_declaration_start(text, start, abbr, syntax.indented()) {
            return None;
        }
        (start, abbr, css::expand(abbr, indent, syntax, false)?)
    } else if is_html_context(language) {
        match extract::raw_text_element(text, offset) {
            Some(("style", css_start)) => {
                let (start, abbr) = css_abbreviation_before(text, offset)?;
                if start < css_start
                    || !extract::css_declaration_start(
                        &text[css_start..],
                        start - css_start,
                        abbr,
                        false,
                    )
                {
                    return None;
                }
                (start, abbr, expand_css_with_caret(abbr, indent)?)
            }
            Some(_) => return None,
            None => match extract::style_attribute_start(text, offset) {
                Some(value_start) => {
                    let (start, abbr) = css_abbreviation_before(text, offset)?;
                    if start < value_start
                        || !extract::inline_declaration_start(&text[value_start..start])
                    {
                        return None;
                    }
                    (start, abbr, css::expand(abbr, indent, Syntax::Css, true)?)
                }
                None => {
                    let (start, abbr) = abbreviation_before(text, offset)?;
                    if extract::inside_tag(text, start) || extract::inside_template_tag(text, start)
                    {
                        return None;
                    }
                    let parent = extract::enclosing_element(text, start);
                    (
                        start,
                        abbr,
                        if matches!(
                            language_key(language).as_str(),
                            "jsx" | "tsx" | "javascriptreact" | "typescriptreact"
                        ) {
                            html::expand_jsx_in(abbr, indent, parent.as_deref())?
                        } else {
                            expand_html_in(abbr, indent, parent.as_deref())?
                        },
                    )
                }
            },
        }
    } else {
        return None;
    };
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..];
    let base = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
    Some((start..start + abbr.len(), expansion.with_base_indent(base)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_contexts() {
        for css in ["css", "CSS", "scss", ".less", "Sass", "stylus"] {
            assert!(is_css_context(css), "{css}");
        }
        for not_css in ["html", "python", "", "cs"] {
            assert!(!is_css_context(not_css), "{not_css}");
        }
        assert!(is_html_context("HTML"));
        assert!(is_html_context("jinja2"));
        assert!(!is_html_context("python"));
        assert!(!is_html_context("css"));
    }
}
