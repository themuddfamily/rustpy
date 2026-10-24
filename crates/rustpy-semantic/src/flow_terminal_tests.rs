use super::*;
use crate::db::Analysis;

fn tail(src: &str, project_modules: &[(&str, &str)], visited: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-terminal-authority").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, src).unwrap();
    for (name, source) in project_modules {
        std::fs::write(directory.path().join(format!("{name}.py")), source).unwrap();
    }
    let db = Analysis::for_project(directory.path());
    for module in ["sys", "os", "typing", "typing_extensions"] {
        let _ = db.module(module);
    }
    let snapshot = db
        .snapshot(
            std::slice::from_ref(&path),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    let db = snapshot.analysis();
    let file = db.file_for_path(&path).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let scope = flow_scope(&file, reference.scope).unwrap();
    let flow = flow_of_with_db(db, &file, scope);
    assert_eq!(
        flow.refs.contains_key(&reference.range.start()),
        visited,
        "{src}"
    );
    if visited {
        let reaches = &flow.refs[&reference.range.start()];
        assert_eq!(reaches.len(), 1);
        assert_eq!(
            file.scopes.binding(reaches[0].binding.unwrap()).name,
            "marker"
        );
    }
}

#[test]
fn captured_import_aliases_terminate_but_rebound_or_mixed_bindings_do_not() {
    tail(
        "def f():\n    import sys as system\n    marker=1\n    system.exit()\n    marker\n",
        &[],
        false,
    );
    tail(
        "def f():\n    from os import abort as stop\n    marker=1\n    stop()\n    marker\n",
        &[],
        false,
    );
    tail("def f(fake, flag):\n    import sys\n    if flag:\n        sys=fake\n    marker=1\n    sys.exit()\n    marker\n", &[], true);
}

#[test]
fn project_modules_with_standard_names_do_not_grant_terminal_authority() {
    tail(
        "import sys\ndef f():\n    marker=1\n    sys.exit()\n    marker\n",
        &[("sys", "def exit():\n    return None\n")],
        true,
    );
    tail(
        "import os\ndef f():\n    marker=1\n    os.abort()\n    marker\n",
        &[("os", "def abort():\n    return None\n")],
        true,
    );
}

#[test]
fn local_annotated_terminal_functions_require_actual_typing_authority() {
    for annotation in ["NoReturn", "Never"] {
        tail(&format!("from typing import {annotation}\ndef stop() -> {annotation}:\n    raise RuntimeError()\ndef f():\n    marker=1\n    stop()\n    marker\n"), &[], false);
    }
    tail("from typing import NoReturn\nasync def stop() -> NoReturn:\n    raise RuntimeError()\ndef f():\n    marker=1\n    stop()\n    marker\n", &[], true);
    tail("NoReturn=object()\ndef stop() -> NoReturn:\n    return None\ndef f():\n    marker=1\n    stop()\n    marker\n", &[], true);
    tail("from typing import NoReturn\ndef deco(f):\n    return lambda: None\n@deco\ndef stop() -> NoReturn:\n    raise RuntimeError()\ndef f():\n    marker=1\n    stop()\n    marker\n", &[], true);
}

#[test]
fn file_only_is_conservative_but_actual_live_consumer_proves_canonical_calls() {
    let db = Analysis::for_project(std::path::Path::new("/proj"));
    let id = db.set_file_text(
        std::path::Path::new("/proj/main.py"),
        "import sys\nmarker=1\nsys.exit()\nmarker\n",
    );
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    assert!(flow_of(&file, ScopeId(0))
        .refs
        .contains_key(&reference.range.start()));
    assert!(!flow_of_with_db(&db, &file, ScopeId(0))
        .refs
        .contains_key(&reference.range.start()));
    let inferer = crate::infer::Inferer::new(&db);
    let first = inferer.scope_flow(&file, ScopeId(0));
    let again = inferer.scope_flow(&file, ScopeId(0));
    assert!(Arc::ptr_eq(&first, &again));
    assert!(!again.refs.contains_key(&reference.range.start()));
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .next()
        .unwrap();
    assert!(inferer.call_terminates(&file, &ast::Expr::Call(call)));
}

#[test]
fn ordinary_module_member_writes_and_conditional_imports_are_unknown() {
    for mutation in [
        "sys.exit=lambda: None",
        "alias.exit=lambda: None",
        "del sys.exit",
        "sys.exit += callback",
        "sys.exit: object=lambda: None",
    ] {
        tail(
            &format!("import sys\nalias=sys\n{mutation}\nmarker=1\nsys.exit()\nmarker\n"),
            &[],
            true,
        );
    }
    tail("def f(flag):\n    if flag:\n        import sys\n    marker=1\n    sys.exit()\n    marker\n", &[], true);
}

#[test]
fn live_dependency_edit_retires_cache_while_frozen_world_retains_its_proof() {
    let directory = tempfile::TempDir::with_prefix("rustpy-terminal-live-capture").unwrap();
    let path = directory.path().join("main.py");
    let source = "import sys\nmarker=1\nsys.exit()\nmarker\n";
    std::fs::write(&path, source).unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let inferer = crate::infer::Inferer::new(&db);
    let initial = inferer.scope_flow(&file, ScopeId(0));
    let cached = inferer.scope_flow(&file, ScopeId(0));
    assert!(Arc::ptr_eq(&initial, &cached));
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    assert!(!initial.refs.contains_key(&reference.range.start()));
    assert!(db.module("sys").unwrap().path.starts_with("<typeshed>"));
    let frozen = db
        .snapshot(
            std::slice::from_ref(&path),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    let frozen_file = frozen.analysis().file(file.id).unwrap();
    let frozen_inferer = crate::infer::Inferer::new(frozen.analysis());
    assert!(!frozen_inferer
        .scope_flow(&frozen_file, ScopeId(0))
        .refs
        .contains_key(&reference.range.start()));
    let shadow = directory.path().join("sys.py");
    std::fs::write(&shadow, "def exit():\n    return None\n").unwrap();
    let epoch = db.revision_excluding(&path);
    db.invalidate_modules();
    assert_ne!(db.revision_excluding(&path), epoch);
    assert_eq!(db.module("sys").unwrap().path, shadow);
    let changed = inferer.scope_flow(&file, ScopeId(0));
    assert!(!Arc::ptr_eq(&changed, &cached));
    assert!(changed.refs.contains_key(&reference.range.start()));
    assert!(!frozen_inferer
        .scope_flow(&frozen_file, ScopeId(0))
        .refs
        .contains_key(&reference.range.start()));
    // A changed source with the same FileId must not inherit cached flow.
    let id = db.set_file_text(&path, "import sys\nmarker=2\nsys.exit()\nmarker\n");
    assert_eq!(id, file.id);
    let newer = db.file(id).unwrap();
    let newer_reference = newer
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    assert!(inferer
        .scope_flow(&newer, ScopeId(0))
        .refs
        .contains_key(&newer_reference.range.start()));
    assert!(flow_of_with_db(frozen.analysis(), &newer, ScopeId(0))
        .refs
        .contains_key(&newer_reference.range.start()));
}

#[test]
fn repeated_call_point_is_terminal_only_when_every_reachable_visit_proves_it() {
    let db = Analysis::for_project(std::path::Path::new("/proj"));
    let source = "def f(fake, flag, values):\n    import sys\n    for item in values:\n        if flag:\n            sys=fake\n            continue\n        sys.exit()\n        marker=1\n        marker\n";
    let id = db.set_file_text(std::path::Path::new("/proj/main.py"), source);
    let file = db.file(id).unwrap();
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .next()
        .unwrap();
    let scope = flow_scope(
        &file,
        file.scopes.scope_at(call.syntax().text_range().start()),
    )
    .unwrap();
    let info = flow_of_with_db(&db, &file, scope);
    let sys_load = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "sys" && !r.is_write)
        .unwrap();
    let reaches = &info.refs[&sys_load.range.start()];
    assert!(reaches
        .iter()
        .any(|reach| reach.binding.is_some_and(|id| matches!(
            file.scopes.binding(id).kind,
            crate::scope::BindingKind::Import(_)
        ))));
    assert!(reaches
        .iter()
        .any(|reach| reach.binding.is_some_and(|id| matches!(
            file.scopes.binding(id).kind,
            crate::scope::BindingKind::Assignment
        ))));
    let marker = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    assert!(info.refs.contains_key(&marker.range.start()));
    assert!(!info.terminal_calls.contains(&call.syntax().text_range()));
    assert!(!crate::infer::Inferer::new(&db).call_terminates(&file, &ast::Expr::Call(call)));
}

fn declared_contract_tail(source: &str, modules: &[(&str, &str)], visited: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-declared-terminal-contract").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    for (name, body) in modules {
        std::fs::write(directory.path().join(format!("{name}.py")), body).unwrap();
    }
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let scope = flow_scope(&file, reference.scope).unwrap();
    let info = flow_of_with_db(&db, &file, scope);
    assert_eq!(
        info.refs.contains_key(&reference.range.start()),
        visited,
        "{source}"
    );
}

#[test]
fn imported_declared_contracts_and_static_aliases_drop_tail() {
    let module =
        "from typing import NoReturn\ndef stop() -> NoReturn:\n    raise RuntimeError\nhalt=stop\n";
    for source in [
        "from stops import stop\ndef f():\n    marker=1\n    stop()\n    marker\n",
        "import stops as service\ndef f():\n    marker=1\n    service.stop()\n    marker\n",
        "from stops import halt\ndef f():\n    marker=1\n    halt()\n    marker\n",
        "from stops import stop\ndef f():\n    halt=stop\n    stop=lambda:None\n    marker=1\n    halt()\n    marker\n",
    ] {
        declared_contract_tail(source, &[("stops", module)], false);
    }
}

#[test]
fn assigned_contract_aliases_are_cycle_and_rebinding_aware() {
    let prefix = "from typing import Never\ndef stop() -> Never:\n    raise RuntimeError\n";
    for (body, visited) in [
        ("    halt=stop\n    alias=halt\n    marker=1\n    alias()\n    marker\n", false),
        ("    halt=stop\n    halt=lambda:None\n    marker=1\n    halt()\n    marker\n", true),
        ("    halt=stop\n    if flag:\n        halt=lambda:None\n    marker=1\n    halt()\n    marker\n", true),
    ] {
        declared_contract_tail(&format!("{prefix}def f(flag):\n{body}"), &[], visited);
    }
    declared_contract_tail(
        "from cycle import halt\ndef f():\n    marker=1\n    halt()\n    marker\n",
        &[("cycle", "halt=alias\nalias=halt\n")],
        true,
    );
}

#[test]
fn awaited_async_contract_terminates_but_unawaited_or_decorated_does_not() {
    let module = "from typing import Never\nasync def stop() -> Never:\n    raise RuntimeError\n";
    for (expression, visited) in [
        ("await stop()", false),
        ("stop()", true),
        ("((await stop()))", false),
    ] {
        declared_contract_tail(&format!("from stops import stop\nasync def f():\n    marker=1\n    {expression}\n    marker\n"),
            &[("stops", module)], visited);
    }
    declared_contract_tail("from typing import Never\nasync def stop() -> Never:\n    raise RuntimeError\nasync def f():\n    alias=stop\n    marker=1\n    await alias()\n    marker\n", &[], false);
    declared_contract_tail("from stops import stop\ndef f():\n    marker=1\n    stop()\n    marker\n",
        &[("stops", "from typing import Never\ndef decorate(f):\n    return lambda:None\n@decorate\ndef stop() -> Never:\n    raise RuntimeError\n")], true);
}

#[test]
fn parenthesized_canonical_call_retains_terminal_point_authority() {
    declared_contract_tail(
        "import sys\ndef f():\n    marker=1\n    ((sys.exit()))\n    marker\n",
        &[],
        false,
    );
}

#[test]
fn declared_project_dependency_edit_retires_cached_proof_but_not_frozen_world() {
    let directory = tempfile::TempDir::with_prefix("rustpy-declared-contract-edit").unwrap();
    let path = directory.path().join("main.py");
    let dependency = directory.path().join("stops.py");
    std::fs::write(&path, "from stops import stop\nmarker=1\nstop()\nmarker\n").unwrap();
    std::fs::write(
        &dependency,
        "from typing import Never\ndef stop() -> Never:\n    raise RuntimeError\n",
    )
    .unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let inferer = crate::infer::Inferer::new(&db);
    let reference = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let before = inferer.scope_flow(&file, ScopeId(0));
    assert!(!before.refs.contains_key(&reference.range.start()));
    assert!(Arc::ptr_eq(&before, &inferer.scope_flow(&file, ScopeId(0))));
    let frozen = db
        .snapshot(
            std::slice::from_ref(&path),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    let old_file = frozen.analysis().file(file.id).unwrap();
    db.set_file_text(&dependency, "def stop():\n    return None\n");
    let after = inferer.scope_flow(&file, ScopeId(0));
    assert!(!Arc::ptr_eq(&before, &after));
    assert!(after.refs.contains_key(&reference.range.start()));
    assert!(!crate::infer::Inferer::new(frozen.analysis())
        .scope_flow(&old_file, ScopeId(0))
        .refs
        .contains_key(&reference.range.start()));
}
#[test]
fn relative_project_contract_imports_are_captured_with_actual_package_context() {
    let directory = tempfile::TempDir::with_prefix("rustpy-terminal-relative-contract").unwrap();
    let package = directory.path().join("pkg");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(package.join("__init__.py"), "").unwrap();
    std::fs::write(
        package.join("stops.py"),
        "from typing import Never\ndef stop() -> Never:\n    raise RuntimeError\n",
    )
    .unwrap();
    let path = package.join("consumer.py");
    std::fs::write(
        &path,
        "from .stops import stop\ndef f():\n    marker=1\n    stop()\n    marker\n",
    )
    .unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let scope = flow_scope(&file, reference.scope).unwrap();
    assert!(!flow_of_with_db(&db, &file, scope)
        .refs
        .contains_key(&reference.range.start()));
}

#[test]
fn import_dependency_budget_exhaustion_is_conservative_and_alias_cycles_terminate() {
    let directory = tempfile::TempDir::with_prefix("rustpy-terminal-contract-budget").unwrap();
    for index in 0..40 {
        let source = if index == 39 {
            "from typing import Never\ndef stop() -> Never:\n    raise RuntimeError\n".to_owned()
        } else {
            format!("from dep{} import stop\n", index + 1)
        };
        std::fs::write(directory.path().join(format!("dep{index}.py")), source).unwrap();
    }
    let path = directory.path().join("main.py");
    std::fs::write(&path, "from dep0 import stop\nmarker=1\nstop()\nmarker\n").unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    assert!(flow_of_with_db(&db, &file, ScopeId(0))
        .refs
        .contains_key(&reference.range.start()));
    declared_contract_tail(
        "from cycle_a import stop\ndef f():\n    marker=1\n    stop()\n    marker\n",
        &[
            ("cycle_a", "from cycle_b import stop\n"),
            ("cycle_b", "from cycle_a import stop\n"),
        ],
        true,
    );
}

#[test]
fn oversized_declared_dependency_remains_unknown_without_losing_canonical_control() {
    let mut module = "# padding\n".repeat(420_000);
    module.push_str("from typing import Never\ndef stop() -> Never:\n    raise RuntimeError\n");
    assert!(module.len() > 4 * 1024 * 1024);
    declared_contract_tail(
        "from oversized import stop\ndef f():\n    marker=1\n    stop()\n    marker\n",
        &[("oversized", &module)],
        true,
    );
    declared_contract_tail("import sys\nfrom oversized import stop\ndef f():\n    marker=1\n    sys.exit()\n    marker\n", &[("oversized", &module)], false);
}
#[test]
fn imported_callable_is_captured_before_its_local_name_is_rebound() {
    declared_contract_tail(
        "def f():\n    from stops import stop\n    halt=stop\n    stop=lambda:None\n    marker=1\n    halt()\n    marker\n",
        &[("stops", "from typing import NoReturn\ndef stop() -> NoReturn:\n    raise RuntimeError\n")],
        false,
    );
}

#[test]
fn terminal_invocation_uses_callee_captured_before_argument_walrus() {
    declared_contract_tail(
        "from typing import Never\ndef stop(value=None) -> Never:\n    raise RuntimeError\ndef f():\n    local=stop\n    marker=1\n    local(local:=lambda:None)\n    marker\n",
        &[],
        false,
    );
    declared_contract_tail(
        "from typing import Never\ndef stop(value=None) -> Never:\n    raise RuntimeError\ndef f():\n    local=lambda value:None\n    marker=1\n    local(local:=stop)\n    marker\n",
        &[],
        true,
    );
}
#[test]
fn implicit_decorator_application_uses_captured_genuine_callable_contract() {
    declared_contract_tail("from typing import Never\ndef die(function) -> Never:\n    raise RuntimeError\n@die\ndef decorated():\n    pass\nmarker=1\nmarker\n", &[], false);
    declared_contract_tail("from typing import Never\nasync def die(function) -> Never:\n    raise RuntimeError\n@die\ndef decorated():\n    pass\nmarker=1\nmarker\n", &[], true);
}

#[test]
fn declared_generator_contract_does_not_terminate_at_creation() {
    declared_contract_tail("from typing import Never\ndef stop() -> Never:\n    yield 1\ndef f():\n    marker=1\n    stop()\n    marker\n", &[], true);
    declared_contract_tail("from typing import Never\ndef stop() -> Never:\n    def nested():\n        yield 1\n    raise RuntimeError\ndef f():\n    marker=1\n    stop()\n    marker\n", &[], false);
    declared_contract_tail("from typing import Never\ndef stop() -> Never:\n    nested=lambda:(yield 1)\n    raise RuntimeError\ndef f():\n    marker=1\n    stop()\n    marker\n", &[], false);
}
#[test]
fn ordinary_unshadowed_builtin_callable_alias_preserves_terminal_authority() {
    for builtin in ["exit", "quit"] {
        declared_contract_tail(
            &format!("def f():\n    halt={builtin}\n    marker=1\n    halt()\n    marker\n"),
            &[],
            false,
        );
    }
}

#[test]
fn custom_class_namespace_does_not_fabricate_builtin_callable_alias() {
    declared_contract_tail(
        "class Meta(type):\n    @classmethod\n    def __prepare__(mcls, name, bases):\n        return {'exit': lambda:None}\nclass Container(metaclass=Meta):\n    halt=exit\n    marker=1\n    halt()\n    marker\n",
        &[],
        true,
    );
}
#[test]
fn canonical_callable_declarations_preserve_reaching_rebindings() {
    for (body, visited) in [
        ("    marker=1\n    stop()\n    marker\n", false),
        (
            "    stop=lambda:None\n    marker=1\n    stop()\n    marker\n",
            true,
        ),
    ] {
        declared_contract_tail(&format!("from typing import Callable as Callback, Never as Bottom\ndef f(stop: Callback[[], Bottom]):\n{body}"), &[], visited);
    }
    declared_contract_tail("import typing as t\ndef f(factory):\n    stop: t.Callable[..., t.NoReturn]=factory()\n    marker=1\n    stop()\n    marker\n", &[], false);
    declared_contract_tail("import typing as t\ndef f(factory):\n    stop: t.Callable[..., t.NoReturn]=factory()\n    stop=lambda:None\n    marker=1\n    stop()\n    marker\n", &[], true);
    declared_contract_tail("from typing import Callable\nclass NoReturn:\n    pass\ndef f(stop:Callable[[],NoReturn]):\n    marker=1\n    stop()\n    marker\n", &[], true);
}

#[test]
fn intrinsic_new_constructor_and_inherited_slots_follow_c3() {
    declared_contract_tail("from typing import Never\nclass C(object):\n    def __init__(self)->Never:\n        raise RuntimeError\ndef f():\n    marker=1\n    C()\n    marker\n", &[], false);
    declared_contract_tail("from typing import Never\nclass Base:\n    def __new__(cls)->Never:\n        raise RuntimeError\nclass Child(Base):\n    pass\ndef f():\n    marker=1\n    Child()\n    marker\n", &[], false);
    declared_contract_tail("from typing import Never\nclass Base:\n    def __call__(self)->Never:\n        raise RuntimeError\nclass Child(Base):\n    pass\ndef f():\n    stop=Child()\n    marker=1\n    stop()\n    marker\n", &[], false);
    declared_contract_tail("from typing import Never\nclass Normal:\n    def __call__(self):\n        return None\nclass NeverBase:\n    def __call__(self)->Never:\n        raise RuntimeError\nclass Child(Normal,NeverBase):\n    pass\ndef f():\n    stop=Child()\n    marker=1\n    stop()\n    marker\n", &[], true);
}

#[test]
fn imported_class_alias_and_async_callable_only_terminate_when_awaited() {
    let module = "from typing import Never\nclass C:\n    async def __call__(self)->Never:\n        raise RuntimeError\n";
    for (expression, visited) in [("stop()", true), ("await stop()", false)] {
        declared_contract_tail(&format!("import callbacks as cb\nasync def f():\n    Class=cb.C\n    stop=Class()\n    marker=1\n    {expression}\n    marker\n"), &[("callbacks", module)], visited);
    }
}

#[test]
fn unknown_class_dispatch_never_borrows_an_inherited_terminal_slot() {
    for class in [
        "class Meta(type):\n    def __call__(cls):\n        return lambda:None\nclass C(metaclass=Meta):\n    def __call__(self)->Never:\n        raise RuntimeError\n",
        "class C:\n    def __new__(cls):\n        return lambda:None\n    def __call__(self)->Never:\n        raise RuntimeError\n",
        "class C:\n    def __getattribute__(self,name):\n        return lambda:None\n    def __call__(self)->Never:\n        raise RuntimeError\n",
        "class C:\n    @property\n    def __call__(self)->Never:\n        raise RuntimeError\n",
    ] {
        declared_contract_tail(&format!("from typing import Never\n{class}def f():\n    stop=C()\n    marker=1\n    stop()\n    marker\n"), &[], true);
    }
    declared_contract_tail("from typing import Never\nclass C:\n    def __call__(self)->Never:\n        raise RuntimeError\nC.__call__=lambda self:None\ndef f():\n    stop=C()\n    marker=1\n    stop()\n    marker\n", &[], true);
    declared_contract_tail("from typing import Never\nclass C:\n    def __new__(cls):\n        return lambda:None\n    def __init__(self)->Never:\n        raise RuntimeError\ndef f():\n    marker=1\n    C()\n    marker\n", &[], true);
}

#[test]
fn typed_awaitable_callable_creation_is_normal_and_awaited_result_is_terminal() {
    for result in ["Awaitable[NoReturn]", "Coroutine[Any,Any,NoReturn]"] {
        for (expression, visited) in [("stop()", true), ("await stop()", false)] {
            declared_contract_tail(&format!("from typing import Callable, Awaitable, Coroutine, Any, NoReturn\nasync def f(stop:Callable[[],{result}]):\n    marker=1\n    {expression}\n    marker\n"), &[], visited);
        }
    }
    declared_contract_tail("from typing import Callable, Awaitable\nclass NoReturn:\n    pass\nasync def f(stop:Callable[[],Awaitable[NoReturn]]):\n    marker=1\n    await stop()\n    marker\n", &[], true);
}
#[test]
fn over_limit_intrinsic_base_arguments_do_not_form_a_terminal_class_proof() {
    let bases = std::iter::repeat_n("object", 1000)
        .collect::<Vec<_>>()
        .join(",");
    declared_contract_tail(&format!("from typing import Never\nclass C({bases}):\n    def __call__(self)->Never:\n        raise RuntimeError\ndef f():\n    stop=C()\n    marker=1\n    stop()\n    marker\n"), &[], true);
}

#[test]
fn callable_annotation_aliases_and_quotes_preserve_unknown_authority() {
    for prefix in [
        "from typing import Callable, NoReturn\nStop=Callable[[],NoReturn]\nAlias=Stop\n",
        "from typing import Callable, NoReturn\nStop=\"Callable[[],NoReturn]\"\n",
    ] {
        declared_contract_tail(
            &format!("{prefix}def f(stop:Stop):\n    marker=1\n    stop()\n    marker\n"),
            &[],
            false,
        );
    }
    for prefix in [
        "from typing import Callable, NoReturn\nStop=Callable[[],NoReturn]\nStop=int\n",
        "from typing import Callable, NoReturn\nif flag:\n    Stop=Callable[[],NoReturn]\n",
        "from typing import Callable, NoReturn\nStop=Later\nLater=Callable[[],NoReturn]\n",
        "from typing import Callable, NoReturn\nStop=Other\nOther=Stop\n",
    ] {
        declared_contract_tail(
            &format!("{prefix}def f(stop:Stop):\n    marker=1\n    stop()\n    marker\n"),
            &[],
            true,
        );
    }
    for annotation in [
        "\"Callable[[],NoReturn] garbage\"",
        "b\"Callable[[],NoReturn]\"",
        "\"Callable[[], int]\"",
    ] {
        declared_contract_tail(&format!("from typing import Callable, NoReturn\ndef f(stop:{annotation}):\n    marker=1\n    stop()\n    marker\n"), &[], true);
    }
}

#[test]
fn paramspec_contract_requires_canonical_static_declaration() {
    for (prefix, visited) in [
        ("from typing import Callable, NoReturn, ParamSpec\nP=ParamSpec('P')\n", false),
        ("from typing import Callable, NoReturn\ndef ParamSpec(name):\n    return object()\nP=ParamSpec('P')\n", true),
        ("from typing import Callable, NoReturn, ParamSpec\nP=ParamSpec('P')\nP=object()\n", true),
        ("from typing import Callable, NoReturn, ParamSpec\nif flag:\n    P=ParamSpec('P')\n", true),
    ] {
        declared_contract_tail(&format!("{prefix}def f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    marker\n"), &[], visited);
    }
}

#[test]
fn eager_unknown_class_mapping_does_not_reuse_lexical_annotation_heads() {
    let mapping = "class Hint:\n    def __class_getitem__(cls,item):\n        return object\nclass Meta(type):\n    @classmethod\n    def __prepare__(mcls,name,bases):\n        return {'Callable':Hint}\n";
    for (future, header, annotation, visited) in [
        (
            "",
            "class C(metaclass=Meta):",
            "Callable[[],NoReturn]",
            true,
        ),
        ("", "class C:", "Callable[[],NoReturn]", false),
        (
            "",
            "class C(metaclass=Meta):",
            "\"Callable[[],NoReturn]\"",
            false,
        ),
        (
            "from __future__ import annotations\n",
            "class C(metaclass=Meta):",
            "Callable[[],NoReturn]",
            false,
        ),
        (
            "from .__future__ import annotations\n",
            "class C(metaclass=Meta):",
            "Callable[[],NoReturn]",
            true,
        ),
    ] {
        declared_contract_tail(&format!("{future}from typing import Callable, NoReturn\n{mapping}{header}\n    def f(self,stop:{annotation}):\n        marker=1\n        stop()\n        marker\n"), &[], visited);
    }
}

#[test]
fn declared_method_receivers_preserve_mro_and_async_creation() {
    let classes="from typing import NoReturn\nclass Base:\n    def stop(self)->NoReturn:\n        raise RuntimeError\nclass C(Base):\n    pass\n";
    declared_contract_tail(
        &format!("{classes}def f():\n    c=C()\n    marker=1\n    c.stop()\n    marker\n"),
        &[],
        false,
    );
    for (call, visited) in [("c.stop()", true), ("await c.stop()", false)] {
        declared_contract_tail(&format!("from typing import NoReturn\nclass C:\n    async def stop(self)->NoReturn:\n        raise RuntimeError\nasync def f():\n    c=C()\n    marker=1\n    {call}\n    marker\n"), &[], visited);
    }
    declared_contract_tail("from typing import NoReturn\nclass Base:\n    def stop(self)->NoReturn:\n        raise RuntimeError\nclass C(Base):\n    def stop(self):\n        return None\ndef f():\n    c=C()\n    marker=1\n    c.stop()\n    marker\n", &[], true);
}

#[test]
fn declared_methods_refuse_changed_descriptors_and_receivers() {
    for decorator in ["property", "wrapper", "staticmethod"] {
        let prefix = if decorator == "staticmethod" {
            "def staticmethod(fn):\n    return lambda:None\n"
        } else if decorator == "wrapper" {
            "def wrapper(fn):\n    return lambda:None\n"
        } else {
            ""
        };
        declared_contract_tail(&format!("from typing import NoReturn\n{prefix}class C:\n    @{decorator}\n    def stop(self)->NoReturn:\n        raise RuntimeError\ndef f():\n    c=C()\n    marker=1\n    c.stop()\n    marker\n"), &[], decorator != "property");
    }
    for body in [
        "    c=C()\n    c.stop=lambda:None\n",
        "    c=C()\n    c=object()\n",
    ] {
        declared_contract_tail(&format!("from typing import NoReturn\nclass C:\n    def stop(self)->NoReturn:\n        raise RuntimeError\ndef f():\n{body}    marker=1\n    c.stop()\n    marker\n"), &[], true);
    }
    declared_contract_tail("from typing import NoReturn\nclass C:\n    def __new__(cls):\n        return object()\n    def stop(self)->NoReturn:\n        raise RuntimeError\ndef f():\n    c=C()\n    marker=1\n    c.stop()\n    marker\n", &[], true);
}

#[test]
fn typed_and_inline_method_receivers_preserve_unknown_authority() {
    for source in [
        "from typing import NoReturn\nclass C:\n    def stop(self)->NoReturn:\n        raise RuntimeError\ndef f(c:C):\n    c=object()\n    marker=1\n    c.stop()\n    marker\n",
        "from typing import NoReturn\nclass C:\n    def __new__(cls):\n        return object()\n    def stop(self)->NoReturn:\n        raise RuntimeError\ndef f():\n    marker=1\n    C().stop()\n    marker\n",
        "from typing import NoReturn\nclass C:\n    def stop(self)->NoReturn:\n        raise RuntimeError\ndef f():\n    C=lambda:object()\n    marker=1\n    C().stop()\n    marker\n",
        "from typing import NoReturn\nclass C:\n    def stop(self)->NoReturn:\n        raise RuntimeError\ndef f():\n    c=C()\n    halt=c.stop\n    halt=lambda:None\n    marker=1\n    halt()\n    marker\n",
    ] { declared_contract_tail(source,&[],true); }
}

#[test]
fn imported_callable_annotations_preserve_binding_and_dependency_authority() {
    for module in [
        "from typing import Callable,NoReturn\nStop=Callable[[],NoReturn]\nStop=int\n",
        "from typing import Callable,NoReturn\nif flag:\n    Stop=Callable[[],NoReturn]\n",
        "from other import Stop\n",
    ] {
        declared_contract_tail(
            "from contracts import Stop\ndef f(stop:Stop):\n    marker=1\n    stop()\n    marker\n",
            &[
                ("contracts", module),
                ("other", "from contracts import Stop\n"),
            ],
            true,
        );
    }
    declared_contract_tail("import contracts as c\nc.Stop=int\ndef f(stop:c.Stop):\n    marker=1\n    stop()\n    marker\n",&[("contracts","from typing import Callable,NoReturn\nStop=Callable[[],NoReturn]\n")],true);
}
#[test]
fn paramspec_options_do_not_authorize_unknown_arguments() {
    for options in [
        "covariant=True,contravariant=True",
        "covariant=flag",
        "bound=factory()",
        "unknown=True",
        "covariant=True,covariant=False",
        "**options",
    ] {
        declared_contract_tail(&format!("from typing import Callable,NoReturn,ParamSpec\nP=ParamSpec('P',{options})\ndef f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    marker\n"),&[],true);
    }
}

fn captured_context_metadata(source: &str) -> ContextContract {
    let directory = tempfile::TempDir::with_prefix("rustpy-context-contract-authority").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let function = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .find(|function| function.name().is_some_and(|name| name.text() == "f"))
        .unwrap();
    let scope = file
        .scopes
        .scope_for_node(function.syntax().text_range())
        .unwrap();
    let mut walker = Walker::new(&file, scope, terminal::Authority::captured(&db, &file));
    let mut state = State::default();
    for statement in function.body().unwrap().statements() {
        if let ast::Stmt::With(with) = &statement {
            let context = with.items().next().unwrap().context_expr().unwrap();
            walker.visit(context.syntax(), &mut state).unwrap();
            return walker.context_contract(&context, &state, function.is_async());
        }
        state = walker.stmt(&statement, state).unwrap();
    }
    panic!("actual context statement missing");
}
#[test]
fn context_contract_captures_entry_exit_and_literal_suppression() {
    for (value, expected) in [
        ("False", ContextSuppression::False),
        ("True", ContextSuppression::True),
    ] {
        let source=format!("class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return {value}\ndef f():\n    with C():\n        pass\n");
        let contract = captured_context_metadata(&source);
        assert!(!contract.entry_terminal && !contract.exit_terminal);
        assert_eq!(contract.suppression, expected);
    }
    let contract=captured_context_metadata("from typing import NoReturn\nclass C:\n    def __enter__(self)->NoReturn:\n        raise RuntimeError\n    def __exit__(self,*args)->NoReturn:\n        raise RuntimeError\ndef f():\n    with C():\n        pass\n");
    assert!(contract.entry_terminal && contract.exit_terminal);
    let contract=captured_context_metadata("from typing import NoReturn\nclass C:\n    async def __aenter__(self)->NoReturn:\n        raise RuntimeError\n    async def __aexit__(self,*args):\n        return False\nasync def f():\n    ctx=C()\n    alias=ctx\n    async with alias:\n        pass\n");
    assert!(contract.entry_terminal);
    assert_eq!(contract.suppression, ContextSuppression::False);
}
#[test]
fn context_contract_refuses_class_object_receiver_and_changed_members() {
    for body in [
        "    with C:\n        pass\n",
        "    ctx=C()\n    ctx.__exit__=lambda *args:True\n    with ctx:\n        pass\n",
    ] {
        let contract=captured_context_metadata(&format!("from typing import NoReturn\nclass C:\n    def __enter__(self)->NoReturn:\n        raise RuntimeError\n    def __exit__(self,*args):\n        return False\ndef f():\n{body}"));
        if body.contains("with C:") {
            assert_eq!(contract, ContextContract::default());
        } else {
            assert_eq!(contract.suppression, ContextSuppression::Unknown);
        }
    }
}
#[test]
fn contextlib_suppress_uses_captured_canonical_and_builtin_exception_identity() {
    let contract=captured_context_metadata("from contextlib import suppress\ndef f():\n    with suppress(ValueError,RuntimeError):\n        pass\n");
    assert_eq!(
        contract.suppression,
        ContextSuppression::Exceptions(vec![
            SmolStr::new("ValueError"),
            SmolStr::new("RuntimeError")
        ])
    );
    for prefix in [
        "ValueError=object()\n",
        "def suppress(*args):\n    return object()\n",
    ] {
        let contract=captured_context_metadata(&format!("from contextlib import suppress\n{prefix}def f():\n    with suppress(ValueError):\n        pass\n"));
        assert_eq!(contract.suppression, ContextSuppression::Unknown);
    }
}

#[test]
fn captured_suppression_preserves_base_exception_family_identity() {
    for name in [
        "Exception",
        "BaseException",
        "SystemExit",
        "KeyboardInterrupt",
        "GeneratorExit",
    ] {
        let contract = captured_context_metadata(&format!(
            "from contextlib import suppress\ndef f():\n    with suppress({name}):\n        pass\n"
        ));
        assert_eq!(
            contract.suppression,
            ContextSuppression::Exceptions(vec![SmolStr::new(name)])
        );
        assert!(!contract.exit_may_raise);
    }
    let contract=captured_context_metadata("from contextlib import suppress\nSystemExit=object()\ndef f():\n    with suppress(SystemExit):\n        pass\n");
    assert_eq!(contract.suppression, ContextSuppression::Unknown);
    assert!(contract.exit_may_raise);
}
#[test]
fn captured_exit_exception_edge_requires_compatible_literal_method() {
    for (method, known) in [
        (
            "    def __exit__(self,*args):\n        return False\n",
            true,
        ),
        ("    def __exit__(self,a,b,c):\n        return True\n", true),
        ("    def __exit__(self):\n        return False\n", false),
        (
            "    def __exit__(self,*args,required):\n        return False\n",
            false,
        ),
        (
            "    def __exit__(self,*args):\n        return flag\n",
            false,
        ),
    ] {
        let contract=captured_context_metadata(&format!("class C:\n    def __enter__(self):\n        return self\n{method}def f():\n    with C():\n        pass\n"));
        assert_eq!(
            contract.exit_may_raise, !known,
            "literal body cannot remove argument-binding/unknown invocation exceptions: {method}"
        );
        if !known {
            assert_eq!(contract.suppression, ContextSuppression::Unknown);
        }
    }
}

fn captured_typed_context_metadata(source: &str) -> ContextContract {
    let directory = tempfile::TempDir::with_prefix("rustpy-typed-context-authority").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&path).unwrap();
    let function = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::FuncDef::cast)
        .find(|function| function.name().is_some_and(|name| name.text() == "f"))
        .unwrap();
    let scope = file
        .scopes
        .scope_for_node(function.syntax().text_range())
        .unwrap();
    let mut walker = Walker::new(&file, scope, terminal::Authority::captured(&db, &file));
    let mut state = State::default();
    // Same parameter binding setup as the maintained flow analyze() route.
    for parameter in function
        .params()
        .into_iter()
        .flat_map(|parameters| parameters.params())
    {
        if let Some(name) = parameter.name() {
            walker.bind_at(&mut state, name.text_range().start());
        }
    }
    for statement in function.body().unwrap().statements() {
        if let ast::Stmt::With(with) = &statement {
            let context = with.items().next().unwrap().context_expr().unwrap();
            walker.visit(context.syntax(), &mut state).unwrap();
            return walker.context_contract(&context, &state, function.is_async());
        }
        state = walker.stmt(&statement, state).unwrap();
    }
    panic!("actual context statement missing");
}
#[test]
fn typed_context_parameters_and_aliases_do_not_borrow_base_exit_body() {
    let classes="class C:\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return False\nclass D(C):\n    def __exit__(self,*args):\n        raise RuntimeError\n";
    for body in [
        "    with ctx:\n        pass\n",
        "    alias=ctx\n    with alias:\n        pass\n",
    ] {
        let contract = captured_typed_context_metadata(&format!("{classes}def f(ctx:C):\n{body}"));
        assert_eq!(contract.suppression, ContextSuppression::Unknown);
        assert!(contract.exit_may_raise);
    }
}
#[test]
fn opaque_initializer_cannot_prove_exact_exit_body() {
    for initializer in [
        "    def __init__(self):\n        hand_to_helper(self)\n",
        "    def __init__(self):\n        self.__class__=Other\n",
    ] {
        let source=format!("class C:\n{initializer}    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return False\ndef f():\n    ctx=C()\n    alias=ctx\n    with alias:\n        pass\n");
        let contract = captured_context_metadata(&source);
        assert_eq!(contract.suppression, ContextSuppression::Unknown);
        assert!(contract.exit_may_raise);
    }
    let contract=captured_context_metadata("class C:\n    def __init__(self):\n        pass\n    def __enter__(self):\n        return self\n    def __exit__(self,*args):\n        return False\ndef f():\n    ctx=C()\n    alias=ctx\n    with alias:\n        pass\n");
    assert_eq!(contract.suppression, ContextSuppression::False);
    assert!(!contract.exit_may_raise);
}

#[test]
fn canonical_annotation_shapes_refuse_shadowed_heads_and_bounds() {
    for source in [
        "from typing import Callable,Concatenate,ParamSpec,NoReturn\nP=[]\ndef f(stop:Callable[Concatenate[int,P],NoReturn]):\n    marker=1\n    stop(0)\n    marker\n",
        "from typing import Callable,Concatenate,NoReturn\ndef f(stop:Callable[Concatenate[int,[]],NoReturn]):\n    marker=1\n    stop(0)\n    marker\n",
        "from typing import Callable,ParamSpec,NoReturn\nobject=object()\nP=ParamSpec('P',bound=object)\ndef f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    marker\n",
        "from typing import Callable,Annotated,NoReturn\nAnnotated=object()\ndef f(stop:Annotated[Callable[[],NoReturn],'metadata']):\n    marker=1\n    stop()\n    marker\n",
    ] { declared_contract_tail(source,&[],true); }
}
#[test]
fn bounded_fixed_tuple_projection_retains_index_and_shape_authority() {
    for (annotation, index, visited) in [
        ("tuple[Callable[[],NoReturn],Callable[[],int]]", "-2", false),
        ("tuple[Callable[[],NoReturn],Callable[[],int]]", "-1", true),
        ("tuple[Callable[[],NoReturn],Callable[[],int]]", "-3", true),
        ("Tuple[Callable[[],NoReturn],Callable[[],int]]", "0", false),
        ("tuple[Callable[[],NoReturn],...]", "0", false),
        (
            "Annotated[tuple[Callable[[],NoReturn]],'fixed']",
            "0",
            false,
        ),
        ("\"tuple[Callable[[],NoReturn]]\"", "0", false),
    ] {
        declared_contract_tail(&format!("from typing import Callable,NoReturn,Tuple,Annotated\ndef f(stops:{annotation}):\n    marker=1\n    stops[{index}]()\n    marker\n"),&[],visited);
    }
}

#[test]
fn canonical_builtin_and_static_class_paramspec_bounds_preserve_authority() {
    for bound in ["str", "int", "object"] {
        declared_contract_tail(&format!("from typing import Callable,ParamSpec,NoReturn\nP=ParamSpec('P',bound={bound})\ndef f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    marker\n"),&[],false);
    }
    declared_contract_tail("from typing import Callable,ParamSpec,NoReturn\nclass Base:\n    pass\nAlias=Base\nP=ParamSpec('P',bound=Alias)\ndef f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    marker\n",&[],false);
    declared_contract_tail("from typing import Callable,ParamSpec,NoReturn\nstr=object()\nP=ParamSpec('P',bound=str)\ndef f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    marker\n",&[],true);
}
#[test]
fn homogeneous_variadic_tuple_keeps_terminal_and_returning_elements_distinct() {
    for (element, index, visited) in [
        ("NoReturn", "0", false),
        ("NoReturn", "-1", false),
        ("NoReturn", "100", false),
        ("int", "0", true),
    ] {
        declared_contract_tail(&format!("from typing import Callable,NoReturn\ndef f(stops:tuple[Callable[[],{element}],...]):\n    marker=1\n    stops[{index}]()\n    marker\n"),&[],visited);
    }
}

#[test]
fn declared_factory_results_follow_imported_and_captured_alias_bindings() {
    let module="from typing import Callable,NoReturn\ndef stop()->NoReturn:\n    raise RuntimeError\ndef factory()->Callable[[],NoReturn]:\n    return stop\n";
    declared_contract_tail("from factories import factory\ndef f():\n    alias=factory\n    marker=1\n    alias()()\n    marker\n",&[("factories",module)],false);
    declared_contract_tail("import factories as lib\ndef f():\n    alias=lib.factory\n    marker=1\n    alias()()\n    marker\n",&[("factories",module)],false);
    declared_contract_tail("from factories import factory\ndef f():\n    alias=factory\n    alias=lambda:lambda:1\n    marker=1\n    alias()()\n    marker\n",&[("factories",module)],true);
}
#[test]
fn declared_parameter_factory_returns_keep_creation_and_awaiting_distinct() {
    declared_contract_tail("from typing import Callable,NoReturn\ndef f(factory:Callable[[],Callable[[],NoReturn]]):\n    marker=1\n    factory()()\n    marker\n",&[],false);
    for (expression, visited) in [("factory()()", true), ("await factory()()", false)] {
        declared_contract_tail(&format!("from typing import Callable,Awaitable,NoReturn\nasync def f(factory:Callable[[],Callable[[],Awaitable[NoReturn]]]):\n    marker=1\n    {expression}\n    marker\n"),&[],visited);
    }
    declared_contract_tail("from typing import Callable,Awaitable,NoReturn\nasync def f(factory:Callable[[],Awaitable[Callable[[],NoReturn]]]):\n    marker=1\n    (await factory())()\n    marker\n",&[],false);
}
#[test]
fn async_function_factory_result_requires_awaited_creation() {
    let prefix="from typing import Callable,NoReturn\ndef stop()->NoReturn:\n    raise RuntimeError\nasync def factory()->Callable[[],NoReturn]:\n    return stop\n";
    for (expression, visited) in [("factory()()", true), ("(await factory())()", false)] {
        declared_contract_tail(
            &format!("{prefix}async def f():\n    marker=1\n    {expression}\n    marker\n"),
            &[],
            visited,
        );
    }
}
#[test]
fn transformed_or_generator_factory_results_do_not_borrow_declared_signature() {
    for definition in [
        "def wrapper(fn):\n    return lambda:lambda:1\n@wrapper\ndef factory()->Callable[[],NoReturn]:\n    return stop\n",
        "def factory()->Callable[[],NoReturn]:\n    yield stop\n",
    ] {
        declared_contract_tail(&format!("from typing import Callable,NoReturn\ndef stop()->NoReturn:\n    raise RuntimeError\n{definition}def f():\n    marker=1\n    factory()()\n    marker\n"),&[],true);
    }
}

#[test]
fn captured_class_and_instance_factory_method_aliases_preserve_results() {
    let prefix="from typing import Callable,NoReturn\ndef stop()->NoReturn:\n    raise RuntimeError\nclass C:\n    @staticmethod\n    def factory()->Callable[[],NoReturn]:\n        return stop\n";
    declared_contract_tail(
        &format!(
            "{prefix}def f():\n    factory=C.factory\n    marker=1\n    factory()()\n    marker\n"
        ),
        &[],
        false,
    );
    let prefix="from typing import Callable,NoReturn\ndef stop()->NoReturn:\n    raise RuntimeError\nclass C:\n    def factory(self)->Callable[[],NoReturn]:\n        return stop\n";
    declared_contract_tail(&format!("{prefix}def f():\n    c=C()\n    factory=c.factory\n    marker=1\n    factory()()\n    marker\n"),&[],false);
    declared_contract_tail(&format!("{prefix}def f():\n    c=C()\n    factory=c.factory\n    factory=lambda:lambda:1\n    marker=1\n    factory()()\n    marker\n"),&[],true);
}
#[test]
fn qualified_factory_results_refuse_returning_transformed_and_mutated_members() {
    for decorator in ["staticmethod", "wrapper"] {
        let definition = if decorator == "wrapper" {
            "def wrapper(fn):\n    return lambda:lambda:1\n"
        } else {
            ""
        };
        let return_type = if decorator == "wrapper" {
            "NoReturn"
        } else {
            "int"
        };
        let source=format!("from typing import Callable,NoReturn\n{definition}class C:\n    @{decorator}\n    def factory()->Callable[[],{return_type}]:\n        return lambda:1\ndef f():\n    marker=1\n    C.factory()()\n    marker\n");
        declared_contract_tail(&source, &[], true);
    }
    declared_contract_tail("from typing import Callable,NoReturn\nclass C:\n    @staticmethod\n    def factory()->Callable[[],NoReturn]:\n        return lambda:None\ndef f():\n    C.factory=lambda:lambda:1\n    marker=1\n    C.factory()()\n    marker\n",&[],true);
}
