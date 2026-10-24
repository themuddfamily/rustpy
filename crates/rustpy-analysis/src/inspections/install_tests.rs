//! "Install package" quick fix for unresolved imports, and module lookups
//! recovering once the package is installed.

use std::path::Path;

use super::{inspect, install_package_target, install_package_title, InspectionOptions};
use rustpy_semantic::db::Analysis;

/// `(message, install fix titles)` of the "No module named" problems.
fn missing_modules(db: &Analysis, path: &Path, src: &str) -> Vec<(String, Vec<String>)> {
    let id = db.set_file_text(path, src);
    let f = db.file(id).unwrap();
    inspect(db, &f, &InspectionOptions::default())
        .into_iter()
        .filter(|p| p.diagnostic.message.starts_with("No module named"))
        .map(|p| {
            (
                p.diagnostic.message,
                p.fixes
                    .iter()
                    .filter(|f| install_package_target(&f.title).is_some())
                    .map(|f| f.title.clone())
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn unresolved_imports_offer_the_distribution_first() {
    let db = Analysis::for_project(Path::new("/proj"));
    let src = "import yaml\nimport requests\nfrom PIL import Image\nimport sklearn.linear_model\n";
    let got = missing_modules(&db, Path::new("/proj/m.py"), src);
    let titles: Vec<&str> = got
        .iter()
        .flat_map(|(_, t)| t.iter().map(String::as_str))
        .collect();
    assert_eq!(
        titles,
        vec![
            "Install package 'PyYAML'",
            "Install package 'requests'",
            "Install package 'Pillow'",
            "Install package 'scikit-learn'"
        ],
        "{got:?}"
    );
    // The install fix comes first (suppressions follow).
    let id = db.set_file_text(Path::new("/proj/m.py"), "import yaml\n");
    let f = db.file(id).unwrap();
    let probs = inspect(&db, &f, &InspectionOptions::default());
    let p = probs
        .iter()
        .find(|p| p.diagnostic.message == "No module named 'yaml'")
        .unwrap();
    assert_eq!(p.fixes[0].title, "Install package 'PyYAML'");
    assert!(p.fixes[0].edits.is_empty());
    assert_eq!(
        install_package_target(&install_package_title("PyYAML")),
        Some("PyYAML")
    );
}

#[test]
fn namespace_packages_install_their_own_distribution() {
    let dir = tempfile::TempDir::with_prefix("rustpy_install").unwrap();
    let (proj, site) = (dir.path().join("proj"), dir.path().join("site"));
    std::fs::create_dir_all(&proj).unwrap();
    // Another `google.cloud` distribution is installed: `google` and
    // `google.cloud` resolve as namespace packages.
    std::fs::create_dir_all(site.join("google/cloud/storage")).unwrap();
    std::fs::write(site.join("google/cloud/storage/__init__.py"), "").unwrap();
    let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        vec![proj.clone()],
        vec![site.clone()],
        (3, 12),
    ));
    let src = "import google.cloud.firestore\nimport google.colab\nimport google.cloud.storage\n";
    let got = missing_modules(&db, &proj.join("m.py"), src);
    assert_eq!(
        got.iter()
            .find(|(m, _)| m == "No module named 'google.cloud.firestore'")
            .map(|(_, t)| t.clone()),
        Some(vec!["Install package 'google-cloud-firestore'".to_string()]),
        "{got:?}"
    );
    // No distribution is guessed for an unknown part of a namespace package.
    assert!(
        got.iter()
            .all(|(m, t)| !m.contains("google.colab") || t.is_empty()),
        "{got:?}"
    );
    assert!(
        !got.iter().any(|(m, _)| m.contains("google.cloud.storage")),
        "{got:?}"
    );
    // `from <namespace> import <subpackage>` with nothing of the namespace installed.
    let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        vec![proj.clone()],
        Vec::new(),
        (3, 12),
    ));
    let got = missing_modules(
        &db,
        &proj.join("m.py"),
        "from google.cloud import firestore\nfrom google import colab\n",
    );
    let titles: Vec<String> = got.iter().flat_map(|(_, t)| t.clone()).collect();
    assert_eq!(
        titles,
        vec!["Install package 'google-cloud-firestore'".to_string()],
        "{got:?}"
    );
}

#[test]
fn no_install_fix_for_stdlib_relative_or_partial_imports() {
    let dir = tempfile::TempDir::with_prefix("rustpy_install").unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    std::fs::write(root.join("pkg/__init__.py"), "").unwrap();
    let db = Analysis::for_project(root);
    // A missing submodule of a project package, a relative import, a stdlib
    // module removed for the target version (reported once, by PyDeprecation).
    let src =
        "import pkg.missing\nfrom . import nothing_here\nfrom .gone import x\nimport distutils\n";
    let got = missing_modules(&db, &root.join("m.py"), src);
    let messages: Vec<&str> = got.iter().map(|(m, _)| m.as_str()).collect();
    assert!(
        messages.contains(&"No module named 'pkg.missing'"),
        "{got:?}"
    );
    assert!(
        !messages.contains(&"No module named 'distutils'"),
        "{got:?}"
    );
    assert!(got.iter().all(|(_, titles)| titles.is_empty()), "{got:?}");
}

#[test]
fn installed_packages_resolve_after_invalidation() {
    let dir = tempfile::TempDir::with_prefix("rustpy_install").unwrap();
    let (proj, site) = (dir.path().join("proj"), dir.path().join("site"));
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&site).unwrap();
    let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        vec![proj.clone()],
        vec![site.clone()],
        (3, 12),
    ));
    let path = proj.join("m.py");
    assert_eq!(missing_modules(&db, &path, "import yaml\n").len(), 1);
    // pip installs the package: the failed lookup is cached until invalidated.
    std::fs::create_dir_all(site.join("yaml")).unwrap();
    std::fs::write(
        site.join("yaml/__init__.py"),
        "def safe_load(stream): ...\n",
    )
    .unwrap();
    assert_eq!(missing_modules(&db, &path, "import yaml\n").len(), 1);
    db.invalidate_modules();
    assert!(missing_modules(&db, &path, "import yaml\nyaml.safe_load('')\n").is_empty());
}
