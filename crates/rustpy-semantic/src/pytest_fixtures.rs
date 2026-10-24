//! Shared pytest fixture inventory for completion, declaration navigation and usages.
use crate::{
    db::{Analysis, FileInfo},
    infer::{Definition, Inferer},
    scope::BindingKind,
    types::{LiteralValue, Type},
};
use rustpy_core::TextSize;
use rustpy_syntax::ast::{self, AstNode};
use std::{collections::HashSet, sync::Arc};

#[derive(Clone)]
pub struct Fixture {
    pub name: String,
    pub definition: Definition,
    pub explicit_name: bool,
}
/// Nearest definitions come first: file, nearest conftest, then ancestors.
/// Inventory stays rooted in the resolver and never scans excluded directories.
pub fn available(db: &Analysis, file: &Arc<FileInfo>, offset: Option<TextSize>) -> Vec<Fixture> {
    let mut files = vec![file.clone()];
    let mut directory = file.path.parent();
    for _ in 0..32 {
        let Some(dir) = directory else {
            break;
        };
        if !db.resolver.roots.iter().any(|root| dir.starts_with(root)) {
            break;
        }
        let path = dir.join("conftest.py");
        if path != file.path && !db.resolver.is_excluded(&path) {
            if let Some(source) = db.file_for_path(&path) {
                files.push(source);
            }
        }
        directory = dir.parent();
    }
    let mut fixtures = Vec::new();
    for source in files {
        for function in source
            .parse
            .syntax_node()
            .descendants()
            .take(8192)
            .filter_map(ast::FuncDef::cast)
        {
            if !crate::basic_completion::fixture_decorator(db, &source, &function) {
                continue;
            }
            if function
                .syntax()
                .ancestors()
                .skip(1)
                .any(|node| ast::FuncDef::cast(node).is_some())
            {
                continue;
            }
            if let Some(class) = function
                .syntax()
                .ancestors()
                .skip(1)
                .find_map(ast::ClassDef::cast)
            {
                if source.path != file.path
                    || !offset.is_some_and(|offset| class.text_range().contains_inclusive(offset))
                {
                    continue;
                }
            }
            let Some(token) = function.name() else {
                continue;
            };
            let Some((binding, _)) = source.scopes.binding_at(token.text_range().start()) else {
                continue;
            };
            let mut name = function.name_text();
            let mut explicit_name = false;
            for decorator in function.decorators() {
                if let Some(ast::Expr::Call(call)) = decorator.expr() {
                    if !crate::basic_completion::fixture_expression(
                        db,
                        &source,
                        ast::Expr::Call(call.clone()),
                    ) {
                        continue;
                    }
                    if let Some(value) = call
                        .arg_list()
                        .and_then(|args| {
                            args.keywords()
                                .find(|argument| argument.name_text() == "name")
                        })
                        .and_then(|argument| argument.value())
                    {
                        if let Type::Literal(LiteralValue::Str(alias)) =
                            Inferer::new(db).type_of_expr(&source, &value)
                        {
                            name = alias.to_string();
                            explicit_name = true;
                        }
                    }
                }
            }
            fixtures.push(Fixture {
                name,
                explicit_name,
                definition: Definition {
                    file: source.clone(),
                    binding,
                },
            });
        }
    }
    fixtures
}
fn consumer_function(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<ast::FuncDef> {
    let function = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .filter(|function| function.text_range().contains_inclusive(offset))
        .min_by_key(|function| u32::from(function.text_range().len()))?;
    (function.name_text().starts_with("test_")
        || crate::basic_completion::fixture_decorator(db, file, &function))
    .then_some(function)
}
/// An injected fixture parameter or one of its body references.
pub fn resolve(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<Definition> {
    let function = consumer_function(db, file, offset)?;
    let token = crate::features::ident_at(file, offset)?;
    let parameter = function.params()?.params().find(|parameter| {
        parameter
            .name()
            .is_some_and(|name| name.text() == token.text())
    })?;
    let name = parameter.name()?;
    let binding = file.scopes.binding_at(name.text_range().start())?.0;
    if name.text_range() != token.text_range() {
        let definition =
            Inferer::new(db).resolve_name(file, file.scopes.scope_at(offset), token.text())?;
        if definition.file.path != file.path || definition.binding != binding {
            return None;
        }
    }
    available(db, file, Some(offset))
        .into_iter()
        .find(|fixture| {
            fixture.name == name.text()
                && !(fixture.definition.file.path == file.path
                    && function
                        .text_range()
                        .contains_inclusive(fixture.definition.binding().range.start()))
        })
        .map(|fixture| fixture.definition)
}
/// Parameter injection and its body references resolve to the same fixture
/// definition, including aliases and a nearer conftest override.
pub fn usages_for(
    db: &Analysis,
    file: &Arc<FileInfo>,
    definition: &Definition,
) -> Vec<crate::usages::Usage> {
    if definition.binding().kind != BindingKind::Function {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for function in file
        .parse
        .syntax_node()
        .descendants()
        .take(8192)
        .filter_map(ast::FuncDef::cast)
    {
        if !function.name_text().starts_with("test_")
            && !crate::basic_completion::fixture_decorator(db, file, &function)
        {
            continue;
        }
        let Some(params) = function.params() else {
            continue;
        };
        for parameter in params.params() {
            let Some(name) = parameter.name() else {
                continue;
            };
            let Some(resolved) = resolve(db, file, name.text_range().start()) else {
                continue;
            };
            if resolved.file.path != definition.file.path || resolved.binding != definition.binding
            {
                continue;
            }
            let dynamic = available(db, file, Some(name.text_range().start()))
                .into_iter()
                .any(|fixture| {
                    fixture.explicit_name
                        && fixture.definition.file.path == resolved.file.path
                        && fixture.definition.binding == resolved.binding
                        && fixture.name == name.text()
                });
            if seen.insert(name.text_range()) {
                let mut usage = crate::usages::usage_at(file, name.text_range(), false, false);
                usage.dynamic = dynamic;
                out.push(usage);
            }
            if let Some((binding, _)) = file.scopes.binding_at(name.text_range().start()) {
                for reference in file.scopes.references_to(binding) {
                    if seen.insert(reference.range) {
                        let mut usage =
                            crate::usages::usage_at(file, reference.range, false, false);
                        usage.dynamic = dynamic;
                        out.push(usage);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleResolver;
    #[test]
    fn fixture_alias_navigation_usage_search_and_completion_share_inventory() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        let conftest = root.join("conftest.py");
        std::fs::write(&conftest, "import pytest\n@pytest.fixture(name='database')\ndef build_database():\n    return 42\n").unwrap();
        let path = root.join("tests/test_data.py");
        let text = "def test_data(database):\n    assert database == 42\n";
        std::fs::write(&path, text).unwrap();
        let database = Analysis::new(ModuleResolver::new(
            vec![root.to_path_buf()],
            Vec::new(),
            (3, 12),
        ));
        let file = database.file_for_path(&path).unwrap();
        let parameter = TextSize::from(text.find("database").unwrap() as u32);
        let body = TextSize::from(text.rfind("database").unwrap() as u32);
        for offset in [parameter, body] {
            let locations = crate::features::definition_at(&database, &file, offset);
            assert_eq!(locations.len(), 1);
            assert_eq!(locations[0].path, conftest);
            let definition = database.file_for_path(&locations[0].path).unwrap();
            assert_eq!(
                &definition.text[usize::from(locations[0].range.start())
                    ..usize::from(locations[0].range.end())],
                "build_database"
            );
        }
        let query = crate::usages::UsageQuery::prepare(&database, &file, body).unwrap();
        assert!(query.candidate_text_may_match(text));
        let usages = query.in_file(&database, &file);
        assert!(usages.iter().any(|usage| usage.range.start() == parameter));
        assert!(usages.iter().any(|usage| usage.range.start() == body));
        // An explicitly named fixture's Python function and injection name are
        // distinct rename identities: default Rename must leave aliases intact.
        assert!(usages
            .iter()
            .filter(|usage| usage.range.start() == parameter || usage.range.start() == body)
            .all(|usage| usage.dynamic));
        let completed = crate::basic_completion::complete(&database, &file, parameter).unwrap();
        assert!(completed.items.iter().any(|item| item.label == "database"));
    }
    #[test]
    fn nearer_conftest_overrides_and_unrelated_parameters_keep_their_own_authority() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(
            root.join("conftest.py"),
            "import pytest\n@pytest.fixture\ndef value(): return 1\n",
        )
        .unwrap();
        let near = root.join("nested/conftest.py");
        std::fs::write(
            &near,
            "import pytest\n@pytest.fixture\ndef value(): return 2\n",
        )
        .unwrap();
        let path = root.join("nested/test_value.py");
        let text = "def test_value(value):\n    assert value == 2\ndef ordinary(value):\n    return value\n";
        std::fs::write(&path, text).unwrap();
        let database = Analysis::new(ModuleResolver::new(
            vec![root.to_path_buf()],
            Vec::new(),
            (3, 12),
        ));
        let file = database.file_for_path(&path).unwrap();
        assert_eq!(
            resolve(
                &database,
                &file,
                TextSize::from(text.find("(value)").unwrap() as u32 + 1)
            )
            .unwrap()
            .file
            .path,
            near
        );
        assert!(resolve(
            &database,
            &file,
            TextSize::from(text.rfind("value").unwrap() as u32)
        )
        .is_none());
    }
}
