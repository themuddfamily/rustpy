//! Django default and standard i18n/tz convenience tag syntax, without evaluation.
use crate::{
    template::Dialect,
    template_builtin::{Diagnostic, Error, Validation},
    template_expression::{self, Context},
};
use std::ops::Range;
fn words(s: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut escape = false;
    for (i, c) in s.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if quote.is_some() && c == '\\' {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if c.is_whitespace() {
            if let Some(a) = start.take() {
                out.push(a..i);
            }
        } else {
            start.get_or_insert(i);
            if matches!(c, '\'' | '"') {
                quote = Some(c);
            }
        }
    }
    if let Some(a) = start {
        out.push(a..s.len());
    }
    out
}
pub fn validate(source: &str, dialect: Dialect) -> Validation {
    let mut out = Validation {
        recognized: false,
        diagnostics: Vec::new(),
    };
    if dialect != Dialect::Django {
        return out;
    }
    let mut parts = words(source);
    if parts.is_empty() {
        return out;
    }
    let tag = &source[parts.remove(0)];
    let mut expression = None;
    let mut error = None;
    match tag {
        "lorem" => {
            if parts.last().is_some_and(|r| &source[r.clone()] == "random") {
                parts.pop();
            }
            if parts
                .last()
                .is_some_and(|r| matches!(&source[r.clone()], "w" | "p" | "b"))
            {
                parts.pop();
            }
            if parts.len() > 1 {
                error = Some(Error::InvalidOption);
            } else {
                expression = parts.first().cloned();
            }
        }
        "language" | "timezone" => {
            if parts.len() != 1 {
                error = Some(Error::MissingArgument);
            } else {
                expression = parts.first().cloned();
            }
        }
        "localtime" => {
            if parts.len() > 1
                || parts
                    .first()
                    .is_some_and(|r| !matches!(&source[r.clone()], "on" | "off"))
            {
                error = Some(Error::InvalidOption);
            }
        }
        "get_current_timezone"
        | "get_available_languages"
        | "get_current_language"
        | "get_current_language_bidi" => {
            // These four upstream getters use plain split(), not split_contents().
            let plain: Vec<_> = source.split_whitespace().collect();
            if plain.len() != 3 || plain[1] != "as" {
                error = Some(Error::InvalidBinding);
            }
        }
        "get_language_info" | "get_language_info_list" => {
            if parts.len() != 4
                || &source[parts[0].clone()] != "for"
                || &source[parts[2].clone()] != "as"
            {
                error = Some(Error::InvalidBinding);
            } else {
                expression = Some(parts[1].clone());
            }
        }
        // Django's block parsers consume the closing token without validating its suffix.
        "endlanguage" | "endlocaltime" | "endtimezone" => {}
        _ => return out,
    }
    out.recognized = true;
    if let Some(error) = error {
        out.diagnostics.push(Diagnostic {
            range: 0..source.len(),
            error,
        });
    }
    if let Some(r) = expression {
        for diagnostic in
            template_expression::parse(&source[r.clone()], dialect, Context::Output).diagnostics
        {
            out.diagnostics.push(Diagnostic {
                range: r.start + diagnostic.range.start..r.start + diagnostic.range.end,
                error: Error::InvalidExpression,
            });
        }
    }
    out
}
