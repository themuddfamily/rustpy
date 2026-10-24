//! Public inference consumers for PEP 484 comments and structured docstrings.
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
fn inferred(source: &str, expression: &str) -> Type {
    let directory = tempfile::TempDir::with_prefix("rustpy-stub-annotations").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let value = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|node| node.syntax().text() == expression)
        .last();
    let inferer = Inferer::new(&db);
    if let Some(value) = value {
        return inferer.type_of_expr(&file, &value);
    }
    // A function declaration's identifier is a token, not a NameExpr. Query
    // that actual definition binding when the fixture requests its signature.
    let id = file
        .scopes
        .bindings_in(file.scopes.module_scope(), expression)
        .iter()
        .copied()
        .find(|id| file.scopes.binding(*id).kind == rustpy_semantic::scope::BindingKind::Function)
        .expect("actual expression or function declaration");
    inferer.type_of_binding(&file, id)
}
#[test]
fn assignment_comments_project_scalar_tuple_star_and_multiline_targets() {
    for (source, expression, expected) in [
        ("value = None # type: str\nvalue\n", "value", "str"),
        (
            "left, right = (None, None) # type: (int, str)\nright\n",
            "right",
            "str",
        ),
        (
            "head, *tail = (None, None, None) # type: (int, str, str)\ntail\n",
            "tail",
            "list[str]",
        ),
        (
            "value = (\n None\n) # type: bytes\nvalue\n",
            "value",
            "bytes",
        ),
        ("value: int = 1 # type: str\nvalue\n", "value", "int"),
        (
            "value = 1 # type: ignore[assignment]\nvalue\n",
            "value",
            "int",
        ),
    ] {
        assert_eq!(
            inferred(source, expression).to_string(),
            expected,
            "{source}"
        );
    }
}
#[test]
fn function_and_parameter_comments_supply_signatures_without_overriding_annotations() {
    for source in [
        "def convert(value): # type: (int) -> str\n    return value\nconvert\n",
        "def convert(\n value, # type: int\n):\n    # type: (...) -> str\n    return value\nconvert\n",
        "def convert(value: int) -> str: # type: (bytes) -> float\n    return str(value)\nconvert\n",
    ] {
        let Type::Function(signature) = inferred(source, "convert") else { panic!("function signature"); };
        assert_eq!(signature.params[0].ty.to_string(), "int", "{source}");
        assert_eq!(signature.ret.to_string(), "str", "{source}");
    }
}
#[test]
fn rest_google_and_numpy_docstrings_supply_parameter_and_return_types() {
    for doc in [
        ":type value: int\n    :rtype: str",
        "Args:\n        value (int): Input.\n\n    Returns:\n        str: Output.",
        "Parameters\n    ----------\n    value : int\n        Input.\n\n    Returns\n    -------\n    str\n        Output.",
    ] {
        let source = format!("def convert(value):\n    \"\"\"{doc}\"\"\"\n    return value\nconvert\n");
        let Type::Function(signature) = inferred(&source, "convert") else { panic!("function signature"); };
        assert_eq!(signature.params[0].ty.to_string(), "int", "{source}");
        assert_eq!(signature.ret.to_string(), "str", "{source}");
    }
}

#[test]
fn epytext_and_role_marked_types_feed_real_signatures_and_calls() {
    let source = "class Value:\n    pass\nAlias = Value\ndef convert(value):\n    \"\"\"Convert.\n\n    @type value: :py:class:`Input <~Alias>` or None\n    @rtype: :class:`str`\n    \"\"\"\n    return str(value)\nconvert(None)\n";
    let Type::Function(signature) = inferred(source, "convert") else {
        panic!("function");
    };
    assert!(matches!(signature.params[0].ty, Type::Union(_)));
    assert!(signature.params[0].ty.to_string().contains("Value"));
    assert!(signature.params[0].ty.to_string().contains("None"));
    assert_eq!(inferred(source, "convert(None)").to_string(), "str");
    let source = "def convert(value: bytes) -> int:\n    \"\"\"Convert.\n\n    @type value: :class:`str`\n    @rtype: :class:`bytes`\n    \"\"\"\n    return 1\nconvert\n";
    let Type::Function(signature) = inferred(source, "convert") else {
        panic!("function");
    };
    assert_eq!(signature.params[0].ty.to_string(), "bytes");
    assert_eq!(signature.ret.to_string(), "int");
}

#[test]
fn numpy_multiple_returns_and_yield_docs_preserve_container_identity() {
    let source = "def pair():\n    \"\"\"Pair.\n\n    Returns\n    -------\n    count : int\n        Count.\n    label : str\n        Label.\n    \"\"\"\n    return 1, 'x'\npair()\n";
    assert_eq!(inferred(source, "pair()").to_string(), "tuple[int, str]");
    for doc in [
        "Yields:\n        :class:`str`: Item.",
        ":ytype: :class:`str`",
        "@ytype: str",
        "Yields\n    ------\n    str\n        Item.",
    ] {
        let source = format!(
            "def values():\n    \"\"\"Stream.\n\n    {doc}\n    \"\"\"\n    yield 'x'\nvalues()\n"
        );
        let ty = inferred(&source, "values()");
        let Type::Instance(class, args) = ty else {
            panic!("generator container: {source}");
        };
        assert_eq!(class.name.as_str(), "Generator");
        assert_eq!(args[0].to_string(), "str");
    }
    let source = "async def values():\n    \"\"\"Stream.\n\n    :ytype: str\n    \"\"\"\n    yield 'x'\nvalues()\n";
    let Type::Instance(class, args) = inferred(source, "values()") else {
        panic!("async generator");
    };
    assert_eq!(class.name.as_str(), "AsyncGenerator");
    assert_eq!(args[0].to_string(), "str");
}

#[test]
fn result_mismatch_uses_own_body_and_documented_name_ranges_and_policy() {
    let directory = tempfile::TempDir::with_prefix("rustpy-doc-result-contract").unwrap();
    let path = directory.path().join("module.py");
    let source = "def empty(actual):\n    \"\"\"Mention absent here as prose.\n\n    :param absent: input\n    :return: a value\n    :rtype: int\n    \"\"\"\n    def nested():\n        yield 1\n    pass\ndef none_result() -> None:\n    \"\"\"None.\n\n    :return: nothing\n    :rtype: None\n    \"\"\"\n    pass\n";
    let db = Analysis::for_project(directory.path());
    let id = db.set_file_text(&path, source);
    let file = db.file(id).unwrap();
    let problems = rustpy_semantic::docstrings::docstring_problems(&db, &file);
    let (range, _) = problems
        .iter()
        .find(|(_, message)| message.contains("not declared"))
        .unwrap();
    assert_eq!(
        usize::from(range.start()),
        source.find(":param absent").unwrap() + 7
    );
    assert_eq!(
        problems
            .iter()
            .filter(|(_, message)| message.contains("does not return"))
            .count(),
        1
    );
    db.set_analyze_doc_comments(false);
    assert!(rustpy_semantic::docstrings::docstring_problems(&db, &file).is_empty());
}
#[test]
fn doc_analysis_policy_is_frozen_and_leaves_annotations_type_comments_and_docs_available() {
    let directory = tempfile::TempDir::with_prefix("rustpy-doc-policy").unwrap();
    let path = directory.path().join("module.py");
    let source = "def documented(value):\n    \"\"\"Convert.\n\n    :type value: int\n    :rtype: str\n    \"\"\"\n    return value\n\ndef commented(value): # type: (int) -> str\n    return value\n\ndef annotated(value: int) -> str:\n    return str(value)\n";
    let db = Analysis::for_project(directory.path());
    let id = db.set_file_text(&path, source);
    let file = db.file(id).unwrap();
    let functions: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .collect();
    let signature = Inferer::new(&db).signature(&file, &functions[0], None);
    assert_eq!(signature.params[0].ty.to_string(), "int");
    assert_eq!(signature.ret.to_string(), "str");
    let revision = db.revision_excluding(&path);
    db.set_analyze_doc_comments(false);
    assert_ne!(db.revision_excluding(&path), revision);
    let signature = Inferer::new(&db).signature(&file, &functions[0], None);
    assert_eq!(signature.params[0].ty, Type::Unknown);
    assert_eq!(signature.ret, Type::Unknown);
    assert!(signature.docstring.as_ref().unwrap().contains("Convert."));
    for function in &functions[1..] {
        let signature = Inferer::new(&db).signature(&file, function, None);
        assert_eq!(signature.params[0].ty.to_string(), "int");
        assert_eq!(signature.ret.to_string(), "str");
    }
    let frozen = db
        .snapshot(
            std::slice::from_ref(&path),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    db.set_analyze_doc_comments(true);
    assert!(!frozen.analysis().analyze_doc_comments());
    assert!(db.analyze_doc_comments());
}
#[test]
fn type_comments_and_docstrings_retain_real_imports_but_ignore_prose_and_ignore_codes() {
    let directory = tempfile::TempDir::with_prefix("rustpy-stub-imports").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, "from typing import List, Dict, Set, Tuple\nx = None # type: List[int]\ndef f(value):\n    \"\"\":type value: Dict[str, int]\n    Set is mentioned only as prose.\n    \"\"\"\n    pass\ny = 1 # type: ignore[Tuple]\n").unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let unused = rustpy_semantic::imports::unused_import_candidates(&file);
    let names: Vec<_> = unused.iter().map(|item| item.name.as_str()).collect();
    assert!(!names.contains(&"List"));
    assert!(!names.contains(&"Dict"));
    assert!(names.contains(&"Set"));
    assert!(names.contains(&"Tuple"));
}
#[test]
fn callable_stub_definition_recovers_runtime_source_documentation() {
    let directory = tempfile::TempDir::with_prefix("rustpy-callable-stub-docs").unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("demo.pyi"),
        "def convert(value: int) -> str: ...\n",
    )
    .unwrap();
    std::fs::write(root.join("demo.py"), "def convert(value):\n    \"\"\"Runtime conversion documentation.\"\"\"\n    return str(value)\n").unwrap();
    let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        vec![],
        vec![root.to_path_buf()],
        (3, 12),
    ));
    let file = db.module("demo").unwrap();
    assert!(file.is_stub);
    let id = file
        .scopes
        .bindings
        .iter()
        .position(|binding| binding.name == "convert")
        .unwrap();
    let Type::Function(signature) =
        Inferer::new(&db).type_of_binding(&file, rustpy_semantic::scope::BindingId(id as u32))
    else {
        panic!("stub function");
    };
    let callable = Type::Callable(signature);
    assert_eq!(
        rustpy_semantic::stub_docs::doc_for_type(&db, &callable).as_deref(),
        Some("Runtime conversion documentation.")
    );
}

#[test]
fn adjacent_one_line_function_does_not_borrow_later_function_type_comment() {
    let source = "def identity(value): return value\ndef convert(value): # type: (int) -> str\n    return str(value)\nidentity\n";
    let Type::Function(identity) = inferred(source, "identity") else {
        panic!("identity signature");
    };
    assert!(
        identity.params[0].ty.is_unknown(),
        "later parameter contract must stay with convert"
    );
    assert!(
        identity.ret.is_unknown(),
        "later return contract must stay with convert"
    );
    let Type::Function(convert) = inferred(source, "convert") else {
        panic!("convert signature");
    };
    assert_eq!(convert.params[0].ty.to_string(), "int");
    assert_eq!(convert.ret.to_string(), "str");
}
