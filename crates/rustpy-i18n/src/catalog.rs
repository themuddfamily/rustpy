//! Translation catalogs, the current UI locale and lookups.

use std::borrow::Cow;
use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use rustc_hash::FxHashMap;
use serde_json::Value;

use crate::locale::{index_of, ENGLISH, LOCALES, LOCALE_COUNT, SOURCE_CATALOG};
use crate::plural::{PluralCategory, PluralCount, PluralRules};

/// A parsed catalog: translations and plural forms by English source string.
#[derive(Default)]
struct Catalog {
    texts: FxHashMap<String, String>,
    plurals: FxHashMap<String, PluralForms>,
}

/// Plural forms indexed by [`PluralCategory`].
#[derive(Default)]
struct PluralForms([Option<String>; 6]);

impl PluralForms {
    /// The form for `category`, else the `other` form.
    fn get(&self, category: PluralCategory) -> Option<&str> {
        self.0[category as usize]
            .as_deref()
            .or(self.0[PluralCategory::Other as usize].as_deref())
    }
}

impl Catalog {
    /// Parses `{"Source": "Translation", "{n} file": {"one": …, "other": …}}`.
    /// Empty translations count as untranslated; malformed entries are
    /// skipped with a warning (the tests keep the shipped catalogs clean).
    fn parse(code: &str, json: &str) -> Catalog {
        let entries = match serde_json::from_str::<Value>(json) {
            Ok(Value::Object(entries)) => entries,
            Ok(_) => {
                log::warn!("{code} catalog is not a JSON object");
                return Catalog::default();
            }
            Err(e) => {
                log::warn!("{code} catalog is not valid JSON: {e}");
                return Catalog::default();
            }
        };
        let mut catalog = Catalog::default();
        for (source, value) in entries {
            match value {
                Value::String(text) if !text.is_empty() => {
                    catalog.texts.insert(source, text);
                }
                Value::String(_) => {}
                Value::Object(forms) => {
                    let mut plural = PluralForms::default();
                    for (name, form) in forms {
                        match (PluralCategory::from_name(&name), form) {
                            (Some(category), Value::String(text)) if !text.is_empty() => {
                                plural.0[category as usize] = Some(text)
                            }
                            (Some(_), Value::String(_)) => {}
                            _ => log::warn!(
                                "{code} catalog: ignoring plural form {name:?} of {source:?}"
                            ),
                        }
                    }
                    if plural.0.iter().any(Option::is_some) {
                        catalog.plurals.insert(source, plural);
                    }
                }
                _ => {
                    log::warn!("{code} catalog: ignoring the non-string translation of {source:?}")
                }
            }
        }
        catalog
    }

    fn text(&self, source: &str) -> Option<&str> {
        self.texts.get(source).map(String::as_str)
    }

    /// The form of `singular` for `category`; single-form languages may
    /// translate a plural entry with a plain string.
    fn plural(&self, singular: &str, category: PluralCategory) -> Option<&str> {
        match self.plurals.get(singular) {
            Some(forms) => forms.get(category),
            None => self.text(singular),
        }
    }

    fn contains(&self, source: &str) -> bool {
        self.texts.contains_key(source) || self.plurals.contains_key(source)
    }
}

static CATALOGS: [OnceLock<Catalog>; LOCALE_COUNT] = [const { OnceLock::new() }; LOCALE_COUNT];
static SOURCE: OnceLock<Catalog> = OnceLock::new();
static CURRENT: AtomicUsize = AtomicUsize::new(ENGLISH);

/// The catalog of `LOCALES[index]`, parsed on first use.
fn catalog(index: usize) -> &'static Catalog {
    CATALOGS[index].get_or_init(|| {
        let def = &LOCALES[index];
        def.catalog
            .map_or_else(Catalog::default, |json| Catalog::parse(def.code, json))
    })
}

fn source_catalog() -> &'static Catalog {
    SOURCE.get_or_init(|| Catalog::parse("en", SOURCE_CATALOG))
}

fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

/// Switches the UI language. Accepts the codes of [`available_locales`]
/// (`en`, `zh-CN`, `ja`, `ko`, `de`, `fr`, `es`, `pt-BR`, `ru`) and any tag
/// [`match_locale`] maps to one of them (`de_DE.UTF-8`, `pt-PT`, `ZH-cn`).
/// Returns `false`, keeping the current locale, for anything else.
///
/// [`available_locales`]: crate::available_locales
/// [`match_locale`]: crate::match_locale
pub fn set_locale(code: &str) -> bool {
    match index_of(code) {
        Some(index) => {
            catalog(index);
            CURRENT.store(index, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// The canonical code of the current UI locale (`"en"` until [`set_locale`]).
pub fn locale() -> String {
    LOCALES[current()].code.to_owned()
}

fn lookup(index: usize, source: &str) -> Option<&'static str> {
    if index == ENGLISH {
        None
    } else {
        catalog(index).text(source)
    }
}

fn translate(index: usize, source: &str) -> Cow<'static, str> {
    match lookup(index, source) {
        Some(text) => Cow::Borrowed(text),
        None => Cow::Owned(source.to_owned()),
    }
}

/// Translates an English UI string into the current locale, falling back to
/// the English string itself when the locale has no translation (and for
/// English, the identity).
///
/// ```
/// rustpy_i18n::set_locale("ja");
/// assert_eq!(rustpy_i18n::tr("Find Action…"), "アクションの検索…");
/// assert_eq!(rustpy_i18n::tr("no such string"), "no such string");
/// ```
pub fn tr(source: &str) -> Cow<'static, str> {
    translate(current(), source)
}

/// [`tr`] for a `'static` source such as an action's text: no allocation,
/// translated or not.
pub fn tr_static(source: &'static str) -> &'static str {
    lookup(current(), source).unwrap_or(source)
}

/// [`tr`] in the given locale instead of the current one (unsupported
/// locales give the English source).
pub fn tr_in(locale: &str, source: &str) -> Cow<'static, str> {
    translate(index_of(locale).unwrap_or(ENGLISH), source)
}

/// A count with its noun in the current locale: picks the plural form for
/// `n` with the locale's CLDR rules and substitutes `{n}`.
///
/// The catalog entry is keyed by the English `singular`. When the locale
/// lacks it, the English `singular`/`plural` pair is used with the English
/// rule (`1 file`, `0 files`).
///
/// ```
/// use rustpy_i18n::{set_locale, tr_n};
/// set_locale("ru");
/// assert_eq!(tr_n("{n} file", "{n} files", 1), "1 файл");
/// assert_eq!(tr_n("{n} file", "{n} files", 3), "3 файла");
/// assert_eq!(tr_n("{n} file", "{n} files", 5), "5 файлов");
/// set_locale("en");
/// assert_eq!(tr_n("{n} file", "{n} files", 1), "1 file");
/// ```
pub fn tr_n(singular: &str, plural: &str, n: impl PluralCount) -> String {
    plural_in(current(), singular, plural, n)
}

/// [`tr_n`] in the given locale instead of the current one.
pub fn tr_n_in(locale: &str, singular: &str, plural: &str, n: impl PluralCount) -> String {
    plural_in(index_of(locale).unwrap_or(ENGLISH), singular, plural, n)
}

fn plural_in(index: usize, singular: &str, plural: &str, n: impl PluralCount) -> String {
    let magnitude = n.magnitude();
    let translated = if index == ENGLISH {
        None
    } else {
        catalog(index).plural(singular, LOCALES[index].rules.category(magnitude))
    };
    let template = translated.unwrap_or(match PluralRules::OneOther.category(magnitude) {
        PluralCategory::One => singular,
        _ => plural,
    });
    fill(template, &[("n", &n)])
}

/// The CLDR plural category `n` selects in `locale` (a code or tag
/// [`set_locale`] accepts; other locales use the English rule).
pub fn plural_category(locale: &str, n: impl PluralCount) -> PluralCategory {
    let rules = index_of(locale).map_or(PluralRules::OneOther, |i| LOCALES[i].rules);
    rules.category(n.magnitude())
}

/// The plural categories integer counts select in `locale`: the forms a
/// catalog entry needs (`["one", "other"]` for German, `["one", "few",
/// "many"]` for Russian, `["other"]` for Japanese).
pub fn plural_categories(locale: &str) -> &'static [PluralCategory] {
    index_of(locale)
        .map_or(PluralRules::OneOther, |i| LOCALES[i].rules)
        .integer_categories()
}

/// Substitutes `{name}` placeholders of `template` with `args`, in one pass
/// (substituted values are not scanned again); unknown placeholders and
/// other braces are kept.
///
/// ```
/// let s = rustpy_i18n::fill("Renamed {old} to {new}", &[("old", &"a"), ("new", &"b")]);
/// assert_eq!(s, "Renamed a to b");
/// ```
pub fn fill(template: &str, args: &[(&str, &dyn fmt::Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 8);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let arg = after.find('}').and_then(|close| {
            args.iter()
                .find(|(name, _)| *name == &after[..close])
                .map(|(_, value)| (close, value))
        });
        match arg {
            Some((close, value)) => {
                // Writing into a String cannot fail.
                let _ = write!(out, "{value}");
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// [`tr`] followed by [`fill`]: `tr_fmt("Rename {name}", &[("name", &file)])`.
pub fn tr_fmt(source: &str, args: &[(&str, &dyn fmt::Display)]) -> String {
    fill(&tr(source), args)
}

/// How much of the source catalog `locale` translates, as
/// `(translated, total)`; `None` for unsupported locales. English is
/// complete by definition. Lets a language picker flag partial locales.
pub fn translation_coverage(locale: &str) -> Option<(usize, usize)> {
    let index = index_of(locale)?;
    let source = source_catalog();
    let total = source.texts.len() + source.plurals.len();
    if index == ENGLISH {
        return Some((total, total));
    }
    let catalog = catalog(index);
    let translated = source
        .texts
        .keys()
        .chain(source.plurals.keys())
        .filter(|key| catalog.contains(key))
        .count();
    Some((translated, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_catalog_parses() {
        assert!(source_catalog().texts.len() > 300);
        for (index, def) in LOCALES.iter().enumerate().skip(1) {
            let catalog = catalog(index);
            assert!(
                catalog.texts.len() > 100,
                "{} has {} translations",
                def.code,
                catalog.texts.len()
            );
            assert!(
                !catalog.plurals.is_empty(),
                "{} has no plural entries",
                def.code
            );
        }
    }

    #[test]
    fn malformed_catalogs_degrade_to_english() {
        assert!(Catalog::parse("xx", "not json").texts.is_empty());
        assert!(Catalog::parse("xx", "[1, 2]").texts.is_empty());
        let c = Catalog::parse(
            "xx",
            r#"{"A": "a", "B": 3, "C": "", "{n} x": {"one": "1 x", "bogus": "?", "other": ""}, "{n} y": {}}"#,
        );
        assert_eq!(c.text("A"), Some("a"));
        assert_eq!(c.text("B"), None);
        assert_eq!(c.text("C"), None);
        assert_eq!(c.plural("{n} x", PluralCategory::One), Some("1 x"));
        assert_eq!(c.plural("{n} x", PluralCategory::Other), None);
        assert!(!c.contains("{n} y"));
    }

    #[test]
    fn plural_forms_fall_back_to_other() {
        let c = Catalog::parse(
            "xx",
            r#"{"{n} file": {"one": "{n} Datei", "other": "{n} Dateien"}, "{n} dir": "{n} 个目录"}"#,
        );
        assert_eq!(
            c.plural("{n} file", PluralCategory::Many),
            Some("{n} Dateien")
        );
        assert_eq!(c.plural("{n} dir", PluralCategory::One), Some("{n} 个目录"));
        assert_eq!(c.plural("{n} other", PluralCategory::One), None);
    }

    #[test]
    fn fill_substitutes_known_placeholders_once() {
        assert_eq!(fill("{a}{b}{a}", &[("a", &1), ("b", &"{a}")]), "1{a}1");
        assert_eq!(fill("{unknown} {", &[("a", &1)]), "{unknown} {");
        assert_eq!(fill("{}{{a}}", &[("a", &2)]), "{}{2}");
        assert_eq!(fill("", &[]), "");
        assert_eq!(fill("ß {n} ü", &[("n", &-3)]), "ß -3 ü");
    }
}
