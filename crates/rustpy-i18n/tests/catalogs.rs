//! Catalog quality, and the coverage of the UI vocabulary.
//!
//! Hard failures are structural: catalogs parse, keep placeholders, use the
//! plural forms of their locale and only translate strings of the source
//! catalog `locales/en.json`.
//!
//! Coverage is a report, never a failure: the UI vocabulary is extracted
//! from the `rustpy-ui` sources at test time (see `support/mod.rs`), and a
//! new action, menu, tool window or settings page shows in English until it
//! is added to `locales/en.json` and translated in `zh-CN`, `ja`, `ko`, `de`,
//! `fr` and `es`. `cargo run -p rustpy-i18n --example i18n_extract` prints
//! the coverage (`-- --missing <locale>` a JSON stub of what a locale
//! lacks), and `cargo test -p rustpy-i18n -- --ignored` runs the strict
//! completeness checks.

mod support;

use std::collections::{HashMap, HashSet};

use support::{
    placeholders, read_catalog, tokenize, Entry, Tok, UiStrings, COMPLETE_LOCALES, PARTIAL_LOCALES,
};

fn ui() -> UiStrings {
    UiStrings::from_workspace().unwrap_or_else(|e| panic!("cannot extract the UI strings: {e}"))
}

fn catalog(code: &str) -> Vec<(String, Entry)> {
    read_catalog(code).unwrap_or_else(|e| panic!("{e}"))
}

fn catalog_map(code: &str) -> HashMap<String, Entry> {
    catalog(code).into_iter().collect()
}

fn all_locales() -> impl Iterator<Item = &'static str> {
    COMPLETE_LOCALES.iter().chain(PARTIAL_LOCALES).copied()
}

/// Whether `entry` translates its source (non-empty text or plural forms).
fn is_translated(entry: Option<&Entry>) -> bool {
    match entry {
        Some(Entry::Text(s)) => !s.is_empty(),
        Some(Entry::Plural(forms)) => forms.values().any(|f| !f.is_empty()),
        None => false,
    }
}

fn report(problems: &[String]) -> String {
    let shown: Vec<&str> = problems.iter().take(40).map(String::as_str).collect();
    let more = problems.len().saturating_sub(shown.len());
    format!(
        "{}{}",
        shown.join("\n"),
        if more > 0 {
            format!("\n… and {more} more")
        } else {
            String::new()
        }
    )
}

/// UI strings `locales/en.json` does not list (they show in English).
fn ui_strings_missing_from_the_source_catalog(ui: &UiStrings) -> Vec<String> {
    let source = catalog_map("en");
    ui.all()
        .into_iter()
        .filter(|s| !source.contains_key(*s))
        .map(|s| format!("  {s:?}"))
        .collect()
}

/// Per complete locale, the UI strings it does not translate.
fn untranslated_ui_strings(ui: &UiStrings) -> Vec<String> {
    let mut problems = Vec::new();
    for code in COMPLETE_LOCALES {
        let catalog = catalog_map(code);
        let missing: Vec<&str> = ui
            .all()
            .into_iter()
            .filter(|s| !is_translated(catalog.get(*s)))
            .collect();
        if !missing.is_empty() {
            problems.push(format!(
                "{code}: {} untranslated: {missing:?}",
                missing.len()
            ));
        }
    }
    problems
}

/// Source catalog entries a complete locale does not translate.
fn untranslated_source_strings() -> Vec<String> {
    let source = catalog("en");
    let mut problems = Vec::new();
    for code in COMPLETE_LOCALES {
        let catalog = catalog_map(code);
        for (key, _) in &source {
            if !is_translated(catalog.get(key)) {
                problems.push(format!("{code}: {key:?}"));
            }
        }
    }
    problems
}

#[test]
fn extraction_finds_the_ui_vocabulary() {
    // Only the shape of the vocabulary: the UI changes all the time.
    let ui = ui();
    let actions_rs =
        std::fs::read_to_string(support::ui_crate_dir().join("src/actions.rs")).unwrap();
    assert!(
        ui.actions.len() >= actions_rs.matches(" => (\"").count(),
        "every one-line `Name => (\"Id\", \"Text\", [...])` action is extracted"
    );
    assert!(ui.actions.len() > 300, "{} actions", ui.actions.len());
    for menu in ["File", "Edit", "View", "Help"] {
        assert!(
            ui.menus.iter().any(|m| m == menu),
            "top-level menu {menu} in {:?}",
            ui.menus
        );
        assert!(
            !ui.submenus.iter().any(|m| m == menu),
            "top-level menu {menu} counted as a submenu: {:?}",
            ui.submenus
        );
    }
    assert!(
        !ui.submenus.is_empty() && !ui.menu_toggles.is_empty(),
        "{ui:?}"
    );
    assert!(ui.tool_windows.len() >= 10, "{:?}", ui.tool_windows);
    assert!(ui.settings_pages.len() >= 20, "{:?}", ui.settings_pages);
    assert!(ui.settings_tree.len() >= 20, "{:?}", ui.settings_tree);
    assert!(
        !ui.tr_calls.is_empty(),
        "the UI translates strings with tr()/tr_static()"
    );
    let all = ui.all();
    assert_eq!(
        all.len(),
        all.iter().collect::<HashSet<_>>().len(),
        "deduplicated"
    );
    println!("{} strings extracted from crates/rustpy-ui:", all.len());
    for (origin, strings) in ui.groups() {
        println!("  {:>4} {origin}", strings.len());
    }
}

#[test]
fn coverage_gaps_are_reported_not_fatal() {
    // A string without a translation shows in English: list what is missing
    // (run with --nocapture to see it), the strict tests below fail on it.
    let ui = ui();
    for (title, problems) in [
        (
            "UI strings missing from locales/en.json",
            ui_strings_missing_from_the_source_catalog(&ui),
        ),
        (
            "UI strings complete locales do not translate",
            untranslated_ui_strings(&ui),
        ),
        (
            "source strings complete locales do not translate",
            untranslated_source_strings(),
        ),
        (
            "menu bar literals shown without tr()",
            ui.untranslated.iter().map(|s| format!("  {s:?}")).collect(),
        ),
        (
            "dialog literals shown without tr()",
            ui.untranslated_dialogs
                .iter()
                .map(|s| format!("  {s:?}"))
                .collect(),
        ),
    ] {
        if !problems.is_empty() {
            println!("{title}:\n{}", report(&problems));
        }
    }
}

#[test]
#[ignore = "coverage: strings missing from en.json show in English (run with --ignored)"]
fn source_catalog_lists_every_ui_string() {
    let missing = ui_strings_missing_from_the_source_catalog(&ui());
    assert!(
        missing.is_empty(),
        "locales/en.json lacks {} UI strings; add them (mapped to themselves) and translate them in every complete locale:\n{}",
        missing.len(),
        report(&missing)
    );
}

#[test]
#[ignore = "coverage: untranslated strings show in English (run with --ignored)"]
fn complete_locales_translate_every_ui_string() {
    let problems = untranslated_ui_strings(&ui());
    assert!(
        problems.is_empty(),
        "complete locales must translate every UI string:\n{}",
        report(&problems)
    );
}

#[test]
#[ignore = "coverage: untranslated strings show in English (run with --ignored)"]
fn complete_locales_translate_the_whole_source_catalog() {
    let problems = untranslated_source_strings();
    assert!(
        problems.is_empty(),
        "untranslated source strings:\n{}",
        report(&problems)
    );
}

#[test]
#[ignore = "coverage: menu labels without tr() show in English (run with --ignored)"]
fn menu_bar_literals_are_translated() {
    let ui = ui();
    assert!(
        ui.untranslated.is_empty(),
        "wrap these menu bar literals in crate::i18n::tr_static(…): {:?}",
        ui.untranslated
    );
}

#[test]
#[ignore = "coverage: list labels without tr_static() show in English (run with --ignored)"]
fn label_list_literals_are_translated() {
    let ui = ui();
    assert!(
        ui.untranslated_label_lists.is_empty(),
        "pass these looped list labels through crate::i18n::tr_static(…): {:?}",
        ui.untranslated_label_lists
    );
}

#[test]
fn label_lists_rendered_by_loops_are_extracted() {
    let src = r#"
        for (label, value) in [("Shell integration", &mut a), ("Cursor blink", &mut b)] {
            ui.checkbox(value, crate::i18n::tr_static(label));
        }
        for (id, name) in [("bash", "Bash shell"), ("zsh", "Z shell")] {
            ui.selectable_value(&mut shell, id, name);
        }
        for title in ["Plain", "Bold"] { ui.label(title); }
        for (key, value) in [("A", 1), ("B", 2)] { map.insert(key, value); }
    "#;
    let (translated, shown) = support::label_list_strings(&support::tokenize(src));
    assert_eq!(translated, ["Shell integration", "Cursor blink"]);
    assert_eq!(shown, ["Bash shell", "Z shell", "Plain", "Bold"]);
}

#[test]
fn ordinary_central_dialog_literals_are_translated() {
    let ui = ui();
    assert!(
        ui.untranslated_dialogs.is_empty(),
        "ordinary dialog labels must use translation calls: {:?}",
        ui.untranslated_dialogs
    );
    for label in [
        "Make Writable",
        "Preview in Safe Mode",
        "Expression",
        "Filter rows",
        "Change the file's disk attribute and allow editing?",
    ] {
        assert!(
            ui.dialogs.iter().any(|key| key == label),
            "actual dialog consumer missing: {label}"
        );
        for code in COMPLETE_LOCALES {
            assert!(
                is_translated(catalog_map(code).get(label)),
                "{code} lacks actual dialog key {label}"
            );
        }
    }
    assert_eq!(
        ui.tips.len(),
        10,
        "all dynamic Tip-of-the-Day source keys are extracted"
    );
}

#[test]
fn localized_dialog_formats_preserve_opaque_unicode_names_and_paths() {
    let value = "项目/quote' {untouched}/π.py";
    for code in COMPLETE_LOCALES {
        for source in ["Save changes to {title}?", "Quick Definition — {title}"] {
            let translated = rustpy_i18n::tr_in(code, source);
            assert_ne!(translated.as_ref(), source, "{code}: dialog fallback");
            let rendered = rustpy_i18n::fill(&translated, &[("title", &value)]);
            assert!(rendered.contains(value), "opaque user text changed: {code}");
            assert!(!rendered.contains("{title}"));
        }
        let template =
            rustpy_i18n::tr_in(code, "Rename {path} and update imports across the project");
        let rendered = rustpy_i18n::fill(&template, &[("path", &value)]);
        assert!(rendered.contains(value));
        assert!(!rendered.contains("{path}"));
    }
}

#[test]
fn direct_menu_buttons_and_dialog_widgets_cannot_silently_escape_extraction() {
    let source = r##"
        fn menu_bar(&mut self, ui: &mut Ui) {
            ui.button("Untranslated action");
            ui.small_button(tr_static("Translated action"));
        }
        fn other() { ui.label("Not a dialog"); }
        fn dialogs(&mut self, ctx: &Context) {
            Window::new(tr_static("Title")).show(ctx, |ui| {
                ui.label("Untranslated body");
                ui.button(tr_static("Translated button"));
                ui.label(RichText::new("Untranslated rich text").strong());
                TextEdit::singleline(&mut text).hint_text("Untranslated hint");
                ui.label(crate::i18n::tr_fmt("Save {path}?", &[("path", &path)]));
                ui.label(path.display().to_string());
                ui.label(provider_diagnostic);
                ui.heading("RustPy");
                TextEdit::singleline(&mut regex).hint_text("IGNORECASE|MULTILINE|VERBOSE");
                TextEdit::singleline(&mut executable).hint_text("python3");
                ui.label("#"); Button::new("◀"); Button::new("▶");
            });
        }
    "##;
    let tokens = tokenize(source);
    let menu = support::menu_strings(&tokens);
    assert_eq!(menu.labels, ["Untranslated action", "Translated action"]);
    assert_eq!(menu.untranslated, ["Untranslated action"]);
    let dialogs = support::dialog_strings(&tokens);
    assert_eq!(
        dialogs.labels,
        [
            "Title",
            "Untranslated body",
            "Translated button",
            "Untranslated rich text",
            "Untranslated hint",
            "Save {path}?"
        ]
    );
    assert_eq!(
        dialogs.untranslated,
        [
            "Untranslated body",
            "Untranslated rich text",
            "Untranslated hint"
        ]
    );
}

#[test]
fn partial_locales_are_reported() {
    let source = catalog("en");
    for code in PARTIAL_LOCALES {
        let catalog = catalog_map(code);
        let done = source
            .iter()
            .filter(|(key, _)| is_translated(catalog.get(key)))
            .count();
        println!("{code}: {done}/{} strings translated", source.len());
        assert!(
            done > source.len() / 4,
            "{code} translates too little to be offered"
        );
    }
}

#[test]
fn catalogs_only_translate_source_strings() {
    let source = catalog_map("en");
    let mut problems = Vec::new();
    for code in all_locales() {
        for (key, _) in catalog(code) {
            if !source.contains_key(&key) {
                problems.push(format!(
                    "{code}: {key:?} is not in locales/en.json (typo or removed string?)"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn source_catalog_is_the_identity() {
    for (key, entry) in catalog("en") {
        match entry {
            Entry::Text(text) => assert_eq!(text, key, "en.json maps each string to itself"),
            Entry::Plural(forms) => {
                assert_eq!(
                    forms.keys().map(String::as_str).collect::<Vec<_>>(),
                    ["one", "other"],
                    "{key:?}"
                );
                assert_eq!(
                    forms["one"], key,
                    "the key of a plural entry is its singular form"
                );
                assert_ne!(forms["other"], key, "{key:?} needs a distinct plural form");
            }
        }
    }
}

#[test]
fn plural_entries_have_the_forms_of_the_locale() {
    let source = catalog_map("en");
    let mut problems = Vec::new();
    for code in all_locales() {
        let required: Vec<&str> = rustpy_i18n::plural_categories(code)
            .iter()
            .map(|c| c.name())
            .collect();
        let single_form = required == ["other"];
        for (key, entry) in catalog(code) {
            let source_is_plural = matches!(source.get(&key), Some(Entry::Plural(_)));
            match entry {
                Entry::Plural(forms) => {
                    if !source_is_plural {
                        problems.push(format!(
                            "{code}: {key:?} has plural forms but the source is a plain string"
                        ));
                    }
                    for (name, form) in &forms {
                        if rustpy_i18n::PluralCategory::from_name(name).is_none() {
                            problems.push(format!(
                                "{code}: {key:?} has an unknown plural category {name:?}"
                            ));
                        }
                        if form.is_empty() {
                            problems.push(format!("{code}: {key:?} has an empty {name:?} form"));
                        }
                    }
                    for name in &required {
                        if !forms.contains_key(*name) {
                            problems.push(format!(
                                "{code}: {key:?} lacks the {name:?} form ({required:?} needed)"
                            ));
                        }
                    }
                }
                Entry::Text(_) if source_is_plural && !single_form => {
                    problems.push(format!(
                        "{code}: {key:?} needs plural forms {required:?}, not a single string"
                    ));
                }
                Entry::Text(_) => {}
            }
        }
    }
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn translations_keep_placeholders_ellipses_and_breadcrumbs() {
    let mut problems = Vec::new();
    for code in all_locales() {
        for (key, entry) in catalog(code) {
            for form in entry.forms() {
                if form.is_empty() {
                    continue;
                }
                if placeholders(form) != placeholders(&key) {
                    problems.push(format!("{code}: {key:?} → {form:?}: placeholders differ"));
                }
                if key.ends_with('…') != form.ends_with('…') {
                    problems.push(format!(
                        "{code}: {key:?} → {form:?}: a trailing … (opens a dialog) must be kept"
                    ));
                }
                if form.trim() != form || form.contains("  ") {
                    problems.push(format!("{code}: {key:?} → {form:?}: stray whitespace"));
                }
                if key.matches(" › ").count() != form.matches(" › ").count() {
                    problems.push(format!(
                        "{code}: {key:?} → {form:?}: the › path must keep its segments"
                    ));
                }
                if form.contains("...") {
                    problems.push(format!("{code}: {key:?} → {form:?}: use … instead of ..."));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn cjk_locales_are_not_left_in_english() {
    // Product names and conventions remain in Latin script; × is a language-neutral close symbol.
    const VERBATIM: &[&str] = &[
        "Git",
        "TODO",
        "AI Assistant",
        "OK",
        "Mercurial / SVN / Perforce",
        "SciView",
        "×",
    ];
    // Tool, format, encoding and language names, commands and code identifiers
    // shown in settings lists, plus the B / I / H1 / H2 formatting buttons.
    const TECHNICAL: &[&str] = &[
        "+ FTP",
        "+ FTPS",
        "+ SFTP",
        "+ rsync",
        "Ansible / Helm",
        "B",
        "Black",
        "Buildout",
        "CR",
        "CRLF",
        "CRLF - Windows (\\r\\n)",
        "CSS",
        "CSV",
        "Chameleon",
        "Collaboration",
        "Compose",
        "Compose down",
        "Compose up",
        "Deutsch",
        "Django",
        "Docker",
        "Docker Compose",
        "Epytext",
        "Español",
        "Fixup…",
        "Français",
        "Google",
        "GraphQL",
        "H1",
        "H2",
        "HTML",
        "I",
        "ISO-8859-1",
        "JSON",
        "JavaScript",
        "Jinja2",
        "KDialog",
        "LF",
        "Makefile",
        "Mako",
        "NumPy",
        "PEP 8",
        "Prettier",
        "Pyramid pserve",
        "Python",
        "READ COMMITTED",
        "READ UNCOMMITTED",
        "REPEATABLE READ",
        "Ruff",
        "SARIF",
        "SDK",
        "SERIALIZABLE",
        "SQL",
        "SSH",
        "ShellCheck",
        "Stderr",
        "TSV",
        "UTF-16 BE",
        "UTF-16 LE",
        "UTF-8",
        "UTF-8 BOM",
        "Vagrant",
        "Viridis",
        "WSL",
        "WebSocket",
        "XML",
        "YAML",
        "Zenity",
        "ascii",
        "autopep8",
        "aws",
        "basic",
        "bearer",
        "behave",
        "check --deploy",
        "code_style",
        "compose",
        "config",
        "curl",
        "dict",
        "digest",
        "docker",
        "doctest",
        "epytext",
        "gRPC",
        "google",
        "gtr",
        "html",
        "ipynb",
        "isort",
        "latin-1",
        "lettuce",
        "list",
        "makemigrations",
        "migrate",
        "mptr",
        "nose",
        "nose2",
        "npm install",
        "numpy",
        "oauth2",
        "pdf",
        "ptr",
        "py",
        "pyqt5",
        "pyqt6",
        "pyside2",
        "pyside6",
        "pytest",
        "reStructuredText",
        "rest",
        "rsync",
        "scp",
        "set",
        "shfmt",
        "showmigrations --plan",
        "ssh",
        "tox",
        "trial",
        "tuple",
        "unittest",
        "utf-16be",
        "utf-16le",
        "utf-8",
        "vagrant",
        "windows-1252",
        "wsl",
        "yapf",
        "💡",
    ];
    let is_cjk = |c: char| matches!(c, '\u{3000}'..='\u{30FF}' | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' | '\u{FF00}'..='\u{FFEF}');
    let mut problems = Vec::new();
    for code in ["zh-CN", "ja", "ko"] {
        for (key, entry) in catalog(code) {
            for form in entry.forms() {
                if !VERBATIM.contains(&key.as_str())
                    && !TECHNICAL.contains(&key.as_str())
                    && !form.chars().any(is_cjk)
                {
                    problems.push(format!("{code}: {key:?} → {form:?}"));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "untranslated-looking CJK entries:\n{}",
        report(&problems)
    );
}

#[test]
fn settings_breadcrumbs_match_the_tree() {
    let mut problems = Vec::new();
    for code in all_locales() {
        let texts: HashMap<String, String> = catalog(code)
            .into_iter()
            .filter_map(|(k, e)| match e {
                Entry::Text(t) if !t.is_empty() => Some((k, t)),
                _ => None,
            })
            .collect();
        for (key, translation) in &texts {
            if !key.contains(" › ") {
                continue;
            }
            for (segment, translated) in key.split(" › ").zip(translation.split(" › ")) {
                if let Some(expected) = texts.get(segment) {
                    if expected != translated {
                        problems.push(format!("{code}: {key:?} → {translation:?}: {segment:?} is {expected:?} in the settings tree"));
                    }
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", report(&problems));
}

#[test]
fn every_catalog_file_is_a_registered_locale() {
    let mut files: Vec<String> = std::fs::read_dir(support::locales_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".json")
                .map(str::to_owned)
        })
        .collect();
    files.sort();
    let mut expected: Vec<String> = rustpy_i18n::available_locales()
        .into_iter()
        .map(|(code, _)| code.to_owned())
        .collect();
    expected.sort();
    assert_eq!(
        files, expected,
        "one locales/<code>.json per available locale (en.json is the source catalog)"
    );
    let mut listed: Vec<&str> = all_locales().collect();
    listed.push("en");
    listed.sort_unstable();
    assert_eq!(
        listed,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "every locale is complete or partial"
    );
}

#[test]
fn embedded_catalogs_match_the_files() {
    for code in all_locales() {
        for (key, entry) in catalog(code) {
            if let Entry::Text(text) = &entry {
                if !text.is_empty() {
                    assert_eq!(
                        rustpy_i18n::tr_in(code, &key),
                        text.as_str(),
                        "{code}: {key:?}"
                    );
                }
            }
        }
    }
    // The coverage the IDE shows is the one of the files.
    let source = catalog("en");
    for code in all_locales() {
        let file = catalog_map(code);
        let translated = source
            .iter()
            .filter(|(key, _)| is_translated(file.get(key)))
            .count();
        assert_eq!(
            rustpy_i18n::translation_coverage(code),
            Some((translated, source.len())),
            "{code} coverage"
        );
    }
}

#[test]
fn extraction_tokenizes_rust_syntax() {
    let src = r####"
        // "commented out" => ("x", "y", [])
        /* block /* nested */ "also commented" */
        fn f<'a>(x: &'a str) -> char { let _ = '"'; let _ = '\''; let _ = '\u{2026}'; 'label: loop {} }
        const S: &str = "esc \"q\" \\ \u{2026} \x41 \
                         continued";
        const R: &str = r#"raw "quoted""#;
        const B: &[u8] = b"bytes";
    "####;
    let strings: Vec<String> = tokenize(src)
        .into_iter()
        .filter_map(|t| match t {
            Tok::Str(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(
        strings,
        ["esc \"q\" \\ … A continued", "raw \"quoted\"", "bytes"]
    );

    let tools = r#"
        impl BottomTool {
            fn title(self) -> &'static str { tr_static(self.stable_name()) }
            fn stable_name(self) -> &'static str { match self { Self::Run => "Run", Self::Debug => "Debug" } }
            fn unrelated(self) -> &'static str { "Not a tool title" }
        }
    "#;
    assert_eq!(
        support::fn_strings(&tokenize(tools), "BottomTool", "title"),
        ["Run", "Debug"]
    );
    let recursive =
        r#"impl Loop { fn title(self) { self.other() } fn other(self) { self.title() } }"#;
    assert!(support::fn_strings(&tokenize(recursive), "Loop", "title").is_empty());

    let actions = r#"
        macro_rules! actions { ($( $variant:ident => ($id:literal, $text:literal, [$($chord:literal),*]) ),* $(,)?) => {}; }
        actions! {
            // ----- File -----
            NewFile => ("NewFile", "New File…", ["Ctrl+Alt+Insert"]),
            Split => (
                "Split",
                "Split \"Right\"",
                [],
            ),
        }
    "#;
    assert_eq!(
        support::action_texts(&tokenize(actions)),
        ["New File…", "Split \"Right\""]
    );

    let menus = r#"
        fn other() { ui.menu_button("Not a menu", |ui| {}); }
        fn menu_bar(&mut self, ui: &mut egui::Ui) {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    ui.menu_button("Recent Projects", |ui| { ui.label("(none)"); });
                    ui.checkbox(&mut self.flag, "Status Bar");
                });
                ui.menu_button(egui::RichText::new(label), |ui| {});
                ui.menu_button("Help", |ui| {});
            });
        }
    "#;
    let m = support::menu_strings(&tokenize(menus));
    assert_eq!(m.menus, ["File", "Help"]);
    assert_eq!(m.submenus, ["Recent Projects"]);
    assert_eq!(m.toggles, ["Status Bar"]);
    assert_eq!(m.labels, ["(none)"]);
    assert_eq!(m.untranslated, ["(none)"]);

    // Labels wrapped in a translation call, constructed widgets and tooltips.
    let menus = r#"
        fn menu_bar(&mut self, ui: &mut egui::Ui) {
            egui::MenuBar::new().ui(ui, |ui| {
                let ui = &mut crate::i18n::LocalizedUi::new(ui);
                ui.menu_button(tr("View"), |ui| {
                    ui.menu_button(crate::i18n::tr_static("Appearance"), |ui| {
                        ui.checkbox(&mut self.a, tr_static("Status Bar"));
                        ui.add_enabled(false, egui::Checkbox::new(&mut self.b, crate::i18n::tr_static("Navigation Bar"))).on_disabled_hover_text(crate::i18n::tr_static("Not supported yet"));
                        ui.add(egui::Checkbox::new(&mut self.c, "Compact"));
                        ui.add(egui::Button::new("Plain")).on_hover_text("Tip");
                        ui.add(egui::Button::new(a.text()).shortcut_text(shortcut));
                        ui.label(tr_static("(none)"));
                    });
                    ui.menu_button(egui::RichText::new(label), |ui, extra| {});
                });
            });
        }
    "#;
    let m = support::menu_strings(&tokenize(menus));
    assert_eq!(m.menus, ["View"]);
    assert_eq!(m.submenus, ["Appearance"]);
    assert_eq!(m.toggles, ["Status Bar", "Navigation Bar", "Compact"]);
    assert_eq!(m.labels, ["Plain", "(none)"]);
    assert_eq!(m.hover_texts, ["Not supported yet", "Tip"]);
    assert_eq!(m.untranslated, ["Compact", "Plain", "Tip"]);

    let view = r#"
        fn ui() {
            resp.context_menu(|ui| {
                let ui = &mut crate::i18n::LocalizedUi::new(ui);
                run(ui, Action::Cut);
                ui.menu_button("Refactor", |ui| { run(ui, Action::Rename); });
                ui.menu_button(tr("Go To"), |ui| {});
            });
            ui.menu_button("Elsewhere", |ui| {});
        }
    "#;
    assert_eq!(
        support::context_menu_strings(&tokenize(view)),
        ["Refactor", "Go To"]
    );

    let calls = r#"
        use rustpy_i18n::{tr, tr_static};
        pub fn tr(source: &str) -> String { source.to_owned() }
        fn f() {
            ui.label(tr("Language"));
            ui.button(crate::i18n::tr_static("OK"));
            let s = rustpy_i18n::tr_fmt("Rename {name}", &[("name", &n)]);
            let n = tr_n("{n} file", "{n} files", 3);
            let x = other.tr("not a translation");
            let y = tr(&dynamic);
        }
    "#;
    assert_eq!(
        support::tr_calls(&tokenize(calls)),
        ["Language", "OK", "Rename {name}", "{n} file"]
    );
}
