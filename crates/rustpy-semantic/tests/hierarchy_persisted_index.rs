//! Hierarchy queries (subclasses, Go to Implementation, method hierarchy)
//! answer the same whether or not the persisted project index supplies class
//! bases, including for files edited since they were indexed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustpy_index::{build, IndexMeta, RootKind, RootSpec, SourceFile, SymbolIndex};
use rustpy_semantic::db::Analysis;
use rustpy_semantic::hierarchy;

const FILES: &[(&str, &str)] = &[
    ("base.py", "class Base:\n    def run(self): pass\n\nclass Other: pass\n"),
    (
        "child.py",
        "from base import Base as Parent\nclass Child(Parent):\n    def run(self): pass\n",
    ),
    (
        "grand.py",
        "import child\nclass Grand(child.Child):\n    def run(self): pass\n",
    ),
    ("pkg/__init__.py", "from .impl import Base as Widget\n"),
    ("pkg/impl.py", "from base import Base\n"),
    (
        "user.py",
        "from pkg import Widget\nclass ViaReexport(Widget): pass\n",
    ),
    (
        "alias.py",
        "from base import Base\nAlias = Base\nclass ViaVariable(Alias): pass\n",
    ),
    (
        "local.py",
        "from base import Base\ndef make():\n    class Local(Base):\n        def run(self): pass\n    return Local\n",
    ),
    (
        "generic.py",
        "from typing import Generic, TypeVar\nfrom base import Other\nT = TypeVar('T')\nclass Box(Other, Generic[T]): pass\n",
    ),
    ("unrelated.py", "class Plain(object): pass\nclass Err(Exception): pass\n"),
];

fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (rel, text) in FILES {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    (dir, root)
}

fn index_of(root: &Path) -> Arc<SymbolIndex> {
    let roots = [RootSpec {
        path: root.to_path_buf(),
        kind: RootKind::Project,
    }];
    let files = FILES
        .iter()
        .map(|(rel, _)| SourceFile {
            root: 0,
            path: root.join(rel),
        })
        .collect();
    let (index, _) = build::refresh(
        None,
        IndexMeta::default(),
        &roots,
        files,
        &[],
        &build::BuildOptions::default(),
        &build::Progress::default(),
    )
    .unwrap();
    Arc::new(index.unwrap())
}

fn analysis(root: &Path) -> Analysis {
    let db = Analysis::for_project(root);
    for (rel, text) in FILES {
        db.set_file_text(&root.join(rel), text);
    }
    db
}

/// Every hierarchy answer for the classes and `run` methods of the project.
fn answers(db: &Analysis, root: &Path) -> Vec<String> {
    let paths: Vec<PathBuf> = FILES.iter().map(|(rel, _)| root.join(rel)).collect();
    let mut out = Vec::new();
    for path in &paths {
        let file = db.file_for_path(path).unwrap();
        for (at, _) in file.text.match_indices("class ") {
            let offset = rustpy_core::TextSize::from((at + 6) as u32);
            let implementations = hierarchy::implementations(db, &file, offset, &paths);
            out.push(format!(
                "{}@{at}: impl {:?}",
                path.strip_prefix(root).unwrap().display(),
                implementations
                    .iter()
                    .map(|l| (l.path.strip_prefix(root).unwrap().to_path_buf(), l.range))
                    .collect::<Vec<_>>()
            ));
            if let Some(tree) = hierarchy::subtypes(db, &file, offset, &paths) {
                out.push(format!("{tree:?}"));
            }
        }
        for (at, _) in file.text.match_indices("def run") {
            let offset = rustpy_core::TextSize::from((at + 4) as u32);
            out.push(format!(
                "{:?}",
                hierarchy::implementations(db, &file, offset, &paths)
            ));
            out.push(format!(
                "{:?}",
                hierarchy::method_hierarchy(db, &file, offset, &paths)
            ));
        }
    }
    out
}

fn subclass_names(db: &Analysis, root: &Path, rel: &str, class: &str) -> Vec<String> {
    let paths: Vec<PathBuf> = FILES.iter().map(|(rel, _)| root.join(rel)).collect();
    let files: Vec<_> = paths.iter().filter_map(|p| db.file_for_path(p)).collect();
    let file = db.file_for_path(&root.join(rel)).unwrap();
    let class = file
        .class_declarations()
        .iter()
        .find(|c| c.name == class)
        .unwrap()
        .clone();
    hierarchy::direct_subclasses(db, &class, &files)
        .iter()
        .map(|c| c.name.to_string())
        .collect()
}

#[test]
fn persisted_bases_give_the_same_hierarchy() {
    let (_dir, root) = project();
    let index = index_of(&root);
    let plain = analysis(&root);
    let indexed = analysis(&root);
    indexed.set_project_index(Some(index.clone()));
    let expected = answers(&plain, &root);
    assert_eq!(answers(&indexed, &root), expected);
    // Aliases (imported, re-exported, assigned) and classes inside functions
    // are found whatever their recorded base names say.
    assert_eq!(
        subclass_names(&indexed, &root, "base.py", "Base"),
        ["Child", "ViaReexport", "ViaVariable", "Local"]
    );
    assert_eq!(
        subclass_names(&indexed, &root, "child.py", "Child"),
        ["Grand"]
    );
    assert_eq!(subclass_names(&indexed, &root, "base.py", "Other"), ["Box"]);

    // Edited since indexing: the current text decides, not the index.
    for db in [&plain, &indexed] {
        db.set_file_text(
            &root.join("child.py"),
            "from base import Other as Parent\nclass Child(Parent):\n    def run(self): pass\n",
        );
        db.set_file_text(
            &root.join("unrelated.py"),
            "from base import Base\nclass Plain(Base): pass\nclass Err(Exception): pass\n",
        );
    }
    assert_eq!(answers(&indexed, &root), answers(&plain, &root));
    assert_eq!(
        subclass_names(&indexed, &root, "base.py", "Base"),
        ["ViaReexport", "ViaVariable", "Local", "Plain"]
    );
    assert_eq!(
        subclass_names(&indexed, &root, "base.py", "Other"),
        ["Child", "Box"]
    );

    // Withdrawing the index keeps the answers.
    indexed.set_project_index(None);
    assert_eq!(answers(&indexed, &root), answers(&plain, &root));
}
