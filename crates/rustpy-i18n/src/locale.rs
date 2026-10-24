//! The supported UI locales, locale tag parsing and system locale detection.

use crate::plural::PluralRules;

/// A supported UI locale.
pub(crate) struct LocaleDef {
    /// Canonical code: `en`, `zh-CN`, `pt-BR`, ….
    pub code: &'static str,
    /// The language's own name, for language pickers.
    pub native_name: &'static str,
    pub rules: PluralRules,
    /// The embedded catalog; `None` for English, the identity.
    pub catalog: Option<&'static str>,
}

/// Index of English in [`LOCALES`].
pub(crate) const ENGLISH: usize = 0;

pub(crate) const LOCALE_COUNT: usize = 9;

pub(crate) static LOCALES: [LocaleDef; LOCALE_COUNT] = [
    LocaleDef {
        code: "en",
        native_name: "English",
        rules: PluralRules::OneOther,
        catalog: None,
    },
    LocaleDef {
        code: "zh-CN",
        native_name: "简体中文",
        rules: PluralRules::Invariant,
        catalog: Some(include_str!("../locales/zh-CN.json")),
    },
    LocaleDef {
        code: "ja",
        native_name: "日本語",
        rules: PluralRules::Invariant,
        catalog: Some(include_str!("../locales/ja.json")),
    },
    LocaleDef {
        code: "ko",
        native_name: "한국어",
        rules: PluralRules::Invariant,
        catalog: Some(include_str!("../locales/ko.json")),
    },
    LocaleDef {
        code: "de",
        native_name: "Deutsch",
        rules: PluralRules::OneOther,
        catalog: Some(include_str!("../locales/de.json")),
    },
    LocaleDef {
        code: "fr",
        native_name: "Français",
        rules: PluralRules::ZeroOneMillions,
        catalog: Some(include_str!("../locales/fr.json")),
    },
    LocaleDef {
        code: "es",
        native_name: "Español",
        rules: PluralRules::OneMillions,
        catalog: Some(include_str!("../locales/es.json")),
    },
    LocaleDef {
        code: "pt-BR",
        native_name: "Português (Brasil)",
        rules: PluralRules::ZeroOneMillions,
        catalog: Some(include_str!("../locales/pt-BR.json")),
    },
    LocaleDef {
        code: "ru",
        native_name: "Русский",
        rules: PluralRules::EastSlavic,
        catalog: Some(include_str!("../locales/ru.json")),
    },
];

/// The source catalog: every translatable string, mapped to itself.
pub(crate) const SOURCE_CATALOG: &str = include_str!("../locales/en.json");

/// The supported locales as `(code, native name)`, English first:
/// `("en", "English")`, `("zh-CN", "简体中文")`, `("ja", "日本語")`, ….
pub fn available_locales() -> Vec<(&'static str, &'static str)> {
    LOCALES.iter().map(|l| (l.code, l.native_name)).collect()
}

/// Index in [`LOCALES`] of the locale [`match_locale`] picks for `tag`.
pub(crate) fn index_of(tag: &str) -> Option<usize> {
    let code = match_locale(tag)?;
    LOCALES.iter().position(|l| l.code == code)
}

/// Converts a POSIX locale name (`zh_CN.UTF-8`, `de_DE@euro`,
/// `sr_RS@latin`) or a BCP 47 tag in any case (`ZH-hant-tw`) into a
/// normalized BCP 47 tag: `zh-CN`, `de-DE`, `sr-Latn-RS`, `zh-Hant-TW`.
///
/// The codeset is dropped; the `@latin`/`@cyrillic` modifiers become a
/// script. Returns `None` for `C`/`POSIX` (no language preference) and for
/// names whose language is not 2–3 ASCII letters.
pub fn parse_posix_locale(name: &str) -> Option<String> {
    let name = name.trim();
    let (base, modifier) = match name.split_once('@') {
        Some((base, modifier)) => (base, Some(modifier)),
        None => (name, None),
    };
    let base = base.split('.').next().unwrap_or_default();
    if base.is_empty() || base.eq_ignore_ascii_case("C") || base.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let mut parts = base.split(['_', '-']);
    let language = parts.next()?;
    if !(2..=3).contains(&language.len()) || !language.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mut script = modifier.and_then(|m| match m.to_ascii_lowercase().as_str() {
        "latin" => Some("Latn".to_owned()),
        "cyrillic" => Some("Cyrl".to_owned()),
        "devanagari" => Some("Deva".to_owned()),
        _ => None,
    });
    let mut region = None;
    for part in parts {
        if part.len() == 1 {
            break; // an extension (`-u-ca-…`) or private use (`-x-…`) follows
        }
        let alpha = part.bytes().all(|b| b.is_ascii_alphabetic());
        if part.len() == 4 && alpha {
            let mut s = part.to_ascii_lowercase();
            s[..1].make_ascii_uppercase();
            script = Some(s);
        } else if (part.len() == 2 && alpha)
            || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
        {
            region = Some(part.to_ascii_uppercase());
        }
    }
    let mut tag = language.to_ascii_lowercase();
    for subtag in [script, region].into_iter().flatten() {
        tag.push('-');
        tag.push_str(&subtag);
    }
    Some(tag)
}

/// The supported locale for a BCP 47 tag or POSIX locale name, by language:
/// `de-AT` → `de`, `zh_CN.UTF-8` / `zh-SG` / `zh-Hans` → `zh-CN`,
/// `pt-PT` → `pt-BR`, `EN_us` → `en`.
///
/// Traditional Chinese (`zh-TW`, `zh-HK`, `zh-MO`, `zh-Hant`) has no catalog
/// and gives `None`, like every other unsupported language.
pub fn match_locale(tag: &str) -> Option<&'static str> {
    let tag = parse_posix_locale(tag)?;
    let mut subtags = tag.split('-');
    let language = subtags.next()?;
    let rest: Vec<&str> = subtags.collect();
    Some(match language {
        "en" => "en",
        "zh" if rest.contains(&"Hans") => "zh-CN",
        "zh" if rest
            .iter()
            .any(|s| matches!(*s, "Hant" | "TW" | "HK" | "MO")) =>
        {
            return None
        }
        "zh" => "zh-CN",
        "ja" => "ja",
        "ko" => "ko",
        "de" => "de",
        "fr" => "fr",
        "es" => "es",
        "pt" => "pt-BR",
        "ru" => "ru",
        _ => return None,
    })
}

/// The supported locale of the user's environment, or `None` when it is
/// unset, `C`/`POSIX`, or a language without a catalog (the UI then stays in
/// English).
///
/// Follows POSIX and gettext precedence: the first non-empty of `LC_ALL`,
/// `LC_MESSAGES` and `LANG` is the message locale (`zh_CN.UTF-8` →
/// `zh-CN`, `de_AT.UTF-8` → `de`); when it is not `C`, the GNU `LANGUAGE`
/// priority list (`fr:de`) is consulted first. Only the environment is read:
/// the Windows and macOS system APIs are not.
pub fn system_locale() -> Option<String> {
    system_locale_from(|var| std::env::var(var).ok())
}

/// [`system_locale`] with the environment supplied by `var` (for tests and
/// for callers that snapshot the environment).
pub fn system_locale_from(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    let get = |name: &str| var(name).filter(|v| !v.trim().is_empty());
    let messages = get("LC_ALL")
        .or_else(|| get("LC_MESSAGES"))
        .or_else(|| get("LANG"))?;
    let tag = parse_posix_locale(&messages)?;
    if let Some(list) = get("LANGUAGE") {
        if let Some(code) = list.split(':').find_map(match_locale) {
            return Some(code.to_owned());
        }
    }
    match_locale(&tag).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_names_become_bcp47_tags() {
        assert_eq!(parse_posix_locale("zh_CN.UTF-8").as_deref(), Some("zh-CN"));
        assert_eq!(parse_posix_locale("ja_JP.eucJP").as_deref(), Some("ja-JP"));
        assert_eq!(parse_posix_locale("de_DE@euro").as_deref(), Some("de-DE"));
        assert_eq!(
            parse_posix_locale("sr_RS@latin").as_deref(),
            Some("sr-Latn-RS")
        );
        assert_eq!(parse_posix_locale("pt_BR").as_deref(), Some("pt-BR"));
        assert_eq!(parse_posix_locale("ru").as_deref(), Some("ru"));
        assert_eq!(
            parse_posix_locale("es_419.UTF-8").as_deref(),
            Some("es-419")
        );
        assert_eq!(
            parse_posix_locale("ZH-hant-tw").as_deref(),
            Some("zh-Hant-TW")
        );
        assert_eq!(parse_posix_locale(" en_US.utf8 ").as_deref(), Some("en-US"));
        assert_eq!(
            parse_posix_locale("en-US-u-ca-gregory").as_deref(),
            Some("en-US")
        );
    }

    #[test]
    fn c_locale_and_garbage_have_no_language() {
        for name in [
            "", "C", "C.UTF-8", "POSIX", "c.utf8", "english", "1234", "x", "_US",
        ] {
            assert_eq!(parse_posix_locale(name), None, "{name:?}");
        }
    }

    #[test]
    fn tags_match_supported_locales_by_language() {
        let cases = [
            ("en", Some("en")),
            ("en_GB.UTF-8", Some("en")),
            ("zh_CN.UTF-8", Some("zh-CN")),
            ("zh", Some("zh-CN")),
            ("zh-SG", Some("zh-CN")),
            ("zh-Hans-HK", Some("zh-CN")),
            ("zh_TW.UTF-8", None),
            ("zh-HK", None),
            ("zh-Hant", None),
            ("ja_JP.UTF-8", Some("ja")),
            ("ko-KR", Some("ko")),
            ("de_AT", Some("de")),
            ("fr-CA", Some("fr")),
            ("es-MX", Some("es")),
            ("pt_BR", Some("pt-BR")),
            ("pt-PT", Some("pt-BR")),
            ("ru_RU.KOI8-R", Some("ru")),
            ("it_IT.UTF-8", None),
            ("C.UTF-8", None),
            ("", None),
        ];
        for (tag, expected) in cases {
            assert_eq!(match_locale(tag), expected, "{tag:?}");
        }
    }

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn system_locale_follows_posix_precedence() {
        assert_eq!(
            system_locale_from(env(&[("LANG", "zh_CN.UTF-8")])).as_deref(),
            Some("zh-CN")
        );
        assert_eq!(
            system_locale_from(env(&[("LANG", "de_AT.UTF-8")])).as_deref(),
            Some("de")
        );
        assert_eq!(
            system_locale_from(env(&[
                ("LANG", "en_US.UTF-8"),
                ("LC_MESSAGES", "ja_JP.UTF-8")
            ]))
            .as_deref(),
            Some("ja")
        );
        assert_eq!(
            system_locale_from(env(&[
                ("LANG", "en_US.UTF-8"),
                ("LC_MESSAGES", "ja_JP.UTF-8"),
                ("LC_ALL", "ko_KR.UTF-8")
            ]))
            .as_deref(),
            Some("ko")
        );
        // Empty variables are unset.
        assert_eq!(
            system_locale_from(env(&[("LC_ALL", ""), ("LANG", "fr_FR.UTF-8")])).as_deref(),
            Some("fr")
        );
        // LC_ALL=C wins over LANG: no preference.
        assert_eq!(
            system_locale_from(env(&[("LC_ALL", "C"), ("LANG", "fr_FR.UTF-8")])),
            None
        );
        assert_eq!(system_locale_from(env(&[])), None);
        assert_eq!(system_locale_from(env(&[("LANG", "it_IT.UTF-8")])), None);
    }

    #[test]
    fn language_priority_list_comes_first_unless_the_locale_is_c_or_unset() {
        let vars = [("LANG", "en_US.UTF-8"), ("LANGUAGE", "it:pt_BR:de")];
        assert_eq!(system_locale_from(env(&vars)).as_deref(), Some("pt-BR"));
        let unsupported = [("LANG", "ru_RU.UTF-8"), ("LANGUAGE", "it:nl")];
        assert_eq!(system_locale_from(env(&unsupported)).as_deref(), Some("ru"));
        let c = [("LANG", "C.UTF-8"), ("LANGUAGE", "de")];
        assert_eq!(system_locale_from(env(&c)), None);
        // As in gettext; and a GUI app on Windows or macOS usually has no locale variable at
        // all, so the system default is English there.
        assert_eq!(system_locale_from(env(&[("LANGUAGE", "de")])), None);
    }
}
