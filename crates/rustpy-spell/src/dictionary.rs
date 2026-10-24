//! [`Dictionary`]: the bundled English word list plus user/project words.

use std::collections::BTreeMap;
use std::fmt;

use crate::bundled::bundled;
use crate::distance::{plausibility, prefix_coded, Entries, TrieWalker};
use crate::split::{
    contraction, find_apostrophe, match_case, split_identifier_ranges, Contraction,
};
use crate::vocab;

/// Maximum edit distance of [`Dictionary::suggest`] suggestions.
pub const MAX_SUGGESTION_DISTANCE: u32 = 2;

/// Queries longer than this (in chars) get no suggestions, and words with
/// apostrophes longer than this (in bytes) are not looked up whole.
const MAX_QUERY_LEN: usize = 64;

/// A set of known words.
///
/// [`Dictionary::bundled`] knows about 370,000 English words (from
/// dwyl/english-words), the identifier vocabulary of the Python standard
/// library (from typeshed), programming jargon, and Python keywords and
/// builtins. User and project words come on top
/// ([`with_user_words`](Self::with_user_words), [`add_word`](Self::add_word)).
///
/// Cloning is cheap apart from the user words: the bundled list is shared,
/// decoded lazily, once per process.
#[derive(Clone)]
pub struct Dictionary {
    bundled: bool,
    /// Lowercased user word → the spelling it was added with.
    user: BTreeMap<String, String>,
}

impl Dictionary {
    /// The bundled dictionary (English, standard-library names, jargon).
    pub fn bundled() -> Self {
        // Decode the list now rather than in the middle of the first check.
        let _ = bundled();
        Self {
            bundled: true,
            user: BTreeMap::new(),
        }
    }

    /// A dictionary without the bundled words; only words added to it are
    /// known (useful for project or language-specific word lists).
    pub fn empty() -> Self {
        Self {
            bundled: false,
            user: BTreeMap::new(),
        }
    }

    /// Adds user or project words (as loaded by [`crate::load_user_words`]).
    /// Lookups ignore case; suggestions keep the spelling of words that have
    /// capitals (`GitLab`). Blank entries and entries with whitespace are
    /// ignored.
    pub fn with_user_words(mut self, words: &[String]) -> Self {
        for word in words {
            self.add_word(word);
        }
        self
    }

    /// Adds one word ("Save to dictionary"); returns false when the word was
    /// already a user word or is not a single word.
    pub fn add_word(&mut self, word: &str) -> bool {
        let word = word.trim();
        if word.is_empty() || word.chars().any(char::is_whitespace) {
            return false;
        }
        let lower = word.to_lowercase();
        if self.user.contains_key(&lower) {
            return false;
        }
        self.user.insert(lower, word.to_string());
        true
    }

    /// Number of user words.
    pub fn user_word_count(&self) -> usize {
        self.user.len()
    }

    /// True when `word` is known, ignoring case. Identifiers are split into
    /// their words first ([`crate::split_identifier`]), so `getValue`,
    /// `HTTPServer` and `parse_json` are known when all their parts are.
    /// Contractions and possessives are understood (`isn't`, `user's`).
    /// Single letters count as known; strings without letters do not.
    pub fn contains(&self, word: &str) -> bool {
        // Apostrophes are taken one at a time, left to right (a loop rather
        // than recursion: `a'a'a'…` must not exhaust the stack).
        let mut word = word;
        while let Some((at, len)) = find_apostrophe(word) {
            // Only user words have apostrophes, and none is this long.
            if word.len() <= MAX_QUERY_LEN && self.contains_lower(&word.to_lowercase()) {
                return true;
            }
            let (head, tail) = (&word[..at], &word[at + len..]);
            if !head.is_empty() {
                match contraction(head, tail) {
                    Contraction::Suffix => return self.contains_plain(head),
                    Contraction::Negation => return self.negation_known(head),
                    Contraction::Other if !self.contains_plain(head) => return false,
                    Contraction::Other if tail.is_empty() => return true,
                    Contraction::Other => {}
                }
            }
            word = tail;
        }
        self.contains_plain(word)
    }

    /// [`Self::contains`] for a word without apostrophes.
    fn contains_plain(&self, word: &str) -> bool {
        if self.contains_lower(&word.to_lowercase()) {
            return true;
        }
        let parts = split_identifier_ranges(word);
        if parts.is_empty() || (parts.len() == 1 && parts[0].len() == word.len()) {
            return parts.len() == 1 && word.chars().count() == 1;
        }
        parts.into_iter().all(|range| {
            let part = &word[range];
            part.chars().count() == 1 || self.contains_lower(&part.to_lowercase())
        })
    }

    /// Exact lookup of an already-lowercased word.
    pub(crate) fn contains_lower(&self, lower: &str) -> bool {
        (self.bundled && (bundled().contains(lower) || vocab::is_known_word(lower)))
            || self.user.contains_key(lower)
    }

    /// True when `head` (the part of `isn't`, `can't` or `won't` before the
    /// apostrophe) is a known negation.
    pub(crate) fn negation_known(&self, head: &str) -> bool {
        let lower = head.to_lowercase();
        let stem = lower.strip_suffix('n').unwrap_or(&lower);
        matches!(lower.as_str(), "won" | "shan" | "ain")
            || self.contains_lower(&lower)
            || self.contains_lower(stem)
    }

    /// Up to `max` corrections for `word`: known words within a
    /// Damerau-Levenshtein (optimal string alignment) distance of 2, ranked by
    /// distance, then by how typical the typo is (doubled or swapped letters,
    /// vowel mix-ups and neighbouring keys first; no word frequencies are
    /// used), then alphabetically. Suggestions are re-cased like `word`
    /// (`Recieve` → `Receive`), except user words with capitals.
    pub fn suggest(&self, word: &str, max: usize) -> Vec<String> {
        let lower = word.to_lowercase();
        self.suggest_lower(&lower, max)
            .into_iter()
            .map(|s| recase(word, s))
            .collect()
    }

    /// [`Self::suggest`] for a lowercase word, without re-casing.
    pub(crate) fn suggest_lower(&self, lower: &str, max: usize) -> Vec<String> {
        let query: Vec<char> = lower.chars().collect();
        if max == 0 || query.is_empty() || query.len() > MAX_QUERY_LEN {
            return Vec::new();
        }
        // `(distance, word)` of every candidate. A word can be in several of
        // the lists (always at the same distance).
        let mut found: Vec<(u32, String)> = Vec::new();
        let mut record = |w: &[char], distance: u32| {
            if distance > 0 {
                found.push((distance, w.iter().collect()));
            }
        };
        let mut walker = TrieWalker::new(&query, MAX_SUGGESTION_DISTANCE);
        if self.bundled {
            walker.walk(bundled().cursor(), &mut record);
            walker.walk(
                Entries(prefix_coded(vocab::extra_words().iter().copied())),
                &mut record,
            );
        }
        walker.walk(
            Entries(prefix_coded(self.user.keys().map(String::as_str))),
            &mut record,
        );
        found.sort_unstable();
        found.dedup();
        // Only candidates up to the distance of the `max`-th one can be
        // suggested; the tie-breaker is computed for those alone.
        if let Some(&(cutoff, _)) = found.get(max - 1) {
            let end = found.partition_point(|&(distance, _)| distance <= cutoff);
            found.truncate(end);
        }
        let mut ranked: Vec<(u32, u32, String)> = found
            .into_iter()
            .map(|(distance, w)| {
                let candidate: Vec<char> = w.chars().collect();
                (distance, plausibility(&query, &candidate), w)
            })
            .collect();
        ranked.sort_unstable();
        ranked.truncate(max);
        ranked
            .into_iter()
            .map(|(_, _, w)| match self.user.get(&w) {
                Some(original) => original.clone(),
                None => w,
            })
            .collect()
    }
}

/// Re-cases a suggestion like the misspelled word, unless the suggestion has
/// its own capitals (a user word such as `GitLab`).
pub(crate) fn recase(template: &str, suggestion: String) -> String {
    if suggestion.chars().any(char::is_uppercase) {
        suggestion
    } else {
        match_case(template, &suggestion)
    }
}

impl Default for Dictionary {
    /// The bundled dictionary.
    fn default() -> Self {
        Self::bundled()
    }
}

impl fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dictionary")
            .field("bundled", &self.bundled)
            .field("user_words", &self.user.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distance::osa_distance;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn contains_ignores_case() {
        let d = Dictionary::bundled();
        for w in [
            "receive",
            "Receive",
            "RECEIVE",
            "the",
            "Python",
            "kwargs",
            "isinstance",
            "len",
            "getcwd",
        ] {
            assert!(d.contains(w), "{w}");
        }
        for w in ["recieve", "Recieve", "teh", "", "123", "__", "qwxzzk"] {
            assert!(!d.contains(w), "{w:?}");
        }
        // Misspellings that the source list has are dropped, including `creat`
        // (which typeshed's `O_CREAT` would bring back).
        for w in [
            "creat",
            "formating",
            "handel",
            "occured",
            "seperate",
            "untill",
        ] {
            assert!(!d.contains(w), "{w:?}");
        }
    }

    #[test]
    fn long_apostrophe_chains_do_not_recurse() {
        let chain = "a'".repeat(50_000) + "b";
        let d = Dictionary::bundled();
        assert!(d.contains(&chain));
        assert!(!d.contains(&(chain.clone() + "'recieve")));
        let src = format!("# {chain}'recieve\n");
        let typos = crate::check_text(&src, &d, crate::CheckOptions::default());
        assert_eq!(
            typos.iter().map(|t| t.word.as_str()).collect::<Vec<_>>(),
            ["recieve"]
        );
    }

    #[test]
    fn dictionary_is_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Dictionary>();
        let d = std::sync::Arc::new(Dictionary::bundled());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let d = std::sync::Arc::clone(&d);
                std::thread::spawn(move || d.suggest("recieve", 1))
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), ["receive"]);
        }
    }

    #[test]
    fn contains_splits_identifiers() {
        let d = Dictionary::bundled();
        for w in [
            "getValue",
            "HTTPServer",
            "parse_json",
            "MAX_RETRIES",
            "__init__",
            "iPhone",
            "x_value",
            "utf8_decode",
        ] {
            assert!(d.contains(w), "{w}");
        }
        for w in ["recieveData", "get_teh_value", "HTTPServr"] {
            assert!(!d.contains(w), "{w}");
        }
        assert!(d.contains("x"));
        assert!(d.contains("I"));
    }

    #[test]
    fn contains_understands_contractions() {
        let d = Dictionary::bundled();
        for w in [
            "isn't",
            "don't",
            "can't",
            "won't",
            "doesn't",
            "shouldn’t",
            "user's",
            "we're",
            "they'll",
            "I'm",
            "o'clock",
        ] {
            assert!(d.contains(w), "{w}");
        }
        for w in ["recieve's", "teh'll", "o'clcok"] {
            assert!(!d.contains(w), "{w}");
        }
    }

    #[test]
    fn user_words() {
        let mut d =
            Dictionary::bundled().with_user_words(&words(&["GitLab", "rustpy", " ", "two words"]));
        assert_eq!(d.user_word_count(), 2);
        assert!(d.contains("gitlab"));
        assert!(d.contains("GITLAB"));
        assert!(d.contains("RustPy"));
        assert!(d.contains("rustpyConfig"));
        assert!(!d.contains("frobnicator"));
        assert!(d.add_word("frobnicator"));
        assert!(!d.add_word("Frobnicator"));
        assert!(d.contains("Frobnicator"));
        // Suggestions keep the user's capitals.
        assert_eq!(d.suggest("gitlav", 1), ["GitLab"]);
        assert_eq!(d.suggest("Rustpi", 1), ["Rustpy"]);
    }

    #[test]
    fn empty_dictionary_only_knows_added_words() {
        let d = Dictionary::empty().with_user_words(&words(&["alpha", "beta"]));
        assert!(d.contains("Alpha"));
        assert!(d.contains("alphaBeta"));
        assert!(!d.contains("the"));
        assert!(!d.contains("kwargs"));
        assert_eq!(d.suggest("alpah", 5), ["alpha"]);
        assert_eq!(
            Dictionary::empty().suggest("alpah", 5),
            Vec::<String>::new()
        );
    }

    #[test]
    fn suggests_receive_for_recieve() {
        let d = Dictionary::bundled();
        let s = d.suggest("recieve", 5);
        assert_eq!(s.first().map(String::as_str), Some("receive"), "{s:?}");
        assert!(s.len() <= 5);
        assert_eq!(d.suggest("Recieve", 3)[0], "Receive");
        assert_eq!(d.suggest("RECIEVE", 3)[0], "RECEIVE");
    }

    #[test]
    fn suggestions_for_common_typos() {
        let d = Dictionary::bundled();
        for (typo, expected) in [
            ("teh", "the"),
            ("adress", "address"),
            ("untill", "until"),
            ("wich", "which"),
            ("lenght", "length"),
            ("seperate", "separate"),
            ("definately", "definitely"),
            ("occured", "occurred"),
            ("accomodate", "accommodate"),
            ("funtion", "function"),
            ("paramter", "parameter"),
            ("retrun", "return"),
            ("mesage", "message"),
            ("strnig", "string"),
            ("dependancy", "dependency"),
            ("kwrags", "kwargs"),
        ] {
            let s = d.suggest(typo, 5);
            assert_eq!(
                s.first().map(String::as_str),
                Some(expected),
                "{typo}: {s:?}"
            );
        }
    }

    #[test]
    fn suggestions_are_ranked_and_within_distance() {
        let d = Dictionary::bundled();
        for typo in ["recieve", "teh", "abcdefgh", "xq", "pythn"] {
            let s = d.suggest(typo, 50);
            let distances: Vec<u32> = s.iter().map(|w| osa_distance(typo, w)).collect();
            assert!(
                distances
                    .iter()
                    .all(|&dist| (1..=MAX_SUGGESTION_DISTANCE).contains(&dist)),
                "{typo}: {s:?}"
            );
            assert!(
                distances.windows(2).all(|p| p[0] <= p[1]),
                "{typo}: not ranked by distance: {s:?}"
            );
            assert!(s.iter().all(|w| d.contains(w)));
            assert!(!s.iter().any(|w| w == typo));
        }
        assert!(d.suggest("recieve", 0).is_empty());
        assert!(d.suggest("", 5).is_empty());
        assert!(d.suggest(&"x".repeat(100), 5).is_empty());
    }

    #[test]
    fn walker_finds_every_bundled_word_within_distance() {
        // Cross-check the pruned trie walk against a brute-force scan.
        let all = crate::bundled::bundled().words();
        let d = Dictionary::bundled();
        for typo in ["recieve", "teh", "zyzzyva"] {
            let mut expected: Vec<String> = all
                .iter()
                .filter(|w| {
                    w.len().abs_diff(typo.len()) <= 2
                        && (1..=MAX_SUGGESTION_DISTANCE).contains(&osa_distance(typo, w))
                })
                .cloned()
                .collect();
            for w in vocab::extra_words() {
                if (1..=MAX_SUGGESTION_DISTANCE).contains(&osa_distance(typo, w))
                    && !expected.iter().any(|e| e == w)
                {
                    expected.push(w.to_string());
                }
            }
            expected.sort();
            let mut got = d.suggest(typo, usize::MAX);
            got.sort();
            assert_eq!(got, expected, "{typo}");
        }
    }

    #[test]
    fn debug_output_is_compact() {
        let d = Dictionary::bundled().with_user_words(&words(&["one"]));
        assert_eq!(
            format!("{d:?}"),
            "Dictionary { bundled: true, user_words: 1 }"
        );
        assert!(Dictionary::default().contains("hello"));
    }
}
