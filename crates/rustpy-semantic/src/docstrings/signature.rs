//! Source-preserving parameter documentation used by Rename and Change Signature.
use super::*;
use std::collections::HashSet;

struct Header {
    line: usize,
    names: Vec<(String, usize)>,
    indent: usize,
    kind: HeaderKind,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum HeaderKind {
    Field,
    Type,
    Google,
    NumPy,
}

fn headers(raw: &str) -> Vec<Header> {
    let mut normalized = raw.to_string();
    if let Some(opening) = opening_quotes(raw) {
        normalized.replace_range(..opening, &" ".repeat(opening));
    }
    let lines: Vec<_> = normalized.lines().collect();
    let base = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| indent_of(line))
        .min()
        .unwrap_or(0);
    let mut out = Vec::new();
    let mut google_args = false;
    let mut google_indent = None;
    let mut numpy_args = false;
    for (index, line) in lines.iter().enumerate() {
        let indent = indent_of(line);
        let trimmed = line.trim_start();
        if indent == base || index == 0 {
            if let Some(section) = GOOGLE_SECTION.captures(line) {
                google_args = matches!(
                    &section[2],
                    "Args"
                        | "Arguments"
                        | "Parameters"
                        | "Params"
                        | "Keyword Args"
                        | "Keyword Arguments"
                        | "Kwargs"
                        | "Other Parameters"
                );
                google_indent = None;
                numpy_args = false;
                continue;
            }
            if lines
                .get(index + 1)
                .is_some_and(|line| is_underline(line, '-'))
            {
                numpy_args = matches!(trimmed, "Parameters" | "Other Parameters");
                google_args = false;
                continue;
            }
            if let Some(field) = FIELD_LINE.captures(line) {
                google_args = false;
                numpy_args = false;
                if matches!(
                    &field[2],
                    "param"
                        | "parameter"
                        | "arg"
                        | "argument"
                        | "key"
                        | "keyword"
                        | "kwarg"
                        | "kwparam"
                        | "type"
                ) {
                    if let Some(arg) = field.get(3) {
                        if let Some(name) = arg.as_str().split_whitespace().last() {
                            let bare = strip_stars(name);
                            if valid_name(bare) {
                                out.push(Header {
                                    line: index,
                                    names: vec![(
                                        bare.into(),
                                        arg.start()
                                            + arg.as_str().rfind(name).unwrap()
                                            + name.len()
                                            - bare.len(),
                                    )],
                                    indent,
                                    kind: if &field[2] == "type" {
                                        HeaderKind::Type
                                    } else {
                                        HeaderKind::Field
                                    },
                                });
                            }
                        }
                    }
                }
                continue;
            }
        }
        if google_args && indent > base {
            if let Some(arg) = GOOGLE_ARG.captures(trimmed) {
                let name = arg.get(1).unwrap().as_str();
                if google_indent.is_none_or(|expected| expected == indent) {
                    google_indent = Some(indent);
                    let bare = strip_stars(name);
                    out.push(Header {
                        line: index,
                        names: vec![(bare.into(), indent + name.len() - bare.len())],
                        indent,
                        kind: HeaderKind::Google,
                    });
                }
            }
        } else if numpy_args && indent == base && !is_underline(line, '-') {
            let lhs = trimmed
                .split_once(':')
                .map_or(trimmed, |(names, _)| names)
                .trim_end();
            let names: Vec<_> = lhs.split(',').map(str::trim).collect();
            if !names.is_empty() && names.iter().all(|name| valid_name(strip_stars(name))) {
                let mut at = indent;
                let mut located = Vec::new();
                for name in names {
                    let bare = strip_stars(name);
                    let start = at + line[at..].find(name).unwrap();
                    located.push((bare.into(), start + name.len() - bare.len()));
                    at = start + name.len();
                }
                out.push(Header {
                    line: index,
                    names: located,
                    indent,
                    kind: HeaderKind::NumPy,
                });
            }
        } else if !trimmed.is_empty() && indent <= base {
            google_args = false;
        }
    }
    out
}

fn opening_quotes(raw: &str) -> Option<usize> {
    let prefix = raw.len() - raw.trim_start_matches(['r', 'R', 'u', 'U']).len();
    let text = &raw[prefix..];
    if text.starts_with("\"\"\"") || text.starts_with("'''") {
        Some(prefix + 3)
    } else if text.starts_with(['\'', '"']) {
        Some(prefix + 1)
    } else {
        None
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(rustpy_core::is_ident_start)
        && chars.all(rustpy_core::is_ident_continue)
}

fn top_colon(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (at, ch) in text.char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some(at),
            _ => {}
        }
    }
    None
}

pub(crate) fn parameter_name_ranges(raw: &str, name: &str) -> Vec<TextRange> {
    let lines: Vec<_> = raw.split_inclusive('\n').collect();
    let offsets: Vec<_> = lines
        .iter()
        .scan(0usize, |at, line| {
            let start = *at;
            *at += line.len();
            Some(start)
        })
        .collect();
    headers(raw)
        .into_iter()
        .flat_map(|header| {
            let start = offsets[header.line];
            header
                .names
                .into_iter()
                .filter_map(move |(candidate, offset)| {
                    (candidate == name).then(|| {
                        TextRange::at(TextSize::new((start + offset) as u32), TextSize::of(name))
                    })
                })
        })
        .collect()
}

/// Synchronize a candidate function after signature edits. Existing prose and
/// unrecognized sections remain byte-for-byte; only actual parameter fields
/// are removed/updated and missing fields are inserted in the existing style.
/// `original_names` limits deletion to parameters the refactoring removed.
pub fn synchronize_signature(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    original_names: &[String],
    options: StubOptions,
) -> Option<(TextRange, String)> {
    let root = file.parse.syntax_node();
    let function = func_at(&root, offset)?;
    let doc = function.docstring()?;
    let facts = facts_for(file, &function)?;
    let range = doc.syntax().text_range();
    let raw = &file.text[usize::from(range.start())..usize::from(range.end())];
    // Concatenated and escaped single-line strings need a decoder-aware rewrite.
    // They carry no independently editable physical section lines here.
    if !raw.contains('\n') {
        return update_with_options(db, file, offset, detect_one(&doc.value()), options);
    }
    let lines: Vec<_> = raw.split_inclusive('\n').collect();
    let mut starts = Vec::new();
    let mut pos = 0;
    for line in &lines {
        starts.push(pos);
        pos += line.len();
    }
    let old: HashSet<_> = original_names.iter().map(String::as_str).collect();
    let desired: HashSet<_> = facts
        .params
        .iter()
        .map(|(name, _)| strip_stars(name))
        .collect();
    let mut edits = Vec::<(TextRange, String)>::new();
    let doc_headers = headers(raw);
    let header_lines: HashSet<_> = doc_headers.iter().map(|header| header.line).collect();
    for header in doc_headers {
        let line = lines[header.line];
        let remaining: Vec<_> = header
            .names
            .iter()
            .filter(|(name, _)| !old.contains(name.as_str()) || desired.contains(name.as_str()))
            .collect();
        if remaining.is_empty() {
            let mut end = header.line + 1;
            while end < lines.len()
                && !header_lines.contains(&end)
                && !matches!(lines[end].trim(), "\"\"\"" | "'''")
                && (lines[end].trim().is_empty() || indent_of(lines[end]) > header.indent)
            {
                end += 1;
            }
            while end > header.line + 1 && lines[end - 1].trim().is_empty() {
                end -= 1;
            }
            let stop = starts.get(end).copied().unwrap_or(raw.len());
            edits.push((
                TextRange::new(
                    TextSize::new(starts[header.line] as u32),
                    TextSize::new(stop as u32),
                ),
                if header.line == 0 {
                    format!("{}\n", &raw[..opening_quotes(raw)?])
                } else {
                    String::new()
                },
            ));
            continue;
        }
        let annotation = if remaining.len() == 1 {
            facts
                .params
                .iter()
                .find(|(name, _)| strip_stars(name) == remaining[0].0)
                .and_then(|(_, annotation)| annotation.as_deref())
        } else {
            None
        };
        let mut changed = line.to_string();
        if header.kind == HeaderKind::NumPy {
            let suffix = line.find(':').map(|at| &line[at..]).unwrap_or("\n");
            if remaining.len() != header.names.len() {
                changed = format!(
                    "{}{}{}{}",
                    &line[..header.indent],
                    remaining
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    if suffix.starts_with(':') { " " } else { "" },
                    suffix
                );
            }
            if let Some(annotation) = annotation {
                if let Some(colon) = changed.find(':') {
                    changed = format!(
                        "{}: {}{}",
                        &changed[..colon],
                        annotation,
                        if line.ends_with('\n') { "\n" } else { "" }
                    );
                }
            }
        } else if let Some(annotation) = annotation {
            if header.kind == HeaderKind::Type {
                if let Some(colon) = FIELD_LINE
                    .captures(changed.trim_end_matches(['\r', '\n']))
                    .and_then(|field| field.get(3))
                    .and_then(|argument| {
                        changed[argument.end()..]
                            .find(':')
                            .map(|at| argument.end() + at)
                    })
                {
                    changed = format!(
                        "{} {}{}",
                        &changed[..=colon],
                        annotation,
                        if line.ends_with('\n') { "\n" } else { "" }
                    );
                }
            } else if header.kind == HeaderKind::Google {
                let (name, at) = &remaining[0];
                let after_name = at + name.len();
                if let Some(colon) = top_colon(&changed[after_name..]) {
                    let colon = after_name + colon;
                    changed = format!(
                        "{} ({}){}",
                        &changed[..after_name],
                        annotation,
                        &changed[colon..]
                    );
                }
            } else if let Some(field) = FIELD_LINE.captures(line.trim_end_matches(['\r', '\n'])) {
                if let Some(argument) = field
                    .get(3)
                    .filter(|argument| argument.as_str().split_whitespace().count() > 1)
                {
                    let name = argument.as_str().split_whitespace().last().unwrap();
                    changed = format!(
                        "{}{} {}{}",
                        &line[..argument.start()],
                        annotation,
                        name,
                        &line[argument.end()..]
                    );
                }
            }
        }
        if changed != line {
            edits.push((
                TextRange::new(
                    TextSize::new(starts[header.line] as u32),
                    TextSize::new((starts[header.line] + line.len()) as u32),
                ),
                changed,
            ));
        }
    }
    if let Some(Some(annotation)) = &facts.returns {
        let mut return_header = None;
        for (index, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with(":rtype:") || line.trim_start().starts_with("@rtype:")
            {
                let marker = line.find("rtype:").unwrap() + "rtype:".len();
                let changed = format!(
                    "{} {}{}",
                    &line[..marker],
                    annotation,
                    if line.ends_with('\n') { "\n" } else { "" }
                );
                if changed != *line {
                    edits.push((
                        TextRange::at(TextSize::new(starts[index] as u32), TextSize::of(*line)),
                        changed,
                    ));
                }
            }
            if GOOGLE_SECTION
                .captures(line)
                .is_some_and(|captures| matches!(&captures[2], "Returns" | "Return"))
            {
                return_header = Some((index, true));
            } else if matches!(line.trim(), "Returns" | "Return")
                && lines
                    .get(index + 1)
                    .is_some_and(|line| is_underline(line, '-'))
            {
                return_header = Some((index + 1, false));
            }
        }
        if let Some((header, google)) = return_header {
            if let Some((index, line)) = lines
                .iter()
                .enumerate()
                .skip(header + 1)
                .find(|(_, line)| !line.trim().is_empty())
            {
                let indent = indent_of(line);
                if google && indent > indent_of(lines[header]) {
                    // A typed Google return entry has `type: description`.
                    if let Some(colon) = top_colon(line) {
                        edits.push((
                            TextRange::at(
                                TextSize::new((starts[index] + indent) as u32),
                                TextSize::new((colon - indent) as u32),
                            ),
                            annotation.clone(),
                        ));
                    }
                } else if !google
                    && indent == indent_of(lines[header - 1])
                    && !NUMPY_SECTION.is_match(line)
                    && !matches!(line.trim(), "\"\"\"" | "'''")
                {
                    let start = top_colon(line).map_or(indent, |colon| {
                        colon + 1 + line[colon + 1..].len() - line[colon + 1..].trim_start().len()
                    });
                    edits.push((
                        TextRange::at(
                            TextSize::new((starts[index] + start) as u32),
                            TextSize::new(line.trim_end().len().saturating_sub(start) as u32),
                        ),
                        annotation.clone(),
                    ));
                }
            }
        }
    }
    // Apply edits in reverse source order; each header owns a disjoint block.
    edits.sort_by_key(|(range, _)| range.start());
    let mut replacement = raw.to_string();
    for (edit, text) in edits.into_iter().rev() {
        replacement.replace_range(usize::from(edit.start())..usize::from(edit.end()), &text);
    }
    let mut candidate = file.text.to_string();
    candidate.replace_range(
        usize::from(range.start())..usize::from(range.end()),
        &replacement,
    );
    let candidate = Arc::new(file.with_candidate_text(candidate));
    let format = detect_one(&doc.value());
    if format != DocstringFormat::Plain {
        if let Some((edit, text)) = update_with_options(db, &candidate, offset, format, options) {
            let mut text_file = candidate.text.to_string();
            text_file.replace_range(usize::from(edit.start())..usize::from(edit.end()), &text);
            let parsed = rustpy_syntax::parse(&text_file);
            let updated = func_at(&parsed.syntax_node(), offset)?.docstring()?;
            replacement = updated.syntax().text().to_string();
        }
    }
    (replacement != raw).then_some((range, replacement))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(source: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/doc-signature"));
        let id = db.set_file_text(Path::new("/doc-signature/module.py"), source);
        let file = db.file(id).unwrap();
        (db, file)
    }

    #[test]
    fn structured_name_ranges_include_epytext_and_exclude_prose_and_other_sections() {
        for raw in [
            "\"\"\":param café: café in prose\n    :type café: int\n    \"\"\"",
            "\"\"\"Summary.\n\n    @param café: café in prose\n    @type café: int\n    \"\"\"",
            "\"\"\"Summary.\n\n    :param int café: café in prose\n    :type café: int\n    \"\"\"",
        ] {
            let ranges = parameter_name_ranges(raw, "café");
            assert_eq!(ranges.len(), 2);
            for range in ranges {
                assert_eq!(
                    &raw[usize::from(range.start())..usize::from(range.end())],
                    "café"
                );
            }
        }
        let google = "\"\"\"Summary.\n\n    Args:\n        value: documented\n            value: description example\n    Attributes:\n        value: not a parameter\n    \"\"\"";
        assert_eq!(parameter_name_ranges(google, "value").len(), 1);
        let numpy = "\"\"\"Summary.\n\n    Parameters\n    ----------\n    first, second : int\n        value: description\n\n    Attributes\n    ----------\n    first : str\n    \"\"\"";
        assert_eq!(parameter_name_ranges(numpy, "first").len(), 1);
        assert_eq!(parameter_name_ranges(numpy, "second").len(), 1);
        assert!(parameter_name_ranges(numpy, "value").is_empty());
    }

    #[test]
    fn signature_sync_removes_retired_blocks_and_adds_fields_in_all_structured_styles() {
        for fields in [
            ":param old: retired prose\n    :type old: int\n    :param kept: retained prose\n    :return: retained return",
            "@param old: retired prose\n    @type old: int\n    @param kept: retained prose\n    @return: retained return",
            "Args:\n        old: retired prose\n        kept: retained prose\n\n    Returns:\n        int: retained return",
            "Parameters\n    ----------\n    old : int\n        retired prose\n    kept : int\n        retained prose\n\n    Returns\n    -------\n    int\n        retained return",
        ] {
            let source = format!("def f(kept: int, added: str):\n    \"\"\"Summary.\n\n    {fields}\n    \"\"\"\n    return kept\n");
            let (db, file) = fixture(&source);
            let (_, replacement) = synchronize_signature(
                &db, &file, TextSize::new(4), &["old".into(), "kept".into()], StubOptions::default(),
            ).unwrap();
            let parsed = parse(&replacement);
            assert!(replacement.contains("retained prose"), "{replacement}");
            assert!(replacement.contains("retained return"), "{replacement}");
            assert!(!replacement.contains("retired prose"), "{replacement}");
            assert_eq!(parsed.params.iter().map(|parameter| parameter.0.as_str()).collect::<Vec<_>>(), ["kept", "added"], "{replacement}");
            assert_eq!(file.text.as_ref(), source, "candidate does not publish source");
        }
    }

    #[test]
    fn numpy_group_removal_keeps_shared_description_and_return_annotation_updates() {
        let source = "def f(kept: float) -> str:\n    \"\"\"Summary.\n\n    Parameters\n    ----------\n    retired, kept : int\n        shared description\n    \"\"\"\n    return str(kept)\n";
        let (db, file) = fixture(source);
        let (_, replacement) = synchronize_signature(
            &db,
            &file,
            TextSize::new(4),
            &["retired".into(), "kept".into()],
            StubOptions::default(),
        )
        .unwrap();
        assert!(replacement.contains("kept : float"), "{replacement}");
        assert!(replacement.contains("shared description"));
        assert!(!replacement.contains("retired"));

        let source = "def f(value: str) -> bytes:\n    \"\"\"Summary.\n\n    :param value: retained\n    :type value: int\n    :return: response\n    :rtype: str\n    \"\"\"\n    return value.encode()\n";
        let (db, file) = fixture(source);
        let (_, replacement) = synchronize_signature(
            &db,
            &file,
            TextSize::new(4),
            &["value".into()],
            StubOptions::default(),
        )
        .unwrap();
        assert!(replacement.contains(":type value: str\n"));
        assert!(replacement.contains(":rtype: bytes\n"));
        assert!(replacement.contains(":return: response\n"));
    }

    #[test]
    fn removing_a_field_on_the_opening_quote_line_keeps_a_valid_docstring_literal() {
        let source = "def f(kept):\n    \"\"\":param retired: removed\n    :param kept: retained\n    \"\"\"\n    return kept\n";
        let (db, file) = fixture(source);
        let edit = synchronize_signature(
            &db,
            &file,
            TextSize::new(4),
            &["retired".into(), "kept".into()],
            StubOptions::default(),
        )
        .unwrap();
        let output = crate::edit_helpers::apply_edits(source, &[edit]);
        let parsed = rustpy_syntax::parse(&output);
        let function = func_at(&parsed.syntax_node(), TextSize::new(4)).unwrap();
        let doc = function.docstring().unwrap();
        assert!(doc.value().contains("retained"), "{output}");
        assert!(!doc.value().contains("retired"));
        assert!(output.ends_with("    return kept\n"));
    }

    #[test]
    fn return_type_option_does_not_require_parameter_placeholders_and_is_idempotent() {
        let source = "def f(value: int) -> str:\n    return str(value)\n";
        for format in [DocstringFormat::ReStructuredText, DocstringFormat::Epytext] {
            let (db, file) = fixture(source);
            let options = StubOptions {
                type_placeholders: false,
                return_type: true,
            };
            let edit =
                generate_with_options(&db, &file, TextSize::new(4), format, options).unwrap();
            assert!(edit.1.contains("rtype: str"));
            assert!(!edit.1.contains("type value:"));
            let output = crate::edit_helpers::apply_edits(source, &[edit]);
            let (db, file) = fixture(&output);
            assert!(update_with_options(&db, &file, TextSize::new(4), format, options).is_none());
        }
        let source = "def f(value: int) -> str:\n    \"\"\"Summary.\n\n    :param value: retained prose\n    \"\"\"\n    return str(value)\n";
        let (db, file) = fixture(source);
        let edit = update_with_options(
            &db,
            &file,
            TextSize::new(4),
            DocstringFormat::ReStructuredText,
            StubOptions {
                type_placeholders: false,
                return_type: true,
            },
        )
        .unwrap();
        assert!(edit.1.contains(":rtype: str"));
        assert!(edit.1.contains(":param value: retained prose"));
    }
    #[test]
    fn terminated_field_lines_update_types_without_replacing_descriptions() {
        for prefix in [":", "@"] {
            for newline in ["\n", "\r\n"] {
                let source = format!("def f(value: float) -> str:\n    \"\"\"Summary.\n\n    {prefix}param int value: retained inline description\n    {prefix}type value: int\n    \"\"\"\n    return str(value)\n").replace('\n', newline);
                let (db, file) = fixture(&source);
                let (_, replacement) = synchronize_signature(
                    &db,
                    &file,
                    TextSize::new(4),
                    &["value".into()],
                    StubOptions::default(),
                )
                .unwrap();
                assert!(
                    replacement.contains(&format!("{prefix}type value: float")),
                    "{replacement}"
                );
                assert!(
                    replacement.contains(&format!(
                        "{prefix}param float value: retained inline description"
                    )),
                    "{replacement}"
                );
            }
        }
    }
}
