//! Offline upstream-oracle assessment. This never invokes Python or Black.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
pub struct Manifest {
    pub revision: String,
    pub tag: String,
    pub files: Vec<Entry>,
}
#[derive(Deserialize)]
pub struct Entry {
    pub file: String,
    pub sha256: String,
    pub bytes: usize,
}
#[derive(Serialize)]
pub struct Assessment {
    pub revision: String,
    pub tag: String,
    pub total: usize,
    pub exact: usize,
    pub different: usize,
    pub refused: usize,
    pub unsupported: usize,
    pub unstable: usize,
    pub comments_changed: usize,
    pub cases: Vec<CaseResult>,
}
#[derive(Serialize)]
pub struct CaseResult {
    pub file: String,
    pub status: String,
    pub detail: String,
}
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/black_corpus")
}
pub fn verified_manifest() -> Result<Manifest, String> {
    let root = root();
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if manifest.revision != "6305bf1ae645ab7541be4f5028a86239316178eb"
        || manifest.files.len() != 197
    {
        return Err("unexpected upstream revision or incomplete inventory".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for entry in &manifest.files {
        if Path::new(&entry.file).components().count() != 1 || !names.insert(&entry.file) {
            return Err("invalid or duplicate corpus path".into());
        }
        let bytes =
            std::fs::read(root.join("cases").join(&entry.file)).map_err(|e| e.to_string())?;
        if bytes.len() != entry.bytes || format!("{:x}", Sha256::digest(&bytes)) != entry.sha256 {
            return Err(format!(
                "upstream fixture integrity mismatch: {}",
                entry.file
            ));
        }
    }
    let actual = std::fs::read_dir(root.join("cases"))
        .map_err(|e| e.to_string())?
        .count();
    if actual != names.len() {
        return Err("unmanifested corpus entries".into());
    }
    Ok(manifest)
}

/// Implements Black's tests/util.py input/output protocol, including whitespace
/// placeholder removal. Unsupported flags remain visible in the assessment.
pub fn fixture(text: &str) -> (Vec<String>, String, String) {
    let mut flags = Vec::new();
    let mut input = String::new();
    let mut output = String::new();
    let mut after = false;
    for line in text.split_inclusive('\n') {
        if input.is_empty() && !after {
            if let Some(raw) = line.strip_prefix("# flags: ") {
                flags.extend(raw.split_whitespace().map(str::to_owned));
                if !raw.contains("--line-ranges") {
                    continue;
                }
            }
        }
        if line.trim_end() == "# output" {
            after = true;
            continue;
        }
        let line = line.replace(
            "# EMPTY LINE WITH WHITESPACE (this comment will be removed)",
            "",
        );
        if after {
            output.push_str(&line);
        } else {
            input.push_str(&line);
        }
    }
    if output.is_empty() {
        output.clone_from(&input);
    }
    fn trim(s: &str) -> String {
        format!(
            "{}\n",
            s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        )
    }
    (flags, trim(&input), trim(&output))
}
pub struct FixtureMode {
    options: rustpy_format::FormatOptions,
    line_ranges: Option<Vec<(usize, usize)>>,
}
pub struct FormattedCase {
    pub text: String,
    mapped_ranges: Option<Vec<rustpy_syntax::TextRange>>,
}
impl FixtureMode {
    pub fn format(&self, source: &str) -> Result<FormattedCase, rustpy_format::FormatError> {
        let ranges = self
            .line_ranges
            .as_ref()
            .map(|ranges| {
                let lines = source.split_inclusive('\n').collect::<Vec<_>>();
                let starts = std::iter::once(0)
                    .chain(source.match_indices('\n').map(|(at, _)| at + 1))
                    .collect::<Vec<_>>();
                ranges
                    .iter()
                    .filter_map(|&(first, last)| {
                        if first > lines.len() {
                            return None;
                        }
                        Some((
                            starts[first - 1],
                            starts
                                .get(last.min(lines.len()))
                                .copied()
                                .unwrap_or(source.len()),
                        ))
                    })
                    .map(|(start, end)| {
                        let start = u32::try_from(start).map_err(|_| {
                            rustpy_format::FormatError::Unsafe(
                                "range fixture exceeds offset capacity".into(),
                            )
                        })?;
                        let end = u32::try_from(end).map_err(|_| {
                            rustpy_format::FormatError::Unsafe(
                                "range fixture exceeds offset capacity".into(),
                            )
                        })?;
                        Ok(rustpy_syntax::TextRange::new(start.into(), end.into()))
                    })
                    .collect::<Result<Vec<_>, rustpy_format::FormatError>>()
            })
            .transpose()?;
        self.format_owned(source, ranges.as_deref())
    }
    pub fn format_again(
        &self,
        previous: &FormattedCase,
    ) -> Result<FormattedCase, rustpy_format::FormatError> {
        self.format_owned(&previous.text, previous.mapped_ranges.as_deref())
    }
    fn format_owned(
        &self,
        source: &str,
        ranges: Option<&[rustpy_syntax::TextRange]>,
    ) -> Result<FormattedCase, rustpy_format::FormatError> {
        let (text, mapped_ranges) = if let Some(ranges) = ranges {
            let result = rustpy_format::format_logical_ranges(source, ranges, &self.options)?;
            let text = rustpy_syntax::printer::apply_text_edits(source, &result.edits)
                .map_err(|error| rustpy_format::FormatError::Unsafe(error.to_string()))?;
            (text, Some(result.mapped_ranges))
        } else {
            (rustpy_format::format_source(source, &self.options)?, None)
        };
        if rustpy_format::semantic_fingerprint(source).is_none()
            || rustpy_format::semantic_fingerprint(source)
                != rustpy_format::semantic_fingerprint(&text)
        {
            return Err(rustpy_format::FormatError::Unsafe(
                "fixture formatting changed syntax/meaning".into(),
            ));
        }
        Ok(FormattedCase {
            text,
            mapped_ranges,
        })
    }
}
pub fn options(flags: &[String]) -> Result<FixtureMode, String> {
    let mut options = rustpy_format::FormatOptions::black();
    let mut line_ranges = None;
    for flag in flags {
        if let Some(value) = flag.strip_prefix("--line-length=") {
            options.max_line_length = value.parse().map_err(|_| format!("invalid {flag}"))?;
        } else if let Some(value) = flag.strip_prefix("--line-ranges=") {
            let (first, last) = value
                .split_once('-')
                .ok_or_else(|| format!("invalid {flag}"))?;
            let first = first
                .parse::<usize>()
                .map_err(|_| format!("invalid {flag}"))?;
            let last = last
                .parse::<usize>()
                .map_err(|_| format!("invalid {flag}"))?;
            if first == 0 || last < first {
                return Err(format!("invalid {flag}"));
            }
            line_ranges.get_or_insert_with(Vec::new).push((first, last));
        } else if flag == "--skip-string-normalization" {
            options.quote = rustpy_format::QuoteStyle::Preserve;
        } else if flag == "--skip-magic-trailing-comma" {
            options.magic_trailing_comma = false;
        } else if flag == "--fast" || flag.starts_with("--minimum-version=") {
            // Runner metadata, not formatting options. RustPy retains safety checks.
        } else {
            return Err(format!("unsupported upstream fixture mode: {flag}"));
        }
    }
    Ok(FixtureMode {
        options,
        line_ranges,
    })
}
pub fn assess() -> Result<Assessment, String> {
    let manifest = verified_manifest()?;
    let mut report = Assessment {
        revision: manifest.revision,
        tag: manifest.tag,
        total: manifest.files.len(),
        exact: 0,
        different: 0,
        refused: 0,
        unsupported: 0,
        unstable: 0,
        comments_changed: 0,
        cases: Vec::new(),
    };
    for entry in manifest.files {
        let raw = std::fs::read_to_string(root().join("cases").join(&entry.file))
            .map_err(|e| e.to_string())?;
        let (flags, source, expected) = fixture(&raw);
        let (status, detail) = match options(&flags) {
            Err(reason) => {
                report.unsupported += 1;
                ("unsupported", reason)
            }
            Ok(options) => match options.format(&source) {
                Err(error) => {
                    report.refused += 1;
                    ("refused", error.to_string())
                }
                Ok(actual) => {
                    if !rustpy_format::comments_preserved(&source, &actual.text) {
                        report.comments_changed += 1;
                        (
                            "comments_changed",
                            "comment payload/multiplicity changed".into(),
                        )
                    } else if options
                        .format_again(&actual)
                        .map(|result| result.text)
                        .as_ref()
                        .ok()
                        != Some(&actual.text)
                    {
                        report.unstable += 1;
                        (
                            "unstable",
                            "second formatting pass differs or refuses".into(),
                        )
                    } else if actual.text == expected {
                        report.exact += 1;
                        ("exact", String::new())
                    } else {
                        report.different += 1;
                        let line = actual
                            .text
                            .lines()
                            .zip(expected.lines())
                            .position(|(a, b)| a != b)
                            .unwrap_or_else(|| {
                                actual.text.lines().count().min(expected.lines().count())
                            })
                            + 1;
                        (
                            "different",
                            format!(
                                "first difference at line {line}; actual {} bytes, expected {} bytes",
                                actual.text.len(),
                                expected.len()
                            ),
                        )
                    }
                }
            },
        };
        report.cases.push(CaseResult {
            file: entry.file,
            status: status.into(),
            detail,
        });
    }
    Ok(report)
}
