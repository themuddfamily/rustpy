//! Shared lexical/multiline match authority for search and replacement.
use super::*;
use rustpy_syntax::SyntaxKind;
use std::io::Read;

#[derive(Clone, Debug)]
pub struct CapturedScope {
    pub compiled: rustpy_settings::scopes::CompiledScope,
    pub context: rustpy_settings::scopes::ScopeContext,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextFilter {
    #[default]
    Anywhere,
    Comments,
    Strings,
    ExceptComments,
    ExceptStrings,
    Code,
}
impl ContextFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::Anywhere => "Anywhere",
            Self::Comments => "In comments",
            Self::Strings => "In string literals",
            Self::ExceptComments => "Except comments",
            Self::ExceptStrings => "Except string literals",
            Self::Code => "Except strings and comments",
        }
    }
}

pub fn read_text(path: &Path, limit: usize) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > limit || bytes.iter().take(8000).any(|byte| *byte == 0) {
        return None;
    }
    if matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("py" | "pyi")
    ) {
        rustpy_text::Encoding::decode_python_source(&bytes).ok()
    } else {
        rustpy_text::Encoding::decode(&bytes)
            .ok()
            .map(|(text, _)| text)
    }
}

/// Byte ranges always refer to the full original text, including CRLF bytes.
pub fn ranges(
    text: &str,
    regex: &regex::Regex,
    options: &SearchOptions,
) -> Vec<std::ops::Range<usize>> {
    let mut regions = Vec::new();
    if options.context != ContextFilter::Anywhere {
        for token in rustpy_syntax::parse(text)
            .syntax_node()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
        {
            let kind = match token.kind() {
                SyntaxKind::COMMENT => 1,
                SyntaxKind::STRING | SyntaxKind::FSTRING_MIDDLE | SyntaxKind::TSTRING_MIDDLE => 2,
                _ => continue,
            };
            regions.push((
                usize::from(token.text_range().start())..usize::from(token.text_range().end()),
                kind,
            ));
        }
    }
    let accepts = |range: &std::ops::Range<usize>| {
        let overlap = |region: &std::ops::Range<usize>| {
            if range.is_empty() {
                region.contains(&range.start)
            } else {
                range.start < region.end && region.start < range.end
            }
        };
        let inside = |kind| {
            regions[regions.partition_point(|(region, _)| region.end <= range.start)..]
                .iter()
                .take_while(|(region, _)| region.start <= range.end)
                .any(|(region, value)| {
                    *value == kind
                        && region.start <= range.start
                        && range.end <= region.end
                        && overlap(region)
                })
        };
        let touches = |kind| {
            regions[regions.partition_point(|(region, _)| region.end <= range.start)..]
                .iter()
                .take_while(|(region, _)| region.start <= range.end)
                .any(|(region, value)| *value == kind && overlap(region))
        };
        match options.context {
            ContextFilter::Anywhere => true,
            ContextFilter::Comments => inside(1),
            ContextFilter::Strings => inside(2),
            ContextFilter::ExceptComments => !touches(1),
            ContextFilter::ExceptStrings => !touches(2),
            ContextFilter::Code => !touches(1) && !touches(2),
        }
    };
    let mut out = Vec::new();
    if options.multiline {
        for found in regex.find_iter(text) {
            let range = found.range();
            if accepts(&range) {
                out.push(range);
            }
            if out.len() > options.max_results {
                break;
            }
        }
    } else {
        for (offset, line) in lines_with_offsets(text) {
            for found in regex.find_iter(line) {
                let range = offset + found.start()..offset + found.end();
                if accepts(&range) {
                    out.push(range);
                }
                if out.len() > options.max_results {
                    break;
                }
            }
            if out.len() > options.max_results {
                break;
            }
        }
    }
    out
}

pub fn matches(
    path: &Path,
    text: &str,
    regex: &regex::Regex,
    options: &SearchOptions,
) -> Vec<SearchMatch> {
    let starts = line_starts(text);
    ranges(text, regex, options)
        .into_iter()
        .map(|range| {
            let line = starts
                .partition_point(|start| *start <= range.start)
                .saturating_sub(1);
            let start = starts[line];
            let end = text[range.end..]
                .find('\n')
                .map_or(text.len(), |offset| range.end + offset);
            SearchMatch {
                path: path.into(),
                line,
                start: range.start - start,
                end: range.end - start,
                line_text: text[start..end].trim_end_matches('\r').into(),
            }
        })
        .collect()
}

/// Explicit opt-in walk for ignored/generated folders; VCS internals and symlinks stay excluded.
pub(super) fn expanded_files(
    scope: &ProjectScope,
    options: &SearchOptions,
    cancelled: &impl Fn() -> bool,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in scope.walk_roots() {
        let mut queue = vec![(root, 0usize)];
        while let Some((directory, depth)) = queue.pop() {
            if cancelled() || out.len() >= 100_000 {
                break;
            }
            if depth > 24 {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                if cancelled() || out.len() >= 100_000 {
                    break;
                }
                let path = entry.path();
                if !options.include_excluded && scope.is_excluded(&path) {
                    continue;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_symlink() {
                    continue;
                }
                if kind.is_dir() {
                    if !matches!(entry.file_name().to_str(), Some(".git" | ".hg" | ".svn")) {
                        queue.push((path, depth + 1));
                    }
                } else if kind.is_file() {
                    out.push(path);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

pub(super) fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            text.bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_filters_keep_code_separate_from_strings_and_comments() {
        let text = "needle = 'needle' # needle\nother = f'{needle} needle'\n";
        for (context, count) in [
            (ContextFilter::Anywhere, 5),
            (ContextFilter::Comments, 1),
            (ContextFilter::Strings, 2),
            (ContextFilter::Code, 2),
        ] {
            let options = SearchOptions {
                context,
                ..Default::default()
            };
            let regex = build_regex("needle", &options).unwrap();
            assert_eq!(ranges(text, &regex, &options).len(), count, "{context:?}");
        }
    }

    #[test]
    fn multiline_groups_and_exclusion_share_exact_crlf_byte_ranges() {
        let text = "head\r\nπ = 1\r\nnext = 2\r\nend\r\n";
        let options = SearchOptions {
            regex: true,
            multiline: true,
            ..Default::default()
        };
        let query = r"π = (?P<a>1)\r\nnext = (?P<b>2)";
        let found = matches(
            Path::new("a.py"),
            text,
            &build_regex(query, &options).unwrap(),
            &options,
        );
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].line, found[0].start), (1, 0));
        let edits = replace_edits(
            text,
            query,
            "π = $b\r\nnext = $a",
            &options,
            &|line, col| line == 1 && col == 0,
        )
        .unwrap();
        assert_eq!(
            apply_edits(text, &edits),
            "head\r\nπ = 2\r\nnext = 1\r\nend\r\n"
        );
        assert!(replace_edits(text, query, "x", &options, &|_, _| false)
            .unwrap()
            .is_empty());
        let single = SearchOptions {
            multiline: false,
            ..options
        };
        assert!(ranges(text, &build_regex(query, &single).unwrap(), &single).is_empty());
    }

    #[test]
    fn file_limits_masks_and_explicit_ignored_excluded_opt_ins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generated")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "generated/\n").unwrap();
        std::fs::write(dir.path().join("small.py"), "needle\n").unwrap();
        std::fs::write(dir.path().join("small_test.py"), "needle\n").unwrap();
        std::fs::write(dir.path().join("generated/out.py"), "needle\n").unwrap();
        let options = SearchOptions {
            file_mask: "*.py,!*test*".into(),
            max_file_bytes: 4,
            ..Default::default()
        };
        assert!(find_in_files(dir.path(), "needle", &options, None)
            .unwrap()
            .matches
            .is_empty());
        let options = SearchOptions {
            max_file_bytes: 1024,
            include_generated: true,
            include_excluded: true,
            ..options
        };
        let sources = SearchSources {
            excluded: vec![dir.path().join("generated")],
            ..Default::default()
        };
        let control = SearchControl::default();
        let output = Mutex::new(Vec::new());
        find_in_files_with(
            dir.path(),
            "needle",
            &options,
            &sources,
            &control,
            &|batch| output.lock().unwrap().extend(batch),
        )
        .unwrap();
        let result = output.into_inner().unwrap();
        assert_eq!(result.len(), 2);
        assert!(result
            .iter()
            .any(|hit| hit.path.ends_with("generated/out.py")));
        assert!(!result.iter().any(|hit| hit.path.ends_with("small_test.py")));
    }

    #[test]
    fn literal_line_and_multiline_search_have_different_newline_contracts() {
        let text = "one\ntwo\n";
        let ordinary = SearchOptions::default();
        assert!(ranges(
            text,
            &build_regex("one\ntwo", &ordinary).unwrap(),
            &ordinary
        )
        .is_empty());
        let multiline = SearchOptions {
            multiline: true,
            ..ordinary
        };
        assert_eq!(
            ranges(
                text,
                &build_regex("one\ntwo", &multiline).unwrap(),
                &multiline
            ),
            vec![0..7]
        );
    }
    #[test]
    fn shared_project_scope_uses_worker_catalogue_when_index_is_pending() {
        use rustpy_settings::scopes::{
            PredefinedScope, ScopeContext, ScopeSelection, ScopeSettings,
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.py"), "needle\n").unwrap();
        let settings = ScopeSettings::default();
        let sources = SearchSources {
            scope_filter: Some(CapturedScope {
                compiled: settings
                    .compile(&ScopeSelection::Predefined(PredefinedScope::Project))
                    .unwrap(),
                context: ScopeContext {
                    root: dir.path().into(),
                    ..Default::default()
                },
            }),
            ..Default::default()
        };
        let output = Mutex::new(Vec::new());
        find_in_files_with(
            dir.path(),
            "needle",
            &SearchOptions::default(),
            &sources,
            &SearchControl::default(),
            &|batch| output.lock().unwrap().extend(batch),
        )
        .unwrap();
        assert_eq!(output.into_inner().unwrap().len(), 1);
    }
}
