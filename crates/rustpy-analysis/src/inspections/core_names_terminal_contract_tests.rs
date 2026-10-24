use super::*;

#[test]
fn unknown_returning_decorators_cannot_keep_original_never_contract_or_body() {
    for name in ["exit", "stop"] {
        let source = format!(
            "from typing import NoReturn\ndef deco(fn):\n    return lambda: None\n@deco\ndef {name}() -> NoReturn:\n    raise RuntimeError\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        {name}()\n    return value\n"
        );
        let names: Vec<_> = found(&source, &run(&source), "PyUnboundLocalVariable")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["value"], "{source}");
    }
}

#[test]
fn project_typing_module_spelling_and_local_never_objects_are_not_contracts() {
    for source in [
        "NoReturn = object()\ndef exit() -> NoReturn:\n    return None\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        exit()\n    return value\n",
        "Never = object()\ndef stop() -> Never:\n    return None\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        stop()\n    return value\n",
    ] {
        // This is a real project FileInfo named typing, not a resolver claim
        // that the protected typing import selected a project replacement.
        let names: Vec<_> = found(
            source,
            &run_in("/proj/typing.py", source),
            "PyUnboundLocalVariable",
        )
        .into_iter()
        .map(|(name, _)| name)
        .collect();
        assert_eq!(names, ["value"], "{source}");
    }
}

#[test]
fn genuine_named_nonfinal_never_contract_preserves_defined_join() {
    for annotation in ["NoReturn", "Never"] {
        let source = format!(
            "from typing import {annotation} as Bottom\ndef stop() -> Bottom:\n    raise RuntimeError\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        stop()\n        pass\n    return value\n"
        );
        assert!(
            found(&source, &run(&source), "PyUnboundLocalVariable").is_empty(),
            "{source}"
        );
    }
}

#[test]
fn non_shortlist_no_return_method_replacement_keeps_reachable_tail() {
    let source = "from typing import NoReturn\nclass C:\n    def stop(self) -> NoReturn:\n        raise RuntimeError\ndef f(flag):\n    c = C()\n    c.stop = lambda: None\n    if flag:\n        value = 1\n    else:\n        c.stop()\n    return value\n";
    let names: Vec<_> = found(source, &run(source), "PyUnboundLocalVariable")
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["value"]);
}

#[test]
fn replaced_typing_annotation_member_is_not_an_unchanged_never_contract() {
    let source = "import typing\ntyping.NoReturn = object\ndef stop() -> typing.NoReturn:\n    return None\ndef f(flag):\n    if flag:\n        value = 1\n    else:\n        stop()\n    return value\n";
    let names: Vec<_> = found(source, &run(source), "PyUnboundLocalVariable")
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["value"]);
}
