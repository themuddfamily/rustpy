//! Actual shortcut-before-editor frames: language ownership and coalesced input.
use super::*;

fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    window_frame(app, ctx, time, events, true).drop_without_applying_deltas();
}

/// One frame of the real shortcut → editor order; `window_focused` is the OS
/// window's activation (egui's `RawInput::focused`).
fn window_frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
    window_focused: bool,
) -> egui::FullOutput {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: window_focused,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    )
}

fn document(name: &str, source: &str) -> (tempfile::TempDir, IdeApp, egui::Context) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(name);
    std::fs::write(&path, source).unwrap();
    let mut settings = Settings::default();
    // Leave completion enabled, including context-character insertion and
    // native document suggestions. No external language tool is admitted here.
    settings.completion.auto_popup_delay_ms = 0;
    // Match the genuine native HTML-save case's independent pairing policy;
    // all completion/pop-up/context-character preferences remain enabled.
    settings.editor.auto_close_brackets = false;
    settings.editor.auto_close_quotes = false;
    settings.editor.auto_close_triple_quotes = false;
    settings.editor.surround_selection_on_typing = false;
    settings.web_development.enabled = false; // native fallback, no installed provider admission
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    let index = app.active.unwrap();
    let end = app.tabs[index].state.doc.len();
    app.tabs[index]
        .state
        .set_carets(vec![crate::editor::Caret::at(end)]);
    app.focus_editor = true;
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.02, vec![]);
    assert!(ctx.memory(|memory| memory.has_focus(app.tabs[index].view.id)));
    assert!(app.settings.completion.show_as_you_type);
    assert!(app.settings.completion.insert_by_context_characters);
    (root, app, ctx)
}

#[test]
fn non_python_prefixes_never_fall_through_to_python_semantic_completion() {
    for (name, source) in [
        ("index.html", "<!doctype"),
        ("index.html", "<title>actual embedded"),
        ("style.css", "unknown_css_prefix"),
        ("app.js", "unknownJsPrefix"),
    ] {
        let (_root, mut app, ctx) = document(name, source);
        let original = app.tabs[app.active.unwrap()].state.doc.text();
        app.open_completion(true);
        assert!(
            !app.completion.visible,
            "{name}: a missing language candidate must not admit Python symbols: {:?}",
            app.completion.items
        );
        app.open_smart_completion();
        assert!(
            !app.completion.visible,
            "{name}: smart completion must retain language ownership"
        );
        frame(&mut app, &ctx, 0.04, vec![egui::Event::Text(" ".into())]);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            format!("{original} ")
        );
    }
}

#[test]
fn batched_web_prefix_and_space_preserve_exact_typed_text_with_native_completion() {
    for (name, before, typed, item, expected) in [
        ("index.html", "<met", "a", "meta", "<meta "),
        ("index.html", "<scrip", "t", "script", "<script "),
        ("style.css", "backgroun", "d", "background", "background "),
        ("app.js", "cons", "t", "const", "const "),
    ] {
        let (_root, mut app, ctx) = document(name, before);
        app.open_completion(false);
        assert!(
            app.completion.visible,
            "native completion remains enabled for {name}"
        );
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|candidate| candidate.label == item)
            .expect("real native language candidate");
        // Exercise the production order: global completion shortcuts see BOTH
        // text events before editor_area inserts either one.
        frame(
            &mut app,
            &ctx,
            0.04,
            vec![
                egui::Event::Text(typed.into()),
                egui::Event::Text(" ".into()),
            ],
        );
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            expected,
            "{name}: no stale-prefix completion may duplicate the final typed character"
        );
    }
}

#[test]
fn single_python_context_character_still_commits_but_earlier_text_does_not() {
    for (events, expected) in [
        (vec![egui::Event::Text(" ".into())], "value = 1\nvalue "),
        (
            vec![egui::Event::Text("u".into()), egui::Event::Text(" ".into())],
            "value = 1\nvalu ",
        ),
    ] {
        let (_root, mut app, ctx) = document("owned.py", "value = 1\nval");
        app.open_completion(false);
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|candidate| candidate.label == "value")
            .expect("real Python local candidate");
        frame(&mut app, &ctx, 0.04, events);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), expected);
    }
}

const NATIVE_HTML_SAVE_PAYLOAD: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>actual embedded &#960; saved</title><link rel="stylesheet" href="assets/style%20%CF%80.css"></head><body data-phase="saved"><div id="panel"><p>Waiting for actual JavaScript</p></div><script src="assets/app%20%CE%BB.js"></script></body></html>"#;

#[test]
fn native_html_save_payload_survives_automatic_completion_and_real_frame_batching() {
    assert_eq!(
        NATIVE_HTML_SAVE_PAYLOAD.len(),
        298,
        "retain the exact failed native input"
    );
    for batch in [1, 3] {
        let (_root, mut app, ctx) = document("index.html", "");
        let chars = NATIVE_HTML_SAVE_PAYLOAD.chars().collect::<Vec<_>>();
        let mut time = 0.04;
        for chunk in chars.chunks(batch) {
            let events = chunk
                .iter()
                .map(|character| egui::Event::Text(character.to_string()))
                .collect();
            frame(&mut app, &ctx, time, events);
            time += 0.04;
            // Let the actual auto-popup timer run between native-like batches.
            frame(&mut app, &ctx, time, vec![]);
            time += 0.04;
        }
        let index = app.active.unwrap();
        assert_eq!(
            app.tabs[index].state.doc.text(),
            NATIVE_HTML_SAVE_PAYLOAD,
            "batch {batch}: no completion may inject HTML words into prose/values/empty tokens"
        );
        assert!(
            !app.completion.visible,
            "closing </html> must not leave a context-free keyword lookup"
        );
        assert!(app.save_tab(index));
        // The added final newline uses the platform line separator.
        assert_eq!(
            std::fs::read_to_string(app.tabs[index].state.path.as_ref().unwrap())
                .unwrap()
                .replace("\r\n", "\n"),
            format!("{NATIVE_HTML_SAVE_PAYLOAD}\n"),
            "actual Save must publish the typed bytes plus its configured final newline"
        );
    }
}

#[test]
fn html_prose_values_comments_declarations_and_raw_text_do_not_context_commit() {
    for source in [
        "<p>for",
        "<title>for",
        "<a href=\"for",
        "<a href='for",
        "<a href=for",
        "<!-- for",
        "<!doctype for",
        "<script>for",
        "<style>for",
        "</html>",
        "<div ",
    ] {
        let (_root, mut app, ctx) = document("index.html", source);
        app.open_completion(false);
        assert!(
            !app.completion.visible,
            "no automatic HTML name context: {source}"
        );
        app.open_completion(true);
        // Explicit invocation may browse vocabulary, but merely typing a space
        // is not manual selection and must never commit it in these contexts.
        frame(&mut app, &ctx, 0.04, vec![egui::Event::Text(" ".into())]);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            format!("{source} ")
        );
    }
}

#[test]
fn html_tag_attribute_and_explicit_selection_completion_remain_available() {
    for (source, item, commit, expected) in [
        ("<scr", "script", " ", "<script "),
        ("<div cla", "class", "=", "<div class="),
    ] {
        let (_root, mut app, ctx) = document("index.html", source);
        app.open_completion(false);
        assert!(app.completion.visible);
        app.completion.selected = app
            .completion
            .items
            .iter()
            .position(|candidate| candidate.label == item)
            .unwrap();
        frame(&mut app, &ctx, 0.04, vec![egui::Event::Text(commit.into())]);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), expected);
    }
    let (_root, mut app, ctx) = document("index.html", "<p>for");
    app.open_completion(true);
    app.completion.selected = app
        .completion
        .items
        .iter()
        .position(|candidate| candidate.label == "form")
        .expect("manual HTML vocabulary remains available");
    frame(
        &mut app,
        &ctx,
        0.04,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert_eq!(
        app.tabs[app.active.unwrap()].state.doc.text(),
        "<p>form",
        "explicit selected Enter is distinct from ordinary punctuation"
    );
}

fn ctrl_space() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Space,
        physical_key: Some(egui::Key::Space),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    }
}

fn lookup_labels(app: &IdeApp) -> Vec<String> {
    app.completion
        .items
        .iter()
        .map(|item| item.label.to_string())
        .collect()
}

#[test]
fn ctrl_space_shows_drawn_lookups_through_real_action_dispatch() {
    for (name, source, expected) in [
        ("Dockerfile", "", "FROM"),
        ("Dockerfile", "FROM python:3.12\nEXP", "EXPOSE"),
        (".dockerignore", "ven", "venv"),
        (".dockerignore", "Dock", "Dockerfile"),
        (".dockerignore", ".py", ".pytest_cache"),
        (".gitignore", ".py", ".pytest_cache/"),
        ("pyproject.toml", "[tool", "[tool.ruff]"),
        // The shared lookup path also serves Python and web documents.
        ("index.html", "<scr", "script"),
        (
            "views.py",
            "def reverse_url(name):\n    return name\n\n\nx = rev",
            "reverse_url",
        ),
    ] {
        let (_root, mut app, ctx) = document(name, source);
        assert!(!app.completion.visible);
        // The real keymap → global shortcut → action dispatch → provider →
        // editor frame that draws the lookup.
        frame(&mut app, &ctx, 0.04, vec![ctrl_space()]);
        frame(&mut app, &ctx, 0.08, vec![]);
        frame(&mut app, &ctx, 0.5, vec![]);
        let index = app.active.unwrap();
        assert_eq!(
            app.tabs[index].state.doc.text(),
            source,
            "{name}: Ctrl+Space must not edit the document"
        );
        assert!(
            app.completion.visible && app.completion.explicit,
            "{name} {source:?}: explicit lookup open"
        );
        assert!(
            app.completion.rect.is_some(),
            "{name} {source:?}: lookup drawn on screen"
        );
        assert!(
            lookup_labels(&app).iter().any(|label| label == expected),
            "{name} {source:?}: {:?}",
            lookup_labels(&app)
        );
    }
}

#[test]
fn typing_auto_pops_drawn_lookups_in_non_python_documents() {
    for (name, before, typed, expected) in [
        ("Dockerfile", "FROM python\n", "RU", "RUN"),
        (".dockerignore", "", "ven", "venv"),
        (".gitignore", "", "__py", "__pycache__/"),
        ("pyproject.toml", "[project]\n", "requ", "requires-python"),
        ("docker-compose.yml", "", "serv", "services:"),
        ("index.html", "", "<scr", "script"),
        ("style.css", "a { ", "backg", "background"),
    ] {
        let (_root, mut app, ctx) = document(name, before);
        let mut time = 0.04;
        for character in typed.chars() {
            frame(
                &mut app,
                &ctx,
                time,
                vec![egui::Event::Text(character.to_string())],
            );
            // Let the auto-popup timer fire between keystrokes.
            frame(&mut app, &ctx, time + 0.02, vec![]);
            time += 0.04;
        }
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            format!("{before}{typed}")
        );
        assert!(
            app.completion.visible && app.completion.rect.is_some(),
            "{name} {typed:?}: automatic lookup drawn"
        );
        assert!(
            lookup_labels(&app).iter().any(|label| label == expected),
            "{name} {typed:?}: {:?}",
            lookup_labels(&app)
        );
    }
}

#[test]
fn lookups_survive_an_inactive_window_but_close_when_ide_focus_moves() {
    for (name, source) in [
        ("Dockerfile", "FROM python:3.12\nEXP"),
        ("pyproject.toml", "[tool"),
        (
            "views.py",
            "def reverse_url(name):\n    return name\n\n\nx = rev",
        ),
    ] {
        let (_root, mut app, ctx) = document(name, source);
        frame(&mut app, &ctx, 0.04, vec![ctrl_space()]);
        assert!(app.completion.visible, "{name}: opened");
        // Alt+Tab / a headless display: the OS window is inactive, the
        // editor still owns IDE focus.
        for step in 0..3 {
            window_frame(&mut app, &ctx, 0.1 + step as f64 * 0.1, vec![], false)
                .drop_without_applying_deltas();
        }
        assert!(
            app.completion.visible,
            "{name}: an inactive window keeps the lookup (same policy as Python)"
        );
        frame(&mut app, &ctx, 0.5, vec![]);
        assert!(app.completion.visible, "{name}: back in the window");
        // Focus moves elsewhere inside the IDE: the lookup closes.
        let view = app.tabs[app.active.unwrap()].view.id;
        ctx.memory_mut(|memory| memory.surrender_focus(view));
        frame(&mut app, &ctx, 0.6, vec![]);
        assert!(
            !app.completion.visible,
            "{name}: lookup closes when the editor loses IDE focus"
        );
    }
}

#[test]
fn unclaimed_non_python_contexts_drop_the_previous_lookup_items() {
    // Cargo.toml has no pyproject vocabulary; `[` must not leave Poetry/uv
    // tables behind as hidden items of a later lookup.
    let (_root, mut app, ctx) = document("Cargo.toml", "");
    let mut time = 0.04;
    for character in "[pack".chars() {
        frame(
            &mut app,
            &ctx,
            time,
            vec![egui::Event::Text(character.to_string())],
        );
        frame(&mut app, &ctx, time + 0.02, vec![]);
        time += 0.04;
    }
    frame(&mut app, &ctx, time, vec![ctrl_space()]);
    assert!(!app.completion.visible);
    assert!(
        app.completion.items.is_empty(),
        "stale items: {:?}",
        lookup_labels(&app)
    );
}

#[test]
fn lookup_rows_are_not_clipped_by_the_list_viewport() {
    let (_root, mut app, ctx) = document("pyproject.toml", "[tool");
    frame(&mut app, &ctx, 0.04, vec![ctrl_space()]);
    let labels = lookup_labels(&app);
    assert_eq!(labels.len(), 4, "{labels:?}");
    let output = window_frame(&mut app, &ctx, 0.08, vec![], true);
    let mut painted = 0;
    for clipped in &output.shapes {
        if let egui::Shape::Text(text) = &clipped.shape {
            if labels.iter().any(|label| text.galley.text() == label) {
                painted += 1;
                let rect = text.visual_bounding_rect();
                assert!(
                    rect.max.y <= clipped.clip_rect.max.y + 0.5,
                    "{:?} clipped: {rect:?} outside {:?}",
                    text.galley.text(),
                    clipped.clip_rect
                );
            }
        }
    }
    assert_eq!(painted, 4, "every row of a short list is painted");
}

#[test]
fn markdown_link_targets_and_fence_languages_use_the_shared_lookup() {
    for (source, expected) in [
        ("# Intro\n\nSee [the intro](#in", "#intro"),
        ("See [readme](REA", "README.md"),
        ("```py", "python"),
    ] {
        let (_root, mut app, ctx) = document("README.md", source);
        frame(&mut app, &ctx, 0.04, vec![ctrl_space()]);
        frame(&mut app, &ctx, 0.08, vec![]);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), source);
        assert!(
            app.completion.visible && app.completion.rect.is_some(),
            "{source:?}: drawn lookup"
        );
        assert!(
            lookup_labels(&app).iter().any(|label| label == expected),
            "{source:?}: {:?}",
            lookup_labels(&app)
        );
    }
    // Prose, closed links and closing fences offer nothing.
    for source in ["Hel", "See [a](b.md) and", "```python\nx\n```"] {
        let (_root, mut app, ctx) = document("README.md", source);
        frame(&mut app, &ctx, 0.04, vec![ctrl_space()]);
        assert!(
            !app.completion.visible,
            "{source:?}: {:?}",
            lookup_labels(&app)
        );
    }
}

/// Types `text` one character per frame, letting the real auto-popup timer
/// run between characters as it does at human typing speed.
fn type_slowly(app: &mut IdeApp, ctx: &egui::Context, time: &mut f64, text: &str) {
    for character in text.chars() {
        frame(
            app,
            ctx,
            *time,
            vec![egui::Event::Text(character.to_string())],
        );
        *time += 0.04;
        frame(app, ctx, *time, vec![]);
        *time += 0.04;
    }
}

fn key_event(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn template_space_after_open_delimiter_never_commits_an_empty_prefix_lookup() {
    for (name, source, typed) in [
        ("page.jinja2", "<p>", "{% url"),
        ("page.jinja2", "<p>", "{{ but"),
        ("page.jinja2", "<p>", "{%- if x"),
    ] {
        let (_root, mut app, ctx) = document(name, source);
        let mut time = 0.04;
        // The lookup may pop up right after `{%`/`{{` with an empty prefix.
        type_slowly(&mut app, &ctx, &mut time, &typed[..2]);
        assert!(
            app.completion.visible && app.completion.prefix_start == app.completion.caret,
            "{name}: the empty-prefix lookup after {:?} is what the space must not commit",
            &typed[..2]
        );
        type_slowly(&mut app, &ctx, &mut time, &typed[2..]);
        assert_eq!(
            app.tabs[app.active.unwrap()].state.doc.text(),
            format!("{source}{typed}"),
            "{name}: a space typed into an unnavigated empty-prefix lookup is text"
        );
    }
}

#[test]
fn python_context_characters_follow_the_insert_by_context_characters_setting() {
    // Setting on (default): with a typed prefix, the preselected item of the
    // auto-popup commits on space even when several candidates match.
    let source = "xa = 1\nxb = 2\n";
    let (_root, mut app, ctx) = document("spaces.py", source);
    let mut time = 0.04;
    type_slowly(&mut app, &ctx, &mut time, "x");
    assert!(app.completion.visible && !app.completion.explicit);
    let chosen = app.completion.items[app.completion.selected].insert.clone();
    type_slowly(&mut app, &ctx, &mut time, " ");
    let index = app.active.unwrap();
    assert_eq!(
        app.tabs[index].state.doc.text(),
        format!("{source}{chosen} ")
    );
    // Setting off: context characters never commit.
    let (_root, mut app, ctx) = document("spaces.py", source);
    app.settings.completion.insert_by_context_characters = false;
    let mut time = 0.04;
    type_slowly(&mut app, &ctx, &mut time, "x");
    assert!(app.completion.visible);
    type_slowly(&mut app, &ctx, &mut time, " = 1 ");
    assert_eq!(
        app.tabs[app.active.unwrap()].state.doc.text(),
        format!("{source}x = 1 ")
    );
}

#[test]
fn python_empty_prefix_member_lookup_commits_only_a_navigated_item() {
    let source = "class Box:\n    alpha = 1\n    beta = 2\n\n\nBox";
    for (navigate, dotted) in [(false, false), (true, true)] {
        let (_root, mut app, ctx) = document("members.py", source);
        let mut time = 0.04;
        type_slowly(&mut app, &ctx, &mut time, ".");
        assert!(
            app.completion.visible && app.completion.prefix_start == app.completion.caret,
            "member lookup with an empty prefix after `.`"
        );
        if navigate {
            frame(&mut app, &ctx, time, vec![key_event(egui::Key::ArrowDown)]);
            time += 0.04;
        }
        let chosen = app.completion.items[app.completion.selected].insert.clone();
        frame(&mut app, &ctx, time, vec![egui::Event::Text(" ".into())]);
        let expected = if dotted {
            format!("{source}.{chosen} ")
        } else {
            format!("{source}. ")
        };
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), expected);
    }
}

#[test]
fn python_dot_commits_the_preselected_item_of_a_multi_match_prefix() {
    let source = "class Box:\n    path = 1\n    pardir = 2\n\n\nBox.pa";
    let (_root, mut app, ctx) = document("dots.py", source);
    app.open_completion(false);
    assert!(app.completion.visible && app.completion.items.len() >= 2);
    let chosen = app.completion.items[app.completion.selected].insert.clone();
    frame(&mut app, &ctx, 0.04, vec![egui::Event::Text(".".into())]);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.doc.text(),
        format!("{}{chosen}.", &source[..source.len() - 2])
    );
}

#[test]
fn explicit_lookup_with_typed_prefix_still_commits_on_context_character() {
    let (_root, mut app, ctx) = document("explicit.py", "xa = 1\nxb = 2\nx");
    app.open_completion(true);
    assert!(app.completion.visible && app.completion.explicit);
    let chosen = app.completion.items[app.completion.selected].insert.clone();
    frame(&mut app, &ctx, 0.04, vec![egui::Event::Text(" ".into())]);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.doc.text(),
        format!("xa = 1\nxb = 2\n{chosen} ")
    );
}

/// Types each chunk within ONE frame (fast typing coalesces several
/// characters per frame), then lets the real auto-popup and the completion
/// worker settle before the next chunk.
fn type_fast(app: &mut IdeApp, ctx: &egui::Context, time: &mut f64, chunks: &[&str]) {
    for chunk in chunks {
        let events = chunk
            .chars()
            .map(|character| egui::Event::Text(character.to_string()))
            .collect();
        frame(app, ctx, *time, events);
        *time += 0.04;
        for _ in 0..500 {
            frame(app, ctx, *time, vec![]);
            *time += 0.04;
            if !app.completion_loading() && !app.completion_worker.pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[test]
fn fast_python_typing_never_context_commits_a_fuzzy_non_prefix_match() {
    let source = "import os\n";
    let (_root, mut app, ctx) = document("fast.py", source);
    let mut time = 0.04;
    type_fast(&mut app, &ctx, &mut time, &["x"]);
    // The auto-popup may offer fuzzy matches (`anext` for `x`), but none of
    // them begins with the typed prefix, so the space is plain text.
    assert!(app
        .completion
        .items
        .iter()
        .all(|item| !item.label.starts_with('x')));
    type_fast(&mut app, &ctx, &mut time, &[" = 1"]);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.doc.text(),
        format!("{source}x = 1")
    );
}

#[test]
fn fast_python_typing_commits_only_real_prefix_matches_and_prefers_the_submodule() {
    let source = "import os\n";
    let (_root, mut app, ctx) = document("fast.py", source);
    let mut time = 0.04;
    // `ValueError` matches `value` only case-insensitively: the space after
    // the new name `value` must not turn it into `ValueError`.
    type_fast(&mut app, &ctx, &mut time, &["va", "lue"]);
    type_fast(&mut app, &ctx, &mut time, &[" = o", "s", ".", "pa"]);
    assert!(app.completion.visible, "member lookup for `os.pa`");
    assert_eq!(
        app.completion.items[app.completion.selected].label.as_str(),
        "path",
        "`path` ranks before `pardir`: {:?}",
        lookup_labels(&app)
    );
    type_fast(&mut app, &ctx, &mut time, &["."]);
    assert_eq!(
        app.tabs[app.active.unwrap()].state.doc.text(),
        format!("{source}value = os.path.")
    );
}

#[test]
fn a_long_lookup_lays_out_only_the_rows_in_view() {
    let source: String = (0..3000)
        .map(|i| format!("value_{i:04} = {i}\n"))
        .chain(std::iter::once("result = value_".to_string()))
        .collect();
    let (_root, mut app, ctx) = document("long_lookup.py", &source);
    frame(&mut app, &ctx, 0.04, vec![ctrl_space()]);
    frame(&mut app, &ctx, 0.08, vec![]);
    frame(&mut app, &ctx, 0.5, vec![]);
    assert!(app.completion.visible);
    assert!(
        app.completion.items.len() >= 3000,
        "{}",
        app.completion.items.len()
    );
    let output = window_frame(&mut app, &ctx, 0.6, vec![], true);
    let painted = output
        .shapes
        .iter()
        .filter(|clipped| {
            matches!(&clipped.shape, egui::Shape::Text(text)
                if text.galley.text().starts_with("value_") && text.galley.text().len() == 10
                    && clipped.clip_rect.max.y > 0.0)
        })
        .count();
    output.drop_without_applying_deltas();
    // The lookup shows ten rows; the editor paints a screenful of its lines.
    assert!(painted < 120, "{painted} lookup rows laid out");
}
