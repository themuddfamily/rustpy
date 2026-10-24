//! Help › Diagnostic Tools › Debug Log Settings: extra per-module log
//! categories (`module`, `#module:trace` or `RUST_LOG`-style `module=level`) applied without a restart.
//!
//! The categories are saved in [`crate::diagnostics::Settings`]; the installed
//! logger consults [`enabled`] beside its `RUST_LOG` filter, so a record passes
//! when either one admits it (categories only add detail, `RUST_LOG` stays in force).
use log::{Level, LevelFilter};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::RwLock;

/// One category: a module path prefix and the most detailed level it logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directive {
    pub category: String,
    pub level: LevelFilter,
}

/// At most this many categories are kept; each lookup scans them all.
pub const MAX_DIRECTIVES: usize = 256;

/// Parses the dialog text: one category per line (or comma separated), written
/// `rustpy_index` (debug), `rustpy_debug:trace`, `#rustpy_debug:trace` or
/// `rustpy_debug=trace`. Crate names may use dashes (`rustpy-index`). Errors
/// name the first bad line.
pub fn parse(text: &str) -> Result<Vec<Directive>, String> {
    let mut out: Vec<Directive> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        for entry in line.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let body = entry.strip_prefix('#').unwrap_or(entry).trim();
            // `a::b:trace` - the level follows the last single colon.
            let (category, level) = match body.rsplit_once('=') {
                Some((category, level)) => (category, Some(level)),
                None => match body.rfind(':') {
                    Some(at) if !body[..at].ends_with(':') && !body[at + 1..].starts_with(':') => {
                        (&body[..at], Some(&body[at + 1..]))
                    }
                    _ => (body, None),
                },
            };
            let category = category.trim().replace('-', "_");
            if category.is_empty()
                || !category.split("::").all(|part| {
                    !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
            {
                return Err(format!(
                    "Line {}: '{entry}' is not a module path",
                    number + 1
                ));
            }
            let level = match level.map(|level| level.trim().to_ascii_lowercase()) {
                None => LevelFilter::Debug,
                Some(level) => match level.as_str() {
                    "" | "debug" => LevelFilter::Debug,
                    "trace" | "all" => LevelFilter::Trace,
                    "info" => LevelFilter::Info,
                    "warn" | "warning" => LevelFilter::Warn,
                    "error" => LevelFilter::Error,
                    "off" => LevelFilter::Off,
                    _ => {
                        return Err(format!(
                            "Line {}: unknown level '{level}' (use trace, debug, info, warn, error or off)",
                            number + 1
                        ))
                    }
                },
            };
            // A repeated category keeps its last level, like RUST_LOG.
            out.retain(|directive| directive.category != category);
            if out.len() >= MAX_DIRECTIVES {
                return Err(format!("Use at most {MAX_DIRECTIVES} categories"));
            }
            out.push(Directive { category, level });
        }
    }
    Ok(out)
}

/// The level of the most specific category covering `target` (`a::b` covers
/// `a::b` and `a::b::c`, not `a::bc`).
pub fn level_for(directives: &[Directive], target: &str) -> Option<LevelFilter> {
    directives
        .iter()
        .filter(|directive| {
            target
                .strip_prefix(directive.category.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
        })
        .max_by_key(|directive| directive.category.len())
        .map(|directive| directive.level)
}

static DIRECTIVES: RwLock<Vec<Directive>> = RwLock::new(Vec::new());
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The installed logger's own maximum level; `usize::MAX` until one is installed.
static BASE: AtomicUsize = AtomicUsize::new(usize::MAX);

fn filter_from(index: usize) -> LevelFilter {
    LevelFilter::iter().nth(index).unwrap_or(LevelFilter::Trace)
}

/// Called by the logger once installed with its `RUST_LOG`/default maximum.
pub fn set_base_level(level: LevelFilter) {
    BASE.store(level as usize, Ordering::Relaxed);
    update_max_level();
}

fn update_max_level() {
    let base = BASE.load(Ordering::Relaxed);
    if base == usize::MAX {
        return;
    }
    let extra = DIRECTIVES
        .read()
        .map(|directives| {
            directives
                .iter()
                .map(|d| d.level)
                .max()
                .unwrap_or(LevelFilter::Off)
        })
        .unwrap_or(LevelFilter::Off);
    log::set_max_level(filter_from(base).max(extra));
}

/// Parses and installs `text` for the running process; returns the number of
/// categories. On an error the previous categories stay in force.
pub fn apply(text: &str) -> Result<usize, String> {
    let directives = parse(text)?;
    let count = directives.len();
    if let Ok(mut current) = DIRECTIVES.write() {
        ACTIVE.store(!directives.is_empty(), Ordering::Relaxed);
        *current = directives;
    }
    update_max_level();
    Ok(count)
}

/// Whether the configured categories admit a `level` record from `target`.
pub fn enabled(target: &str, level: Level) -> bool {
    if !ACTIVE.load(Ordering::Relaxed) {
        return false;
    }
    DIRECTIVES
        .read()
        .ok()
        .and_then(|directives| level_for(&directives, target))
        .is_some_and(|max| level <= max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directive(category: &str, level: LevelFilter) -> Directive {
        Directive {
            category: category.into(),
            level,
        }
    }

    #[test]
    fn categories_accept_the_hash_prefixed_and_rust_log_spellings() {
        let parsed = parse(
            "rustpy_index\n#rustpy_debug:trace\nrustpy-lsp::server=info, rustpy_vcs:WARNING\n\n  ",
        )
        .unwrap();
        assert_eq!(
            parsed,
            [
                directive("rustpy_index", LevelFilter::Debug),
                directive("rustpy_debug", LevelFilter::Trace),
                directive("rustpy_lsp::server", LevelFilter::Info),
                directive("rustpy_vcs", LevelFilter::Warn),
            ]
        );
        // A module path's `::` is not mistaken for the level separator.
        assert_eq!(
            parse("a::b::c").unwrap(),
            [directive("a::b::c", LevelFilter::Debug)]
        );
        assert_eq!(
            parse("a::b:off").unwrap(),
            [directive("a::b", LevelFilter::Off)]
        );
        // The last spelling of a category wins.
        assert_eq!(
            parse("x:trace\nx:error").unwrap(),
            [directive("x", LevelFilter::Error)]
        );
    }

    #[test]
    fn bad_categories_name_their_line() {
        assert!(parse("ok\nrustpy_index:loud")
            .unwrap_err()
            .starts_with("Line 2"));
        assert!(parse("has space:debug").unwrap_err().starts_with("Line 1"));
        assert!(parse("a:::b").is_err());
        assert!(parse(":trace").is_err());
        let many = (0..=MAX_DIRECTIVES)
            .map(|i| format!("m{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(parse(&many).is_err());
    }

    #[test]
    fn the_most_specific_category_decides() {
        let directives = [
            directive("rustpy_index", LevelFilter::Debug),
            directive("rustpy_index::parser", LevelFilter::Off),
        ];
        assert_eq!(
            level_for(&directives, "rustpy_index"),
            Some(LevelFilter::Debug)
        );
        assert_eq!(
            level_for(&directives, "rustpy_index::store"),
            Some(LevelFilter::Debug)
        );
        assert_eq!(
            level_for(&directives, "rustpy_index::parser::tokens"),
            Some(LevelFilter::Off)
        );
        // A crate whose name merely starts with a category is not covered.
        assert_eq!(level_for(&directives, "rustpy_indexer"), None);
        assert_eq!(level_for(&directives, "rustpy_debug"), None);
    }

    #[test]
    fn applied_categories_filter_records_until_replaced() {
        assert_eq!(apply("rustpy_test_category_a:trace").unwrap(), 1);
        assert!(enabled("rustpy_test_category_a::x", Level::Trace));
        assert!(!enabled("rustpy_test_category_b", Level::Error));
        // A bad text keeps the previous categories.
        assert!(apply("rustpy_test_category_b:loud").is_err());
        assert!(enabled("rustpy_test_category_a", Level::Debug));
        apply("rustpy_test_category_a:info").unwrap();
        assert!(!enabled("rustpy_test_category_a", Level::Debug));
        assert!(enabled("rustpy_test_category_a", Level::Warn));
        apply("").unwrap();
        assert!(!enabled("rustpy_test_category_a", Level::Error));
    }
}
