use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn selected(db: &Analysis, source: &Path, text: &str) -> (String, String) {
    let id = db.set_file_text(source, text);
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(last) = module.statements().last().unwrap() else {
        panic!("expression required")
    };
    let infer = Inferer::new(db);
    let ty = infer.type_of_expr(&file, &last.expr().unwrap());
    let Type::Module(id) = ty else {
        panic!("expected one selected module, got {ty}")
    };
    let target = db.file(id).unwrap().module_name.clone().unwrap();
    let def = infer
        .resolve_name(&file, file.scopes.module_scope(), "selected")
        .unwrap();
    (
        target,
        file.text[usize::from(def.binding().node_range.start())
            ..usize::from(def.binding().node_range.end())]
            .to_owned(),
    )
}
#[test]
fn importerror_selects_available_primary_then_changes_to_fallback_when_removed() {
    let temp = tempfile::TempDir::with_prefix("import-fallback").unwrap();
    let primary = temp.path().join("primary.py");
    write(&primary, "value = 1\n");
    write(&temp.path().join("fallback.py"), "value = 2\n");
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    let code = "try:\n    import primary as selected\nexcept ImportError:\n    import fallback as selected\nselected\n";
    let (module, binding) = selected(&db, &source, code);
    assert_eq!(module, "primary");
    assert!(binding.contains("primary"));
    std::fs::remove_file(&primary).unwrap();
    db.invalidate_path(&primary);
    let (module, binding) = selected(&db, &source, code);
    assert_eq!(module, "fallback");
    assert!(binding.contains("fallback"));
    write(&primary, "restored = 3\n");
    db.invalidate_path(&primary);
    assert_eq!(selected(&db, &source, code).0, "primary");
}
#[test]
fn from_import_availability_checks_the_actual_member_or_submodule() {
    let temp = tempfile::TempDir::with_prefix("from-fallback").unwrap();
    write(&temp.path().join("pkg/__init__.py"), "");
    write(&temp.path().join("pkg/api.py"), "value = 1\n");
    write(&temp.path().join("fallback.py"), "value = 2\n");
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    let code = "try:\n    from pkg import api as selected\nexcept ImportError:\n    import fallback as selected\nselected\n";
    assert_eq!(selected(&db, &source, code).0, "pkg.api");
    std::fs::remove_file(temp.path().join("pkg/api.py")).unwrap();
    db.invalidate_path(&temp.path().join("pkg/api.py"));
    assert_eq!(selected(&db, &source, code).0, "fallback");
}
#[test]
fn unknown_exceptions_shadowed_handlers_and_executable_try_bodies_remain_conservative() {
    let temp = tempfile::TempDir::with_prefix("conservative-fallback").unwrap();
    write(&temp.path().join("primary.py"), "");
    write(&temp.path().join("fallback.py"), "");
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    for code in [
        "try:\n    import primary as selected\nexcept ValueError:\n    import fallback as selected\nselected\n",
        "ImportError = ValueError\ntry:\n    import primary as selected\nexcept ImportError:\n    import fallback as selected\nselected\n",
        "try:\n    import primary as selected\n    user_function()\nexcept ImportError:\n    import fallback as selected\nselected\n",
    ] {
        let id = db.set_file_text(&source, code);
        let file = db.file(id).unwrap();
        let infer = Inferer::new(&db);
        let def = infer.resolve_name(&file, file.scopes.module_scope(), "selected").unwrap();
        let binding = &file.text[usize::from(def.binding().node_range.start())..usize::from(def.binding().node_range.end())];
        assert!(binding.contains("fallback"), "unproven paths must not be pruned: {code}");
        assert_eq!(file.scopes.bindings_in(file.scopes.module_scope(), "selected").len(), 2);
    }
}

#[test]
fn late_import_failure_preserves_earlier_successful_bindings() {
    let temp = tempfile::TempDir::with_prefix("partial-import-fallback").unwrap();
    write(&temp.path().join("present.py"), "value = 1\n");
    write(&temp.path().join("fallback.py"), "value = 2\n");
    write(&temp.path().join("pkg/__init__.py"), "");
    write(&temp.path().join("pkg/api.py"), "value = 3\n");
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    for (body, expected) in [
        (
            "    import present as kept\n    import missing\n",
            "present",
        ),
        ("    import present as kept, missing\n", "present"),
        ("    from pkg import api as kept, missing\n", "pkg.api"),
    ] {
        for (name, module_name) in [("kept", expected), ("other", "fallback")] {
            let text =
                format!("try:\n{body}except ImportError:\n    import fallback as other\n{name}\n");
            let id = db.set_file_text(&source, &text);
            let file = db.file(id).unwrap();
            let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
            let ast::Stmt::Expr(last) = module.statements().last().unwrap() else {
                panic!("expression required")
            };
            let infer = Inferer::new(&db);
            let ty = infer.type_of_expr(&file, &last.expr().unwrap());
            let Type::Module(id) = ty else {
                panic!("successful binding {name} was lost: {ty}")
            };
            assert_eq!(
                db.file(id).unwrap().module_name.as_deref(),
                Some(module_name)
            );
            let def = infer
                .resolve_name(&file, file.scopes.module_scope(), name)
                .unwrap();
            assert_ne!(infer.type_of_binding(&file, def.binding), Type::Never);
        }
    }
}

#[test]
fn first_known_failure_retires_only_the_failed_and_later_import_bindings() {
    let temp = tempfile::TempDir::with_prefix("ordered-import-fallback").unwrap();
    write(&temp.path().join("present.py"), "");
    write(&temp.path().join("unreached.py"), "");
    write(&temp.path().join("fallback.py"), "");
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    for body in [
        "    import present as kept\n    import missing as failed\n    import unreached as later\n",
        "    import present as kept, missing as failed, unreached as later\n",
    ] {
        let text =
            format!("try:\n{body}except ImportError:\n    import fallback as selected\nselected\n");
        let (target, _) = selected(&db, &source, &text);
        assert_eq!(target, "fallback");
        let file = db.file_for_path(&source).unwrap();
        let infer = Inferer::new(&db);
        let kept = infer
            .resolve_name(&file, file.scopes.module_scope(), "kept")
            .unwrap();
        let Type::Module(id) = infer.type_of_binding(&file, kept.binding) else {
            panic!("early import was lost")
        };
        assert_eq!(db.file(id).unwrap().module_name.as_deref(), Some("present"));
        for name in ["failed", "later"] {
            assert!(
                infer
                    .resolve_name(&file, file.scopes.module_scope(), name)
                    .is_none(),
                "{name} was never assigned"
            );
            let id = file.scopes.bindings_in(file.scopes.module_scope(), name)[0];
            assert_eq!(infer.type_of_binding(&file, id), Type::Never);
        }
    }
}

#[test]
fn successful_try_does_not_bind_an_unexecuted_exception_handler_name() {
    let temp = tempfile::TempDir::with_prefix("unused-import-handler").unwrap();
    write(&temp.path().join("present.py"), "");
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    let id = db.set_file_text(
        &source,
        "try:\n    import present\nexcept ImportError as error:\n    pass\n",
    );
    let file = db.file(id).unwrap();
    let infer = Inferer::new(&db);
    assert!(infer
        .resolve_name(&file, file.scopes.module_scope(), "present")
        .is_some());
    assert!(infer
        .resolve_name(&file, file.scopes.module_scope(), "error")
        .is_none());
}
