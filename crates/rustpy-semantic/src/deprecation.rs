//! Semantic queries for PEP 702 deprecation metadata.

use crate::db::FileInfo;
use crate::typeshed::PyVersion;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};
use smol_str::SmolStr;

/// Names of the `@deprecated` definitions of `f` (a deprecated
/// `__init__`/`__new__` also names its class), cached with the file version.
pub fn deprecated_names(f: &FileInfo) -> &[SmolStr] {
    f.deprecated_names.get_or_init(|| {
        let mut out: Vec<SmolStr> = Vec::new();
        if !f.text.contains("deprecated") {
            return out;
        }
        let name = |n: &SyntaxNode| {
            n.children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == SyntaxKind::IDENT)
                .map(|t| SmolStr::new(t.text()))
        };
        for def in f
            .parse
            .syntax_node()
            .descendants()
            .filter(|n| matches!(n.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF))
        {
            if deprecation_message(&def).is_none() {
                continue;
            }
            let Some(def_name) = name(&def) else { continue };
            if matches!(def_name.as_str(), "__init__" | "__new__") {
                if let Some(class) = def
                    .ancestors()
                    .skip(1)
                    .find(|n| n.kind() == SyntaxKind::CLASS_DEF)
                    .and_then(|c| name(&c))
                {
                    out.push(class);
                }
            }
            out.push(def_name);
        }
        out.sort();
        out.dedup();
        out
    })
}

/// `Some(message)` when a `def`/`class` is decorated with `@deprecated`
/// (any `deprecated` / `x.deprecated` decorator, called or bare).
pub fn deprecation_message(def: &SyntaxNode) -> Option<String> {
    let decorators: Vec<ast::Decorator> = match def.kind() {
        SyntaxKind::FUNC_DEF => ast::FuncDef::cast(def.clone())?.decorators().collect(),
        SyntaxKind::CLASS_DEF => ast::ClassDef::cast(def.clone())?.decorators().collect(),
        _ => return None,
    };
    for d in decorators {
        let Some(name) = d.name_text() else { continue };
        if name != "deprecated" && !name.ends_with(".deprecated") {
            continue;
        }
        // The message: the first argument, implicit concatenation included.
        let message = match d.expr() {
            Some(ast::Expr::Call(c)) => c
                .arg_list()
                .and_then(|l| l.args().next())
                .and_then(|a| a.value())
                .map(|v| {
                    v.syntax()
                        .descendants_with_tokens()
                        .filter_map(|e| e.into_node())
                        .filter_map(ast::StringExpr::cast)
                        .map(|s| s.value())
                        .collect::<String>()
                })
                .unwrap_or_default(),
            _ => String::new(),
        };
        return Some(message);
    }
    None
}

/// A stdlib module deprecated as a whole (no PEP 702 marker can say so).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeprecatedModule {
    pub name: &'static str,
    /// First version that deprecates it.
    pub since: PyVersion,
    /// First version without it.
    pub removed: Option<PyVersion>,
    /// What to use instead.
    pub advice: &'static str,
}

const fn module(
    name: &'static str,
    since: PyVersion,
    removed: PyVersion,
    advice: &'static str,
) -> DeprecatedModule {
    DeprecatedModule {
        name,
        since,
        removed: Some(removed),
        advice,
    }
}

const PEP_594: &str = "see PEP 594 for replacements";

/// Deprecated top-level stdlib modules (their submodules included).
pub static DEPRECATED_MODULES: &[DeprecatedModule] = &[
    module("imp", (3, 4), (3, 12), "use importlib"),
    module("asynchat", (3, 6), (3, 12), "use asyncio"),
    module("asyncore", (3, 6), (3, 12), "use asyncio"),
    module("smtpd", (3, 6), (3, 12), "use aiosmtpd"),
    module("distutils", (3, 10), (3, 12), "use setuptools or packaging"),
    module(
        "lib2to3",
        (3, 11),
        (3, 13),
        "use a maintained parser such as LibCST or parso",
    ),
    module("aifc", (3, 11), (3, 13), PEP_594),
    module("audioop", (3, 11), (3, 13), PEP_594),
    module("cgi", (3, 11), (3, 13), PEP_594),
    module("cgitb", (3, 11), (3, 13), PEP_594),
    module("chunk", (3, 11), (3, 13), PEP_594),
    module("crypt", (3, 11), (3, 13), PEP_594),
    module("imghdr", (3, 11), (3, 13), PEP_594),
    module("mailcap", (3, 11), (3, 13), PEP_594),
    module("msilib", (3, 11), (3, 13), PEP_594),
    module("nis", (3, 11), (3, 13), PEP_594),
    module("nntplib", (3, 11), (3, 13), PEP_594),
    module("ossaudiodev", (3, 11), (3, 13), PEP_594),
    module("pipes", (3, 11), (3, 13), "use subprocess and shlex.quote"),
    module("sndhdr", (3, 11), (3, 13), PEP_594),
    module("spwd", (3, 11), (3, 13), PEP_594),
    module("sunau", (3, 11), (3, 13), PEP_594),
    module("telnetlib", (3, 11), (3, 13), PEP_594),
    module("uu", (3, 11), (3, 13), "use base64"),
    module("xdrlib", (3, 11), (3, 13), PEP_594),
];

/// The deprecation of the stdlib module `name` (or its top-level package)
/// when it applies to `version`.
pub fn deprecated_module(name: &str, version: PyVersion) -> Option<&'static DeprecatedModule> {
    let top = name.split('.').next().unwrap_or(name);
    DEPRECATED_MODULES
        .iter()
        .find(|m| m.name == top && version >= m.since)
}

/// The `collections.abc` classes `collections` itself re-exported until 3.10
/// (deprecated there since 3.3).
pub static COLLECTIONS_ABC_ALIASES: &[&str] = &[
    "AsyncGenerator",
    "AsyncIterable",
    "AsyncIterator",
    "Awaitable",
    "ByteString",
    "Callable",
    "Collection",
    "Container",
    "Coroutine",
    "Generator",
    "Hashable",
    "ItemsView",
    "Iterable",
    "Iterator",
    "KeysView",
    "Mapping",
    "MappingView",
    "MutableMapping",
    "MutableSequence",
    "MutableSet",
    "Reversible",
    "Sequence",
    "Set",
    "Sized",
    "ValuesView",
];

/// First version whose `collections` lacks the ABC aliases.
pub const COLLECTIONS_ABC_REMOVED: PyVersion = (3, 10);

pub fn is_collections_abc_alias(name: &str) -> bool {
    COLLECTIONS_ABC_ALIASES.binary_search(&name).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Analysis;
    use std::path::Path;

    #[test]
    fn deprecated_names_cache_belongs_to_each_file_version() {
        let db = Analysis::for_project(Path::new("/proj"));
        let text = "from typing_extensions import deprecated\nclass Api:\n    @deprecated('use run()')\n    def __init__(self): pass\n    @deprecated('x')\n    def start(self): pass\n";
        let id = db.set_file_text(Path::new("/proj/lib.py"), text);
        let file = db.file(id).unwrap();
        assert!(file.deprecated_names.get().is_none());
        assert_eq!(
            deprecated_names(&file)
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>(),
            vec!["Api", "__init__", "start"]
        );
        let cached = file.deprecated_names.get().unwrap();
        assert!(std::ptr::eq(deprecated_names(&file), cached.as_slice()));
        db.set_file_text(Path::new("/proj/lib.py"), &format!("{text}\n"));
        let next = db.file(id).unwrap();
        assert!(next.deprecated_names.get().is_none());
        assert_eq!(deprecated_names(&next), deprecated_names(&file));
    }

    #[test]
    fn stdlib_module_and_collections_tables() {
        assert!(COLLECTIONS_ABC_ALIASES.windows(2).all(|w| w[0] < w[1]));
        assert!(is_collections_abc_alias("Mapping"));
        assert!(is_collections_abc_alias("Iterable"));
        assert!(!is_collections_abc_alias("OrderedDict"));
        assert!(!is_collections_abc_alias("Buffer"));
        let imp = deprecated_module("imp", (3, 8)).unwrap();
        assert_eq!((imp.since, imp.removed), ((3, 4), Some((3, 12))));
        assert!(deprecated_module("distutils.core", (3, 10)).is_some());
        // Not yet deprecated for older targets; unrelated modules never.
        assert!(deprecated_module("distutils", (3, 9)).is_none());
        assert!(deprecated_module("cgi", (3, 10)).is_none());
        assert!(deprecated_module("importlib", (3, 12)).is_none());
        assert!(deprecated_module("impish", (3, 12)).is_none());
        // Each table entry is (or was) a typeshed module and is gone at its removal.
        for m in DEPRECATED_MODULES {
            assert!(crate::typeshed::is_stdlib(m.name), "{}", m.name);
            let removed = m.removed.unwrap();
            assert!(
                !crate::typeshed::module_available(m.name, removed),
                "{}",
                m.name
            );
            assert!(
                crate::typeshed::module_available(m.name, m.since),
                "{}",
                m.name
            );
        }
    }
}
