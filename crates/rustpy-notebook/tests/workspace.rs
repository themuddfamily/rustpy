//! Notebook workspace contracts; pure fixtures never start a process, kernel, or server.
use rustpy_notebook::{new_notebook, virtual_file::VirtualFile, Cell, Connection, Output};
#[test]
fn utf8_cell_boundaries_preserve_cross_cell_python_offsets_and_magic_lines() {
    let mut notebook = new_notebook();
    notebook.cells = vec![
        Cell::code("café = '🌍'\n%matplotlib inline\n"),
        Cell::markdown("# Heading\n## Detail"),
        Cell::code("print(café)\n"),
    ];
    let file = VirtualFile::new(&notebook);
    let offset = file.offset(2, 6).unwrap();
    assert_eq!(file.locate(offset), Some((2, 6)));
    assert_eq!(&file.text[offset..offset + 5], "café");
    assert_eq!(
        file.boundaries[0].end - file.boundaries[0].start,
        notebook.cells[0].source.len()
    );
    assert!(!file.text.contains("%matplotlib"));
    assert_eq!(
        VirtualFile::headings(&notebook),
        vec![(1, 1, "Heading".into()), (1, 2, "Detail".into())]
    );
}
#[test]
fn semantic_edits_are_atomic_and_reject_stale_or_cross_cell_ranges() {
    let mut notebook = new_notebook();
    notebook.cells = vec![Cell::code("name=1"), Cell::code("print(name)")];
    let file = VirtualFile::new(&notebook);
    let start = file.offset(0, 0).unwrap();
    let second = file.offset(1, 6).unwrap();
    file.apply_edits(
        &mut notebook,
        &[
            (start..start + 4, "value".into()),
            (second..second + 4, "value".into()),
        ],
    )
    .unwrap();
    assert_eq!(notebook.cells[0].source, "value=1");
    assert_eq!(notebook.cells[1].source, "print(value)");
    let captured = notebook.clone();
    assert!(file
        .apply_edits(&mut notebook, &[(start..start + 4, "other".into())])
        .is_err());
    assert_eq!(notebook, captured);
    let current = VirtualFile::new(&notebook);
    let original = notebook.clone();
    assert!(current
        .apply_edits(
            &mut notebook,
            &[(
                current.boundaries[0].start..current.boundaries[1].end,
                "bad".into()
            )]
        )
        .is_err());
    assert_eq!(notebook, original);
}
#[test]
fn remote_connection_normalization_captures_token_without_leaving_it_in_server_url() {
    let connection = Connection {
        kind: "remote".into(),
        url: "https://example.invalid/user/alice/lab?token=private%20token#section".into(),
        ..Default::default()
    }
    .normalized()
    .unwrap();
    assert!(connection.verify_ssl);
    assert_eq!(connection.url, "https://example.invalid/user/alice/");
    assert_eq!(connection.token, "private token");
    assert!(Connection {
        kind: "remote".into(),
        url: "https://user:secret@example.invalid".into(),
        ..Default::default()
    }
    .normalized()
    .is_err());
    assert!(Connection {
        kind: "ssh".into(),
        host: "-oProxyCommand=bad".into(),
        ..Default::default()
    }
    .validate()
    .is_err());
}
#[test]
fn untrusted_export_never_publishes_active_html() {
    let mut notebook = new_notebook();
    let mut bundle = rustpy_notebook::MimeBundle::new();
    bundle.insert(
        "text/html".into(),
        "<script>window.untrusted=true</script>".into(),
    );
    bundle.insert("text/plain".into(), "safe fallback".into());
    notebook.cells[0].outputs.push(Output::display_data(bundle));
    let untrusted = rustpy_notebook::tasks::export_html(&notebook, false);
    assert!(!untrusted.contains("<script>"));
    assert!(untrusted.contains("safe fallback"));
    let trusted = rustpy_notebook::tasks::export_html(&notebook, true);
    assert!(trusted.contains("<script>window.untrusted"));
}
#[test]
fn percent_conversion_preserves_cell_types_and_headings() {
    let source = "# %% [markdown]\n# # Analysis\n# Equation $x^2$\n\n# %%\nvalue = 42\n";
    let notebook = rustpy_notebook::Notebook::from_py(source);
    assert!(notebook.cells.iter().any(|cell| cell.is_code()));
    assert_eq!(VirtualFile::headings(&notebook)[0].2, "Analysis");
    assert!(notebook.to_py().contains("value = 42"));
}
