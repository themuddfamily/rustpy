use std::path::{Path, PathBuf};

use rustpy_project::{
    env::{pythonpath_for, terminal_env, venv_env},
    Interpreter, ProjectModel,
};

fn separator() -> &'static str {
    if cfg!(windows) {
        ";"
    } else {
        ":"
    }
}
fn interpreter(path: &str, prefix: &str, is_venv: bool) -> Interpreter {
    Interpreter {
        path: PathBuf::from(path),
        version: "3.12.1".into(),
        prefix: PathBuf::from(prefix),
        sys_path: Vec::new(),
        is_venv,
        platform: "unused".into(),
        kind: "venv".into(),
    }
}

#[test]
fn pythonpath_preserves_content_order_sorts_both_root_kinds_and_appends_inherited() {
    let model = ProjectModel {
        content_roots: vec!["/attached/z".into(), "/attached/a".into()],
        source_roots: vec!["/p/z-source".into(), "/p/a-source".into()],
        test_roots: vec!["/p/m-tests".into()],
        resource_roots: vec!["/resources".into()],
        excluded: vec!["/excluded".into()],
        ..Default::default()
    };
    let inherited = format!("/inherited/one{}/inherited/two", separator());
    let expected = [
        "/p",
        "/attached/z",
        "/attached/a",
        "/p/a-source",
        "/p/m-tests",
        "/p/z-source",
        inherited.as_str(),
    ]
    .join(separator());
    assert_eq!(
        pythonpath_for(Path::new("/p"), &model, true, true, Some(&inherited)),
        Some(expected)
    );
    assert_eq!(
        model.content_roots,
        vec![PathBuf::from("/attached/z"), PathBuf::from("/attached/a")]
    );
    assert_eq!(model.source_roots[0], PathBuf::from("/p/z-source"));
}

#[test]
fn root_flags_are_independent_and_empty_inheritance_adds_no_part() {
    let model = ProjectModel {
        content_roots: vec!["extra".into()],
        source_roots: vec!["src".into()],
        test_roots: vec!["tests".into()],
        ..Default::default()
    };
    assert_eq!(
        pythonpath_for(Path::new("root"), &model, true, false, None),
        Some(["root", "extra"].join(separator()))
    );
    assert_eq!(
        pythonpath_for(Path::new("root"), &model, false, true, Some("")),
        Some(["src", "tests"].join(separator()))
    );
    assert_eq!(
        pythonpath_for(Path::new("root"), &model, false, false, None),
        None
    );
    assert_eq!(
        pythonpath_for(Path::new("root"), &model, false, false, Some("")),
        None
    );
    assert_eq!(
        pythonpath_for(Path::new("root"), &model, false, false, Some("inherited")),
        Some("inherited".into())
    );
    assert_eq!(
        pythonpath_for(
            Path::new("root"),
            &ProjectModel::default(),
            false,
            true,
            None
        ),
        None
    );
}

#[test]
fn pythonpath_keeps_duplicate_empty_and_non_normalized_path_display() {
    let model = ProjectModel {
        content_roots: vec!["same".into(), "".into()],
        source_roots: vec!["same".into(), "src/../src".into()],
        test_roots: vec!["same".into()],
        ..Default::default()
    };
    assert_eq!(
        pythonpath_for(Path::new("same"), &model, true, true, Some("same")),
        Some(["same", "same", "", "same", "same", "src/../src", "same"].join(separator()))
    );
    assert_eq!(
        pythonpath_for(Path::new(""), &ProjectModel::default(), true, false, None),
        Some(String::new())
    );
}

#[test]
fn terminal_activation_gate_and_selected_interpreter_control_overrides() {
    let venv = interpreter("/p/.venv/bin/python", "/p/.venv", true);
    let expected = vec![
        ("VIRTUAL_ENV".into(), "/p/.venv".into()),
        (
            "PATH".into(),
            format!("/p/.venv/bin{}old path", separator()),
        ),
        ("VIRTUAL_ENV_PROMPT".into(), ".venv".into()),
    ];
    assert_eq!(terminal_env(true, Some(&venv), "old path"), expected);
    assert_eq!(venv_env(Some(&venv), "old path"), expected);
    assert!(terminal_env(false, Some(&venv), "old path").is_empty());
    assert!(terminal_env(true, None, "old path").is_empty());
    let system = Interpreter {
        is_venv: false,
        ..venv
    };
    assert!(terminal_env(true, Some(&system), "old path").is_empty());
    assert!(venv_env(Some(&system), "old path").is_empty());
}

#[test]
fn empty_inherited_path_and_parent_fallback_are_preserved() {
    let ordinary = interpreter("/p/bin/python", "/p", true);
    assert_eq!(
        venv_env(Some(&ordinary), "")[1].1,
        format!("/p/bin{}", separator())
    );
    let missing_path = interpreter("", "", true);
    assert_eq!(
        venv_env(Some(&missing_path), "")[1].1,
        format!(".{}", separator())
    );
    assert_eq!(venv_env(Some(&missing_path), "")[2].1, "");
    let bare_name = interpreter("python", ".", true);
    assert_eq!(
        venv_env(Some(&bare_name), "inherited")[1].1,
        format!("{}inherited", separator())
    );
    assert_eq!(venv_env(Some(&bare_name), "inherited")[2].1, "");
}

#[test]
fn unicode_spaces_and_inherited_empty_segments_are_retained() {
    let venv = interpreter("项目/virtual env/bin/python", "项目/virtual env", true);
    let inherited = format!("{}tools path{}", separator(), separator());
    assert_eq!(
        venv_env(Some(&venv), &inherited),
        vec![
            ("VIRTUAL_ENV".into(), "项目/virtual env".into()),
            (
                "PATH".into(),
                format!("项目/virtual env/bin{}{inherited}", separator())
            ),
            ("VIRTUAL_ENV_PROMPT".into(), "virtual env".into()),
        ]
    );
    assert_eq!(
        pythonpath_for(
            Path::new("项目/root space"),
            &ProjectModel::default(),
            true,
            false,
            Some(&inherited)
        ),
        Some(format!("项目/root space{}{inherited}", separator()))
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_use_the_existing_lossy_display_policy() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let path = PathBuf::from(OsString::from_vec(b"/project/\xff".to_vec()));
    let model = ProjectModel {
        source_roots: vec![path.clone()],
        ..Default::default()
    };
    assert_eq!(
        pythonpath_for(&path, &model, true, true, None),
        Some(format!("/project/�{}{}", separator(), "/project/�"))
    );
    let venv = Interpreter {
        path: path.join("bin/python"),
        prefix: path,
        ..interpreter("", "", true)
    };
    assert_eq!(venv_env(Some(&venv), "")[0].1, "/project/�");
    assert_eq!(
        venv_env(Some(&venv), "")[1].1,
        format!("/project/�/bin{}", separator())
    );
    assert_eq!(venv_env(Some(&venv), "")[2].1, "�");
}
