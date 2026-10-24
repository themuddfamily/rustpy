//! `.editorconfig` support: walks from the file's directory upwards,
//! collects matching sections and resolves indent/line-length properties.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod document;
mod glob;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct EditorConfigProps {
    pub indent_style: Option<String>,
    pub indent_size: Option<usize>,
    /// Use the effective editor tab width even when no tab_width is specified.
    pub indent_size_is_tab: bool,
    pub tab_width: Option<usize>,
    pub max_line_length: Option<usize>,
    /// Explicit `off` disables wrapping instead of inheriting the IDE margin.
    pub max_line_length_off: bool,
    pub ide: IdeCodeStyle,
    /// Unapplied IDE properties are retained, never advertised as supported.
    pub unsupported_ide: BTreeMap<String, String>,
    pub end_of_line: Option<String>,
    pub insert_final_newline: Option<bool>,
    pub trim_trailing_whitespace: Option<bool>,
    pub charset: Option<String>,
    pub spelling_language: Option<String>,
}

/// Verified `ij_*` `.editorconfig` properties with actual consumers in this IDE.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct IdeCodeStyle {
    pub python: BTreeMap<String, String>,
    pub wrap_on_typing: Option<bool>,
    /// None inherits the IDE guides; Some(empty) explicitly clears them.
    pub visual_guides: Option<Vec<usize>>,
    pub continuation_indent_size: Option<usize>,
    pub continuation_parameters: Option<bool>,
    pub continuation_arguments: Option<bool>,
    pub continuation_collections: Option<bool>,
    pub formatter_enabled: Option<bool>,
    pub smart_tabs: Option<bool>,
    pub formatter_tags_enabled: Option<bool>,
    pub formatter_off_tag: Option<String>,
    pub formatter_on_tag: Option<String>,
    pub blank_lines_top_level: Option<usize>,
    pub blank_lines_in_class: Option<usize>,
    pub quote_style: Option<String>,
    pub magic_trailing_comma: Option<bool>,
    pub keep_line_breaks: Option<bool>,
    pub keep_simple_blocks: Option<bool>,
    pub keep_first_column_comments: Option<bool>,
}

impl IdeCodeStyle {
    fn from_properties(values: &BTreeMap<String, String>, python: bool) -> Self {
        let boolean = |key: &str| match values.get(key).map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        };
        let number = |key: &str| {
            values
                .get(key)
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n <= 100)
        };
        Self {
            python: if python {
                values
                    .iter()
                    .filter(|(key, value)| {
                        document::valid_value(key, value)
                            && crate::python_style::PythonStyle::default()
                                .import_property(key, value)
                    })
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            } else {
                BTreeMap::new()
            },
            quote_style: python
                .then(|| {
                    values
                        .get("ij_python_quote_style")
                        .filter(|v| matches!(v.as_str(), "preserve" | "single" | "double"))
                        .cloned()
                })
                .flatten(),
            magic_trailing_comma: python
                .then(|| boolean("ij_python_magic_trailing_comma"))
                .flatten(),
            keep_line_breaks: python
                .then(|| boolean("ij_python_keep_line_breaks"))
                .flatten(),
            keep_simple_blocks: python
                .then(|| boolean("ij_python_keep_simple_blocks_in_one_line"))
                .flatten(),
            keep_first_column_comments: python
                .then(|| boolean("ij_python_keep_first_column_comment"))
                .flatten(),
            wrap_on_typing: boolean("ij_wrap_on_typing"),
            visual_guides: values
                .get("ij_visual_guides")
                .and_then(|v| parse_visual_guides(v)),
            formatter_enabled: boolean("ij_formatter_enabled"),
            smart_tabs: boolean("ij_smart_tabs"),
            formatter_tags_enabled: boolean("ij_formatter_tags_enabled"),
            formatter_off_tag: values
                .get("ij_formatter_off_tag")
                .filter(|v| v.len() <= 256 && !v.eq_ignore_ascii_case("unset"))
                .cloned(),
            formatter_on_tag: values
                .get("ij_formatter_on_tag")
                .filter(|v| v.len() <= 256 && !v.eq_ignore_ascii_case("unset"))
                .cloned(),
            continuation_indent_size: if python {
                values
                    .get("ij_continuation_indent_size")
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|n| (1..=256).contains(n))
            } else {
                None
            },
            continuation_parameters: python
                .then(|| boolean("ij_python_use_continuation_indent_for_parameters"))
                .flatten(),
            continuation_arguments: python
                .then(|| boolean("ij_python_use_continuation_indent_for_arguments"))
                .flatten(),
            continuation_collections: python
                .then(|| {
                    boolean("ij_python_use_continuation_indent_for_collection_and_comprehensions")
                })
                .flatten(),
            blank_lines_top_level: python
                .then(|| number("ij_python_blank_lines_around_top_level_classes_functions"))
                .flatten(),
            // A language-specific value takes precedence over the common option.
            // `unset` on the specific key removes that override and exposes the common value.
            blank_lines_in_class: if python {
                number("ij_python_blank_lines_around_method")
                    .or_else(|| number("ij_any_blank_lines_around_method"))
            } else {
                None
            },
        }
    }
}

/// Matches upstream VisualGuidesAccessor: comma-separated signed 32-bit columns,
/// malformed/negative entries discarded, explicit empty list permitted. `unset`
/// inherits instead of clearing. Bound adversarial lists before allocating.
fn parse_visual_guides(value: &str) -> Option<Vec<usize>> {
    if value.eq_ignore_ascii_case("unset") || value.split(',').take(129).count() > 128 {
        return None;
    }
    let mut guides: Vec<_> = value
        .split(',')
        .filter_map(|part| part.trim().parse::<i32>().ok())
        .filter(|column| *column >= 0)
        .map(|column| column as usize)
        .collect();
    guides.sort_unstable();
    guides.dedup();
    Some(guides)
}

/// EditorConfig wildcard matching (including nested alternatives, decimal
/// ranges, escaped characters and literal bracket sequences).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    glob::Glob::compile(pattern).is_some_and(|glob| glob.matches(path))
}

fn section_matches(section_dir: &Path, pattern: &str, file: &Path) -> bool {
    let rel = file
        .strip_prefix(section_dir)
        .unwrap_or(file)
        .to_string_lossy();
    let rel = if cfg!(windows) {
        rel.replace('\\', "/")
    } else {
        rel.into_owned()
    };
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if glob::has_separator(pattern) {
        let pat = pattern.trim_start_matches('/');
        glob_match(pat, &rel)
    } else {
        glob_match(pattern, &name) || glob_match(&format!("**/{pattern}"), &rel)
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedValue {
    pub value: String,
    pub config: PathBuf,
    pub range: std::ops::Range<usize>,
}
#[derive(Clone, Debug, Default)]
pub struct Resolution {
    pub props: EditorConfigProps,
    pub values: BTreeMap<String, ResolvedValue>,
    pub configs: Vec<PathBuf>,
    pub errors: Vec<String>,
}
fn normalized(path: &Path) -> PathBuf {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    result
}
/// Resolve with the origin of every effective raw property retained for preview.
pub fn resolve_detailed(file: &Path) -> Resolution {
    resolve_with_overlay(file, None)
}
/// Preview an unsaved `.editorconfig` without changing the disk or shared cache.
pub fn resolve_with_overlay(file: &Path, overlay: Option<(&Path, &str)>) -> Resolution {
    use std::io::Read;
    let file = normalized(file);
    let overlay = overlay.map(|(path, text)| (normalized(path), text));
    let mut result = Resolution::default();
    let mut files = Vec::new();
    let mut dir = file.parent();
    while let Some(directory) = dir {
        let path = directory.join(".editorconfig");
        let text = if let Some((_, text)) = overlay.as_ref().filter(|(overlay, _)| overlay == &path)
        {
            Ok((*text).to_string())
        } else {
            std::fs::File::open(&path).and_then(|file| {
                let mut text = String::new();
                file.take(8 * 1024 * 1024 + 1).read_to_string(&mut text)?;
                Ok(text)
            })
        };
        match text {
            Ok(text) => {
                let document = document::parse(&text);
                result.configs.push(path.clone());
                if text.len() > 8 * 1024 * 1024 {
                    result
                        .errors
                        .push(format!("{} exceeds 8 MiB", path.display()));
                } else {
                    let root = document.root;
                    files.push((path, document));
                    if root {
                        break;
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => result.errors.push(format!("{}: {error}", path.display())),
        }
        dir = directory.parent();
    }
    for (path, document) in files.iter().rev() {
        for section in &document.sections {
            if section_matches(
                path.parent().unwrap_or(Path::new("")),
                &section.pattern,
                &file,
            ) {
                for property in &section.properties {
                    if property.value.eq_ignore_ascii_case("unset") {
                        result.values.remove(&property.key);
                    } else {
                        result.values.insert(
                            property.key.clone(),
                            ResolvedValue {
                                value: property.value.clone(),
                                config: path.clone(),
                                range: property.value_range.clone(),
                            },
                        );
                    }
                }
            }
        }
    }
    let merged: BTreeMap<_, _> = result
        .values
        .iter()
        .map(|(key, value)| (key.clone(), value.value.clone()))
        .collect();
    result.props = props_from_properties(&file, &merged);
    result
}
/// Resolves the properties applying to `file` (closest `.editorconfig` wins).
pub fn resolve(file: &Path) -> EditorConfigProps {
    resolve_detailed(file).props
}
fn props_from_properties(file: &Path, values: &BTreeMap<String, String>) -> EditorConfigProps {
    let mut merged = values.clone();
    // Standard enumerations are case-insensitive; custom values such as
    // formatter tags keep their spelling and punctuation.
    for key in [
        "indent_style",
        "indent_size",
        "charset",
        "end_of_line",
        "max_line_length",
        "ij_python_quote_style",
    ] {
        if let Some(value) = merged.get_mut(key) {
            value.make_ascii_lowercase();
        }
    }
    for (key, value) in &mut merged {
        if key.starts_with("ij_python_") && !document::values(key).is_empty() {
            value.make_ascii_lowercase();
        }
    }
    let get_usize = |k: &str| {
        merged
            .get(k)
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|value| *value > 0)
    };
    // `unset` (or an unknown value) removes a property instead of turning it off.
    let get_bool = |k: &str| match merged.get(k).map(|v| v.to_ascii_lowercase()).as_deref() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    let python = file
        .extension()
        .is_some_and(|e| matches!(e.to_str(), Some("py" | "pyi" | "pyx" | "pxd")));
    let known = [
        "ij_formatter_tags_enabled",
        "ij_formatter_off_tag",
        "ij_formatter_on_tag",
        "ij_smart_tabs",
        "ij_continuation_indent_size",
        "ij_python_use_continuation_indent_for_parameters",
        "ij_python_use_continuation_indent_for_arguments",
        "ij_python_use_continuation_indent_for_collection_and_comprehensions",
        "ij_visual_guides",
        "ij_wrap_on_typing",
        "ij_formatter_enabled",
        "ij_python_blank_lines_around_top_level_classes_functions",
        "ij_python_quote_style",
        "ij_python_magic_trailing_comma",
        "ij_python_keep_line_breaks",
        "ij_python_keep_simple_blocks_in_one_line",
        "ij_python_keep_first_column_comment",
        "ij_python_blank_lines_around_method",
        "ij_any_blank_lines_around_method",
    ];
    let mut props = EditorConfigProps {
        indent_style: merged
            .get("indent_style")
            .filter(|v| v.eq_ignore_ascii_case("tab") || v.eq_ignore_ascii_case("space"))
            .cloned(),
        indent_size: get_usize("indent_size"),
        indent_size_is_tab: merged
            .get("indent_size")
            .is_some_and(|v| v.eq_ignore_ascii_case("tab")),
        tab_width: get_usize("tab_width"),
        max_line_length: get_usize("max_line_length"),
        max_line_length_off: merged
            .get("max_line_length")
            .is_some_and(|v| v.eq_ignore_ascii_case("off")),
        ide: IdeCodeStyle::from_properties(&merged, python),
        unsupported_ide: merged
            .iter()
            .filter(|(k, v)| {
                k.starts_with("ij_")
                    && !known.contains(&k.as_str())
                    && !(python
                        && crate::python_style::PythonStyle::default().import_property(k, v))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        end_of_line: merged
            .get("end_of_line")
            .filter(|value| matches!(value.as_str(), "lf" | "cr" | "crlf"))
            .cloned(),
        insert_final_newline: get_bool("insert_final_newline"),
        trim_trailing_whitespace: get_bool("trim_trailing_whitespace"),
        charset: merged
            .get("charset")
            .filter(|value| {
                matches!(
                    value.as_str(),
                    "latin1" | "utf-8" | "utf-8-bom" | "utf-16le" | "utf-16be"
                )
            })
            .cloned(),
        spelling_language: merged
            .get("spelling_language")
            .and_then(|value| document::spelling_language(value)),
    };
    if props.indent_style.as_deref() == Some("tab") && !merged.contains_key("indent_size") {
        props.indent_size_is_tab = true;
        props.indent_size = props.tab_width;
    }
    if props.indent_size.is_none()
        && merged
            .get("indent_size")
            .is_some_and(|v| v.eq_ignore_ascii_case("tab"))
    {
        props.indent_size = props.tab_width;
    }
    if props.tab_width.is_none() {
        props.tab_width = props.indent_size;
    }
    props
}

/// How long [`resolve_cached`] reuses a result before reading the files again.
const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

type Cache =
    std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::Instant, EditorConfigProps)>>;

fn cache() -> &'static Cache {
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// [`resolve`] with a short-lived per-path cache: the editor asks every frame,
/// and edits of `.editorconfig` files still apply within a couple of seconds
/// (saving one in the IDE calls [`invalidate_cache`]).
pub fn resolve_cached(file: &Path) -> EditorConfigProps {
    let now = std::time::Instant::now();
    if let Some((_, props)) = cache().lock().ok().and_then(|map| {
        map.get(file)
            .filter(|(at, _)| now.duration_since(*at) < CACHE_TTL)
            .cloned()
    }) {
        return props;
    }
    let props = resolve(file);
    if let Ok(mut map) = cache().lock() {
        if map.len() > 1024 {
            map.clear();
        }
        map.insert(file.to_path_buf(), (now, props.clone()));
    }
    props
}

/// Forgets cached [`resolve_cached`] results (an `.editorconfig` changed).
pub fn invalidate_cache() {
    if let Ok(mut map) = cache().lock() {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detailed_resolution_and_unsaved_preview_preserve_origins_unset_and_parent_files() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join(".editorconfig");
        let sub = root.path().join("pkg");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(&config, "root = true\n[*.py]\nindent_style = TAB\nindent_size = 4\ncharset = UTF-8\nend_of_line = CRLF\nspelling_language = EN-gb\ncustom = Keep My Case # literal\n").unwrap();
        let child = sub.join(".editorconfig");
        std::fs::write(&child, "[test{1..3}.py]\nindent_size = 2\n").unwrap();
        let file = sub.join("test2.py");
        let result = resolve_detailed(&file);
        assert_eq!(result.props.indent_size, Some(2));
        assert_eq!(result.props.indent_style.as_deref(), Some("tab"));
        assert_eq!(result.props.end_of_line.as_deref(), Some("crlf"));
        assert_eq!(result.props.charset.as_deref(), Some("utf-8"));
        assert_eq!(result.props.spelling_language.as_deref(), Some("en-GB"));
        assert_eq!(result.values["indent_size"].config, child);
        assert_eq!(result.values["custom"].value, "Keep My Case # literal");
        let overlay = "[*.py]\nindent_size = unset\ncharset = UTF-16LE\n";
        let preview = resolve_with_overlay(&file, Some((&child, overlay)));
        assert_eq!(preview.props.indent_size, None);
        assert!(preview.props.indent_size_is_tab);
        assert_eq!(preview.props.charset.as_deref(), Some("utf-16le"));
        assert_eq!(
            &overlay[preview.values["charset"].range.clone()],
            "UTF-16LE"
        );
        assert_eq!(
            resolve(&file).indent_size,
            Some(2),
            "preview never writes or replaces cached settings"
        );
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.py", "main.py"));
        assert!(!glob_match("*.py", "dir/main.py"));
        assert!(glob_match("**/*.py", "dir/sub/main.py"));
        assert!(glob_match("*.{py,pyi}", "x.pyi"));
        assert!(glob_match("test_?.py", "test_a.py"));
        assert!(glob_match("[a-c]*.py", "bx.py"));
        assert!(!glob_match("[!a-c]*.py", "bx.py"));
    }

    #[test]
    fn resolves_nearest_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".editorconfig"), "root = true\n[*]\nindent_style = space\nindent_size = 2\nmax_line_length = 100\n[*.py]\nindent_size = 4\n").unwrap();
        let sub = dir.path().join("pkg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".editorconfig"), "[*.py]\nmax_line_length = 79\n").unwrap();
        let props = resolve(&sub.join("mod.py"));
        assert_eq!(props.indent_size, Some(4));
        assert_eq!(props.max_line_length, Some(79));
        assert_eq!(props.indent_style.as_deref(), Some("space"));
        let txt = resolve(&sub.join("notes.txt"));
        assert_eq!(txt.indent_size, Some(2));
        assert_eq!(txt.max_line_length, Some(100));
    }

    #[test]
    fn unset_removes_a_property() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".editorconfig"), "root = true\n[*]\nindent_style = tab\ntrim_trailing_whitespace = true\ninsert_final_newline = true\n[*.py]\nindent_style = unset\ntrim_trailing_whitespace = unset\ninsert_final_newline = FALSE\n").unwrap();
        let py = resolve(&dir.path().join("a.py"));
        assert_eq!(py.indent_style, None, "unset is not 'spaces'");
        assert_eq!(
            py.trim_trailing_whitespace, None,
            "unset is not 'false': the IDE setting applies"
        );
        assert_eq!(py.insert_final_newline, Some(false));
        let txt = resolve(&dir.path().join("a.txt"));
        assert_eq!(
            (txt.indent_style.as_deref(), txt.trim_trailing_whitespace),
            (Some("tab"), Some(true))
        );
    }

    #[test]
    fn cached_resolution_until_invalidated() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.py");
        std::fs::write(
            dir.path().join(".editorconfig"),
            "root = true\n[*.py]\nindent_size = 2\n",
        )
        .unwrap();
        assert_eq!(resolve_cached(&file).indent_size, Some(2));
        std::fs::write(
            dir.path().join(".editorconfig"),
            "root = true\n[*.py]\nindent_size = 3\n",
        )
        .unwrap();
        assert_eq!(
            resolve_cached(&file).indent_size,
            Some(2),
            "served from the cache"
        );
        invalidate_cache();
        assert_eq!(resolve_cached(&file).indent_size, Some(3));
    }
}

#[cfg(test)]
mod ide_tests {
    use super::*;
    #[test]
    fn bom_root_tab_off_and_unapplied_properties_are_lossless() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".editorconfig"), "[*]\nindent_size=9\n").unwrap();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join(".editorconfig"), "\u{feff}root=TRUE\n[*]\nindent_size=TAB\ntab_width=3\nmax_line_length=OFF\nij_wrap_on_typing=true\nij_unknown_future=Keep Value\n").unwrap();
        let ec = resolve(&sub.join("a.py"));
        assert_eq!(ec.indent_size, Some(3));
        assert!(ec.max_line_length_off);
        assert_eq!(ec.ide.wrap_on_typing, Some(true));
        assert_eq!(ec.unsupported_ide["ij_unknown_future"], "Keep Value");
    }
    #[test]
    fn python_specific_common_nested_unset_invalid_and_other_languages() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".editorconfig"), "root=true\n[*]\nij_any_blank_lines_around_method=2\nij_python_blank_lines_around_method=3\nij_python_blank_lines_around_top_level_classes_functions=1\nij_formatter_enabled=false\n").unwrap();
        let py = resolve(&root.path().join("a.py"));
        assert_eq!(py.ide.blank_lines_in_class, Some(3));
        assert_eq!(py.ide.blank_lines_top_level, Some(1));
        assert_eq!(py.ide.formatter_enabled, Some(false));
        assert_eq!(
            resolve(&root.path().join("a.js")).ide.blank_lines_in_class,
            None
        );
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join(".editorconfig"), "[*]\nij_python_blank_lines_around_method=unset\nij_formatter_enabled=invalid\nij_python_blank_lines_around_top_level_classes_functions=999999\n").unwrap();
        let nested = resolve(&sub.join("a.py"));
        assert_eq!(nested.ide.blank_lines_in_class, Some(2));
        assert_eq!(nested.ide.blank_lines_top_level, None);
        assert_eq!(nested.ide.formatter_enabled, None);
    }
}

#[cfg(test)]
mod visual_guides_tests {
    use super::*;
    #[test]
    fn guides_match_upstream_empty_invalid_and_bounded_list_semantics() {
        assert_eq!(
            parse_visual_guides("79, 80,80,0,bad,-1,+120"),
            Some(vec![0, 79, 80, 120])
        );
        assert_eq!(parse_visual_guides(""), Some(vec![]));
        assert_eq!(parse_visual_guides("bad,-1,2147483648"), Some(vec![]));
        assert_eq!(parse_visual_guides("UnSeT"), None);
        assert_eq!(parse_visual_guides(&vec!["1"; 129].join(",")), None);
    }
    #[test]
    fn guides_nested_clear_unset_and_unknown_retention() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(".editorconfig"),
            "root=true\n[*]\nij_visual_guides=79,120\nij_unrecognized=keep\n",
        )
        .unwrap();
        let dir = root.path().join("sub");
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("a.py");
        assert_eq!(resolve(&file).ide.visual_guides, Some(vec![79, 120]));
        std::fs::write(dir.join(".editorconfig"), "[*]\nij_visual_guides=\n").unwrap();
        let empty = resolve(&file);
        assert_eq!(empty.ide.visual_guides, Some(vec![]));
        assert!(!empty.unsupported_ide.contains_key("ij_visual_guides"));
        assert_eq!(empty.unsupported_ide["ij_unrecognized"], "keep");
        std::fs::write(dir.join(".editorconfig"), "[*]\nij_visual_guides=unset\n").unwrap();
        assert_eq!(resolve(&file).ide.visual_guides, None);
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    #[test]
    fn continuation_properties_are_typed_python_only_and_unset() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".editorconfig"), "root=true\n[*]\nij_continuation_indent_size=6\nij_python_use_continuation_indent_for_arguments=TRUE\nij_python_use_continuation_indent_for_parameters=false\nij_python_use_continuation_indent_for_collection_and_comprehensions=true\n[*.pyi]\nij_continuation_indent_size=unset\n").unwrap();
        let py = resolve(&root.path().join("a.py"));
        assert_eq!(py.ide.continuation_indent_size, Some(6));
        assert_eq!(py.ide.continuation_parameters, Some(false));
        assert_eq!(py.ide.continuation_arguments, Some(true));
        assert_eq!(py.ide.continuation_collections, Some(true));
        assert_eq!(
            resolve(&root.path().join("a.pyi"))
                .ide
                .continuation_indent_size,
            None
        );
        let txt = resolve(&root.path().join("a.txt"));
        assert_eq!(txt.ide.continuation_indent_size, None);
        assert_eq!(txt.ide.continuation_parameters, None);
    }
}

#[cfg(test)]
mod smart_tabs_tests {
    use super::*;
    #[test]
    fn smart_tabs_bool_false_and_unset_are_distinct_from_absent() {
        let root = tempfile::tempdir().unwrap();
        for (value, expected) in [
            ("TRUE", Some(true)),
            ("false", Some(false)),
            ("unset", None),
            ("invalid", None),
        ] {
            std::fs::write(
                root.path().join(".editorconfig"),
                format!("root=true\n[*]\nij_smart_tabs={value}\n"),
            )
            .unwrap();
            let ec = resolve(&root.path().join("a.py"));
            assert_eq!(ec.ide.smart_tabs, expected);
            assert!(!ec.unsupported_ide.contains_key("ij_smart_tabs"));
        }
    }
}

#[cfg(test)]
mod formatter_control_tags_tests {
    use super::*;
    #[test]
    fn tags_are_case_preserved_bounded_unset_and_bool_enabled() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".editorconfig"),"root=true\n[*]\nij_formatter_tags_enabled=TRUE\nij_formatter_off_tag=KEEP π\nij_formatter_on_tag=RESUME π\n[*.pyi]\nij_formatter_off_tag=unset\n").unwrap();
        let ec = resolve(&root.path().join("a.py"));
        assert_eq!(ec.ide.formatter_tags_enabled, Some(true));
        assert_eq!(ec.ide.formatter_off_tag.as_deref(), Some("KEEP π"));
        assert_eq!(ec.ide.formatter_on_tag.as_deref(), Some("RESUME π"));
        assert_eq!(
            resolve(&root.path().join("a.pyi")).ide.formatter_off_tag,
            None
        );
        std::fs::write(
            root.path().join(".editorconfig"),
            format!("root=true\n[*]\nij_formatter_off_tag={}\n", "x".repeat(257)),
        )
        .unwrap();
        assert_eq!(
            resolve(&root.path().join("a.py")).ide.formatter_off_tag,
            None
        );
    }
}
