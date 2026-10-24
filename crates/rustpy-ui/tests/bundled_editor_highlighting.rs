use rustpy_text::Document;
use rustpy_ui::editor::{EditorState, Language};
use std::path::Path;

#[test]
fn bundled_language_paths_reach_actual_editor_highlighting() {
    for (path, language, source) in [
        (".gitignore", Language::GitIgnore, "# note\n*.pyc\n"),
        (
            ".dockerignore",
            Language::DockerIgnore,
            "# note\n**/*.pyc\n!keep.pyc\n",
        ),
        // Docker trims before the `!` exception marker; plain `.gitignore`
        // colouring would leave this line uncoloured.
        (
            "Dockerfile.dockerignore",
            Language::DockerIgnore,
            "  !keep.pyc\n",
        ),
        (".env.local", Language::DotEnv, "VALUE='text'\n"),
        ("Makefile", Language::Makefile, "all: input\n\techo ready\n"),
        ("pattern.regex", Language::Regex, "[a-z]+"),
        ("change.patch", Language::Diff, "@@ -1 +1 @@\n-old\n+new\n"),
        ("table.csv", Language::Csv, "name,42\n"),
        ("app.properties", Language::Properties, "name=value\n"),
    ] {
        assert_eq!(Language::from_path(Path::new(path)), language);
        let mut editor = EditorState::new(Document::new(source), Some(path.into()));
        assert!(!editor.highlight_spans().is_empty(), "{path}");
    }
    let mut plain = EditorState::new(Document::new("# untouched"), Some("readme.txt".into()));
    assert!(plain.highlight_spans().is_empty());
}
