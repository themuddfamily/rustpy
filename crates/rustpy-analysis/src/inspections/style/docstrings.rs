use super::*;
use rustpy_semantic::docstrings::{self, DocstringFormat};
pub(super) fn run(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    config: &StyleInspections,
    sink: &mut Sink,
) {
    for node in file.parse.syntax_node().descendants() {
        let doc = match node.kind() {
            SyntaxKind::MODULE => {
                ast::Module::cast(node.clone()).and_then(|module| module.docstring())
            }
            SyntaxKind::CLASS_DEF => {
                ast::ClassDef::cast(node.clone()).and_then(|class| class.docstring())
            }
            SyntaxKind::FUNC_DEF => {
                ast::FuncDef::cast(node.clone()).and_then(|function| function.docstring())
            }
            _ => None,
        };
        let Some(doc) = doc else { continue };
        let value = doc.value();
        let raw = doc.syntax().text().to_string();
        if config.single_quoted_docstrings
            && raw
                .trim_start_matches(['r', 'R', 'u', 'U'])
                .starts_with('\'')
        {
            let escaped = value
                .replace('\\', "\\\\")
                .replace("\"\"\"", "\\\"\\\"\\\"");
            sink.push(
                doc.syntax().text_range(),
                Severity::WeakWarning,
                "PyDocstringStyle",
                "Docstrings should use triple double quotes",
                vec![QuickFix {
                    title: "Use triple double quotes for docstring".into(),
                    edits: vec![(doc.syntax().text_range(), format!("\"\"\"{escaped}\"\"\""))],
                }],
            );
        }
        for line in value.lines() {
            let heading = line.trim();
            if matches!(
                heading,
                "Arg:" | "Arguments:" | "Paramters:" | "Retuns:" | "Return:" | "Raise:"
            ) {
                sink.push(
                    doc.syntax().text_range(),
                    Severity::WeakWarning,
                    "PyDocstringStyle",
                    format!("Incorrect docstring section '{heading}'"),
                    Vec::new(),
                );
            }
        }
        let Some(function) = ast::FuncDef::cast(node) else {
            continue;
        };
        let parsed = docstrings::parse(&value);
        if parsed.format == DocstringFormat::Plain {
            continue;
        }
        if let Some(params) = function.params() {
            for param in params.params() {
                let (Some(name), Some(annotation)) = (param.name(), param.annotation()) else {
                    continue;
                };
                if let Some((_, Some(documented), _)) = parsed
                    .params
                    .iter()
                    .find(|(parameter, _, _)| parameter.trim_start_matches('*') == name.text())
                {
                    let declared_type = inf.annotation_type(file, &annotation);
                    let declared = declared_type.to_string();
                    if types_differ(inf, file, &annotation, documented, &declared_type) {
                        sink.push(doc.syntax().text_range(),Severity::WeakWarning,"PyDocstringType",format!("Docstring type '{documented}' for '{}' differs from annotation '{declared}'",name.text()),Vec::new());
                    }
                }
            }
        }
        if let (Some((Some(documented), _)), Some(annotation)) =
            (&parsed.returns, function.return_annotation())
        {
            let declared_type = inf.annotation_type(file, &annotation);
            let declared_type = if docstrings::type_information::result_is_yield(&value) {
                inf.documentation_yield_type(&declared_type, function.is_async())
            } else {
                declared_type
            };
            let declared = declared_type.to_string();
            if types_differ(inf, file, &annotation, documented, &declared_type) {
                sink.push(
                    doc.syntax().text_range(),
                    Severity::WeakWarning,
                    "PyDocstringType",
                    format!(
                        "Docstring return type '{documented}' differs from annotation '{declared}'"
                    ),
                    Vec::new(),
                );
            }
        }
    }
}
fn types_differ(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    annotation: &ast::Expr,
    documented: &str,
    declared: &rustpy_semantic::types::Type,
) -> bool {
    use rustpy_semantic::types::Type;
    if matches!(declared, Type::Any | Type::Unknown) {
        return false;
    }
    inf.documentation_type_in_scope(
        file,
        file.scopes
            .references
            .iter()
            .find(|reference| annotation.text_range().contains_range(reference.range))
            .map_or_else(
                || file.scopes.scope_at(annotation.text_range().start()),
                |reference| reference.scope,
            ),
        documented,
    )
    .is_some_and(|documented| {
        !matches!(documented, Type::Any | Type::Unknown) && documented != *declared
    })
}
