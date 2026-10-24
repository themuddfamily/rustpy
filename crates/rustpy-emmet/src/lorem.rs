//! The `lorem` / `loremN` / `loremN-M` / `lipsum` text generator. Output is
//! pseudo-random but deterministic (a fixed-seed generator), so the same
//! abbreviation always expands to the same text.

/// Upper bound on generated words per `lorem` node.
pub(crate) const MAX_WORDS: usize = 5_000;

#[rustfmt::skip]
const COMMON: [&str; 8] = [
    "lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipisicing", "elit",
];

#[rustfmt::skip]
const WORDS: &[&str] = &[
    "exercitationem", "perferendis", "perspiciatis", "laborum", "eveniet", "sunt", "iure", "nam", "nobis", "eum",
    "cum", "officiis", "excepturi", "odio", "consectetur", "quasi", "aut", "quisquam", "vel", "eligendi", "itaque",
    "non", "odit", "tempore", "quaerat", "dignissimos", "facilis", "neque", "nihil", "expedita", "vitae", "vero",
    "ipsum", "nisi", "animi", "cumque", "pariatur", "velit", "modi", "natus", "iusto", "eaque", "sequi", "illo",
    "sed", "ex", "et", "voluptatibus", "tempora", "veritatis", "ratione", "assumenda", "incidunt", "nostrum",
    "placeat", "aliquid", "fuga", "provident", "praesentium", "rem", "necessitatibus", "suscipit", "adipisci",
    "quidem", "possimus", "voluptas", "debitis", "sint", "accusantium", "unde", "sapiente", "voluptate", "qui",
    "aspernatur", "laudantium", "soluta", "amet", "quo", "aliquam", "saepe", "culpa", "libero", "ipsa", "dicta",
    "reiciendis", "nesciunt", "doloribus", "autem", "impedit", "minima", "maiores", "repudiandae", "ipsam",
    "obcaecati", "ullam", "enim", "totam", "delectus", "ducimus", "quis", "voluptates", "dolores", "molestiae",
    "harum", "dolorem", "quia", "voluptatem", "molestias", "magni", "distinctio", "omnis", "illum", "dolorum",
    "voluptatum", "ea", "quas", "quam", "corporis", "quae", "blanditiis", "atque", "deserunt", "laboriosam",
    "earum", "consequuntur", "hic", "cupiditate", "quibusdam", "accusamus", "ut", "rerum", "error", "minus", "eius",
    "ab", "ad", "nemo", "fugit", "officia", "at", "in", "id", "quos", "reprehenderit", "numquam", "iste", "fugiat",
    "sit", "inventore", "beatae", "repellendus", "magnam", "recusandae", "quod", "explicabo", "doloremque",
    "aperiam", "consequatur", "asperiores", "commodi", "optio", "dolor", "labore", "temporibus", "repellat",
    "veniam", "architecto", "est", "esse", "mollitia", "nulla", "a", "similique", "eos", "alias", "dolore",
    "tenetur", "deleniti", "porro", "facere", "maxime", "corrupti",
];

/// SplitMix64: tiny, fast and good enough for filler text.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    pub(crate) fn range(&mut self, lo: usize, hi: usize) -> usize {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() % (hi - lo + 1) as u64) as usize
    }
}

/// Parses a generator name: `lorem` (30 words), `lorem10`, `lorem10-20`
/// (between 10 and 20 words) and the same with `lipsum`. Returns the
/// `(min, max)` word counts.
pub(crate) fn parse_name(name: &str) -> Option<(usize, usize)> {
    let rest = name
        .strip_prefix("lorem")
        .or_else(|| name.strip_prefix("lipsum"))?;
    let (low, high) = match rest.split_once('-') {
        Some((low, high)) => (low, Some(high)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let min = if low.is_empty() {
        30
    } else if digits(low) {
        low.parse::<usize>()
            .unwrap_or(MAX_WORDS)
            .clamp(1, MAX_WORDS)
    } else {
        return None;
    };
    let max = match high {
        None => min,
        Some(h) if digits(h) => h
            .parse::<usize>()
            .unwrap_or(MAX_WORDS)
            .clamp(min, MAX_WORDS),
        Some(_) => return None,
    };
    Some((min, max))
}

/// A paragraph of exactly `count` words. With `common_start` it opens with
/// "Lorem ipsum dolor sit amet, consectetur adipisicing elit".
pub(crate) fn paragraph(count: usize, common_start: bool, rng: &mut Rng) -> String {
    let mut sentences: Vec<String> = Vec::new();
    let mut total = 0;
    if common_start {
        let n = count.min(COMMON.len());
        let mut words: Vec<String> = COMMON[..n].iter().map(|w| w.to_string()).collect();
        if n > 5 {
            words[4].push(',');
        }
        sentences.push(sentence(words, '.'));
        total = n;
    }
    while total < count {
        let n = rng.range(3, 14).min(count - total);
        let mut words: Vec<String> = Vec::with_capacity(n);
        while words.len() < n {
            let w = WORDS[rng.range(0, WORDS.len() - 1)];
            if words.last().is_none_or(|prev| prev != w) {
                words.push(w.to_string());
            }
        }
        insert_commas(&mut words, rng);
        let end = ['.', '.', '.', '?', '!'][rng.range(0, 4)];
        sentences.push(sentence(words, end));
        total += n;
    }
    sentences.join(" ")
}

fn sentence(mut words: Vec<String>, end: char) -> String {
    if let Some(first) = words.first_mut() {
        let mut chars = first.chars();
        if let Some(c) = chars.next() {
            *first = c.to_uppercase().chain(chars).collect();
        }
    }
    let mut s = words.join(" ");
    s.push(end);
    s
}

fn insert_commas(words: &mut [String], rng: &mut Rng) {
    let len = words.len();
    if len < 4 {
        return;
    }
    let commas = match len {
        4..=6 => rng.range(0, 1),
        7..=12 => rng.range(0, 2),
        _ => rng.range(1, 4),
    };
    for _ in 0..commas {
        let at = rng.range(0, len - 2);
        if !words[at].ends_with(',') {
            words[at].push(',');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(parse_name("lorem"), Some((30, 30)));
        assert_eq!(parse_name("lorem7"), Some((7, 7)));
        assert_eq!(parse_name("lipsum3-9"), Some((3, 9)));
        assert_eq!(parse_name("lorem0"), Some((1, 1)));
        assert_eq!(
            parse_name("lorem99999999999999999999999"),
            Some((MAX_WORDS, MAX_WORDS))
        );
        assert_eq!(parse_name("loremx"), None);
        assert_eq!(parse_name("lorem5-"), None);
        assert_eq!(parse_name("ipsum"), None);
    }

    #[test]
    fn exact_word_counts_and_common_start() {
        for n in [1, 4, 8, 9, 30, 100] {
            let text = paragraph(n, true, &mut Rng::new(1));
            assert_eq!(text.split_whitespace().count(), n, "{text}");
            assert!(text.starts_with("Lorem"), "{text}");
        }
        assert_eq!(
            paragraph(4, true, &mut Rng::new(1)),
            "Lorem ipsum dolor sit."
        );
        assert!(paragraph(30, true, &mut Rng::new(1))
            .starts_with("Lorem ipsum dolor sit amet, consectetur adipisicing elit. "));
        let other = paragraph(12, false, &mut Rng::new(2));
        assert_eq!(other.split_whitespace().count(), 12);
        assert!(other.chars().next().unwrap().is_uppercase());
        assert!(other.ends_with(['.', '?', '!']));
    }

    #[test]
    fn deterministic() {
        assert_eq!(
            paragraph(40, false, &mut Rng::new(7)),
            paragraph(40, false, &mut Rng::new(7))
        );
        assert_ne!(
            paragraph(40, false, &mut Rng::new(7)),
            paragraph(40, false, &mut Rng::new(8))
        );
    }
}
