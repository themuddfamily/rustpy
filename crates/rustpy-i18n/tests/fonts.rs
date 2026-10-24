//! CJK fallback font discovery over fake font directory trees.

use std::fs;
use std::path::{Path, PathBuf};

use rustpy_i18n::{cjk_font_candidates_in, find_fallback_font_in, system_font_dirs};

/// A `name` table with Windows (UTF-16) records for `(name id, text)`.
fn name_table(records: &[(u16, &str)]) -> Vec<u8> {
    let strings: Vec<Vec<u8>> = records
        .iter()
        .map(|(_, s)| s.encode_utf16().flat_map(u16::to_be_bytes).collect())
        .collect();
    let mut out = Vec::new();
    for v in [0u16, records.len() as u16, (6 + 12 * records.len()) as u16] {
        out.extend(v.to_be_bytes());
    }
    let mut offset = 0usize;
    for ((id, _), bytes) in records.iter().zip(&strings) {
        for v in [3u16, 1, 0x0409, *id, bytes.len() as u16, offset as u16] {
            out.extend(v.to_be_bytes());
        }
        offset += bytes.len();
    }
    strings.iter().for_each(|s| out.extend(s));
    out
}

/// A collection (`ttcf`) whose faces have only a `name` table with the given
/// `(family, style)` names, like `NotoSansCJK-Regular.ttc`.
fn collection(faces: &[(&str, &str)]) -> Vec<u8> {
    let tables: Vec<Vec<u8>> = faces
        .iter()
        .map(|(family, style)| name_table(&[(1, family), (2, style)]))
        .collect();
    let header_len = 12 + 4 * faces.len();
    let mut out = Vec::new();
    out.extend(b"ttcf");
    out.extend(0x0001_0000u32.to_be_bytes());
    out.extend((faces.len() as u32).to_be_bytes());
    for i in 0..faces.len() {
        out.extend(((header_len + 28 * i) as u32).to_be_bytes());
    }
    let mut table_offset = header_len + 28 * faces.len();
    for table in &tables {
        out.extend(0x0001_0000u32.to_be_bytes());
        out.extend(1u16.to_be_bytes());
        out.extend([0u8; 6]);
        out.extend(b"name");
        out.extend(0u32.to_be_bytes());
        out.extend((table_offset as u32).to_be_bytes());
        out.extend((table.len() as u32).to_be_bytes());
        table_offset += table.len();
    }
    tables.iter().for_each(|t| out.extend(t));
    out
}

/// The published face order of `NotoSansCJK-Regular.ttc` (2.x).
const NOTO_CJK_FACES: &[(&str, &str)] = &[
    ("Noto Sans CJK JP", "Regular"),
    ("Noto Sans CJK KR", "Regular"),
    ("Noto Sans CJK SC", "Regular"),
    ("Noto Sans CJK TC", "Regular"),
    ("Noto Sans CJK HK", "Regular"),
    ("Noto Sans Mono CJK JP", "Regular"),
    ("Noto Sans Mono CJK KR", "Regular"),
    ("Noto Sans Mono CJK SC", "Regular"),
    ("Noto Sans Mono CJK TC", "Regular"),
    ("Noto Sans Mono CJK HK", "Regular"),
];

fn write(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    path
}

fn rel(root: &Path, found: Option<(PathBuf, u32)>) -> Option<(String, u32)> {
    found.map(|(path, face)| {
        (
            path.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/"),
            face,
        )
    })
}

/// A Debian-like tree: Noto Sans CJK (regular and bold collections),
/// WenQuanYi, Droid Sans Fallback, and fonts that are not CJK.
fn linux_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let noto = collection(NOTO_CJK_FACES);
    let bold: Vec<(&str, &str)> = NOTO_CJK_FACES
        .iter()
        .map(|(family, _)| (*family, "Bold"))
        .collect();
    write(
        root,
        "usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
        &collection(&bold),
    );
    write(
        root,
        "usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        &noto,
    );
    write(
        root,
        "usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        b"not really a font",
    );
    write(
        root,
        "usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        b"",
    );
    write(root, "usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", b"");
    write(root, "usr/share/fonts/README", b"fonts");
    dir
}

#[test]
fn picks_the_locales_face_of_a_pan_cjk_collection() {
    let dir = linux_tree();
    let roots = vec![dir.path().to_path_buf()];
    let noto = "usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
    for (locale, face) in [
        ("zh-CN", 2),
        ("zh_CN.UTF-8", 2),
        ("ja", 0),
        ("ko", 1),
        ("zh-TW", 3),
        ("zh-HK", 4),
        // Locales without CJK text of their own: the Simplified Chinese face.
        ("en", 2),
        ("de", 2),
        ("ru", 2),
    ] {
        assert_eq!(
            rel(dir.path(), find_fallback_font_in(locale, &roots)),
            Some((noto.to_owned(), face)),
            "{locale}"
        );
    }
}

#[test]
fn ranks_candidates_family_first_then_weight() {
    let dir = linux_tree();
    let found: Vec<String> = cjk_font_candidates_in(&[dir.path().to_path_buf()])
        .into_iter()
        .map(|p| {
            p.strip_prefix(dir.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(
        found,
        [
            "usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
            "usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ]
    );
}

#[test]
fn falls_back_to_linux_packages_without_noto() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        b"",
    );
    let zenhei = collection(&[
        ("WenQuanYi Zen Hei", "Regular"),
        ("WenQuanYi Zen Hei Mono", "Regular"),
        ("WenQuanYi Zen Hei Sharp", "Regular"),
    ]);
    write(root, "share/fonts/truetype/wqy/wqy-zenhei.ttc", &zenhei);
    let roots = vec![root.to_path_buf()];
    // WenQuanYi ranks above Droid Sans Fallback, its proportional face first.
    for locale in ["zh-CN", "ja", "ko", "en"] {
        assert_eq!(
            rel(root, find_fallback_font_in(locale, &roots)),
            Some(("share/fonts/truetype/wqy/wqy-zenhei.ttc".to_owned(), 0)),
            "{locale}"
        );
    }
    // A Japanese font is preferred for Japanese even when ranked lower.
    write(
        root,
        "share/fonts/opentype/ipaexfont-gothic/ipaexg.ttf",
        b"",
    );
    assert_eq!(
        rel(root, find_fallback_font_in("ja", &roots)),
        Some((
            "share/fonts/opentype/ipaexfont-gothic/ipaexg.ttf".to_owned(),
            0
        ))
    );
    assert_eq!(
        rel(root, find_fallback_font_in("zh-CN", &roots)).unwrap().0,
        "share/fonts/truetype/wqy/wqy-zenhei.ttc"
    );
}

#[test]
fn windows_fonts_by_script() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Windows/Fonts/msyh.ttc",
        &collection(&[
            ("Microsoft YaHei", "Regular"),
            ("Microsoft YaHei UI", "Regular"),
        ]),
    );
    write(
        root,
        "Windows/Fonts/msyhbd.ttc",
        &collection(&[("Microsoft YaHei", "Bold"), ("Microsoft YaHei UI", "Bold")]),
    );
    write(root, "Windows/Fonts/MALGUN.TTF", b"");
    write(
        root,
        "Windows/Fonts/msgothic.ttc",
        &collection(&[
            ("MS Gothic", "Regular"),
            ("MS UI Gothic", "Regular"),
            ("MS PGothic", "Regular"),
        ]),
    );
    write(
        root,
        "Windows/Fonts/meiryo.ttc",
        &collection(&[("Meiryo", "Regular"), ("Meiryo UI", "Regular")]),
    );
    write(
        root,
        "Windows/Fonts/simsun.ttc",
        &collection(&[("SimSun", "Regular"), ("NSimSun", "Regular")]),
    );
    write(root, "Windows/Fonts/arial.ttf", b"");
    let roots = vec![root.join("Windows/Fonts")];
    assert_eq!(
        rel(root, find_fallback_font_in("zh-CN", &roots)),
        Some(("Windows/Fonts/msyh.ttc".to_owned(), 0))
    );
    assert_eq!(
        rel(root, find_fallback_font_in("ko", &roots)),
        Some(("Windows/Fonts/MALGUN.TTF".to_owned(), 0))
    );
    assert_eq!(
        rel(root, find_fallback_font_in("ja", &roots)),
        Some(("Windows/Fonts/meiryo.ttc".to_owned(), 0))
    );
    // Traditional Chinese has no native font here: the closest is a Simplified one.
    assert_eq!(
        rel(root, find_fallback_font_in("zh-TW", &roots)).unwrap().0,
        "Windows/Fonts/msyh.ttc"
    );
}

#[test]
fn macos_collections_and_japanese_file_names() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let pingfang = collection(&[
        ("PingFang HK", "Regular"),
        ("PingFang SC", "Semibold"),
        ("PingFang SC", "Regular"),
        ("PingFang TC", "Regular"),
    ]);
    write(root, "System/Library/Fonts/PingFang.ttc", &pingfang);
    write(
        root,
        "System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        &collection(&[("Hiragino Sans", "W3"), ("Hiragino Kaku Gothic ProN", "W3")]),
    );
    write(
        root,
        "System/Library/Fonts/ヒラギノ角ゴシック W6.ttc",
        &collection(&[("Hiragino Sans", "W6"), ("Hiragino Kaku Gothic ProN", "W6")]),
    );
    write(
        root,
        "System/Library/Fonts/AppleSDGothicNeo.ttc",
        &collection(&[
            ("Apple SD Gothic Neo", "Regular"),
            ("Apple SD Gothic Neo", "Bold"),
        ]),
    );
    let roots = vec![root.join("System/Library/Fonts")];
    assert_eq!(
        rel(root, find_fallback_font_in("zh-CN", &roots)),
        Some(("System/Library/Fonts/PingFang.ttc".to_owned(), 2))
    );
    assert_eq!(
        rel(root, find_fallback_font_in("zh-HK", &roots)),
        Some(("System/Library/Fonts/PingFang.ttc".to_owned(), 0))
    );
    assert_eq!(
        rel(root, find_fallback_font_in("zh-TW", &roots)),
        Some(("System/Library/Fonts/PingFang.ttc".to_owned(), 3))
    );
    assert_eq!(
        rel(root, find_fallback_font_in("ja", &roots)),
        Some((
            "System/Library/Fonts/ヒラギノ角ゴシック W3.ttc".to_owned(),
            0
        ))
    );
    assert_eq!(
        rel(root, find_fallback_font_in("ko", &roots)),
        Some(("System/Library/Fonts/AppleSDGothicNeo.ttc".to_owned(), 0))
    );
}

#[test]
fn no_cjk_font_means_none() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "fonts/DejaVuSans.ttf", b"");
    write(dir.path(), "fonts/NotoSans-Regular.ttf", b"");
    write(dir.path(), "fonts/msyh.ttc.bak", b"");
    let roots = vec![dir.path().to_path_buf(), dir.path().join("does-not-exist")];
    assert_eq!(find_fallback_font_in("zh-CN", &roots), None);
    assert!(cjk_font_candidates_in(&roots).is_empty());
    assert_eq!(find_fallback_font_in("ja", &[]), None);
}

#[test]
fn earlier_directories_win_ties_and_files_are_listed_once() {
    let user = tempfile::tempdir().unwrap();
    let system = tempfile::tempdir().unwrap();
    let noto = collection(NOTO_CJK_FACES);
    let user_font = write(user.path(), "NotoSansCJK-Regular.ttc", &noto);
    write(
        system.path(),
        "opentype/noto/NotoSansCJK-Regular.ttc",
        &noto,
    );
    let roots = vec![
        user.path().to_path_buf(),
        system.path().to_path_buf(),
        user.path().to_path_buf(),
    ];
    assert_eq!(
        find_fallback_font_in("ko", &roots),
        Some((user_font.clone(), 1))
    );
    let found = cjk_font_candidates_in(&roots);
    assert_eq!(
        found.len(),
        2,
        "the repeated user directory is not listed twice: {found:?}"
    );
    assert_eq!(found[0], user_font);
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_followed_without_loops() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let real = write(
        root,
        "store/NotoSansCJK-Regular.ttc",
        &collection(NOTO_CJK_FACES),
    );
    fs::create_dir_all(root.join("fonts/truetype")).unwrap();
    std::os::unix::fs::symlink(&real, root.join("fonts/truetype/NotoSansCJK-Regular.ttc")).unwrap();
    std::os::unix::fs::symlink(root.join("fonts"), root.join("fonts/truetype/loop")).unwrap();
    std::os::unix::fs::symlink(
        root.join("store/missing.ttc"),
        root.join("fonts/broken.ttc"),
    )
    .unwrap();
    let found = find_fallback_font_in("ja", &[root.join("fonts")]).unwrap();
    assert_eq!(
        found,
        (root.join("fonts/truetype/NotoSansCJK-Regular.ttc"), 0)
    );
    let all = cjk_font_candidates_in(&[root.join("fonts"), root.join("store")]);
    assert_eq!(
        all.len(),
        1,
        "a font reachable through a link and directly is listed once: {all:?}"
    );
}

#[test]
fn system_font_dirs_exist_and_are_distinct() {
    let dirs = system_font_dirs();
    for dir in &dirs {
        assert!(dir.is_dir(), "{}", dir.display());
    }
    let mut unique = dirs.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), dirs.len());
    // Informational: what this machine offers.
    println!("font dirs: {dirs:?}");
    for locale in ["zh-CN", "ja", "ko"] {
        println!("{locale}: {:?}", find_fallback_font_in(locale, &dirs));
    }
}
