//! The deployment workflow through the public API, as the UI drives it, with a local
//! (mounted) folder as the server.

mod common;

use std::path::{Path, PathBuf};

use common::{mtime, read, write, TempDir};
use rustpy_deploy::{
    collect_files, download_paths, execute_sync, local_path, plan_server_sync, plan_sync,
    remote_path, transport_for, upload_paths, web_url, AutoUpload, DeploymentConfig, Server,
    SyncAction, SyncDirection, SyncItem,
};

const T: i64 = 1_700_000_000;

fn actions(items: &[SyncItem]) -> Vec<(&str, SyncAction)> {
    items
        .iter()
        .map(|i| (i.rel_path.as_str(), i.action))
        .collect()
}

fn all_ok(results: &[(PathBuf, anyhow::Result<()>)]) {
    for (path, result) in results {
        assert!(
            result.is_ok(),
            "{}: {:#}",
            path.display(),
            result.as_ref().unwrap_err()
        );
    }
}

/// A project with a default mapping to `/www` and `static` mapped to `/public`.
fn setup() -> (TempDir, TempDir, DeploymentConfig) {
    let project = TempDir::new("project");
    let mount = TempDir::new("mount");
    let p = project.path();
    write(&p.join("app.py"), "print('v1')\n", T);
    write(&p.join("pkg/__init__.py"), "", T);
    write(&p.join("pkg/my module.py"), "x = \"it's\"\n", T);
    write(&p.join("pkg/__pycache__/m.cpython-312.pyc"), "bytecode", T);
    write(&p.join("static/site.css"), "body {}\n", T);
    write(&p.join(".git/HEAD"), "ref: refs/heads/main\n", T);
    write(&p.join("notes.log"), "debug", T);
    let server = Server::local("mount", mount.path())
        .with_mapping("", "/www")
        .with_mapping("static", "/public")
        .with_web_url("http://localhost:8000/");
    let mut excluded = rustpy_deploy::default_excluded();
    excluded.push("*.log".into());
    let cfg = DeploymentConfig {
        servers: vec![server],
        default_server: Some("mount".into()),
        auto_upload: AutoUpload::OnExplicitSave,
        excluded,
    };
    (project, mount, cfg)
}

#[test]
fn upload_sync_and_download_with_a_mounted_folder() {
    let (project, mount, cfg) = setup();
    let (p, m) = (project.path(), mount.path());

    // The UI stores the configuration in .rustpy/deployment.json and reads it back.
    cfg.save(p).unwrap();
    assert!(p.join(".rustpy/deployment.json").is_file());
    let cfg = DeploymentConfig::load(p).unwrap();
    assert!(cfg.validate().is_empty(), "{:?}", cfg.validate());
    let server = cfg.default_server_entry().unwrap();
    let transport = transport_for(server).unwrap();
    assert!(transport.test_connection().unwrap().contains("accessible"));

    // "Upload to…" on the project root: the project's exclusions apply, mappings pick targets.
    let files = collect_files(p, &[p.to_path_buf()], &cfg.excluded);
    let mut progress = Vec::new();
    let results = upload_paths(transport.as_ref(), server, p, &files, |i, n, path| {
        progress.push(format!("{i}/{n} {path}"))
    });
    all_ok(&results);
    assert_eq!(
        progress,
        [
            "0/4 app.py",
            "1/4 pkg/__init__.py",
            "2/4 pkg/my module.py",
            "3/4 static/site.css"
        ]
    );
    assert_eq!(read(&m.join("www/app.py")), "print('v1')\n");
    assert_eq!(read(&m.join("www/pkg/my module.py")), "x = \"it's\"\n");
    assert_eq!(read(&m.join("public/site.css")), "body {}\n");
    assert_eq!(
        mtime(&m.join("www/app.py")),
        T,
        "modification times are kept"
    );
    for skipped in [
        "www/static",
        "www/notes.log",
        "www/.git",
        "www/.rustpy",
        "www/pkg/__pycache__",
    ] {
        assert!(!m.join(skipped).exists(), "{skipped} must not be deployed");
    }
    let plan = plan_server_sync(
        transport.as_ref(),
        server,
        p,
        &cfg.excluded,
        SyncDirection::Both,
    )
    .unwrap();
    assert!(plan.is_empty(), "in sync right after the upload: {plan:?}");

    // Changes on both sides.
    write(&m.join("www/app.py"), "print('remote')\n", T + 100);
    write(&m.join("www/extra.txt"), "remote only", T);
    write(&m.join("www/debug.log"), "excluded", T);
    write(&p.join("pkg/my module.py"), "x = 2\n", T + 200);
    write(&p.join("pkg/new.py"), "new", T);

    use SyncAction::*;
    let upload_plan = plan_server_sync(
        transport.as_ref(),
        server,
        p,
        &cfg.excluded,
        SyncDirection::Upload,
    )
    .unwrap();
    assert_eq!(
        actions(&upload_plan),
        [
            ("app.py", Conflict),
            ("extra.txt", Skip),
            ("pkg/my module.py", Upload),
            ("pkg/new.py", Upload)
        ]
    );
    let plan = plan_server_sync(
        transport.as_ref(),
        server,
        p,
        &cfg.excluded,
        SyncDirection::Both,
    )
    .unwrap();
    assert_eq!(
        actions(&plan),
        [
            ("app.py", Download),
            ("extra.txt", Download),
            ("pkg/my module.py", Upload),
            ("pkg/new.py", Upload)
        ]
    );
    let app = &plan[0];
    assert_eq!(
        (
            app.local_size,
            app.remote_size,
            app.local_mtime,
            app.remote_mtime
        ),
        (Some(12), Some(16), Some(T), Some(T + 100))
    );

    let results = execute_sync(transport.as_ref(), server, p, &plan, |_, _, _| {});
    all_ok(&results);
    assert_eq!(read(&p.join("app.py")), "print('remote')\n");
    assert_eq!(mtime(&p.join("app.py")), T + 100);
    assert_eq!(read(&p.join("extra.txt")), "remote only");
    assert_eq!(read(&m.join("www/pkg/my module.py")), "x = 2\n");
    assert_eq!(read(&m.join("www/pkg/new.py")), "new");
    assert!(plan_server_sync(
        transport.as_ref(),
        server,
        p,
        &cfg.excluded,
        SyncDirection::Both
    )
    .unwrap()
    .is_empty());

    // The generic planner on one mapping, fed with a recursive listing.
    write(&p.join("static/img/logo.svg"), "<svg/>", T);
    let listing = transport.list_recursive("/public", &cfg.excluded).unwrap();
    let plan = plan_sync(
        &p.join("static"),
        &cfg.excluded,
        &listing,
        SyncDirection::Upload,
    );
    assert_eq!(actions(&plan), [("img/logo.svg", Upload)]);
    all_ok(&upload_paths(
        transport.as_ref(),
        server,
        p,
        &[PathBuf::from("static/img/logo.svg")],
        |_, _, _| {},
    ));
    assert_eq!(read(&m.join("public/img/logo.svg")), "<svg/>");

    // "Download from…" a directory brings back a deleted file.
    std::fs::remove_file(p.join("pkg/__init__.py")).unwrap();
    let mut seen = Vec::new();
    let results = download_paths(
        transport.as_ref(),
        server,
        p,
        &[PathBuf::from("pkg")],
        &cfg.excluded,
        |_, _, path| seen.push(path.to_string()),
    );
    all_ok(&results);
    assert_eq!(seen, ["pkg/__init__.py", "pkg/my module.py", "pkg/new.py"]);
    assert!(p.join("pkg/__init__.py").is_file());

    transport.delete("/www/extra.txt").unwrap();
    assert!(!m.join("www/extra.txt").exists());
    let unmapped = upload_paths(
        transport.as_ref(),
        server,
        p,
        &[PathBuf::from("/definitely/elsewhere.py")],
        |_, _, _| {},
    );
    assert!(format!("{:#}", unmapped[0].1.as_ref().unwrap_err()).contains("no deployment mapping"));
}

#[test]
fn mapping_helpers_and_auto_upload() {
    let (project, _mount, cfg) = setup();
    let p = project.path();
    let server = cfg.default_server_entry().unwrap();
    assert_eq!(
        remote_path(server, p, Path::new("static/site.css")).as_deref(),
        Some("/public/site.css")
    );
    assert_eq!(
        remote_path(server, p, &p.join("pkg/my module.py")).as_deref(),
        Some("/www/pkg/my module.py")
    );
    assert_eq!(
        local_path(server, p, "/public/img/a.svg"),
        Some(p.join("static/img/a.svg"))
    );
    assert_eq!(local_path(server, p, "/elsewhere/a.svg"), None);
    assert_eq!(
        web_url(server, p, Path::new("pkg/my module.py")).as_deref(),
        Some("http://localhost:8000/www/pkg/my%20module.py")
    );

    let app = p.join("app.py");
    assert_eq!(
        cfg.auto_upload_target(p, &app, true)
            .map(|s| s.name.as_str()),
        Some("mount")
    );
    assert!(
        cfg.auto_upload_target(p, &app, false).is_none(),
        "auto-save is not an explicit save"
    );
    assert!(
        cfg.auto_upload_target(p, &p.join("notes.log"), true)
            .is_none(),
        "excluded"
    );
    let mut always = cfg.clone();
    always.auto_upload = AutoUpload::Always;
    assert!(always.auto_upload_target(p, &app, false).is_some());
}
