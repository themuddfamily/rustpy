//! Compiler-context, migration and selected-interpreter compatibility contracts.
use rustpy_analysis::inspections::{self, InspectionOptions, Problem};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn inspect(source: &str, version: (u32, u32)) -> Vec<Problem> {
    let db = Analysis::for_project(Path::new("/syntax-compatibility"));
    let id = db.set_file_text(Path::new("/syntax-compatibility/m.py"), source);
    let file = db.file(id).unwrap();
    inspections::inspect(
        &db,
        &file,
        &InspectionOptions {
            python_version: version,
            ..Default::default()
        },
    )
}
fn has(problems: &[Problem], part: &str) -> bool {
    problems.iter().any(|p| p.diagnostic.message.contains(part))
}
fn compatibility(problems: &[Problem]) -> Vec<&str> {
    problems
        .iter()
        .filter(|p| p.diagnostic.code == "PyCompatibility")
        .map(|p| p.diagnostic.message.as_str())
        .collect()
}

#[test]
fn each_modern_syntax_feature_has_a_selected_version_boundary() {
    for (source, version, feature) in [
        ("if (count := 1): pass\n", (3, 8), "assignment expressions"),
        ("def f(value, /): pass\n", (3, 8), "positional-only"),
        (
            "match value:\n case 1: pass\n",
            (3, 10),
            "'match' statement",
        ),
        ("try: pass\nexcept* Exception: pass\n", (3, 11), "'except*'"),
        ("type Alias[T] = list[T]\n", (3, 12), "'type' alias"),
        ("def f[T=int](): pass\n", (3, 13), "type parameter defaults"),
        (
            "def f[*Ts=*tuple[int], **P=[int]](): pass\n",
            (3, 13),
            "type parameter defaults",
        ),
        ("value=t'hello {name}'\n", (3, 14), "template strings"),
        ("async def f(): await task\n", (3, 5), "async functions"),
        ("def f(): yield from values\n", (3, 3), "yield from"),
    ] {
        let older = inspect(source, (version.0, version.1 - 1));
        assert!(
            compatibility(&older).iter().any(|m| m.contains(feature)),
            "{feature}: {older:?}"
        );
        let current = inspect(source, version);
        assert!(
            !compatibility(&current).iter().any(|m| m.contains(feature)),
            "{feature}: {current:?}"
        );
    }
}
#[test]
fn fstring_baseline_debug_and_pep701_boundaries_are_distinct() {
    for (source, minimum, part) in [
        ("value=f'{name}'\n", (3, 6), "formatted string literals"),
        ("value=f'{name=}'\n", (3, 8), "self-documenting"),
        ("value=f\"{record[\"key\"]}\"\n", (3, 12), "PEP 701"),
        ("value=f\"{'\\n'.join(values)}\"\n", (3, 12), "PEP 701"),
        ("value=f'''{name # comment\n}'''\n", (3, 12), "PEP 701"),
    ] {
        assert!(
            has(&inspect(source, (minimum.0, minimum.1 - 1)), part),
            "{source}"
        );
        assert!(!has(&inspect(source, minimum), part), "{source}");
    }
    assert!(!has(
        &inspect("value=f\"{record['key']}\"\n", (3, 6)),
        "PEP 701"
    ));
}
#[test]
fn python2_calls_dunders_and_syntax_have_actionable_migration_diagnostics() {
    let source = "print 'hello'\nexec 'code'\nvalue=0755\nif a <> b: pass\nvalue=`obj`\nraise ValueError, 'bad'\nraise 'bad'\nvalues.iteritems()\nvalues.has_key('k')\nvalues.sort(cmp=compare)\nfor n in xrange(3): pass\nraw_input('prompt')\nunicode(1)\nlong(1)\nfile('p')\nbasestring\nclass Old:\n __metaclass__=Meta\n def __nonzero__(self): return True\n def __cmp__(self, other): return 0\n def __div__(self, other): return self\n";
    let problems = inspect(source, (3, 12));
    for part in [
        "print",
        "exec",
        "octal",
        "!=",
        "backquotes",
        "raise E, value",
        "String exceptions",
        "iterator method",
        "has_key",
        "cmp=",
        "xrange",
        "raw_input",
        "unicode",
        "long",
        "file",
        "basestring",
        "__metaclass__",
        "__nonzero__",
        "__cmp__",
        "__div__",
    ] {
        assert!(has(&problems, part), "missing {part}: {problems:?}");
    }
    let fix = problems
        .iter()
        .flat_map(|p| &p.fixes)
        .find(|f| f.title == "Convert to print() call")
        .unwrap();
    assert_eq!(fix.edits[0].1, "('hello')");
    let local = inspect(
        "def xrange(n): return [n]\ndef raw_input(s): return s\nxrange(1)\nraw_input('ok')\n",
        (3, 12),
    );
    assert!(!has(&local, "Python 2 builtin"), "{local:?}");
}
#[test]
fn declarations_are_checked_in_their_actual_scope_and_source_order() {
    let invalid = "def outer(value):\n print(value)\n global value\n def inner():\n  print(other)\n  nonlocal other\n other=1\n";
    let problems = inspect(invalid, (3, 12));
    for part in ["prior to global", "Parameter 'value'", "prior to nonlocal"] {
        assert!(has(&problems, part), "{problems:?}");
    }
    let valid = "value=1\ndef outer():\n other=1\n def inner():\n  global value\n  nonlocal other\n  value=other\n return inner\n";
    assert!(!has(&inspect(valid, (3, 12)), "prior to"));
    assert!(has(&inspect("nonlocal x\n", (3, 12)), "module level"));
    assert!(has(&inspect("global x\n", (3, 12)), "has no effect"));
}
#[test]
fn async_contexts_do_not_leak_across_nested_definitions_or_classes() {
    let invalid = "async def outer():\n def inner():\n  async for x in values: pass\n  async with manager: pass\n  await task\n class Nested:\n  async with manager: pass\n";
    let problems = inspect(invalid, (3, 12));
    assert_eq!(
        problems
            .iter()
            .filter(|p| p.diagnostic.message.contains("async for/with' outside"))
            .count(),
        3,
        "{problems:?}"
    );
    assert!(has(&problems, "'await' outside async"));
    let valid = "async def f():\n async for x in values: pass\n async with manager as (a,b): pass\n return [item async for item in values]\n";
    assert!(!has(&inspect(valid, (3, 12)), "outside async"));
}
#[test]
fn assignment_deletion_and_unpacking_targets_reject_only_invalid_contexts() {
    for source in [
        "1 = value\n",
        "del 1\n",
        "__debug__=False\n",
        "del __debug__\n",
        "a,*b,*c=values\n",
        "(a,b)+=values\n",
        "def f(__debug__): pass\n",
        "*value\n",
    ] {
        assert!(
            inspect(source, (3, 12))
                .iter()
                .any(|p| p.diagnostic.code == "PySyntaxError"),
            "{source}"
        );
    }
    let valid = "a,*rest=values\n(a,(b,*tail))=values\ndel a, obj.attr, items[0]\nfor (a,*tail) in values: pass\nwith manager as (a,*tail): pass\nresult=(*values,)\n";
    let problems = inspect(valid, (3, 12));
    assert!(
        !problems
            .iter()
            .any(|p| p.diagnostic.code == "PySyntaxError"),
        "{problems:?}"
    );
}
#[test]
fn parameter_and_call_ordering_keep_keyword_only_and_unpacking_controls() {
    for source in [
        "def f(a,a): pass\n",
        "def f(a=1,b): pass\n",
        "def f(*): pass\n",
        "def f(/,a): pass\n",
        "def f(a,/,/): pass\n",
        "def f(**kw,other): pass\n",
        "f(a=1, 2)\n",
        "f(**kw,*args)\n",
    ] {
        let problems = inspect(source, (3, 12));
        assert!(
            problems.iter().any(|p| matches!(
                p.diagnostic.code.as_str(),
                "PyParameterList" | "PyArgumentList" | "PySyntaxError"
            )),
            "{source}: {problems:?}"
        );
    }
    let valid = inspect(
        "def f(a=1,/,*,required,optional=2,**kw): pass\nf(1,*args,required=2,**kw)\n",
        (3, 12),
    );
    assert!(!valid.iter().any(|p|p.diagnostic.code=="PyParameterList"||p.diagnostic.code=="PySyntaxError"),"{valid:?}");
}
#[test]
fn exception_order_and_except_star_control_transfer_are_validated() {
    for (source, part) in [
        (
            "try: pass\nexcept: pass\nexcept ValueError: pass\n",
            "must be last",
        ),
        (
            "try: pass\nexcept* ValueError: pass\nexcept TypeError: pass\n",
            "Cannot mix",
        ),
        (
            "def f():\n try: pass\n except* Exception: return 1\n",
            "cannot leave an except*",
        ),
    ] {
        assert!(has(&inspect(source, (3, 12)), part), "{source}");
    }
    let valid = "def f():\n try: pass\n except* Exception:\n  for item in values:\n   break\n  def nested(): return 1\n";
    assert!(!has(&inspect(valid, (3, 12)), "cannot leave an except*"));
    let modern = "try: pass\nexcept ValueError, TypeError: pass\n";
    assert!(has(&inspect(modern, (3, 13)), "unparenthesized multiple"));
    assert!(!has(&inspect(modern, (3, 14)), "unparenthesized multiple"));
}
#[test]
fn patterns_validate_irrefutability_stars_and_equal_mapping_keys() {
    for (source, part) in [
        (
            "match value:\n case name: pass\n case 1: pass\n",
            "Irrefutable pattern",
        ),
        ("match value:\n case [*a,*b]: pass\n", "Multiple starred"),
        (
            "match value:\n case {'x': a, \"x\": b}: pass\n",
            "Duplicate key",
        ),
        (
            "match value:\n case {1: a, True: b}: pass\n",
            "Duplicate key",
        ),
        (
            "match value:\n case 0 | name | 1: pass\n",
            "Irrefutable alternative",
        ),
    ] {
        let problems = inspect(source, (3, 12));
        assert!(has(&problems, part), "{problems:?}");
    }
    let valid = "match value:\n case name if condition: pass\n case {'x': a, 'y': b, **rest}: pass\n case [first,*tail]: pass\n case _: pass\n";
    assert!(!inspect(valid, (3, 12))
        .iter()
        .any(|p| p.diagnostic.code == "PySyntaxError"));
}
#[test]
fn strings_and_future_imports_respect_encoding_and_module_prelude() {
    for (source, part) in [
        ("value=b'π'\n", "ASCII"),
        ("value=b'x' 'y'\n", "Cannot mix"),
        (
            "def f():\n from __future__ import annotations\n",
            "module beginning",
        ),
        ("from __future__ import missing_feature\n", "not defined"),
        (
            "value=1\nfrom __future__ import annotations\n",
            "beginning of the file",
        ),
    ] {
        assert!(has(&inspect(source, (3, 12)), part), "{source}");
    }
    assert!(has(&inspect("value='π'\n", (2, 7)), "encoding declaration"));
    assert!(!has(
        &inspect("# coding: utf-8\nvalue='π'\n", (2, 7)),
        "encoding declaration"
    ));
    let valid =
        "'''Module docs'''\nfrom __future__ import annotations\nvalue=b'\\xcf\\x80'\ntext='π'\n";
    assert!(
        !inspect(valid, (3, 12))
            .iter()
            .any(|p| p.diagnostic.code == "PySyntaxError"
                || p.diagnostic.code == "PyFromFutureImport")
    );
}
#[test]
fn control_statements_and_class_cells_use_lexical_owners() {
    let invalid =
        "return 1\nyield 1\nbreak\ncontinue\nawait task\nsuper()\ndef f():\n return __class__\n";
    let problems = inspect(invalid, (3, 12));
    for part in [
        "return' outside",
        "yield' outside",
        "break' outside",
        "continue' outside",
        "await' outside",
        "super() requires",
        "__class__ cell",
    ] {
        assert!(has(&problems, part), "{problems:?}");
    }
    let valid = "class C:\n def f(self):\n  super().f()\n  return __class__\ndef f():\n for item in values:\n  continue\n yield 1\n return\n";
    assert!(!has(&inspect(valid, (3, 12)), "__class__ cell"));
}
#[test]
fn parser_remains_authoritative_for_malformed_grammar() {
    for source in [
        "def f((a,b)): pass\n",
        "def f() -> int, str: pass\n",
        "value = (1,,2)\n",
        "if True pass\n",
    ] {
        let db = Analysis::for_project(Path::new("/syntax-compatibility"));
        let id = db.set_file_text(Path::new("/syntax-compatibility/bad.py"), source);
        let file = db.file(id).unwrap();
        assert!(!file.parse.errors().is_empty(), "{source}");
        assert!(
            inspections::inspect(&db, &file, &InspectionOptions::default())
                .iter()
                .any(|p| p.diagnostic.code == "syntax"),
            "{source}"
        );
    }
    let problems = inspect("def f():\n return 1\n unreachable()\n", (3, 12));
    assert!(has(&problems, "unreachable"));
}

#[test]
fn relative_imports_use_package_depth_and_init_module_identity() {
    let db = Analysis::for_project(Path::new("/syntax-relative"));
    for (path, module, source, message) in [
        (
            "/syntax-relative/m.py",
            "m",
            "from . import sibling\n",
            Some("non-package"),
        ),
        (
            "/syntax-relative/pkg/m.py",
            "pkg.m",
            "from .. import sibling\n",
            Some("beyond the top-level"),
        ),
        (
            "/syntax-relative/pkg/__init__.py",
            "pkg",
            "from . import sibling\n",
            None,
        ),
        (
            "/syntax-relative/pkg/m.py",
            "pkg.m",
            "from . import sibling\n",
            None,
        ),
    ] {
        let id = db.set_file_text(Path::new(path), source);
        let file = db.file(id).unwrap();
        assert_eq!(
            file.module_name.as_deref(),
            Some(module),
            "package authority premise"
        );
        let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
        match message {
            Some(message) => assert!(has(&problems, message), "{problems:?}"),
            None => assert!(
                !problems
                    .iter()
                    .any(|p| p.diagnostic.code == "PyRelativeImport"),
                "{problems:?}"
            ),
        }
    }
}
#[test]
fn generator_returns_and_implicit_generator_argument_commas_are_contextual() {
    let source = "def f():\n yield 1\n return 2\n";
    assert!(has(
        &inspect(source, (3, 2)),
        "return with a value in a generator"
    ));
    assert!(!has(
        &inspect(source, (3, 3)),
        "return with a value in a generator"
    ));
    for source in [
        "async def f():\n yield 1\n return 2\n",
        "async def f():\n yield from values\n",
    ] {
        assert!(
            inspect(source, (3, 12))
                .iter()
                .any(|p| p.diagnostic.code == "PySyntaxError"),
            "{source}"
        );
    }
    for source in [
        "consume(x for x in values,)\n",
        "consume(x for x in values, other)\n",
    ] {
        assert!(has(
            &inspect(source, (3, 12)),
            "Generator expression must be parenthesized"
        ));
    }
    for source in [
        "consume(x for x in values)\n",
        "consume((x for x in values), other)\n",
    ] {
        assert!(!has(
            &inspect(source, (3, 12)),
            "Generator expression must be parenthesized"
        ));
    }
}
