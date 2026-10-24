//! Marketplace catalog parsing, update detection, and downloading + installing
//! against a local mock HTTP server (no real network).

mod common;

use common::{example_dir, plugins_dir_entries, write_plugin, MockServer, Route};
use rustpy_plugins::{
    download, download_and_install, pack_bundle, sha256_hex, shipped_bundle, Catalog, CatalogEntry,
    PluginError, PluginRegistry,
};
use serde_json::json;

const EXAMPLE_ID: &str = "com.example.solarized-theme";

fn entry(id: &str, version: &str, url: &str, sha256: &str) -> CatalogEntry {
    CatalogEntry {
        id: id.into(),
        name: format!("Plugin {id}"),
        version: version.into(),
        download_url: url.into(),
        sha256: sha256.into(),
        ..CatalogEntry::default()
    }
}

#[test]
fn bundled_catalog_lists_four_valid_examples() {
    let catalog = Catalog::bundled();
    assert!(catalog.problems.is_empty(), "{:?}", catalog.problems);
    assert_eq!(catalog.plugins.len(), 4);
    for e in &catalog.plugins {
        assert!(e.problems().is_empty(), "{}: {:?}", e.id, e.problems());
        assert!(
            !e.description.is_empty()
                && !e.vendor.is_empty()
                && !e.tags.is_empty()
                && e.homepage.is_some()
        );
        assert!(e.is_compatible());
    }
    assert!(catalog.latest(EXAMPLE_ID).is_some());
    assert_eq!(catalog.listing().len(), 4);
}

#[test]
fn parsing_skips_invalid_entries_and_explains_why() {
    let sha = "AB".repeat(32);
    let catalog = Catalog::parse(
        &json!({"plugins": [
            {"id": "com.example.good", "name": "Good", "version": "1.0.0", "download_url": "https://example.com/good.rpplugin", "sha256": sha, "tags": ["x"], "future_field": true},
            {"id": "com.example.bad-sha", "name": "Bad", "version": "1.0.0", "download_url": "https://example.com/b", "sha256": "1234"},
            {"id": "Not An Id", "name": "Bad", "version": "1.0.0", "download_url": "https://example.com/b", "sha256": sha},
            {"id": "com.example.bad-url", "name": "Bad", "version": "1.0.0", "download_url": "file:///etc/passwd", "sha256": sha},
            {"id": "com.example.good", "name": "Good again", "version": "1.0", "download_url": "https://example.com/good.rpplugin", "sha256": sha},
            {"id": "com.example.typed", "name": "Typed", "version": 3},
            "not an object"
        ]})
        .to_string(),
    )
    .unwrap();
    assert_eq!(catalog.plugins.len(), 1);
    assert_eq!(
        catalog.plugins[0].sha256,
        "ab".repeat(32),
        "normalized to lowercase"
    );
    let problems = catalog.problems.join("\n");
    for needle in [
        "plugins[1] (`com.example.bad-sha`): sha256: must be 64 hexadecimal digits",
        "plugins[2] (`Not An Id`): id:",
        "plugins[3] (`com.example.bad-url`): download_url: `file:///etc/passwd` is not an http(s) URL",
        "plugins[4]: `com.example.good` 1.0 is listed more than once",
        "plugins[5]: invalid type",
        "plugins[6]: invalid type",
    ] {
        assert!(problems.contains(needle), "missing {needle:?} in\n{problems}");
    }

    assert!(Catalog::parse("{").is_err());
    assert!(Catalog::parse("[]").is_err());
    assert!(Catalog::parse(r#"{"plugins": {}}"#).is_err());
    assert!(Catalog::parse("{}").unwrap().plugins.is_empty());
}

#[test]
fn updates_compare_installed_versions() {
    let sha = "0".repeat(64);
    let url = "https://example.com/x.rpplugin";
    let mut future = entry("com.example.c", "9.0.0", url, &sha);
    future.min_ide_version = Some("999.0".into());
    let catalog = Catalog {
        plugins: vec![
            entry("com.example.a", "1.2.0", url, &sha),
            entry("com.example.a", "1.10.0", url, &sha),
            entry("com.example.a", "1.0.0", url, &sha),
            entry("com.example.b", "2.0.0", url, &sha),
            entry("com.example.c", "1.1.0", url, &sha),
            future,
            entry("com.example.d", "0.5.0", url, &sha),
        ],
        problems: Vec::new(),
    };
    let updates = catalog.updates_for([
        ("com.example.a", "1.2.0"),
        ("com.example.b", "2.0"),
        ("com.example.c", "1.0.0"),
        ("com.example.d", "1.0.0"),
        ("com.example.e", "1.0.0"),
        ("com.example.f", "broken"),
    ]);
    let found: Vec<(&str, &str, &str)> = updates
        .iter()
        .map(|u| {
            (
                u.id.as_str(),
                u.installed_version.as_str(),
                u.entry.version.as_str(),
            )
        })
        .collect();
    // a: 1.10.0 is the newest; b: same version; c: 9.0.0 needs a newer IDE, 1.1.0 is offered;
    // d: the catalog is older; e: not in the catalog; f: unreadable installed version.
    assert_eq!(
        found,
        vec![
            ("com.example.a", "1.2.0", "1.10.0"),
            ("com.example.c", "1.0.0", "1.1.0")
        ]
    );

    // The same through a registry.
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    registry.install_from_dir(&example_dir()).unwrap();
    let catalog = Catalog {
        plugins: vec![
            entry(EXAMPLE_ID, "1.0.1", url, &sha),
            entry("com.example.other", "5.0.0", url, &sha),
        ],
        problems: Vec::new(),
    };
    let updates = catalog.updates(&registry);
    assert_eq!(updates.len(), 1);
    assert_eq!(
        (
            updates[0].id.as_str(),
            updates[0].installed_version.as_str()
        ),
        (EXAMPLE_ID, "1.0.0")
    );
    assert_eq!(updates[0].entry.version, "1.0.1");
    assert!(Catalog {
        plugins: vec![entry(EXAMPLE_ID, "1.0.0", url, &sha)],
        problems: vec![]
    }
    .updates(&registry)
    .is_empty());
}

#[test]
fn search_is_case_insensitive_over_names_descriptions_and_tags() {
    let catalog = Catalog::bundled();
    assert_eq!(catalog.search("").len(), 4);
    let theme: Vec<&str> = catalog
        .search("SOLARIZED")
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(theme, vec![EXAMPLE_ID]);
    assert!(catalog
        .search("keymap")
        .iter()
        .any(|e| e.id == "com.example.emacs-keymap"));
    assert!(catalog.search("no such plugin anywhere").is_empty());
    let names: Vec<&str> = catalog.listing().iter().map(|e| e.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_by_key(|n| n.to_lowercase());
    assert_eq!(names, sorted, "the listing is sorted by name");
}

#[test]
fn fetch_reads_a_catalog_over_http() {
    let body = json!({"plugins": [
        {"id": "com.example.remote", "name": "Remote", "version": "3.1.4", "description": "From the server",
         "vendor": "Mock", "download_url": "https://example.com/r.rpplugin", "sha256": "f".repeat(64), "tags": ["remote"]}
    ]})
    .to_string();
    let server = MockServer::start(vec![
        Route::ok("/catalog.json", body),
        Route::status("/broken.json", 500),
        Route::ok("/garbage", "<html>"),
    ]);

    let catalog = Catalog::fetch(&server.url("/catalog.json")).unwrap();
    assert_eq!(catalog.plugins.len(), 1);
    assert_eq!(catalog.plugins[0].version, "3.1.4");

    let err = Catalog::fetch(&server.url("/missing.json")).unwrap_err();
    assert!(matches!(&err, PluginError::Download { .. }), "{err}");
    assert!(err.to_string().contains("HTTP 404"), "{err}");
    assert!(Catalog::fetch(&server.url("/broken.json"))
        .unwrap_err()
        .to_string()
        .contains("HTTP 500"));
    assert!(matches!(
        Catalog::fetch(&server.url("/garbage")),
        Err(PluginError::Json { .. })
    ));
    assert!(Catalog::fetch("ftp://example.com/catalog.json")
        .unwrap_err()
        .to_string()
        .contains("not an http(s) URL"));
    assert_eq!(server.hits(), 4);
}

#[test]
fn an_unreachable_catalog_is_reported_once_with_its_url() {
    // A port nothing listens on: the connection is refused.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("http://127.0.0.1:{port}/catalog.json");
    let err = Catalog::fetch(&url).unwrap_err();
    assert!(matches!(&err, PluginError::Download { .. }), "{err}");
    let text = err.to_string();
    assert_eq!(text.matches(&url).count(), 1, "{text}");
    assert!(text.contains("Connection Failed"), "{text}");
}

#[test]
fn download_and_install_verifies_the_checksum() {
    let bundle = pack_bundle(&example_dir()).unwrap();
    let sha = sha256_hex(&bundle);
    let server = MockServer::start(vec![Route::ok("/solarized-1.0.0.rpplugin", bundle.clone())]);
    let url = server.url("/solarized-1.0.0.rpplugin");
    let config = tempfile::tempdir().unwrap();

    // A wrong checksum installs nothing.
    let tampered = entry(EXAMPLE_ID, "1.0.0", &url, &"0".repeat(64));
    match download_and_install(&tampered, config.path()) {
        Err(PluginError::ChecksumMismatch {
            expected, actual, ..
        }) => {
            assert_eq!(expected, "0".repeat(64));
            assert_eq!(actual, sha);
        }
        other => panic!("expected a checksum mismatch, got {other:?}"),
    }
    assert!(PluginRegistry::load(config.path()).plugins().is_empty());
    assert!(
        plugins_dir_entries(config.path()).is_empty(),
        "{:?}",
        plugins_dir_entries(config.path())
    );

    // The right checksum (in any case) installs the plugin.
    let good = entry(EXAMPLE_ID, "1.0.0", &url, &sha.to_uppercase());
    let outcome = download_and_install(&good, config.path()).unwrap();
    assert_eq!(
        (
            outcome.id.as_str(),
            outcome.version.as_str(),
            outcome.previous_version.as_deref()
        ),
        (EXAMPLE_ID, "1.0.0", None)
    );
    let registry = PluginRegistry::load(config.path());
    assert!(registry.get(EXAMPLE_ID).unwrap().is_active());
    assert_eq!(
        registry.contributions().color_schemes[0].scheme.name,
        "Solarized Dark (Plugin)"
    );
    assert_eq!(plugins_dir_entries(config.path()), vec![EXAMPLE_ID]);
    assert_eq!(
        server.requested_paths(),
        vec!["/solarized-1.0.0.rpplugin"; 2]
    );

    // Installing the same version again is refused before downloading.
    assert!(matches!(
        download_and_install(&good, config.path()),
        Err(PluginError::AlreadyInstalled { .. })
    ));
    assert_eq!(server.hits(), 2);

    // `download` alone returns the verified bytes.
    assert_eq!(download(&good).unwrap(), bundle);
}

#[test]
fn download_and_install_upgrades_and_checks_the_bundle_matches_the_entry() {
    let src = tempfile::tempdir().unwrap();
    let v1 = pack_bundle(&write_plugin(
        src.path(),
        "v1",
        json!({"id": "com.example.up", "version": "1.0.0"}),
        &[],
    ))
    .unwrap();
    let v2 = pack_bundle(&write_plugin(
        src.path(),
        "v2",
        json!({"id": "com.example.up", "version": "2.0.0"}),
        &[],
    ))
    .unwrap();
    let server = MockServer::start(vec![
        Route::ok("/v1", v1.clone()),
        Route::ok("/v2", v2.clone()),
        Route::ok("/junk", "junk"),
    ]);
    let config = tempfile::tempdir().unwrap();

    download_and_install(
        &entry(
            "com.example.up",
            "1.0.0",
            &server.url("/v1"),
            &sha256_hex(&v1),
        ),
        config.path(),
    )
    .unwrap();

    // The catalog promises 3.0.0 but serves 2.0.0: refused.
    let lying = entry(
        "com.example.up",
        "3.0.0",
        &server.url("/v2"),
        &sha256_hex(&v2),
    );
    let err = download_and_install(&lying, config.path()).unwrap_err();
    assert!(
        matches!(&err, PluginError::Bundle(m) if m.contains("com.example.up 2.0.0") && m.contains("com.example.up 3.0.0")),
        "{err}"
    );
    // A different plugin under this id: refused.
    let wrong_id = entry(
        "com.example.other",
        "2.0.0",
        &server.url("/v2"),
        &sha256_hex(&v2),
    );
    assert!(matches!(
        download_and_install(&wrong_id, config.path()),
        Err(PluginError::Bundle(_))
    ));
    // Not a bundle at all (checksum fine): refused.
    let junk = entry(
        "com.example.junk",
        "1.0.0",
        &server.url("/junk"),
        &sha256_hex(b"junk"),
    );
    assert!(download_and_install(&junk, config.path())
        .unwrap_err()
        .to_string()
        .contains("not valid JSON"));

    let outcome = download_and_install(
        &entry(
            "com.example.up",
            "2.0.0",
            &server.url("/v2"),
            &sha256_hex(&v2),
        ),
        config.path(),
    )
    .unwrap();
    assert_eq!(outcome.previous_version.as_deref(), Some("1.0.0"));
    let registry = PluginRegistry::load(config.path());
    assert_eq!(
        registry.get("com.example.up").unwrap().manifest.version,
        "2.0.0"
    );
    assert_eq!(registry.plugins().len(), 1);
    assert_eq!(plugins_dir_entries(config.path()), vec!["com.example.up"]);
}

#[test]
fn download_errors() {
    let server = MockServer::start(vec![Route::status("/oops", 500)]);
    let config = tempfile::tempdir().unwrap();
    let sha = "a".repeat(64);

    let err = download_and_install(
        &entry("com.example.x", "1.0.0", &server.url("/oops"), &sha),
        config.path(),
    )
    .unwrap_err();
    assert!(
        matches!(&err, PluginError::Download { message, .. } if message.contains("HTTP 500")),
        "{err}"
    );
    let err = download_and_install(
        &entry("com.example.x", "1.0.0", &server.url("/nope"), &sha),
        config.path(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("HTTP 404"), "{err}");

    // Invalid entries never reach the network.
    let hits = server.hits();
    let err = download(&entry("com.example.x", "1.0.0", "file:///etc/passwd", &sha)).unwrap_err();
    assert!(
        matches!(&err, PluginError::Catalog(m) if m.contains("download_url")),
        "{err}"
    );
    assert!(download(&entry(
        "com.example.x",
        "1.0.0",
        &server.url("/oops"),
        "short"
    ))
    .is_err());
    let mut incompatible = entry("com.example.x", "1.0.0", &server.url("/oops"), &sha);
    incompatible.min_ide_version = Some("999.0".into());
    assert!(matches!(
        download_and_install(&incompatible, config.path()),
        Err(PluginError::Incompatible { .. })
    ));
    assert_eq!(server.hits(), hits);

    // Nothing reachable on a closed port: a transport error, quickly.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/x", closed.local_addr().unwrap());
    drop(closed);
    let err = download(&entry("com.example.x", "1.0.0", &url, &sha)).unwrap_err();
    assert!(matches!(err, PluginError::Download { .. }), "{err}");
    assert!(PluginRegistry::load(config.path()).plugins().is_empty());
}

/// The example plugins of the bundled catalog: (directory under `examples/`, id, version).
const SHIPPED: &[(&str, &str, &str)] = &[
    ("solarized-theme", "com.example.solarized-theme", "1.0.0"),
    ("emacs-keymap", "com.example.emacs-keymap", "0.3.1"),
    ("pytest-kit", "com.example.pytest-kit", "2.1.0"),
    ("csv-table", "org.rustpy.csv-table", "1.0.0"),
];

/// `assets/bundles/` holds the packed examples and `assets/catalog.json` lists
/// their SHA-256. After changing an example, run
/// `RUSTPY_UPDATE_BUNDLES=1 cargo test -p rustpy-plugins --test catalog shipped`
/// and copy the printed checksums into the catalog.
#[cfg(unix)] // packing records Unix executable bits
#[test]
fn shipped_bundles_are_the_packed_examples() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let catalog = Catalog::bundled();
    let update = std::env::var_os("RUSTPY_UPDATE_BUNDLES").is_some();
    let mut problems = Vec::new();
    for (dir, id, version) in SHIPPED {
        let packed = pack_bundle(&root.join("examples").join(dir)).unwrap();
        let path = root
            .join("assets/bundles")
            .join(format!("{id}-{version}.rpplugin"));
        if update {
            std::fs::write(&path, &packed).unwrap();
        }
        if std::fs::read(&path).unwrap_or_default() != packed {
            problems.push(format!(
                "{} is not the packed examples/{dir}",
                path.display()
            ));
        }
        let sha = sha256_hex(&packed);
        eprintln!("{id} {version}: sha256 {sha}");
        match catalog
            .plugins
            .iter()
            .find(|e| e.id == *id && e.version == *version)
        {
            Some(e) if e.sha256 == sha => {}
            Some(e) => problems.push(format!(
                "catalog.json lists {id} {version} with sha256 {}, the bundle's is {sha}",
                e.sha256
            )),
            None => problems.push(format!("catalog.json does not list {id} {version}")),
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_bundled_catalog_installs_without_a_network() {
    let config = tempfile::tempdir().unwrap();
    let catalog = Catalog::bundled();
    for entry in catalog.listing() {
        assert!(shipped_bundle(entry).is_some(), "{} is shipped", entry.id);
        // The download URLs do not resolve; only the shipped bundles can install.
        let outcome = download_and_install(entry, config.path()).unwrap();
        assert_eq!(
            (outcome.id.as_str(), outcome.version.as_str()),
            (entry.id.as_str(), entry.version.as_str())
        );
    }
    let registry = PluginRegistry::load(config.path());
    assert_eq!(registry.plugins().len(), SHIPPED.len());
    for plugin in registry.plugins() {
        assert!(plugin.is_active(), "{}: {:?}", plugin.id, plugin.errors);
        assert!(
            plugin.warnings.is_empty(),
            "{}: {:?}",
            plugin.id,
            plugin.warnings
        );
    }
    let c = registry.contributions();
    assert!(c.color_scheme("Solarized Dark (Plugin)").is_some());
    assert_eq!(
        c.keymap("Emacs").unwrap().bindings["GotoAction"],
        ["Ctrl+Shift+A", "Alt+X"]
    );
    let names: Vec<&str> = c.file_templates.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Pytest Module", "Pytest conftest.py"]);
    assert_eq!(c.external_tools[0].tool.name, "pytest current file");
    assert_eq!(c.external_tools[0].tool.group, "Pytest");
    let abbreviations: Vec<String> = c
        .all_live_templates()
        .into_iter()
        .map(|t| t.abbreviation)
        .collect();
    for abbr in ["bp", "logger", "todo", "fixture", "param", "raises"] {
        assert!(
            abbreviations.iter().any(|a| a == abbr),
            "{abbr} in {abbreviations:?}"
        );
    }

    // The same bundle listed by another catalog (another URL) is downloaded.
    let mut mirrored = catalog.plugins[0].clone();
    mirrored.download_url = "https://mirror.example.org/x.rpplugin".into();
    assert!(shipped_bundle(&mirrored).is_none());
    let mut tampered = catalog.plugins[0].clone();
    tampered.sha256 = "0".repeat(64);
    assert!(shipped_bundle(&tampered).is_none());
}

#[test]
fn shipped_csv_catalog_install_registers_the_table_editor_and_disable_retires_it() {
    let catalog = Catalog::bundled();
    let entry = catalog.latest("org.rustpy.csv-table").unwrap();
    assert_eq!(entry.version, "1.0.0");
    assert!(catalog
        .search("csv")
        .iter()
        .any(|entry| entry.id == "org.rustpy.csv-table"));
    let config = tempfile::tempdir().unwrap();
    download_and_install(entry, config.path()).unwrap();
    let mut registry = PluginRegistry::load(config.path());
    let contributions = registry.contributions();
    assert_eq!(contributions.table_editors.len(), 1);
    let editor = &contributions.table_editors[0];
    assert_eq!(editor.plugin_id, "org.rustpy.csv-table");
    assert_eq!(editor.extensions, ["csv"]);
    assert_eq!(editor.kind, rustpy_plugins::TableEditorKind::Csv);
    registry.disable("org.rustpy.csv-table").unwrap();
    assert!(registry.contributions().table_editors.is_empty());
    registry.enable("org.rustpy.csv-table").unwrap();
    assert_eq!(
        PluginRegistry::load(config.path())
            .contributions()
            .table_editors
            .len(),
        1
    );
}

#[test]
fn file_type_suggestions_are_validated_advisory_catalog_data() {
    let catalog = Catalog::bundled();
    let csv = catalog.latest("org.rustpy.csv-table").unwrap();
    assert!(csv
        .file_extensions
        .iter()
        .any(|extension| extension == "csv"));
    let mut invalid = csv.clone();
    invalid.file_extensions = vec!["../escape".into()];
    assert!(!invalid.problems().is_empty());
    invalid = csv.clone();
    invalid.screenshots = vec!["file:///etc/passwd".into()];
    assert!(!invalid.problems().is_empty());
}
