//! CLDR plural rules of the supported locales, for integer counts.

use std::fmt;

/// A CLDR plural category: the grammatical form a count selects
/// (`1 file` / `2 files`; Russian `1 файл` / `2 файла` / `5 файлов`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl PluralCategory {
    /// Every category, in CLDR order.
    pub const ALL: [PluralCategory; 6] = [
        Self::Zero,
        Self::One,
        Self::Two,
        Self::Few,
        Self::Many,
        Self::Other,
    ];

    /// The CLDR keyword used as key in catalogs: `"zero"`, `"one"`, `"two"`,
    /// `"few"`, `"many"` or `"other"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::Two => "two",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }

    /// Parses a CLDR keyword ([`PluralCategory::name`]).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }
}

impl fmt::Display for PluralCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A count that selects a plural form: any primitive integer type.
///
/// Negative counts select by their magnitude, as CLDR specifies (`-1 file`).
pub trait PluralCount: Copy + fmt::Display {
    /// The absolute value of the count.
    fn magnitude(self) -> u128;
}

macro_rules! unsigned_count {
    ($($t:ty),*) => {$(
        impl PluralCount for $t {
            fn magnitude(self) -> u128 {
                u128::from(self)
            }
        }
    )*};
}

macro_rules! signed_count {
    ($($t:ty),*) => {$(
        impl PluralCount for $t {
            fn magnitude(self) -> u128 {
                u128::from(self.unsigned_abs())
            }
        }
    )*};
}

unsigned_count!(u8, u16, u32, u64);
signed_count!(i8, i16, i32, i64);

impl PluralCount for u128 {
    fn magnitude(self) -> u128 {
        self
    }
}

impl PluralCount for i128 {
    fn magnitude(self) -> u128 {
        self.unsigned_abs()
    }
}

impl PluralCount for usize {
    fn magnitude(self) -> u128 {
        self as u128
    }
}

impl PluralCount for isize {
    fn magnitude(self) -> u128 {
        self.unsigned_abs() as u128
    }
}

/// The plural rule families of the supported locales (CLDR, integer operands:
/// `i` is the count, `v = 0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PluralRules {
    /// zh, ja, ko: a single form (`other`).
    Invariant,
    /// en, de: `one` for 1, `other` otherwise.
    OneOther,
    /// fr and pt (Brazil): `one` for 0 and 1, `many` for non-zero multiples
    /// of a million (`1 000 000 de fichiers`), `other` otherwise.
    ZeroOneMillions,
    /// es: `one` for 1, `many` for non-zero multiples of a million
    /// (`1 000 000 de archivos`), `other` otherwise.
    OneMillions,
    /// ru: `one` for …1 except …11, `few` for …2–…4 except …12–…14, `many`
    /// otherwise (`other` is only reached by fractions).
    EastSlavic,
}

impl PluralRules {
    pub(crate) fn category(self, i: u128) -> PluralCategory {
        let millions = i != 0 && i.is_multiple_of(1_000_000);
        match self {
            Self::Invariant => PluralCategory::Other,
            Self::OneOther if i == 1 => PluralCategory::One,
            Self::OneOther => PluralCategory::Other,
            Self::ZeroOneMillions | Self::OneMillions if millions => PluralCategory::Many,
            Self::ZeroOneMillions if i <= 1 => PluralCategory::One,
            Self::OneMillions if i == 1 => PluralCategory::One,
            Self::ZeroOneMillions | Self::OneMillions => PluralCategory::Other,
            Self::EastSlavic => {
                let (units, tens) = (i % 10, i % 100);
                if units == 1 && tens != 11 {
                    PluralCategory::One
                } else if (2..=4).contains(&units) && !(12..=14).contains(&tens) {
                    PluralCategory::Few
                } else {
                    PluralCategory::Many
                }
            }
        }
    }

    /// The categories integer counts can select, i.e. the forms a complete
    /// catalog entry needs.
    pub(crate) fn integer_categories(self) -> &'static [PluralCategory] {
        use PluralCategory::{Few, Many, One, Other};
        match self {
            Self::Invariant => &[Other],
            Self::OneOther => &[One, Other],
            Self::ZeroOneMillions | Self::OneMillions => &[One, Many, Other],
            Self::EastSlavic => &[One, Few, Many],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PluralCategory::{Few, Many, One, Other};
    use super::*;

    fn categories(rules: PluralRules, counts: &[u128]) -> Vec<PluralCategory> {
        counts.iter().map(|&n| rules.category(n)).collect()
    }

    #[test]
    fn english_and_german() {
        assert_eq!(
            categories(PluralRules::OneOther, &[0, 1, 2, 11, 21, 101]),
            [Other, One, Other, Other, Other, Other]
        );
    }

    #[test]
    fn chinese_japanese_korean_have_one_form() {
        assert_eq!(
            categories(PluralRules::Invariant, &[0, 1, 2, 1_000_000]),
            [Other; 4]
        );
    }

    #[test]
    fn french_and_portuguese_treat_zero_as_singular() {
        assert_eq!(
            categories(
                PluralRules::ZeroOneMillions,
                &[0, 1, 2, 999_999, 1_000_000, 1_000_001, 3_000_000]
            ),
            [One, One, Other, Other, Many, Other, Many]
        );
    }

    #[test]
    fn spanish_zero_is_plural() {
        assert_eq!(
            categories(
                PluralRules::OneMillions,
                &[0, 1, 2, 1_000_000, 2_000_000, 2_000_001]
            ),
            [Other, One, Other, Many, Many, Other]
        );
    }

    #[test]
    fn russian_one_few_many() {
        let counts = [
            0, 1, 2, 4, 5, 10, 11, 12, 14, 15, 20, 21, 22, 25, 101, 111, 112, 121, 1_000_001,
        ];
        let expected = [
            Many, One, Few, Few, Many, Many, Many, Many, Many, Many, Many, One, Few, Many, One,
            Many, Many, One, One,
        ];
        assert_eq!(categories(PluralRules::EastSlavic, &counts), expected);
    }

    #[test]
    fn integer_categories_are_exactly_the_reachable_ones() {
        let samples: Vec<u128> = (0..=250)
            .chain([1_000_000, 2_000_000, 1_000_001, 10_000_000_000])
            .collect();
        for rules in [
            PluralRules::Invariant,
            PluralRules::OneOther,
            PluralRules::ZeroOneMillions,
            PluralRules::OneMillions,
            PluralRules::EastSlavic,
        ] {
            let mut reached: Vec<PluralCategory> =
                samples.iter().map(|&n| rules.category(n)).collect();
            reached.sort();
            reached.dedup();
            let mut declared = rules.integer_categories().to_vec();
            declared.sort();
            assert_eq!(reached, declared, "{rules:?}");
        }
    }

    #[test]
    fn counts_of_every_integer_type() {
        assert_eq!(5u8.magnitude(), 5);
        assert_eq!((-5i8).magnitude(), 5);
        assert_eq!(i64::MIN.magnitude(), 1u128 << 63);
        assert_eq!(usize::MAX.magnitude(), usize::MAX as u128);
        assert_eq!((-3isize).magnitude(), 3);
        assert_eq!(u128::MAX.magnitude(), u128::MAX);
        assert_eq!(i128::MIN.magnitude(), 1u128 << 127);
    }

    #[test]
    fn category_names_round_trip() {
        for c in PluralCategory::ALL {
            assert_eq!(PluralCategory::from_name(c.name()), Some(c));
            assert_eq!(c.to_string(), c.name());
        }
        assert_eq!(PluralCategory::from_name("several"), None);
    }
}
