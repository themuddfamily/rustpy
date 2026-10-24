//! `rustpy format`: the command-line formatter (`format.sh`-style options) on
//! RustPy's formatter, without opening a window.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use rustpy_settings::editorconfig::glob_match;
use rustpy_settings::Settings;
use rustpy_text::LineEnding;

use crate::args::FormatArgs;
use crate::style::{self, Decision, StyleResolver};

/// Masks used when `-mask` is not given.
pub const DEFAULT_MASKS: &[&str] = &["*.py", "*.pyi"];

/// An encoding forced with `-charset`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Charset {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// UTF-16 with a byte order mark (big-endian without one, like Java).
    Utf16,
    Latin1,
    Ascii,
}

impl Charset {
    pub fn parse(name: &str) -> Option<Self> {
        let n: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        Some(match n.as_str() {
            "utf8" => Self::Utf8,
            "utf16le" => Self::Utf16Le,
            "utf16be" => Self::Utf16Be,
            "utf16" => Self::Utf16,
            "iso88591" | "latin1" | "l1" | "cp819" => Self::Latin1,
            "usascii" | "ascii" => Self::Ascii,
            _ => return None,
        })
    }
}

/// How a file's bytes map to text, so it is written back the same way.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Encoded {
    /// Detected (no `-charset`): `rustpy_text`'s rules (BOMs, UTF-8, else Latin-1).
    Detected(rustpy_text::Encoding),
    Utf8 {
        bom: bool,
    },
    Utf16 {
        little: bool,
        bom: bool,
    },
    Latin1,
}

fn decode(bytes: &[u8], charset: Option<Charset>) -> Result<(String, Encoded), String> {
    // An explicit charset does not authorize replacing malformed declared
    // Unicode or ignoring an unsupported/conflicting source cookie.
    if charset.is_some() {
        rustpy_text::Encoding::decode(bytes).map_err(|e| e.to_string())?;
    }
    let utf16 = |rest: &[u8], little: bool, bom: bool| -> Result<(String, Encoded), String> {
        if !rest.len().is_multiple_of(2) {
            return Err("not valid UTF-16 (odd length)".into());
        }
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| {
                if little {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16(&units)
            .map(|s| (s, Encoded::Utf16 { little, bom }))
            .map_err(|_| "not valid UTF-16".to_string())
    };
    match charset {
        None => {
            let (text, enc) = rustpy_text::Encoding::decode(bytes).map_err(|e| e.to_string())?;
            Ok((text, Encoded::Detected(enc)))
        }
        Some(Charset::Utf8) => {
            let (rest, bom) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
                Some(r) => (r, true),
                None => (bytes, false),
            };
            String::from_utf8(rest.to_vec())
                .map(|s| (s, Encoded::Utf8 { bom }))
                .map_err(|_| "not valid UTF-8".to_string())
        }
        Some(Charset::Ascii) => {
            if bytes.iter().any(|b| *b > 0x7F) {
                return Err("not ASCII".into());
            }
            Ok((
                String::from_utf8_lossy(bytes).into_owned(),
                Encoded::Utf8 { bom: false },
            ))
        }
        Some(Charset::Latin1) => Ok((bytes.iter().map(|&b| b as char).collect(), Encoded::Latin1)),
        Some(Charset::Utf16Le) => match bytes.strip_prefix(&[0xFF, 0xFE]) {
            Some(rest) => utf16(rest, true, true),
            None => utf16(bytes, true, false),
        },
        Some(Charset::Utf16Be) => match bytes.strip_prefix(&[0xFE, 0xFF]) {
            Some(rest) => utf16(rest, false, true),
            None => utf16(bytes, false, false),
        },
        Some(Charset::Utf16) => match (
            bytes.strip_prefix(&[0xFF, 0xFE]),
            bytes.strip_prefix(&[0xFE, 0xFF]),
        ) {
            (Some(rest), _) => utf16(rest, true, true),
            (_, Some(rest)) => utf16(rest, false, true),
            _ => utf16(bytes, false, false),
        },
    }
}

fn encode(text: &str, how: Encoded) -> Result<Vec<u8>, String> {
    Ok(match how {
        Encoded::Detected(enc) => {
            if enc == rustpy_text::Encoding::Latin1 && text.chars().any(|c| c as u32 > 0xFF) {
                return Err("the formatted text does not fit ISO-8859-1".into());
            }
            enc.encode(text).map_err(|e| e.to_string())?
        }
        Encoded::Utf8 { bom } => (if bom {
            rustpy_text::Encoding::Utf8Bom
        } else {
            rustpy_text::Encoding::Utf8
        })
        .encode(text)
        .map_err(|e| e.to_string())?,
        Encoded::Utf16 { little, bom } => {
            let mut v = Vec::with_capacity(text.len() * 2 + 2);
            if bom {
                v.extend_from_slice(if little { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
            }
            for u in text.encode_utf16() {
                v.extend_from_slice(&if little {
                    u.to_le_bytes()
                } else {
                    u.to_be_bytes()
                });
            }
            v
        }
        Encoded::Latin1 => rustpy_text::Encoding::Latin1
            .encode(text)
            .map_err(|e| e.to_string())?,
    })
}

/// The result for one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Formatted,
    AlreadyFormatted,
    NeedsReformatting,
    Failed(String),
    Skipped(String),
}

/// Formats the file at `path` (or only checks it with `dry`).
pub fn format_file(
    path: &Path,
    charset: Option<Charset>,
    resolver: &mut StyleResolver,
    dry: bool,
) -> Outcome {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    // UTF-16 text is full of NUL bytes; anything else with one is binary.
    let utf16 = matches!(
        charset,
        Some(Charset::Utf16 | Charset::Utf16Le | Charset::Utf16Be)
    ) || bytes.starts_with(&[0xFF, 0xFE])
        || bytes.starts_with(&[0xFE, 0xFF]);
    if !utf16 && bytes[..bytes.len().min(8192)].contains(&0) {
        return Outcome::Skipped("binary file".into());
    }
    let (text, how) = match decode(&bytes, charset) {
        Ok(d) => d,
        Err(e) => return Outcome::Failed(e),
    };
    let opts = match resolver.decide(path, &text) {
        Decision::Format(o) => o,
        Decision::Skip(reason) => return Outcome::Skipped(reason),
    };
    let (ending, _) = LineEnding::detect(&text);
    let formatted = match rustpy_format::format_source(&text, &opts) {
        // The formatter keeps the first line's separator (like Black); the
        // file's own line ending applies to every line.
        Ok(f) => ending
            .apply(&f.replace("\r\n", "\n").replace('\r', "\n"))
            .into_owned(),
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    if formatted == text {
        return Outcome::AlreadyFormatted;
    }
    if dry {
        return Outcome::NeedsReformatting;
    }
    let out = match encode(&formatted, how) {
        Ok(b) => b,
        Err(e) => return Outcome::Failed(e),
    };
    match std::fs::write(path, out) {
        Ok(()) => Outcome::Formatted,
        Err(e) => Outcome::Failed(format!("cannot write: {e}")),
    }
}

/// Whether `path` (found under `root`) matches one of the masks: masks with a
/// `/` match the path relative to `root`, others the file name.
pub fn mask_matches(masks: &[String], root: &Path, path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    masks.iter().any(|m| {
        if m.contains('/') {
            glob_match(m.trim_start_matches("./"), &rel)
                || glob_match(&format!("**/{}", m.trim_start_matches('/')), &rel)
        } else {
            glob_match(m, &name)
        }
    })
}

/// Folders marked Excluded in the project that contains `dir`.
fn excluded_dirs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut d = Some(dir);
    while let Some(p) = d {
        if p.join(rustpy_settings::PROJECT_DIR_NAME).is_dir() {
            return rustpy_project::ProjectModel::try_load(p).map(|model| model.excluded);
        }
        d = p.parent();
    }
    Ok(Vec::new())
}

/// The files to look at under `dir`.
fn walk(dir: &Path, recursive: bool, resolver: &mut StyleResolver) -> Result<Vec<PathBuf>, String> {
    let mut files = if recursive {
        let canonical = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let excluded = excluded_dirs(&canonical)?;
        rustpy_search::project_files_excluding(dir, &excluded)
            .into_iter()
            .filter(|f| {
                let c = dunce::canonicalize(f).unwrap_or_else(|_| f.clone());
                !excluded.iter().any(|x| c.starts_with(x))
            })
            .collect::<Vec<_>>()
    } else {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| {
                        e.file_type().is_ok_and(|t| t.is_file())
                            && !e.file_name().to_string_lossy().starts_with('.')
                    })
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    };
    let excludes = resolver.black_excludes(dir);
    if !excludes.is_empty() {
        files.retain(|f| {
            let mut p = Some(f.as_path());
            while let Some(x) = p {
                if style::black_excluded(&excludes, x) {
                    return false;
                }
                if x == dir {
                    break;
                }
                p = x.parent();
            }
            true
        });
    }
    files.sort();
    Ok(files)
}

/// Counts for the summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub scanned: usize,
    pub formatted: usize,
    pub already: usize,
    pub needs: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Arguments that do not exist.
    pub missing: usize,
}

impl Summary {
    /// 2 for failures, 1 for files a dry run would change, otherwise 0.
    pub fn exit_code(&self) -> i32 {
        if self.failed > 0 || self.missing > 0 {
            2
        } else if self.needs > 0 {
            1
        } else {
            0
        }
    }
}

/// Runs `rustpy format` with the IDE settings `settings`; progress goes to `out`,
/// problems to `err`. Returns the exit status.
pub fn run(args: &FormatArgs, settings: Settings, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let style_file = match &args.settings {
        Some(path) => match style::load_style_file(path) {
            Ok(s) => Some(s),
            Err(e) => {
                let _ = writeln!(err, "rustpy format: {e}");
                return 2;
            }
        },
        None => None,
    };
    let charset = match &args.charset {
        Some(name) => match Charset::parse(name) {
            Some(c) => Some(c),
            None => {
                let _ = writeln!(err, "rustpy format: unsupported charset '{name}' (use UTF-8, UTF-16, UTF-16LE, UTF-16BE, ISO-8859-1 or US-ASCII)");
                return 2;
            }
        },
        None => None,
    };
    let masks: Vec<String> = if args.masks.is_empty() {
        DEFAULT_MASKS.iter().map(|m| m.to_string()).collect()
    } else {
        args.masks.clone()
    };
    let mut resolver = StyleResolver::new(settings, style_file, args.allow_defaults);
    let mut summary = Summary::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let verb = if args.dry { "Checking" } else { "Formatting" };
    for arg in &args.paths {
        let files: Vec<(PathBuf, bool)> = if arg.is_dir() {
            match walk(arg, args.recursive, &mut resolver) {
                Ok(files) => files,
                Err(error) => {
                    let _ = writeln!(err, "rustpy format: {error}");
                    return 1;
                }
            }
            .into_iter()
            .filter(|f| mask_matches(&masks, arg, f))
            .map(|f| (f, false))
            .collect()
        } else if arg.is_file() {
            vec![(arg.clone(), true)]
        } else {
            let _ = writeln!(
                err,
                "rustpy format: {}: no such file or directory",
                arg.display()
            );
            summary.missing += 1;
            continue;
        };
        for (file, explicit) in files {
            if !seen.insert(dunce::canonicalize(&file).unwrap_or_else(|_| file.clone())) {
                continue;
            }
            if explicit && !mask_matches(&masks, file.parent().unwrap_or(Path::new("")), &file) {
                let _ = writeln!(
                    out,
                    "Skipping {}: does not match the file masks ({})",
                    file.display(),
                    masks.join(",")
                );
                summary.skipped += 1;
                continue;
            }
            summary.scanned += 1;
            let outcome = format_file(&file, charset, &mut resolver, args.dry);
            let line = match &outcome {
                Outcome::Formatted => {
                    summary.formatted += 1;
                    format!("{verb} {}...Formatted", file.display())
                }
                Outcome::AlreadyFormatted => {
                    summary.already += 1;
                    format!("{verb} {}...Already formatted", file.display())
                }
                Outcome::NeedsReformatting => {
                    summary.needs += 1;
                    format!("{verb} {}...Needs reformatting", file.display())
                }
                Outcome::Failed(e) => {
                    summary.failed += 1;
                    format!("{verb} {}...Failed: {e}", file.display())
                }
                Outcome::Skipped(reason) => {
                    summary.skipped += 1;
                    format!("Skipping {}: {reason}", file.display())
                }
            };
            let _ = writeln!(out, "{line}");
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "{} file(s) scanned.", summary.scanned);
    if args.dry {
        let _ = writeln!(out, "{} file(s) need reformatting.", summary.needs);
    } else {
        let _ = writeln!(out, "{} file(s) formatted.", summary.formatted);
    }
    if summary.failed > 0 {
        let _ = writeln!(out, "{} file(s) could not be formatted.", summary.failed);
    }
    if summary.skipped > 0 {
        let _ = writeln!(out, "{} file(s) skipped.", summary.skipped);
    }
    summary.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    const UGLY: &str = "import os\ndef f( a,b ):\n    return {'x':a,'y':b}\n";

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rustpy")).unwrap();
        dir
    }

    fn run_format(args: FormatArgs) -> (i32, String, String) {
        run_with(args, Settings::default())
    }

    fn run_with(args: FormatArgs, settings: Settings) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, settings, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn formats_a_project_recursively_and_reports_each_file() {
        let dir = project();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pkg/sub")).unwrap();
        std::fs::write(root.join("pkg/a.py"), UGLY).unwrap();
        std::fs::write(root.join("pkg/sub/b.py"), "x = 1\n").unwrap();
        std::fs::write(root.join("pkg/notes.txt"), "not python {'x':1}\n").unwrap();
        std::fs::create_dir_all(root.join(".venv/lib")).unwrap();
        std::fs::write(root.join(".venv/lib/c.py"), UGLY).unwrap();
        let (code, out, err) = run_format(FormatArgs {
            recursive: true,
            paths: vec![root.to_path_buf()],
            ..Default::default()
        });
        assert_eq!(code, 0, "{out}{err}");
        assert!(
            out.contains(&format!(
                "Formatting {}...Formatted",
                root.join("pkg").join("a.py").display()
            )),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "Formatting {}...Already formatted",
                root.join("pkg").join("sub").join("b.py").display()
            )),
            "{out}"
        );
        assert!(
            !out.contains("notes.txt") && !out.contains(".venv"),
            "{out}"
        );
        assert!(
            out.contains("2 file(s) scanned.") && out.contains("1 file(s) formatted."),
            "{out}"
        );
        let a = std::fs::read_to_string(root.join("pkg/a.py")).unwrap();
        assert!(a.contains("def f(a, b):"), "{a}");
        assert_eq!(
            std::fs::read_to_string(root.join(".venv/lib/c.py")).unwrap(),
            UGLY,
            "virtual environments are skipped"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("pkg/notes.txt")).unwrap(),
            "not python {'x':1}\n"
        );
    }

    #[test]
    fn dry_run_reports_and_exits_one() {
        let dir = project();
        let f = dir.path().join("a.py");
        std::fs::write(&f, UGLY).unwrap();
        let (code, out, _) = run_format(FormatArgs {
            dry: true,
            paths: vec![f.clone()],
            ..Default::default()
        });
        assert_eq!(code, 1);
        assert!(
            out.contains(&format!("Checking {}...Needs reformatting", f.display())),
            "{out}"
        );
        assert!(out.contains("1 file(s) need reformatting."), "{out}");
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            UGLY,
            "-dry writes nothing"
        );
        run_format(FormatArgs {
            paths: vec![f.clone()],
            ..Default::default()
        });
        let (code, out, _) = run_format(FormatArgs {
            dry: true,
            paths: vec![f.clone()],
            ..Default::default()
        });
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("Already formatted"), "{out}");
    }

    #[test]
    fn non_recursive_directories_masks_and_explicit_files() {
        let dir = project();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("top.py"), UGLY).unwrap();
        std::fs::write(root.join("sub/deep.py"), UGLY).unwrap();
        std::fs::write(root.join("bin/tool"), UGLY).unwrap();
        let (_, out, _) = run_format(FormatArgs {
            dry: true,
            paths: vec![root.to_path_buf()],
            ..Default::default()
        });
        assert!(
            out.contains("top.py") && !out.contains("deep.py"),
            "without -r only the folder's own files: {out}"
        );
        let (_, out, _) = run_format(FormatArgs {
            dry: true,
            recursive: true,
            masks: vec!["bin/*".into()],
            paths: vec![root.to_path_buf()],
            ..Default::default()
        });
        let listing = out.replace('\\', "/"); // relative paths print with the platform separator
        assert!(
            listing.contains("bin/tool") && !listing.contains("top.py"),
            "{out}"
        );
        let (code, out, _) = run_format(FormatArgs {
            dry: true,
            paths: vec![root.join("bin/tool")],
            ..Default::default()
        });
        assert!(out.contains("does not match the file masks"), "{out}");
        assert_eq!(code, 0);
        let (code, out, _) = run_format(FormatArgs {
            dry: true,
            masks: vec!["tool".into()],
            paths: vec![root.join("bin/tool")],
            ..Default::default()
        });
        assert_eq!(code, 1, "{out}");
    }

    #[test]
    fn excluded_folders_are_skipped() {
        let dir = project();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("gen")).unwrap();
        std::fs::write(root.join("gen/g.py"), UGLY).unwrap();
        std::fs::write(root.join("m.py"), UGLY).unwrap();
        let ws = rustpy_settings::Workspace {
            excluded_dirs: vec![root.join("gen")],
            ..Default::default()
        };
        ws.save(&root).unwrap();
        let (_, out, _) = run_format(FormatArgs {
            dry: true,
            recursive: true,
            paths: vec![root.clone()],
            ..Default::default()
        });
        assert!(out.contains("m.py") && !out.contains("g.py"), "{out}");
    }

    #[test]
    fn syntax_errors_and_missing_paths_fail() {
        let dir = project();
        let bad = dir.path().join("bad.py");
        std::fs::write(&bad, "def f(:\n").unwrap();
        let (code, out, _) = run_format(FormatArgs {
            paths: vec![bad.clone()],
            ..Default::default()
        });
        assert_eq!(code, 2);
        assert!(out.contains("...Failed: cannot format: line 1"), "{out}");
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "def f(:\n");
        let (code, _, err) = run_format(FormatArgs {
            paths: vec![dir.path().join("nope.py")],
            ..Default::default()
        });
        assert_eq!(code, 2);
        assert!(err.contains("no such file or directory"), "{err}");
    }

    #[test]
    fn settings_file_overrides_the_style() {
        let dir = project();
        let f = dir.path().join("a.py");
        std::fs::write(
            &f,
            "x = ['aaaaaaaaaa', 'bbbbbbbbbb', 'cccccccccc', 'dddddddddd']\n",
        )
        .unwrap();
        let style = dir.path().join("style.xml");
        std::fs::write(&style, r#"<code_scheme name="narrow"><codeStyleSettings language="Python"><option name="RIGHT_MARGIN" value="40"/></codeStyleSettings></code_scheme>"#).unwrap();
        let (code, out, err) = run_format(FormatArgs {
            settings: Some(style),
            paths: vec![f.clone()],
            ..Default::default()
        });
        assert_eq!(code, 0, "{out}{err}");
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(
            text.lines().count() > 1 && text.lines().all(|l| l.len() <= 40),
            "split at 40 columns: {text}"
        );
        let (code, _, err) = run_format(FormatArgs {
            settings: Some(dir.path().join("missing.json")),
            paths: vec![f],
            ..Default::default()
        });
        assert_eq!(code, 2);
        assert!(err.contains("missing.json"), "{err}");
    }

    #[test]
    fn ide_code_style_and_black_configuration() {
        let dir = project();
        let f = dir.path().join("a.py");
        std::fs::write(&f, "x = {'a': 1}\n").unwrap();
        let mut s = Settings::default();
        s.code_style.quote_style = "double".into();
        let (code, _, _) = run_with(
            FormatArgs {
                paths: vec![f.clone()],
                ..Default::default()
            },
            s.clone(),
        );
        assert_eq!(code, 0);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "x = {\"a\": 1}\n");
        // Black as the formatter: [tool.black] skip-string-normalization keeps the quotes.
        std::fs::write(&f, "x = {'a': 1}\n").unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.black]\nskip-string-normalization = true\n",
        )
        .unwrap();
        s.code_style.formatter = "black".into();
        let (_, out, _) = run_with(
            FormatArgs {
                dry: true,
                paths: vec![f.clone()],
                ..Default::default()
            },
            s,
        );
        assert!(out.contains("Already formatted"), "{out}");
    }

    #[test]
    fn line_endings_and_encodings_are_kept() {
        let dir = project();
        let f = dir.path().join("crlf.py");
        std::fs::write(&f, "x=1\r\ny=2\r\n").unwrap();
        run_format(FormatArgs {
            paths: vec![f.clone()],
            ..Default::default()
        });
        assert_eq!(std::fs::read(&f).unwrap(), b"x = 1\r\ny = 2\r\n");
        let latin = dir.path().join("latin.py");
        std::fs::write(&latin, b"s='\xe9'\n").unwrap();
        let (code, out, _) = run_format(FormatArgs {
            charset: Some("ISO-8859-1".into()),
            paths: vec![latin.clone()],
            ..Default::default()
        });
        assert_eq!(code, 0, "{out}");
        assert_eq!(std::fs::read(&latin).unwrap(), b"s = '\xe9'\n");
        let (code, out, _) = run_format(FormatArgs {
            charset: Some("UTF-8".into()),
            paths: vec![latin.clone()],
            ..Default::default()
        });
        assert_eq!(code, 2, "{out}");
        assert!(out.contains("not valid UTF-8"), "{out}");
        let u16 = dir.path().join("u16.py");
        let mut bytes = vec![0xFF, 0xFE];
        for u in "a=1\n".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&u16, &bytes).unwrap();
        let (code, out, _) = run_format(FormatArgs {
            paths: vec![u16.clone()],
            ..Default::default()
        });
        assert_eq!(code, 0, "{out}");
        let mut expected = vec![0xFF, 0xFE];
        for u in "a = 1\n".encode_utf16() {
            expected.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(std::fs::read(&u16).unwrap(), expected);
        let (code, _, err) = run_format(FormatArgs {
            charset: Some("EBCDIC".into()),
            paths: vec![u16],
            ..Default::default()
        });
        assert_eq!(code, 2);
        assert!(err.contains("unsupported charset"), "{err}");
    }

    #[test]
    fn formatting_real_files_preserves_supported_bytes_or_returns_error() {
        let dir = project();
        let path = dir.path().join("m.py");
        for encoding in [
            rustpy_text::Encoding::Latin1,
            rustpy_text::Encoding::Utf8Bom,
            rustpy_text::Encoding::Utf16Le,
            rustpy_text::Encoding::Utf16Be,
        ] {
            let text = if encoding == rustpy_text::Encoding::Latin1 {
                "# coding: latin-1\r\ns='é'\r\n"
            } else {
                "s='é'\r\n"
            };
            std::fs::write(&path, encoding.encode(text).unwrap()).unwrap();
            let (code, output, _) = run_format(FormatArgs {
                paths: vec![path.clone()],
                ..Default::default()
            });
            assert_eq!(code, 0, "{output}");
            assert_eq!(
                std::fs::read(&path).unwrap(),
                encoding.encode(&text.replace("s=", "s = ")).unwrap()
            );
        }
        for bytes in [
            b"# coding: unknown-codec\nx=1\n".as_slice(),
            b"\xef\xbb\xbf\xff",
            b"\xff\xfea",
        ] {
            std::fs::write(&path, bytes).unwrap();
            let (code, output, _) = run_format(FormatArgs {
                paths: vec![path.clone()],
                ..Default::default()
            });
            assert_eq!(code, 2, "{output}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn detected_formats_are_strict_and_round_trip() {
        for encoding in [
            rustpy_text::Encoding::Latin1,
            rustpy_text::Encoding::Utf8Bom,
            rustpy_text::Encoding::Utf16Le,
            rustpy_text::Encoding::Utf16Be,
        ] {
            let text = if encoding == rustpy_text::Encoding::Latin1 {
                "# coding: latin-1\r\nx = 'é'\r\n"
            } else {
                "x = 'é'\r\n"
            };
            let bytes = encoding.encode(text).unwrap();
            let (decoded, how) = decode(&bytes, None).unwrap();
            assert_eq!(encode(&decoded, how).unwrap(), bytes);
            if encoding == rustpy_text::Encoding::Latin1 {
                assert!(encode(&(decoded + "😀"), how).is_err());
            }
        }
        for bytes in [
            b"\xef\xbb\xbf\xff".as_slice(),
            b"\xff\xfea",
            b"# coding: unknown\n",
        ] {
            assert!(decode(bytes, None).is_err());
        }
    }

    #[test]
    fn charsets() {
        assert_eq!(Charset::parse("utf-8"), Some(Charset::Utf8));
        assert_eq!(Charset::parse("UTF-16LE"), Some(Charset::Utf16Le));
        assert_eq!(Charset::parse("ISO-8859-1"), Some(Charset::Latin1));
        assert_eq!(Charset::parse("US-ASCII"), Some(Charset::Ascii));
        assert_eq!(Charset::parse("klingon"), None);
        assert_eq!(
            decode(b"\xFE\xFF\x00a", Some(Charset::Utf16)).unwrap().0,
            "a"
        );
        assert!(decode(b"\xC3", Some(Charset::Utf8)).is_err());
        assert!(decode("é".as_bytes(), Some(Charset::Ascii)).is_err());
    }

    #[test]
    fn masks() {
        let root = Path::new("/p");
        let m = |masks: &[&str], path: &str| {
            mask_matches(
                &masks.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                root,
                Path::new(path),
            )
        };
        assert!(m(&["*.py"], "/p/a/b.py"));
        assert!(!m(&["*.py"], "/p/a/b.pyc"));
        assert!(m(&["*.py", "*.pyi"], "/p/x.pyi"));
        assert!(m(&["bin/*"], "/p/bin/tool"));
        assert!(m(&["bin/*"], "/p/sub/bin/tool"));
        assert!(!m(&["bin/*"], "/p/lib/tool"));
    }
}

#[cfg(test)]
mod checked_workspace_format_tests {
    use super::*;
    #[test]
    fn actual_format_command_refuses_corrupt_exclusion_workspace_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("owned.py");
        std::fs::write(&file, b"x=1\n").unwrap();
        let path = rustpy_settings::Workspace::path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken exclusions").unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &FormatArgs {
                paths: vec![dir.path().to_path_buf()],
                recursive: true,
                allow_defaults: true,
                ..Default::default()
            },
            Settings::default(),
            &mut out,
            &mut err,
        );
        assert_eq!(code, 1);
        assert!(String::from_utf8(err).unwrap().contains("workspace.json"));
        assert_eq!(std::fs::read(file).unwrap(), b"x=1\n");
        assert_eq!(std::fs::read(path).unwrap(), b"broken exclusions");
    }
}

#[cfg(test)]
mod shared_project_exclusion_tests {
    use super::*;
    #[test]
    fn actual_format_command_obeys_modern_shared_project_excluded_roots() {
        let dir = tempfile::tempdir().unwrap();
        let excluded = dir.path().join("excluded");
        std::fs::create_dir(&excluded).unwrap();
        let hidden = excluded.join("owned.py");
        let visible = dir.path().join("visible.py");
        std::fs::write(&hidden, b"hidden=1\n").unwrap();
        std::fs::write(&visible, b"visible=2\n").unwrap();
        rustpy_project::ProjectModel {
            excluded: vec![excluded],
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &FormatArgs {
                paths: vec![dir.path().to_path_buf()],
                recursive: true,
                allow_defaults: true,
                ..Default::default()
            },
            Settings::default(),
            &mut out,
            &mut err,
        );
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
        assert_eq!(std::fs::read(hidden).unwrap(), b"hidden=1\n");
        assert_eq!(std::fs::read(visible).unwrap(), b"visible = 2\n");
    }
}
