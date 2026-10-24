//! Actual save action regressions, each rooted in an owned project.
use super::*;
use crate::editor::Caret;
use rustpy_text::{Encoding, LineEnding};

fn fixture(name: &str, bytes: &[u8], ec: &str) -> (tempfile::TempDir, IdeApp, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    if !ec.is_empty() {
        std::fs::write(directory.path().join(".editorconfig"), ec).unwrap();
    }
    let mut settings = Settings::default();
    settings.trust_project(directory.path());
    let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
    assert_eq!(app.project.as_ref().unwrap().root, directory.path());
    assert!(app.tabs.is_empty());
    app.open_file(&path);
    assert_eq!(app.tabs[0].state.path.as_deref(), Some(path.as_path()));
    (directory, app, path)
}

#[derive(Debug, PartialEq)]
struct EditorProof {
    text: String,
    version: u64,
    document: rustpy_text::DocumentId,
    history: String,
    current: String,
    readonly: bool,
    synced: Option<u64>,
    last_op: String,
    indent: String,
    encoding: Encoding,
    ending: LineEnding,
    mixed: bool,
    modified: bool,
    carets: Vec<Caret>,
    closers: Vec<TextSize>,
    revision: Option<rustpy_core::vfs::ContentRevision>,
    override_key: Option<String>,
    generation: u64,
    notebook: Option<(String, bool, Option<u64>)>,
    bookmarks: Vec<(PathBuf, usize, Option<char>, String)>,
    breakpoints: Vec<(usize, crate::breakpoints::LineBp)>,
    highlights: Vec<TextRange>,
}
fn proof(app: &IdeApp, path: &Path) -> EditorProof {
    let state = &app.tabs[0].state;
    let doc = &state.doc;
    EditorProof {
        text: doc.text(),
        version: doc.version(),
        document: doc.identity(),
        readonly: doc.is_read_only(),
        synced: state.synced_version,
        last_op: format!("{:?}", state.last_op),
        indent: format!("{:?}", state.detected_indent),
        history: format!("{:?}", doc.undo_history()),
        current: format!("{:?}", doc.current_history()),
        encoding: doc.encoding(),
        ending: doc.line_ending(),
        mixed: doc.has_mixed_line_endings(),
        modified: doc.is_modified(),
        carets: state.carets.clone(),
        closers: state.auto_closers.clone(),
        revision: app.disk_revisions.get(path).copied().flatten(),
        override_key: app.encoding_overrides.get(path).cloned(),
        generation: app.encoding_generation,
        notebook: app.tabs[0]
            .notebook
            .as_ref()
            .map(|nb| (nb.text(), nb.is_dirty(), nb.synced_version())),
        bookmarks: app
            .bookmarks
            .iter()
            .map(|b| (b.path.clone(), b.line, b.mnemonic, b.description.clone()))
            .collect(),
        breakpoints: app
            .breakpoints
            .lines(path)
            .into_iter()
            .map(|line| (line, app.breakpoints.get(path, line).unwrap().clone()))
            .collect(),
        highlights: app.sticky_highlights_for(0),
    }
}

fn dirty_astral(app: &mut IdeApp) {
    let end = app.tabs[0].state.doc.len();
    app.tabs[0].state.apply_text_edits(
        vec![(TextRange::empty(end), "# unsaved 😀".into())],
        "Unsaved Unicode",
    );
    app.tabs[0]
        .state
        .set_carets(vec![Caret::at(TextSize::new(0)), Caret::at(end)]);
    app.tabs[0].state.auto_closers.push(TextSize::new(1));
    app.set_sticky_highlights(0, vec![TextRange::new(TextSize::new(0), TextSize::new(4))]);
}
#[test]
fn failed_latin1_save_does_not_publish_builtin_formatting() {
    let (_directory, mut app, path) =
        fixture("m.py", b"# coding: latin-1\r\nx=1  \r\ny=2  \r\n", "");
    dirty_astral(&mut app);
    app.settings.code_style.format_on_save = true;
    let before = proof(&app, &path);
    let bytes = std::fs::read(&path).unwrap();
    assert!(!app.save_tab(0));
    assert_eq!(proof(&app, &path), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(app
        .status
        .as_ref()
        .unwrap()
        .0
        .contains("cannot be encoded as Latin-1"));
}
#[test]
fn failed_latin1_save_does_not_publish_import_optimization_or_analysis_text() {
    let (_directory, mut app, path) = fixture(
        "m.py",
        b"# coding: latin-1\nimport os\nimport sys\nx=1\n",
        "",
    );
    dirty_astral(&mut app);
    app.settings.code_style.optimize_imports_on_save = true;
    let analysis = app.project.as_ref().unwrap().analysis.clone();
    let original_analysis = analysis.file_for_path(&path).unwrap().text.clone();
    let before = proof(&app, &path);
    let bytes = std::fs::read(&path).unwrap();
    assert!(!app.save_tab(0));
    assert_eq!(proof(&app, &path), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        analysis.file_for_path(&path).unwrap().text,
        original_analysis
    );
}
#[test]
fn combined_save_publishes_one_full_format_history_unit_and_mapped_markers() {
    let original = "import os\nimport sys\nx=1  \ny=2  ";
    let (_directory, mut app, path) = fixture(
        "m.py",
        original.as_bytes(),
        "root=true\n[*]\ncharset=utf-8-bom\nend_of_line=crlf\n",
    );
    app.settings.code_style.format_on_save = true;
    app.settings.code_style.optimize_imports_on_save = true;
    app.settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::All;
    app.settings.editor.ensure_newline_at_eof = true;
    app.settings.editor.keep_trailing_spaces_on_caret_line = false;
    let at = original.find("x=1").unwrap() as u32;
    app.tabs[0]
        .state
        .set_carets(vec![Caret::at(TextSize::new(at))]);
    let breakpoint = crate::breakpoints::LineBp {
        condition: "x > 0".into(),
        log_message: "μ {x}".into(),
        hit_condition: ">= 5".into(),
        enabled: false,
        temporary: true,
        ..Default::default()
    };
    app.breakpoints.set(&path, 3, breakpoint.clone());
    app.bookmarks.push(Bookmark {
        kind: rustpy_settings::BookmarkKind::Line,
        list: String::new(),
        path: path.clone(),
        line: 3,
        mnemonic: Some('x'),
        description: "x body".into(),
    });
    app.reconcile_document_markers();
    app.set_sticky_highlights(
        0,
        vec![TextRange::new(TextSize::new(at), TextSize::new(at + 1))],
    );
    let before = app.tabs[0].state.carets.clone();
    assert!(app.save_tab(0), "{:?}", app.status);
    let final_text = app.tabs[0].state.doc.text();
    assert!(!final_text.contains("import "));
    assert!(final_text.contains("x = 1"));
    assert!(final_text.ends_with('\n'));
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        bytes,
        Encoding::Utf8Bom
            .encode(&LineEnding::CrLf.apply(&final_text))
            .unwrap()
    );
    let x = TextSize::new(final_text.find("x = 1").unwrap() as u32);
    let saved_line = app.tabs[0].state.doc.line_of(x) + 1;
    assert_eq!(app.breakpoints.get(&path, saved_line), Some(&breakpoint));
    assert_eq!(app.bookmarks[0].line, saved_line);
    assert_eq!(
        app.sticky_highlights_for(0),
        vec![TextRange::new(x, x + TextSize::new(1))]
    );
    assert_eq!(
        app.tabs[0]
            .state
            .doc
            .undo_history()
            .iter()
            .filter(|entry| entry.parent.is_some())
            .count(),
        1
    );
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), original);
    assert_eq!(app.tabs[0].state.doc.encoding(), Encoding::Utf8);
    assert_eq!(app.tabs[0].state.doc.line_ending(), LineEnding::Lf);
    app.reconcile_document_markers();
    assert_eq!(app.breakpoints.get(&path, 3), Some(&breakpoint));
    assert_eq!(app.tabs[0].state.carets, before);
    assert!(app.tabs[0].state.doc.is_modified());
    assert!(!app.tabs[0].state.doc.can_undo());
    assert!(app.tabs[0].state.redo());
    app.reconcile_document_markers();
    assert_eq!(app.breakpoints.get(&path, saved_line), Some(&breakpoint));
    assert_eq!(app.tabs[0].state.doc.text(), final_text);
    assert!(!app.tabs[0].state.doc.is_modified());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
#[test]
fn save_respects_primary_selection_and_active_python_scope() {
    let text = "x=1\ny=2\n";
    let (_directory, mut app, path) = fixture("m.py", text.as_bytes(), "");
    app.settings.code_style.format_on_save = true;
    app.settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::None;
    app.tabs[0]
        .state
        .set_carets(vec![Caret::new(TextSize::new(0), TextSize::new(3))]);
    assert!(app.save_tab(0));
    assert_eq!(app.tabs[0].state.doc.text(), "x = 1\ny=2\n");
    std::fs::write(path.parent().unwrap().join("other.py"), b"z=3\n").unwrap();
    app.open_file(&path.parent().unwrap().join("other.py"));
    app.tabs[0].state.apply_text_edits(
        vec![(
            TextRange::new(TextSize::new(0), TextSize::new(5)),
            "x=9".into(),
        )],
        "Inactive current text",
    );
    assert!(app.save_tab(0));
    assert!(app.tabs[0].state.doc.text().starts_with("x=9"));
}
#[test]
fn successful_save_joins_outer_transaction_and_noop_save_creates_no_history() {
    let (_directory, mut app, path) = fixture(
        "m.py",
        b"x=1  \ny=2  ",
        "root=true\n[*]\nend_of_line=crlf\n",
    );
    app.settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::All;
    app.settings.editor.keep_trailing_spaces_on_caret_line = false;
    app.settings.editor.ensure_newline_at_eof = true;
    let original = app.tabs[0].state.doc.text();
    let ending = app.tabs[0].state.doc.line_ending();
    let outer = app.tabs[0]
        .state
        .begin_transaction("Outer owned command")
        .unwrap();
    assert!(app.save_tab(0));
    assert_eq!(app.tabs[0].state.doc.transaction_depth(), 1);
    app.tabs[0].state.commit_transaction(outer).unwrap();
    assert_eq!(
        app.tabs[0]
            .state
            .doc
            .undo_history()
            .iter()
            .filter(|entry| entry.parent.is_some())
            .count(),
        1
    );
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), original);
    assert_eq!(app.tabs[0].state.doc.line_ending(), ending);
    assert!(app.tabs[0].state.redo());
    let version = app.tabs[0].state.doc.version();
    let history = app.tabs[0].state.doc.undo_history();
    assert!(app.save_tab(0));
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert_eq!(app.tabs[0].state.doc.undo_history(), history);
    assert_eq!(std::fs::read(&path).unwrap(), b"x=1\r\ny=2\r\n");
}
#[test]
fn dirty_notebook_conflict_preserves_model_document_and_sync_state() {
    let notebook = rustpy_notebook::new_notebook();
    let (_directory, mut app, path) = fixture("m.ipynb", notebook.to_string().as_bytes(), "");
    let nb = app.tabs[0].notebook.as_mut().unwrap();
    nb.set_cell_source(0, "print('pending cell')".into());
    nb.set_dirty(true);
    let before = proof(&app, &path);
    std::fs::write(&path, b"external conflict").unwrap();
    assert!(!app.save_tab(0));
    assert_eq!(proof(&app, &path), before);
    assert_eq!(std::fs::read(&path).unwrap(), b"external conflict");
}
#[test]
fn dirty_notebook_save_publishes_json_only_after_success_and_save_all_sees_it() {
    let notebook = rustpy_notebook::new_notebook();
    let (_directory, mut app, path) = fixture("m.ipynb", notebook.to_string().as_bytes(), "");
    let source = "print('pending cell')  \n# literal trailing spaces   \n\n";
    let nb = app.tabs[0].notebook.as_mut().unwrap();
    nb.set_cell_source(0, source.into());
    nb.set_dirty(true);
    app.settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::All;
    app.settings.editor.keep_trailing_spaces_on_caret_line = false;
    app.settings.editor.ensure_newline_at_eof = true;
    assert!(!app.tabs[0].state.doc.is_modified());
    assert!(app.save_all());
    let saved: rustpy_notebook::Notebook = std::str::from_utf8(&std::fs::read(&path).unwrap())
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(saved.cells[0].source, source);
    let nb = app.tabs[0].notebook.as_ref().unwrap();
    assert!(!nb.is_dirty());
    assert_eq!(nb.synced_version(), Some(app.tabs[0].state.doc.version()));
    assert!(!app.tabs[0].state.doc.is_modified());
    assert!(app.tabs[0].state.undo());
    assert_eq!(app.tabs[0].state.doc.text(), notebook.to_string());
}
#[test]
fn caret_whitespace_toggle_preserves_markdown_and_python_strings_independently() {
    let (_directory, mut app, path) = fixture(
        "m.py",
        b"text='''a  \nb  '''\nx=1  \ny=2  ",
        "root=true\n[*]\ntrim_trailing_whitespace=true\ninsert_final_newline=true\n",
    );
    app.tabs[0].state.set_carets(vec![
        Caret::at(TextSize::new(21)),
        Caret::at(TextSize::new(27)),
    ]);
    app.settings.editor.keep_trailing_spaces_on_caret_line = false;
    assert!(app.save_tab(0));
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"text='''a  \nb  '''\n"));
    assert!(bytes.ends_with(b"x=1\ny=2\n"));
    let (_directory, mut app, path) = fixture(
        "m.md",
        b"hard break  \ntrim me   ",
        "root=true\n[*]\ntrim_trailing_whitespace=true\n",
    );
    app.settings.editor.keep_trailing_spaces_on_caret_line = false;
    app.settings.editor.ensure_newline_at_eof = false;
    assert!(app.save_tab(0));
    assert_eq!(std::fs::read(&path).unwrap(), b"hard break  \ntrim me   ");
}

#[cfg(unix)]
fn external_provider(app: &mut IdeApp, root: &Path, body: &str) -> bool {
    let python = PathBuf::from("/usr/bin/python3");
    if !python.is_file() {
        return false;
    }
    let program = format!("import sys, os, json\ntext = sys.stdin.read()\nwith open('formatter-request.json', 'w') as output:\n    json.dump({{'argv':sys.argv,'cwd':os.getcwd(),'python':sys.executable,'input':text,'pythonpath':os.environ.get('PYTHONPATH')}}, output)\n{body}\n");
    std::fs::write(root.join("black.py"), program).unwrap();
    let project = app.project.as_mut().unwrap();
    project.interpreter_rx = None;
    project.interpreter = Some(rustpy_project::Interpreter {
        path: python,
        version: "3.12".into(),
        prefix: PathBuf::from("/usr"),
        sys_path: Vec::new(),
        is_venv: false,
        platform: "linux".into(),
        kind: "owned test provider".into(),
    });
    app.settings.code_style.formatter = "black".into();
    app.settings.code_style.format_on_save = true;
    true
}
#[cfg(unix)]
#[test]
fn late_os_write_failure_after_external_format_and_cleanup_preserves_everything() {
    let (directory, mut app, path) = fixture(
        "m.py",
        b"x=1  \ny=2  ",
        "root=true\n[*]\ncharset=utf-8-bom\nend_of_line=crlf\n",
    );
    if !external_provider(&mut app, directory.path(), "os.chmod('m.py', 0o444)\nsys.stdout.write(text.replace('x=1', 'x = 1').replace('y=2', 'y = 2'))") { return; }
    app.settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::All;
    app.settings.editor.keep_trailing_spaces_on_caret_line = false;
    app.settings.editor.ensure_newline_at_eof = true;
    app.tabs[0].state.set_carets(vec![
        Caret::at(TextSize::new(0)),
        Caret::at(TextSize::new(7)),
    ]);
    app.breakpoints.set(
        &path,
        2,
        crate::breakpoints::LineBp {
            condition: "y > 0".into(),
            log_message: "μ {y}".into(),
            hit_condition: "% 3".into(),
            enabled: false,
            ..Default::default()
        },
    );
    app.bookmarks.push(Bookmark {
        kind: rustpy_settings::BookmarkKind::Line,
        list: String::new(),
        path: path.clone(),
        line: 2,
        mnemonic: Some('y'),
        description: "owned body".into(),
    });
    app.reconcile_document_markers();
    app.set_sticky_highlights(0, vec![TextRange::new(TextSize::new(0), TextSize::new(1))]);
    let before = proof(&app, &path);
    let bytes = std::fs::read(&path).unwrap();
    let permissions = std::fs::metadata(&path).unwrap().permissions();
    let history_dir = rustpy_vcs::local_history::file_dir(
        &rustpy_vcs::local_history::base_dir(),
        Some(directory.path()),
        &path,
    );
    let history = rustpy_vcs::local_history::list_entries(&history_dir);
    assert!(!app.save_tab(0));
    assert!(
        std::fs::metadata(&path).unwrap().permissions().readonly(),
        "formatter actually ran before checked-write refusal"
    );
    assert_eq!(proof(&app, &path), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        rustpy_vcs::local_history::list_entries(&history_dir),
        history
    );
    assert!(app.status.as_ref().unwrap().0.contains("file is read-only"));
    let request: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.path().join("formatter-request.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(request["input"], "x=1  \ny=2  ");
    assert_eq!(request["cwd"], directory.path().display().to_string());
    assert_eq!(
        request["argv"],
        serde_json::json!([
            directory.path().join("black.py").display().to_string(),
            "-q",
            "-"
        ])
    );
    std::fs::set_permissions(&path, permissions).unwrap();
}
#[cfg(unix)]
#[test]
fn external_formatter_failure_aborts_save_and_safe_mode_never_executes_it() {
    let (directory, mut app, path) = fixture("m.py", b"x=1  \n", "");
    if !external_provider(
        &mut app,
        directory.path(),
        "sys.stderr.write('owned provider failed\\n')\nsys.exit(17)",
    ) {
        return;
    }
    let before = proof(&app, &path);
    let bytes = std::fs::read(&path).unwrap();
    assert!(!app.save_tab(0));
    assert_eq!(proof(&app, &path), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(app
        .status
        .as_ref()
        .unwrap()
        .0
        .contains("owned provider failed"));
    std::fs::remove_file(directory.path().join("formatter-request.json")).unwrap();
    app.safe_mode = true;
    assert!(!app.save_tab(0));
    assert_eq!(proof(&app, &path), before);
    assert!(!directory.path().join("formatter-request.json").exists());
    assert!(app.status.as_ref().unwrap().0.contains("trust the project"));
}
#[cfg(unix)]
#[test]
fn external_formatter_process_formats_whole_file_with_selected_project_environment() {
    let (directory, mut app, path) = fixture("m.py", b"x=1\ny=2\n", "");
    if !external_provider(
        &mut app,
        directory.path(),
        "sys.stdout.write(text.replace('x=1', 'x = 1').replace('y=2', 'y = 2'))",
    ) {
        return;
    }
    app.tabs[0]
        .state
        .set_carets(vec![Caret::new(TextSize::new(0), TextSize::new(3))]);
    assert!(app.save_tab(0), "{:?}", app.status);
    assert_eq!(std::fs::read(&path).unwrap(), b"x = 1\ny = 2\n");
    let request: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.path().join("formatter-request.json")).unwrap(),
    )
    .unwrap();
    // The fixture records sys.executable; on macOS /usr/bin/python3 is a shim
    // that re-executes the real interpreter, so accept what Python reports.
    let reported = std::process::Command::new("/usr/bin/python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/bin/python3".into());
    assert!(
        request["python"] == "/usr/bin/python3" || request["python"] == reported,
        "{:?} is neither the selected interpreter nor its sys.executable {reported}",
        request["python"]
    );
    assert!(request["pythonpath"]
        .as_str()
        .unwrap()
        .starts_with(&directory.path().display().to_string()));
}
#[test]
fn dirty_notebook_encoding_refusal_preserves_pending_model_without_sync() {
    let notebook = rustpy_notebook::new_notebook();
    let (_directory, mut app, path) = fixture(
        "m.ipynb",
        notebook.to_string().as_bytes(),
        "root=true\n[*]\ncharset=latin1\n",
    );
    let nb = app.tabs[0].notebook.as_mut().unwrap();
    nb.set_cell_source(0, "print('😀 pending')".into());
    nb.set_dirty(true);
    let before = proof(&app, &path);
    let bytes = std::fs::read(&path).unwrap();
    assert!(!app.save_tab(0));
    assert_eq!(proof(&app, &path), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn caret_whitespace_protection_applies_to_every_head_and_final_newline_independently() {
    let (_directory, mut app, path) = fixture(
        "m.txt",
        b"a=1  \nb=2  \nc=3  ",
        "root=true\n[*]\ntrim_trailing_whitespace=true\ninsert_final_newline=true\n",
    );
    app.settings.editor.strip_trailing_spaces_on_save = rustpy_settings::StripTrailing::None;
    app.tabs[0].state.set_carets(vec![
        Caret::at(TextSize::new(0)),
        Caret::at(TextSize::new(6)),
    ]);
    assert!(app.settings.editor.keep_trailing_spaces_on_caret_line);
    assert!(app.save_tab(0));
    assert_eq!(std::fs::read(&path).unwrap(), b"a=1  \nb=2  \nc=3\n");
    app.settings.editor.keep_trailing_spaces_on_caret_line = false;
    assert!(app.save_tab(0));
    assert_eq!(std::fs::read(&path).unwrap(), b"a=1\nb=2\nc=3\n");
}
#[cfg(unix)]
#[test]
fn checked_write_conflict_after_formatter_does_not_reload_clean_editor() {
    let (directory, mut app, path) = fixture("m.py", b"x=1\n", "");
    if !external_provider(&mut app, directory.path(), "with open('m.py', 'w') as target:\n    target.write('external=2\\n')\nsys.stdout.write(text.replace('x=1','x = 1'))") { return; }
    let before = proof(&app, &path);
    assert!(!app.save_tab(0));
    assert_eq!(proof(&app, &path), before);
    assert_eq!(std::fs::read(&path).unwrap(), b"external=2\n");
    assert!(matches!(app.dialog, Dialog::ExternalChange(_)));
}

#[test]
fn save_action_strip_caret_editorconfig_matrix_uses_only_caret_heads() {
    use rustpy_settings::StripTrailing;
    for strip in [
        StripTrailing::None,
        StripTrailing::All,
        StripTrailing::ModifiedLines,
    ] {
        for protect_head in [false, true] {
            for ec_trim in [None, Some(false), Some(true)] {
                let ec = format!(
                    "root=true\n[*]\n{}",
                    ec_trim
                        .map(|value| format!("trim_trailing_whitespace={value}\n"))
                        .unwrap_or_default()
                );
                let baseline = "unchanged=1  \nchanged=2  \noutside=3  ";
                let (_directory, mut app, path) = fixture("matrix.txt", baseline.as_bytes(), &ec);
                app.settings.editor.strip_trailing_spaces_on_save = strip;
                app.settings.editor.keep_trailing_spaces_on_caret_line = protect_head;
                app.settings.editor.ensure_newline_at_eof = false;
                let change = baseline.find("changed=2").unwrap() + "changed=".len();
                app.tabs[0].state.apply_text_edits(
                    vec![(
                        TextRange::new(
                            TextSize::new(change as u32),
                            TextSize::new(change as u32 + 1),
                        ),
                        "4".into(),
                    )],
                    "Genuinely changed line",
                );
                let head = (baseline.find("\nchanged=").unwrap() + 1) as u32;
                // The anchor on unchanged line zero must never protect it.
                app.tabs[0]
                    .state
                    .set_carets(vec![Caret::new(TextSize::new(0), TextSize::new(head))]);
                assert_eq!(
                    app.tabs[0]
                        .state
                        .doc
                        .line_of(app.tabs[0].state.primary().head()),
                    1
                );
                assert_eq!(
                    app.tabs[0]
                        .state
                        .doc
                        .line_of(app.tabs[0].state.primary().sel.anchor),
                    0
                );
                let effective = match ec_trim {
                    Some(true) => StripTrailing::All,
                    Some(false) => StripTrailing::None,
                    None => strip,
                };
                let unchanged = if effective == StripTrailing::All {
                    ""
                } else {
                    "  "
                };
                let dirty = if !protect_head && effective != StripTrailing::None {
                    ""
                } else {
                    "  "
                };
                let expected =
                    format!("unchanged=1{unchanged}\nchanged=4{dirty}\noutside=3{unchanged}");
                assert!(
                    app.save_tab(0),
                    "strip={strip:?} head={protect_head} EC={ec_trim:?}: {:?}",
                    app.status
                );
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    expected.as_bytes(),
                    "strip={strip:?} head={protect_head} EC={ec_trim:?}"
                );
                assert!(!app.tabs[0].state.doc.is_modified());
            }
        }
    }
}
#[test]
fn final_newline_precedence_is_independent_and_explicit_false_removes_newlines() {
    for setting in [false, true] {
        for ec_final in [None, Some(false), Some(true)] {
            let ec = format!(
                "root=true\n[*]\n{}",
                ec_final
                    .map(|value| format!("insert_final_newline={value}\n"))
                    .unwrap_or_default()
            );
            let (_directory, mut app, path) = fixture("final.txt", b"nonempty", &ec);
            app.settings.editor.ensure_newline_at_eof = setting;
            app.settings.editor.keep_trailing_spaces_on_caret_line = false;
            // The added final newline uses the platform line separator.
            let expected = if ec_final.unwrap_or(setting) {
                if cfg!(windows) {
                    b"nonempty\r\n".as_slice()
                } else {
                    b"nonempty\n".as_slice()
                }
            } else {
                b"nonempty".as_slice()
            };
            assert!(app.save_tab(0));
            assert_eq!(std::fs::read(&path).unwrap(), expected);
        }
    }
    let (_directory, mut app, path) = fixture(
        "existing.txt",
        b"nonempty\n\n\n",
        "root=true\n[*]\ninsert_final_newline=false\n",
    );
    app.settings.editor.ensure_newline_at_eof = true;
    assert!(app.save_tab(0));
    // EditorConfig false enforces no final newline; an absent property preserves
    // existing newlines when the IDE's insertion preference is disabled.
    assert_eq!(std::fs::read(&path).unwrap(), b"nonempty");
    let (_directory, mut app, path) =
        fixture("unconstrained.txt", b"nonempty\n\n\n", "root=true\n[*]\n");
    app.settings.editor.ensure_newline_at_eof = false;
    assert!(app.save_tab(0));
    assert_eq!(std::fs::read(&path).unwrap(), b"nonempty\n\n\n");
}
