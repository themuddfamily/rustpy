//! "Check RegExp": evaluates a pattern against sample text with the real
//! Python `re` module so the result matches the interpreter exactly.

use std::path::Path;

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct MatchResult {
    pub matched: bool,
    /// `(name or index, captured text)` of the first match.
    #[serde(default)]
    pub groups: Vec<(String, Option<String>)>,
    /// `(start, end)` character spans of every match (`finditer`).
    #[serde(default)]
    pub spans: Vec<(usize, usize)>,
    /// Compile error from `re`, if any.
    #[serde(default)]
    pub error: Option<String>,
}

const SCRIPT: &str = r#"
import json, re, sys
pat, flags, text = sys.argv[1], sys.argv[2], sys.argv[3]
f = 0
table = {"i": re.I, "m": re.M, "s": re.S, "x": re.X, "a": re.A, "u": re.U, "l": re.L}
for ch in flags:
    f |= table.get(ch.lower(), 0)
try:
    rx = re.compile(pat, f)
except re.error as e:
    print(json.dumps({"matched": False, "groups": [], "spans": [], "error": str(e)}))
    sys.exit(0)
m = rx.search(text)
spans = [list(x.span()) for x in rx.finditer(text)]
groups = []
if m:
    for i in range(1, rx.groups + 1):
        groups.append([str(i), m.group(i)])
    for name, idx in rx.groupindex.items():
        groups.append([name, m.group(idx)])
print(json.dumps({"matched": m is not None, "groups": groups, "spans": spans, "error": None}))
"#;

/// Runs `pattern` (with flag letters such as `"im"`) against `text` using `interpreter`.
pub fn test_match(
    interpreter: &Path,
    pattern: &str,
    flags: &str,
    text: &str,
) -> anyhow::Result<MatchResult> {
    let output = rustpy_core::interpreter_launch::interpreter_command(interpreter)
        .arg("-c")
        .arg(SCRIPT)
        .arg(pattern)
        .arg(flags)
        .arg(text)
        .env("PYTHONIOENCODING", "utf-8")
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "{} exited with {}: {}",
            interpreter.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().last().unwrap_or("").trim();
    let result: MatchResult = serde_json::from_str(line)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python() -> Option<std::path::PathBuf> {
        which::which("python3").ok()
    }

    #[test]
    fn matches_with_groups_and_spans() {
        let Some(py) = python() else { return };
        let r = test_match(
            &py,
            r"(?P<word>\w+)@(\w+)",
            "i",
            "mail: Bob@example, eve@corp",
        )
        .unwrap();
        assert!(r.matched);
        assert_eq!(r.spans, vec![(6, 17), (19, 27)]);
        assert!(r
            .groups
            .contains(&("1".to_string(), Some("Bob".to_string()))));
        assert!(r
            .groups
            .contains(&("word".to_string(), Some("Bob".to_string()))));
        assert!(r.error.is_none());
    }

    #[test]
    fn reports_compile_errors_and_misses() {
        let Some(py) = python() else { return };
        let r = test_match(&py, "(unclosed", "", "x").unwrap();
        assert!(!r.matched);
        assert!(r.error.is_some());
        let r = test_match(&py, "zzz", "", "abc").unwrap();
        assert!(!r.matched);
        assert!(r.spans.is_empty());
    }
}
