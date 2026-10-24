//! Validated live-template interchange and editor-context policy.
use crate::live::{self, ExpandContext, Expansion, LiveTemplate, TemplateContext};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_TEMPLATE_BYTES: usize = 1024 * 1024;
pub const MAX_TEMPLATES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpandKey {
    #[default]
    Tab,
    Enter,
    Space,
}
impl ExpandKey {
    pub const ALL: [Self; 3] = [Self::Tab, Self::Enter, Self::Space];
    pub fn label(self) -> &'static str {
        match self {
            Self::Tab => "Tab",
            Self::Enter => "Enter",
            Self::Space => "Space",
        }
    }
}

/// Values captured by the application from its current completion/semantic state.
/// The engine never reads files or invents semantic candidates.
#[derive(Clone, Debug, Default)]
pub struct ExpressionInputs {
    pub completion: Vec<String>,
    pub iterable_variables: Vec<String>,
    pub suggested_names: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariableChoices {
    pub name: String,
    pub values: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractiveExpansion {
    pub expansion: Expansion,
    pub choices: Vec<VariableChoices>,
}

pub fn expand_with_inputs(
    template: &LiveTemplate,
    ctx: &ExpandContext,
    inputs: &ExpressionInputs,
) -> InteractiveExpansion {
    let mut template = template.clone();
    let mut choices = Vec::new();
    let mut resolved = Vec::new();
    for name in template.variable_names() {
        let Some(variable) = template.variables.iter_mut().find(|v| v.name == name) else {
            continue;
        };
        let expr = variable.expression.trim();
        let candidates = match expr {
            "complete()" => inputs.completion.clone(),
            "pyIterableVariable()" => inputs.iterable_variables.clone(),
            "suggestVariableName()" | "pyVariableName()" => inputs.suggested_names.clone(),
            _ if expr.starts_with("enum(") && expr.ends_with(')') => {
                live::split_args(&expr[5..expr.len() - 1])
                    .iter()
                    .filter_map(|a| live::eval_expression(a, ctx, &resolved))
                    .collect()
            }
            _ => Vec::new(),
        };
        let mut seen = HashSet::new();
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|v| v.len() <= MAX_TEMPLATE_BYTES && seen.insert(v.clone()))
            .take(256)
            .collect();
        if let Some(first) = candidates.first() {
            // A literal value cannot accidentally execute another expression.
            variable.expression = serde_json::to_string(first).expect("String serialization");
            variable.default.clear();
            variable.always_stop_at = true;
            resolved.push((name.clone(), first.clone()));
            choices.push(VariableChoices {
                name,
                values: candidates,
            });
        } else if let Some(value) = live::eval_expression(&variable.expression, ctx, &resolved) {
            resolved.push((name, value));
        }
    }
    InteractiveExpansion {
        expansion: live::expand(&template, ctx),
        choices,
    }
}

/// Suggestions for a new iteration variable, not aliases for an already
/// occupied scope binding. A conservative singularization keeps identifiers
/// intact; collisions receive a suffix rather than shadowing the iterable.
pub fn suggest_names(iterables: &[String], occupied: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    for iterable in iterables.iter().take(32) {
        let stem = iterable.rsplit('.').next().unwrap_or(iterable);
        let candidate = if let Some(prefix) = stem.strip_suffix("ies") {
            format!("{prefix}y")
        } else if let Some(prefix) = stem
            .strip_suffix('s')
            .filter(|prefix| !prefix.is_empty() && !prefix.ends_with('s'))
        {
            prefix.to_string()
        } else {
            "item".into()
        };
        let candidate = unique_name(&candidate, occupied);
        if !names.contains(&candidate) {
            names.push(candidate);
        }
    }
    if names.is_empty() {
        names.push(unique_name("item", occupied));
    }
    names
}
fn unique_name(stem: &str, occupied: &[String]) -> String {
    if !occupied.iter().any(|name| name == stem) {
        return stem.into();
    }
    for suffix in 1..=4096 {
        let candidate = format!("{stem}{suffix}");
        if !occupied.contains(&candidate) {
            return candidate;
        }
    }
    // Bounded deterministic fallback remains an editable suggestion.
    format!("{stem}_item")
}

pub fn validate(template: &LiveTemplate) -> Result<(), String> {
    if template.abbreviation.is_empty()
        || template.abbreviation.len() > 128
        || !template
            .abbreviation
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_')
    {
        return Err("Abbreviation must contain letters, digits or underscores.".into());
    }
    if template.template.len() > MAX_TEMPLATE_BYTES
        || template.description.len() > 4096
        || template.group.is_empty()
        || template.group.len() > 128
        || template.variables.len() > 256
    {
        return Err("Template exceeds its size limit or has no group.".into());
    }
    if template.contexts.is_empty() {
        return Err("Choose at least one applicable context.".into());
    }
    let names = template.variable_names();
    let mut seen = HashSet::new();
    for v in &template.variables {
        if !names.contains(&v.name) || !seen.insert(&v.name) {
            return Err(format!(
                "Variable {} is absent from the template or declared twice.",
                v.name
            ));
        }
        if v.expression.len() > 4096 || v.default.len() > MAX_TEMPLATE_BYTES {
            return Err("Variable expression or default is too large.".into());
        }
    }
    Ok(())
}

pub fn import_json(text: &str) -> Result<Vec<LiveTemplate>, String> {
    if text.len() > 8 * MAX_TEMPLATE_BYTES {
        return Err("Import exceeds 8 MiB.".into());
    }
    let templates: Vec<LiveTemplate> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if templates.len() > MAX_TEMPLATES {
        return Err("Too many templates.".into());
    }
    let mut seen = HashSet::new();
    for template in &templates {
        validate(template)?;
        if !seen.insert(&template.abbreviation) {
            return Err(format!("Duplicate abbreviation: {}", template.abbreviation));
        }
    }
    Ok(templates)
}
pub fn export_json(templates: &[LiveTemplate]) -> Result<String, String> {
    if templates.len() > MAX_TEMPLATES {
        return Err("Too many templates.".into());
    }
    let mut total = 0usize;
    for template in templates {
        validate(template)?;
        total = total
            .saturating_add(template.template.len())
            .saturating_add(template.description.len())
            .saturating_add(template.group.len());
        for variable in &template.variables {
            total = total
                .saturating_add(variable.expression.len())
                .saturating_add(variable.default.len());
        }
        if total > 8 * MAX_TEMPLATE_BYTES {
            return Err("Export exceeds 8 MiB.".into());
        }
    }
    let text = serde_json::to_string_pretty(templates).map_err(|e| e.to_string())?;
    import_json(&text)?;
    Ok(text)
}
/// Imported abbreviations replace user entries; unrelated entries survive.
pub fn merge_import(existing: &[LiveTemplate], imported: &[LiveTemplate]) -> Vec<LiveTemplate> {
    existing
        .iter()
        .filter(|old| {
            !imported
                .iter()
                .any(|new| new.abbreviation == old.abbreviation)
        })
        .chain(imported)
        .cloned()
        .collect()
}
/// Escape literal dollars in selected source so saving a selection does not
/// turn source identifiers into template variables unintentionally.
pub fn from_selection(selection: &str, context: TemplateContext) -> LiveTemplate {
    LiveTemplate::new(
        "abbr",
        "Selected text",
        &(selection.replace('$', "$$") + "$END$"),
        &[context],
        "user",
    )
}

/// Python uses the actual lexer and CST ancestors. Other supported template
/// languages have explicit contexts rather than masquerading as Python.
pub fn context_at(language: &str, source: &str, offset: usize) -> TemplateContext {
    use rustpy_syntax::{SyntaxKind as K, TextSize};
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    match language {
        "html" => return TemplateContext::Html,
        "javascript" | "typescript" => return TemplateContext::JavaScript,
        "django" | "jinja" | "jinja2" => return TemplateContext::Django,
        "python" | "cython" => {}
        _ => return TemplateContext::Any,
    }
    let parsed = rustpy_syntax::parse(source);
    let at = TextSize::from(offset as u32);
    let token = parsed.syntax_node().token_at_offset(at).left_biased();
    if let Some(token) = &token {
        if token.kind() == K::COMMENT {
            return TemplateContext::Comment;
        }
        if matches!(
            token.kind(),
            K::STRING | K::FSTRING_MIDDLE | K::TSTRING_MIDDLE
        ) {
            return TemplateContext::String;
        }
    }
    let start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let before = &source[start..offset];
    if !before
        .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
        .trim()
        .is_empty()
    {
        return TemplateContext::Expression;
    }
    if let Some(token) = token {
        for node in token.parent_ancestors() {
            if node.kind() == K::FUNC_DEF {
                return TemplateContext::Statement;
            }
            if node.kind() == K::CLASS_DEF {
                return TemplateContext::ClassBody;
            }
        }
    }
    if before.starts_with([' ', '\t']) {
        TemplateContext::Statement
    } else {
        TemplateContext::Module
    }
}

/// A surround operation belongs to the selected source, not the final caret
/// token. A complete expression may also be a complete expression statement;
/// both legitimate catalogues remain available without granting string/comment
/// templates to ordinary code.
pub fn selection_contexts(
    language: &str,
    source: &str,
    start: usize,
    end: usize,
) -> Vec<TemplateContext> {
    use rustpy_syntax::{SyntaxKind as K, TextSize};
    let start = start.min(source.len());
    let end = end.min(source.len());
    if start >= end || !source.is_char_boundary(start) || !source.is_char_boundary(end) {
        return vec![context_at(language, source, end)];
    }
    let context = context_at(language, source, start);
    if !matches!(language, "python" | "cython") {
        return vec![context];
    }
    let parsed = rustpy_syntax::parse(source);
    let root = parsed.syntax_node();
    let range =
        rustpy_syntax::TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32));
    if let Some(token) = root.token_at_offset(range.start()).right_biased() {
        if token.kind() == K::COMMENT {
            return vec![TemplateContext::Comment];
        }
        if matches!(
            token.kind(),
            K::STRING | K::FSTRING_MIDDLE | K::TSTRING_MIDDLE
        ) && range.start() > token.text_range().start()
        {
            return vec![TemplateContext::String];
        }
    }
    let mut contexts = vec![context];
    if root
        .descendants()
        .any(|node| node.kind().is_expression() && node.text_range() == range)
        && !contexts.contains(&TemplateContext::Expression)
    {
        contexts.push(TemplateContext::Expression);
    }
    contexts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TemplateVariable;
    #[test]
    fn surround_context_uses_selected_statement_start_and_actual_expression_range() {
        let statements = "print('π🙂')\nkeep()";
        assert_eq!(
            selection_contexts("python", statements, 0, statements.len()),
            vec![TemplateContext::Module]
        );
        let expression = "π + value";
        assert!(
            selection_contexts("python", expression, 0, expression.len())
                .contains(&TemplateContext::Expression)
        );
        let comment = "# custom";
        assert_eq!(
            selection_contexts("python", comment, 0, comment.len()),
            vec![TemplateContext::Comment]
        );
        let string = "value = 'custom'";
        let start = string.find("custom").unwrap();
        assert_eq!(
            selection_contexts("python", string, start, start + 6),
            vec![TemplateContext::String]
        );
    }
    #[test]
    fn real_expansion_choices_mirror_unicode_and_resolve_later_variables() {
        let t = LiveTemplate::new(
            "choice",
            "",
            "$NAME$ = $NAME$\n$NEXT$$END$",
            &[TemplateContext::Statement],
            "Python",
        )
        .with_variables(vec![
            TemplateVariable::new("NAME", "enum(\"π\", \"items\", \"π\")", ""),
            TemplateVariable::new("NEXT", "capitalize(NAME)", ""),
        ]);
        let e = expand_with_inputs(&t, &ExpandContext::default(), &ExpressionInputs::default());
        assert_eq!(e.expansion.text, "π = π\nΠ");
        assert_eq!(e.choices[0].values, vec!["π", "items"]);
        assert_eq!(e.expansion.tab_stops[1], (5, 7, "NAME".into()));
    }
    #[test]
    fn completion_and_iterable_choices_use_captured_values() {
        let t = LiveTemplate::new(
            "iter",
            "",
            "$ITEM$ in $SOURCE$",
            &[TemplateContext::Statement],
            "Python",
        )
        .with_variables(vec![
            TemplateVariable::new("ITEM", "suggestVariableName()", "item"),
            TemplateVariable::new("SOURCE", "pyIterableVariable()", "iterable"),
        ]);
        let e = expand_with_inputs(
            &t,
            &ExpandContext::default(),
            &ExpressionInputs {
                suggested_names: vec!["entry".into()],
                iterable_variables: vec!["values".into(), "names".into()],
                completion: vec![],
            },
        );
        assert_eq!(e.expansion.text, "entry in values");
        assert_eq!(e.choices[1].values.len(), 2);
        let complete = LiveTemplate::new("cc", "", "$C$", &[TemplateContext::Expression], "Python")
            .with_variables(vec![TemplateVariable::new("C", "complete()", "")]);
        assert_eq!(
            expand_with_inputs(
                &complete,
                &ExpandContext::default(),
                &ExpressionInputs {
                    completion: vec!["call()".into()],
                    ..Default::default()
                }
            )
            .expansion
            .text,
            "call()"
        );
    }
    #[test]
    fn interchange_preserves_variables_groups_and_rejects_invalid_import_atomically() {
        let mut t = from_selection("cost = '$π$'\n", TemplateContext::String);
        t.group = "My group".into();
        let text = export_json(&[t.clone()]).unwrap();
        assert_eq!(import_json(&text).unwrap(), vec![t.clone()]);
        assert_eq!(
            live::expand(&t, &ExpandContext::default()).text,
            "cost = '$π$'\n"
        );
        assert!(import_json(&format!(
            "[{},{}]",
            serde_json::to_string(&t).unwrap(),
            serde_json::to_string(&t).unwrap()
        ))
        .is_err());
        let old = LiveTemplate::new("old", "", "", &[TemplateContext::Any], "user");
        assert_eq!(
            merge_import(std::slice::from_ref(&old), std::slice::from_ref(&t)),
            vec![old, t]
        );
    }
    #[test]
    fn python_and_web_contexts_do_not_leak_between_strings_comments_and_class_bodies() {
        let s = "class C:\n    field\n    def f(self):\n        value = 'abc'\n        # note\n";
        assert_eq!(
            context_at("python", s, s.find("field").unwrap() + 5),
            TemplateContext::ClassBody
        );
        assert_eq!(
            context_at("python", s, s.find("abc").unwrap() + 1),
            TemplateContext::String
        );
        assert_eq!(
            context_at("python", s, s.find("note").unwrap() + 2),
            TemplateContext::Comment
        );
        assert_eq!(
            context_at("python", "thing = ab", 10),
            TemplateContext::Expression
        );
        assert_eq!(context_at("html", "<p>", 3), TemplateContext::Html);
        assert_eq!(context_at("django", "{{ x }}", 3), TemplateContext::Django);
        assert!(!TemplateContext::Statement.allows(TemplateContext::String));
    }
    #[test]
    fn regex_is_bounded_and_unknown_functions_remain_editable() {
        let t = LiveTemplate::new("rx", "", "$V$", &[TemplateContext::Any], "user").with_variables(
            vec![TemplateVariable::new(
                "V",
                "regularExpression(\"a1b2\", \"[0-9]\", \"_\")",
                "",
            )],
        );
        assert_eq!(live::expand(&t, &ExpandContext::default()).text, "a_b_");
        let mut invalid = t;
        invalid.variables[0].expression = "regularExpression(\"x\", \"[\", \"y\")".into();
        invalid.variables[0].default = "fallback".into();
        assert_eq!(
            live::expand(&invalid, &ExpandContext::default()).text,
            "fallback"
        );
    }
}

#[cfg(test)]
mod fstring_context_tests {
    use super::*;
    #[test]
    fn fstring_literal_and_replacement_expression_have_distinct_real_cst_contexts() {
        let source = "value = f'custom {calculate(1)} tail'";
        assert_eq!(
            context_at("python", source, source.find("custom").unwrap() + 2),
            TemplateContext::String
        );
        assert_eq!(
            context_at("python", source, source.find("calculate").unwrap() + 3),
            TemplateContext::Expression
        );
        assert_eq!(
            context_at("python", source, source.find("tail").unwrap() + 2),
            TemplateContext::String
        );
        let nested = "f\"{'literal'}\"";
        assert_eq!(
            context_at("python", nested, nested.find("literal").unwrap() + 2),
            TemplateContext::String
        );
    }
}

#[cfg(test)]
mod name_suggestion_tests {
    use super::*;
    #[test]
    fn suggestions_do_not_shadow_iterables_or_existing_scope_bindings() {
        assert_eq!(
            suggest_names(
                &["values".into(), "entries".into(), "πs".into()],
                &["values".into(), "value".into(), "entry".into()]
            ),
            vec!["value1", "entry1", "π"]
        );
        assert_eq!(
            suggest_names(&[], &["item".into(), "item1".into()]),
            vec!["item2"]
        );
    }
}
