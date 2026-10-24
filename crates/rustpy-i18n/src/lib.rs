//! UI localization for the RustPy IDE (roadmap "Internationalization"):
//! translated menus, actions, tool windows and settings pages in the
//! the major languages of software development.
//!
//! # Catalogs
//!
//! One JSON file per locale under `locales/`, embedded with
//! `include_str!` and parsed on first use, maps English source strings to
//! translations:
//!
//! ```json
//! { "File": "文件", "Find Action…": "查找操作…",
//!   "{n} file": { "one": "{n} Datei", "other": "{n} Dateien" } }
//! ```
//!
//! English is the identity: anything a catalog lacks (or translates with an
//! empty string) falls back to the English source. Plural entries hold one
//! form per CLDR category the locale uses ([`plural_categories`]).
//! `locales/en.json` is the source catalog listing every translatable
//! string. `cargo run -p rustpy-i18n --example i18n_extract` extracts the UI
//! vocabulary from the `rustpy-ui` sources (action texts, menus, tool window
//! titles, settings pages and every literal passed to `tr`, `tr_static`,
//! `tr_fmt` or `tr_n`), reports what `en.json` or a locale lacks and prints
//! JSON stubs of it. A gap is not an error: the string shows in English
//! until it is translated (`-- --check` and
//! `cargo test -p rustpy-i18n -- --ignored` fail on gaps).
//!
//! Complete locales: `zh-CN`, `ja`, `ko`, `de`, `fr`, `es`. `pt-BR` and `ru`
//! may lag behind (new strings show in English).
//!
//! ```
//! use rustpy_i18n::{set_locale, tr, tr_n, tr_static};
//!
//! assert!(set_locale("zh-CN"));
//! assert_eq!(tr("File"), "文件");
//! assert_eq!(tr_static("Find Action…"), "查找操作…");
//! assert_eq!(tr("Not in any catalog"), "Not in any catalog");
//! assert_eq!(tr_n("{n} file", "{n} files", 3), "3 个文件");
//!
//! assert!(set_locale("de_DE.UTF-8"));
//! assert_eq!(tr_n("{n} file", "{n} files", 1), "1 Datei");
//! assert!(!set_locale("tlh"));
//! assert_eq!(rustpy_i18n::locale(), "de");
//! ```
//!
//! # Locale detection
//!
//! [`system_locale`] maps `LC_ALL` / `LC_MESSAGES` / `LANG` (and GNU
//! `LANGUAGE`) to a supported locale: `zh_CN.UTF-8` → `zh-CN`.
//!
//! # Fonts
//!
//! [`find_fallback_font`] finds a system CJK font (Noto Sans CJK, Source Han
//! Sans, PingFang, Hiragino, Microsoft YaHei, Malgun Gothic, WenQuanYi, …)
//! for the locale's glyph variant, with the face index inside `.ttc`
//! collections; nothing is bundled.

mod catalog;
mod fonts;
mod locale;
mod plural;
pub mod regional;
mod sfnt;

pub use catalog::{
    fill, locale, plural_categories, plural_category, set_locale, tr, tr_fmt, tr_in, tr_n, tr_n_in,
    tr_static, translation_coverage,
};
pub use fonts::{
    cjk_font_candidates, cjk_font_candidates_in, find_fallback_font, find_fallback_font_in,
    system_font_dirs,
};
pub use locale::{
    available_locales, match_locale, parse_posix_locale, system_locale, system_locale_from,
};
pub use plural::{PluralCategory, PluralCount};
