//! `.ipynb` model and `.py` conversion tests (python3 is only used, when present, to cross-check
//! the writer against Python's `json` module).

use std::path::Path;
use std::str::FromStr;

use rustpy_notebook::{new_notebook, Cell, CellKind, MimeBundle, Notebook, Output};
use serde_json::{json, Value};

/// Written by a script reproducing nbformat's writer (`json.dumps(sort_keys=True, indent=1,
/// ensure_ascii=False)` after `split_lines`), with unknown fields at every level.
const SAMPLE: &str = include_str!("data/sample.ipynb");

fn text(output: &Output) -> String {
    output.plain_text().unwrap_or_default()
}

#[test]
fn sample_round_trips_byte_for_byte() {
    let nb = Notebook::from_str(SAMPLE).unwrap();
    assert_eq!(nb.to_string(), SAMPLE);
    // Parsing the output again gives the same model.
    assert_eq!(nb.to_string().parse::<Notebook>().unwrap(), nb);
}

/// Random notebooks written by Python exactly as nbformat writes them (arbitrary doubles, 64-bit
/// integers, control and non-BMP characters, unknown fields; see `data/random_notebooks.py`)
/// are reproduced byte for byte.
#[test]
fn python_written_notebooks_round_trip_byte_for_byte() {
    let Ok(python) = which::which("python3") else {
        eprintln!("python3 not found; skipping");
        return;
    };
    const COUNT: usize = 60;
    let dir = tempfile::tempdir().unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/random_notebooks.py");
    let status = std::process::Command::new(python)
        .arg(script)
        .arg(dir.path())
        .args([COUNT.to_string().as_str(), "20261009"])
        .status()
        .unwrap();
    assert!(status.success(), "the generator failed");
    for i in 0..COUNT {
        let path = dir.path().join(format!("nb{i}.ipynb"));
        let written = std::fs::read_to_string(&path).unwrap();
        let nb = Notebook::load(&path).unwrap_or_else(|e| panic!("nb{i}: {e:#}"));
        let again = nb.to_string();
        if let Some((line, (python, rust))) = written
            .lines()
            .zip(again.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
        {
            panic!(
                "nb{i} line {}: Python wrote\n{python:?}\nbut the model wrote\n{rust:?}",
                line + 1
            );
        }
        assert_eq!(again, written, "nb{i}");
        assert_eq!(again.parse::<Notebook>().unwrap(), nb);
    }
}

#[test]
fn sample_model_contents() {
    let nb: Notebook = SAMPLE.parse().unwrap();
    assert_eq!((nb.nbformat, nb.nbformat_minor), (4, 5));
    assert_eq!(nb.metadata["kernelspec"]["name"], "python3");
    assert_eq!(
        nb.metadata["custom_tool"]["alpha"]["b"],
        "é ü 中文 \u{2028}"
    );
    assert_eq!(nb.metadata["jupytext"]["formats"], "ipynb,py:percent");
    assert_eq!(
        nb.extra["x_custom_top_level"],
        json!({ "keep": ["me", 1, true, null] })
    );
    assert_eq!(nb.cells.len(), 7);

    let md = &nb.cells[0];
    assert_eq!(
        (md.kind, md.id.as_deref()),
        (CellKind::Markdown, Some("a1b2c3d4"))
    );
    assert_eq!(
        md.source,
        "# Title\n\nSome *text* — ünïcödé ✓\n![img](attachment:img.png)"
    );
    assert_eq!(md.metadata["tags"], json!(["intro"]));
    assert!(md.extra["attachments"]["img.png"]["image/png"].is_string());
    assert!(md.outputs.is_empty() && md.execution_count.is_none());

    let code = &nb.cells[1];
    assert_eq!((code.kind, code.execution_count), (CellKind::Code, Some(1)));
    assert_eq!(
        code.source,
        "import sys\nprint('hello')\nprint('oops', file=sys.stderr)\n41 + 1"
    );
    assert_eq!(code.metadata["custom"], json!({ "k": "v" }));
    assert_eq!(code.outputs[0], Output::stream("stdout", "hello\n"));
    assert_eq!(code.outputs[1], Output::stream("stderr", "oops\n"));
    assert_eq!(
        code.outputs[2],
        Output::execute_result(
            Some(1),
            MimeBundle::from([("text/plain".to_owned(), "42".to_owned())])
        )
    );

    let rich = &nb.cells[2].outputs;
    let Output::DisplayData { data, metadata, .. } = &rich[0] else {
        panic!("expected display data, got {:?}", rich[0])
    };
    assert!(data["image/png"].starts_with("iVBORw0KGgo"));
    assert_eq!(data["image/svg+xml"], "<svg>\n<rect/>\n</svg>");
    assert_eq!(
        serde_json::from_str::<Value>(&data["application/json"]).unwrap(),
        json!({ "a": [1, 2, 3], "b": { "c": null } })
    );
    assert_eq!(
        serde_json::from_str::<Value>(&data["application/vnd.custom.v1+json"]).unwrap(),
        json!({ "x": 1 })
    );
    assert_eq!(metadata["image/png"]["width"], 640);
    assert_eq!(
        rich[1].data().unwrap()["text/html"],
        "<table>\n<tr><td>1</td></tr>\n</table>"
    );
    assert_eq!(text(&rich[1]), "   a\n0  1");

    let Output::Error {
        ename,
        evalue,
        traceback,
        ..
    } = &nb.cells[3].outputs[0]
    else {
        panic!()
    };
    assert_eq!(
        (ename.as_str(), evalue.as_str(), traceback.len()),
        ("ZeroDivisionError", "division by zero", 4)
    );
    assert!(traceback[0].starts_with("\u{1b}[0;31m"));

    let raw = &nb.cells[4];
    assert_eq!(
        (raw.kind, raw.source.as_str()),
        (CellKind::Raw, "raw *text*\n\tindented with a tab")
    );
    assert_eq!(raw.metadata["format"], "text/restructuredtext");

    let odd = &nb.cells[5];
    assert_eq!((odd.source.as_str(), odd.execution_count), ("", None));
    assert_eq!(odd.extra["x_cell_extra"], json!({ "keep": true }));
    assert_eq!(odd.outputs.len(), 1);
    assert_eq!(odd.outputs[0].output_type(), "x_custom_output");
    assert!(matches!(&odd.outputs[0], Output::Unknown(obj) if obj["payload"] == json!([1, 2])));

    // Stored stream text is not rewritten on load (only new output gets `\r` processing).
    assert_eq!(text(&nb.cells[6].outputs[0]), "10%\r100%\n\u{c}page\n");
}

#[test]
fn edits_keep_unknown_fields() {
    let mut nb: Notebook = SAMPLE.parse().unwrap();
    nb.cells[1].source = "print('changed')".into();
    nb.cells[1].clear_outputs();
    nb.insert_cell(2, Cell::markdown("new *cell*"));
    nb.delete_cell(5); // the raw cell, now at index 5
    nb.metadata.insert("added".into(), json!(true));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edited.ipynb");
    nb.save(&path).unwrap();
    let back = Notebook::load(&path).unwrap();
    assert_eq!(back, nb);
    assert_eq!(back.extra["x_custom_top_level"]["keep"][0], "me");
    assert_eq!(
        back.metadata["custom_tool"]["zeta"],
        json!([1, 2, { "a": null, "z": true }])
    );
    assert_eq!(back.metadata["added"], true);
    assert!(back.cells[0].extra.contains_key("attachments"));
    assert_eq!(back.cells[1].source, "print('changed')");
    assert_eq!(
        (back.cells[1].outputs.len(), back.cells[1].execution_count),
        (0, None)
    );
    assert_eq!(back.cells[2].kind, CellKind::Markdown);
    assert_eq!(
        back.cells
            .iter()
            .filter(|c| c.kind == CellKind::Raw)
            .count(),
        0
    );
    assert_eq!(back.cells[5].extra["x_cell_extra"]["keep"], true);

    // The file on disk is in Jupyter's layout.
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.starts_with("{\n \"cells\": [\n  {\n"),
        "{written:.80}"
    );
    assert!(written.contains("   \"source\": [\n    \"print('changed')\"\n   ]"));
    assert!(written.ends_with("}\n"));
}

#[test]
fn older_notebooks_and_single_string_fields() {
    let text = r##"{"nbformat": 4, "nbformat_minor": 2, "metadata": {"kernelspec": {"name": "python3"}},
        "cells": [
          {"cell_type": "code", "execution_count": 7, "metadata": {}, "source": "a = 1\nb = 2\n",
           "outputs": [{"output_type": "stream", "name": "stdout", "text": "x\ny\n"},
                       {"output_type": "execute_result", "execution_count": 7, "metadata": {}, "data": {"text/plain": "3", "image/png": ["aGVs", "bG8="]}}]},
          {"cell_type": "markdown", "metadata": {}, "source": ["# Head", "ing\n", "text"]}
        ]}"##;
    let mut nb: Notebook = text.parse().unwrap();
    assert!(!nb.uses_cell_ids());
    assert_eq!(nb.cells[0].id, None);
    assert_eq!(nb.cells[0].source, "a = 1\nb = 2\n");
    assert_eq!(nb.cells[1].source, "# Heading\ntext");
    assert_eq!(
        nb.cells[0].outputs[1].data().unwrap()["image/png"],
        "aGVsbG8="
    );

    // Written in list-of-lines form, without inventing ids for a 4.2 notebook.
    let value: Value = serde_json::from_str(&nb.to_string()).unwrap();
    assert_eq!(value["cells"][0]["source"], json!(["a = 1\n", "b = 2\n"]));
    assert_eq!(
        value["cells"][0]["outputs"][0]["text"],
        json!(["x\n", "y\n"])
    );
    assert_eq!(
        value["cells"][0]["outputs"][1]["data"],
        json!({ "image/png": "aGVsbG8=", "text/plain": ["3"] })
    );
    assert_eq!(value["cells"][1]["source"], json!(["# Heading\n", "text"]));
    assert!(value["cells"][0].get("id").is_none() && value["nbformat_minor"] == 2);
    nb.insert_cell(1, Cell::code("c = 3"));
    assert_eq!(nb.cells[1].id, None);

    // Upgrading assigns unique ids.
    assert!(nb.ensure_cell_ids());
    assert!(nb.uses_cell_ids() && nb.nbformat_minor == 5);
    let ids: std::collections::HashSet<_> =
        nb.cells.iter().map(|c| c.id.clone().unwrap()).collect();
    assert_eq!(ids.len(), 3);
    assert!(!nb.ensure_cell_ids());

    // Duplicated and empty ids are repaired, the first owner keeps its id.
    nb.cells[1].id = nb.cells[0].id.clone();
    nb.cells[2].id = Some(String::new());
    let first = nb.cells[0].id.clone();
    assert!(nb.ensure_cell_ids());
    assert_eq!(nb.cells[0].id, first);
    assert_ne!(nb.cells[1].id, first);
    assert!(!nb.cells[2].id.as_deref().unwrap().is_empty());

    // A byte-order mark is accepted.
    assert!(format!("\u{feff}{text}").parse::<Notebook>().is_ok());
}

#[test]
fn invalid_notebooks_are_rejected_with_context() {
    let err = |text: &str| format!("{:#}", text.parse::<Notebook>().unwrap_err());
    assert!(err("not json").contains("not valid JSON"));
    assert!(err("[]").contains("JSON object"));
    assert!(err(r#"{"cells": []}"#).contains("nbformat"));
    assert!(err(r#"{"nbformat": 3, "nbformat_minor": 0, "worksheets": []}"#).contains("version 3"));
    let bad_cell = r#"{"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [{"cell_type": "code", "source": "", "metadata": {}, "outputs": []}, {"cell_type": "heading", "source": "", "metadata": {}}]}"#;
    let message = err(bad_cell);
    assert!(
        message.contains("cell #2") && message.contains("heading"),
        "{message}"
    );
    let bad_count = r#"{"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [{"cell_type": "code", "source": "", "metadata": {}, "outputs": [], "execution_count": "1"}]}"#;
    assert!(err(bad_count).contains("execution_count"));
    let bad_output = r#"{"nbformat": 4, "nbformat_minor": 5, "metadata": {}, "cells": [{"cell_type": "code", "source": "", "metadata": {}, "outputs": [{"name": "stdout"}]}]}"#;
    assert!(err(bad_output).contains("output #1"));

    let dir = tempfile::tempdir().unwrap();
    assert!(Notebook::load(dir.path().join("missing.ipynb")).is_err());
}

#[test]
fn save_replaces_the_file_in_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nb.ipynb");
    let mut nb = new_notebook();
    nb.save(&path).unwrap();
    nb.cells[0].source = "changed".into();
    nb.save(&path).unwrap();
    assert_eq!(Notebook::load(&path).unwrap(), nb);
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["nb.ipynb"], "no temporary file is left behind");

    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("link.ipynb");
        symlink(&path, &link).unwrap();
        nb.cells[0].source = "saved through a link".into();
        nb.save(&link).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link stays a link"
        );
        assert_eq!(
            Notebook::load(&path).unwrap().cells[0].source,
            "saved through a link"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );

        // A read-only notebook is not replaced (unless this process may write it anyway, as root can).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o440)).unwrap();
        if std::fs::OpenOptions::new().write(true).open(&path).is_err() {
            nb.cells[0].source = "must not be saved".into();
            assert!(nb.save(&path).is_err());
            assert_eq!(
                Notebook::load(&path).unwrap().cells[0].source,
                "saved through a link"
            );
        }
    }

    assert!(nb.save(dir.path().join("missing-dir/nb.ipynb")).is_err());
    assert!(nb.save(dir.path()).is_err(), "a directory is not replaced");
    assert!(dir.path().join("nb.ipynb").is_file());
}

#[test]
fn new_notebook_defaults() {
    let nb = new_notebook();
    assert_eq!((nb.nbformat, nb.nbformat_minor), (4, 5));
    assert_eq!(
        nb.metadata["kernelspec"],
        json!({ "display_name": "Python 3 (ipykernel)", "language": "python", "name": "python3" })
    );
    assert_eq!(nb.metadata["language_info"]["name"], "python");
    assert_eq!(nb.cells.len(), 1);
    let cell = &nb.cells[0];
    assert_eq!((cell.kind, cell.source.as_str()), (CellKind::Code, ""));
    let id = cell.id.as_deref().unwrap();
    assert!(id.len() == 8 && id.chars().all(|c| c.is_ascii_hexdigit()));

    let text = nb.to_string();
    assert_eq!(text.parse::<Notebook>().unwrap(), nb);
    assert!(text.contains("   \"execution_count\": null,\n   \"id\": \""));
    assert!(text.contains("   \"outputs\": [],\n   \"source\": []\n"));
    assert!(Notebook::empty().cells.is_empty());
}

#[test]
fn insert_delete_move_cells() {
    let mut nb = Notebook::empty();
    assert_eq!(nb.insert_cell(0, Cell::code("b")), 0);
    assert_eq!(nb.insert_cell(0, Cell::markdown("a")), 0);
    assert_eq!(nb.insert_cell(99, Cell::raw("c")), 2); // clamped to the end
    let sources = |nb: &Notebook| {
        nb.cells
            .iter()
            .map(|c| c.source.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(sources(&nb), ["a", "b", "c"]);

    // Every inserted cell got a distinct id; a copy of an existing cell gets a new one.
    let copy = nb.cells[1].clone();
    let original_id = copy.id.clone();
    nb.insert_cell(2, copy);
    assert_ne!(nb.cells[2].id, original_id);
    let mut keep = Cell::code("own id");
    keep.id = Some("my-id".into());
    nb.insert_cell(0, keep);
    assert_eq!(nb.cells[0].id.as_deref(), Some("my-id"));
    assert_eq!(nb.cell_index("my-id"), Some(0));
    let ids: std::collections::HashSet<_> =
        nb.cells.iter().map(|c| c.id.clone().unwrap()).collect();
    assert_eq!(ids.len(), nb.cells.len());

    assert_eq!(sources(&nb), ["own id", "a", "b", "b", "c"]);
    assert!(nb.move_cell(0, 4));
    assert_eq!(sources(&nb), ["a", "b", "b", "c", "own id"]);
    assert!(nb.move_cell(3, 0));
    assert_eq!(sources(&nb), ["c", "a", "b", "b", "own id"]);
    assert!(nb.move_cell(1, 1));
    assert!(!nb.move_cell(5, 0) && !nb.move_cell(0, 5));

    assert_eq!(nb.delete_cell(4).unwrap().source, "own id");
    assert!(nb.delete_cell(4).is_none());
    assert_eq!(sources(&nb), ["c", "a", "b", "b"]);
    assert_eq!(nb.cell_index("my-id"), None);

    nb.cells[2].outputs.push(Output::stream("stdout", "x"));
    nb.cells[2].execution_count = Some(3);
    nb.clear_outputs();
    assert!(nb
        .cells
        .iter()
        .all(|c| c.outputs.is_empty() && c.execution_count.is_none()));
}

#[test]
fn py_export_and_import_round_trip() {
    let mut nb = Notebook::empty();
    nb.insert_cell(0, Cell::markdown("# Title\n\nSome *text*\n#tag line"));
    nb.insert_cell(1, Cell::code("import os\n\n\ndef f():\n    return 1\nf()"));
    let mut titled = Cell::code("x = 1");
    titled.metadata.insert("title".into(), json!("Load data"));
    nb.insert_cell(2, titled);
    nb.insert_cell(3, Cell::raw("raw text\n  indented"));
    nb.insert_cell(4, Cell::code(""));
    nb.insert_cell(5, Cell::code("print('end')\n"));

    let script = nb.to_py();
    assert_eq!(
        script,
        "# %% [markdown]\n# # Title\n#\n# Some *text*\n# #tag line\n\n\
         # %%\nimport os\n\n\ndef f():\n    return 1\nf()\n\n\
         # %% Load data\nx = 1\n\n\
         # %% [raw]\n# raw text\n#   indented\n\n\
         # %%\n\n\
         # %%\nprint('end')\n"
    );

    let back = Notebook::from_py(&script);
    let summary = |nb: &Notebook| {
        nb.cells
            .iter()
            .map(|c| {
                (
                    c.kind,
                    c.source.trim_end().to_owned(),
                    c.metadata.get("title").cloned(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(summary(&back), summary(&nb));
    assert!(back.cells.iter().all(|c| c.id.is_some()));
    assert_eq!(back.metadata["kernelspec"]["name"], "python3");
    // Exporting the imported notebook gives the same script.
    assert_eq!(back.to_py(), script);
}

#[test]
fn py_import_variants() {
    let script = "#!/usr/bin/env python\n\"\"\"Module docstring.\"\"\"\n\n#%%\r\na = 1\r\n# %%time is a commented magic, not a marker\r\n\r\n# %% [md]\r\n# Notes\r\n#\r\n# more\r\n#    %%   Spaced   title\n\n\nb = 2\n\n\n";
    let nb = Notebook::from_py(script);
    let cells: Vec<(CellKind, &str)> = nb
        .cells
        .iter()
        .map(|c| (c.kind, c.source.as_str()))
        .collect();
    assert_eq!(
        cells,
        [
            (
                CellKind::Code,
                "#!/usr/bin/env python\n\"\"\"Module docstring.\"\"\""
            ),
            (
                CellKind::Code,
                "a = 1\n# %%time is a commented magic, not a marker"
            ),
            (CellKind::Markdown, "Notes\n\nmore"),
            (CellKind::Code, "b = 2"),
        ]
    );
    assert_eq!(nb.cells[3].metadata["title"], "Spaced   title");

    let plain = Notebook::from_py("print(1)\nprint(2)\n");
    assert_eq!(plain.cells.len(), 1);
    assert_eq!(plain.cells[0].source, "print(1)\nprint(2)");
    assert!(Notebook::from_py("").cells.is_empty());
    assert!(Notebook::from_py("\n\n").cells.is_empty());

    // A byte-order mark does not hide the first marker.
    let bom = Notebook::from_py("\u{feff}# %% [markdown]\n# Hi\n# %%\nx = 1\n");
    assert_eq!(
        bom.cells
            .iter()
            .map(|c| (c.kind, c.source.as_str()))
            .collect::<Vec<_>>(),
        [(CellKind::Markdown, "Hi"), (CellKind::Code, "x = 1")]
    );
}
