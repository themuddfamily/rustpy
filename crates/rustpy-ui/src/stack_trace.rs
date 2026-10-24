//! Bounded, read-only analysis of pasted Python, JVM and native stack dumps.
use std::path::{Path, PathBuf};
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_LINES: usize = 20_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Python,
    Java,
    Native,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub path: String,
    pub line: usize,
    pub column: Option<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub row: usize,
    pub thread: usize,
    pub language: Language,
    pub symbol: String,
    pub source: Option<Source>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub title: String,
    pub state: Option<String>,
    pub language: Language,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    pub lines: Vec<String>,
    pub frames: Vec<Frame>,
    pub threads: Vec<Thread>,
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Exact(PathBuf),
    Ambiguous(Vec<PathBuf>),
    Unavailable,
}

pub fn bounded_text(text: &str) -> (&str, bool) {
    let mut end = text.len().min(MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], end < text.len())
}
fn number(text: &str) -> Option<usize> {
    let value = text.parse().ok()?;
    (value > 0).then_some(value)
}
fn source_location(text: &str) -> Option<Source> {
    let text = text.trim().trim_end_matches([')', ',']);
    let (before, last) = text.rsplit_once(':')?;
    let last = number(last)?;
    if let Some((path, line)) = before
        .rsplit_once(':')
        .and_then(|(path, line)| Some((path, number(line)?)))
    {
        Some(Source {
            path: path.into(),
            line,
            column: Some(last),
        })
    } else {
        Some(Source {
            path: before.into(),
            line: last,
            column: None,
        })
    }
}
fn python_frame(line: &str) -> Option<(String, Source)> {
    let text = line.trim_start().trim_start_matches(['|', '+', ' ']);
    let rest = text
        .strip_prefix("File \"")
        .or_else(|| text.strip_prefix("File '"))?;
    let delimiter = if text.starts_with("File \"") {
        "\", line "
    } else {
        "', line "
    };
    let (path, tail) = rest.split_once(delimiter)?;
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    let line = number(&tail[..digits])?;
    let symbol = tail[digits..]
        .strip_prefix(", in ")
        .or_else(|| tail[digits..].strip_prefix(" in "))
        .unwrap_or("")
        .trim()
        .to_string();
    Some((
        symbol,
        Source {
            path: path.into(),
            line,
            column: None,
        },
    ))
}
fn java_frame(line: &str) -> Option<(String, Option<Source>)> {
    let text = line.trim().strip_prefix("at ")?;
    let (symbol, location) = text.rsplit_once('(')?;
    let location = location.strip_suffix(')')?;
    if !symbol.contains('.') {
        return None;
    }
    Some((symbol.into(), source_location(location)))
}
fn native_frame(line: &str) -> Option<(String, Option<Source>)> {
    let text = line.trim_start().trim_start_matches("* ");
    let numbered = text
        .strip_prefix('#')
        .is_some_and(|tail| tail.starts_with(|ch: char| ch.is_ascii_digit()))
        || text.starts_with("frame #");
    let apple = text
        .split_whitespace()
        .next()
        .is_some_and(|token| !token.is_empty() && token.chars().all(|ch| ch.is_ascii_digit()))
        && text.contains("0x");
    if !numbered && !apple {
        return None;
    }
    let source = if let Some((_, location)) = text.rsplit_once(" at ") {
        source_location(location)
    } else if let Some((_, location)) = text.rsplit_once('(') {
        source_location(location)
    } else {
        text.split_whitespace().last().and_then(source_location)
    };
    Some((text.to_string(), source))
}
fn thread_header(line: &str) -> Option<Thread> {
    let text = line.trim();
    let python = (text.starts_with("Thread ")
        || text.starts_with("Current thread ")
        || text.starts_with("Stack "))
        && text.contains("most recent call first");
    let java = text.starts_with('"')
        && text[1..].contains('"')
        && [" tid=", " nid=", " prio=", " #", " daemon"]
            .iter()
            .any(|part| text.contains(part));
    let native = text.starts_with("Thread ")
        || text.starts_with("thread #")
        || text.starts_with("* thread #");
    (python || java || native).then(|| Thread {
        title: text.to_string(),
        state: None,
        language: if python {
            Language::Python
        } else if java {
            Language::Java
        } else {
            Language::Unknown
        },
    })
}
pub fn parse(text: &str) -> Analysis {
    let (text, truncated) = bounded_text(text);
    let mut result = Analysis {
        truncated,
        ..Default::default()
    };
    let mut current = None;
    for (row, line) in text.lines().enumerate() {
        if row >= MAX_LINES {
            result.truncated = true;
            break;
        }
        // Preserve source text; terminal SGR color escapes carry no location data.
        let line = strip_ansi(line.trim_end_matches('\r'));
        if let Some(thread) = thread_header(&line) {
            result.threads.push(thread);
            current = Some(result.threads.len() - 1);
        }
        if let Some(state) = line.trim().strip_prefix("java.lang.Thread.State:") {
            if let Some(index) = current {
                result.threads[index].state = Some(state.trim().into());
            }
        }
        let frame = python_frame(&line)
            .map(|(symbol, source)| (Language::Python, symbol, Some(source)))
            .or_else(|| java_frame(&line).map(|(symbol, source)| (Language::Java, symbol, source)))
            .or_else(|| {
                native_frame(&line).map(|(symbol, source)| (Language::Native, symbol, source))
            });
        if let Some((language, symbol, source)) = frame {
            let thread = *current.get_or_insert_with(|| {
                result.threads.push(Thread {
                    title: "Stack trace".into(),
                    state: None,
                    language,
                });
                result.threads.len() - 1
            });
            if result.threads[thread].language == Language::Unknown {
                result.threads[thread].language = language;
            }
            result.frames.push(Frame {
                row,
                thread,
                language,
                symbol,
                source,
            });
        }
        result.lines.push(line);
    }
    result
}
pub fn looks_like_dump(text: &str) -> bool {
    let analysis = parse(text);
    !analysis.frames.is_empty()
        && (analysis.frames.len() > 1
            || analysis.frames.iter().any(|frame| frame.source.is_some())
            || text.contains("Traceback (")
            || text.contains("Exception")
            || text.contains("most recent call first")
            || text.contains("thread #")
            || text.contains("Thread "))
}
fn strip_ansi(text: &str) -> String {
    let mut chars = text.chars().peekable();
    let mut result = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}
fn absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with("\\\\") || path.as_bytes().get(1) == Some(&b':')
}
fn normalized(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
/// Resolve only existing exact paths or a unique JVM/package/file match.
/// A missing absolute path never falls back to an unrelated same-named file.
/// Remote prefixes are translated only through the explicit user mapping.
pub fn resolve(
    frame: &Frame,
    roots: &[PathBuf],
    files: &[PathBuf],
    mapping: Option<(&str, &Path)>,
) -> Resolution {
    let Some(source) = &frame.source else {
        return Resolution::Unavailable;
    };
    if source.path.starts_with('<') || source.path.is_empty() {
        return Resolution::Unavailable;
    }
    let mut hint = source.path.replace('\\', "/");
    if let Some((remote, local)) = mapping.filter(|(remote, _)| !remote.is_empty()) {
        let prefix = remote.replace('\\', "/");
        let prefix = prefix.trim_end_matches('/');
        if hint == prefix
            || hint
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('/'))
        {
            let tail = hint[prefix.len()..].trim_start_matches('/');
            let mapped = local.join(tail);
            return if mapped.is_file() {
                Resolution::Exact(mapped)
            } else {
                Resolution::Unavailable
            };
        }
    }
    if hint.starts_with("//") {
        return Resolution::Unavailable;
    }
    if absolute(&hint) {
        let path = PathBuf::from(&source.path);
        return if path.is_file() {
            Resolution::Exact(path)
        } else {
            Resolution::Unavailable
        };
    }
    if frame.language == Language::Java && !hint.contains('/') {
        let symbol = frame.symbol.rsplit('/').next().unwrap_or(&frame.symbol);
        if let Some((class, _)) = symbol.rsplit_once('.') {
            let class = class.split('$').next().unwrap_or(class);
            if let Some((package, _)) = class.rsplit_once('.') {
                hint = format!("{}/{}", package.replace('.', "/"), hint);
            }
        }
    }
    let relative = PathBuf::from(&hint);
    let mut matches: Vec<_> = roots
        .iter()
        .flat_map(|root| {
            [
                root.join(&relative),
                root.join("src/main/java").join(&relative),
                root.join("src/test/java").join(&relative),
            ]
        })
        .filter(|path| path.is_file())
        .collect();
    if matches.is_empty() {
        matches.extend(
            files
                .iter()
                .filter(|path| {
                    path.is_file()
                        && (normalized(path) == hint
                            || normalized(path).ends_with(&format!("/{hint}")))
                })
                .cloned(),
        );
    }
    matches.sort();
    matches.dedup();
    // Distinct lexical paths to the same actual file are one exact source.
    let mut canonical = std::collections::BTreeSet::new();
    matches.retain(|path| {
        canonical.insert(dunce::canonicalize(path).unwrap_or_else(|_| path.clone()))
    });
    match matches.len() {
        0 => Resolution::Unavailable,
        1 => Resolution::Exact(matches.remove(0)),
        _ => Resolution::Ambiguous(matches),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PYTHON: &str = include_str!("fixtures/stack_trace/python.txt");
    const JAVA: &str = include_str!("fixtures/stack_trace/java.txt");
    const NATIVE: &str = include_str!("fixtures/stack_trace/native.txt");
    #[test]
    fn python_chains_windows_paths_and_faulthandler_threads_keep_source_lines() {
        let parsed = parse(PYTHON);
        assert_eq!(parsed.frames.len(), 5);
        assert_eq!(parsed.threads.len(), 3);
        assert_eq!(
            parsed.frames[0].source.as_ref().unwrap().path,
            "/srv/My Project/main.py"
        );
        assert_eq!(
            parsed.frames[2].source.as_ref().unwrap().path,
            r"C:\Repo With Spaces\main.py"
        );
        assert_eq!(parsed.frames[2].source.as_ref().unwrap().line, 13);
        assert!(parsed
            .lines
            .iter()
            .any(|line| line == "RuntimeError: chained"));
        assert!(looks_like_dump(PYTHON));
    }
    #[test]
    fn java_threads_states_module_names_and_native_methods_stay_distinct() {
        let parsed = parse(JAVA);
        assert_eq!(parsed.threads.len(), 2);
        assert_eq!(parsed.frames.len(), 4);
        assert!(parsed.threads[0]
            .state
            .as_deref()
            .unwrap()
            .starts_with("BLOCKED"));
        assert_eq!(parsed.threads[1].state.as_deref(), Some("RUNNABLE"));
        assert_eq!(parsed.frames[0].symbol, "app.module/com.example.Worker.run");
        assert!(parsed.frames[1].source.is_none());
        assert_eq!(parsed.frames[2].thread, 1);
    }
    #[test]
    fn native_gdb_lldb_windows_columns_and_apple_symbols_are_retained() {
        let parsed = parse(NATIVE);
        assert_eq!(parsed.threads.len(), 2);
        assert_eq!(parsed.frames.len(), 4);
        let windows = parsed.frames[1].source.as_ref().unwrap();
        assert_eq!(windows.path, r"C:\work\app.cpp");
        assert_eq!((windows.line, windows.column), (19, Some(3)));
        assert_eq!(
            parsed.frames[2].source.as_ref().unwrap().path,
            "/build/My Project/lib.rs"
        );
        assert!(parsed.frames[3].source.is_none());
    }
    #[test]
    fn exact_package_resolution_and_explicit_mapping_do_not_guess_missing_absolute_paths() {
        let directory = tempfile::tempdir().unwrap();
        let java = directory
            .path()
            .join("src/main/java/com/example/Worker.java");
        std::fs::create_dir_all(java.parent().unwrap()).unwrap();
        std::fs::write(&java, "class Worker {}\n").unwrap();
        let frame = &parse(JAVA).frames[0];
        assert_eq!(
            resolve(frame, &[directory.path().into()], &[], None),
            Resolution::Exact(java)
        );
        let mapped = directory.path().join("main.py");
        std::fs::write(&mapped, "print('mapped')\n").unwrap();
        let frame = &parse(PYTHON).frames[0];
        assert_eq!(
            resolve(
                frame,
                &[directory.path().into()],
                std::slice::from_ref(&mapped),
                None
            ),
            Resolution::Unavailable
        );
        assert_eq!(
            resolve(frame, &[], &[], Some(("/srv/My Project", directory.path()))),
            Resolution::Exact(mapped)
        );
    }
    #[test]
    fn ambiguous_relative_sources_remain_unlinked_and_explicit_root_disambiguates() {
        let directory = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for module in ["one", "two"] {
            let path = directory.path().join(module).join("sample.py");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "pass\n").unwrap();
            files.push(path);
        }
        let frame=&parse("Traceback (most recent call last):\n  File \"sample.py\", line 1, in run\nError: sample\n").frames[0];
        assert!(
            matches!(resolve(frame,&[],&files,None),Resolution::Ambiguous(paths)if paths.len()==2)
        );
        assert_eq!(
            resolve(frame, &[directory.path().join("one")], &files, None),
            Resolution::Exact(files[0].clone())
        );
    }
    #[test]
    fn malformed_locations_virtual_files_ansi_and_size_limits_are_safe() {
        assert!(!looks_like_dump(
            "ordinary copied prose\nwith no stack frames"
        ));
        assert!(parse("at com.Foo.method(Foo.java:0)").frames[0]
            .source
            .is_none());
        let parsed = parse("\u{1b}[31m  File \"<stdin>\", line 1, in run\u{1b}[0m");
        assert_eq!(
            resolve(&parsed.frames[0], &[], &[], None),
            Resolution::Unavailable
        );
        let source = "π".repeat(MAX_BYTES / 2 + 3);
        let (text, truncated) = bounded_text(&source);
        assert!(truncated);
        assert_eq!(text.len(), MAX_BYTES);
        assert!(parse(&"x\n".repeat(MAX_LINES + 1)).truncated);
        let unc = Frame {
            row: 0,
            thread: 0,
            language: Language::Native,
            symbol: String::new(),
            source: Some(Source {
                path: r"\\remote-host\share\file.c".into(),
                line: 1,
                column: None,
            }),
        };
        assert_eq!(resolve(&unc, &[], &[], None), Resolution::Unavailable);
    }
}
