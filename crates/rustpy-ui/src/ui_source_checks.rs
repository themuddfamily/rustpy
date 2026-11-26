//! Checks over the sources of all the UI crates (`rustpy-ui` and the crates split
//! out of it): every dialog declares its modality, every id-salted combo box has an
//! accessible name, and the main menu and editor context menu are localized.

/// The `src` directories of the UI crates: `crates/rustpy-ui` and every
/// `crates/rustpy-ui-*` split out of it.
fn ui_source_roots() -> Vec<std::path::PathBuf> {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut roots: Vec<_> = std::fs::read_dir(crates)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "rustpy-ui" || name.starts_with("rustpy-ui-"))
        })
        .map(|path| path.join("src"))
        .collect();
    roots.sort();
    assert!(roots.len() > 1, "the UI crates under {}", crates.display());
    roots
}

/// Every Rust source of the UI crates, sorted, with its path for messages
/// (`<crate>/src/...`).
fn ui_sources() -> Vec<(std::path::PathBuf, String)> {
    let mut files = Vec::new();
    for root in ui_source_roots() {
        rust_sources(&root, &mut files);
    }
    files.sort();
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    files
        .into_iter()
        .map(|file| {
            let shown = file.strip_prefix(crates).unwrap().display().to_string();
            (file, shown)
        })
        .collect()
}

/// The end of the expression statement starting at `start`: a `;` or `,`
/// at its own nesting depth, or the bracket closing the enclosing one.
/// String and char literals are skipped so their brackets do not count.
fn statement_end(src: &str, start: usize) -> usize {
    let bytes = src.as_bytes();
    let mut depth = 0_i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // A raw string (`r"…"`, `r#"…"#`) ends at the quote and its hashes.
                let mut hashes = 0;
                let mut j = i;
                while j > 0 && bytes[j - 1] == b'#' {
                    hashes += 1;
                    j -= 1;
                }
                let raw = j > 0 && bytes[j - 1] == b'r';
                i += 1;
                while i < bytes.len() {
                    if !raw && bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"'
                        && (!raw || bytes[i + 1..].iter().take(hashes).all(|b| *b == b'#'))
                    {
                        i += if raw { hashes } else { 0 };
                        break;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                // A char literal ('x', '\n', '{'); otherwise a lifetime.
                if bytes.get(i + 1) == Some(&b'\\') {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\'' {
                        i += 1;
                    }
                } else if let Some(c) = src[i + 1..].chars().next() {
                    if src[i + 1 + c.len_utf8()..].starts_with('\'') {
                        i += c.len_utf8() + 1;
                    }
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 {
                    return i;
                }
            }
            b';' | b',' if depth == 0 => return i,
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Unlabelled combo boxes in `src` (line numbers): an id-salted combo box
/// whose statement does not name it with `AccessibleCombo`.
fn unnamed_combo_lines(src: &str) -> Vec<usize> {
    let pattern = concat!("ComboBox::", "from_id_salt(");
    src.match_indices(pattern)
        .map(|(at, _)| at)
        .filter(|&at| {
            let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
            !src[line_start..at].trim_start().starts_with("//")
        })
        .filter(|&at| {
            let statement = &src[at..statement_end(src, at)];
            !statement.contains(".combo_labelled(") && !statement.contains(".combo_named(")
        })
        .map(|at| src[..at].matches('\n').count() + 1)
        .collect()
}

#[test]
fn combo_lint_finds_unnamed_combo_boxes() {
    let unnamed = concat!(
        "fn f(ui: &mut Ui) {\n    egui::ComboBox::",
        "from_id_salt(\"a\").show_ui(ui, |ui| { ui.label(\"}\"); let c = '{'; });\n}\n"
    );
    assert_eq!(unnamed_combo_lines(unnamed), vec![2]);
    let named = concat!(
        "fn f(ui: &mut Ui) {\n    let l = ui.label(\"Theme\");\n    egui::ComboBox::",
        "from_id_salt(\"a\")\n        .show_ui(ui, |ui| { ui.label(\"x\"); })\n",
        "        .combo_labelled(&l);\n    egui::ComboBox::",
        "from_id_salt(\"b\").show_index(ui, &mut i, 2, |i| r#\"(\"#.into()).combo_named(\"Mode\");\n}\n"
    );
    assert!(unnamed_combo_lines(named).is_empty());
    // The name must belong to the same statement, not a later one.
    let later = concat!(
        "fn f(ui: &mut Ui) {\n    egui::ComboBox::",
        "from_id_salt(\"a\").show_ui(ui, |ui| {});\n    x.combo_named(\"b\");\n}\n"
    );
    assert_eq!(unnamed_combo_lines(later), vec![2]);
}

/// The text of `src[start..end]` with every bracketed group emptied, so
/// only the top level of a method chain remains:
/// `Window::new(…).open(…).show_modal(…)` → `Window::new().open().show_modal()`.
fn top_level(src: &str, start: usize, end: usize) -> String {
    let mut out = String::new();
    let mut i = start;
    while i < end {
        let c = src[i..].chars().next().unwrap();
        out.push(c);
        if matches!(c, '(' | '[' | '{') {
            let mut j = i + 1;
            loop {
                let e = statement_end(src, j);
                if e < src.len() && matches!(src.as_bytes()[e], b',' | b';') {
                    j = e + 1;
                } else {
                    i = e;
                    break;
                }
            }
            continue;
        }
        i += c.len_utf8();
    }
    out
}

/// Windows in `src` (line numbers) not shown through `DialogWindow`:
/// the `Window::new` chain, or the variable it is bound to, must end in
/// `show_modal` or `show_modeless`, never a plain `show`.
fn unclassified_window_lines(src: &str) -> Vec<usize> {
    let pattern = concat!("Window::", "new(");
    let shown = |text: &str| {
        (text.contains(".show_modal(") || text.contains(".show_modeless("))
            && !text.contains(".show(")
    };
    src.match_indices(pattern)
        .map(|(at, _)| at)
        .filter(|&at| {
            let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
            let prefix = src[line_start..at].trim_start();
            if prefix.starts_with("//") {
                return false;
            }
            let chain = top_level(src, at, statement_end(src, at));
            if shown(&chain) {
                return false;
            }
            // `let mut window = egui::Window::new(…)…;` shown later as `window.show_…`.
            let binding = prefix
                .strip_prefix("let ")
                .map(|rest| rest.trim_start_matches("mut "))
                .and_then(|rest| rest.split('=').next())
                .map(str::trim)
                .filter(|name| {
                    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
                });
            match binding {
                Some(name) => {
                    let rest = &src[at..];
                    let modal = format!("{name}.show_modal(");
                    let modeless = format!("{name}.show_modeless(");
                    let plain = format!("{name}.show(");
                    !((rest.contains(&modal) || rest.contains(&modeless)) && !rest.contains(&plain))
                }
                None => true,
            }
        })
        .map(|at| src[..at].matches('\n').count() + 1)
        .collect()
}

#[test]
fn window_lint_finds_windows_without_a_modality() {
    let w = concat!("egui::Window::", "new");
    let plain = format!(
        "fn f(ctx: &Context) {{\n    {w}(\"a\").open(&mut o).show(ctx, |ui| {{ \
         ScrollArea::new().show_modal(ui, |ui| {{}}); }});\n}}\n"
    );
    assert_eq!(unclassified_window_lines(&plain), vec![2]);
    let classified = format!(
        "fn f(ctx: &Context) {{\n    {w}(if x {{ \"a\" }} else {{ \"b\" }})\n        \
         .show_modal(ctx, |ui| {{ ui.label(\")\"); }});\n    \
         let r = {w}(\"b\").show_modeless(ctx, |ui| {{ x.show(ui) }});\n}}\n"
    );
    assert!(unclassified_window_lines(&classified).is_empty());
    let bound = format!(
        "fn f(ctx: &Context) {{\n    let mut win = {w}(\"a\");\n    win = win.open(&mut o);\n    \
         win.show_modal(ctx, |ui| {{}});\n    let mut other = {w}(\"b\");\n    \
         other.show(ctx, |ui| {{}});\n}}\n"
    );
    assert_eq!(unclassified_window_lines(&bound), vec![5]);
}

/// Every `egui::Window` in the UI sources says whether it is a modal dialog
/// (focus trapped inside it) or a non-modal tool window, so a new dialog
/// cannot silently let Tab wander into the main window behind it.
#[test]
fn every_window_declares_its_modality() {
    let mut problems = Vec::new();
    let mut modal = 0;
    for (file, shown) in ui_sources() {
        let src = std::fs::read_to_string(&file).unwrap();
        modal += src.matches(".show_modal(").count();
        for line in unclassified_window_lines(&src) {
            problems.push(format!("{shown}:{line}"));
        }
    }
    assert!(modal > 100, "the modal dialogs trap focus ({modal})");
    assert!(
        problems.is_empty(),
        "windows shown without a modality (use .show_modal(ctx, …) for a dialog that \
         blocks the main window, .show_modeless(ctx, …) for a tool window, from \
         crate::a11y::DialogWindow):\n{}",
        problems.join("\n")
    );
}

/// Every combo box built from an id salt in the UI sources has an
/// accessible name: Orca read the unnamed ones as just "combo box".
#[test]
fn every_id_salted_combo_box_has_an_accessible_name() {
    let mut problems = Vec::new();
    for (file, shown) in ui_sources() {
        let src = std::fs::read_to_string(&file).unwrap();
        for line in unnamed_combo_lines(&src) {
            problems.push(format!("{shown}:{line}"));
        }
    }
    assert!(
        problems.is_empty(),
        "combo boxes without an accessible name (chain .combo_labelled(&label) or \
         .combo_named(tr(…)) from crate::a11y::AccessibleCombo):\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_main_menu_and_the_editor_context_menu_are_localized() {
    // Their labels are English literals that `LocalizedUi` translates: without it they stay English.
    let app = include_str!("app.rs");
    let menu_bar = &app[app.find("    fn menu_bar(").expect("IdeApp::menu_bar")..];
    let menu_bar = &menu_bar[..menu_bar[1..]
        .find("\n    fn ")
        .map_or(menu_bar.len(), |end| end + 1)];
    assert!(
        menu_bar.contains("LocalizedUi::new(ui)"),
        "IdeApp::menu_bar builds its menus on crate::i18n::LocalizedUi"
    );
    let view = include_str!("editor/view.rs");
    assert!(
        context_menu_prologue(view).contains("LocalizedUi::new(ui)"),
        "the editor context menu is built on crate::i18n::LocalizedUi"
    );
}

/// The editor context menu's closure up to its first item (`run(ui, …)`): where its `ui`
/// has to become a `LocalizedUi` for the submenus to be translated.
fn context_menu_prologue(view: &str) -> &str {
    let menu = &view[view
        .find("crate::a11y::bound_keyboard_context_menu(")
        .expect("the authority-bound editor context menu")..];
    let body = &menu[menu
        .find(".show(|ui| {")
        .expect("the editor context menu's actual popup closure")..];
    &body[..body
        .find("run(ui, ")
        .expect("the context menu adds its items with run(ui, …)")]
}

#[test]
fn the_context_menu_check_reads_the_whole_prologue() {
    // Main's has_cells line, merged in before the wrapper, pushed it past the 300 characters
    // the check used to read.
    let long = format!("crate::a11y::bound_keyboard_context_menu(&resp, cfg.keyboard, authority).show(|ui| {{\n    let has_cells = {};\n    let ui = &mut crate::i18n::LocalizedUi::new(ui);\n    let mut run = |ui: &mut egui::Ui, a: Action| {{}};\n    run(ui, Action::Cut);\n", "x".repeat(400));
    assert!(context_menu_prologue(&long).contains("LocalizedUi::new(ui)"));
    let unrelated = format!(
        "other.show(|ui| {{ let ui = LocalizedUi::new(ui); run(ui, Action::Copy); }});\n{long}"
    );
    assert!(
        context_menu_prologue(&unrelated).contains("has_cells"),
        "the prologue comes from the actual bound menu, not an unrelated popup"
    );
    let late = "crate::a11y::bound_keyboard_context_menu(&resp, cfg.keyboard, authority).show(|ui| {\n    run(ui, Action::Cut);\n    let ui = &mut crate::i18n::LocalizedUi::new(ui);\n";
    assert!(
        !context_menu_prologue(late).contains("LocalizedUi::new(ui)"),
        "a wrapper after the first item does not count"
    );
}
