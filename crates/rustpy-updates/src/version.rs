//! Release versions: `0.2.0`, `v0.2.0`, `0.3.0-eap.1`, `1.0.0-beta.2+build.7`.
//!
//! Precedence follows Semantic Versioning 2.0: major, minor and patch compare as
//! numbers, a pre-release sorts before its release (`0.3.0-eap.1 < 0.3.0`), and
//! pre-release identifiers compare one by one (numbers numerically and before
//! words, a longer list after its prefix). Build metadata never counts. Two
//! leniencies for hand-written tags: a leading `v` and a missing minor or patch
//! number (`v1.2` = `1.2.0`) are accepted, and a word with a number at its end
//! compares naturally (`eap2 < eap10`, where strict SemVer would sort `eap10` first).

use std::cmp::Ordering;
use std::fmt;

/// A parsed release version.
#[derive(Clone, Debug)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifiers (`eap.1` → `eap`, `1`); empty for a final release.
    pub pre: Vec<Identifier>,
    /// Build metadata after `+` (shown, never compared).
    pub build: String,
}

/// One dot-separated pre-release identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identifier {
    Numeric(u64),
    Alphanumeric(String),
}

impl Version {
    /// Parses a version or release tag; `None` when it is not one (`nightly`, `1.2.3.4`).
    pub fn parse(text: &str) -> Option<Version> {
        let s = text.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let (rest, build) = match s.split_once('+') {
            Some((r, b)) => {
                if !b.split('.').all(|p| {
                    !p.is_empty() && p.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                }) {
                    return None;
                }
                (r, b)
            }
            None => (s, ""),
        };
        let (core, pre) = match rest.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (rest, None),
        };
        let mut parts = core.split('.');
        let major = number(parts.next()?)?;
        let minor = parts.next().map_or(Some(0), number)?;
        let patch = parts.next().map_or(Some(0), number)?;
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            Some(p) => p.split('.').map(identifier).collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
            build: build.to_string(),
        })
    }

    /// Whether this is a pre-release (`-eap.1`, `-beta.2`, `-rc.1`).
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The final release a pre-release leads to: `0.3.0` for `0.3.0-eap.1+abc`.
    pub fn release(&self) -> Version {
        Version {
            major: self.major,
            minor: self.minor,
            patch: self.patch,
            pre: Vec::new(),
            build: String::new(),
        }
    }
}

fn number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn identifier(s: &str) -> Option<Identifier> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().ok().map(Identifier::Numeric);
    }
    Some(Identifier::Alphanumeric(s.to_string()))
}

/// Compares words with embedded numbers naturally: `eap2 < eap10`, `alpha < beta`.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let db = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let na = trim_zeros(&a[..da]);
                let nb = trim_zeros(&b[..db]);
                // More significant digits = larger; equal length compares digit by digit.
                let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[da..];
                b = &b[db..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let zeros = digits.iter().take_while(|&&d| d == b'0').count();
    &digits[zeros..]
}

impl Ord for Identifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Identifier::Numeric(a), Identifier::Numeric(b)) => a.cmp(b),
            (Identifier::Numeric(_), Identifier::Alphanumeric(_)) => Ordering::Less,
            (Identifier::Alphanumeric(_), Identifier::Numeric(_)) => Ordering::Greater,
            (Identifier::Alphanumeric(a), Identifier::Alphanumeric(b)) => natural_cmp(a, b),
        }
    }
}

impl PartialOrd for Identifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Equal precedence (build metadata is ignored, as in [`Ord`]).
impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Identifier::Numeric(n) => write!(f, "{n}"),
            Identifier::Alphanumeric(s) => f.write_str(s),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, id) in self.pre.iter().enumerate() {
            write!(f, "{}{id}", if i == 0 { '-' } else { '.' })?;
        }
        if !self.build.is_empty() {
            write!(f, "+{}", self.build)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("not a version: {s}"))
    }

    #[test]
    fn parses_tags_and_versions() {
        let x = v("v1.2.3-eap.4+git.abc");
        assert_eq!((x.major, x.minor, x.patch), (1, 2, 3));
        assert_eq!(
            x.pre,
            vec![
                Identifier::Alphanumeric("eap".into()),
                Identifier::Numeric(4)
            ]
        );
        assert_eq!(x.build, "git.abc");
        assert_eq!(x.to_string(), "1.2.3-eap.4+git.abc");
        assert_eq!(v("0.2").to_string(), "0.2.0");
        assert_eq!(v(" V7 ").to_string(), "7.0.0");
        assert!(v("0.3.0-beta.1").is_prerelease());
        assert!(!v("0.3.0").is_prerelease());
        assert_eq!(v("0.3.0-rc.1+b5").release().to_string(), "0.3.0");
        for bad in [
            "",
            "v",
            "nightly",
            "1.2.3.4",
            "1..2",
            "1.2.x",
            "1.2.3-",
            "1.2.3-eap..1",
            "1.2.3+",
            "1.2.3-é",
            "-1.0.0",
            "1.0.0 beta",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?} parsed");
        }
    }

    #[test]
    fn orders_by_semver_precedence() {
        // The precedence example from the SemVer 2.0 specification, in ascending order.
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for w in ordered.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert!(v("0.9.9") < v("0.10.0"));
        assert!(v("0.10.0") < v("1.0.0-eap.1"));
        assert!(v("1.0.0-eap.1") < v("1.0.0-eap.2"));
        assert!(v("2.0.0") > v("1.99.99"));
        assert!(v("1.0.0-eap.2") < v("1.0.0-eap.10"));
        // Numeric identifiers sort before words; a longer list after its prefix.
        assert!(v("1.0.0-1") < v("1.0.0-eap"));
        assert!(v("1.0.0-eap") < v("1.0.0-eap.1"));
    }

    #[test]
    fn natural_order_for_numbered_words_and_ignores_build_metadata() {
        assert!(v("1.0.0-eap2") < v("1.0.0-eap10"));
        assert!(v("1.0.0-eap10") < v("1.0.0-rc1"));
        assert_eq!(v("1.0.0-eap02").cmp(&v("1.0.0-eap2")), Ordering::Equal);
        assert_eq!(v("1.0.0+linux"), v("v1.0.0+macos"));
        assert_eq!(v("1.0"), v("1.0.0"));
        assert_ne!(v("1.0.0-eap.1"), v("1.0.0"));
    }
}
