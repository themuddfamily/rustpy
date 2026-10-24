//! PEP 440-style version parsing, ordering and specifier matching (the subset
//! the IDE needs: releases, epochs, a/b/rc pre-releases, post and dev
//! releases, local labels; `==`, `!=`, `<`, `<=`, `>`, `>=`, `~=`, `===` and
//! `.*` prefix matches).

use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PreKind {
    Alpha,
    Beta,
    Rc,
}

/// A parsed version. Ordering follows PEP 440; `1.0 == 1.0.0`.
#[derive(Clone, Debug, Eq)]
pub struct Version {
    pub epoch: u64,
    pub release: Vec<u64>,
    pub pre: Option<(PreKind, u64)>,
    pub post: Option<u64>,
    pub dev: Option<u64>,
    pub local: Option<String>,
    /// False when the text did not look like a version; such values compare
    /// textually and sort before every valid version.
    pub valid: bool,
    /// The original (trimmed) text.
    pub raw: String,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum PreKey {
    NegInf,
    Pre(PreKind, u64),
    Inf,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum PostKey {
    NegInf,
    Post(u64),
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum DevKey {
    Dev(u64),
    Inf,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum LocalSeg {
    Alpha(String),
    Num(u64),
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum LocalKey {
    NegInf,
    Local(Vec<LocalSeg>),
}

type Key = (
    bool,
    u64,
    Vec<u64>,
    PreKey,
    PostKey,
    DevKey,
    LocalKey,
    String,
);

impl Version {
    fn key(&self) -> Key {
        if !self.valid {
            return (
                false,
                0,
                Vec::new(),
                PreKey::NegInf,
                PostKey::NegInf,
                DevKey::Inf,
                LocalKey::NegInf,
                self.raw.to_ascii_lowercase(),
            );
        }
        let mut release = self.release.clone();
        while release.len() > 1 && release.last() == Some(&0) {
            release.pop();
        }
        let pre = match self.pre {
            None if self.post.is_none() && self.dev.is_some() => PreKey::NegInf,
            None => PreKey::Inf,
            Some((k, n)) => PreKey::Pre(k, n),
        };
        let post = self.post.map(PostKey::Post).unwrap_or(PostKey::NegInf);
        let dev = self.dev.map(DevKey::Dev).unwrap_or(DevKey::Inf);
        let local = match &self.local {
            None => LocalKey::NegInf,
            Some(l) => LocalKey::Local(
                l.split(['.', '-', '_'])
                    .map(|s| {
                        s.parse::<u64>()
                            .map(LocalSeg::Num)
                            .unwrap_or_else(|_| LocalSeg::Alpha(s.to_ascii_lowercase()))
                    })
                    .collect(),
            ),
        };
        (
            true,
            self.epoch,
            release,
            pre,
            post,
            dev,
            local,
            String::new(),
        )
    }

    /// The version without its local label (`1.0+cpu` → `1.0`).
    pub fn public(&self) -> Version {
        Version {
            local: None,
            ..self.clone()
        }
    }

    /// True for alpha/beta/rc/dev versions.
    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some() || self.dev.is_some()
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.valid {
            return f.write_str(&self.raw);
        }
        if self.epoch > 0 {
            write!(f, "{}!", self.epoch)?;
        }
        let rel: Vec<String> = self.release.iter().map(u64::to_string).collect();
        f.write_str(&rel.join("."))?;
        if let Some((k, n)) = self.pre {
            let tag = match k {
                PreKind::Alpha => "a",
                PreKind::Beta => "b",
                PreKind::Rc => "rc",
            };
            write!(f, "{tag}{n}")?;
        }
        if let Some(n) = self.post {
            write!(f, ".post{n}")?;
        }
        if let Some(n) = self.dev {
            write!(f, ".dev{n}")?;
        }
        if let Some(l) = &self.local {
            write!(f, "+{l}")?;
        }
        Ok(())
    }
}

/// Parses a version string leniently; never fails (see [`Version::valid`]).
pub fn parse_version(text: &str) -> Version {
    let raw = text.trim().to_string();
    let mut v = Version {
        epoch: 0,
        release: Vec::new(),
        pre: None,
        post: None,
        dev: None,
        local: None,
        valid: false,
        raw: raw.clone(),
    };
    let lower = raw.to_ascii_lowercase();
    let mut s = lower.as_str();
    s = s.strip_prefix('v').unwrap_or(s);
    let (main, local) = match s.split_once('+') {
        Some((m, l)) => (m, Some(l)),
        None => (s, None),
    };
    let main = match main.split_once('!') {
        Some((e, rest)) => match e.parse::<u64>() {
            Ok(e) => {
                v.epoch = e;
                rest
            }
            Err(_) => return v,
        },
        None => main,
    };
    let bytes = main.as_bytes();
    let mut i = 0;
    // Release segment: digits separated by dots.
    loop {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            if v.release.is_empty() {
                return v; // no leading number: not a version
            }
            break;
        }
        v.release
            .push(main[start..i].parse::<u64>().unwrap_or(u64::MAX));
        if i < bytes.len()
            && bytes[i] == b'.'
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
        {
            i += 1;
            continue;
        }
        break;
    }
    v.valid = true;
    // Pre/post/dev segments.
    while i < bytes.len() {
        let mut sep = false;
        while i < bytes.len() && matches!(bytes[i], b'.' | b'-' | b'_') {
            sep = true;
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let ls = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let letters = &main[ls..i];
        while i < bytes.len() && matches!(bytes[i], b'.' | b'-' | b'_') && !letters.is_empty() {
            i += 1;
        }
        let ds = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let num: Option<u64> = if ds == i {
            None
        } else {
            main[ds..i].parse::<u64>().ok()
        };
        match letters {
            "a" | "alpha" => v.pre = Some((PreKind::Alpha, num.unwrap_or(0))),
            "b" | "beta" => v.pre = Some((PreKind::Beta, num.unwrap_or(0))),
            "rc" | "c" | "pre" | "preview" => v.pre = Some((PreKind::Rc, num.unwrap_or(0))),
            "post" | "rev" | "r" => v.post = Some(num.unwrap_or(0)),
            "dev" => v.dev = Some(num.unwrap_or(0)),
            "" if sep && num.is_some() => v.post = Some(num.unwrap_or(0)), // implicit post release `1.0-1`
            _ => {
                v.valid = false;
                return v;
            }
        }
    }
    v.local = local
        .map(|l| {
            l.trim_matches(|c| c == '.' || c == '-' || c == '_')
                .to_string()
        })
        .filter(|l| !l.is_empty());
    v
}

/// One clause of a specifier, e.g. `>=1.2` or `==1.*`.
fn clause_matches(version: &Version, clause: &str) -> bool {
    let clause = clause.trim().trim_matches(|c| c == '(' || c == ')').trim();
    if clause.is_empty() {
        return true;
    }
    let (op, operand) = ["===", "==", "!=", "<=", ">=", "~=", "<", ">"]
        .iter()
        .find_map(|op| clause.strip_prefix(op).map(|rest| (*op, rest.trim())))
        .unwrap_or(("==", clause));
    if op == "===" {
        return version.raw.trim() == operand;
    }
    let (operand, wildcard) = match operand.strip_suffix(".*") {
        Some(o) => (o, true),
        None => (operand, false),
    };
    let target = parse_version(operand);
    let prefix_match = |target: &Version| -> bool {
        if version.epoch != target.epoch {
            return false;
        }
        let n = target.release.len();
        let mut rel = version.release.clone();
        rel.resize(rel.len().max(n), 0);
        rel[..n] == target.release[..]
    };
    match op {
        "==" if wildcard => prefix_match(&target),
        "!=" if wildcard => !prefix_match(&target),
        "==" => {
            if target.local.is_none() {
                version.public() == target
            } else {
                *version == target
            }
        }
        "!=" => {
            if target.local.is_none() {
                version.public() != target
            } else {
                *version != target
            }
        }
        "<=" => version.public() <= target,
        ">=" => version.public() >= target,
        "<" => version.public() < target,
        ">" => version.public() > target,
        "~=" => {
            if target.release.len() < 2 {
                return version.public() >= target;
            }
            let mut prefix = target.clone();
            prefix.release.pop();
            prefix.pre = None;
            prefix.post = None;
            prefix.dev = None;
            version.public() >= target && prefix_match(&prefix)
        }
        _ => false,
    }
}

/// True when `version` satisfies the (comma separated) specifier set.
pub fn specifier_matches(version: &Version, specifier: &str) -> bool {
    specifier.split(',').all(|c| clause_matches(version, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        parse_version(s)
    }

    #[test]
    fn parses_components() {
        let x = v("2!1.2.3rc4.post5.dev6+local.7");
        assert_eq!(x.epoch, 2);
        assert_eq!(x.release, vec![1, 2, 3]);
        assert_eq!(x.pre, Some((PreKind::Rc, 4)));
        assert_eq!(x.post, Some(5));
        assert_eq!(x.dev, Some(6));
        assert_eq!(x.local.as_deref(), Some("local.7"));
        assert!(x.valid);
        assert_eq!(x.to_string(), "2!1.2.3rc4.post5.dev6+local.7");
        assert_eq!(v("1.0-alpha.1").pre, Some((PreKind::Alpha, 1)));
        assert_eq!(v("1.0-1").post, Some(1));
        assert_eq!(v("V1.0b").pre, Some((PreKind::Beta, 0)));
        assert!(!v("latest").valid);
        assert!(!v("1.0.weird").valid);
    }

    #[test]
    fn orders_like_pep440() {
        let ordered = [
            "0.9",
            "1.0.dev1",
            "1.0a1.dev1",
            "1.0a1",
            "1.0a2",
            "1.0b1",
            "1.0rc1",
            "1.0",
            "1.0+local",
            "1.0.post1.dev1",
            "1.0.post1",
            "1.0.1",
            "1.1",
            "2.0",
            "1!0.1",
        ];
        for w in ordered.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("1.0"), v("1.0.0"));
        assert_eq!(v("1.0"), v("1"));
        assert!(v("1.10") > v("1.9"));
        assert!(v("2022.1") > v("3.0"));
        assert!(v("not-a-version") < v("0.0.1"));
        let mut list: Vec<Version> = ["1.0", "0.1", "2.0rc1", "2.0", "1.5.post2"]
            .iter()
            .map(|s| v(s))
            .collect();
        list.sort();
        let got: Vec<String> = list.iter().map(|x| x.raw.clone()).collect();
        assert_eq!(got, vec!["0.1", "1.0", "1.5.post2", "2.0rc1", "2.0"]);
    }

    #[test]
    fn matches_specifiers() {
        let m = |ver: &str, spec: &str| specifier_matches(&v(ver), spec);
        assert!(m("1.5", ">=1.2,<2"));
        assert!(!m("2.0", ">=1.2,<2"));
        assert!(m("1.4.2", "~=1.4"));
        assert!(m("1.9", "~=1.4"));
        assert!(!m("2.0", "~=1.4"));
        assert!(m("1.4.7", "~=1.4.5"));
        assert!(!m("1.5", "~=1.4.5"));
        assert!(m("1.2.3", "==1.*"));
        assert!(m("1.2.3", "==1.2.*"));
        assert!(!m("1.3", "==1.2.*"));
        assert!(m("1.3", "!=1.2.*"));
        assert!(m("1.0.0", "==1.0"));
        assert!(m("1.0+cpu", "==1.0"));
        assert!(!m("1.0", "==1.0+cpu"));
        assert!(m("3.0", ">2.9.9"));
        assert!(!m("2.9.9", ">2.9.9"));
        assert!(m("2.9.9", "<=2.9.9"));
        assert!(m("1.0", "===1.0"));
        assert!(!m("1.0.0", "===1.0"));
        assert!(m("1.0", " ( >= 0.5 , != 0.7 ) "));
        assert!(m("1.0", ""));
        assert!(m("1.2", "1.2"));
    }
}
