//! Reformat Code (Ctrl+Alt+L) for a document or a range: the builtin
//! formatter with the IDE's code style, the file's indentation (editor
//! options, detected indents, `.editorconfig`) and right margin — or the
//! external formatter chosen in Settings › Code Style (Black, Ruff, …).

use std::path::Path;

use lsp_types::{FormattingOptions, TextEdit};
use rustpy_core::{TextRange, TextSize};

use crate::line_index::{Encoding, LineIndex};
use crate::snapshot::{HResult, LspError, Snapshot};

/// The formatter options for `path` (mirrors `IdeApp::reformat_code`).
pub fn format_options(
    snap: &Snapshot,
    path: &Path,
    text: &str,
    client: &FormattingOptions,
) -> rustpy_format::FormatOptions {
    let settings = &snap.config.settings;
    let cs = &settings.code_style;
    let mut editor = settings.editor.clone();
    cs.apply_language(path, &mut editor);
    // Indentation layers: the editor's options (the client's tab size and
    // spaces), the file's detected indentation, then `.editorconfig`.
    let mut indent = if client.tab_size > 0 {
        client.tab_size as usize
    } else {
        editor.indent_size
    };
    let mut use_tabs = !client.insert_spaces;
    if editor.detect_indents {
        match rustpy_text::util::detect_indent(text, true) {
            Some(rustpy_text::util::IndentStyle::Spaces(n)) => {
                indent = n;
                use_tabs = false;
            }
            Some(rustpy_text::util::IndentStyle::Tabs) => use_tabs = true,
            None => {}
        }
    }
    let mut margin = editor.right_margin;
    let ec = if !crate::paths::is_virtual_stub(path) {
        editor.editorconfig_for(path)
    } else {
        Default::default()
    };
    if !crate::paths::is_virtual_stub(path) {
        if let Some(style) = &ec.indent_style {
            use_tabs = style.eq_ignore_ascii_case("tab");
        }
        if let Some(n) = ec.indent_size {
            indent = n;
        }
        if let Some(n) = ec.max_line_length {
            margin = n;
        }
    }

    let mut opts = rustpy_format::FormatOptions::pep8();
    opts.max_line_length = margin.max(40);
    opts.indent = indent.clamp(1, 16);
    opts.tabs = use_tabs.then_some(rustpy_format::TabIndent {
        width: ec.tab_width.unwrap_or(editor.tab_size).clamp(1, 16),
        smart: ec.ide.smart_tabs.unwrap_or(false),
    });
    if ec.max_line_length_off {
        opts.max_line_length = 16384;
    }
    if let Some(width) = ec.ide.continuation_indent_size {
        opts.continuation_indent = Some(rustpy_format::ContinuationIndent {
            width,
            parameters: ec.ide.continuation_parameters.unwrap_or(true),
            arguments: ec.ide.continuation_arguments.unwrap_or(true),
            collections: ec.ide.continuation_collections.unwrap_or(true),
        })
    }
    opts.quote = match cs.quote_style.as_str() {
        "double" => rustpy_format::QuoteStyle::Double,
        "single" => rustpy_format::QuoteStyle::Single,
        _ => rustpy_format::QuoteStyle::Preserve,
    };
    opts.magic_trailing_comma = cs.magic_trailing_comma;
    opts.blank_lines_top_level = cs.blank_lines_top_level;
    opts.blank_lines_in_class = cs.blank_lines_in_class;
    rustpy_format::style_policy::apply(&mut opts, cs, &ec);
    opts
}

/// Edits reformatting `range` of `text` (`None`: the whole document).
pub fn format(
    snap: &Snapshot,
    path: &Path,
    index: &LineIndex,
    range: Option<TextRange>,
    client: &FormattingOptions,
) -> HResult<Vec<TextEdit>> {
    let mut effective = snap.clone();
    let mut config = (*snap.config).clone();
    let root = snap
        .analysis
        .resolver
        .roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count());
    if let Some(root) = root {
        config
            .settings
            .load_project_code_style(root)
            .map_err(LspError::failed)?;
    }
    effective.config = std::sync::Arc::new(config);
    let snap = &effective;
    let text = index.text();
    let style = &snap.config.settings.code_style;
    if snap
        .config
        .settings
        .editor
        .editorconfig_for(path)
        .ide
        .formatter_enabled
        == Some(false)
    {
        return Ok(Vec::new());
    }
    let context = rustpy_settings::scopes::ScopeContext {
        root: root.cloned().unwrap_or_default(),
        project_files: snap.project_files(),
        ..Default::default()
    };
    if snap
        .config
        .settings
        .formatting_excluded(path, &context)
        .map_err(LspError::failed)?
    {
        return Ok(Vec::new());
    }
    if style.formatter != "builtin"
        && (style.python != rustpy_settings::python_style::PythonStyle::default()
            || style.reformat_scope != "all"
            || style.formatter_markers
            || style.keep_line_breaks
            || style.keep_simple_blocks
            || style.keep_first_column_comments)
    {
        return Err(LspError::failed(
            "Keep/marker preferences require the builtin formatter",
        ));
    }
    let formatter = snap.config.settings.code_style.formatter.as_str();
    if formatter != "builtin" && !formatter.is_empty() && range.is_none() {
        if let Some(formatted) = external_format(snap, formatter, text)? {
            return Ok(minimal_edits(text, &formatted, index, snap.enc));
        }
    }
    let mut opts = format_options(snap, path, text, client);
    if range.is_none() {
        opts.changed_lines = match style.reformat_scope.as_str() {
            "vcs" => {
                let git = rustpy_vcs::GitRepo::discover(path)
                    .ok_or_else(|| LspError::failed("VCS changed text requires Git"))?;
                Some(
                    git.line_changes(path, text)
                        .iter()
                        .map(|change| (change.start, change.count))
                        .collect(),
                )
            }
            "changed" => {
                let saved = std::fs::read_to_string(path).map_err(|_| {
                    LspError::failed("Changed-lines formatting requires a saved UTF-8 file")
                })?;
                Some(
                    rustpy_vcs::line_changes(
                        &rustpy_text::LineEnding::normalize_to_lf(&saved),
                        text,
                    )
                    .iter()
                    .map(|change| (change.start, change.count))
                    .collect(),
                )
            }
            _ => None,
        };
    }
    // The formatter works on `\n` text; documents with `\r\n` keep their line breaks.
    let crlf = text.contains('\r');
    if crlf {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let eol = if text.contains("\r\n") { "\r\n" } else { "\r" };
        if let Some(r) = range {
            // Only the lines of the range: map them through the normalized text.
            let n_range = TextRange::new(
                normalize_offset(text, r.start()),
                normalize_offset(text, r.end()),
            );
            let edits = rustpy_format::format_range(&normalized, n_range, &opts)
                .map_err(|e| LspError::failed(e.to_string()))?;
            let n_index = LineIndex::new(normalized.as_str());
            return Ok(edits
                .into_iter()
                .map(|(r, new)| {
                    // Same lines and columns in both texts (line breaks differ only).
                    let range = n_index.range(r, snap.enc);
                    TextEdit::new(range, new.replace('\n', eol))
                })
                .collect());
        }
        let formatted = rustpy_format::format_source(&normalized, &opts)
            .map_err(|e| LspError::failed(e.to_string()))?;
        return Ok(minimal_edits(
            text,
            &formatted.replace('\n', eol),
            index,
            snap.enc,
        ));
    }
    let whole = TextRange::new(TextSize::new(0), index.len());
    let edits = rustpy_format::format_range(text, range.unwrap_or(whole), &opts)
        .map_err(|e| LspError::failed(e.to_string()))?;
    Ok(edits
        .into_iter()
        .map(|(r, new)| TextEdit::new(index.range(r, snap.enc), new))
        .collect())
}

/// The offset in the `\n`-normalized text matching `off` of `text`.
fn normalize_offset(text: &str, off: TextSize) -> TextSize {
    let off = usize::from(off).min(text.len());
    let removed = text.as_bytes()[..off]
        .windows(2)
        .filter(|w| w == b"\r\n")
        .count();
    TextSize::new((off - removed) as u32)
}

/// One edit replacing the differing middle of `old` by that of `new`.
pub fn minimal_edits(old: &str, new: &str, index: &LineIndex, enc: Encoding) -> Vec<TextEdit> {
    if old == new {
        return Vec::new();
    }
    let (range, inserted) = rustpy_syntax::reparse::diff_edit(old, new);
    vec![TextEdit::new(index.range(range, enc), inserted.to_string())]
}

/// Runs Black / Ruff / autopep8 / yapf through the project interpreter, as
/// the IDE does (`python -m <formatter> …`, the text on stdin).
fn external_format(snap: &Snapshot, formatter: &str, text: &str) -> HResult<Option<String>> {
    use std::io::Write;
    // `python -m <tool>` must not pick up a module of that name from an untrusted project.
    let Some(python) = snap.interpreter.clone().filter(|_| snap.trusted) else {
        return Ok(None);
    };
    let mut args: Vec<String> = vec!["-m".into(), formatter.to_string()];
    match formatter {
        "black" => args.extend(["-q".into(), "-".into()]),
        "ruff" => args.extend(["format".into(), "-".into()]),
        "autopep8" => args.push("-".into()),
        _ => {}
    }
    args.extend(
        snap.config
            .settings
            .code_style
            .formatter_args
            .split_whitespace()
            .map(str::to_string),
    );
    let mut child = rustpy_core::interpreter_launch::interpreter_command(python.as_ref())
        .args(&args)
        .current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| LspError::failed(format!("cannot run {formatter}: {e}")))?;
    let mut stdin = child.stdin.take();
    let input = text.to_string();
    // Feed stdin from a thread so a formatter writing a lot of output cannot deadlock.
    let writer = std::thread::spawn(move || {
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(input.as_bytes());
        }
    });
    let out = child
        .wait_with_output()
        .map_err(|e| LspError::failed(format!("{formatter} failed: {e}")))?;
    let _ = writer.join();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(LspError::failed(format!(
            "{formatter} failed: {}",
            err.lines().last().unwrap_or("error")
        )));
    }
    let formatted = String::from_utf8_lossy(&out.stdout).into_owned();
    Ok((!formatted.is_empty()).then_some(formatted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_offsets_map_to_the_normalized_text() {
        let text = "a\r\nb\r\nc";
        assert_eq!(normalize_offset(text, TextSize::new(3)), TextSize::new(2));
        assert_eq!(normalize_offset(text, TextSize::new(7)), TextSize::new(5));
        let idx = LineIndex::new(text);
        let edits = minimal_edits(text, "a\r\nB\r\nc", &idx, Encoding::Utf16);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "B");
        assert_eq!(
            edits[0].range,
            lsp_types::Range::new(
                lsp_types::Position::new(1, 0),
                lsp_types::Position::new(1, 1)
            )
        );
    }
}
