//! The public API as the IDE uses it: switching the UI language, looking up
//! translations with English fallback, plurals and locale detection.

use std::sync::{Mutex, MutexGuard};

use rustpy_i18n::{
    available_locales, fill, locale, match_locale, plural_categories, plural_category, set_locale,
    system_locale, system_locale_from, tr, tr_fmt, tr_in, tr_n, tr_n_in, tr_static,
    translation_coverage, PluralCategory,
};

/// Tests that change the global locale run one at a time.
fn global_locale() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_locale("en");
    guard
}

#[test]
fn set_locale_accepts_supported_codes_and_rejects_others() {
    let _g = global_locale();
    assert_eq!(locale(), "en");
    for (code, _) in available_locales() {
        assert!(set_locale(code), "{code}");
        assert_eq!(locale(), code);
    }
    // Spelling variants select the canonical locale.
    for (tag, canonical) in [
        ("zh_CN.UTF-8", "zh-CN"),
        ("ZH-cn", "zh-CN"),
        ("de-AT", "de"),
        ("pt_PT", "pt-BR"),
        ("EN_us", "en"),
    ] {
        assert!(set_locale(tag), "{tag}");
        assert_eq!(locale(), canonical, "{tag}");
    }
    // Unsupported or malformed codes are refused and keep the current locale.
    assert!(set_locale("ja"));
    for bad in [
        "",
        "xx",
        "it",
        "zh-TW",
        "zh-Hant",
        "C",
        "POSIX",
        "japanese",
        "j",
        "日本語",
        "ja\u{0}",
    ] {
        assert!(!set_locale(bad), "{bad:?}");
        assert_eq!(locale(), "ja", "{bad:?} must not change the locale");
    }
}

#[test]
fn tr_translates_into_the_current_locale() {
    let _g = global_locale();
    assert_eq!(tr("File"), "File", "English is the identity");
    let expected = [
        ("zh-CN", "文件", "查找操作…"),
        ("ja", "ファイル", "アクションの検索…"),
        ("ko", "파일", "액션 찾기…"),
        ("de", "Datei", "Aktion suchen…"),
        ("fr", "Fichier", "Rechercher une action…"),
        ("es", "Archivo", "Buscar acción…"),
        ("pt-BR", "Arquivo", "Localizar ação…"),
        ("ru", "Файл", "Найти действие…"),
    ];
    for (code, file, find_action) in expected {
        assert!(set_locale(code));
        assert_eq!(tr("File"), file, "{code}");
        assert_eq!(tr("Find Action…"), find_action, "{code}");
        assert_eq!(tr_in(code, "File"), file);
    }
}

#[test]
fn untranslated_strings_fall_back_to_english() {
    let _g = global_locale();
    for (code, _) in available_locales() {
        assert!(set_locale(code));
        assert_eq!(tr("Not a UI string"), "Not a UI string", "{code}");
        assert_eq!(tr(""), "");
        assert_eq!(
            tr_static("Not a UI string either"),
            "Not a UI string either"
        );
        assert_eq!(tr_in(code, "Unknown {x}"), "Unknown {x}");
        // Lookups are exact: no case folding or trimming.
        assert_eq!(tr("file"), "file", "{code}");
        assert_eq!(tr(" File"), " File", "{code}");
    }
    assert_eq!(
        tr_in("xx", "File"),
        "File",
        "unsupported locales give the source"
    );
}

#[test]
fn tr_static_never_allocates() {
    let _g = global_locale();
    let source: &'static str = "Find Action…";
    assert!(
        std::ptr::eq(tr_static(source), source),
        "English returns the source itself"
    );
    assert!(set_locale("ko"));
    let translated = tr_static(source);
    assert_eq!(translated, "액션 찾기…");
    assert!(
        std::ptr::eq(tr_static(source), translated),
        "the catalog's string, not a copy"
    );
    let missing: &'static str = "Not translated";
    assert!(std::ptr::eq(tr_static(missing), missing));
    assert!(matches!(tr(source), std::borrow::Cow::Borrowed(_)));
}

#[test]
fn plurals_follow_each_locales_rules() {
    let cases: &[(&str, &[(u64, &str)])] = &[
        ("en", &[(0, "0 files"), (1, "1 file"), (2, "2 files")]),
        ("de", &[(0, "0 Dateien"), (1, "1 Datei"), (2, "2 Dateien")]),
        (
            "fr",
            &[
                (0, "0 fichier"),
                (1, "1 fichier"),
                (2, "2 fichiers"),
                (1_000_000, "1000000 de fichiers"),
            ],
        ),
        (
            "es",
            &[
                (0, "0 archivos"),
                (1, "1 archivo"),
                (2, "2 archivos"),
                (2_000_000, "2000000 de archivos"),
            ],
        ),
        (
            "pt-BR",
            &[(0, "0 arquivo"), (1, "1 arquivo"), (2, "2 arquivos")],
        ),
        (
            "ru",
            &[
                (1, "1 файл"),
                (2, "2 файла"),
                (5, "5 файлов"),
                (11, "11 файлов"),
                (21, "21 файл"),
                (22, "22 файла"),
                (111, "111 файлов"),
            ],
        ),
        ("zh-CN", &[(1, "1 个文件"), (2, "2 个文件")]),
        ("ja", &[(1, "1 個のファイル"), (7, "7 個のファイル")]),
        ("ko", &[(1, "파일 1개"), (3, "파일 3개")]),
    ];
    for (code, counts) in cases {
        for (n, expected) in *counts {
            assert_eq!(
                tr_n_in(code, "{n} file", "{n} files", *n),
                *expected,
                "{code} {n}"
            );
        }
    }
    // Any integer type; negative counts select by magnitude.
    assert_eq!(tr_n_in("ru", "{n} error", "{n} errors", 3usize), "3 ошибки");
    assert_eq!(tr_n_in("ru", "{n} error", "{n} errors", -1i32), "-1 ошибка");
    assert_eq!(
        tr_n_in("de", "{n} warning", "{n} warnings", 1u8),
        "1 Warnung"
    );
}

#[test]
fn tr_n_uses_the_current_locale() {
    let _g = global_locale();
    assert_eq!(tr_n("{n} match", "{n} matches", 1), "1 match");
    assert_eq!(tr_n("{n} match", "{n} matches", 4), "4 matches");
    assert!(set_locale("ru"));
    assert_eq!(tr_n("{n} match", "{n} matches", 4), "4 совпадения");
    assert!(set_locale("es"));
    assert_eq!(
        tr_n("{n} test failed", "{n} tests failed", 1),
        "1 prueba fallida"
    );
}

#[test]
fn missing_plurals_fall_back_to_the_english_pair_and_rule() {
    for code in ["ru", "pt-BR", "ja", "xx"] {
        assert_eq!(
            tr_n_in(code, "{n} widget", "{n} widgets", 1),
            "1 widget",
            "{code}"
        );
        assert_eq!(
            tr_n_in(code, "{n} widget", "{n} widgets", 2),
            "2 widgets",
            "{code}"
        );
        assert_eq!(
            tr_n_in(code, "{n} widget", "{n} widgets", 0),
            "0 widgets",
            "{code}"
        );
    }
    // Without a {n} placeholder the count is only used to choose the form.
    assert_eq!(tr_n_in("en", "one file", "several files", 1), "one file");
    assert_eq!(
        tr_n_in("en", "one file", "several files", 9),
        "several files"
    );
}

#[test]
fn plural_categories_per_locale() {
    use PluralCategory::{Few, Many, One, Other};
    assert_eq!(plural_category("ru", 3), Few);
    assert_eq!(plural_category("ru_RU.UTF-8", 25), Many);
    assert_eq!(plural_category("fr", 0), One);
    assert_eq!(plural_category("es", 0), Other);
    assert_eq!(plural_category("ja", 1), Other);
    assert_eq!(
        plural_category("xx", 1),
        One,
        "unknown locales use the English rule"
    );
    assert_eq!(plural_categories("ru"), [One, Few, Many]);
    assert_eq!(plural_categories("de"), [One, Other]);
    assert_eq!(plural_categories("fr"), [One, Many, Other]);
    assert_eq!(plural_categories("zh-CN"), [Other]);
}

#[test]
fn placeholders_are_filled_after_translation() {
    let _g = global_locale();
    assert_eq!(
        fill("{a} and {b}", &[("a", &1), ("b", &"two")]),
        "1 and two"
    );
    assert!(set_locale("de"));
    assert_eq!(tr_fmt("Settings", &[("unused", &0)]), "Einstellungen");
    assert_eq!(
        tr_fmt("Open {file}", &[("file", &"main.py")]),
        "Open main.py",
        "untranslated templates are filled too"
    );
}

#[test]
fn available_locales_have_native_names() {
    let locales = available_locales();
    assert_eq!(locales.len(), 9);
    assert_eq!(locales[0], ("en", "English"));
    for expected in [
        ("zh-CN", "简体中文"),
        ("ja", "日本語"),
        ("ko", "한국어"),
        ("de", "Deutsch"),
        ("fr", "Français"),
        ("es", "Español"),
        ("pt-BR", "Português (Brasil)"),
        ("ru", "Русский"),
    ] {
        assert!(locales.contains(&expected), "{expected:?}");
    }
    for (code, _) in &locales {
        assert_eq!(match_locale(code), Some(*code), "codes are canonical");
    }
}

#[test]
fn coverage_flags_partial_locales() {
    let (en, total) = translation_coverage("en").unwrap();
    assert_eq!(en, total);
    assert!(total > 350);
    // Untranslated strings show in English, so coverage may lag behind the
    // source catalog; the complete locales are close to it.
    for code in ["zh-CN", "ja", "ko", "de", "fr", "es"] {
        let (done, all) = translation_coverage(code).unwrap();
        assert_eq!(all, total);
        assert!(done > total / 2 && done <= total, "{code}: {done}/{total}");
    }
    for code in ["pt-BR", "ru"] {
        let (done, all) = translation_coverage(code).unwrap();
        assert_eq!(all, total);
        assert!(done > total / 4 && done <= total, "{code}: {done}/{total}");
    }
    assert_eq!(translation_coverage("it"), None);
}

#[test]
fn system_locale_comes_from_the_environment() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    };
    assert_eq!(
        system_locale_from(env(&[("LANG", "zh_CN.UTF-8")])).as_deref(),
        Some("zh-CN")
    );
    assert_eq!(
        system_locale_from(env(&[("LANG", "ja_JP.UTF-8")])).as_deref(),
        Some("ja")
    );
    assert_eq!(
        system_locale_from(env(&[("LANG", "pt_BR.UTF-8")])).as_deref(),
        Some("pt-BR")
    );
    assert_eq!(
        system_locale_from(env(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "ko_KR.UTF-8")])).as_deref(),
        Some("ko")
    );
    assert_eq!(system_locale_from(env(&[("LANG", "C.UTF-8")])), None);
    // Whatever this machine is set to, the result is usable with set_locale.
    if let Some(code) = system_locale() {
        assert!(
            available_locales().iter().any(|(c, _)| *c == code),
            "{code}"
        );
    }
}
