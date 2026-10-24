//! RustPy IDE — Find in Files, file finder and fuzzy matching
//! (roadmap §2.7 "Find in Files", "Go to File", "Search Everywhere").
//!
//! Project-wide walks honor a [`ProjectScope`] (extra content roots and the
//! directories marked Excluded); [`FileIndex`] keeps the scope's file list
//! up to date on a background thread so popups never walk the disk on the
//! UI thread.

pub mod code_vision;
pub mod file_search;
mod index;
pub use file_search::ContextFilter;
pub mod structural;
pub mod usages;
mod words;

pub use index::FileIndex;
// Compatibility exports: implementation and policy belong to rustpy-project.
pub use rustpy_project::{
    files::{
        project_files, project_files_excluding, project_files_in, project_files_with_progress,
    },
    ProjectScope,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
#[cfg(test)]
use rustpy_core::ProgressReporter;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub regex: bool,
    /// File masks such as `*.py, !*test*` (empty = all text files).
    pub file_mask: String,
    pub max_results: usize,
    pub context: ContextFilter,
    pub multiline: bool,
    pub max_file_bytes: usize,
    pub include_generated: bool,
    pub include_excluded: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            whole_words: false,
            regex: false,
            file_mask: String::new(),
            max_results: 5000,
            context: ContextFilter::Anywhere,
            multiline: false,
            max_file_bytes: 4 * 1024 * 1024,
            include_generated: false,
            include_excluded: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub path: PathBuf,
    /// 0-based line index.
    pub line: usize,
    /// Byte columns within the line.
    pub start: usize,
    pub end: usize,
    pub line_text: String,
}

#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    pub files_searched: usize,
    pub truncated: bool,
}

/// Compiles the regex for a query under the given options.
pub fn build_regex(query: &str, opts: &SearchOptions) -> Result<regex::Regex, String> {
    let mut pattern = if opts.regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    if opts.whole_words {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!opts.case_sensitive)
        .multi_line(opts.multiline)
        .dot_matches_new_line(opts.multiline)
        .build()
        .map_err(|e| e.to_string())
}

/// Parses a file mask like `*.py, *.pyi, !*_test.py` into (include, exclude) globs.
fn parse_mask(mask: &str) -> (Vec<glob_like::Pattern>, Vec<glob_like::Pattern>) {
    let mut inc = Vec::new();
    let mut exc = Vec::new();
    for part in mask
        .split([',', ';'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if let Some(neg) = part.strip_prefix('!') {
            exc.push(glob_like::Pattern::new(neg));
        } else {
            inc.push(glob_like::Pattern::new(part));
        }
    }
    (inc, exc)
}

mod glob_like {
    /// Tiny glob: `*` and `?` only, matched against the file name (or path when it contains `/`).
    #[derive(Clone, Debug)]
    pub struct Pattern {
        text: String,
    }
    impl Pattern {
        pub fn new(s: &str) -> Self {
            Self {
                text: s.to_string(),
            }
        }
        pub fn matches(&self, name: &str) -> bool {
            fn m(p: &[char], s: &[char]) -> bool {
                match (p.first(), s.first()) {
                    (None, None) => true,
                    (Some('*'), _) => m(&p[1..], s) || (!s.is_empty() && m(p, &s[1..])),
                    (Some('?'), Some(_)) => m(&p[1..], &s[1..]),
                    (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && m(&p[1..], &s[1..]),
                    _ => false,
                }
            }
            let p: Vec<char> = self.text.chars().collect();
            let s: Vec<char> = name.chars().collect();
            m(&p, &s)
        }
        pub fn has_slash(&self) -> bool {
            self.text.contains('/')
        }
    }
}

fn is_text_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    const BINARY: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "bmp", "ico", "pdf", "zip", "gz", "tar", "xz", "7z", "exe",
        "dll", "so", "dylib", "pyc", "pyo", "whl", "ttf", "otf", "woff", "woff2", "mp3", "mp4",
        "wav", "db", "sqlite", "class", "jar", "o", "a", "lock",
    ];
    !BINARY.contains(&ext.as_str())
}

/// What a search reads besides the files on disk.
#[derive(Clone, Debug, Default)]
pub struct SearchSources {
    pub ignored_patterns: String,
    /// Editor text by path, searched instead of the file on disk (so unsaved
    /// changes are found and match positions agree with the editor).
    pub overrides: HashMap<PathBuf, String>,
    /// Directories that are never searched (marked *Excluded*).
    pub excluded: Vec<PathBuf>,
    /// Search exactly these files (the *Open Files* scope) instead of walking the root.
    pub only: Option<Vec<PathBuf>>,
    /// Folders walked besides the root (content roots attached to the project).
    pub content_roots: Vec<PathBuf>,
    pub scope_filter: Option<file_search::CapturedScope>,
}

/// Cancellation flag and progress counters of a running search, shared with
/// the thread that displays it.
#[derive(Debug, Default)]
pub struct SearchControl {
    pub cancel: AtomicBool,
    pub files_total: AtomicUsize,
    pub files_done: AtomicUsize,
}

impl SearchControl {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// How a finished (or cancelled) search went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchSummary {
    pub files_searched: usize,
    pub matches: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

/// `text`'s lines with the byte offset where each starts (line endings are
/// stripped like [`str::lines`] does).
pub fn lines_with_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut start = 0;
    text.split_inclusive('\n').map(move |seg| {
        let off = start;
        start += seg.len();
        let line = match seg.strip_suffix('\n') {
            Some(l) => l.strip_suffix('\r').unwrap_or(l),
            None => seg,
        };
        (off, line)
    })
}

/// File-name filter for the file mask (and binary extensions).
fn mask_filter(root: &Path, opts: &SearchOptions) -> impl Fn(&Path) -> bool {
    let (inc, exc) = parse_mask(&opts.file_mask);
    let root = root.to_path_buf();
    move |p: &Path| {
        if !is_text_file(p) {
            return false;
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let rel = p
            .strip_prefix(&root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/");
        let ok_inc = inc.is_empty()
            || inc.iter().any(|g| {
                if g.has_slash() {
                    g.matches(&rel)
                } else {
                    g.matches(name)
                }
            });
        let ok_exc = !exc.iter().any(|g| {
            if g.has_slash() {
                g.matches(&rel)
            } else {
                g.matches(name)
            }
        });
        ok_inc && ok_exc
    }
}

/// Find in Files over editor-or-disk text: walks `root` and
/// `sources.content_roots` (or searches `sources.only`), skips excluded
/// directories, prefers `sources.overrides` to the disk and hands matches to
/// `sink` one file at a time as they are found (from worker threads, in no
/// particular order; see [`sort_matches`]). `control` cancels the search and
/// counts progress.
pub fn find_in_files_with(
    root: &Path,
    query: &str,
    opts: &SearchOptions,
    sources: &SearchSources,
    control: &SearchControl,
    sink: &(dyn Fn(Vec<SearchMatch>) + Sync),
) -> Result<SearchSummary, String> {
    search_impl(root, query, opts, sources, control, None, sink)
}

/// Searches in the background scan pool ([`rustpy_core::parallel`]): a
/// long search never holds up parallel work the UI thread waits for.
fn search_impl(
    root: &Path,
    query: &str,
    opts: &SearchOptions,
    sources: &SearchSources,
    control: &SearchControl,
    extra_cancel: Option<&AtomicBool>,
    sink: &(dyn Fn(Vec<SearchMatch>) + Sync),
) -> Result<SearchSummary, String> {
    rustpy_core::parallel::in_background_pool(|| {
        search_here(root, query, opts, sources, control, extra_cancel, sink)
    })
}

fn search_here(
    root: &Path,
    query: &str,
    opts: &SearchOptions,
    sources: &SearchSources,
    control: &SearchControl,
    extra_cancel: Option<&AtomicBool>,
    sink: &(dyn Fn(Vec<SearchMatch>) + Sync),
) -> Result<SearchSummary, String> {
    if query.is_empty() {
        return Ok(SearchSummary::default());
    }
    let re = build_regex(query, opts)?;
    let keep = mask_filter(root, opts);
    let mut scope = ProjectScope::new(root)
        .with_content_roots(sources.content_roots.iter().cloned())
        .with_excluded(
            sources
                .excluded
                .iter()
                .filter(|_| !opts.include_excluded)
                .cloned(),
        );
    scope.ignored_patterns = sources.ignored_patterns.clone();
    let mut files: Vec<PathBuf> = match &sources.only {
        Some(only) => {
            let mut files = Vec::new();
            for path in only {
                if control.is_cancelled() {
                    break;
                }
                if path.is_dir() {
                    let nested = {
                        let mut nested =
                            ProjectScope::new(path).with_excluded(scope.excluded.iter().cloned());
                        nested.ignored_patterns = scope.ignored_patterns.clone();
                        nested
                    };
                    let found = if opts.include_generated {
                        file_search::expanded_files(&nested, opts, &|| control.is_cancelled())
                    } else {
                        rustpy_project::files::project_files_in_with_cancel(&nested, &|| {
                            control.is_cancelled()
                        })
                    };
                    files.extend(found);
                } else {
                    files.push(path.clone());
                }
            }
            files.retain(|path| keep(path) && !scope.is_excluded(path));
            files
        }
        None => {
            let files = if opts.include_generated {
                file_search::expanded_files(&scope, opts, &|| control.is_cancelled())
            } else {
                rustpy_project::files::project_files_in_with_cancel(&scope, &|| {
                    control.is_cancelled()
                })
            };
            files.into_iter().filter(|p| keep(p)).collect()
        }
    };
    if sources.only.is_none() {
        // Open editors whose file is not on disk (any more) are searched too.
        let mut extra: Vec<PathBuf> = sources
            .overrides
            .keys()
            .filter(|p| scope.contains(p) && keep(p) && !p.exists())
            .cloned()
            .collect();
        extra.sort();
        files.extend(extra);
    }
    if let Some(filter) = &sources.scope_filter {
        let mut context = filter.context.clone();
        if context.project_files.is_empty() {
            let mut project: Vec<_> = files
                .iter()
                .filter(|path| {
                    path.starts_with(&context.root)
                        || context.modules.values().any(|root| path.starts_with(root))
                })
                .cloned()
                .collect();
            project.sort();
            project.dedup();
            context.project_files = Arc::new(project);
        }
        files.retain(|path| filter.compiled.matches(path, &context));
    }
    files.sort();
    files.dedup();
    control.files_total.store(files.len(), Ordering::Relaxed);
    let cancelled =
        || control.is_cancelled() || extra_cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let count = AtomicUsize::new(0);
    let truncated = AtomicBool::new(false);
    let max = opts.max_results;
    files.par_iter().for_each(|path| {
        if cancelled() || truncated.load(Ordering::Relaxed) {
            return;
        }
        let disk;
        let text: &str = match sources.overrides.get(path) {
            Some(t) => t,
            None => match file_search::read_text(path, opts.max_file_bytes.min(64 * 1024 * 1024)) {
                Some(text) => {
                    disk = text;
                    &disk
                }
                None => {
                    control.files_done.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            },
        };
        if text.len() > opts.max_file_bytes.min(64 * 1024 * 1024) {
            control.files_done.fetch_add(1, Ordering::Relaxed);
            return;
        }

        if opts.whole_words
            && !opts.regex
            && !opts.multiline
            && !words::may_match(path, text, query, &re)
        {
            control.files_done.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut local = file_search::matches(path, text, &re, opts);
        control.files_done.fetch_add(1, Ordering::Relaxed);
        if !local.is_empty() {
            let n = count.fetch_add(local.len(), Ordering::Relaxed);
            if n + local.len() > max {
                truncated.store(true, Ordering::Relaxed);
                local.truncate(max.saturating_sub(n));
            }
            if !local.is_empty() {
                sink(local);
            }
        }
    });
    Ok(SearchSummary {
        files_searched: files.len(),
        matches: count.load(Ordering::Relaxed).min(max),
        truncated: truncated.load(Ordering::Relaxed),
        cancelled: cancelled(),
    })
}

/// Orders matches by file, line and column (streamed batches arrive unordered).
pub fn sort_matches(matches: &mut [SearchMatch]) {
    matches.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.line.cmp(&b.line))
            .then(a.start.cmp(&b.start))
    });
}

/// Searches `root` for `query` (parallel over files). `cancel` may be set to stop early.
pub fn find_in_files(
    root: &Path,
    query: &str,
    opts: &SearchOptions,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<SearchResults, String> {
    collect_search(
        root,
        query,
        opts,
        &SearchSources::default(),
        cancel.as_deref(),
    )
}

/// Searches the files of `scope` for `query`: content roots are searched and
/// excluded directories are skipped.
pub fn find_in_files_in(
    scope: &ProjectScope,
    query: &str,
    opts: &SearchOptions,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<SearchResults, String> {
    let sources = SearchSources {
        excluded: scope.excluded.clone(),
        ignored_patterns: scope.ignored_patterns.clone(),
        content_roots: scope.content_roots.clone(),
        ..Default::default()
    };
    collect_search(&scope.root, query, opts, &sources, cancel.as_deref())
}

/// Searches exactly `files` (for example a [`FileIndex`] list) for `query`
/// without walking the disk. File masks that contain `/` match the path
/// relative to `root`.
pub fn find_in_files_among(
    root: &Path,
    files: &[PathBuf],
    query: &str,
    opts: &SearchOptions,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<SearchResults, String> {
    let sources = SearchSources {
        only: Some(files.to_vec()),
        ..Default::default()
    };
    collect_search(root, query, opts, &sources, cancel.as_deref())
}

/// Runs a search to the end and returns its matches in order.
fn collect_search(
    root: &Path,
    query: &str,
    opts: &SearchOptions,
    sources: &SearchSources,
    cancel: Option<&AtomicBool>,
) -> Result<SearchResults, String> {
    let control = SearchControl::default();
    let results: Mutex<Vec<SearchMatch>> = Mutex::new(Vec::new());
    let sink = |batch: Vec<SearchMatch>| results.lock().unwrap().extend(batch);
    let summary = search_impl(root, query, opts, sources, &control, cancel, &sink)?;
    let mut matches = results.into_inner().unwrap();
    sort_matches(&mut matches);
    Ok(SearchResults {
        matches,
        files_searched: summary.files_searched,
        truncated: summary.truncated,
    })
}

/// The text that replaces the match of `re` starting at byte `start` of
/// `line`: `replacement` with `$1`/`${name}` groups expanded in regex mode,
/// verbatim otherwise.
pub fn replacement_text(
    re: &regex::Regex,
    line: &str,
    start: usize,
    replacement: &str,
    regex_mode: bool,
) -> String {
    if !regex_mode {
        return replacement.to_string();
    }
    match re.captures_at(line, start) {
        Some(caps) => {
            let mut out = String::new();
            caps.expand(replacement, &mut out);
            out
        }
        None => replacement.to_string(),
    }
}

/// Replace in Files edits for `text`: `(byte range, new text)` for each match
/// found line by line exactly as [`find_in_files_with`] reports them, minus
/// the ones `keep(line, start_column)` rejects (occurrences excluded in the
/// preview).
pub fn replace_edits(
    text: &str,
    query: &str,
    replacement: &str,
    opts: &SearchOptions,
    keep: &dyn Fn(usize, usize) -> bool,
) -> Result<Vec<(std::ops::Range<usize>, String)>, String> {
    let re = build_regex(query, opts)?;
    let mut out = Vec::new();
    let starts = file_search::line_starts(text);
    for range in file_search::ranges(text, &re, opts) {
        let line = starts
            .partition_point(|start| *start <= range.start)
            .saturating_sub(1);
        let line_start = starts[line];
        if !keep(line, range.start - line_start) {
            continue;
        }
        let replacement = if opts.multiline {
            replacement_text(&re, text, range.start, replacement, opts.regex)
        } else {
            let line = text[line_start..]
                .split('\n')
                .next()
                .unwrap_or("")
                .trim_end_matches('\r');
            replacement_text(&re, line, range.start - line_start, replacement, opts.regex)
        };
        out.push((range, replacement));
    }
    Ok(out)
}

/// Applies non-overlapping byte-range edits to `text`.
pub fn apply_edits(text: &str, edits: &[(std::ops::Range<usize>, String)]) -> String {
    let mut sorted: Vec<&(std::ops::Range<usize>, String)> = edits.iter().collect();
    sorted.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    let mut out = text.to_string();
    for (r, rep) in sorted {
        if r.end <= out.len() && out.is_char_boundary(r.start) && out.is_char_boundary(r.end) {
            out.replace_range(r.clone(), rep);
        }
    }
    out
}

/// Replaces all matches of `query` in `path`'s content with `replacement`
/// (regex groups allowed). Matches are found line by line, exactly as Find in
/// Files reports them: `^` and `$` are line boundaries and no match spans a
/// line break.
pub fn replace_in_file(
    path: &Path,
    query: &str,
    replacement: &str,
    opts: &SearchOptions,
) -> Result<usize, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let edits = replace_edits(&text, query, replacement, opts, &|_, _| true)?;
    if edits.is_empty() {
        return Ok(0);
    }
    std::fs::write(path, apply_edits(&text, &edits)).map_err(|e| e.to_string())?;
    Ok(edits.len())
}

// ---------------------------------------------------------------------------
// Fuzzy matching (Go to File / Class / Symbol, Search Everywhere)

/// Scores how well `query` matches `candidate` (higher is better); `None` = no match.
/// Supports prefix, substring, camel-hump (`fb` → `FooBar`) and subsequence matching.
pub fn fuzzy_score(candidate: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    // Symbol searches score hundreds of thousands of names per keystroke:
    // ASCII text (nearly all of it) is scored on bytes, without allocating.
    if candidate.is_ascii() && query.is_ascii() {
        return fuzzy_score_ascii(candidate.as_bytes(), query.as_bytes());
    }
    let cand: Vec<char> = candidate.chars().collect();
    let q: Vec<char> = query.chars().collect();
    let lower_c = candidate.to_lowercase();
    let lower_q = query.to_lowercase();
    if lower_c == lower_q {
        return Some(1000);
    }
    if lower_c.starts_with(&lower_q) {
        return Some(800 - candidate.len() as i32);
    }
    // Substring match, by character (byte offsets of the lowercased text do
    // not index `cand` once a name has non-ASCII characters).
    let lc: Vec<char> = cand
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let lq: Vec<char> = q
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    if let Some(pos) = lc.windows(lq.len()).position(|w| w == lq.as_slice()) {
        let boundary = pos == 0
            || !cand[pos - 1].is_alphanumeric()
            || (cand[pos].is_uppercase() && cand[pos - 1].is_lowercase());
        return Some(if boundary { 600 } else { 400 } - pos as i32 - candidate.len() as i32 / 4);
    }
    // Subsequence with hump bonuses.
    let mut qi = 0;
    let mut score = 0i32;
    let mut prev_matched = false;
    for (i, c) in cand.iter().enumerate() {
        if qi < q.len() && c.to_lowercase().next() == q[qi].to_lowercase().next() {
            let is_start = i == 0
                || !cand[i - 1].is_alphanumeric()
                || (c.is_uppercase() && !cand[i - 1].is_uppercase());
            score += if is_start {
                30
            } else if prev_matched {
                15
            } else {
                3
            };
            if c.is_uppercase() == q[qi].is_uppercase() {
                score += 2;
            }
            qi += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
    }
    if qi == q.len() {
        Some(score - candidate.len() as i32 / 8)
    } else {
        None
    }
}

/// [`fuzzy_score`] for ASCII text (same scores).
fn fuzzy_score_ascii(cand: &[u8], q: &[u8]) -> Option<i32> {
    let len = cand.len() as i32;
    if cand.len() >= q.len() {
        if cand[..q.len()].eq_ignore_ascii_case(q) {
            return Some(if cand.len() == q.len() {
                1000
            } else {
                800 - len
            });
        }
        if let Some(pos) = (1..=cand.len() - q.len()).find(|&p| {
            cand[p].eq_ignore_ascii_case(&q[0]) && cand[p..p + q.len()].eq_ignore_ascii_case(q)
        }) {
            let boundary = !cand[pos - 1].is_ascii_alphanumeric()
                || (cand[pos].is_ascii_uppercase() && cand[pos - 1].is_ascii_lowercase());
            return Some(if boundary { 600 } else { 400 } - pos as i32 - len / 4);
        }
    }
    let mut qi = 0;
    let mut score = 0i32;
    let mut prev_matched = false;
    for (i, &c) in cand.iter().enumerate() {
        if qi < q.len() && c.eq_ignore_ascii_case(&q[qi]) {
            let is_start = i == 0
                || !cand[i - 1].is_ascii_alphanumeric()
                || (c.is_ascii_uppercase() && !cand[i - 1].is_ascii_uppercase());
            score += if is_start {
                30
            } else if prev_matched {
                15
            } else {
                3
            };
            if c.is_ascii_uppercase() == q[qi].is_ascii_uppercase() {
                score += 2;
            }
            qi += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
    }
    (qi == q.len()).then(|| score - len / 8)
}

/// Finds files matching `query`; `query` may contain `/` (path match) and a
/// trailing `:line[:col]`.
pub fn find_files(root: &Path, query: &str, limit: usize) -> Vec<(PathBuf, i32)> {
    find_files_among(root, &project_files(root), query, limit)
}

/// [`find_files`] over the files of `scope` (excluded directories are skipped).
pub fn find_files_in(scope: &ProjectScope, query: &str, limit: usize) -> Vec<(PathBuf, i32)> {
    find_files_among(&scope.root, &project_files_in(scope), query, limit)
}

/// [`find_files`] over a cached file list (no disk access); paths inside
/// `root` are matched relative to it.
pub fn find_files_among(
    root: &Path,
    files: &[PathBuf],
    query: &str,
    limit: usize,
) -> Vec<(PathBuf, i32)> {
    let (q, _) = split_line_suffix(query);
    let q = q.trim();
    let mut scored: Vec<(PathBuf, i32)> = files
        .par_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/");
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let s = if q.contains('/') {
                navigation_score(&rel, q)
            } else {
                navigation_score(name, q).or_else(|| navigation_score(&rel, q).map(|s| s - 200))
            }?;
            Some((p.clone(), s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(limit);
    scored
}

/// File and symbol navigation patterns retain ordinary camel-hump scoring.
/// Wildcards match the entire name/path, with bounded dynamic programming.
pub fn navigation_score(candidate: &str, query: &str) -> Option<i32> {
    if !query.contains(['*', '?']) {
        return fuzzy_score(candidate, query);
    }
    if candidate.len() > 4096 || query.len() > 256 {
        return None;
    }
    let value: Vec<_> = candidate.to_lowercase().chars().collect();
    let mut row = vec![false; value.len() + 1];
    row[0] = true;
    for token in query.to_lowercase().chars() {
        let mut next = vec![false; row.len()];
        if token == '*' {
            next[0] = row[0];
        }
        for i in 1..row.len() {
            next[i] = if token == '*' {
                row[i] || next[i - 1]
            } else {
                row[i - 1] && (token == '?' || token == value[i - 1])
            };
        }
        row = next;
    }
    row[value.len()].then_some(600)
}

/// Splits `name:12:3` into (`name`, Some((12, Some(3)))).
pub fn split_line_suffix(query: &str) -> (&str, Option<(usize, Option<usize>)>) {
    let mut parts = query.splitn(3, ':');
    let name = parts.next().unwrap_or("");
    let line = parts.next().and_then(|l| l.trim().parse::<usize>().ok());
    let col = parts.next().and_then(|c| c.trim().parse::<usize>().ok());
    match line {
        Some(l) => (name, Some((l, col))),
        None => (query, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_navigation_and_line_suffix_use_cached_file_authority() {
        let root = Path::new("/project");
        let files = vec![
            root.join("src/UserModel.py"),
            root.join("src/UserView.py"),
            root.join("other.txt"),
        ];
        let wildcard = find_files_among(root, &files, "src/*Model.py:12:3", 10);
        assert_eq!(wildcard.len(), 1);
        assert_eq!(wildcard[0].0, files[0]);
        assert!(navigation_score("UserModel", "UM").is_some());
        assert!(navigation_score("UserModel.py", "UserM?del.*").is_some());
        assert!(navigation_score("UserView.py", "*Model.py").is_none());
        assert_eq!(
            split_line_suffix("src/UserModel.py:12:3"),
            ("src/UserModel.py", Some((12, Some(3))))
        );
    }

    #[test]
    fn fuzzy_score_handles_non_ascii_names() {
        // The substring position used to be a byte offset into the lowercased text.
        assert!(fuzzy_score("größe.py", "py").is_some());
        assert!(fuzzy_score("größe", "e").is_some());
        assert!(fuzzy_score("ÄÖÜ_helper", "helper").is_some());
        assert!(fuzzy_score("İstanbul", "bul").is_some());
        assert!(fuzzy_score("größe", "xyz").is_none());
    }

    #[test]
    fn finds_matches_with_options() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
        std::fs::write(
            dir.path().join("a.py"),
            "def Hello():\n    hello = 1\n    return hello_world\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("pkg/b.py"), "HELLO = 'x'\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello there\n").unwrap();
        let r = find_in_files(dir.path(), "hello", &SearchOptions::default(), None).unwrap();
        assert_eq!(r.matches.len(), 5);
        let r = find_in_files(
            dir.path(),
            "hello",
            &SearchOptions {
                case_sensitive: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(r.matches.len(), 3);
        let r = find_in_files(
            dir.path(),
            "hello",
            &SearchOptions {
                whole_words: true,
                case_sensitive: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(r.matches.len(), 2);
        let r = find_in_files(
            dir.path(),
            "hel+o",
            &SearchOptions {
                regex: true,
                file_mask: "*.py".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(r.matches.len(), 4);
        let r = find_in_files(
            dir.path(),
            "hello",
            &SearchOptions {
                file_mask: "*.py, !pkg/*".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(r.matches.len(), 3);
        assert_eq!(
            replace_in_file(
                &dir.path().join("notes.txt"),
                "hello",
                "bye",
                &SearchOptions::default()
            )
            .unwrap(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "bye there\n"
        );
    }

    fn collect(
        root: &Path,
        query: &str,
        opts: &SearchOptions,
        sources: &SearchSources,
    ) -> (Vec<SearchMatch>, SearchSummary, usize) {
        let control = SearchControl::default();
        let batches = Mutex::new(Vec::new());
        let sink = |b: Vec<SearchMatch>| batches.lock().unwrap().push(b);
        let summary = find_in_files_with(root, query, opts, sources, &control, &sink).unwrap();
        let batches = batches.into_inner().unwrap();
        let n_batches = batches.len();
        let mut all: Vec<SearchMatch> = batches.into_iter().flatten().collect();
        sort_matches(&mut all);
        assert_eq!(
            control.files_done.load(Ordering::Relaxed),
            control.files_total.load(Ordering::Relaxed),
            "every file is counted"
        );
        (all, summary, n_batches)
    }

    #[test]
    fn searches_unsaved_buffers_streams_per_file_and_skips_excluded_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("gen")).unwrap();
        std::fs::write(root.join("a.py"), "needle = 1\n").unwrap();
        std::fs::write(root.join("b.py"), "nothing here\n").unwrap();
        std::fs::write(root.join("gen/out.py"), "needle()\n").unwrap();
        // Disk only: three files searched, two contain the needle.
        let (m, s, batches) = collect(
            root,
            "needle",
            &SearchOptions::default(),
            &SearchSources::default(),
        );
        assert_eq!((m.len(), s.files_searched, batches), (2, 3, 2), "{m:?}");
        // The editor buffer of b.py has an unsaved needle; a.py's buffer lost it.
        let mut sources = SearchSources::default();
        sources
            .overrides
            .insert(root.join("b.py"), "x = 0\nprint(needle)\n".into());
        sources
            .overrides
            .insert(root.join("a.py"), "haystack = 1\n".into());
        sources.excluded.push(root.join("gen"));
        let (m, s, _) = collect(root, "needle", &SearchOptions::default(), &sources);
        assert_eq!(s.files_searched, 2, "the excluded folder is not walked");
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(
            (
                m[0].path.file_name().unwrap().to_str().unwrap(),
                m[0].line,
                m[0].start
            ),
            ("b.py", 1, 6)
        );
        // An open editor whose file is not on disk is searched too.
        sources
            .overrides
            .insert(root.join("new.py"), "needle\n".into());
        assert_eq!(
            collect(root, "needle", &SearchOptions::default(), &sources)
                .0
                .len(),
            2
        );
        // `only`: just the listed files (Open Files scope).
        let only = SearchSources {
            only: Some(vec![root.join("gen/out.py")]),
            ..Default::default()
        };
        assert_eq!(
            collect(root, "needle", &SearchOptions::default(), &only)
                .0
                .len(),
            1
        );
        assert!(project_files_excluding(root, &[root.join("gen")])
            .iter()
            .all(|p| !p.starts_with(root.join("gen"))));
        // A cancelled search reports so and stops early.
        let control = SearchControl::default();
        control.cancel();
        let s = find_in_files_with(
            root,
            "needle",
            &SearchOptions::default(),
            &SearchSources::default(),
            &control,
            &|_| panic!("no results after cancel"),
        )
        .unwrap();
        assert!(s.cancelled);
    }

    #[test]
    fn replace_edits_expand_groups_and_skip_excluded_occurrences() {
        let text = "foo(1)\r\nbar = foo(2) + foo(3)\nfoo";
        let lines: Vec<(usize, &str)> = lines_with_offsets(text).collect();
        assert_eq!(
            lines,
            vec![(0, "foo(1)"), (8, "bar = foo(2) + foo(3)"), (30, "foo")]
        );
        let opts = SearchOptions {
            regex: true,
            case_sensitive: true,
            ..Default::default()
        };
        // Exclude the second occurrence on line 1 (column 15).
        let edits = replace_edits(text, r"foo\((\d)\)", "baz($1, 0)", &opts, &|line, col| {
            !(line == 1 && col == 15)
        })
        .unwrap();
        assert_eq!(
            edits,
            vec![
                (0..6, "baz(1, 0)".to_string()),
                (14..20, "baz(2, 0)".to_string())
            ]
        );
        assert_eq!(
            apply_edits(text, &edits),
            "baz(1, 0)\r\nbar = baz(2, 0) + foo(3)\nfoo"
        );
        // Literal mode never expands `$1`.
        let edits =
            replace_edits(text, "foo", "$1", &SearchOptions::default(), &|_, _| true).unwrap();
        assert_eq!(edits.len(), 4);
        assert!(edits.iter().all(|(_, t)| t == "$1"));
        let re = build_regex(r"(\w+)=(\w+)", &opts).unwrap();
        assert_eq!(replacement_text(&re, "a=b c=d", 4, "$2=$1", true), "d=c");
    }

    #[test]
    fn zero_width_replacements_preserve_lines_unicode_and_exclusions() {
        let opts = SearchOptions {
            regex: true,
            ..Default::default()
        };
        for ending in ["\n", "\r\n"] {
            let text = format!("alpha{ending}βeta{ending}");
            let edits = replace_edits(&text, "^", "start_", &opts, &|_, _| true).unwrap();
            assert_eq!(edits.len(), 2);
            assert_eq!(
                apply_edits(&text, &edits),
                format!("start_alpha{ending}start_βeta{ending}")
            );
            let edits = replace_edits(&text, "$", "_end", &opts, &|_, _| true).unwrap();
            assert_eq!(edits.len(), 2);
            assert_eq!(
                apply_edits(&text, &edits),
                format!("alpha_end{ending}βeta_end{ending}")
            );
            let edits = replace_edits(&text, "^", "start_", &opts, &|line, _| line != 0).unwrap();
            assert_eq!(
                apply_edits(&text, &edits),
                format!("alpha{ending}start_βeta{ending}")
            );
        }
        let edits = replace_edits("é中", "()", "${1}_", &opts, &|_, _| true).unwrap();
        assert_eq!(apply_edits("é中", &edits), "_é_中_");
        let edits = replace_edits("a\n\nb", "^|$", "|", &opts, &|_, _| true).unwrap();
        assert_eq!(apply_edits("a\n\nb", &edits), "|a|\n|\n|b|");
    }

    /// The general (char-based) scorer, without the ASCII fast path.
    fn fuzzy_score_chars(candidate: &str, query: &str) -> Option<i32> {
        // A non-ASCII suffix that no query matches keeps `fuzzy_score` off the fast path;
        // the scores are then corrected for the extra bytes and characters.
        let padded = format!("{candidate}\u{e9}");
        let extra = padded.len() as i32 - candidate.len() as i32;
        let s = fuzzy_score(&padded, query)?;
        let lower_c = candidate.to_lowercase();
        let lower_q = query.to_lowercase();
        let len = candidate.len() as i32;
        let plen = padded.len() as i32;
        Some(if lower_c == lower_q {
            1000
        } else if lower_c.starts_with(&lower_q) {
            s + extra
        } else if lower_c.contains(&lower_q) {
            s + plen / 4 - len / 4
        } else {
            s + plen / 8 - len / 8
        })
    }

    #[test]
    fn ascii_fast_path_scores_like_the_general_scorer() {
        // Deterministic pseudo-random names and queries over a small alphabet,
        // so that prefixes, substrings, humps and subsequences all occur.
        let alphabet = b"aAbBcCdD_.x9/";
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let mut checked = 0;
        for _ in 0..20_000 {
            let clen = 1 + next(12);
            let qlen = 1 + next(4);
            let cand: String = (0..clen)
                .map(|_| alphabet[next(alphabet.len())] as char)
                .collect();
            let query: String = (0..qlen)
                .map(|_| alphabet[next(alphabet.len())] as char)
                .collect();
            if query.contains('\u{e9}') {
                continue;
            }
            assert_eq!(
                fuzzy_score(&cand, &query),
                fuzzy_score_chars(&cand, &query),
                "{cand:?} / {query:?}"
            );
            checked += 1;
        }
        assert!(checked > 10_000);
        for (c, q) in [
            ("getattr", "gtattr"),
            ("JSONDecoder", "jsondecoder"),
            ("JSONDecoder", "Decoder"),
            ("decode_hook", "dec"),
            ("FooBar", "fb"),
            ("main.py", "mp"),
            ("a", "a"),
            ("ab", "abc"),
            ("x_y", "_y"),
        ] {
            assert_eq!(fuzzy_score(c, q), fuzzy_score_chars(c, q), "{c:?} / {q:?}");
        }
    }

    #[test]
    fn replace_in_file_replaces_what_find_in_files_finds() {
        // `^` matches at every line start and `\s+` never joins lines, like the
        // preview (the whole-file replacement used to touch only line 1 and
        // could swallow line breaks).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        let text = "import os\nimport sys\r\nx = 1  \n  import json\nimport re";
        std::fs::write(&path, text).unwrap();
        let opts = SearchOptions {
            regex: true,
            case_sensitive: true,
            ..Default::default()
        };
        let found = find_in_files(dir.path(), "^import", &opts, None)
            .unwrap()
            .matches
            .len();
        assert_eq!(found, 3);
        assert_eq!(
            replace_in_file(&path, "^import", "IMPORT", &opts).unwrap(),
            found
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "IMPORT os\nIMPORT sys\r\nx = 1  \n  import json\nIMPORT re"
        );
        assert_eq!(
            replace_in_file(&path, r"\s+$", "", &opts).unwrap(),
            1,
            "trailing blanks of one line only"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "IMPORT os\nIMPORT sys\r\nx = 1\n  import json\nIMPORT re"
        );
        assert_eq!(
            replace_in_file(&path, r"(\w+)$", "<$1>", &opts).unwrap(),
            5,
            "groups expand at each line end"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "IMPORT <os>\nIMPORT <sys>\r\nx = <1>\n  import <json>\nIMPORT <re>"
        );
        assert_eq!(replace_in_file(&path, "nothing", "x", &opts).unwrap(), 0);
    }

    #[test]
    fn project_walks_stop_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            std::fs::write(dir.path().join(format!("m{i}.py")), "x = 1\n").unwrap();
        }
        std::fs::create_dir_all(dir.path().join("skip")).unwrap();
        std::fs::write(dir.path().join("skip/hidden.py"), "y = 2\n").unwrap();
        let live = ProgressReporter::new();
        let excluded = vec![dir.path().join("skip")];
        let mut walked = project_files_with_progress(dir.path(), &excluded, &live);
        let mut plain = project_files_excluding(dir.path(), &excluded);
        walked.sort();
        plain.sort();
        assert_eq!(walked, plain);
        assert_eq!(walked.len(), 50, "the excluded folder is left out");
        assert_eq!(
            project_files_with_progress(dir.path(), &[], &live).len(),
            51
        );
        let cancelled = ProgressReporter::new();
        cancelled.token().cancel();
        assert!(project_files_with_progress(dir.path(), &[], &cancelled).is_empty());
    }

    #[test]
    fn fuzzy_and_file_finder() {
        assert!(fuzzy_score("FooBar", "fb").is_some());
        assert!(fuzzy_score("FooBar", "foo").unwrap() > fuzzy_score("BarFoo", "foo").unwrap());
        assert!(fuzzy_score("main.py", "mp").is_some());
        assert_eq!(fuzzy_score("abc", "x"), None);
        assert_eq!(
            split_line_suffix("main.py:12:3"),
            ("main.py", Some((12, Some(3))))
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/util")).unwrap();
        std::fs::write(dir.path().join("src/util/helpers.py"), "").unwrap();
        std::fs::write(dir.path().join("src/main.py"), "").unwrap();
        let found = find_files(dir.path(), "help", 10);
        assert_eq!(found[0].0.file_name().unwrap(), "helpers.py");
        let found = find_files(dir.path(), "util/hel", 10);
        assert_eq!(found.len(), 1);
        let found = find_files(dir.path(), "mai:5", 10);
        assert_eq!(found[0].0.file_name().unwrap(), "main.py");
    }

    /// A project with vendored code in an excluded folder and an attached sibling folder.
    fn scoped_project() -> (tempfile::TempDir, ProjectScope) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        for d in ["src", "vendor/lib", "vendorx"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("shared")).unwrap();
        std::fs::write(root.join("src/main.py"), "import helpers\nhelpers.run()\n").unwrap();
        std::fs::write(
            root.join("vendor/lib/helpers.py"),
            "def run(): pass  # helpers\n",
        )
        .unwrap();
        std::fs::write(root.join("vendorx/helpers_x.py"), "helpers = 1\n").unwrap();
        std::fs::write(dir.path().join("shared/helpers_shared.py"), "helpers = 2\n").unwrap();
        let scope = ProjectScope::new(&root)
            .with_excluded([root.join("vendor")])
            .with_content_roots([dir.path().join("shared"), root.join("src")]);
        (dir, scope)
    }

    #[test]
    fn excluded_folders_are_not_listed_or_searched() {
        let (dir, scope) = scoped_project();
        let root = scope.root.clone();
        let files = project_files_in(&scope);
        let mut expected = vec![
            root.join("src/main.py"),
            root.join("vendorx/helpers_x.py"),
            dir.path().join("shared/helpers_shared.py"),
        ];
        expected.sort();
        assert_eq!(files, expected);
        assert!(scope.is_excluded(&root.join("vendor/lib/helpers.py")));
        assert!(
            !scope.is_excluded(&root.join("vendorx/helpers_x.py")),
            "prefix match is per path component"
        );
        assert_eq!(
            scope.walk_roots(),
            vec![root.clone(), dir.path().join("shared")],
            "a content root inside the root is walked once"
        );
        // Without the scope the vendored file is found.
        assert_eq!(project_files(&root).len(), 3);

        let r = find_in_files_in(&scope, "helpers", &SearchOptions::default(), None).unwrap();
        let hit_files: std::collections::BTreeSet<_> =
            r.matches.iter().map(|m| m.path.clone()).collect();
        assert!(
            !hit_files.contains(&root.join("vendor/lib/helpers.py")),
            "{hit_files:?}"
        );
        assert!(hit_files.contains(&root.join("src/main.py")));
        assert!(
            hit_files.contains(&dir.path().join("shared/helpers_shared.py")),
            "content roots are searched"
        );
        assert!(find_in_files_in(
            &scope,
            "(",
            &SearchOptions {
                regex: true,
                ..Default::default()
            },
            None
        )
        .is_err());

        let found: Vec<PathBuf> = find_files_in(&scope, "helpers", 10)
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert!(
            !found.contains(&root.join("vendor/lib/helpers.py")),
            "{found:?}"
        );
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn cached_lists_need_no_walk() {
        let (_dir, scope) = scoped_project();
        let root = scope.root.clone();
        // A cached list may name files that no longer exist: scoring never touches the disk.
        let cached = vec![
            root.join("src/main.py"),
            root.join("gone/helpers_cached.py"),
        ];
        let found = find_files_among(&root, &cached, "helpers", 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, root.join("gone/helpers_cached.py"));
        let found = find_files_among(&root, &cached, "gone/hel", 10);
        assert_eq!(found.len(), 1);
        let r = find_in_files_among(&root, &cached, "helpers", &SearchOptions::default(), None)
            .unwrap();
        assert_eq!(r.files_searched, 2);
        assert_eq!(r.matches.len(), 2, "only main.py is readable");
        let cancel = Arc::new(AtomicBool::new(true));
        let r = find_in_files_among(
            &root,
            &cached,
            "helpers",
            &SearchOptions::default(),
            Some(cancel),
        )
        .unwrap();
        assert!(r.matches.is_empty(), "a cancelled search stops");
        assert_eq!(
            scope.filter(&[root.join("vendor/x.py"), root.join("src/main.py")]),
            vec![root.join("src/main.py")]
        );
    }
}
