use rustpy_analysis::inspections::{self, InspectionOptions, Problem};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn run(source: &str) -> Vec<Problem> {
    let db = Analysis::for_project(Path::new("/references-names"));
    let id = db.set_file_text(Path::new("/references-names/main.py"), source);
    inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default())
}
fn code<'a>(problems: &'a [Problem], name: &str) -> Vec<&'a Problem> {
    problems
        .iter()
        .filter(|p| p.diagnostic.code == name)
        .collect()
}
fn has(problems: &[Problem], name: &str, text: &str) -> bool {
    code(problems, name)
        .iter()
        .any(|p| p.diagnostic.message.contains(text))
}
fn apply(source: &str, fix: &inspections::QuickFix) -> String {
    let mut text = source.to_string();
    let mut edits = fix.edits.clone();
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start()));
    for (range, replacement) in edits {
        text.replace_range(
            usize::from(range.start())..usize::from(range.end()),
            &replacement,
        );
    }
    text
}
#[test]
fn unused_parameters_local_definitions_and_target_bindings_have_exclusions() {
    let source = "def work(used, unused, _ignored):\n print(used)\n def local(): return 1\n class Local: pass\n for item in values: pass\n with manager as resource: pass\n return 1\n";
    let problems = run(source);
    assert!(
        has(&problems, "PyUnusedParameter", "unused"),
        "{problems:?}"
    );
    assert!(!has(&problems, "PyUnusedParameter", "_ignored"));
    assert!(!has(&problems, "PyUnusedParameter", "'used'"));
    assert!(has(&problems, "PyUnusedLocalDefinition", "local"));
    assert!(has(&problems, "PyUnusedLocalDefinition", "Local"));
    assert!(has(&problems, "PyUnusedLocal", "item"));
    assert!(has(&problems, "PyUnusedLocal", "resource"));
    let controls = run(
        "class Base:\n def execute(self, argument): raise NotImplementedError\nclass Child(Base):\n def execute(self, argument): return 1\ndef callback(ignored): ...\ndef closure(value):\n def inner(): return value\n return inner\n",
    );
    assert!(
        code(&controls, "PyUnusedParameter").is_empty(),
        "{controls:?}"
    );
    assert!(code(&controls, "PyUnusedLocalDefinition").is_empty());
}
#[test]
fn eager_global_reads_and_rhs_reads_are_distinct_from_deferred_function_reads() {
    let problems = run(
        "print(later)\nlater=1\nself_reference=self_reference+1\ndef deferred(): return future\nfuture=2\nprint(len([]))\nlen=42\n",
    );
    assert!(has(&problems, "PyGlobalUsedBeforeAssignment", "later"));
    assert!(has(
        &problems,
        "PyGlobalUsedBeforeAssignment",
        "self_reference"
    ));
    assert!(!has(&problems, "PyGlobalUsedBeforeAssignment", "future"));
    assert!(!has(&problems, "PyGlobalUsedBeforeAssignment", "len"));
    let annotations = run("from __future__ import annotations\nvalue: Later\nclass Later: pass\n");
    assert!(
        code(&annotations, "PyGlobalUsedBeforeAssignment").is_empty(),
        "{annotations:?}"
    );
}
#[test]
fn overwritten_values_do_not_confuse_rhs_reads_or_conditional_alternatives() {
    let problems = run(
        "def f():\n x=1\n x=2\n print(x)\n y=1\n y=y+1\n print(y)\n if flag:\n  z=1\n else:\n  z=2\n return z\n",
    );
    assert!(has(&problems, "PyRedeclaration", "'x'"), "{problems:?}");
    assert!(!has(&problems, "PyRedeclaration", "'y'"));
    assert!(!has(&problems, "PyRedeclaration", "'z'"));
    assert!(has(&run("import os\nos=1\n"), "PyImportShadowed", "os"));
}
#[test]
fn literal_getattr_references_respect_defaults_unknowns_and_local_replacements() {
    let source = "class Value:\n known:int=1\nvalue=Value()\ngetattr(value,'known')\ngetattr(value,'absent')\ngetattr(value,'optional',None)\nclass Dynamic:\n def __getattr__(self,name): return 1\ngetattr(Dynamic(),'allowed')\ndef query(unknown): return getattr(unknown,'anything')\n";
    let problems = run(source);
    assert!(
        has(&problems, "PyUnresolvedReferences", "attribute 'absent'"),
        "{problems:?}"
    );
    for name in ["known", "optional", "allowed", "anything"] {
        assert!(
            !has(
                &problems,
                "PyUnresolvedReferences",
                &format!("attribute '{name}'")
            ),
            "{problems:?}"
        );
    }
    assert!(!has(
        &run("def getattr(value,name): return name\ngetattr(1,'whatever')\n"),
        "PyUnresolvedReferences",
        "whatever"
    ));
}
#[test]
fn super_arguments_use_nominal_relationship_and_allow_one_argument_proxy() {
    let problems = run(
        "class Base: pass\nclass Child(Base): pass\nclass Other: pass\nsuper(Base,Child())\nsuper(Base)\nsuper(1,Child())\nsuper(Base,Other())\nsuper(Base,Child(),42)\n",
    );
    assert_eq!(code(&problems, "PySuperArguments").len(), 3, "{problems:?}");
    assert!(has(&problems, "PySuperArguments", "first argument"));
    assert!(has(&problems, "PySuperArguments", "second argument"));
    assert!(has(&problems, "PySuperArguments", "zero, one or two"));
    assert!(code(
        &run("def super(a,b): return a\nsuper(1,2)\n"),
        "PySuperArguments"
    )
    .is_empty());
}
#[test]
fn all_entries_are_validated_without_treating_unrelated_descendant_strings_as_exports() {
    assert!(has(
        &run("__all__='name'\n"),
        "PyInvalidAll",
        "list or tuple"
    ));
    assert!(has(
        &run("__all__=['good',1,b'bad']\ngood=1\n"),
        "PyInvalidAll",
        "only strings"
    ));
    let computed = run("visible=1\nitems=['visible','missing']\n__all__=items+[]\n");
    assert!(
        has(&computed, "PyUnresolvedReferences", "'missing' in __all__"),
        "{computed:?}"
    );
    assert!(!has(
        &computed,
        "PyUnresolvedReferences",
        "'visible' in __all__"
    ));
    let dynamic =
        run("def transform(text): return text\n__all__=[transform('not_an_export_declaration')]\n");
    assert!(
        !has(
            &dynamic,
            "PyUnresolvedReferences",
            "not_an_export_declaration"
        ),
        "{dynamic:?}"
    );
    assert!(code(&run("a=1\n__all__=(['a'])\n"), "PyInvalidAll").is_empty());
}
#[test]
fn trailing_semicolon_and_implicit_join_fixes_preserve_source_and_required_continuations() {
    let source = "π=(1 + \\\n 2); # comment ; \\\nrequired=1 + \\\n 2\ntext=';'\n";
    let problems = run(source);
    assert_eq!(
        code(&problems, "PyUnnecessaryBackslash").len(),
        1,
        "{problems:?}"
    );
    assert_eq!(code(&problems, "PyUnnecessarySemicolon").len(), 1);
    let no_slash = apply(
        source,
        &code(&problems, "PyUnnecessaryBackslash")[0].fixes[0],
    );
    assert!(no_slash.starts_with("π=(1 + \n 2);"));
    assert!(no_slash.contains("required=1 + \\\n 2"));
    let no_semicolon = apply(
        source,
        &code(&problems, "PyUnnecessarySemicolon")[0].fixes[0],
    );
    assert!(no_semicolon.contains(") # comment ;"));
    let separator = run("x=1; y=2\n");
    assert!(code(&separator, "PyUnnecessarySemicolon")[0]
        .primary_fix()
        .is_none());
}
#[test]
fn source_import_identity_cycles_wildcards_and_duplicates_are_connected() {
    let db = Analysis::for_project(Path::new("/references-imports"));
    db.set_file_text(Path::new("/references-imports/a.py"), "import b\nvalue=1\n");
    db.set_file_text(Path::new("/references-imports/b.py"), "import main\n");
    db.set_file_text(
        Path::new("/references-imports/exports.py"),
        "__all__=['value']\nvalue=1\n",
    );
    let id = db.set_file_text(
        Path::new("/references-imports/main.py"),
        "import main\nimport a\nimport os\nimport os\nfrom exports import *\n",
    );
    let file = db.file(id).unwrap();
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    assert!(
        has(&problems, "PyImportSelf", "containing file"),
        "{problems:?}"
    );
    assert!(has(&problems, "PyCyclicImport", "Cyclic import"));
    assert!(has(&problems, "PyDuplicateImport", "os"));
    assert!(has(&problems, "PyWildcardImport", "Wildcard"));
    assert!(has(&problems, "PyUnusedImports", "Unused wildcard"));
    let id = db.set_file_text(
        Path::new("/references-imports/main.py"),
        "from exports import *\nprint(value)\n",
    );
    let used = inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default());
    assert!(
        !has(&used, "PyUnusedImports", "Unused wildcard"),
        "{used:?}"
    );
}
#[test]
fn nested_import_notes_and_style_reuse_the_actual_import_optimizer() {
    let problems =
        run("import sys\nimport os\nprint(sys,os)\ndef f():\n import math\n return math.pi\n");
    assert!(has(&problems, "PyLocalImport", "function or class"));
    let order = code(&problems, "PyUnsortedImports");
    assert_eq!(order.len(), 1, "{problems:?}");
    assert_eq!(order[0].fixes[0].title, "Optimize imports");
    let source = "import sys\nimport os\nprint(sys,os)\n";
    let fixed = apply(source, &order[0].fixes[0]);
    assert!(fixed.starts_with("import os\nimport sys\n"), "{fixed}");
}
#[test]
fn maintained_name_member_method_and_unbound_consumers_remain_active() {
    let source = "class Value:\n def missing_receiver(): return 1\n def method(self):\n  return 1\nvalue=Value()\nvalue.unknown\nmissing_name\nself\ndef f(flag):\n if flag: local=1\n return local\n";
    let problems = run(source);
    for (name, text) in [
        ("PyUnresolvedReferences", "unknown"),
        ("PyUnresolvedReferences", "missing_name"),
        ("PySelfOutsideMethod", "self"),
        ("PyUnboundLocalVariable", "local"),
        ("PyMethodParameters", "first parameter"),
        ("PyMethodMayBeStatic", "method"),
    ] {
        assert!(has(&problems, name, text), "{name}: {problems:?}");
    }
    let underscore = run("def f(_ignored):\n _temporary=1\n return 1\n");
    assert!(code(&underscore, "PyUnusedParameter").is_empty());
    assert!(code(&underscore, "PyUnusedLocal").is_empty());
}

#[test]
fn loop_and_with_reassignments_preserve_acquired_value_reads() {
    let problems = run(
        "def work():\n for value in values:\n  value=42\n  print(value)\n with manager as resource:\n  resource=42\n  print(resource)\n",
    );
    assert!(
        has(&problems, "PyLoopOrWithVariableAssignment", "value"),
        "{problems:?}"
    );
    assert!(has(&problems, "PyLoopOrWithVariableAssignment", "resource"));
    let controls = run(
        "def work():\n for value in values:\n  value=normalize(value)\n  print(value)\n with manager as resource:\n  print(resource)\n  resource=42\n",
    );
    assert!(
        code(&controls, "PyLoopOrWithVariableAssignment").is_empty(),
        "{controls:?}"
    );
}

#[test]
fn overwrite_checks_use_statement_owners_and_preserve_conditional_reads() {
    let imports = run("from os import path as location\nlocation=42\n");
    assert!(has(&imports, "PyImportShadowed", "location"), "{imports:?}");
    let imports = run(
        "import os\nos=os.path\nfrom os import path as location\nlocation=location.abspath('.')\n",
    );
    assert!(code(&imports, "PyImportShadowed").is_empty(), "{imports:?}");
    let targets = run("def work():\n with first as a, second as b:\n  a=1\n  b=2\n");
    assert!(has(&targets, "PyLoopOrWithVariableAssignment", "'a'"));
    assert!(has(&targets, "PyLoopOrWithVariableAssignment", "'b'"));
    let conditional = run("def work():\n for value in values:\n  if condition:\n   value=42\n  print(value)\n with manager as resource:\n  if condition:\n   resource=42\n  print(resource)\n");
    assert!(
        code(&conditional, "PyLoopOrWithVariableAssignment").is_empty(),
        "{conditional:?}"
    );
}
fn write_tree(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}
fn inspect_on_disk(db: &Analysis, path: &Path) -> Vec<Problem> {
    let text = std::fs::read_to_string(path).unwrap();
    let id = db.set_file_text(path, &text);
    inspections::inspect(db, &db.file(id).unwrap(), &InspectionOptions::default())
}
#[test]
fn imports_found_only_relative_to_a_file_outside_project_roots_are_reported() {
    let tmp = tempfile::TempDir::with_prefix("module-not-on-path").unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    write_tree(
        &base,
        &[
            ("proj/pkg/__init__.py", ""),
            (
                "proj/pkg/a.py",
                "from . import b\nfrom .b import value\nimport os\nimport pkg.b\n",
            ),
            ("proj/pkg/b.py", "value = 1\n"),
            ("outside/main.py", "from .helper import value\nimport os\n"),
            ("outside/helper.py", "value = 1\n"),
        ],
    );
    let db = Analysis::for_project(&base.join("proj"));
    let outside = inspect_on_disk(&db, &base.join("outside/main.py"));
    let hits = code(&outside, "PyModuleNotOnPath");
    assert_eq!(hits.len(), 1, "{outside:?}");
    assert!(hits[0].diagnostic.message.contains("'helper'"), "{hits:?}");
    assert!(hits[0]
        .fixes
        .iter()
        .any(|fix| fix.title.starts_with("Suppress")));
    let inside = inspect_on_disk(&db, &base.join("proj/pkg/a.py"));
    assert!(code(&inside, "PyModuleNotOnPath").is_empty(), "{inside:?}");
    // Without any configured roots nothing can be judged outside the project.
    let unconfigured = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        Vec::new(),
        Vec::new(),
        (3, 12),
    ));
    let bare = inspect_on_disk(&unconfigured, &base.join("outside/main.py"));
    assert!(code(&bare, "PyModuleNotOnPath").is_empty(), "{bare:?}");
}
#[test]
fn project_files_shadowing_library_modules_and_init_submodule_rebinds_are_reported() {
    let tmp = tempfile::TempDir::with_prefix("import-shadows-module").unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    write_tree(
        &base,
        &[
            ("proj/random.py", "def shuffle(x):\n    return x\n"),
            ("proj/main.py", "import random\nimport os\nfrom random import shuffle\nimport pkg\nprint(random, os, shuffle, pkg)\n"),
            ("proj/pkg/__init__.py", "from .impl import config\nfrom . import helpers\nfrom .version import version\nimport json\nfrom .impl import other as renamed\n"),
            ("proj/pkg/impl.py", "config = 1\nother = 2\n"),
            ("proj/pkg/config.py", ""),
            ("proj/pkg/helpers.py", ""),
            ("proj/pkg/version.py", "version = '1'\n"),
            ("proj/pkg/json.py", ""),
        ],
    );
    let db = Analysis::for_project(&base.join("proj"));
    let main = inspect_on_disk(&db, &base.join("proj/main.py"));
    let hits = code(&main, "PyImportShadowsModule");
    assert_eq!(hits.len(), 2, "{main:?}");
    assert!(hits.iter().all(|p| p
        .diagnostic
        .message
        .contains("standard library module 'random'")));
    assert!(code(&main, "PyImportShadowed").is_empty(), "{main:?}");
    let shadowing_file = inspect_on_disk(&db, &base.join("proj/random.py"));
    assert!(code(&shadowing_file, "PyImportShadowsModule").is_empty());
    let init = inspect_on_disk(&db, &base.join("proj/pkg/__init__.py"));
    let mut names: Vec<_> = code(&init, "PyImportShadowsModule")
        .iter()
        .map(|p| p.diagnostic.message.clone())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "{init:?}");
    assert!(
        names[0].contains("'config'") && names[0].contains("'pkg.config'"),
        "{names:?}"
    );
    assert!(
        names[1].contains("'json'") && names[1].contains("'pkg.json'"),
        "{names:?}"
    );
}
