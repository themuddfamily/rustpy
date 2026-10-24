//! CSV table parser and serializer selected by a validated enabled-plugin contribution.
//! Parsing happens on document/version changes, never inside the paint loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
    pub columns: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CsvError {
    pub byte: usize,
    pub message: &'static str,
}
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_CELLS: usize = 100_000;
const MAX_COLUMNS: usize = 1024;
/// Strict comma-separated text; quoted newlines and doubled quotes are preserved.
/// Empty input has no rows. Ragged records remain visible rather than silently padded.
pub fn parse(source: &str) -> Result<Table, CsvError> {
    parse_delimited(source, ',')
}
pub fn parse_delimited(source: &str, delimiter: char) -> Result<Table, CsvError> {
    let bom_bytes = if source.starts_with('\u{feff}') { 3 } else { 0 };
    let fail = |byte, message| CsvError {
        byte: byte + bom_bytes,
        message,
    };
    if source.len() > MAX_BYTES {
        return Err(fail(0, "CSV exceeds 8 MiB table limit; use source view"));
    }
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut closed = false;
    let mut cells = 0usize;
    let mut chars = source.char_indices().peekable();
    let mut terminated = true;
    while let Some((byte, ch)) = chars.next() {
        terminated = false;
        if quoted {
            if ch == '"' {
                if chars.peek().is_some_and(|(_, c)| *c == '"') {
                    chars.next();
                    cell.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                cell.push(ch);
            }
            continue;
        }
        if ch == delimiter || ch == '\r' || ch == '\n' {
            cells += 1;
            if cells > MAX_CELLS || row.len() >= MAX_COLUMNS {
                return Err(fail(
                    byte,
                    "CSV table cell/column limit exceeded; use source view",
                ));
            }
            row.push(std::mem::take(&mut cell));
            closed = false;
            if ch != delimiter {
                if ch == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                    chars.next();
                }
                rows.push(std::mem::take(&mut row));
                terminated = true;
            }
        } else if closed {
            return Err(fail(byte, "Unexpected text after closing quote"));
        } else if ch == '"' {
            if !cell.is_empty() {
                return Err(fail(byte, "Quote inside unquoted field"));
            }
            quoted = true;
        } else {
            cell.push(ch);
        }
    }
    if quoted {
        return Err(fail(source.len(), "Unterminated quoted field"));
    }
    if !terminated {
        cells += 1;
        if cells > MAX_CELLS || row.len() >= MAX_COLUMNS {
            return Err(fail(
                source.len(),
                "CSV table cell/column limit exceeded; use source view",
            ));
        }
        row.push(cell);
        rows.push(row);
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    Ok(Table { rows, columns })
}
#[cfg(test)]
/// Caller owns source/version cache and mode selection. This widget never edits text.
pub fn show(ui: &mut egui::Ui, table: &Table, id: egui::Id) {
    ui.label(format!(
        "{} records · {} columns · read-only table",
        table.rows.len(),
        table.columns
    ));
    egui::ScrollArea::both()
        .id_salt(id)
        .show_rows(ui, 24.0, table.rows.len(), |ui, visible| {
            egui::Grid::new(id.with("rows"))
                .striped(true)
                .min_row_height(24.0)
                .show(ui, |ui| {
                    for index in visible {
                        ui.label(format!("{}", index + 1));
                        for cell in &table.rows[index] {
                            // Bound painted labels, preserving full field in hover for modest fields.
                            let text: String = cell
                                .chars()
                                .take(240)
                                .map(|c| if c == '\n' || c == '\r' { '↵' } else { c })
                                .collect();
                            let response = ui.add(
                                egui::Label::new(text)
                                    .wrap_mode(egui::TextWrapMode::Extend)
                                    .selectable(true),
                            );
                            if cell.len() <= 4096 {
                                response.on_hover_text(cell);
                            }
                        }
                        ui.end_row();
                    }
                });
        });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_unicode_multiline_and_empty_fields() {
        let t = parse("\u{feff}name,note\r\nπ,\"🙂, first\r\n\"\"second\"\"\"\r\n,,\r\n").unwrap();
        assert_eq!(
            t.rows,
            vec![
                vec!["name", "note"],
                vec!["π", "🙂, first\r\n\"second\""],
                vec!["", "", ""]
            ]
        );
        assert_eq!(t.columns, 3);
    }
    #[test]
    fn malformed_and_limits_are_truthful() {
        assert!(parse("a,\"open").is_err());
        assert!(parse("\"ok\"junk").is_err());
        assert!(parse("a\"b").is_err());
        assert_eq!(parse("\u{feff}\"ok\"junk").unwrap_err().byte, 7);
        assert_eq!(parse("\u{feff}\"open").unwrap_err().byte, 8);
        assert!(parse(&"x".repeat(MAX_BYTES + 1)).is_err());
        assert!(parse(&",".repeat(MAX_COLUMNS)).is_err());
        assert_eq!(parse("").unwrap().rows.len(), 0);
        assert_eq!(parse("a,").unwrap().rows, vec![vec!["a", ""]]);
    }
    #[test]
    fn actual_table_frame_preserves_model_and_paints_unicode() {
        let t = parse("name,value\nπ,\"🙂 first\n\"\"second\"\"\"\n").unwrap();
        let before = t.clone();
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(ui, &t, egui::Id::new("csv"))
        });
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "π")));
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "🙂 first↵\"second\"")));
        assert_eq!(t, before);
        output.drop_without_applying_deltas();
    }
}

pub fn delimiter(source: &str) -> char {
    let preview = source.lines().take(32).collect::<Vec<_>>().join("\n");
    [',', '\t', ';', '|']
        .into_iter()
        .filter_map(|delimiter| {
            parse_delimited(&preview, delimiter).ok().map(|table| {
                let width = table.rows.first().map(Vec::len).unwrap_or(1);
                let consistent = table.rows.iter().filter(|row| row.len() == width).count();
                (
                    delimiter,
                    if width > 1 {
                        consistent * 100 + width.min(99)
                    } else {
                        0
                    },
                )
            })
        })
        .filter(|(_, score)| *score > 0)
        .max_by_key(|(_, score)| *score)
        .map(|(delimiter, _)| delimiter)
        .filter(|_| !preview.is_empty())
        .unwrap_or(',')
}
pub fn serialize(table: &Table, delimiter: char, ending: &str) -> String {
    let mut text = table
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| {
                    if cell.contains([delimiter, '"', '\r', '\n'])
                        || cell.starts_with(char::is_whitespace)
                        || cell.ends_with(char::is_whitespace)
                    {
                        format!("\"{}\"", cell.replace('"', "\"\""))
                    } else {
                        cell.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(&delimiter.to_string())
        })
        .collect::<Vec<_>>()
        .join(ending);
    if !table.rows.is_empty() {
        text.push_str(ending);
    }
    text
}
#[cfg(test)]
mod delimiter_fixtures {
    use super::*;
    #[test]
    fn tsv_roundtrip_preserves_quoted_tabs_and_newlines() {
        let source = "name\tvalue\n\"a\tb\"\t\"two\nlines\"\n";
        assert_eq!(delimiter(source), '\t');
        let table = parse_delimited(source, '\t').unwrap();
        assert_eq!(table.rows[1][0], "a\tb");
        assert_eq!(
            parse_delimited(&serialize(&table, '\t', "\n"), '\t').unwrap(),
            table
        );
    }
}

#[cfg(test)]
mod edit_fixtures {
    use super::*;
    #[test]
    fn table_edit_round_trip_retains_quotes_unicode_and_multiline_fields() {
        let mut table = parse_delimited("name\tvalue\r\nπ\t\"a\tb\"\r\n", '\t').unwrap();
        table.rows[1][1] = "edited\n\"quoted\"".into();
        let output = serialize(&table, '\t', "\r\n");
        assert_eq!(parse_delimited(&output, '\t').unwrap(), table);
        assert!(output.contains("\"\"quoted\"\""));
    }
}
