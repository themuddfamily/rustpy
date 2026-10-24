//! Semver-ish plugin versions: `1`, `1.2`, `1.2.3`, `2024.1.0.7`, with an
//! optional `-pre.release` and an ignored `+build` suffix.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// The longest accepted version string.
const MAX_LEN: usize = 64;
/// At most this many numeric components (`1.2.3.4`).
const MAX_PARTS: usize = 4;

/// A parsed version. Missing numeric parts count as zero (`1.2` == `1.2.0`),
/// a pre-release sorts before its release (`1.0.0-beta.2` < `1.0.0`) and the
/// build suffix is ignored when comparing, as in semver.
#[derive(Clone, Debug)]
pub struct Version {
    text: String,
    numbers: Vec<u64>,
    pre: Vec<PreId>,
}

/// A pre-release identifier. Numeric identifiers sort before alphanumeric
/// ones, which the derived ordering gives via the variant order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PreId {
    Num(u64),
    Text(String),
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Version {
    /// Parses `text`; the error says what is wrong with it.
    pub fn parse(text: &str) -> Result<Version, String> {
        if text.is_empty() {
            return Err("the version is empty".into());
        }
        if text.len() > MAX_LEN {
            return Err(format!(
                "version `{text}` is longer than {MAX_LEN} characters"
            ));
        }
        if text.trim() != text {
            return Err(format!("version `{text}` has leading or trailing spaces"));
        }
        if let Some(rest) = text.strip_prefix(['v', 'V']) {
            if rest.starts_with(|c: char| c.is_ascii_digit()) {
                return Err(format!(
                    "version `{text}`: drop the leading `{}`",
                    &text[..1]
                ));
            }
        }
        let (rest, build) = match text.split_once('+') {
            Some((rest, build)) => (rest, Some(build)),
            None => (text, None),
        };
        if let Some(build) = build {
            if !build.split('.').all(is_identifier) {
                return Err(format!(
                    "version `{text}` has an invalid build suffix after `+`"
                ));
            }
        }
        let (core, pre) = match rest.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (rest, None),
        };
        let mut numbers = Vec::new();
        for part in core.split('.') {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!(
                    "version `{text}` is not semver-like (expected dot-separated numbers such as `1.2.3`)"
                ));
            }
            let n = part
                .parse::<u64>()
                .map_err(|_| format!("version `{text}`: `{part}` is too large"))?;
            numbers.push(n);
        }
        if numbers.len() > MAX_PARTS {
            return Err(format!(
                "version `{text}` has more than {MAX_PARTS} numeric parts"
            ));
        }
        let mut ids = Vec::new();
        if let Some(pre) = pre {
            for id in pre.split('.') {
                if !is_identifier(id) {
                    return Err(format!(
                        "version `{text}` has an invalid pre-release suffix (use letters, digits, `-` and `.`)"
                    ));
                }
                ids.push(match id.parse::<u64>() {
                    Ok(n) if id.bytes().all(|b| b.is_ascii_digit()) => PreId::Num(n),
                    _ => PreId::Text(id.to_string()),
                });
            }
        }
        Ok(Version {
            text: text.to_string(),
            numbers,
            pre: ids,
        })
    }

    /// The version as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// True for pre-releases such as `1.0.0-rc.1`.
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.numbers.len().max(other.numbers.len());
        for i in 0..len {
            let a = self.numbers.get(i).copied().unwrap_or(0);
            let b = other.numbers.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                unequal => return unequal,
            }
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => self.pre.cmp(&other.pre),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Version::parse(s)
    }
}

/// Compares two version strings; `None` when either does not parse.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    Some(Version::parse(a).ok()?.cmp(&Version::parse(b).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn parses_semver_like_versions() {
        for ok in [
            "1",
            "1.2",
            "1.2.3",
            "2024.1.0.7",
            "1.0.0-beta",
            "1.0.0-rc.1",
            "1.0.0+build.5",
            "0.1.0-alpha-2+sha.abc",
        ] {
            assert!(Version::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "v1.2",
            "1..2",
            "1.2.",
            ".1",
            "1.2.3.4.5",
            "1.x",
            "1.0-",
            "1.0-a..b",
            "1.0+",
            " 1.0",
            "1.0 ",
            "99999999999999999999999",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(Version::parse("v1.0").unwrap_err().contains("leading `v`"));
    }

    #[test]
    fn orders_like_semver() {
        assert!(v("1.0.1") > v("1.0.0"));
        assert!(v("1.10") > v("1.9"));
        assert!(v("2") > v("1.99.99"));
        assert_eq!(v("1.2"), v("1.2.0"));
        assert_eq!(v("1.2.0+build.7"), v("1.2.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.beta"));
        assert!(v("1.0.0-beta.2") < v("1.0.0-beta.11"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0-rc.1").is_prerelease());
        assert_eq!(compare_versions("1.1", "1.0.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("1.1", "nope"), None);
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
    }
}
