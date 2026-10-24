//! Edit distances for suggestions.
//!
//! * [`TrieWalker`] computes the optimal-string-alignment distance (the
//!   restricted Damerau-Levenshtein distance: insertions, deletions,
//!   substitutions and adjacent transpositions all cost 1) between a query
//!   and every word of a *sorted* word list. Consecutive words share the DP
//!   rows of their common prefix, and a prefix whose row already exceeds the
//!   maximum distance prunes every word that starts with it, so a lookup only
//!   visits a small part of the implicit trie.
//! * [`plausibility`] is a frequency-free tie-breaker between candidates at the
//!   same distance: a weighted alignment cost that makes the edits people
//!   actually type (doubled letters, swapped letters, vowel mix-ups,
//!   neighbouring keys, dropped letters) cheaper than arbitrary ones.

/// A sorted word list, read as `(chars shared with the previous word, rest
/// of the word)` entries.
pub(crate) trait WordSource<'a> {
    /// The next entry that shares fewer than `below` chars with its
    /// predecessor: the entries in between start with the prefix of length
    /// `below` of the last word returned, which cannot lead to a match.
    fn next_below(&mut self, below: usize) -> Option<(usize, &'a str)>;
}

/// A [`WordSource`] over an iterator of entries (see [`prefix_coded`]).
pub(crate) struct Entries<I>(pub(crate) I);

impl<'a, I: Iterator<Item = (usize, &'a str)>> WordSource<'a> for Entries<I> {
    fn next_below(&mut self, below: usize) -> Option<(usize, &'a str)> {
        self.0.find(|&(shared, _)| shared < below)
    }
}

/// Walks sorted word lists, reporting the words within `max` edits of `query`.
pub(crate) struct TrieWalker<'q> {
    query: &'q [char],
    max: u32,
    /// Row `d` (at `d * width`) holds the distances between the first `d`
    /// characters of `word` and the prefixes of the query. Only the band of
    /// cells `j` with `|d - j| <= max` is computed (a cell outside it is
    /// always above `max`); the cells just outside the band hold `max + 1`.
    rows: Vec<u32>,
    width: usize,
    word: Vec<char>,
}

impl<'q> TrieWalker<'q> {
    pub(crate) fn new(query: &'q [char], max: u32) -> Self {
        let width = query.len() + 1;
        // Words longer than the query plus `max` are never within range, so
        // there are at most that many rows after row 0.
        let mut rows = vec![0; width * (width + max as usize)];
        for (j, cell) in rows[..width].iter_mut().enumerate() {
            *cell = j as u32;
        }
        Self {
            query,
            max,
            rows,
            width,
            word: Vec::with_capacity(24),
        }
    }

    /// Feeds one sorted word list and calls `found(word, distance)` for every
    /// word within the maximum distance.
    pub(crate) fn walk<'a>(
        &mut self,
        mut source: impl WordSource<'a>,
        found: &mut impl FnMut(&[char], u32),
    ) {
        self.word.clear();
        let query_len = self.width - 1;
        // Length of a prefix that cannot lead to a match: the source skips the
        // entries that share it.
        let mut dead = usize::MAX;
        while let Some((shared, rest)) = source.next_below(dead) {
            dead = usize::MAX;
            // Rows up to `shared` still describe the shared prefix.
            self.word.truncate(shared.min(self.word.len()));
            let mut alive = true;
            for c in rest.chars() {
                self.word.push(c);
                if !self.push_row() {
                    dead = self.word.len();
                    alive = false;
                    break;
                }
            }
            let depth = self.word.len();
            if alive && depth > 0 && depth.abs_diff(query_len) <= self.max as usize {
                let distance = self.rows[depth * self.width + query_len];
                if distance <= self.max {
                    found(&self.word, distance);
                }
            }
        }
    }

    /// Computes the DP row for the last character of `word`; returns false
    /// when every entry of the row exceeds the maximum distance. No longer
    /// word can then come back within range: a transposition reaches two rows
    /// back, but that cell plus one is never below the minimum of this row.
    fn push_row(&mut self) -> bool {
        let width = self.width;
        let query_len = width - 1;
        let max = self.max as usize;
        let outside = self.max + 1;
        let depth = self.word.len();
        let (lo, hi) = (depth.saturating_sub(max), (depth + max).min(query_len));
        if lo > query_len {
            return false;
        }
        // `depth <= query_len + max` here, which `new` sized `rows` for.
        let start = depth * width;
        let c = self.word[depth - 1];
        let (done, rest) = self.rows.split_at_mut(start);
        let row = &mut rest[..width];
        let prev = &done[start - width..];
        let query = self.query;
        let mut row_min = outside;
        if lo == 0 {
            row[0] = depth as u32;
            row_min = row[0];
        } else {
            row[lo - 1] = outside;
        }
        if hi < query_len {
            row[hi + 1] = outside;
        }
        if depth >= 2 {
            let before_c = self.word[depth - 2];
            let prev2 = &done[start - 2 * width..start - width];
            for j in lo.max(1)..=hi {
                let q = query[j - 1];
                let mut best = (prev[j] + 1)
                    .min(row[j - 1] + 1)
                    .min(prev[j - 1] + u32::from(q != c));
                if j >= 2 && c == query[j - 2] && before_c == q {
                    best = best.min(prev2[j - 2] + 1);
                }
                row[j] = best;
                row_min = row_min.min(best);
            }
        } else {
            for j in lo.max(1)..=hi {
                let best = (prev[j] + 1)
                    .min(row[j - 1] + 1)
                    .min(prev[j - 1] + u32::from(query[j - 1] != c));
                row[j] = best;
                row_min = row_min.min(best);
            }
        }
        row_min <= self.max
    }
}

/// Turns a sorted iterator of words into the `(shared prefix, rest)` pairs
/// that [`TrieWalker::walk`] expects (the prefix is counted in chars).
pub(crate) fn prefix_coded<'a>(
    words: impl Iterator<Item = &'a str>,
) -> impl Iterator<Item = (usize, &'a str)> {
    let mut prev: &'a str = "";
    words.map(move |word| {
        let mut shared = 0;
        let mut bytes = 0;
        for (a, b) in prev.chars().zip(word.chars()) {
            if a != b {
                break;
            }
            shared += 1;
            bytes += a.len_utf8();
        }
        prev = word;
        (shared, &word[bytes..])
    })
}

/// Optimal-string-alignment distance between two words (full DP).
#[cfg(test)]
pub(crate) fn osa_distance(a: &str, b: &str) -> u32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let width = b.len() + 1;
    let mut d = vec![0u32; (a.len() + 1) * width];
    for (j, cell) in d.iter_mut().take(width).enumerate() {
        *cell = j as u32;
    }
    for i in 1..=a.len() {
        d[i * width] = i as u32;
        for j in 1..=b.len() {
            let cost = u32::from(a[i - 1] != b[j - 1]);
            let mut best = (d[(i - 1) * width + j] + 1)
                .min(d[i * width + j - 1] + 1)
                .min(d[(i - 1) * width + j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[(i - 2) * width + j - 2] + 1);
            }
            d[i * width + j] = best;
        }
    }
    d[a.len() * width + b.len()]
}

// Costs of `plausibility`, in tenths of an edit.
const DOUBLED: u32 = 4;
const TRANSPOSED: u32 = 5;
const VOWEL: u32 = 6;
const NEIGHBOUR: u32 = 7;
const DROPPED: u32 = 8;
const EXTRA: u32 = 9;
const OTHER: u32 = 10;
const FIRST_LETTER: u32 = 3;
const SWAPPED_FIRST_LETTERS: u32 = 1;

const KEYBOARD: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

fn key_position(c: char) -> Option<(usize, usize)> {
    KEYBOARD
        .iter()
        .enumerate()
        .find_map(|(row, keys)| keys.find(c).map(|col| (row, col)))
}

/// True for keys that touch on a QWERTY keyboard (rows are staggered, so a
/// key touches the key below it and the one below-left).
fn neighbours(a: char, b: char) -> bool {
    let (Some((ra, ca)), Some((rb, cb))) = (key_position(a), key_position(b)) else {
        return false;
    };
    let ((upper_row, upper_col), (lower_row, lower_col)) = if ra <= rb {
        ((ra, ca), (rb, cb))
    } else {
        ((rb, cb), (ra, ca))
    };
    if upper_row == lower_row {
        upper_col.abs_diff(lower_col) == 1
    } else {
        lower_row == upper_row + 1 && (lower_col == upper_col || lower_col + 1 == upper_col)
    }
}

fn substitution(a: char, b: char) -> u32 {
    let vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u');
    if a == b {
        0
    } else if vowel(a) && vowel(b) {
        VOWEL
    } else if neighbours(a, b) {
        NEIGHBOUR
    } else {
        OTHER
    }
}

/// How plausible it is that `candidate` was mistyped as `typo` (lower is more
/// plausible). Both are lowercase. Used to order suggestions that have the same
/// edit distance; it never uses word frequencies.
pub(crate) fn plausibility(typo: &[char], candidate: &[char]) -> u32 {
    let (a, b) = (typo, candidate);
    let width = b.len() + 1;
    let mut d = vec![u32::MAX; (a.len() + 1) * width];
    d[0] = 0;
    // The typo lacks `b[j - 1]`; dropping one of a pair of doubled letters is
    // the most common slip of all.
    let dropped = |j: usize| {
        let c = b[j - 1];
        if (j >= 2 && b[j - 2] == c) || b.get(j) == Some(&c) {
            DOUBLED
        } else {
            DROPPED
        }
    };
    // The typo has an extra `a[i - 1]`.
    let extra = |i: usize| {
        let c = a[i - 1];
        if (i >= 2 && a[i - 2] == c) || a.get(i) == Some(&c) {
            DOUBLED
        } else {
            EXTRA
        }
    };
    for i in 0..=a.len() {
        for j in 0..=b.len() {
            if i == 0 && j == 0 {
                continue;
            }
            let mut best = u32::MAX;
            if i > 0 {
                best = best.min(d[(i - 1) * width + j].saturating_add(extra(i)));
            }
            if j > 0 {
                best = best.min(d[i * width + j - 1].saturating_add(dropped(j)));
            }
            if i > 0 && j > 0 {
                best = best.min(
                    d[(i - 1) * width + j - 1].saturating_add(substitution(a[i - 1], b[j - 1])),
                );
            }
            if i > 1
                && j > 1
                && a[i - 1] == b[j - 2]
                && a[i - 2] == b[j - 1]
                && a[i - 1] != a[i - 2]
            {
                best = best.min(d[(i - 2) * width + j - 2].saturating_add(TRANSPOSED));
            }
            d[i * width + j] = best;
        }
    }
    let mut cost = d[a.len() * width + b.len()];
    // Typos rarely change the first letter, a little less rarely by swapping
    // the first two (`hte` → `the`).
    if a.first() != b.first() {
        let swapped_start = a.len() >= 2 && b.len() >= 2 && a[0] == b[1] && a[1] == b[0];
        cost = cost.saturating_add(if swapped_start {
            SWAPPED_FIRST_LETTERS
        } else {
            FIRST_LETTER
        });
    }
    cost
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn walk(query: &str, words: &[&str], max: u32) -> Vec<(String, u32)> {
        let mut sorted = words.to_vec();
        sorted.sort_unstable();
        let q = chars(query);
        let mut walker = TrieWalker::new(&q, max);
        let mut out = Vec::new();
        walker.walk(
            Entries(prefix_coded(sorted.iter().copied())),
            &mut |w, d| out.push((w.iter().collect(), d)),
        );
        out
    }

    #[test]
    fn osa_distance_basics() {
        assert_eq!(osa_distance("", ""), 0);
        assert_eq!(osa_distance("abc", ""), 3);
        assert_eq!(osa_distance("", "abc"), 3);
        assert_eq!(osa_distance("kitten", "sitting"), 3);
        assert_eq!(osa_distance("recieve", "receive"), 1);
        assert_eq!(osa_distance("teh", "the"), 1);
        // OSA does not edit a substring twice (true Damerau-Levenshtein gives 2).
        assert_eq!(osa_distance("ca", "abc"), 3);
    }

    #[test]
    fn walker_matches_brute_force() {
        let words = [
            "a", "ab", "abc", "abcd", "abd", "acb", "b", "ba", "bac", "receive", "received",
            "receiver", "relieve", "deceive", "recite", "the", "they", "then", "tea", "tee", "het",
            "eth", "hte", "naïve", "naive", "zzzz",
        ];
        for query in [
            "recieve",
            "teh",
            "abdc",
            "b",
            "",
            "naive",
            "zz",
            "receivedx",
            "x",
        ] {
            for max in 0..=2 {
                let mut expected: Vec<(String, u32)> = words
                    .iter()
                    .map(|w| (w.to_string(), osa_distance(query, w)))
                    .filter(|(_, d)| *d <= max)
                    .collect();
                expected.sort();
                let mut got = walk(query, &words, max);
                got.sort();
                assert_eq!(got, expected, "query {query:?}, max {max}");
            }
        }
    }

    #[test]
    fn walker_handles_prefixes_and_pruned_subtrees() {
        // The `aaa…` subtree dies at depth 3 and is skipped; the words after it
        // (which share a shorter prefix) must still be found.
        let found = walk("abz", &["aaaa", "aaaab", "aaab", "abz", "abzz"], 1);
        assert_eq!(found, vec![("abz".to_string(), 0), ("abzz".to_string(), 1)]);
        // A word that is a prefix of the next one.
        let found = walk("test", &["test", "tests", "testing"], 1);
        assert_eq!(
            found,
            vec![("test".to_string(), 0), ("tests".to_string(), 1)]
        );
    }

    #[test]
    fn prefix_coding_counts_chars() {
        let coded: Vec<(usize, &str)> = prefix_coded(["naïf", "naïve", "nb"].into_iter()).collect();
        assert_eq!(coded, vec![(0, "naïf"), (3, "ve"), (1, "b")]);
    }

    #[test]
    fn keyboard_neighbours() {
        assert!(neighbours('q', 'w'));
        assert!(neighbours('w', 's'));
        assert!(neighbours('w', 'a'));
        assert!(neighbours('s', 'z'));
        assert!(neighbours('d', 'x'));
        assert!(!neighbours('q', 'e'));
        assert!(!neighbours('a', 'x'));
        assert!(!neighbours('c', 's'));
        assert!(!neighbours('1', 'q'));
    }

    #[test]
    fn plausibility_prefers_typical_typos() {
        let p = |typo: &str, cand: &str| plausibility(&chars(typo), &chars(cand));
        // Doubled letters beat other edits.
        assert!(p("adress", "address") < p("adress", "adless"));
        assert!(p("untill", "until") < p("untill", "unfill"));
        // Swaps beat substitutions; swapping the first two letters is not a
        // first-letter change.
        assert!(p("teh", "the") < p("teh", "ten"));
        assert!(p("hte", "the") < p("hte", "hate"));
        assert!(p("teh", "the") < p("teh", "eth"));
        // Vowel mix-ups beat other substitutions.
        assert!(p("seperate", "separate") < p("seperate", "severate"));
        // Dropped letters beat arbitrary substitutions.
        assert!(p("wich", "which") < p("wich", "with"));
        // Changing the first letter is penalised.
        assert!(p("cat", "bat") > p("cat", "cab"));
        assert_eq!(p("same", "same"), 0);
    }
}
