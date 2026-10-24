//! RustPy IDE — declarative plugins (roadmap §5 "Plugins").
//!
//! A plugin is a directory with a [`plugin.json`](PluginManifest) manifest and
//! data files. Declarative plugins contribute live templates, color schemes,
//! keymaps, TextMate grammars, external tools and file templates. Optional WASM
//! modules run only in a trusted project after explicit capability consent,
//! through the import-free, fuel-metered [`runtime`] JSON ABI. Everything is
//! validated — paths must stay inside the plugin directory, installed files
//! get plain `0644`/`0755` permissions — so installing a plugin cannot write
//! outside its directory or run anything. An external tool, once the user
//! runs it, can do whatever its program does, like a tool added by hand.
//!
//! Layout: `<config_dir>/plugins/<id>/plugin.json`, enabled state in
//! `<config_dir>/plugins/state.json`.
//!
//! - [`PluginManifest`] / [`validate_plugin_dir`]: manifest parsing and validation.
//! - [`PluginRegistry`]: installed plugins — load, enable/disable,
//!   install from a directory or a bundle, uninstall, and the aggregated
//!   [`Contributions`] of the enabled ones.
//! - [`Catalog`]: the Marketplace — parse/fetch a catalog, detect updates,
//!   [`download_and_install`] with SHA-256 verification.
//! - [`pack_bundle`]: single-file plugin archives (`.rpplugin`, JSON — no zip
//!   crate is available, and the settings export uses the same approach).
//!
//! # Plugin format
//!
//! `plugin.json` (unknown fields are ignored with a warning; every path is
//! relative to the plugin directory, `/`-separated, without `..`):
//!
//! ```jsonc
//! {
//!   "id": "com.example.my-plugin",           // lowercase reverse-DNS, also the directory name
//!   "name": "My Plugin",
//!   "version": "1.2.0",                      // 1–4 numbers, optional -pre.release / +build
//!   "description": "…", "vendor": "…",
//!   "homepage": "https://example.com",       // optional
//!   "min_ide_version": "0.1.0",              // optional
//!   "contributes": {
//!     "live_templates": ["templates/python.json"],
//!     "color_schemes": ["colors/dark.json"],
//!     "keymaps": ["keymaps/mine.json"],
//!     "textmate_grammars": [{"language": "Nim", "extensions": ["nim"], "path": "syntaxes/nim.tmLanguage.json"}],
//!     "external_tools": [{"name": "Lint", "program": "ruff", "arguments": "check $FilePath$",
//!                         "working_dir": "$ProjectFileDir$", "group": "Linters", "show_console": true}],
//!     "file_templates": [{"name": "Pytest module", "extension": "py", "path": "file-templates/test.py"}]
//!   }
//! }
//! ```
//!
//! - **Live templates**: a JSON array in the IDE's `live_templates.json`
//!   format — `{"abbreviation", "description", "template", "contexts"
//!   (Any/Statement/Expression/ClassBody/Module/Comment), "variables", "group",
//!   "reformat"}`; `text` and `context` (a name or a list) are accepted as
//!   aliases. `group` defaults to the plugin name.
//! - **Color schemes**: `{"name", "base": "Dark" | "Light",
//!   "dark"?, "colors": {tag: "#rrggbb" | {"fg", "bg", "bold", "italic",
//!   "underline"}}, "editor": {"background": "#rrggbb", …}}` — tags are
//!   highlight tag names (`Keyword`, `function_definition`, …), editor keys are
//!   the editor color names (`caret_row`, `selection`, …); anything unspecified
//!   comes from the base scheme.
//! - **Keymaps**: `{"name", "bindings": {"ActionId": ["Ctrl+Shift+P", …]}}`, the
//!   format of the IDE's bundled keymap presets, layered over the Default keymap.
//! - **TextMate grammars**: `.tmLanguage.json` (or `.tmLanguage` property
//!   lists) with a `scopeName`.
//! - **External tools**: one command plus arguments with the IDE's macros
//!   ([`TOOL_MACROS`]) and `$PluginDir$`, written the way the IDE reads its
//!   External Tools: whitespace separates arguments, `"…"` and `'…'` group
//!   them, and there are no escapes (a backslash is just a character).
//!   `$PluginDir$` may be quoted (`"$PluginDir$/conf.toml"`); its paths, the
//!   program's too, are checked after quote removal, as the IDE reads them,
//!   and must stay inside the plugin. Other `$` uses and shell operators are
//!   rejected. The IDE runs a tool's program directly, without a shell, and
//!   fills in the macros inside each argument, so it passes a file name on
//!   as text and never reads it as a command (a program that runs its
//!   arguments as code, such as `sh -c`, still can). Tools run only when the
//!   user invokes them; installing makes `$PluginDir$/…` programs executable.
//! - **File templates**: a text file; `${NAME}`-style placeholders can be filled
//!   with [`FileTemplateContribution::render`].
//!
//! ```no_run
//! use rustpy_plugins::PluginRegistry;
//! # let config_dir = std::path::Path::new("/tmp/rustpy");
//! let mut registry = PluginRegistry::load(config_dir);
//! registry.install_from_dir(std::path::Path::new("my-plugin"))?;
//! let contributed = registry.contributions();
//! for scheme in &contributed.color_schemes {
//!     println!("{} from {}", scheme.scheme.name, scheme.plugin_id);
//! }
//! # Ok::<(), rustpy_plugins::PluginError>(())
//! ```

mod bundle;
mod catalog;
mod command;
mod contrib;
mod error;
mod manifest;
mod paths;
mod registry;
mod sha256;
mod version;

pub use bundle::{pack_bundle, BUNDLE_EXTENSION, BUNDLE_FORMAT};
pub use catalog::{
    download, download_and_install, download_and_install_with, download_with, shipped_bundle,
    Catalog, CatalogEntry, PluginUpdate, HTTP_TIMEOUT,
};
pub use contrib::{
    runtime_styles, validate_plugin_dir, ColorSchemeContribution, Contributions,
    ExternalToolContribution, FileTemplateContribution, GrammarContribution, KeymapContribution,
    LiveTemplateContribution, TableEditorContribution, ValidatedPlugin,
};
pub use error::PluginError;
pub use manifest::{
    validate_plugin_id, Contributes, ExternalToolDecl, FileTemplateDecl, GrammarDecl,
    PluginManifest, TableEditorDecl, TableEditorKind, MANIFEST_FILE, TOOL_MACROS,
};
pub use registry::{InstallOutcome, InstalledPlugin, PluginRegistry, PLUGINS_DIR, STATE_FILE};
pub use sha256::{sha256_hex, to_hex, Sha256};
pub use version::{compare_versions, Version};

/// The IDE version plugins are checked against (`min_ide_version`); all
/// RustPy crates share the workspace version.
pub const IDE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod runtime;
pub mod signatures;

pub mod requirements;
