//! CJK fallback fonts.
//!
//! The IDE's bundled fonts cover Latin, Greek and Cyrillic; Chinese,
//! Japanese and Korean text needs a system font. The search covers the usual
//! font directories (recursively), recognizes Noto Sans CJK, Source Han
//! Sans and the platforms' CJK fonts by file name, ranks them for the glyph
//! variant of the locale (the same Han characters are drawn differently in
//! Simplified Chinese, Traditional Chinese, Japanese and Korean typography)
//! and picks the right face of a `.ttc` collection from its `name` table.
//! No font is bundled.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::locale::parse_posix_locale;
use crate::sfnt::{self, FaceNames};

/// Directory depth searched below each font directory.
const MAX_DEPTH: usize = 8;

/// Han glyph variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    /// Simplified Chinese.
    Sc,
    /// Traditional Chinese (Taiwan).
    Tc,
    /// Traditional Chinese (Hong Kong).
    Hk,
    Jp,
    Kr,
}

use Variant::{Hk, Jp, Kr, Sc, Tc};

impl Variant {
    /// The variant for a UI locale. Locales that are not Chinese, Japanese or
    /// Korean get Simplified Chinese, the most common CJK text in code.
    fn for_locale(locale: &str) -> Variant {
        let tag = parse_posix_locale(locale).unwrap_or_default();
        let mut subtags = tag.split('-');
        let language = subtags.next().unwrap_or_default();
        let rest: Vec<&str> = subtags.collect();
        match language {
            "ja" => Jp,
            "ko" => Kr,
            "zh" if rest.contains(&"Hans") => Sc,
            "zh" if rest.iter().any(|s| matches!(*s, "HK" | "MO")) => Hk,
            "zh" if rest.iter().any(|s| matches!(*s, "Hant" | "TW")) => Tc,
            _ => Sc,
        }
    }

    /// 0 for the same variant, 1–2 for another Chinese variant (Taiwan and
    /// Hong Kong are closest), 3 for another language.
    fn distance(self, target: Variant) -> u8 {
        let chinese = |v: Variant| matches!(v, Sc | Tc | Hk);
        if self == target {
            0
        } else if matches!((self, target), (Tc, Hk) | (Hk, Tc)) {
            1
        } else if chinese(self) && chinese(target) {
            2
        } else {
            3
        }
    }
}

/// What a font family covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coverage {
    /// Designed for one variant.
    Native(Variant),
    /// Designed for one variant, also complete for kana and hangul.
    Broad(Variant),
    /// Pan-CJK: a collection has one face per variant (Noto Sans CJK, Source
    /// Han Sans).
    Pan,
    /// A large Unicode font without a CJK style of its own.
    Generic,
}

/// Weight penalties, lower is closer to the regular weight UI text uses.
const REGULAR: u8 = 0;
const MEDIUM: u8 = 1;
const LIGHT: u8 = 2;
const SEMIBOLD: u8 = 3;
const BOLD: u8 = 4;
const EXTREME: u8 = 5;

/// A recognized CJK font family.
#[derive(Debug)]
struct Family {
    /// Prefix of the normalized file stem (lower case, without spaces,
    /// hyphens and underscores).
    key: &'static str,
    /// The whole stem must equal `key` (short Windows names such as `msyh`).
    exact: bool,
    coverage: Coverage,
    /// Suitability for UI text, lower is better.
    rank: u8,
    /// Weight penalty of an `exact` entry (`msyhbd` is the bold YaHei).
    weight: u8,
}

const fn prefix(key: &'static str, coverage: Coverage, rank: u8) -> Family {
    Family {
        key,
        exact: false,
        coverage,
        rank,
        weight: REGULAR,
    }
}

const fn exact(key: &'static str, coverage: Coverage, rank: u8, weight: u8) -> Family {
    Family {
        key,
        exact: true,
        coverage,
        rank,
        weight,
    }
}

use Coverage::{Broad, Generic, Native, Pan};

/// Known families, most specific keys first.
const FAMILIES: &[Family] = &[
    // Noto Sans CJK and Source Han Sans (the same design): rank 0.
    prefix("notosanscjksc", Native(Sc), 0),
    prefix("notosanscjktc", Native(Tc), 0),
    prefix("notosanscjkhk", Native(Hk), 0),
    prefix("notosanscjkjp", Native(Jp), 0),
    prefix("notosanscjkkr", Native(Kr), 0),
    prefix("notosanscjk", Pan, 0),
    prefix("notosanssc", Native(Sc), 0),
    prefix("notosanstc", Native(Tc), 0),
    prefix("notosanshk", Native(Hk), 0),
    prefix("notosansjp", Native(Jp), 0),
    prefix("notosanskr", Native(Kr), 0),
    prefix("sourcehansanssc", Native(Sc), 0),
    prefix("sourcehansanscn", Native(Sc), 0),
    prefix("sourcehansanstc", Native(Tc), 0),
    prefix("sourcehansanstw", Native(Tc), 0),
    prefix("sourcehansanshc", Native(Hk), 0),
    prefix("sourcehansanshk", Native(Hk), 0),
    prefix("sourcehansansjp", Native(Jp), 0),
    prefix("sourcehansanskr", Native(Kr), 0),
    prefix("sourcehansansk", Native(Kr), 0),
    prefix("sourcehansans", Pan, 0),
    // Platform UI fonts: PingFang, Hiragino (macOS), YaHei, JhengHei,
    // DengXian, Yu Gothic, Meiryo, Malgun Gothic (Windows), Apple SD Gothic.
    prefix("pingfang", Native(Sc), 1),
    prefix("hiraginosansgb", Native(Sc), 1),
    prefix("hiraginosans", Native(Jp), 1),
    prefix("hiraginokakugothic", Native(Jp), 1),
    prefix("ヒラギノ角ゴ", Native(Jp), 1),
    exact("msyh", Native(Sc), 1, REGULAR),
    exact("msyhl", Native(Sc), 1, LIGHT),
    exact("msyhbd", Native(Sc), 1, BOLD),
    exact("msjh", Native(Tc), 1, REGULAR),
    exact("msjhl", Native(Tc), 1, LIGHT),
    exact("msjhbd", Native(Tc), 1, BOLD),
    exact("deng", Native(Sc), 1, REGULAR),
    exact("dengl", Native(Sc), 1, LIGHT),
    exact("dengb", Native(Sc), 1, BOLD),
    exact("yugothr", Native(Jp), 1, REGULAR),
    exact("yugothm", Native(Jp), 1, MEDIUM),
    exact("yugothl", Native(Jp), 1, LIGHT),
    exact("yugothb", Native(Jp), 1, BOLD),
    prefix("yugothic", Native(Jp), 1),
    exact("meiryo", Native(Jp), 1, REGULAR),
    exact("meiryob", Native(Jp), 1, BOLD),
    exact("malgun", Native(Kr), 1, REGULAR),
    exact("malgunsl", Native(Kr), 1, LIGHT),
    exact("malgunbd", Native(Kr), 1, BOLD),
    prefix("applesdgothicneo", Native(Kr), 1),
    // Common Linux packages and older platform fonts.
    prefix("wqymicrohei", Broad(Sc), 2),
    prefix("wqyzenhei", Broad(Sc), 2),
    prefix("stheiti", Native(Sc), 2),
    prefix("simhei", Native(Sc), 2),
    prefix("nanumgothic", Native(Kr), 2),
    prefix("nanumbarungothic", Native(Kr), 2),
    prefix("ipaexg", Native(Jp), 2),
    prefix("ipagp", Native(Jp), 2),
    exact("ipag", Native(Jp), 2, REGULAR),
    prefix("takaopgothic", Native(Jp), 2),
    prefix("takaogothic", Native(Jp), 2),
    prefix("vlpgothic", Native(Jp), 2),
    prefix("vlgothic", Native(Jp), 2),
    prefix("fontsjapanesegothic", Native(Jp), 2),
    exact("msgothic", Native(Jp), 2, REGULAR),
    prefix("applegothic", Native(Kr), 2),
    exact("gulim", Native(Kr), 2, REGULAR),
    prefix("droidsansfallback", Broad(Sc), 3),
    prefix("undotum", Native(Kr), 3),
    prefix("ヒラギノ丸ゴ", Native(Jp), 3),
    // Monospaced, serif and brush designs: legible, less suited to UI text.
    prefix("notosansmonocjksc", Native(Sc), 3),
    prefix("notosansmonocjktc", Native(Tc), 3),
    prefix("notosansmonocjkhk", Native(Hk), 3),
    prefix("notosansmonocjkjp", Native(Jp), 3),
    prefix("notosansmonocjkkr", Native(Kr), 3),
    prefix("notosansmonocjk", Pan, 3),
    prefix("notoserifcjksc", Native(Sc), 4),
    prefix("notoserifcjktc", Native(Tc), 4),
    prefix("notoserifcjkhk", Native(Hk), 4),
    prefix("notoserifcjkjp", Native(Jp), 4),
    prefix("notoserifcjkkr", Native(Kr), 4),
    prefix("notoserifcjk", Pan, 4),
    prefix("sourcehanserifsc", Native(Sc), 4),
    prefix("sourcehanserifcn", Native(Sc), 4),
    prefix("sourcehanseriftc", Native(Tc), 4),
    prefix("sourcehanseriftw", Native(Tc), 4),
    prefix("sourcehanserifhc", Native(Hk), 4),
    prefix("sourcehanserifhk", Native(Hk), 4),
    prefix("sourcehanserifjp", Native(Jp), 4),
    prefix("sourcehanserifkr", Native(Kr), 4),
    prefix("sourcehanserifk", Native(Kr), 4),
    prefix("sourcehanserif", Pan, 4),
    exact("simsun", Native(Sc), 4, REGULAR),
    prefix("songti", Native(Sc), 4),
    exact("batang", Native(Kr), 4, REGULAR),
    exact("msmincho", Native(Jp), 4, REGULAR),
    prefix("hiraginomincho", Native(Jp), 4),
    prefix("ヒラギノ明朝", Native(Jp), 4),
    prefix("ipaexm", Native(Jp), 4),
    prefix("ipamp", Native(Jp), 4),
    exact("ipam", Native(Jp), 4, REGULAR),
    exact("uming", Pan, 4, REGULAR),
    exact("ukai", Pan, 5, REGULAR),
    // Last resorts with wide coverage.
    prefix("arialuni", Generic, 6),
    prefix("unifont", Generic, 7),
];

/// Face order of the Noto Sans CJK / Source Han Sans collections, used when
/// a collection's names cannot be read.
const PAN_COLLECTION_ORDER: [Variant; 5] = [Jp, Kr, Sc, Tc, Hk];

/// Weight words in file names and style names; longer words before the
/// words they contain.
const WEIGHT_WORDS: &[(&str, u8)] = &[
    ("extralight", EXTREME),
    ("ultralight", EXTREME),
    ("hairline", EXTREME),
    ("thin", EXTREME),
    ("extrabold", EXTREME),
    ("ultrabold", EXTREME),
    ("heavy", EXTREME),
    ("black", EXTREME),
    ("semilight", LIGHT),
    ("demilight", LIGHT),
    ("light", LIGHT),
    ("semibold", SEMIBOLD),
    ("demibold", SEMIBOLD),
    ("bold", BOLD),
    ("medium", MEDIUM),
    ("regular", REGULAR),
    ("normal", REGULAR),
    ("book", REGULAR),
    ("w0", EXTREME),
    ("w1", EXTREME),
    ("w2", LIGHT),
    ("w3", REGULAR),
    ("w4", REGULAR),
    ("w5", MEDIUM),
    ("w6", SEMIBOLD),
    ("w7", BOLD),
    ("w8", EXTREME),
    ("w9", EXTREME),
    // Variable fonts: the default instance is not always the regular one.
    ("variablefont", MEDIUM),
    ("[wght]", MEDIUM),
    ("vf", MEDIUM),
];

/// A CJK font file found on disk.
#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    family: &'static Family,
    /// A `.ttc`/`.otc` collection.
    collection: bool,
    weight: u8,
    /// Monospaced or coding variant (`NanumGothicCoding`).
    mono: bool,
    /// Index of the search directory it was found in.
    root: usize,
}

impl Candidate {
    /// What the file covers: a pan-CJK family outside a collection is a
    /// single-variant font (Adobe's `SourceHanSans-Regular.otf` is Japanese).
    fn coverage(&self) -> Coverage {
        match self.family.coverage {
            Pan if !self.collection => Native(Jp),
            coverage => coverage,
        }
    }

    /// 0: designed for the target variant; 1–2: another Chinese variant;
    /// 3: other languages but complete for kana and hangul; 4: other.
    fn tier(&self, target: Variant) -> u8 {
        match self.coverage() {
            Pan => 0,
            Native(v) => match v.distance(target) {
                3 => 4,
                d => d,
            },
            Broad(v) => v.distance(target),
            Generic => 3,
        }
    }

    fn general_order(&self, other: &Candidate) -> Ordering {
        (self.family.rank, self.weight, self.mono, self.root)
            .cmp(&(other.family.rank, other.weight, other.mono, other.root))
            .then_with(|| self.path.cmp(&other.path))
    }

    fn order_for(&self, other: &Candidate, target: Variant) -> Ordering {
        self.tier(target)
            .cmp(&other.tier(target))
            .then_with(|| self.general_order(other))
    }
}

/// The font directories of this platform that exist, in search order:
///
/// * Linux and other Unix: `$XDG_DATA_HOME/fonts` (`~/.local/share/fonts`),
///   `~/.fonts`, `fonts` under each `$XDG_DATA_DIRS` entry, `/usr/share/fonts`,
///   `/usr/local/share/fonts` (fontconfig's defaults), and the Windows fonts
///   under WSL (`/mnt/c/Windows/Fonts`);
/// * macOS: `~/Library/Fonts`, `/Library/Fonts`, `/System/Library/Fonts`;
/// * Windows: `%WINDIR%\Fonts` and the per-user
///   `%LOCALAPPDATA%\Microsoft\Windows\Fonts`.
pub fn system_font_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = dirs::home_dir();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot"));
        out.push(
            windir
                .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
                .join("Fonts"),
        );
        if let Some(local) = dirs::data_local_dir() {
            out.push(local.join("Microsoft").join("Windows").join("Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = &home {
            out.push(home.join("Library/Fonts"));
        }
        out.extend(
            [
                "/Library/Fonts",
                "/System/Library/Fonts",
                "/Network/Library/Fonts",
            ]
            .map(PathBuf::from),
        );
    } else {
        if let Some(data) = dirs::data_dir() {
            out.push(data.join("fonts"));
        }
        if let Some(home) = &home {
            out.push(home.join(".local/share/fonts"));
            out.push(home.join(".fonts"));
        }
        let data_dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let data_dirs = data_dirs
            .as_deref()
            .unwrap_or("/usr/local/share:/usr/share");
        out.extend(
            data_dirs
                .split(':')
                .filter(|d| !d.is_empty())
                .map(|d| Path::new(d).join("fonts")),
        );
        out.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        if std::env::var_os("WSL_DISTRO_NAME").is_some()
            || Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
        {
            out.push(PathBuf::from("/mnt/c/Windows/Fonts"));
        }
    }
    let mut seen = HashSet::new();
    out.retain(|dir| dir.is_dir() && seen.insert(dir.clone()));
    out
}

/// Every CJK-capable font file in the [`system_font_dirs`], best first:
/// Noto Sans CJK and Source Han Sans, then the platform UI fonts (PingFang,
/// Hiragino, Microsoft YaHei, Malgun Gothic, …), then the other families;
/// regular weights before the others. Independent of the locale.
pub fn cjk_font_candidates() -> Vec<PathBuf> {
    cjk_font_candidates_in(&system_font_dirs())
}

/// [`cjk_font_candidates`] over the given directories, searched recursively.
pub fn cjk_font_candidates_in(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = scan(dirs);
    found.sort_by(Candidate::general_order);
    found.into_iter().map(|c| c.path).collect()
}

/// The fallback font for CJK text in a UI in `locale`, as `(file, face
/// index)` ready for `egui::FontData { index, .. }`; `None` when the system
/// has no CJK font.
///
/// Fonts designed for the locale's glyph variant come first: `zh-CN` gets
/// Simplified Chinese faces (Noto Sans CJK SC, PingFang SC, Microsoft YaHei,
/// WenQuanYi, …), `ja` Japanese ones (Noto Sans CJK JP, Hiragino, Yu Gothic,
/// Meiryo, MS Gothic, …) and `ko` Korean ones (Noto Sans CJK KR, Apple SD
/// Gothic Neo, Malgun Gothic, Nanum Gothic, …). Other locales, whose own
/// scripts the bundled fonts cover, get the Simplified Chinese choice for
/// CJK text in files. In a collection such as `NotoSansCJK-Regular.ttc` the
/// face whose name matches the variant is chosen.
pub fn find_fallback_font(locale: &str) -> Option<(PathBuf, u32)> {
    find_fallback_font_in(locale, &system_font_dirs())
}

/// [`find_fallback_font`] over the given directories, searched recursively.
pub fn find_fallback_font_in(locale: &str, dirs: &[PathBuf]) -> Option<(PathBuf, u32)> {
    let target = Variant::for_locale(locale);
    let best = scan(dirs)
        .into_iter()
        .min_by(|a, b| a.order_for(b, target))?;
    let face = if best.collection {
        select_face(&best.path, best.family.coverage, target)
    } else {
        0
    };
    Some((best.path, face))
}

/// Recognized CJK font files under `dirs`, each file once (symbolic links
/// and repeated directories are resolved).
fn scan(dirs: &[PathBuf]) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (root, dir) in dirs.iter().enumerate() {
        let walker = WalkDir::new(dir)
            .follow_links(true)
            .max_depth(MAX_DEPTH)
            .sort_by_file_name();
        for entry in walker.into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let Some((family, collection, weight, mono)) = classify(entry.path()) else {
                continue;
            };
            let canonical =
                std::fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path().to_path_buf());
            if seen.insert(canonical) {
                out.push(Candidate {
                    path: entry.into_path(),
                    family,
                    collection,
                    weight,
                    mono,
                    root,
                });
            }
        }
    }
    out
}

/// Recognizes a CJK font file by name: its family, whether it is a
/// collection, its weight penalty and whether it is monospaced.
fn classify(path: &Path) -> Option<(&'static Family, bool, u8, bool)> {
    let name = path.file_name()?.to_str()?;
    let (stem, extension) = name.rsplit_once('.')?;
    let collection = match extension.to_ascii_lowercase().as_str() {
        "ttc" | "otc" => true,
        "ttf" | "otf" => false,
        _ => return None,
    };
    let stem = normalize(stem);
    FAMILIES.iter().find_map(|family| {
        let rest = if family.exact {
            (stem == family.key).then_some("")
        } else {
            stem.strip_prefix(family.key)
        }?;
        let weight = if family.exact {
            family.weight
        } else {
            weight_in(rest)
        };
        Some((
            family,
            collection,
            weight,
            rest.contains("mono") || rest.contains("coding"),
        ))
    })
}

/// Lower case without spaces, hyphens and underscores; a kana followed by a
/// combining (semi-)voiced sound mark is composed, as macOS may store
/// `ヒラギノ` decomposed.
fn normalize(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut chars = stem.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, ' ' | '-' | '_') {
            continue;
        }
        let composed = match chars.peek() {
            Some('\u{3099}') if ('\u{3041}'..='\u{30FF}').contains(&c) => {
                char::from_u32(u32::from(c) + 1)
            }
            Some('\u{309A}') if ('\u{3041}'..='\u{30FF}').contains(&c) => {
                char::from_u32(u32::from(c) + 2)
            }
            _ => None,
        };
        if composed.is_some() {
            chars.next();
        }
        out.extend(composed.unwrap_or(c).to_lowercase());
    }
    out
}

/// The weight penalty of the first weight word in a file name remainder.
fn weight_in(rest: &str) -> u8 {
    WEIGHT_WORDS
        .iter()
        .find(|(word, _)| rest.contains(word))
        .map_or(REGULAR, |&(_, weight)| weight)
}

fn words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

fn weight_word(word: &str) -> Option<u8> {
    WEIGHT_WORDS
        .iter()
        .find(|(w, _)| *w == word)
        .map(|&(_, weight)| weight)
}

/// The variant a face's family names declare (`Noto Sans CJK SC`, `PingFang
/// HK`, `Hiragino Sans GB`, `Source Han Sans K`).
fn face_variant(names: &FaceNames) -> Option<Variant> {
    names.families.iter().find_map(|family| {
        let words: Vec<String> = words(family).collect();
        words.iter().enumerate().find_map(|(i, word)| {
            // `J`/`K` (Source Han Sans) only as the last word before a weight.
            let suffix = words[i + 1..].iter().all(|w| weight_word(w).is_some());
            match word.as_str() {
                "sc" | "cn" | "gb" | "hans" | "chs" | "prc" => Some(Sc),
                "tc" | "tw" | "hant" | "cht" => Some(Tc),
                "hk" | "hc" => Some(Hk),
                "jp" | "japanese" => Some(Jp),
                "kr" | "korean" => Some(Kr),
                "j" if suffix => Some(Jp),
                "k" if suffix => Some(Kr),
                _ => None,
            }
        })
    })
}

fn face_weight(names: &FaceNames) -> u8 {
    names
        .styles
        .iter()
        .chain(&names.families)
        .flat_map(|s| words(s))
        .filter_map(|w| weight_word(&w))
        .max()
        .unwrap_or(REGULAR)
}

fn face_is_mono(names: &FaceNames) -> bool {
    names
        .families
        .iter()
        .flat_map(|s| words(s))
        .any(|w| w == "mono")
}

/// The face of a collection to use for `target`: the one whose names declare
/// the variant (else the closest), regular and proportional preferred.
fn select_face(path: &Path, coverage: Coverage, target: Variant) -> u32 {
    let faces = sfnt::read_face_names(path).unwrap_or_default();
    let variants: Vec<Option<Variant>> = faces.iter().map(face_variant).collect();
    if coverage == Pan && variants.iter().all(Option::is_none) {
        return PAN_COLLECTION_ORDER
            .iter()
            .position(|&v| v == target)
            .unwrap_or(0) as u32;
    }
    let score = |v: Option<Variant>| {
        v.map_or(1, |v| {
            if v == target {
                0
            } else {
                1 + v.distance(target)
            }
        })
    };
    let best = faces
        .iter()
        .zip(&variants)
        .enumerate()
        .min_by_key(|&(i, (names, &variant))| {
            (score(variant), face_is_mono(names), face_weight(names), i)
        });
    best.map_or(0, |(i, _)| i as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sfnt::tests::font_file;

    fn family_of(name: &str) -> Option<(&'static str, bool, u8, bool)> {
        classify(Path::new(name))
            .map(|(f, collection, weight, mono)| (f.key, collection, weight, mono))
    }

    #[test]
    fn recognizes_cjk_font_files() {
        assert_eq!(
            family_of("NotoSansCJK-Regular.ttc"),
            Some(("notosanscjk", true, REGULAR, false))
        );
        assert_eq!(
            family_of("NotoSansCJK-Bold.ttc"),
            Some(("notosanscjk", true, BOLD, false))
        );
        assert_eq!(
            family_of("NotoSansCJKsc-Regular.otf"),
            Some(("notosanscjksc", false, REGULAR, false))
        );
        assert_eq!(
            family_of("NotoSansCJK-VF.otf.ttc"),
            Some(("notosanscjk", true, MEDIUM, false))
        );
        assert_eq!(
            family_of("NotoSansSC[wght].ttf"),
            Some(("notosanssc", false, MEDIUM, false))
        );
        assert_eq!(
            family_of("SourceHanSansK-Regular.otf").map(|f| f.0),
            Some("sourcehansanskr")
        );
        assert_eq!(
            family_of("wqy-microhei.ttc"),
            Some(("wqymicrohei", true, REGULAR, false))
        );
        assert_eq!(
            family_of("DroidSansFallbackFull.ttf"),
            Some(("droidsansfallback", false, REGULAR, false))
        );
        assert_eq!(family_of("MSYHBD.TTC"), Some(("msyhbd", true, BOLD, false)));
        assert_eq!(
            family_of("malgun.ttf"),
            Some(("malgun", false, REGULAR, false))
        );
        assert_eq!(
            family_of("YuGothR.ttc"),
            Some(("yugothr", true, REGULAR, false))
        );
        assert_eq!(
            family_of("Hiragino Sans GB.ttc").map(|f| f.0),
            Some("hiraginosansgb")
        );
        assert_eq!(
            family_of("ヒラギノ角ゴシック W6.ttc"),
            Some(("ヒラギノ角ゴ", true, SEMIBOLD, false))
        );
        // macOS may store the name decomposed (ギ = キ + U+3099).
        assert_eq!(
            family_of("ヒラキ\u{3099}ノ角コ\u{3099}シック W3.ttc"),
            Some(("ヒラギノ角ゴ", true, REGULAR, false))
        );
        assert_eq!(
            family_of("NanumGothicCoding.ttf"),
            Some(("nanumgothic", false, REGULAR, true))
        );
        for other in [
            "NotoSans-Regular.ttf",
            "NotoSansKhmer-Regular.ttf",
            "NotoSansTamil.ttf",
            "DejaVuSans.ttf",
            "msyh.ttc.bak",
            "simsunb.ttf",
            "README",
        ] {
            assert_eq!(family_of(other), None, "{other}");
        }
    }

    #[test]
    fn locales_map_to_glyph_variants() {
        for (locale, variant) in [
            ("zh-CN", Sc),
            ("zh_CN.UTF-8", Sc),
            ("zh-Hans-HK", Sc),
            ("zh-TW", Tc),
            ("zh-Hant", Tc),
            ("zh-HK", Hk),
            ("ja", Jp),
            ("ja_JP.UTF-8", Jp),
            ("ko-KR", Kr),
            ("en", Sc),
            ("ru", Sc),
            ("", Sc),
        ] {
            assert_eq!(Variant::for_locale(locale), variant, "{locale}");
        }
    }

    #[test]
    fn face_names_declare_variants() {
        let names = |family: &str| FaceNames {
            families: vec![family.to_owned()],
            styles: vec![],
        };
        assert_eq!(face_variant(&names("Noto Sans CJK SC")), Some(Sc));
        assert_eq!(face_variant(&names("Noto Sans Mono CJK KR")), Some(Kr));
        assert_eq!(face_variant(&names("PingFang HK")), Some(Hk));
        assert_eq!(face_variant(&names("Hiragino Sans GB W3")), Some(Sc));
        assert_eq!(face_variant(&names("AR PL UMing TW MBE")), Some(Tc));
        assert_eq!(face_variant(&names("Source Han Sans K")), Some(Kr));
        assert_eq!(face_variant(&names("Source Han Sans K Bold")), Some(Kr));
        assert_eq!(face_variant(&names("Source Han Sans")), None);
        assert_eq!(face_variant(&names("Microsoft YaHei UI")), None);
        assert_eq!(face_variant(&names("K Gothic")), None);
    }

    #[test]
    fn collection_faces_are_chosen_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PingFang.ttc");
        let faces = [
            ("PingFang HK", "Regular"),
            ("PingFang SC", "Semibold"),
            ("PingFang SC", "Regular"),
            ("PingFang SC", "Ultralight"),
            ("PingFang TC", "Regular"),
        ];
        std::fs::write(&path, font_file(&faces, true)).unwrap();
        assert_eq!(select_face(&path, Native(Sc), Sc), 2);
        assert_eq!(select_face(&path, Native(Sc), Tc), 4);
        assert_eq!(select_face(&path, Native(Sc), Hk), 0);
        assert_eq!(
            select_face(&path, Native(Sc), Jp),
            0,
            "no Japanese face: the first regular one"
        );

        let unreadable = dir.path().join("NotoSansCJK-Regular.ttc");
        std::fs::write(&unreadable, b"").unwrap();
        assert_eq!(
            select_face(&unreadable, Pan, Sc),
            2,
            "the published Noto Sans CJK order"
        );
        assert_eq!(select_face(&unreadable, Pan, Kr), 1);
        assert_eq!(select_face(&unreadable, Native(Sc), Kr), 0);
    }
}
