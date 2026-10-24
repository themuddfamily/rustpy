//! Content-versioned word index used by whole-word Find in Files. Editor text
//! and disk text share the path entry, but reuse requires byte-identical text:
//! timestamp-preserving edits and unsaved buffers cannot hide new matches.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

struct Entry {
    text: Arc<str>,
    words: Vec<String>,
    bytes: usize,
}
#[derive(Default)]
struct Cache {
    files: HashMap<PathBuf, Arc<Entry>>,
    bytes: usize,
}
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_FILES: usize = 4096;
const MAX_WORDS: usize = 65_536;

pub(super) fn may_match(path: &Path, text: &str, query: &str, expression: &regex::Regex) -> bool {
    static WORD: OnceLock<regex::Regex> = OnceLock::new();
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let word = WORD.get_or_init(|| regex::Regex::new(r"\w+").expect("constant word expression"));
    // Substrings, punctuation and regexes require the full text matcher.
    if !word
        .find(query)
        .is_some_and(|found| found.range() == (0..query.len()))
    {
        return true;
    }
    let cache = CACHE.get_or_init(Default::default);
    let existing = {
        let cache = cache.lock().unwrap_or_else(|error| error.into_inner());
        cache
            .files
            .get(path)
            .filter(|entry| entry.text.as_ref() == text)
            .cloned()
    };
    let entry = match existing {
        Some(entry) => entry,
        None => {
            if text.len() > MAX_BYTES / 2 {
                return true;
            }
            let mut words: Vec<_> = word
                .find_iter(text)
                .take(MAX_WORDS + 1)
                .map(|found| found.as_str().to_owned())
                .collect();
            if words.len() > MAX_WORDS {
                return true;
            }
            words.sort_unstable();
            words.dedup();
            let bytes = text.len()
                + words
                    .iter()
                    .map(|word| word.len() + std::mem::size_of::<String>())
                    .sum::<usize>();
            if bytes > MAX_BYTES {
                return true;
            }
            let entry = Arc::new(Entry {
                text: Arc::from(text),
                words,
                bytes,
            });
            let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(previous) = cache.files.remove(path) {
                cache.bytes = cache.bytes.saturating_sub(previous.bytes);
            }
            // Bounded cache; clearing affects speed only, never match completeness.
            if cache.files.len() >= MAX_FILES || cache.bytes.saturating_add(bytes) > MAX_BYTES {
                cache.files.clear();
                cache.bytes = 0;
            }
            cache.bytes += bytes;
            cache.files.insert(path.to_path_buf(), entry.clone());
            entry
        }
    };
    // Use the actual query's Unicode case-folding policy, not ASCII lowercasing.
    entry.words.iter().any(|word| expression.is_match(word))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_word_index_rechecks_exact_editor_or_disk_content() {
        let path = Path::new("/word-index/current.py");
        let expression = regex::Regex::new(r"\bnew_name\b").unwrap();
        assert!(!may_match(path, "old_name=1", "new_name", &expression));
        assert!(may_match(path, "new_name=1", "new_name", &expression));
        assert!(!may_match(path, "old_name=1", "new_name", &expression));
    }
    #[test]
    fn unicode_case_folding_and_nonword_queries_remain_complete() {
        let expression = regex::RegexBuilder::new(r"\bs\b")
            .case_insensitive(true)
            .build()
            .unwrap();
        assert!(may_match(
            Path::new("/word-index/unicode.py"),
            "ſ",
            "s",
            &expression
        ));
        assert!(may_match(
            Path::new("/word-index/punctuation.py"),
            "anything",
            "a.b",
            &expression
        ));
    }
}
