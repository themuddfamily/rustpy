//! The `.ipynb` document model (nbformat 4).
//!
//! Parsing keeps everything the model does not interpret: notebook/cell/output metadata are
//! stored verbatim, unknown keys land in the `extra` maps and outputs of an unknown
//! `output_type` become [`Output::Unknown`]. Serialization follows Jupyter's writer
//! (`json.dumps(sort_keys=True, indent=1, ensure_ascii=False)` plus a final newline, multi-line
//! strings split into lists of lines, floats written like Python's `repr`), and numbers are
//! parsed exactly (serde_json's `float_roundtrip`), so loading and saving an unmodified notebook
//! written by Jupyter reproduces the file byte for byte. (Integers beyond the 64-bit range are
//! the exception: serde_json reads them as floats.)

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Context};
use serde::Serialize;
use serde_json::ser::{Formatter, PrettyFormatter};
use serde_json::{Map, Value};

/// A JSON object: metadata, or fields the model does not interpret.
pub type JsonMap = Map<String, Value>;

/// Mime type → content of one output representation. Text formats hold the text, binary formats
/// (`image/png`, …) their base64 encoding and JSON formats (`application/json`,
/// `application/…+json`) the serialized JSON document.
pub type MimeBundle = BTreeMap<String, String>;

/// The nbformat major version read and written by this crate.
pub const NBFORMAT: u32 = 4;
/// The nbformat minor version of new notebooks (4.5 introduced cell ids).
pub const NBFORMAT_MINOR: u32 = 5;

/// A Jupyter notebook.
#[derive(Clone, Debug, PartialEq)]
pub struct Notebook {
    pub cells: Vec<Cell>,
    /// Notebook metadata (`kernelspec`, `language_info`, tool-specific keys …), kept verbatim.
    pub metadata: JsonMap,
    pub nbformat: u32,
    pub nbformat_minor: u32,
    /// Top-level keys nbformat does not define, written back unchanged.
    pub extra: JsonMap,
}

/// The type of a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellKind {
    Code,
    Markdown,
    Raw,
}

/// One notebook cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// The nbformat 4.5 cell id (`None` in older notebooks).
    pub id: Option<String>,
    pub kind: CellKind,
    pub source: String,
    /// Outputs of a code cell (always empty for other kinds).
    pub outputs: Vec<Output>,
    /// Execution count of a code cell (`None` = not executed).
    pub execution_count: Option<u32>,
    pub metadata: JsonMap,
    /// Keys the model does not interpret (e.g. `attachments` of markdown cells), written back unchanged.
    pub extra: JsonMap,
}

/// One output of a code cell.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Text written to `stdout` or `stderr` (`name`).
    Stream {
        name: String,
        text: String,
        extra: JsonMap,
    },
    /// Rich data shown with `display()`.
    DisplayData {
        data: MimeBundle,
        metadata: JsonMap,
        extra: JsonMap,
    },
    /// The value of the cell's trailing expression.
    ExecuteResult {
        execution_count: Option<u32>,
        data: MimeBundle,
        metadata: JsonMap,
        extra: JsonMap,
    },
    /// An exception raised by the cell; `traceback` holds the formatted lines.
    Error {
        ename: String,
        evalue: String,
        traceback: Vec<String>,
        extra: JsonMap,
    },
    /// An output whose `output_type` nbformat 4 does not define, kept verbatim.
    Unknown(JsonMap),
}

/// A new notebook (nbformat 4.5, Python 3 kernelspec) with one empty code cell.
pub fn new_notebook() -> Notebook {
    let mut nb = Notebook::empty();
    nb.insert_cell(0, Cell::code(""));
    nb
}

impl Notebook {
    /// A notebook without cells (nbformat 4.5, Python 3 kernelspec).
    pub fn empty() -> Notebook {
        let metadata = serde_json::json!({
            "kernelspec": { "display_name": "Python 3 (ipykernel)", "language": "python", "name": "python3" },
            "language_info": { "name": "python" },
        });
        Notebook {
            cells: Vec::new(),
            metadata: metadata.as_object().cloned().unwrap_or_default(),
            nbformat: NBFORMAT,
            nbformat_minor: NBFORMAT_MINOR,
            extra: JsonMap::new(),
        }
    }

    /// Reads an `.ipynb` file.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Notebook> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        text.parse()
            .with_context(|| format!("{} is not a valid notebook", path.display()))
    }

    /// Writes the notebook as Jupyter would (see [`fmt::Display`]). Like Jupyter, the file is
    /// replaced through a temporary file, so a failed save (full disk, crash) never leaves a
    /// truncated notebook behind; symlinks are followed and the file keeps its permissions.
    pub fn save(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        let text = self.render().context("cannot serialize the notebook")?;
        write_file(path, text.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Builds a notebook from parsed JSON (see the module docs for what is preserved).
    pub fn from_json(value: Value) -> anyhow::Result<Notebook> {
        let Value::Object(mut obj) = value else {
            bail!("a notebook must be a JSON object")
        };
        let nbformat = take_count(&mut obj, "nbformat")?
            .ok_or_else(|| anyhow!("missing `nbformat` (not a Jupyter notebook?)"))?;
        if nbformat != NBFORMAT {
            bail!("unsupported notebook format version {nbformat} (only nbformat 4 is supported)");
        }
        let nbformat_minor = take_count(&mut obj, "nbformat_minor")?.unwrap_or(0);
        let metadata = take_object(&mut obj, "metadata")?.unwrap_or_default();
        let cells = match obj.remove("cells") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .into_iter()
                .enumerate()
                .map(|(i, cell)| Cell::from_json(cell).with_context(|| format!("cell #{}", i + 1)))
                .collect::<anyhow::Result<_>>()?,
            Some(other) => bail!("`cells` must be a list, not {}", json_type(&other)),
        };
        Ok(Notebook {
            cells,
            metadata,
            nbformat,
            nbformat_minor,
            extra: obj,
        })
    }

    /// The notebook as JSON (keys in no particular order; see [`fmt::Display`] for the file text).
    pub fn to_json(&self) -> Value {
        let mut obj = self.extra.clone();
        obj.insert(
            "cells".into(),
            Value::Array(self.cells.iter().map(Cell::to_json).collect()),
        );
        obj.insert("metadata".into(), Value::Object(self.metadata.clone()));
        obj.insert("nbformat".into(), self.nbformat.into());
        obj.insert("nbformat_minor".into(), self.nbformat_minor.into());
        Value::Object(obj)
    }

    fn render(&self) -> serde_json::Result<String> {
        let value = sort_keys(self.to_json());
        let mut out = Vec::with_capacity(4096);
        let mut serializer = serde_json::Serializer::with_formatter(
            &mut out,
            JupyterFormatter(PrettyFormatter::with_indent(b" ")),
        );
        value.serialize(&mut serializer)?;
        out.push(b'\n');
        // serde_json only writes valid UTF-8.
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Whether this notebook version has cell ids (nbformat ≥ 4.5).
    pub fn uses_cell_ids(&self) -> bool {
        self.nbformat > 4 || (self.nbformat == 4 && self.nbformat_minor >= 5)
    }

    /// Index of the cell with the given id.
    pub fn cell_index(&self, id: &str) -> Option<usize> {
        self.cells.iter().position(|c| c.id.as_deref() == Some(id))
    }

    /// Inserts `cell` before `index` (clamped to the end) and returns its index. In notebooks with
    /// cell ids, a missing, empty or already used id is replaced by a fresh one.
    pub fn insert_cell(&mut self, index: usize, mut cell: Cell) -> usize {
        let index = index.min(self.cells.len());
        if self.uses_cell_ids() {
            let usable = cell
                .id
                .as_deref()
                .is_some_and(|id| !id.is_empty() && self.cell_index(id).is_none());
            if !usable {
                let taken: HashSet<&str> =
                    self.cells.iter().filter_map(|c| c.id.as_deref()).collect();
                cell.id = Some(fresh_cell_id(|id| taken.contains(id)));
            }
        }
        self.cells.insert(index, cell);
        index
    }

    /// Removes and returns the cell at `index` (`None` when out of range).
    pub fn delete_cell(&mut self, index: usize) -> Option<Cell> {
        (index < self.cells.len()).then(|| self.cells.remove(index))
    }

    /// Moves the cell at `from` so that it ends up at index `to`; false when either is out of range.
    pub fn move_cell(&mut self, from: usize, to: usize) -> bool {
        if from >= self.cells.len() || to >= self.cells.len() {
            return false;
        }
        let cell = self.cells.remove(from);
        self.cells.insert(to, cell);
        true
    }

    /// Gives every cell a unique id (upgrading nbformat 4.0–4.4 notebooks to 4.5, like Jupyter
    /// does on save). Missing, empty and duplicate ids are replaced; returns whether anything changed.
    pub fn ensure_cell_ids(&mut self) -> bool {
        let mut changed = false;
        if self.nbformat == NBFORMAT && self.nbformat_minor < NBFORMAT_MINOR {
            self.nbformat_minor = NBFORMAT_MINOR;
            changed = true;
        }
        let existing: HashSet<String> = self.cells.iter().filter_map(|c| c.id.clone()).collect();
        let mut used: HashSet<String> = HashSet::with_capacity(self.cells.len());
        for cell in &mut self.cells {
            let id = match cell.id.take() {
                Some(id) if !id.is_empty() && !used.contains(&id) => id,
                _ => {
                    changed = true;
                    fresh_cell_id(|id| existing.contains(id) || used.contains(id))
                }
            };
            used.insert(id.clone());
            cell.id = Some(id);
        }
        changed
    }

    /// Clears the outputs and execution counts of all cells.
    pub fn clear_outputs(&mut self) {
        self.cells.iter_mut().for_each(Cell::clear_outputs);
    }

    /// The notebook as a Python script in the "percent" format: one `# %%` block per cell,
    /// markdown and raw cells (`# %% [markdown]`, `# %% [raw]`) as comment lines.
    pub fn to_py(&self) -> String {
        crate::percent::to_py(self)
    }

    /// Builds a notebook from a percent-format script (see [`Notebook::to_py`]); text before the
    /// first `# %%` marker becomes a code cell, and a script without markers a single code cell.
    pub fn from_py(script: &str) -> Notebook {
        crate::percent::from_py(script)
    }
}

impl FromStr for Notebook {
    type Err = anyhow::Error;

    /// Parses the text of an `.ipynb` file.
    fn from_str(text: &str) -> anyhow::Result<Notebook> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let value: Value = serde_json::from_str(text).context("the notebook is not valid JSON")?;
        Notebook::from_json(value)
    }
}

impl fmt::Display for Notebook {
    /// The `.ipynb` file text, formatted like Jupyter: keys sorted at every level, one-space
    /// indentation, non-ASCII characters unescaped, multi-line strings as lists of lines and a
    /// trailing newline.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render().map_err(|_| fmt::Error)?)
    }
}

impl CellKind {
    /// The nbformat `cell_type` value.
    pub fn as_str(self) -> &'static str {
        match self {
            CellKind::Code => "code",
            CellKind::Markdown => "markdown",
            CellKind::Raw => "raw",
        }
    }

    /// Parses an nbformat `cell_type` value.
    pub fn from_name(name: &str) -> Option<CellKind> {
        match name {
            "code" => Some(CellKind::Code),
            "markdown" => Some(CellKind::Markdown),
            "raw" => Some(CellKind::Raw),
            _ => None,
        }
    }
}

impl Cell {
    /// A cell without id, outputs or metadata (ids are assigned by [`Notebook::insert_cell`]).
    pub fn new(kind: CellKind, source: impl Into<String>) -> Cell {
        Cell {
            id: None,
            kind,
            source: source.into(),
            outputs: Vec::new(),
            execution_count: None,
            metadata: JsonMap::new(),
            extra: JsonMap::new(),
        }
    }

    pub fn code(source: impl Into<String>) -> Cell {
        Cell::new(CellKind::Code, source)
    }

    pub fn markdown(source: impl Into<String>) -> Cell {
        Cell::new(CellKind::Markdown, source)
    }

    pub fn raw(source: impl Into<String>) -> Cell {
        Cell::new(CellKind::Raw, source)
    }

    pub fn is_code(&self) -> bool {
        self.kind == CellKind::Code
    }

    /// Removes the outputs and the execution count.
    pub fn clear_outputs(&mut self) {
        self.outputs.clear();
        self.execution_count = None;
    }

    /// Appends stream text the way Jupyter front ends do: consecutive chunks of the same stream
    /// merge into one output, `\r` returns to the start of the line (so progress bars overwrite
    /// themselves) and `\b` erases the previous character.
    pub fn append_stream(&mut self, name: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(Output::Stream {
            name: last_name,
            text: last,
            ..
        }) = self.outputs.last_mut()
        {
            if last_name == name {
                if text.contains(['\r', '\u{8}']) || last.ends_with('\r') {
                    // Only the last line of the existing text can still be overwritten.
                    let keep = last.rfind('\n').map_or(0, |i| i + 1);
                    let mut tail = last.split_off(keep);
                    tail.push_str(text);
                    last.push_str(&resolve_overwrites(&tail));
                } else {
                    last.push_str(text);
                }
                return;
            }
        }
        self.outputs
            .push(Output::stream(name, resolve_overwrites(text)));
    }

    /// Builds a cell from its nbformat JSON (e.g. a cell copied to the clipboard).
    pub fn from_json(value: Value) -> anyhow::Result<Cell> {
        let Value::Object(mut obj) = value else {
            bail!("a cell must be a JSON object")
        };
        let kind = match obj.remove("cell_type") {
            Some(Value::String(name)) => {
                CellKind::from_name(&name).ok_or_else(|| anyhow!("unknown cell_type {name:?}"))?
            }
            Some(other) => bail!("`cell_type` must be a string, not {}", json_type(&other)),
            None => bail!("missing `cell_type`"),
        };
        let id = take_str(&mut obj, "id")?;
        let metadata = take_object(&mut obj, "metadata")?.unwrap_or_default();
        let source = take_multiline(&mut obj, "source")?.unwrap_or_default();
        let (outputs, execution_count) = if kind == CellKind::Code {
            let outputs = match obj.remove("outputs") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) => items
                    .into_iter()
                    .enumerate()
                    .map(|(i, o)| {
                        Output::from_json(o).with_context(|| format!("output #{}", i + 1))
                    })
                    .collect::<anyhow::Result<_>>()?,
                Some(other) => bail!("`outputs` must be a list, not {}", json_type(&other)),
            };
            (outputs, take_count(&mut obj, "execution_count")?)
        } else {
            (Vec::new(), None)
        };
        Ok(Cell {
            id,
            kind,
            source,
            outputs,
            execution_count,
            metadata,
            extra: obj,
        })
    }

    /// The cell's nbformat JSON (code-cell fields only for code cells).
    pub fn to_json(&self) -> Value {
        let mut obj = self.extra.clone();
        obj.insert("cell_type".into(), self.kind.as_str().into());
        if let Some(id) = &self.id {
            obj.insert("id".into(), id.as_str().into());
        }
        obj.insert("metadata".into(), Value::Object(self.metadata.clone()));
        obj.insert("source".into(), multiline(&self.source));
        if self.kind == CellKind::Code {
            obj.insert(
                "execution_count".into(),
                self.execution_count.map_or(Value::Null, Value::from),
            );
            obj.insert(
                "outputs".into(),
                Value::Array(self.outputs.iter().map(Output::to_json).collect()),
            );
        }
        Value::Object(obj)
    }
}

impl Output {
    pub fn stream(name: impl Into<String>, text: impl Into<String>) -> Output {
        Output::Stream {
            name: name.into(),
            text: text.into(),
            extra: JsonMap::new(),
        }
    }

    pub fn display_data(data: MimeBundle) -> Output {
        Output::DisplayData {
            data,
            metadata: JsonMap::new(),
            extra: JsonMap::new(),
        }
    }

    pub fn execute_result(execution_count: Option<u32>, data: MimeBundle) -> Output {
        Output::ExecuteResult {
            execution_count,
            data,
            metadata: JsonMap::new(),
            extra: JsonMap::new(),
        }
    }

    pub fn error(
        ename: impl Into<String>,
        evalue: impl Into<String>,
        traceback: Vec<String>,
    ) -> Output {
        Output::Error {
            ename: ename.into(),
            evalue: evalue.into(),
            traceback,
            extra: JsonMap::new(),
        }
    }

    /// The nbformat `output_type`.
    pub fn output_type(&self) -> &str {
        match self {
            Output::Stream { .. } => "stream",
            Output::DisplayData { .. } => "display_data",
            Output::ExecuteResult { .. } => "execute_result",
            Output::Error { .. } => "error",
            Output::Unknown(obj) => obj.get("output_type").and_then(Value::as_str).unwrap_or(""),
        }
    }

    /// The mime bundle of display data and execute results.
    pub fn data(&self) -> Option<&MimeBundle> {
        match self {
            Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => Some(data),
            _ => None,
        }
    }

    /// A plain-text rendering: stream text, the `text/plain` representation, or the traceback.
    pub fn plain_text(&self) -> Option<String> {
        match self {
            Output::Stream { text, .. } => Some(text.clone()),
            Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
                data.get("text/plain").cloned()
            }
            Output::Error {
                ename,
                evalue,
                traceback,
                ..
            } => Some(if traceback.is_empty() {
                format!("{ename}: {evalue}")
            } else {
                traceback.join("\n")
            }),
            Output::Unknown(_) => None,
        }
    }

    /// Builds an output from its nbformat JSON.
    pub fn from_json(value: Value) -> anyhow::Result<Output> {
        let Value::Object(mut obj) = value else {
            bail!("an output must be a JSON object")
        };
        let output_type = match obj.get("output_type") {
            Some(Value::String(t)) => t.clone(),
            Some(other) => bail!("`output_type` must be a string, not {}", json_type(other)),
            None => bail!("missing `output_type`"),
        };
        if !matches!(
            output_type.as_str(),
            "stream" | "display_data" | "execute_result" | "error"
        ) {
            return Ok(Output::Unknown(obj));
        }
        obj.remove("output_type");
        Ok(match output_type.as_str() {
            "stream" => Output::Stream {
                name: take_str(&mut obj, "name")?.unwrap_or_else(|| "stdout".into()),
                text: take_multiline(&mut obj, "text")?.unwrap_or_default(),
                extra: obj,
            },
            "display_data" => Output::DisplayData {
                data: take_bundle(&mut obj)?,
                metadata: take_object(&mut obj, "metadata")?.unwrap_or_default(),
                extra: obj,
            },
            "execute_result" => Output::ExecuteResult {
                execution_count: take_count(&mut obj, "execution_count")?,
                data: take_bundle(&mut obj)?,
                metadata: take_object(&mut obj, "metadata")?.unwrap_or_default(),
                extra: obj,
            },
            _ => Output::Error {
                ename: take_str(&mut obj, "ename")?.unwrap_or_default(),
                evalue: take_str(&mut obj, "evalue")?.unwrap_or_default(),
                traceback: take_string_list(&mut obj, "traceback")?.unwrap_or_default(),
                extra: obj,
            },
        })
    }

    /// The output's nbformat JSON.
    pub fn to_json(&self) -> Value {
        let (output_type, mut obj) = match self {
            Output::Stream { name, text, extra } => {
                let mut obj = extra.clone();
                obj.insert("name".into(), name.as_str().into());
                obj.insert("text".into(), multiline(text));
                ("stream", obj)
            }
            Output::DisplayData {
                data,
                metadata,
                extra,
            } => {
                let mut obj = extra.clone();
                obj.insert("data".into(), bundle_to_json(data));
                obj.insert("metadata".into(), Value::Object(metadata.clone()));
                ("display_data", obj)
            }
            Output::ExecuteResult {
                execution_count,
                data,
                metadata,
                extra,
            } => {
                let mut obj = extra.clone();
                obj.insert(
                    "execution_count".into(),
                    execution_count.map_or(Value::Null, Value::from),
                );
                obj.insert("data".into(), bundle_to_json(data));
                obj.insert("metadata".into(), Value::Object(metadata.clone()));
                ("execute_result", obj)
            }
            Output::Error {
                ename,
                evalue,
                traceback,
                extra,
            } => {
                let mut obj = extra.clone();
                obj.insert("ename".into(), ename.as_str().into());
                obj.insert("evalue".into(), evalue.as_str().into());
                obj.insert(
                    "traceback".into(),
                    Value::Array(traceback.iter().map(|l| Value::from(l.as_str())).collect()),
                );
                ("error", obj)
            }
            Output::Unknown(obj) => return Value::Object(obj.clone()),
        };
        obj.insert("output_type".into(), output_type.into());
        Value::Object(obj)
    }
}

/// Whether a mime type carries JSON data (stored as a JSON value, not as text).
pub(crate) fn is_json_mime(mime: &str) -> bool {
    mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json"))
}

/// Mime types Jupyter writes as lists of lines.
fn is_line_split_mime(mime: &str) -> bool {
    mime.starts_with("text/") || mime == "application/javascript" || mime == "image/svg+xml"
}

fn take_bundle(obj: &mut JsonMap) -> anyhow::Result<MimeBundle> {
    let data = match obj.remove("data") {
        None | Some(Value::Null) => return Ok(MimeBundle::new()),
        Some(Value::Object(data)) => data,
        Some(other) => bail!("`data` must be an object, not {}", json_type(&other)),
    };
    let mut bundle = MimeBundle::new();
    for (mime, value) in data {
        let text = match value {
            _ if is_json_mime(&mime) => value.to_string(),
            Value::String(text) => text,
            Value::Array(items) if items.iter().all(Value::is_string) => {
                items.iter().filter_map(Value::as_str).collect()
            }
            // Not valid nbformat; keep the JSON text rather than dropping the data.
            other => other.to_string(),
        };
        bundle.insert(mime, text);
    }
    Ok(bundle)
}

fn bundle_to_json(data: &MimeBundle) -> Value {
    let obj = data
        .iter()
        .map(|(mime, text)| {
            let value = if is_json_mime(mime) {
                serde_json::from_str(text).unwrap_or_else(|_| Value::from(text.as_str()))
            } else if is_line_split_mime(mime) {
                multiline(text)
            } else {
                Value::from(text.as_str())
            };
            (mime.clone(), value)
        })
        .collect();
    Value::Object(obj)
}

/// A multi-line string as Jupyter stores it: a list of lines that keep their terminators.
fn multiline(text: &str) -> Value {
    Value::Array(split_lines(text).into_iter().map(Value::from).collect())
}

/// Python's `str.splitlines(keepends=True)`, which Jupyter uses to split multi-line strings.
pub(crate) fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = match c {
            '\r' => match chars.peek() {
                Some(&(j, '\n')) => {
                    chars.next();
                    j + 1
                }
                _ => i + 1,
            },
            '\n' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => i + c.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Applies `\b` (erase the previous character of the line) and `\r` (back to the start of the
/// line; later characters overwrite) like Jupyter front ends. A trailing `\r` is kept so that the
/// next chunk of the stream can still overwrite the line.
fn resolve_overwrites(text: &str) -> String {
    if !text.contains(['\r', '\u{8}']) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut lines = text.split('\n').peekable();
    while let Some(raw) = lines.next() {
        let mut erased: Vec<char> = Vec::with_capacity(raw.len());
        for c in raw.chars() {
            if c == '\u{8}' && erased.last().is_some_and(|&prev| prev != '\u{8}') {
                erased.pop();
            } else {
                erased.push(c);
            }
        }
        let mut line: Vec<char> = Vec::with_capacity(erased.len());
        let mut column = 0;
        let mut pending_return = false;
        for c in erased {
            if c == '\r' {
                column = 0;
                pending_return = true;
                continue;
            }
            pending_return = false;
            if column < line.len() {
                line[column] = c;
            } else {
                line.push(c);
            }
            column += 1;
        }
        out.extend(line);
        if lines.peek().is_some() {
            out.push('\n');
        } else if pending_return {
            out.push('\r');
        }
    }
    out
}

/// Rebuilds `value` with object keys sorted at every level (Jupyter writes with `sort_keys=True`;
/// this also holds if serde_json's `preserve_order` feature gets enabled in the build).
fn sort_keys(value: Value) -> Value {
    match value {
        Value::Object(obj) => {
            let mut entries: Vec<(String, Value)> = obj.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, sort_keys(v)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_keys).collect()),
        other => other,
    }
}

/// Writes `bytes` to `path` (following symlinks) through a temporary file in the same directory
/// that then replaces it; an existing file keeps its permissions. Where the directory does not
/// allow this (no write access to it, a file locked on Windows) the file is written in place, but
/// never after writing the data failed: that is the case the original must survive.
fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // Fail where writing in place would (a read-only notebook, a directory): a rename would not.
    let permissions = match std::fs::OpenOptions::new().write(true).open(&target) {
        Ok(existing) => Some(existing.metadata()?.permissions()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let Some(name) = target.file_name() else {
        return std::fs::write(&target, bytes);
    };
    let tmp = target.with_file_name(format!(
        ".{}.{:08x}.tmp",
        name.to_string_lossy(),
        random_u64() & 0xffff_ffff
    ));
    let Ok(mut file) = std::fs::File::create_new(&tmp) else {
        return std::fs::write(&target, bytes);
    };
    let written = file.write_all(bytes);
    if written.is_ok() {
        if let Some(permissions) = permissions {
            let _ = file.set_permissions(permissions);
        }
        let _ = file.sync_all(); // best effort: not every file system supports it
    }
    drop(file);
    let result = written
        .and_then(|()| std::fs::rename(&tmp, &target).or_else(|_| std::fs::write(&target, bytes)));
    let _ = std::fs::remove_file(&tmp); // (already gone after the rename)
    result
}

/// The layout of `json.dumps(indent=1, ensure_ascii=False)`: serde_json's pretty printer and
/// string escapes already match it; floats are written the way Python writes them.
struct JupyterFormatter(PrettyFormatter<'static>);

impl Formatter for JupyterFormatter {
    fn begin_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_array(writer)
    }

    fn end_array<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array(writer)
    }

    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_array_value(writer, first)
    }

    fn end_array_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array_value(writer)
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object(writer)
    }

    fn end_object<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object(writer)
    }

    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_object_key(writer, first)
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object_value(writer)
    }

    fn end_object_value<W: ?Sized + io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object_value(writer)
    }

    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        writer.write_all(python_float_repr(value).as_bytes())
    }

    fn write_number_str<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        value: &str,
    ) -> io::Result<()> {
        // serde_json's arbitrary_precision feature sends Numbers through this
        // path, including floats. Keep integer text exact; apply our existing
        // Python spelling only to finite floating-point values.
        if value.contains(['.', 'e', 'E']) {
            if let Ok(number) = value.parse::<f64>() {
                if number.is_finite() {
                    return self.write_f64(writer, number);
                }
            }
        }
        writer.write_all(value.as_bytes())
    }
}

/// Python's `repr()` of a finite float (what `json.dumps` writes): the shortest digits that read
/// back as the same value, positional for decimal exponents -4 to 15 (`0.0001`, `100.0`) and
/// scientific otherwise, with a signed exponent of at least two digits (`1e-05`, `1.5e+16`).
fn python_float_repr(value: f64) -> String {
    // `{:e}` gives the shortest round-trip digits too: `-1.25e-7`, `5e-324`, `0e0`.
    let sci = format!("{value:e}");
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(mantissa) => ("-", mantissa),
        None => ("", mantissa),
    };
    let digits = mantissa.replace('.', "");
    if !(-4..=15).contains(&exponent) {
        let (first, rest) = digits.split_at(1);
        let dot = if rest.is_empty() { "" } else { "." };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!(
            "{sign}{first}{dot}{rest}e{exponent_sign}{:02}",
            exponent.unsigned_abs()
        );
    }
    // The decimal point goes after `point` digits (padding with zeros on either side).
    let point = exponent + 1;
    if point <= 0 {
        return format!(
            "{sign}0.{}{digits}",
            "0".repeat(point.unsigned_abs() as usize)
        );
    }
    let point = point as usize;
    if digits.len() <= point {
        format!("{sign}{digits}{}.0", "0".repeat(point - digits.len()))
    } else {
        format!("{sign}{}.{}", &digits[..point], &digits[point..])
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn take_str(obj: &mut JsonMap, key: &str) -> anyhow::Result<Option<String>> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => bail!("`{key}` must be a string, not {}", json_type(&other)),
    }
}

fn take_multiline(obj: &mut JsonMap, key: &str) -> anyhow::Result<Option<String>> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Array(items)) => {
            let mut text = String::new();
            for item in items {
                match item {
                    Value::String(line) => text.push_str(&line),
                    other => bail!(
                        "`{key}` must be a string or a list of strings, not a list containing {}",
                        json_type(&other)
                    ),
                }
            }
            Ok(Some(text))
        }
        Some(other) => bail!(
            "`{key}` must be a string or a list of strings, not {}",
            json_type(&other)
        ),
    }
}

fn take_string_list(obj: &mut JsonMap, key: &str) -> anyhow::Result<Option<Vec<String>>> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .into_iter()
            .map(|item| match item {
                Value::String(s) => Ok(s),
                other => Err(anyhow!(
                    "`{key}` must be a list of strings, not a list containing {}",
                    json_type(&other)
                )),
            })
            .collect::<anyhow::Result<_>>()
            .map(Some),
        Some(other) => bail!(
            "`{key}` must be a list of strings, not {}",
            json_type(&other)
        ),
    }
}

fn take_object(obj: &mut JsonMap, key: &str) -> anyhow::Result<Option<JsonMap>> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map)),
        Some(other) => bail!("`{key}` must be an object, not {}", json_type(&other)),
    }
}

fn take_count(obj: &mut JsonMap, key: &str) -> anyhow::Result<Option<u32>> {
    match obj.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| anyhow!("`{key}` must be a non-negative integer, not {n}")),
        Some(other) => bail!("`{key}` must be an integer, not {}", json_type(&other)),
    }
}

/// 64 random bits (std's randomly keyed SipHash over a counter and the clock).
pub(crate) fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(now.as_nanos());
    }
    hasher.finish()
}

/// A random 8-hex-digit cell id (like nbformat's `random_cell_id`) for which `taken` is false.
fn fresh_cell_id(taken: impl Fn(&str) -> bool) -> String {
    loop {
        let id = format!("{:08x}", random_u64() & 0xffff_ffff);
        if !taken(&id) {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_matches_python_splitlines() {
        assert!(split_lines("").is_empty());
        assert_eq!(split_lines("a"), ["a"]);
        assert_eq!(split_lines("a\nb"), ["a\n", "b"]);
        assert_eq!(split_lines("a\n"), ["a\n"]);
        assert_eq!(split_lines("a\r\nb\rc\n\n"), ["a\r\n", "b\r", "c\n", "\n"]);
        assert_eq!(
            split_lines("x\u{c}y\u{2028}z\u{85}"),
            ["x\u{c}", "y\u{2028}", "z\u{85}"]
        );
        assert_eq!(split_lines("é\nü"), ["é\n", "ü"]);
    }

    #[test]
    fn overwrites_follow_jupyter_rules() {
        assert_eq!(resolve_overwrites("plain\ntext"), "plain\ntext");
        assert_eq!(resolve_overwrites("10%\r20%\r30%\ndone\n"), "30%\ndone\n");
        assert_eq!(resolve_overwrites("abc\rX\n"), "Xbc\n");
        assert_eq!(resolve_overwrites("line\r\nnext"), "line\nnext");
        assert_eq!(resolve_overwrites("ab\u{8}c"), "ac");
        assert_eq!(resolve_overwrites("x\u{8}\u{8}y"), "\u{8}y");
        // A trailing return stays pending so the next chunk overwrites the line.
        assert_eq!(resolve_overwrites("50%\r"), "50%\r");
    }

    #[test]
    fn stream_chunks_merge_and_overwrite() {
        let mut cell = Cell::code("");
        cell.append_stream("stdout", "a");
        cell.append_stream("stdout", "b\n");
        cell.append_stream("stderr", "warn\n");
        cell.append_stream("stdout", "c\n");
        assert_eq!(
            cell.outputs,
            vec![
                Output::stream("stdout", "ab\n"),
                Output::stream("stderr", "warn\n"),
                Output::stream("stdout", "c\n")
            ]
        );

        let mut cell = Cell::code("");
        cell.append_stream("stderr", "start\n 10%\r");
        cell.append_stream("stderr", " 60%\r");
        cell.append_stream("stderr", "100%\n");
        assert_eq!(
            cell.outputs,
            vec![Output::stream("stderr", "start\n100%\n")]
        );
        cell.append_stream("stderr", "");
        assert_eq!(cell.outputs.len(), 1);
    }

    #[test]
    fn json_mimes_keep_their_values() {
        let value = serde_json::json!({
            "output_type": "display_data",
            "data": { "application/json": { "b": [1, 2], "a": "x" }, "application/vnd.custom+json": "just a string", "text/plain": ["x\n", "y"], "image/png": "iVBORw0K" },
            "metadata": {},
        });
        let output = Output::from_json(value.clone()).unwrap();
        let data = output.data().unwrap();
        assert_eq!(data["text/plain"], "x\ny");
        assert_eq!(
            serde_json::from_str::<Value>(&data["application/json"]).unwrap(),
            serde_json::json!({ "a": "x", "b": [1, 2] })
        );
        assert_eq!(output.to_json(), value);
    }

    #[test]
    fn floats_are_written_like_python() {
        // Expected values are Python's repr() of the same floats.
        let cases: [(f64, &str); 21] = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (12.0, "12.0"),
            (-1.5, "-1.5"),
            (0.1, "0.1"),
            (0.30000000000000004, "0.30000000000000004"),
            (123456.789, "123456.789"),
            (0.001234, "0.001234"),
            (0.0001, "0.0001"),
            (1e-5, "1e-05"),
            (2.5e-7, "2.5e-07"),
            (-2.5e-7, "-2.5e-07"),
            (5e-324, "5e-324"),
            (1e15, "1000000000000000.0"),
            (1234567890123456.0, "1234567890123456.0"),
            (1e16, "1e+16"),
            (1.2345678901234568e16, "1.2345678901234568e+16"),
            (1e22, "1e+22"),
            (1.5e300, "1.5e+300"),
            (f64::MAX, "1.7976931348623157e+308"),
            (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
        ];
        for (value, expected) in cases {
            assert_eq!(python_float_repr(value), expected, "{value:e}");
            assert_eq!(expected.parse::<f64>().unwrap().to_bits(), value.to_bits());
        }
        // Inside a document, through the formatter.
        let mut nb = Notebook::empty();
        nb.metadata.insert(
            "floats".into(),
            serde_json::json!([1e-5, 100.0, 0.5, -3, 1e16]),
        );
        assert!(
            nb.to_string()
                .contains("\"floats\": [\n   1e-05,\n   100.0,\n   0.5,\n   -3,\n   1e+16\n  ]"),
            "{nb}"
        );
    }

    #[test]
    fn rendered_number_callback_preserves_large_integers_and_signed_zero() {
        let mut formatter = JupyterFormatter(PrettyFormatter::with_indent(b" "));
        for (input, expected) in [
            (
                "184467440737095516170123456789",
                "184467440737095516170123456789",
            ),
            (
                "-184467440737095516170123456789",
                "-184467440737095516170123456789",
            ),
            ("0.00001", "1e-05"),
            ("-0.0", "-0.0"),
            ("5E-324", "5e-324"),
            ("1e400", "1e400"),
        ] {
            let mut output = Vec::new();
            formatter.write_number_str(&mut output, input).unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), expected);
        }
    }

    #[test]
    fn fresh_ids_are_unique_hex() {
        let mut seen = HashSet::new();
        for _ in 0..2000 {
            let id = fresh_cell_id(|id| seen.contains(id));
            assert_eq!(id.len(), 8);
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(seen.insert(id));
        }
    }
}
