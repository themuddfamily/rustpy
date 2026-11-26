//! CJK fallback fonts (roadmap §8, "fallback fonts").
//!
//! The bundled fonts (egui's and Hack) have no Chinese, Japanese or Korean glyphs.
//! [`CjkFonts`], an egui plugin, appends system fonts found by `rustpy-i18n` to every font
//! family (proportional, monospace and the editor's named families) as the last fallbacks:
//!
//! * **UI language.** For a Chinese, Japanese or Korean UI the font with that language's
//!   glyph forms is read before the first frame (in the background after a switch in
//!   Settings) and becomes the first fallback.
//! * **Text that needs it.** Nothing else is loaded up front. After each frame the plugin
//!   looks at the painted text (only galleys that were not painted in the previous frame)
//!   for CJK characters that no font of their family draws (egui's emoji fonts draw a few,
//!   such as ㊙ and 〽), and looks up a font for their script in the background: Hangul in a
//!   Korean font, kana in a Japanese one, Han in the variant of the UI or system language,
//!   then in the others. The same Han characters are drawn differently in Chinese, Japanese
//!   and Korean typography, and a font for one language often lacks the characters of
//!   another (Microsoft YaHei has no Hangul; Malgun Gothic and Yu Gothic lack many
//!   simplified Chinese characters), so a further font is added when painted text needs it.
//!   The CJK characters of every face looked at are remembered: a face is read again only
//!   for characters it has, and characters that none of the faces has (a rare ideograph,
//!   say) are not looked up again, while the other characters of their script still are.
//!
//! egui centers a fallback font's line box in the one of the family's first font, so a font
//! whose ascent, descent and line gap are split differently sits above or below the Latin
//! baseline (Yu Gothic's large line gap lifts it by 0.28 em). Each fallback is installed with
//! the `FontTweak::y_offset_factor` that puts its baseline on the family's.
//!
//! A font file is read when it is first needed and kept for the rest of the session (5 to
//! 20 MB; fonts are only ever added or reordered), once per file: the faces of a collection
//! (`NotoSansCJK-Regular.ttc` has the Japanese, Korean and Chinese ones) share it. Its bytes
//! are static: egui copies owned font data each time it rebuilds its fonts. Every font
//! installed is logged with the time its lookup took.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crossbeam_channel::{Receiver, TryRecvError};
use egui::epaint::ClippedShape;
use egui::text::Galley;
use egui::{
    Context, FontData, FontDefinitions, FontFamily, FontTweak, FullOutput, RawInput, Shape,
};
use rustc_hash::{FxHashMap, FxHashSet};
use skrifa::instance::{LocationRef, Size};
use skrifa::MetadataProvider;

/// Prefix of the fallback fonts' names in egui's font definitions.
const NAME_PREFIX: &str = "rustpy-cjk:";

/// Missing characters kept per script for the next lookup.
const SAMPLE: usize = 8;

/// A font face: its file and its index in the file (0 unless the file is a collection).
pub(crate) type FaceKey = (PathBuf, u32);

/// Writing systems with their own font needs, in the order fonts are looked up for them:
/// Korean and Japanese fonts have most Han characters too, Chinese fonts often no Hangul.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Script {
    Hangul,
    /// Hiragana and katakana.
    Kana,
    /// Han ideographs, Bopomofo, CJK punctuation, symbols and fullwidth forms.
    Han,
}

impl Script {
    const ALL: [Script; 3] = [Script::Hangul, Script::Kana, Script::Han];

    fn index(self) -> usize {
        self as usize
    }
}

/// The CJK script of `c`; `None` for every other character.
pub(crate) fn script_of(c: char) -> Option<Script> {
    match u32::from(c) {
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7FF | 0xFFA0..=0xFFDC => {
            Some(Script::Hangul)
        }
        0x3040..=0x30FF
        | 0x31F0..=0x31FF
        | 0x32D0..=0x32FE
        | 0xFF66..=0xFF9F
        | 0x1AFF0..=0x1B16F => Some(Script::Kana),
        0x2E80..=0x2FDF
        | 0x2FF0..=0x303F
        | 0x3100..=0x312F
        | 0x31A0..=0x31EF
        | 0x3200..=0x32CF
        | 0x32FF..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF01..=0xFF65
        | 0xFFE0..=0xFFEE
        | 0x20000..=0x323AF => Some(Script::Han),
        _ => None,
    }
}

/// Whether `text` may contain a CJK character. Every character from U+1100 on is encoded
/// with a lead byte of 0xE1 or more, so ASCII and Latin text is ruled out 32 bytes at a time.
fn may_contain_cjk(text: &str) -> bool {
    text.as_bytes()
        .chunks(32)
        .any(|chunk| chunk.iter().fold(0, |max, &b| max.max(b)) >= 0xE1)
}

/// A set of characters: a bitmap of the Basic Multilingual Plane, a sorted list of the rest.
#[derive(Clone, Default)]
pub(crate) struct CharSet {
    bmp: Vec<u64>,
    astral: Vec<u32>,
    len: usize,
}

impl CharSet {
    pub(crate) fn from_chars(chars: impl IntoIterator<Item = char>) -> Self {
        let mut set = CharSet {
            bmp: vec![0; 0x10000 / 64],
            astral: Vec::new(),
            len: 0,
        };
        for c in chars {
            let u = u32::from(c);
            if u < 0x10000 {
                let (word, bit) = ((u / 64) as usize, 1u64 << (u % 64));
                if set.bmp[word] & bit == 0 {
                    set.bmp[word] |= bit;
                    set.len += 1;
                }
            } else {
                set.astral.push(u);
            }
        }
        set.astral.sort_unstable();
        set.astral.dedup();
        set.len += set.astral.len();
        set
    }

    pub(crate) fn contains(&self, c: char) -> bool {
        let u = u32::from(c);
        if u < 0x10000 {
            self.bmp
                .get((u / 64) as usize)
                .is_some_and(|word| word & (1u64 << (u % 64)) != 0)
        } else {
            self.astral.binary_search(&u).is_ok()
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The middle of a font face's line box relative to its baseline, in em: `(ascent + descent -
/// line gap) / 2` with the metrics egui uses (descent is negative). egui centers a fallback
/// font's line box on the primary font's, so the difference of two faces' values is how far
/// the fallback's baseline lies below the primary's. `None` for data skrifa cannot read.
pub(crate) fn vertical_center(bytes: &[u8], index: u32) -> Option<f32> {
    let face = skrifa::FontRef::from_index(bytes, index).ok()?;
    let m = face.metrics(Size::unscaled(), LocationRef::default());
    let em = f32::from(m.units_per_em);
    (em > 0.0).then(|| (m.ascent + m.descent - m.leading) / em / 2.0)
}

/// The CJK characters of face `index` of a font file, by its charmap as egui finds the face
/// for a character (a character mapped to the `.notdef` glyph is not in the face).
pub(crate) fn cjk_chars(bytes: &[u8], index: u32) -> Result<CharSet, String> {
    cjk_chars_where(bytes, index, |_| true)
}

/// [`cjk_chars`] that `keep` accepts.
fn cjk_chars_where(
    bytes: &[u8],
    index: u32,
    keep: impl Fn(char) -> bool,
) -> Result<CharSet, String> {
    let face = skrifa::FontRef::from_index(bytes, index).map_err(|e| e.to_string())?;
    let mapped = face
        .charmap()
        .mappings()
        .filter(|&(_, glyph)| glyph != skrifa::GlyphId::NOTDEF);
    Ok(CharSet::from_chars(
        mapped
            .filter_map(|(u, _)| char::from_u32(u))
            .filter(|&c| script_of(c).is_some() && keep(c)),
    ))
}

/// Whether egui never draws `c` with its built-in font `name` although the font maps it: epaint
/// leaves 卍 and 卐 out of its own fonts (`FontFace::ignore_character`), and `emoji-icon-font`
/// has both.
fn egui_ignores(name: &str, c: char) -> bool {
    matches!(c, '\u{534D}' | '\u{5350}') && FontDefinitions::builtin_font_names().contains(&name)
}

/// The CJK characters egui draws with the font `name` (`data`); none for data skrifa cannot read.
fn drawn_cjk_chars(name: &str, data: &FontData) -> CharSet {
    cjk_chars_where(&data.font, data.index, |c| !egui_ignores(name, c)).unwrap_or_default()
}

/// A font face read from disk.
pub(crate) struct LoadedFont {
    path: PathBuf,
    index: u32,
    /// The locale it was looked up for, which decides its Han glyph forms.
    locale: String,
    /// The font file, kept by [`FontFiles`] (static: see the module documentation).
    data: &'static [u8],
    /// [`vertical_center`] of the face.
    center: f32,
    /// Its CJK characters.
    chars: CharSet,
}

impl LoadedFont {
    /// The CJK characters and the [`vertical_center`] of face `index` of a font file. Fails for
    /// data egui cannot use: egui panics on a font its parser rejects, so the face is opened
    /// with the same parser (skrifa) first.
    pub(crate) fn inspect(bytes: &[u8], index: u32) -> Result<(CharSet, f32), String> {
        let chars = cjk_chars(bytes, index)?;
        if chars.is_empty() {
            return Err("no Chinese, Japanese or Korean characters".into());
        }
        let center = vertical_center(bytes, index).ok_or("unreadable metrics")?;
        Ok((chars, center))
    }

    /// Face `index` of the font file `data` (kept by [`FontFiles`]), inspected with
    /// [`LoadedFont::inspect`].
    pub(crate) fn new(
        path: PathBuf,
        index: u32,
        locale: &str,
        data: &'static [u8],
        chars: CharSet,
        center: f32,
    ) -> Self {
        Self {
            path,
            index,
            locale: locale.to_owned(),
            data,
            center,
            chars,
        }
    }

    fn key(&self) -> FaceKey {
        (self.path.clone(), self.index)
    }
}

/// The font files of installed faces, each read once and kept for the rest of the session:
/// the faces of a collection share one copy.
pub(crate) struct FontFiles {
    read: fn(&Path) -> std::io::Result<Vec<u8>>,
    kept: Mutex<Vec<(PathBuf, &'static [u8])>>,
}

impl FontFiles {
    /// Files read with `read` (`std::fs::read`, or a stand-in in tests).
    pub(crate) const fn new(read: fn(&Path) -> std::io::Result<Vec<u8>>) -> Self {
        Self {
            read,
            kept: Mutex::new(Vec::new()),
        }
    }

    /// The content of the font file `path`: the kept copy, else read from disk.
    fn bytes(&self, path: &Path) -> std::io::Result<Cow<'static, [u8]>> {
        let kept = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(p, _)| p == path)
            .map(|&(_, data)| data);
        match kept {
            Some(data) => Ok(Cow::Borrowed(data)),
            None => (self.read)(path).map(Cow::Owned),
        }
    }

    /// Keeps `bytes`, the content of `path`, for the rest of the session (once per file).
    fn keep(&self, path: &Path, bytes: Cow<'static, [u8]>) -> &'static [u8] {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(&(_, data)) = kept.iter().find(|(p, _)| p == path) {
            return data;
        }
        let data: &'static [u8] = match bytes {
            Cow::Borrowed(data) => data,
            Cow::Owned(bytes) => Box::leak(bytes.into_boxed_slice()),
        };
        log::info!(
            "keeping the font file {} ({:.1} MB) for the session",
            path.display(),
            data.len() as f64 / 1e6
        );
        kept.push((path.to_owned(), data));
        data
    }
}

/// What a font is looked up for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Need {
    /// The glyph forms of the UI language (`zh-CN`, `ja` or `ko`).
    UiLanguage(String),
    /// Painted characters of a script that no installed font has.
    Script(Script),
}

impl fmt::Display for Need {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Need::UiLanguage(locale) => write!(f, "the UI language {locale}"),
            Need::Script(script) => write!(f, "{script:?} text"),
        }
    }
}

/// A font lookup, run by a [`Loader`] on a background thread (before the first frame for the
/// UI language).
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub need: Need,
    /// Locales to look fonts up for, best first.
    pub locales: Vec<String>,
    /// Installed faces (file, index).
    pub installed: Vec<FaceKey>,
    /// Faces read before that have none of `chars` (or cannot be used): not read again.
    pub skip: Vec<FaceKey>,
    /// Characters the font should have (one of them is enough); empty for the UI language.
    pub chars: Vec<char>,
}

/// What a lookup found.
pub(crate) enum Found {
    /// A face read from disk.
    New(LoadedFont),
    /// The UI language's face, which is installed already.
    Installed(PathBuf, u32),
    /// No face that is not installed has any of the characters (or, for the UI language,
    /// can be read).
    Nothing,
    /// The system has no CJK font at all.
    NoCjkFonts,
}

/// The result of a [`Request`].
pub(crate) struct Outcome {
    pub found: Found,
    /// Faces read that have none of the requested characters, with their CJK characters
    /// (none for a face that cannot be used).
    pub inspected: Vec<(FaceKey, CharSet)>,
}

/// Runs a [`Request`]: [`load_system_font`], or a stand-in in tests.
pub(crate) type Loader = Arc<dyn Fn(&Request) -> Outcome + Send + Sync>;

/// [`load_font`] on the system's fonts. `rustpy_i18n::find_fallback_font` walks the font
/// directories, so its answer for a locale is reused for the rest of the session.
pub(crate) fn load_system_font(request: &Request) -> Outcome {
    fn read(path: &Path) -> std::io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    static FILES: FontFiles = FontFiles::new(read);
    static FOUND: Mutex<Vec<(String, Option<FaceKey>)>> = Mutex::new(Vec::new());
    let find = |locale: &str| {
        let known = FOUND
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(l, _)| l == locale)
            .map(|(_, found)| found.clone());
        known.unwrap_or_else(|| {
            let found = rustpy_i18n::find_fallback_font(locale);
            FOUND
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((locale.to_owned(), found.clone()));
            found
        })
    };
    load_font(request, &find, &FILES)
}

/// Looks up a font with `find` (`rustpy_i18n::find_fallback_font`) for each locale of
/// `request` in turn and takes the first face that is neither installed nor skipped and has
/// one of the requested characters (any face for the UI language). Its file is kept in
/// `files`.
pub(crate) fn load_font(
    request: &Request,
    find: &dyn Fn(&str) -> Option<FaceKey>,
    files: &FontFiles,
) -> Outcome {
    let start = Instant::now();
    let mut inspected: Vec<(FaceKey, CharSet)> = Vec::new();
    let mut found_any = false;
    for locale in &request.locales {
        let Some(key) = find(locale) else { continue };
        found_any = true;
        if request.installed.contains(&key) {
            if matches!(request.need, Need::UiLanguage(_)) {
                return Outcome {
                    found: Found::Installed(key.0, key.1),
                    inspected,
                };
            }
            continue;
        }
        if request.skip.contains(&key) || inspected.iter().any(|(k, _)| *k == key) {
            continue;
        }
        let (path, index) = (&key.0, key.1);
        let read = files
            .bytes(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                LoadedFont::inspect(&bytes, index).map(|(chars, center)| (bytes, chars, center))
            });
        let chars = match read {
            Ok((bytes, chars, center))
                if request.chars.is_empty() || request.chars.iter().any(|&c| chars.contains(c)) =>
            {
                let shared = if matches!(bytes, Cow::Borrowed(_)) {
                    ", file already in memory"
                } else {
                    ""
                };
                log::info!("CJK fallback font for {}: {} (face {index}, {} CJK characters{shared}), loaded in {:.0} ms", request.need, path.display(), chars.len(), start.elapsed().as_secs_f64() * 1000.0);
                let data = files.keep(path, bytes);
                return Outcome {
                    found: Found::New(LoadedFont::new(
                        path.clone(),
                        index,
                        locale,
                        data,
                        chars,
                        center,
                    )),
                    inspected,
                };
            }
            Ok((_, chars, _)) => {
                log::info!(
                    "the CJK font {} (face {index}) has none of {:?}",
                    path.display(),
                    request.chars
                );
                chars
            }
            Err(e) => {
                log::warn!(
                    "ignoring the CJK font {} (face {index}): {e}",
                    path.display()
                );
                CharSet::default()
            }
        };
        inspected.push((key, chars));
    }
    if !found_any {
        log::warn!("no Chinese, Japanese or Korean font found, such text shows as boxes: install Noto Sans CJK (e.g. the fonts-noto-cjk package)");
        return Outcome {
            found: Found::NoCjkFonts,
            inspected,
        };
    }
    if request.chars.is_empty() {
        log::info!(
            "no further CJK font for {} ({:.0} ms)",
            request.need,
            start.elapsed().as_secs_f64() * 1000.0
        );
    } else {
        log::info!(
            "no CJK font for {} has any of {:?}: they show as boxes ({:.0} ms)",
            request.need,
            request.chars,
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
    Outcome {
        found: Found::Nothing,
        inspected,
    }
}

/// Locales to look fonts up for, for characters of `script`: Hangul in Korean fonts, kana in
/// Japanese ones, Han in the variant of the UI language, else of the system's message locale
/// (`ja_JP.UTF-8` gives Japanese glyph forms to Han text in an English UI; `zh_TW` gives
/// Traditional Chinese ones), else Simplified Chinese, then in the other variants.
pub(crate) fn script_locales(
    script: Script,
    ui_locale: Option<&str>,
    system_tag: Option<&str>,
) -> Vec<String> {
    match script {
        Script::Hangul => vec!["ko".to_owned()],
        Script::Kana => vec!["ja".to_owned()],
        Script::Han => {
            let first = ui_locale
                .or(system_tag.filter(|tag| super::is_cjk(tag)))
                .unwrap_or("zh-CN");
            let mut locales = vec![first.to_owned()];
            for locale in ["zh-CN", "ja", "ko"] {
                if !locales.iter().any(|l| l == locale) {
                    locales.push(locale.to_owned());
                }
            }
            locales
        }
    }
}

/// Where a font draws its baseline, as a [`vertical_center`] that includes its scale and
/// vertical offset tweaks.
fn drawn_center(data: &FontData) -> Option<f32> {
    let center = vertical_center(&data.font, data.index)?;
    Some(data.tweak.scale * (center + data.tweak.y_offset_factor))
}

/// `defs` with `fonts` appended, in order, to every font family as the last fallbacks; the
/// fallbacks of an earlier call are replaced. Each fallback gets the vertical offset that
/// puts its baseline on the one of the family's first font (one font entry per face and
/// offset, sharing the static font data).
pub(crate) fn with_fallbacks(
    mut defs: FontDefinitions,
    fonts: &[Arc<LoadedFont>],
) -> FontDefinitions {
    defs.font_data
        .retain(|name, _| !name.starts_with(NAME_PREFIX));
    for family in defs.families.values_mut() {
        family.retain(|name| !name.starts_with(NAME_PREFIX));
        let primary = family
            .first()
            .and_then(|name| defs.font_data.get(name))
            .and_then(|data| drawn_center(data));
        for font in fonts {
            // Rounded to 1/100 em (0.13 px at 13 pt) so families with similar first fonts share an entry.
            let y_offset = primary.map_or(0.0, |center| {
                ((center - font.center) * 100.0).round() / 100.0 + 0.0
            });
            let name = format!(
                "{NAME_PREFIX}{}#{}{y_offset:+.2}",
                font.path.display(),
                font.index
            );
            if !defs.font_data.contains_key(&name) {
                let mut data = FontData::from_static(font.data);
                data.index = font.index;
                data.tweak = FontTweak {
                    y_offset_factor: y_offset,
                    ..FontTweak::default()
                };
                defs.font_data.insert(name.clone(), Arc::new(data));
            }
            family.push(name);
        }
    }
    defs
}

/// Which font data a font name stands for: the address and length of the file and the face
/// index. The IDE's font names keep their data, but anything may call `Context::set_fonts`.
type FontIdentity = (usize, usize, u32);

fn font_identity(data: &FontData) -> FontIdentity {
    (data.font.as_ptr() as usize, data.font.len(), data.index)
}

/// A lookup running in the background.
struct Pending {
    need: Need,
    /// The characters it looks for.
    chars: Vec<char>,
    result: Receiver<Outcome>,
}

/// The egui plugin installing CJK fallback fonts (see the module documentation).
pub(crate) struct CjkFonts {
    loader: Loader,
    /// Installed fallback fonts, in fallback order.
    fonts: Vec<Arc<LoadedFont>>,
    /// The UI language when it is Chinese, Japanese or Korean: its font comes first.
    ui_locale: Option<String>,
    /// The UI language's font is still to be looked up.
    ui_font_wanted: bool,
    /// The environment's message locale (`ja_JP.UTF-8`), for the glyph forms of Han text in a
    /// UI in another language.
    system_tag: Option<String>,
    /// Painted characters no font draws: a sample per script, for the next lookup.
    wanted: [Vec<char>; 3],
    /// The CJK characters egui draws with its fonts other than the fallbacks, by font name,
    /// with the font data they were read from: no fallback is needed for them (egui's emoji
    /// fonts have ㊙ and 〽, `emoji-icon-font` fullwidth Latin letters and punctuation).
    own_chars: FxHashMap<String, (FontIdentity, Arc<CharSet>)>,
    /// Painted characters that none of the fonts found for their script has: they show as
    /// boxes and are not looked up again (the other characters of the script still are).
    unfixable: FxHashSet<char>,
    /// Faces read that are not installed, with their CJK characters (none when unusable): a
    /// lookup reads one again only for characters it has.
    known: Vec<(FaceKey, CharSet)>,
    /// Scripts no further font is looked up for: their lookup failed, or the system has no
    /// CJK font.
    exhausted: [bool; 3],
    /// The system has no CJK font at all.
    no_cjk_fonts: bool,
    /// The lookup running in the background.
    pending: Option<Pending>,
    /// `fonts` changed: handed to egui by the next `input_hook`.
    dirty: bool,
    /// A frame has run, so egui has fonts (`Context::fonts` panics before).
    ready: bool,
    /// Galleys painted in the previous frame, which were looked at already, by address (the
    /// `Arc`s keep the addresses from being reused).
    seen: FxHashMap<usize, Arc<Galley>>,
}

impl CjkFonts {
    /// `system_tag`: the environment's message locale (`LC_ALL` / `LC_MESSAGES` / `LANG`).
    pub(crate) fn new(loader: Loader, system_tag: Option<String>) -> Self {
        Self {
            loader,
            fonts: Vec::new(),
            ui_locale: None,
            ui_font_wanted: false,
            system_tag,
            wanted: Default::default(),
            own_chars: FxHashMap::default(),
            unfixable: FxHashSet::default(),
            known: Vec::new(),
            exhausted: [false; 3],
            no_cjk_fonts: false,
            pending: None,
            dirty: false,
            ready: false,
            seen: FxHashMap::default(),
        }
    }

    /// Sets the UI language: `Some` for Chinese, Japanese or Korean, whose font becomes the
    /// first fallback. An installed one moves to the front; otherwise it is looked up, right
    /// away when `blocking` (before the first frame: the UI would show boxes without it), else
    /// in the background.
    pub(crate) fn set_ui_locale(&mut self, ctx: &Context, locale: Option<&str>, blocking: bool) {
        self.ui_locale = locale.map(str::to_owned);
        self.ui_font_wanted = false;
        let Some(locale) = locale else { return };
        if let Some(i) = self.fonts.iter().position(|f| f.locale == locale) {
            if i > 0 {
                let font = self.fonts.remove(i);
                self.fonts.insert(0, font);
                self.dirty = true;
                ctx.request_repaint();
            }
            return;
        }
        if self.no_cjk_fonts {
            return;
        }
        if blocking && self.pending.is_none() {
            let request = self.ui_request(locale);
            let outcome = (self.loader)(&request);
            self.on_outcome(request.need, &request.chars, outcome);
        } else {
            self.ui_font_wanted = true;
            self.request_next(ctx);
        }
    }

    /// Whether the UI language is Chinese, Japanese or Korean but no CJK font is installed
    /// (nor being looked up): the UI shows boxes.
    pub(crate) fn ui_font_missing(&self) -> bool {
        self.ui_locale.is_some()
            && self.fonts.is_empty()
            && self.pending.is_none()
            && !self.ui_font_wanted
    }

    /// The locale whose glyph forms CJK text should have: the UI language when it is Chinese,
    /// Japanese or Korean, else the system's message locale when it is. A font looked up for
    /// it goes before the other fallbacks.
    fn preferred(&self) -> Option<&str> {
        self.ui_locale
            .as_deref()
            .or(self.system_tag.as_deref().filter(|tag| super::is_cjk(tag)))
    }

    fn installed(&self) -> Vec<FaceKey> {
        self.fonts.iter().map(|f| f.key()).collect()
    }

    fn ui_request(&self, locale: &str) -> Request {
        Request {
            need: Need::UiLanguage(locale.to_owned()),
            locales: vec![locale.to_owned()],
            installed: self.installed(),
            skip: Vec::new(),
            chars: Vec::new(),
        }
    }

    /// Whether an installed fallback font has `c`.
    fn covers(&self, c: char) -> bool {
        self.fonts.iter().any(|f| f.chars.contains(c))
    }

    /// Starts the next lookup unless one is running: the UI language's font first, then a
    /// font for the first script with characters no font draws.
    fn request_next(&mut self, ctx: &Context) {
        if self.pending.is_some() || self.no_cjk_fonts {
            return;
        }
        let request = match (&self.ui_locale, self.ui_font_wanted) {
            (Some(locale), true) => self.ui_request(locale),
            _ => {
                let Some(script) = Script::ALL
                    .into_iter()
                    .find(|s| !self.exhausted[s.index()] && !self.wanted[s.index()].is_empty())
                else {
                    return;
                };
                let chars = self.wanted[script.index()].clone();
                // Faces known to have none of the characters are not read again.
                let skip = self
                    .known
                    .iter()
                    .filter(|(_, has)| !chars.iter().any(|&c| has.contains(c)))
                    .map(|(key, _)| key.clone())
                    .collect();
                Request {
                    need: Need::Script(script),
                    locales: script_locales(
                        script,
                        self.ui_locale.as_deref(),
                        self.system_tag.as_deref(),
                    ),
                    installed: self.installed(),
                    skip,
                    chars,
                }
            }
        };
        self.ui_font_wanted = false;
        let (tx, rx) = crossbeam_channel::bounded(1);
        let loader = Arc::clone(&self.loader);
        let (need, chars) = (request.need.clone(), request.chars.clone());
        let repaint = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("cjk-font".into())
            .spawn(move || {
                let outcome = loader(&request);
                if tx.send(outcome).is_ok() {
                    repaint.request_repaint();
                }
            });
        match spawned {
            Ok(_) => {
                self.pending = Some(Pending {
                    need,
                    chars,
                    result: rx,
                })
            }
            Err(e) => {
                log::warn!("cannot start the CJK font lookup: {e}");
                self.on_failure(&need);
            }
        }
    }

    /// Takes in the result of a lookup for `chars`.
    fn on_outcome(&mut self, need: Need, chars: &[char], outcome: Outcome) {
        for (key, has) in outcome.inspected {
            self.known.retain(|(k, _)| *k != key);
            self.known.push((key, has));
        }
        let for_ui = matches!(&need, Need::UiLanguage(locale) if self.ui_locale.as_deref() == Some(locale.as_str()));
        match outcome.found {
            Found::New(font) => {
                let key = font.key();
                self.known.retain(|(k, _)| *k != key);
                let font = Arc::new(font);
                if self.preferred() == Some(font.locale.as_str()) {
                    self.fonts.insert(0, font);
                } else {
                    self.fonts.push(font);
                }
                self.dirty = true;
            }
            Found::Installed(path, index) => {
                if let Some(i) = self
                    .fonts
                    .iter()
                    .position(|f| f.path == path && f.index == index)
                {
                    if for_ui && i > 0 {
                        let font = self.fonts.remove(i);
                        self.fonts.insert(0, font);
                        self.dirty = true;
                    }
                }
            }
            Found::Nothing => {
                // No font for the script has them: they stay boxes, and only they are given up.
                if matches!(need, Need::Script(_)) {
                    self.unfixable.extend(chars.iter().copied());
                }
            }
            Found::NoCjkFonts => {
                self.no_cjk_fonts = true;
                self.exhausted = [true; 3];
            }
        }
        self.refresh_wanted();
        // Look at all painted text again: a galley painted unchanged may have more missing
        // characters than the sample took.
        self.seen.clear();
    }

    /// A lookup that could not run or panicked: its script is not looked up again.
    fn on_failure(&mut self, need: &Need) {
        log::warn!("the CJK font lookup for {need} failed");
        if let Need::Script(script) = need {
            self.exhausted[script.index()] = true;
        }
        self.refresh_wanted();
    }

    /// Drops the wanted characters an installed font has, that no font has, or whose script
    /// is no longer looked up.
    fn refresh_wanted(&mut self) {
        for script in Script::ALL {
            let i = script.index();
            let mut wanted = std::mem::take(&mut self.wanted[i]);
            if self.exhausted[i] {
                wanted.clear();
            } else {
                wanted.retain(|c| !self.covers(*c) && !self.unfixable.contains(c));
            }
            self.wanted[i] = wanted;
        }
    }

    /// Hands the font definitions with the current fallbacks to egui; they take effect at the
    /// start of the next pass, the current frame's when called from `input_hook`.
    fn install(&mut self, ctx: &Context) {
        let base = if self.ready {
            ctx.fonts(|f| f.definitions().clone())
        } else {
            crate::theme::font_definitions()
        };
        ctx.set_fonts(with_fallbacks(base, &self.fonts));
        self.dirty = false;
    }

    /// Notes the CJK characters of newly painted text that no font of their family draws.
    fn scan(&mut self, ctx: &Context, shapes: &[ClippedShape]) {
        if self.exhausted.iter().all(|&e| e) {
            self.seen.clear();
            return;
        }
        let mut seen = FxHashMap::with_capacity_and_hasher(self.seen.len(), Default::default());
        let mut new = Vec::new();
        for clipped in shapes {
            self.collect_new(&clipped.shape, &mut seen, &mut new);
        }
        self.seen = seen;
        if new.is_empty() {
            return;
        }
        let mut families: Vec<FontFamily> = Vec::new();
        for section in new.iter().flat_map(|galley| &galley.job.sections) {
            if !families.contains(&section.format.font_id.family) {
                families.push(section.format.font_id.family.clone());
            }
        }
        let own = self.own_fonts(ctx, families);
        for galley in &new {
            self.note_missing(galley, &own);
        }
    }

    /// The CJK characters of each family's own fonts (those that are not fallbacks of the
    /// plugin), which egui draws without a fallback. They are read from the fonts' charmaps:
    /// `Fonts::has_glyph` cannot tell, as it says no for every character of the face that also
    /// has the replacement glyph ◻ (NotoEmoji in egui's families).
    fn own_fonts(
        &mut self,
        ctx: &Context,
        families: Vec<FontFamily>,
    ) -> Vec<(FontFamily, Vec<Arc<CharSet>>)> {
        let cache = &mut self.own_chars;
        ctx.fonts(|f| {
            let defs = f.definitions();
            families
                .into_iter()
                .map(|family| {
                    let names = defs
                        .families
                        .get(&family)
                        .map(Vec::as_slice)
                        .unwrap_or_default();
                    let chars = names
                        .iter()
                        .filter(|name| !name.starts_with(NAME_PREFIX))
                        .filter_map(|name| {
                            let data = defs.font_data.get(name)?;
                            let identity = font_identity(data);
                            let chars = match cache.get(name) {
                                Some((read_from, chars)) if *read_from == identity => {
                                    Arc::clone(chars)
                                }
                                _ => {
                                    let chars = Arc::new(drawn_cjk_chars(name, data));
                                    cache.insert(name.clone(), (identity, Arc::clone(&chars)));
                                    chars
                                }
                            };
                            Some(chars)
                        })
                        .collect();
                    (family, chars)
                })
                .collect()
        })
    }

    /// Adds the galleys of `shape` with CJK text that were not painted in the previous frame
    /// to `new`.
    fn collect_new(
        &self,
        shape: &Shape,
        seen: &mut FxHashMap<usize, Arc<Galley>>,
        new: &mut Vec<Arc<Galley>>,
    ) {
        match shape {
            Shape::Text(text) => {
                let key = Arc::as_ptr(&text.galley) as usize;
                if seen.contains_key(&key) {
                    return;
                }
                if !self.seen.contains_key(&key) && may_contain_cjk(text.galley.text()) {
                    new.push(Arc::clone(&text.galley));
                }
                seen.insert(key, Arc::clone(&text.galley));
            }
            Shape::Vec(shapes) => {
                for shape in shapes {
                    self.collect_new(shape, seen, new);
                }
            }
            _ => {}
        }
    }

    /// Adds the CJK characters of `galley` that no font of their family draws (`own`: the
    /// characters of the families' own fonts) to `wanted`, up to the sample size.
    fn note_missing(&mut self, galley: &Galley, own: &[(FontFamily, Vec<Arc<CharSet>>)]) {
        let text = galley.text();
        for section in &galley.job.sections {
            let Some(part) = text.get(section.byte_range.start.0..section.byte_range.end.0) else {
                continue;
            };
            let family = own
                .iter()
                .find(|(family, _)| *family == section.format.font_id.family)
                .map_or(&[][..], |(_, chars)| chars.as_slice());
            for c in part.chars() {
                let Some(script) = script_of(c) else { continue };
                let i = script.index();
                if self.exhausted[i]
                    || self.wanted[i].len() >= SAMPLE
                    || self.wanted[i].contains(&c)
                    || self.unfixable.contains(&c)
                    || self.covers(c)
                    || family.iter().any(|chars| chars.contains(c))
                {
                    continue;
                }
                self.wanted[i].push(c);
            }
        }
    }
}

impl egui::plugin::Plugin for CjkFonts {
    fn debug_name(&self) -> &'static str {
        "rustpy-cjk-fonts"
    }

    /// Before each pass: takes in a finished lookup and installs changed fonts, which then
    /// apply to this pass already.
    fn input_hook(&mut self, ctx: &Context, _input: &mut RawInput) {
        if let Some(pending) = self.pending.take() {
            match pending.result.try_recv() {
                Ok(outcome) => self.on_outcome(pending.need, &pending.chars, outcome),
                // The lookup panicked.
                Err(TryRecvError::Disconnected) => self.on_failure(&pending.need),
                Err(TryRecvError::Empty) => self.pending = Some(pending),
            }
        }
        if !self.dirty && self.ready && !self.fonts.is_empty() {
            // Something else replaced the font definitions: add the fallbacks again.
            if !ctx.fonts(|f| {
                f.definitions()
                    .font_data
                    .keys()
                    .any(|name| name.starts_with(NAME_PREFIX))
            }) {
                self.dirty = true;
            }
        }
        if self.dirty {
            self.install(ctx);
        }
        self.request_next(ctx);
    }

    /// After each pass: looks at the painted text for characters no font has.
    fn output_hook(&mut self, ctx: &Context, output: &mut FullOutput) {
        self.ready = true;
        self.scan(ctx, &output.shapes);
        self.request_next(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A font egui can parse whose only characters in the CJK blocks are 〰, 〽, ㊗ and ㊙
    /// (the bundled outline Noto Emoji; `fake_font` claims other characters).
    const SYMBOLS: &[u8] = include_bytes!("../../assets/fonts/NotoEmoji-Variable.ttf");

    /// [`vertical_center`] of Yu Gothic (ascent 0.88, descent -0.222, line gap 0.5 em).
    const YU_GOTHIC_CENTER: f32 = 0.079;

    /// A font egui can load ([`SYMBOLS`]' data) claiming `chars`, with Yu Gothic's metrics.
    fn fake_font(file: &str, locale: &str, chars: &str) -> LoadedFont {
        LoadedFont {
            path: PathBuf::from(file),
            index: 0,
            locale: locale.to_owned(),
            data: SYMBOLS,
            center: YU_GOTHIC_CENTER,
            chars: CharSet::from_chars(chars.chars()),
        }
    }

    /// The files of the fallback fonts among font names.
    fn fallback_files(names: &[String]) -> Vec<String> {
        names
            .iter()
            .filter_map(|n| n.strip_prefix(NAME_PREFIX))
            .map(|n| n[..n.rfind('#').unwrap_or(n.len())].to_owned())
            .collect()
    }

    fn new_font(file: &str, locale: &str, chars: &str) -> Outcome {
        Outcome {
            found: Found::New(fake_font(file, locale, chars)),
            inspected: Vec::new(),
        }
    }

    fn nothing() -> Outcome {
        Outcome {
            found: Found::Nothing,
            inspected: Vec::new(),
        }
    }

    /// A system whose `find_fallback_font` gives, for each `(locale, file, characters)`, the
    /// face `(file, 0)` claiming these CJK characters: answers lookups as [`load_font`] does.
    fn system(
        faces: &'static [(&'static str, &'static str, &'static str)],
    ) -> impl Fn(&Request) -> Outcome + Send + Sync + 'static {
        move |r| {
            let mut inspected: Vec<(FaceKey, CharSet)> = Vec::new();
            for locale in &r.locales {
                let Some(&(_, file, chars)) = faces.iter().find(|(l, _, _)| *l == locale.as_str())
                else {
                    continue;
                };
                let key = (PathBuf::from(file), 0);
                if r.installed.contains(&key) {
                    if matches!(r.need, Need::UiLanguage(_)) {
                        return Outcome {
                            found: Found::Installed(key.0, key.1),
                            inspected,
                        };
                    }
                    continue;
                }
                if r.skip.contains(&key) || inspected.iter().any(|(k, _)| *k == key) {
                    continue;
                }
                let has = CharSet::from_chars(chars.chars());
                if r.chars.is_empty() || r.chars.iter().any(|&c| has.contains(c)) {
                    return Outcome {
                        found: Found::New(fake_font(file, locale, chars)),
                        inspected,
                    };
                }
                inspected.push((key, has));
            }
            Outcome {
                found: if faces.is_empty() {
                    Found::NoCjkFonts
                } else {
                    Found::Nothing
                },
                inspected,
            }
        }
    }

    fn needs(calls: &[Request]) -> Vec<Need> {
        calls.iter().map(|c| c.need.clone()).collect()
    }

    /// A font collection (`.ttc`) with `faces` copies of the font `ttf`.
    fn collection(ttf: &[u8], faces: u32) -> Vec<u8> {
        let header = 12 + 4 * faces;
        let mut out = b"ttcf".to_vec();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&faces.to_be_bytes());
        for _ in 0..faces {
            out.extend_from_slice(&header.to_be_bytes());
        }
        // The table records hold offsets from the start of the file: past the header now.
        let mut font = ttf.to_vec();
        let tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
        for record in 0..tables {
            let at = 12 + 16 * record + 8;
            let offset =
                u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]) + header;
            font[at..at + 4].copy_from_slice(&offset.to_be_bytes());
        }
        out.extend_from_slice(&font);
        out
    }

    #[test]
    fn characters_are_sorted_into_scripts() {
        for c in [
            '中',
            '这',
            '說',
            '。',
            '、',
            '（',
            'Ａ',
            '！',
            '㐀',
            '豈',
            '\u{20000}',
            'ㄅ',
            '㈠',
            '㎡',
        ] {
            assert_eq!(script_of(c), Some(Script::Han), "{c:?}");
        }
        for c in ['あ', 'ア', 'ー', 'ｱ', 'ㇰ', '\u{1B001}'] {
            assert_eq!(script_of(c), Some(Script::Kana), "{c:?}");
        }
        for c in ['한', '글', 'ㄱ', 'ᄀ', 'ﾡ'] {
            assert_eq!(script_of(c), Some(Script::Hangul), "{c:?}");
        }
        for c in [
            'a', 'é', 'Ж', '…', '›', '—', '⟳', '🔍', '\u{E000}', '\u{FFFD}', 'ạ',
        ] {
            assert_eq!(script_of(c), None, "{c:?}");
        }
    }

    #[test]
    fn only_text_with_high_lead_bytes_is_scanned() {
        assert!(!may_contain_cjk(""));
        assert!(!may_contain_cjk("def main(): return 42"));
        assert!(!may_contain_cjk("Größe, naïve, Жук"));
        assert!(may_contain_cjk("中"));
        assert!(
            may_contain_cjk(&format!("{}한", "x".repeat(100))),
            "past the first chunk"
        );
        // A conservative test: punctuation such as … passes and is then looked at per character.
        assert!(may_contain_cjk("Open…"));
    }

    #[test]
    fn char_sets() {
        let set = CharSet::from_chars("中文中\u{20000}\u{20000}あ".chars());
        assert_eq!(set.len(), 4);
        for c in ['中', '文', 'あ', '\u{20000}'] {
            assert!(set.contains(c), "{c:?}");
        }
        for c in ['国', 'a', '\u{20001}', '\u{10FFFF}'] {
            assert!(!set.contains(c), "{c:?}");
        }
        assert!(!CharSet::default().contains('中'));
        assert!(CharSet::default().is_empty() && !set.is_empty());
    }

    #[test]
    fn han_follows_the_ui_language_then_the_system() {
        assert_eq!(
            script_locales(Script::Han, Some("ja"), Some("zh_CN.UTF-8")),
            ["ja", "zh-CN", "ko"]
        );
        assert_eq!(
            script_locales(Script::Han, None, Some("ja_JP.UTF-8")),
            ["ja_JP.UTF-8", "zh-CN", "ja", "ko"],
            "Japanese glyphs in an English UI on a Japanese system"
        );
        assert_eq!(
            script_locales(Script::Han, None, Some("zh_TW.UTF-8")),
            ["zh_TW.UTF-8", "zh-CN", "ja", "ko"],
            "Traditional Chinese has no catalog but a font"
        );
        assert_eq!(
            script_locales(Script::Han, None, Some("de_DE.UTF-8")),
            ["zh-CN", "ja", "ko"]
        );
        assert_eq!(
            script_locales(Script::Han, None, None),
            ["zh-CN", "ja", "ko"]
        );
        assert_eq!(script_locales(Script::Hangul, Some("ja"), None), ["ko"]);
        assert_eq!(script_locales(Script::Kana, None, None), ["ja"]);
    }

    #[test]
    fn fallbacks_go_last_in_every_family_and_replace_earlier_ones() {
        let base = crate::theme::font_definitions();
        let (a, b) = (
            Arc::new(fake_font("/a.ttc", "ja", "あ")),
            Arc::new(fake_font("/b.ttf", "ko", "한")),
        );
        let defs = with_fallbacks(base.clone(), &[Arc::clone(&a), Arc::clone(&b)]);
        assert!(
            defs.families.len() >= 4,
            "proportional, monospace and the editor's Hack faces"
        );
        for (family, names) in &defs.families {
            let original = &base.families[family];
            assert_eq!(
                &names[..original.len()],
                &original[..],
                "{family:?}: the IDE's fonts keep their order"
            );
            assert_eq!(
                fallback_files(&names[original.len()..]),
                ["/a.ttc", "/b.ttf"],
                "{family:?}"
            );
            assert!(
                names.iter().all(|n| defs.font_data.contains_key(n)),
                "{family:?}"
            );
        }
        // Installing again (reordered, one font fewer) replaces the earlier fallbacks.
        let again = with_fallbacks(defs, &[Arc::clone(&b)]);
        for (family, names) in &again.families {
            assert_eq!(fallback_files(names), ["/b.ttf"], "{family:?}");
        }
        assert_eq!(
            fallback_files(&again.font_data.keys().cloned().collect::<Vec<_>>())
                .iter()
                .filter(|f| *f == "/a.ttc")
                .count(),
            0
        );
    }

    #[test]
    fn fallbacks_sit_on_the_baseline_of_each_familys_first_font() {
        // Hack: ascent 1901, descent -483, no line gap (2048 units per em).
        let hack = vertical_center(crate::theme::hack_regular(), 0).unwrap();
        assert!((hack - 0.346).abs() < 1e-3);
        let yu = Arc::new(fake_font("/fonts/YuGothR.ttc", "ja", "あ"));
        let defs = with_fallbacks(crate::theme::font_definitions(), &[yu]);
        for (family, names) in &defs.families {
            let first = drawn_center(&defs.font_data[&names[0]]).unwrap();
            let fallback = &defs.font_data[names.last().unwrap()];
            let expected = first - YU_GOTHIC_CENTER;
            assert!(
                (fallback.tweak.y_offset_factor - expected).abs() <= 0.005,
                "{family:?}: {} vs {expected}",
                fallback.tweak.y_offset_factor
            );
            assert!(
                fallback.tweak.y_offset_factor > 0.25,
                "{family:?}: Yu Gothic moves down by about 0.28 em"
            );
            assert!(
                matches!(fallback.font, std::borrow::Cow::Borrowed(_)),
                "static data, which egui does not copy"
            );
        }
        let entries = defs
            .font_data
            .keys()
            .filter(|k| k.starts_with(NAME_PREFIX))
            .count();
        assert!(
            entries >= 1 && entries < defs.families.len(),
            "families with similar first fonts share an entry: {entries}"
        );
    }

    #[test]
    fn fonts_are_read_with_their_cjk_characters() {
        let (chars, center) = LoadedFont::inspect(SYMBOLS, 0).unwrap();
        // Noto Emoji's only characters in the CJK blocks are four symbols; ascent 1900,
        // descent -500, no line gap (2048 units per em).
        assert_eq!(chars.len(), 4);
        assert!(chars.contains('〽') && chars.contains('㊙') && !chars.contains('中'));
        assert!((center - 0.342).abs() < 1e-3);
        assert!(
            LoadedFont::inspect(b"not a font", 0).is_err(),
            "egui would panic on it"
        );
        assert!(LoadedFont::inspect(SYMBOLS, 3).is_err(), "no such face");
        let font = LoadedFont::new(
            PathBuf::from("/fonts/symbols.ttf"),
            0,
            "ja",
            SYMBOLS,
            chars,
            center,
        );
        assert_eq!(
            (font.key(), font.data.len()),
            ((PathBuf::from("/fonts/symbols.ttf"), 0), SYMBOLS.len())
        );
    }

    /// An egui context with the plugin, whose lookups `respond` answers (and `calls` records).
    struct Harness {
        ctx: Context,
        calls: Arc<Mutex<Vec<Request>>>,
    }

    impl Harness {
        fn new(respond: impl Fn(&Request) -> Outcome + Send + Sync + 'static) -> Self {
            Self::with_system(None, respond)
        }

        /// `system_tag`: the environment's message locale.
        fn with_system(
            system_tag: Option<&str>,
            respond: impl Fn(&Request) -> Outcome + Send + Sync + 'static,
        ) -> Self {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&calls);
            let loader: Loader = Arc::new(move |request: &Request| {
                log.lock().unwrap().push(request.clone());
                respond(request)
            });
            let ctx = Context::default();
            ctx.add_plugin(CjkFonts::new(loader, system_tag.map(str::to_owned)));
            Self { ctx, calls }
        }

        /// Runs a frame showing `text`.
        fn frame(&self, text: &str) {
            self.frame_in(text, &egui::FontId::proportional(13.0));
        }

        /// Runs a frame showing `text` in the font `font_id`.
        fn frame_in(&self, text: &str, font_id: &egui::FontId) {
            let mut out = self.ctx.run_ui(RawInput::default(), |ui| {
                ui.label(egui::RichText::new(text).font(font_id.clone()));
            });
            out.textures_delta.clear();
        }

        /// Runs frames showing `text` until no lookup is running (at most 5 s).
        fn settle(&self, text: &str) {
            self.settle_in(text, &egui::FontId::proportional(13.0));
        }

        /// [`Harness::settle`] in the font `font_id`.
        fn settle_in(&self, text: &str, font_id: &egui::FontId) {
            for _ in 0..500 {
                self.frame_in(text, font_id);
                if self.with(|p| p.pending.is_none()) {
                    self.frame_in(text, font_id);
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the font lookup did not finish");
        }

        fn with<R>(&self, f: impl FnOnce(&mut CjkFonts) -> R) -> R {
            self.ctx
                .with_plugin::<CjkFonts, _>(f)
                .expect("the plugin is registered")
        }

        fn calls(&self) -> Vec<Request> {
            self.calls.lock().unwrap().clone()
        }

        /// The fallbacks egui uses, by file, as every family lists them.
        fn active_fallbacks(&self) -> Vec<String> {
            self.ctx.fonts(|f| {
                let lists: Vec<Vec<String>> = f
                    .definitions()
                    .families
                    .values()
                    .map(|names| fallback_files(names))
                    .collect();
                assert!(
                    lists.windows(2).all(|w| w[0] == w[1]),
                    "every family has the same fallbacks: {lists:?}"
                );
                lists.into_iter().next().unwrap_or_default()
            })
        }
    }

    #[test]
    fn cjk_text_loads_a_font_for_its_script_once() {
        let h = Harness::new(|_| new_font("/fonts/kr.ttf", "ko", "한글"));
        h.frame("Open… — 42 Größe");
        h.frame("Open… — 42 Größe");
        assert!(h.calls().is_empty(), "no CJK text, no lookup");
        h.settle("한글");
        let calls = h.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].need, Need::Script(Script::Hangul));
        assert_eq!(calls[0].locales, ["ko"]);
        assert_eq!(calls[0].chars, ['한', '글']);
        assert_eq!(h.active_fallbacks(), ["/fonts/kr.ttf"]);
        // The font has these characters now: nothing more to look up.
        h.frame("한글 글");
        h.frame("ok");
        assert_eq!(h.calls().len(), 1);
    }

    #[test]
    fn scripts_are_looked_up_in_order_and_faces_are_read_again_only_for_characters_they_have() {
        // A Japanese face with kana and some Han characters, a Chinese one with other Han characters.
        static FACES: [(&str, &str, &str); 2] = [
            ("ja", "/fonts/jp.ttc", "日本語のテキスト"),
            ("zh-CN", "/fonts/sc.ttc", "们国"),
        ];
        let h = Harness::new(system(&FACES));
        h.settle("日本語のテキスト：这个");
        let calls = h.calls();
        assert_eq!(
            needs(&calls),
            [Need::Script(Script::Kana), Need::Script(Script::Han)]
        );
        assert_eq!(
            calls[1].chars,
            ['：', '这', '个'],
            "only what the Japanese font lacks"
        );
        assert_eq!(calls[1].installed, [(PathBuf::from("/fonts/jp.ttc"), 0)]);
        assert!(calls[1].skip.is_empty());
        assert_eq!(
            h.active_fallbacks(),
            ["/fonts/jp.ttc"],
            "the Chinese face has none of them"
        );
        // No face has these: not looked up again, and Han text is still looked up.
        h.frame("这个：");
        h.frame("这个");
        assert_eq!(h.calls().len(), 2);
        assert!(!h.with(|p| p.exhausted[Script::Han.index()]));
        // The Chinese face is known to lack 书: not read for it.
        h.settle("书");
        let calls = h.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            (calls[2].chars.clone(), calls[2].skip.clone()),
            (vec!['书'], vec![(PathBuf::from("/fonts/sc.ttc"), 0)])
        );
        // It has 们: read again for it.
        h.settle("们");
        let calls = h.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            (calls[3].chars.clone(), calls[3].skip.clone()),
            (vec!['们'], vec![])
        );
        assert_eq!(h.active_fallbacks(), ["/fonts/jp.ttc", "/fonts/sc.ttc"]);
        assert!(
            h.with(|p| p.known.is_empty()),
            "the installed face is no longer listed as known"
        );
        h.frame("们国 这个 书");
        assert_eq!(h.calls().len(), 4);
    }

    #[test]
    fn a_character_no_font_has_does_not_stop_the_lookups_for_its_script() {
        // U+2A6D6 is in none of the faces. Before, the lookup for it gave up Han text and the
        // faces read for good, so Chinese text pasted afterwards stayed boxes until a restart.
        static FACES: [(&str, &str, &str); 3] = [
            ("zh-CN", "/fonts/msyh.ttc", "中文你好这是我们设置"),
            ("ja", "/fonts/YuGothR.ttc", "日本語中文"),
            ("ko", "/fonts/malgun.ttf", "한글"),
        ];
        let h = Harness::new(system(&FACES));
        h.settle("s = \"\u{2A6D6}\"");
        let calls = h.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].chars, ['\u{2A6D6}']);
        assert!(h.active_fallbacks().is_empty(), "no face has it");
        assert!(!h.with(|p| p.exhausted.iter().any(|&e| e)));
        h.frame("s = \"\u{2A6D6}\" # again");
        assert_eq!(
            h.calls().len(),
            1,
            "a character no face has is not looked up again"
        );
        let pasted = "s = \"中文 你好 这是 我们 设置\"  # \u{2A6D6}";
        h.settle(pasted);
        let calls = h.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(
            calls[1].chars,
            ['中', '文', '你', '好', '这', '是', '我', '们']
        );
        assert_eq!(
            calls[1].skip,
            [(PathBuf::from("/fonts/malgun.ttf"), 0)],
            "only the Korean face is known to have none of them"
        );
        assert_eq!(h.active_fallbacks(), ["/fonts/msyh.ttc"]);
        // 设 and 置 were beyond the sample: the Chinese face has them, so nothing is left to look up.
        h.frame(pasted);
        h.frame(pasted);
        assert_eq!(h.calls().len(), 2);
        assert!(h.with(|p| p.wanted.iter().all(Vec::is_empty)));
    }

    #[test]
    fn characters_beyond_the_sample_of_text_painted_unchanged_are_looked_up() {
        // Nine characters no face has, then one the Chinese face has, in one label painted unchanged.
        static FACES: [(&str, &str, &str); 1] = [("zh-CN", "/fonts/sc.ttc", "中")];
        let h = Harness::new(system(&FACES));
        let text =
            "\u{2A6D6}\u{2A6D7}\u{2A6D8}\u{2A6D9}\u{2A6DA}\u{2A6DB}\u{2A6DC}\u{2A6DD}\u{2A6DE}中";
        h.settle(text);
        h.settle(text);
        h.settle(text);
        let calls = h.calls();
        // The ninth is looked up with 中, and once more after the face found for 中 lacks it.
        let chars: Vec<String> = calls.iter().map(|c| c.chars.iter().collect()).collect();
        assert_eq!(
            chars,
            [&text[..32], "\u{2A6DE}中", "\u{2A6DE}"],
            "{calls:?}"
        );
        assert_eq!(h.active_fallbacks(), ["/fonts/sc.ttc"]);
        assert!(h.with(|p| p.unfixable.contains(&'\u{2A6DE}') && p.unfixable.len() == 9));
    }

    #[test]
    fn characters_the_ides_own_fonts_draw_need_no_cjk_font() {
        static FACES: [(&str, &str, &str); 2] = [
            ("zh-CN", "/fonts/msyh.ttc", "中文这们设"),
            ("ja", "/fonts/YuGothR.ttc", "日本語中文㊙〽"),
        ];
        let h = Harness::new(system(&FACES));
        crate::theme::install_fonts(&h.ctx);
        h.frame("x");
        let defs = crate::theme::font_definitions();
        let has = |font: &str, c: char| {
            cjk_chars(&defs.font_data[font].font, 0)
                .unwrap()
                .contains(c)
        };
        assert!(
            has("NotoEmoji-Regular", '㊙')
                && has("NotoEmoji-Regular", '〽')
                && has("emoji-icon-font", '（')
        );
        for (family, names) in &defs.families {
            for symbol in ['㊙', '〽'] {
                assert!(
                    names.iter().any(|name| {
                        drawn_cjk_chars(name, &defs.font_data[name]).contains(symbol)
                    }),
                    "{family:?} must draw {symbol} with its own fallback chain"
                );
            }
        }
        let editor = egui::FontId::new(13.0, crate::theme::mono_family(false, false));
        // As in the review: a Python file with an emoji that is a CJK symbol.
        let source = "SECRET = \"㊙\u{FE0F}\"  # 〽";
        h.settle(source);
        h.settle_in(source, &editor);
        h.settle_in("parens = \"（）\"", &editor);
        assert!(h.calls().is_empty(), "no lookup: {:?}", h.calls());
        // So Chinese text gets the Chinese face rather than a Japanese one loaded for ㊙.
        h.settle_in("这们设", &editor);
        assert_eq!(needs(&h.calls()), [Need::Script(Script::Han)]);
        assert_eq!(h.active_fallbacks(), ["/fonts/msyh.ttc"]);
    }

    #[test]
    fn egui_leaves_some_characters_its_own_fonts_map_undrawn() {
        // What the plugin mirrors: egui draws ㊙ with NotoEmoji, but draws 卍 and 卐 as its
        // replacement glyph ◻ although emoji-icon-font, in every family, maps them.
        let mut defs = crate::theme::font_definitions();
        let icons = &defs.font_data["emoji-icon-font"];
        assert!(cjk_chars(&icons.font, icons.index).unwrap().contains('卍'));
        assert!(
            !drawn_cjk_chars("emoji-icon-font", icons).contains('卍')
                && !drawn_cjk_chars("emoji-icon-font", icons).contains('卐')
        );
        assert!(drawn_cjk_chars("emoji-icon-font", icons).contains('！'));
        assert!(
            drawn_cjk_chars("my-font", icons).contains('卍'),
            "only egui's own fonts are affected"
        );
        // Compare glyph identity independently of its fractional x position.
        // With subpixel binning one replacement glyph can occupy several
        // atlas rectangles when a fallback font has fractional advances.
        for data in defs.font_data.values_mut() {
            Arc::make_mut(data).tweak.subpixel_binning = Some(false);
        }
        let ctx = Context::default();
        ctx.set_fonts(defs);
        let mut out = ctx.run_ui(RawInput::default(), |_| {});
        out.textures_delta.clear();
        for family in [
            FontFamily::Proportional,
            crate::theme::mono_family(false, false),
        ] {
            let galley = ctx.fonts_mut(|f| {
                f.layout_no_wrap(
                    "卍卐㊙◻".into(),
                    egui::FontId::new(13.0, family.clone()),
                    egui::Color32::WHITE,
                )
            });
            let uv: Vec<_> = galley.rows[0].glyphs.iter().map(|g| g.uv_rect).collect();
            assert_eq!(uv.len(), 4);
            assert!(
                uv[0] == uv[3] && uv[1] == uv[3],
                "{family:?}: 卍 and 卐 are drawn as ◻"
            );
            assert!(uv[2] != uv[3], "{family:?}: ㊙ has a glyph");
        }
    }

    #[test]
    fn characters_egui_does_not_draw_with_its_own_fonts_are_looked_up() {
        static FACES: [(&str, &str, &str); 1] = [("zh-CN", "/fonts/msyh.ttc", "中文卍")];
        let h = Harness::new(system(&FACES));
        crate::theme::install_fonts(&h.ctx);
        h.frame("x");
        // A file with only 卍: emoji-icon-font maps it, but egui draws a box for it.
        h.settle_in(
            "MAP = \"卍\"",
            &egui::FontId::new(13.0, crate::theme::mono_family(false, false)),
        );
        let calls = h.calls();
        assert_eq!(needs(&calls), [Need::Script(Script::Han)]);
        assert_eq!(calls[0].chars, ['卍']);
        assert_eq!(h.active_fallbacks(), ["/fonts/msyh.ttc"]);
    }

    #[test]
    fn own_fonts_are_read_again_when_their_name_gets_other_data() {
        let h = Harness::new(|_| nothing());
        let family = FontFamily::Name("custom".into());
        let mut defs = FontDefinitions::default();
        let hack = Arc::clone(&defs.font_data["Hack"]);
        let mut set = |data: Arc<FontData>| {
            defs.font_data.insert("custom".into(), data);
            defs.families.insert(family.clone(), vec!["custom".into()]);
            h.ctx.set_fonts(defs.clone());
            h.frame("x");
        };
        set(Arc::new(FontData::from_static(SYMBOLS)));
        let own = h.with(|p| p.own_fonts(&h.ctx, vec![family.clone()]));
        assert!(own[0].1[0].contains('㊙'), "Noto Emoji has ㊙");
        set(hack);
        let own = h.with(|p| p.own_fonts(&h.ctx, vec![family.clone()]));
        assert!(own[0].1[0].is_empty(), "Hack has no CJK characters");
    }

    #[test]
    fn faces_of_a_collection_share_one_copy_of_the_file() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        fn read(path: &Path) -> std::io::Result<Vec<u8>> {
            READS.fetch_add(1, Ordering::SeqCst);
            assert_eq!(path, Path::new("/fonts/NotoSansCJK-Regular.ttc"));
            Ok(collection(SYMBOLS, 3))
        }
        let files = FontFiles::new(read);
        // Noto Sans CJK on Linux: one file with a face per language.
        let find = |locale: &str| {
            Some((
                PathBuf::from("/fonts/NotoSansCJK-Regular.ttc"),
                match locale {
                    "ja" => 0,
                    "ko" => 1,
                    _ => 2,
                },
            ))
        };
        let mut installed: Vec<Arc<LoadedFont>> = Vec::new();
        for locale in ["ja", "ko", "zh-CN"] {
            let request = Request {
                need: Need::UiLanguage(locale.into()),
                locales: vec![locale.into()],
                installed: installed.iter().map(|f| f.key()).collect(),
                skip: Vec::new(),
                chars: Vec::new(),
            };
            let Found::New(font) = load_font(&request, &find, &files).found else {
                panic!("no face for {locale}")
            };
            installed.push(Arc::new(font));
        }
        assert_eq!(
            installed.iter().map(|f| f.index).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(READS.load(Ordering::SeqCst), 1, "the file is read once");
        assert!(
            installed
                .windows(2)
                .all(|w| std::ptr::eq(w[0].data, w[1].data)),
            "and kept once"
        );
        // egui reads every face from the shared data.
        let ctx = Context::default();
        ctx.set_fonts(with_fallbacks(crate::theme::font_definitions(), &installed));
        let mut out = ctx.run_ui(RawInput::default(), |ui| {
            ui.label("｛ok｝");
        });
        out.textures_delta.clear();
        ctx.fonts(|f| {
            for names in f.definitions().families.values() {
                assert_eq!(fallback_files(names), ["/fonts/NotoSansCJK-Regular.ttc"; 3]);
            }
        });
    }

    #[test]
    fn lookups_report_the_faces_they_read_and_skip_the_known_ones() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        fn read(path: &Path) -> std::io::Result<Vec<u8>> {
            READS.fetch_add(1, Ordering::SeqCst);
            match path.to_str() {
                Some("/fonts/symbols.ttf") => Ok(SYMBOLS.to_vec()),
                Some("/fonts/broken.ttf") => Ok(b"not a font".to_vec()),
                _ => Err(std::io::ErrorKind::NotFound.into()),
            }
        }
        let files = FontFiles::new(read);
        let find = |locale: &str| match locale {
            "zh-CN" => Some((PathBuf::from("/fonts/symbols.ttf"), 0)),
            "ja" => Some((PathBuf::from("/fonts/broken.ttf"), 0)),
            _ => None,
        };
        let han = |chars: &str, skip: Vec<FaceKey>| Request {
            need: Need::Script(Script::Han),
            locales: vec!["zh-CN".into(), "ja".into(), "ko".into()],
            installed: Vec::new(),
            skip,
            chars: chars.chars().collect(),
        };
        // Noto Emoji's only CJK characters are 〰, 〽, ㊗ and ㊙; the other face is no font.
        let outcome = load_font(&han("中", Vec::new()), &find, &files);
        assert!(matches!(outcome.found, Found::Nothing));
        let read_faces: Vec<(&str, usize)> = outcome
            .inspected
            .iter()
            .map(|(key, has)| (key.0.to_str().unwrap(), has.len()))
            .collect();
        assert_eq!(
            read_faces,
            [("/fonts/symbols.ttf", 4), ("/fonts/broken.ttf", 0)]
        );
        assert_eq!(READS.load(Ordering::SeqCst), 2);
        // Skipped faces are not read.
        let skip: Vec<FaceKey> = outcome.inspected.into_iter().map(|(key, _)| key).collect();
        let outcome = load_font(&han("中", skip), &find, &files);
        assert!(matches!(outcome.found, Found::Nothing) && outcome.inspected.is_empty());
        assert_eq!(READS.load(Ordering::SeqCst), 2);
        // A face with one of the characters is taken.
        let Found::New(font) = load_font(&han("中㊙", Vec::new()), &find, &files).found else {
            panic!("Noto Emoji has ㊙")
        };
        assert_eq!(font.key(), (PathBuf::from("/fonts/symbols.ttf"), 0));
        let none = |_: &str| None;
        assert!(matches!(
            load_font(&han("中", Vec::new()), &none, &files).found,
            Found::NoCjkFonts
        ));
    }

    #[test]
    fn the_ui_language_font_comes_first_and_is_reused() {
        let h = Harness::new(|r| match &r.need {
            Need::UiLanguage(locale) => {
                new_font(&format!("/fonts/{locale}.ttc"), locale, "日本語中文")
            }
            Need::Script(Script::Hangul) => new_font("/fonts/kr.ttf", "ko", "한글"),
            _ => nothing(),
        });
        // Before the first frame, as at startup: looked up right away.
        let ctx = h.ctx.clone();
        h.with(|p| p.set_ui_locale(&ctx, Some("ja"), true));
        assert_eq!(h.calls().len(), 1);
        assert!(h.with(|p| p.dirty));
        h.frame("日本語");
        assert_eq!(
            h.active_fallbacks(),
            ["/fonts/ja.ttc"],
            "the first frame has the font"
        );
        h.settle("한글");
        assert_eq!(h.active_fallbacks(), ["/fonts/ja.ttc", "/fonts/kr.ttf"]);
        // Korean UI: its font is installed already and moves to the front.
        h.with(|p| p.set_ui_locale(&ctx, Some("ko"), false));
        h.frame("x");
        assert_eq!(h.active_fallbacks(), ["/fonts/kr.ttf", "/fonts/ja.ttc"]);
        assert_eq!(h.calls().len(), 2);
        // Chinese UI: looked up in the background, then first.
        h.with(|p| p.set_ui_locale(&ctx, Some("zh-CN"), false));
        h.settle("x");
        assert_eq!(
            h.active_fallbacks(),
            ["/fonts/zh-CN.ttc", "/fonts/kr.ttf", "/fonts/ja.ttc"]
        );
        assert_eq!(
            h.calls().last().map(|c| c.need.clone()),
            Some(Need::UiLanguage("zh-CN".into()))
        );
        // English UI: the fonts stay for CJK text in files.
        h.with(|p| p.set_ui_locale(&ctx, None, false));
        h.frame("x");
        assert_eq!(h.active_fallbacks().len(), 3);
        assert!(!h.with(|p| p.ui_font_missing()));
    }

    #[test]
    fn a_cjk_system_locale_decides_the_glyph_forms_in_an_english_ui() {
        let h = Harness::with_system(Some("zh_CN.UTF-8"), |r| match &r.need {
            Need::Script(Script::Hangul) => new_font("/fonts/kr.ttf", "ko", "한글中"),
            Need::Script(Script::Han) => new_font("/fonts/sc.ttc", &r.locales[0], "中文这"),
            _ => nothing(),
        });
        h.settle("한글 中文这");
        let calls = h.calls();
        assert_eq!(
            calls.iter().map(|c| c.need.clone()).collect::<Vec<_>>(),
            [Need::Script(Script::Hangul), Need::Script(Script::Han)]
        );
        assert_eq!(calls[1].locales, ["zh_CN.UTF-8", "zh-CN", "ja", "ko"]);
        assert_eq!(calls[1].chars, ['文', '这'], "中 is in the Korean font");
        // Looked up second, but the system's glyph forms come first.
        assert_eq!(h.active_fallbacks(), ["/fonts/sc.ttc", "/fonts/kr.ttf"]);
    }

    #[test]
    fn a_failed_lookup_is_not_repeated() {
        let h = Harness::new(|_| panic!("simulated font lookup failure"));
        h.settle("한글");
        h.frame("한글 글");
        h.frame("한글 글");
        assert_eq!(
            h.calls().len(),
            1,
            "a lookup that panicked counts as nothing found"
        );
        assert!(h.with(|p| p.exhausted[Script::Hangul.index()]));
    }

    #[test]
    fn without_cjk_fonts_nothing_more_is_looked_up() {
        let h = Harness::new(|_| Outcome {
            found: Found::NoCjkFonts,
            inspected: Vec::new(),
        });
        let ctx = h.ctx.clone();
        h.with(|p| p.set_ui_locale(&ctx, Some("zh-CN"), true));
        assert!(h.with(|p| p.ui_font_missing()), "the Settings page warns");
        h.frame("中文 한글 かな");
        h.frame("中文 한글 かな");
        assert_eq!(h.calls().len(), 1);
        assert!(h.active_fallbacks().is_empty());
    }

    #[test]
    fn replaced_font_definitions_get_the_fallbacks_back() {
        let h = Harness::new(|_| new_font("/fonts/sc.ttc", "zh-CN", "中文"));
        h.settle("中文");
        assert_eq!(h.active_fallbacks(), ["/fonts/sc.ttc"]);
        h.ctx.set_fonts(FontDefinitions::default());
        h.frame("中文");
        assert!(
            h.active_fallbacks().is_empty(),
            "egui applied the replacement"
        );
        h.frame("中文");
        assert_eq!(h.active_fallbacks(), ["/fonts/sc.ttc"]);
        assert_eq!(h.calls().len(), 1, "no new lookup");
    }
}
