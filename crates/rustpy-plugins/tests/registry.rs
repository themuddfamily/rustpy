//! Installing, enabling, disabling and uninstalling plugins in a temporary
//! config directory, and aggregating their contributions.

mod common;

use std::fs;

use common::{example_dir, plugins_dir_entries, write_files, write_plugin, write_tool_plugin};
use rustpy_plugins::{pack_bundle, PluginError, PluginRegistry, IDE_VERSION};
use serde_json::json;

const EXAMPLE_ID: &str = "com.example.solarized-theme";

#[test]
fn install_enable_disable_uninstall_round_trip() {
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    assert!(registry.plugins().is_empty());
    assert!(registry.contributions().is_empty());
    assert!(registry.errors().is_empty());

    let outcome = registry.install_from_dir(&example_dir()).unwrap();
    assert_eq!(outcome.id, EXAMPLE_ID);
    assert_eq!(outcome.version, "1.0.0");
    assert_eq!(outcome.previous_version, None);
    assert_eq!(outcome.dir, config.path().join("plugins").join(EXAMPLE_ID));
    assert!(outcome.dir.join("plugin.json").is_file());
    assert!(outcome.dir.join("colors/solarized-dark.json").is_file());
    assert_eq!(
        plugins_dir_entries(config.path()),
        vec![EXAMPLE_ID],
        "no staging leftovers"
    );

    let plugin = registry.get(EXAMPLE_ID).expect("listed after install");
    assert!(plugin.enabled && plugin.is_valid() && plugin.is_active());
    assert_eq!(plugin.name(), "Solarized Theme");
    assert_eq!(plugin.dir, outcome.dir);

    let c = registry.contributions();
    assert_eq!(c.color_schemes.len(), 1);
    assert_eq!(c.live_templates.len(), 1);
    assert_eq!(c.all_live_templates().len(), 3);
    assert!(c.color_scheme("Solarized Dark (Plugin)").is_some());
    for path in [&c.color_schemes[0].path, &c.live_templates[0].path] {
        assert!(path.is_absolute() && path.is_file(), "{}", path.display());
        assert!(
            path.starts_with(&outcome.dir),
            "resolved inside the installed copy: {}",
            path.display()
        );
    }
    assert!(c.color_schemes.iter().all(|s| s.plugin_id == EXAMPLE_ID));

    registry.disable(EXAMPLE_ID).unwrap();
    assert!(!registry.is_enabled(EXAMPLE_ID));
    assert!(
        registry.contributions().is_empty(),
        "disabled plugins contribute nothing"
    );
    let state: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(config.path().join("plugins/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state, json!({"enabled": {EXAMPLE_ID: false}}));

    // The state survives a reload (e.g. an IDE restart).
    let mut registry = PluginRegistry::load(config.path());
    assert!(!registry.get(EXAMPLE_ID).unwrap().enabled);
    assert!(registry.contributions().is_empty());

    registry.enable(EXAMPLE_ID).unwrap();
    assert!(registry.is_enabled(EXAMPLE_ID));
    assert_eq!(registry.contributions().color_schemes.len(), 1);
    assert!(PluginRegistry::load(config.path()).is_enabled(EXAMPLE_ID));

    registry.uninstall(EXAMPLE_ID).unwrap();
    assert!(registry.plugins().is_empty());
    assert!(!config.path().join("plugins").join(EXAMPLE_ID).exists());
    let state = fs::read_to_string(config.path().join("plugins/state.json")).unwrap();
    assert!(!state.contains(EXAMPLE_ID), "state entry removed: {state}");
    assert!(PluginRegistry::load(config.path()).plugins().is_empty());

    assert!(
        matches!(registry.uninstall(EXAMPLE_ID), Err(PluginError::NotInstalled(id)) if id == EXAMPLE_ID)
    );
    assert!(matches!(
        registry.enable("com.example.nope"),
        Err(PluginError::NotInstalled(_))
    ));
    assert!(matches!(
        registry.disable("../../etc"),
        Err(PluginError::NotInstalled(_))
    ));
}

#[test]
fn id_collisions_need_a_newer_version() {
    let config = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());

    let v1 = write_plugin(
        src.path(),
        "v1",
        json!({"version": "1.0.0"}),
        &[("only-in-v1.txt", "old")],
    );
    registry.install_from_dir(&v1).unwrap();
    registry.disable("com.example.test").unwrap();

    for (folder, version) in [
        ("same", "1.0.0"),
        ("same-padded", "1.0"),
        ("older", "0.9.9"),
        ("pre", "1.0.0-rc.1"),
    ] {
        let dir = write_plugin(src.path(), folder, json!({"version": version}), &[]);
        match registry.install_from_dir(&dir) {
            Err(PluginError::AlreadyInstalled {
                id,
                installed,
                offered,
            }) => {
                assert_eq!(
                    (id.as_str(), installed.as_str(), offered.as_str()),
                    ("com.example.test", "1.0.0", version)
                );
            }
            other => panic!("{version}: expected AlreadyInstalled, got {other:?}"),
        }
    }

    let v2 = write_plugin(
        src.path(),
        "v2",
        json!({"version": "1.1.0"}),
        &[("only-in-v2.txt", "new")],
    );
    let outcome = registry.install_from_dir(&v2).unwrap();
    assert_eq!(outcome.previous_version.as_deref(), Some("1.0.0"));
    assert_eq!(outcome.version, "1.1.0");
    let dir = config.path().join("plugins/com.example.test");
    assert!(dir.join("only-in-v2.txt").is_file());
    assert!(
        !dir.join("only-in-v1.txt").exists(),
        "the old version's files are gone"
    );
    let plugin = registry.get("com.example.test").unwrap();
    assert_eq!(plugin.manifest.version, "1.1.0");
    assert!(!plugin.enabled, "an upgrade keeps the plugin disabled");
    assert_eq!(
        plugins_dir_entries(config.path()),
        vec!["com.example.test", "state.json"]
    );
}

#[test]
fn invalid_or_incompatible_plugins_are_not_installed() {
    let config = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());

    let traversal = write_plugin(
        src.path(),
        "evil",
        json!({"contributes": {"color_schemes": ["../../outside.json"]}}),
        &[],
    );
    let err = registry.install_from_dir(&traversal).unwrap_err();
    assert!(
        err.to_string()
            .contains("points outside the plugin directory"),
        "{err}"
    );

    let future = write_plugin(
        src.path(),
        "future",
        json!({"min_ide_version": "999.0"}),
        &[],
    );
    match registry.install_from_dir(&future) {
        Err(PluginError::Incompatible {
            required, current, ..
        }) => {
            assert_eq!(required, "999.0");
            assert_eq!(current, IDE_VERSION);
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }

    let missing = write_plugin(
        src.path(),
        "missing",
        json!({"contributes": {"keymaps": ["nope.json"]}}),
        &[],
    );
    assert!(registry
        .install_from_dir(&missing)
        .unwrap_err()
        .to_string()
        .contains("`nope.json` does not exist"));
    assert!(matches!(
        registry.install_from_dir(&src.path().join("absent")),
        Err(PluginError::Io { .. })
    ));
    let file = src.path().join("evil/plugin.json");
    assert!(registry
        .install_from_dir(&file)
        .unwrap_err()
        .to_string()
        .contains("not a directory"));

    assert!(registry.plugins().is_empty());
    assert!(
        plugins_dir_entries(config.path()).is_empty(),
        "{:?}",
        plugins_dir_entries(config.path())
    );
}

#[cfg(unix)]
#[test]
fn plugins_with_symlinks_are_not_installed() {
    let config = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let dir = write_plugin(src.path(), "linked", json!({}), &[]);
    std::os::unix::fs::symlink("/etc/hostname", dir.join("hostname")).unwrap();
    let mut registry = PluginRegistry::load(config.path());
    let err = registry.install_from_dir(&dir).unwrap_err();
    assert!(
        err.to_string().contains("`hostname` is a symbolic link"),
        "{err}"
    );
    assert!(registry.plugins().is_empty());
}

#[test]
fn installing_from_inside_the_plugins_directory_is_refused() {
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    registry.install_from_dir(&example_dir()).unwrap();
    let installed = config.path().join("plugins").join(EXAMPLE_ID);
    let err = registry.install_from_dir(&installed).unwrap_err();
    assert!(
        err.to_string().contains("inside the plugins directory"),
        "{err}"
    );
    assert!(
        installed.join("plugin.json").is_file(),
        "the installed copy is untouched"
    );
}

#[test]
fn contributions_aggregate_enabled_valid_plugins_only() {
    let config = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    registry.install_from_dir(&example_dir()).unwrap();
    registry
        .install_from_dir(&write_tool_plugin(
            src.path(),
            "tools",
            "com.example.tools",
            "1.0.0",
        ))
        .unwrap();

    // Plugins dropped into the directory by hand: a broken one and a misplaced one.
    write_files(
        &config.path().join("plugins/com.example.broken"),
        &[("plugin.json", "{ this is not json")],
    );
    write_tool_plugin(
        &config.path().join("plugins"),
        "com.example.wrong-dir",
        "com.example.other",
        "1.0.0",
    );
    write_files(
        &config.path().join("plugins"),
        &[("stray-file.txt", "ignored")],
    );
    registry.reload();

    let ids: Vec<&str> = registry.plugins().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "com.example.broken",
            "com.example.solarized-theme",
            "com.example.tools",
            "com.example.wrong-dir"
        ]
    );

    let broken = registry.get("com.example.broken").unwrap();
    assert!(broken.enabled && !broken.is_valid() && !broken.is_active());
    assert!(
        broken.errors[0].starts_with("plugin.json: "),
        "{:?}",
        broken.errors
    );
    assert_eq!(broken.name(), "com.example.broken");
    assert!(broken.contributions().is_empty());

    let wrong = registry.get("com.example.wrong-dir").unwrap();
    assert!(
        wrong.errors[0].contains("holds the plugin `com.example.other`"),
        "{:?}",
        wrong.errors
    );

    let all = registry.contributions();
    assert_eq!(all.color_schemes.len(), 1);
    assert_eq!(all.keymaps.len(), 1, "only the valid tool plugin's keymap");
    assert_eq!(all.keymaps[0].plugin_id, "com.example.tools");
    assert_eq!(all.textmate_grammars[0].plugin_id, "com.example.tools");
    assert_eq!(all.external_tools[0].plugin_id, "com.example.tools");
    assert_eq!(all.file_templates[0].plugin_id, "com.example.tools");
    assert_eq!(all.live_templates[0].plugin_id, EXAMPLE_ID);
    for path in all
        .keymaps
        .iter()
        .map(|k| &k.path)
        .chain(all.textmate_grammars.iter().map(|g| &g.path))
        .chain(all.file_templates.iter().map(|t| &t.path))
    {
        assert!(
            path.is_absolute() && path.starts_with(config.path().join("plugins/com.example.tools")),
            "{}",
            path.display()
        );
    }

    registry.disable(EXAMPLE_ID).unwrap();
    let all = registry.contributions();
    assert!(all.color_schemes.is_empty() && all.live_templates.is_empty());
    assert_eq!(all.keymaps.len(), 1);
    registry.disable("com.example.tools").unwrap();
    assert!(registry.contributions().is_empty());

    // Broken plugins can still be removed.
    registry.uninstall("com.example.broken").unwrap();
    registry.uninstall("com.example.wrong-dir").unwrap();
    assert_eq!(registry.plugins().len(), 2);
}

#[test]
fn a_broken_install_can_be_replaced() {
    let config = tempfile::tempdir().unwrap();
    write_files(
        &config.path().join("plugins").join(EXAMPLE_ID),
        &[("plugin.json", "garbage")],
    );
    let mut registry = PluginRegistry::load(config.path());
    assert!(!registry.get(EXAMPLE_ID).unwrap().is_valid());
    let outcome = registry.install_from_path(&example_dir()).unwrap();
    assert_eq!(outcome.previous_version, None);
    assert!(registry.get(EXAMPLE_ID).unwrap().is_active());
}

#[test]
fn a_fresh_install_is_enabled_despite_a_stale_state_entry() {
    let config = tempfile::tempdir().unwrap();
    let state = format!(r#"{{"enabled": {{"{EXAMPLE_ID}": false, "com.example.other": false}}}}"#);
    write_files(&config.path().join("plugins"), &[("state.json", &state)]);
    let mut registry = PluginRegistry::load(config.path());
    registry.install_from_dir(&example_dir()).unwrap();
    assert!(registry.is_enabled(EXAMPLE_ID));
    let state = fs::read_to_string(config.path().join("plugins/state.json")).unwrap();
    assert!(
        !state.contains(EXAMPLE_ID) && state.contains("com.example.other"),
        "only the stale entry is dropped: {state}"
    );
}

#[test]
fn incompatible_installed_plugins_do_not_contribute() {
    let config = tempfile::tempdir().unwrap();
    write_plugin(
        &config.path().join("plugins"),
        "com.example.test",
        json!({"min_ide_version": "999.1"}),
        &[],
    );
    let registry = PluginRegistry::load(config.path());
    let plugin = registry.get("com.example.test").unwrap();
    assert!(
        plugin.errors[0].contains("requires RustPy 999.1 or newer"),
        "{:?}",
        plugin.errors
    );
    assert!(!plugin.is_active());
}

#[test]
fn a_corrupt_state_file_is_reported_and_plugins_stay_enabled() {
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    registry.install_from_dir(&example_dir()).unwrap();
    fs::write(config.path().join("plugins/state.json"), "{ not json").unwrap();
    let mut registry = PluginRegistry::load(config.path());
    assert!(
        registry.errors()[0].contains("state.json"),
        "{:?}",
        registry.errors()
    );
    assert!(registry.is_enabled(EXAMPLE_ID));
    // Writing the state again repairs the file.
    registry.disable(EXAMPLE_ID).unwrap();
    let registry = PluginRegistry::load(config.path());
    assert!(registry.errors().is_empty());
    assert!(!registry.is_enabled(EXAMPLE_ID));
}

#[test]
fn bundles_round_trip() {
    let bytes = pack_bundle(&example_dir()).unwrap();
    assert_eq!(
        bytes,
        pack_bundle(&example_dir()).unwrap(),
        "packing is deterministic"
    );
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(doc["format"], "rustpy-plugin");
    assert_eq!(doc["version"], 1);
    let files: Vec<&String> = doc["files"].as_object().unwrap().keys().collect();
    assert_eq!(
        files,
        vec![
            "LICENSE.solarized",
            "NOTICE.solarized",
            "colors/solarized-dark.json",
            "plugin.json",
            "templates/python.json"
        ]
    );

    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    let outcome = registry.install_bundle_bytes(&bytes).unwrap();
    assert_eq!(outcome.id, EXAMPLE_ID);
    let installed = config.path().join("plugins").join(EXAMPLE_ID);
    for rel in [
        "LICENSE.solarized",
        "NOTICE.solarized",
        "plugin.json",
        "colors/solarized-dark.json",
        "templates/python.json",
    ] {
        assert_eq!(
            fs::read(installed.join(rel)).unwrap(),
            fs::read(example_dir().join(rel)).unwrap(),
            "{rel} is identical after the round trip"
        );
    }
    assert_eq!(registry.contributions().all_live_templates().len(), 3);

    // Binary files survive as base64; a bundle file installs like the bytes.
    let src = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        src.path(),
        "bin",
        json!({"version": "2.0.0", "id": "com.example.binary"}),
        &[],
    );
    let binary: Vec<u8> = (0..=255u8).rev().collect();
    fs::write(dir.join("icon.bin"), &binary).unwrap();
    let bundle_path = src.path().join("binary.rpplugin");
    fs::write(&bundle_path, pack_bundle(&dir).unwrap()).unwrap();
    registry.install_from_path(&bundle_path).unwrap();
    assert_eq!(
        fs::read(config.path().join("plugins/com.example.binary/icon.bin")).unwrap(),
        binary
    );

    let err = registry
        .install_bundle_bytes(b"{\"format\": \"zip\"}")
        .unwrap_err();
    assert!(matches!(err, PluginError::Bundle(_)), "{err}");
    assert_eq!(
        plugins_dir_entries(config.path()),
        vec!["com.example.binary", EXAMPLE_ID]
    );
}

#[test]
fn packing_refuses_invalid_plugins() {
    let src = tempfile::tempdir().unwrap();
    let dir = write_plugin(src.path(), "bad", json!({"version": "nope"}), &[]);
    assert!(matches!(
        pack_bundle(&dir),
        Err(PluginError::Invalid { .. })
    ));
}

/// A bundle document with the given `files` object.
fn bundle_with(files: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"format": "rustpy-plugin", "version": 1, "files": files})).unwrap()
}

const HOSTILE_MANIFEST: &str =
    r#"{"id": "com.example.hostile", "name": "Hostile", "version": "1.0.0"}"#;

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Runs a tool the way rustpy-ui does (the program directly, see
/// [`common::run_like_the_ide`]) for a file `/p/x.py`, in `scratch`.
#[cfg(unix)]
fn run_like_the_ide(
    tool: &rustpy_settings::ExternalTool,
    scratch: &std::path::Path,
) -> std::process::Output {
    common::run_like_the_ide(
        tool,
        &[("FilePath", "/p/x.py"), ("ProjectFileDir", "/p")],
        scratch,
    )
}

/// Review finding (security): `fs::copy` kept setuid/setgid and
/// group/other-write bits of the source files.
#[cfg(unix)]
#[test]
fn installed_files_never_keep_special_or_writable_modes() {
    use std::os::unix::fs::PermissionsExt;
    let src = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        src.path(),
        "modes",
        json!({"contributes": {"external_tools": [{"name": "Run", "program": "$PluginDir$/bin/run.sh"}]}}),
        &[
            ("bin/run.sh", "#!/bin/sh\necho ran-ok\n"),
            ("data.txt", "x"),
            ("tool2", "y"),
        ],
    );
    let set = |rel: &str, mode: u32| {
        fs::set_permissions(dir.join(rel), fs::Permissions::from_mode(mode)).unwrap()
    };
    set("bin/run.sh", 0o4755);
    set("data.txt", 0o777);
    set("plugin.json", 0o2666);
    set("tool2", 0o1766);
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    let outcome = registry.install_from_dir(&dir).unwrap();
    // Files are created with 0644/0755, which the umask can only narrow.
    for (rel, at_most) in [
        ("bin/run.sh", 0o755),
        ("data.txt", 0o755),
        ("plugin.json", 0o644),
        ("tool2", 0o755),
    ] {
        let mode = mode_of(&outcome.dir.join(rel));
        assert_eq!(
            mode & !at_most,
            0,
            "{rel}: {mode:o} has more than {at_most:o}"
        );
        assert_eq!(mode & 0o7022, 0, "{rel}: {mode:o}");
    }
    assert_ne!(
        mode_of(&outcome.dir.join("bin/run.sh")) & 0o100,
        0,
        "programs stay executable"
    );
    assert_eq!(mode_of(&outcome.dir.join("plugin.json")) & 0o111, 0);
}

/// Review finding: bundle and Marketplace installs dropped the executable bit,
/// so `$PluginDir$/bin/tool` programs failed with "Permission denied".
#[cfg(unix)]
#[test]
fn bundled_programs_stay_executable() {
    use std::os::unix::fs::PermissionsExt;
    let src = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        src.path(),
        "tool",
        json!({"contributes": {"external_tools": [{"name": "Run", "program": "$PluginDir$/bin/run.sh", "arguments": "$PluginDir$/bin/helper.sh"}]}}),
        &[
            ("bin/run.sh", "#!/bin/sh\nexec \"$1\"\n"),
            ("bin/helper.sh", "#!/bin/sh\necho helper-ran\n"),
            ("readme.txt", "plain"),
        ],
    );
    for rel in ["bin/run.sh", "bin/helper.sh"] {
        fs::set_permissions(dir.join(rel), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let bytes = pack_bundle(&dir).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(doc["files"]["bin/run.sh"]["executable"], true);
    assert_eq!(doc["files"]["bin/helper.sh"]["executable"], true);
    assert!(
        doc["files"]["readme.txt"].is_string(),
        "plain files stay plain strings"
    );

    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    let outcome = registry.install_bundle_bytes(&bytes).unwrap();
    assert_ne!(
        mode_of(&outcome.dir.join("bin/helper.sh")) & 0o100,
        0,
        "the bundle's flag is honored"
    );
    assert_eq!(mode_of(&outcome.dir.join("readme.txt")) & 0o111, 0);
    let contributions = registry.contributions();
    let out = run_like_the_ide(&contributions.external_tools[0].tool, src.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "helper-ran\n");
}

/// A bundle without executable flags (packed on Windows, or by hand) still
/// gets runnable tool programs: the manifest's programs are made executable.
#[cfg(unix)]
#[test]
fn declared_programs_become_executable_on_install() {
    let manifest = json!({"id": "com.example.flagless", "name": "Flagless", "version": "1.0.0",
        "contributes": {"external_tools": [{"name": "Run", "program": "$PluginDir$/bin/run.sh"}]}});
    let bytes = bundle_with(json!({
        "plugin.json": manifest.to_string(),
        "bin/run.sh": "#!/bin/sh\necho flagless-ran\n"
    }));
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    let outcome = registry.install_bundle_bytes(&bytes).unwrap();
    let mode = mode_of(&outcome.dir.join("bin/run.sh"));
    assert_ne!(mode & 0o100, 0, "{mode:o}");
    assert_eq!(mode & 0o7022, 0, "{mode:o}");
    let plugin = registry.get("com.example.flagless").unwrap();
    assert!(plugin.warnings.is_empty(), "{:?}", plugin.warnings);
    let scratch = tempfile::tempdir().unwrap();
    let out = run_like_the_ide(
        &registry.contributions().external_tools[0].tool,
        scratch.path(),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "flagless-ran\n");
}

/// Review finding (DoS): installing a FIFO named `*.rpplugin` blocked until a
/// writer appeared; a link to `/dev/zero` would have read until memory ran out.
#[cfg(unix)]
#[test]
fn only_regular_files_are_read_as_bundles() {
    let tmp = tempfile::tempdir().unwrap();
    let fifo = tmp.path().join("cool.rpplugin");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let zero = tmp.path().join("zero.rpplugin");
    std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
    let config = tmp.path().join("config");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut registry = PluginRegistry::load(&config);
        let results = [
            registry.install_from_path(&fifo).map(|_| ()),
            registry.install_from_path(&zero).map(|_| ()),
        ];
        tx.send(results.map(|r| r.unwrap_err().to_string())).ok();
    });
    let errors = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("installing a FIFO or a device must not block");
    for err in errors {
        assert!(err.contains("is not a regular file"), "{err}");
    }
}

/// Review finding: Windows device names in bundle paths were unpacked.
#[test]
fn bundles_with_windows_device_names_are_refused() {
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    for name in [
        "con",
        "nul.json",
        "aux/x.json",
        "com1.txt",
        "lpt1",
        "x/PRN.md",
    ] {
        let bytes = bundle_with(json!({"plugin.json": HOSTILE_MANIFEST, name: "x"}));
        let err = registry.install_bundle_bytes(&bytes).unwrap_err();
        assert!(
            matches!(&err, PluginError::Bundle(m) if m.contains("reserves for a device")),
            "{name}: {err}"
        );
    }
    assert!(plugins_dir_entries(config.path()).is_empty());
}

/// Review finding: bundles had no depth limit, so a bundle could install a
/// plugin that could not be packed again.
#[test]
fn bundles_nest_no_deeper_than_plugin_directories() {
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    let too_deep = format!("{}/x.txt", vec!["d"; 17].join("/"));
    let err = registry
        .install_bundle_bytes(&bundle_with(
            json!({"plugin.json": HOSTILE_MANIFEST, too_deep: "x"}),
        ))
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("nested more than 16 directories deep"),
        "{err}"
    );
    let deepest = format!("{}/x.txt", vec!["d"; 16].join("/"));
    let outcome = registry
        .install_bundle_bytes(&bundle_with(
            json!({"plugin.json": HOSTILE_MANIFEST, deepest: "x"}),
        ))
        .unwrap();
    pack_bundle(&outcome.dir).expect("what installs can be packed again");
}

/// Review finding: staging and backup directories left by a crashed install
/// were never cleaned up. Old ones are removed on load; the backup of an
/// upgrade interrupted between its two renames is put back; fresh ones (a
/// running install) stay.
#[cfg(unix)]
#[test]
fn leftovers_of_crashed_installs_are_cleaned_up() {
    let config = tempfile::tempdir().unwrap();
    let plugins = config.path().join("plugins");
    let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    let age = |path: &std::path::Path| {
        fs::File::open(path)
            .unwrap()
            .set_modified(old_time)
            .unwrap();
    };
    let backup = |name: &str, id: &str| {
        let dir = plugins.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("plugin.json"),
            json!({"id": id, "name": "X", "version": "1.0.0"}).to_string(),
        )
        .unwrap();
        age(&dir);
    };
    // A crash between moving the old version away and moving the new one
    // in: the new version is still staged under the backup's suffix.
    backup(".old-1-2-3", "com.example.lost");
    let interrupted = plugins.join(".staging-1-2-3");
    fs::create_dir_all(&interrupted).unwrap();
    age(&interrupted);
    // A crash after the new version was in place: the backup is just old.
    let replaced = write_plugin(&plugins, ".old-4-5-6", json!({}), &[]);
    write_plugin(
        &plugins,
        "com.example.test",
        json!({"version": "2.0.0"}),
        &[],
    );
    age(&replaced);
    // A backup that was not deleted, of a plugin uninstalled since.
    backup(".old-13-14-15", "com.example.gone");
    // Staging directories and temporary state files of crashed installs.
    let staging = plugins.join(".staging-7-8-9");
    fs::create_dir_all(staging.join("sub")).unwrap();
    fs::write(staging.join("sub/f"), "x").unwrap();
    age(&staging);
    let tmp_state = plugins.join(".state-1-2-3.tmp");
    fs::write(&tmp_state, "{}").unwrap();
    age(&tmp_state);
    // Installs in progress (e.g. in another IDE window) are left alone, also
    // an upgrade between its two renames.
    let fresh = plugins.join(".staging-10-11-12");
    fs::create_dir_all(&fresh).unwrap();
    backup(".old-16-17-18", "com.example.moving");
    fs::create_dir_all(plugins.join(".staging-16-17-18")).unwrap();

    let registry = PluginRegistry::load(config.path());
    assert!(registry.errors().is_empty(), "{:?}", registry.errors());
    assert_eq!(
        plugins_dir_entries(config.path()),
        vec![
            ".old-16-17-18",
            ".staging-10-11-12",
            ".staging-16-17-18",
            "com.example.lost",
            "com.example.test"
        ]
    );
    assert!(
        registry.get("com.example.lost").unwrap().is_active(),
        "restored"
    );
    assert!(registry.get("com.example.gone").is_none(), "not restored");
    assert_eq!(
        registry.get("com.example.test").unwrap().manifest.version,
        "2.0.0"
    );
}

/// Review finding: a backup an upgrade could not delete brought its plugin
/// back after the user uninstalled it (the backup keeps the plugin
/// directory's old modification time, so it counted as a crashed install).
#[cfg(unix)]
#[test]
fn an_uninstalled_plugin_does_not_come_back_from_a_stale_backup() {
    let src = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        src.path(),
        "p",
        json!({"id": "com.example.back", "version": "1.0.0"}),
        &[],
    );
    let mut registry = PluginRegistry::load(config.path());
    registry.install_from_dir(&dir).unwrap();
    // The old version of an upgrade that could not be deleted (say, a
    // read-only folder in it), three hours old.
    let newer = write_plugin(
        src.path(),
        "p2",
        json!({"id": "com.example.back", "version": "1.1.0"}),
        &[],
    );
    registry.install_from_dir(&newer).unwrap();
    let backup = config.path().join("plugins/.old-1-2-3");
    fs::create_dir_all(&backup).unwrap();
    fs::copy(dir.join("plugin.json"), backup.join("plugin.json")).unwrap();
    let three_hours_ago =
        std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 60 * 60);
    fs::File::open(&backup)
        .unwrap()
        .set_modified(three_hours_ago)
        .unwrap();
    registry.uninstall("com.example.back").unwrap();
    let again = PluginRegistry::load(config.path());
    assert!(
        again.get("com.example.back").is_none(),
        "{:?}",
        plugins_dir_entries(config.path())
    );
    assert!(again.errors().is_empty(), "{:?}", again.errors());
    assert!(
        !backup.exists(),
        "the stale backup is deleted: {:?}",
        plugins_dir_entries(config.path())
    );
}

/// An upgrade names the backup of the old version after its staging
/// directory and deletes both when it is done.
#[test]
fn upgrades_leave_nothing_behind() {
    let src = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    for version in ["1.0.0", "1.1.0", "2.0.0"] {
        let dir = write_plugin(
            src.path(),
            version,
            json!({"id": "com.example.up", "version": version}),
            &[],
        );
        registry.install_from_dir(&dir).unwrap();
    }
    assert_eq!(plugins_dir_entries(config.path()), vec!["com.example.up"]);
    assert_eq!(
        registry.get("com.example.up").unwrap().manifest.version,
        "2.0.0"
    );
}

/// Review finding: File › Manage IDE Settings carried `<config>/plugins` in
/// its text bundle. Imported into another configuration, the plugin was
/// active but its program had lost the executable bit, so the tool failed
/// with "Permission denied". Plugins are not part of settings bundles: an
/// import leaves them to Settings › Plugins.
#[cfg(unix)]
#[test]
fn settings_bundles_leave_plugins_out() {
    use rustpy_settings::bundle::{export_bundle, import_bundle};
    let src = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        src.path(),
        "tool",
        json!({"contributes": {"external_tools": [{"name": "Run", "program": "$PluginDir$/bin/run.sh"}]}}),
        &[("bin/run.sh", "#!/bin/sh\necho run-ok\n")],
    );
    let a = tempfile::tempdir().unwrap();
    fs::write(a.path().join("settings.json"), "{}").unwrap();
    let mut registry = PluginRegistry::load(a.path());
    registry.install_from_dir(&dir).unwrap();
    let out = run_like_the_ide(&registry.contributions().external_tools[0].tool, src.path());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "run-ok\n");

    let exported = src.path().join("settings-bundle.json");
    assert_eq!(
        export_bundle(a.path(), &exported).unwrap(),
        ["settings.json"]
    );
    let b = tempfile::tempdir().unwrap();
    assert_eq!(
        import_bundle(b.path(), &exported).unwrap(),
        ["settings.json"]
    );
    let imported = PluginRegistry::load(b.path());
    assert!(imported.plugins().is_empty(), "{:?}", imported.plugins());
    assert!(imported.contributions().external_tools.is_empty());
    assert!(!b.path().join("plugins").exists());
}

/// Review finding: a `$PluginDir$` program was checked before quote removal,
/// so `$PluginDir$/'..'/'..'/esc.sh` passed as a file in a `'..'` folder of
/// the plugin (and that file was made executable), while the shell ran
/// `<plugins>/../esc.sh`, outside the plugin. Programs are checked as the
/// IDE reads them, and run as they were checked.
#[cfg(unix)]
#[test]
fn programs_are_checked_as_the_ide_reads_them() {
    let src = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let mut registry = PluginRegistry::load(config.path());
    let escape = write_plugin(
        src.path(),
        "escape",
        json!({"contributes": {"external_tools": [{"name": "Run", "program": "$PluginDir$/'..'/'..'/esc.sh"}]}}),
        &[("'..'/'..'/esc.sh", "#!/bin/sh\necho escaped\n")],
    );
    let err = registry.install_from_dir(&escape).unwrap_err();
    assert!(
        err.to_string()
            .contains("points outside the plugin directory"),
        "{err}"
    );
    assert!(registry.plugins().is_empty());
    assert!(
        plugins_dir_entries(config.path()).is_empty(),
        "{:?}",
        plugins_dir_entries(config.path())
    );

    // The IDE keeps backslashes, which Windows reads as separators: a check
    // that removed them like `sh` passed `x\..\..\esc.sh` as `x....esc.sh`.
    let backslashes = write_plugin(
        src.path(),
        "backslashes",
        json!({"id": "com.example.backslashes", "contributes": {"external_tools": [{"name": "Run", "program": "$PluginDir$/x\\..\\..\\esc.sh"}]}}),
        &[("x....esc.sh", "#!/bin/sh\necho escaped\n")],
    );
    let err = registry.install_from_dir(&backslashes).unwrap_err();
    assert!(err.to_string().contains("uses `\\`"), "{err}");
    assert!(registry.plugins().is_empty());

    // Quotes that keep the program inside the plugin are fine: the file the
    // IDE runs is the one that was checked and made executable, also when
    // the quotes are inside the program (which the IDE does not remove there).
    let quoted = write_plugin(
        src.path(),
        "quoted",
        json!({"id": "com.example.quoted", "contributes": {"external_tools": [
            {"name": "Run", "program": "\"$PluginDir$\"/bin/'run'.sh"},
            {"name": "Inner", "program": "$PluginDir$/bin/'run'.sh"}
        ]}}),
        &[("bin/run.sh", "#!/bin/sh\necho quoted-ok\n")],
    );
    let outcome = registry.install_from_dir(&quoted).unwrap();
    assert_ne!(mode_of(&outcome.dir.join("bin/run.sh")) & 0o100, 0);
    for c in registry.contributions().external_tools {
        let out = run_like_the_ide(&c.tool, src.path());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "quoted-ok\n",
            "{}: {}",
            c.tool.program,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
