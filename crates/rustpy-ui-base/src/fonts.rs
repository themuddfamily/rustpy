//! Fonts (roadmap §1.6; Settings › Editor › Font and Appearance › UI font):
//! the editor and UI font families come from the installed fonts, and
//! fallback fonts cover the scripts the bundled ones lack (Chinese, Japanese,
//! Korean, Arabic, Hebrew, Thai, Devanagari, …), so such text in comments,
//! strings, file names and the terminal is not drawn as boxes. With *Enable
//! ligatures* off, the editor font's ligatures are switched off.
//!
//! The installed fonts are scanned on a background thread (`fontdb`). The
//! result is kept in `<config>/font_cache.json`, so the next start shows the
//! chosen fonts at once instead of after the scan.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

use egui::{FontData, FontDefinitions, FontFamily};
use serde::{Deserialize, Serialize};

use crate::theme::{self, MONO_REGULAR};

/// The editor font that is always there: the bundled Hack.
pub const BUNDLED_EDITOR_FONT: &str = "Hack";

const BUNDLED_EMOJI: &str = "NotoEmoji-Outline";

/// Adds outline emoji usable by egui's glyph rasterizer, even before the
/// installed-font scan. Bitmap-only system emoji fonts cannot be drawn here.
pub fn install_emoji_fallback(defs: &mut FontDefinitions) {
    defs.font_data.insert(
        BUNDLED_EMOJI.into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/NotoEmoji-Variable.ttf"
        ))),
    );
    for fonts in defs.families.values_mut() {
        fonts.retain(|name| name != BUNDLED_EMOJI);
        let at = fonts
            .iter()
            .position(|name| name == "NotoEmoji-Regular" || name == "emoji-icon-font")
            .unwrap_or(fonts.len());
        fonts.insert(at, BUNDLED_EMOJI.into());
    }
}

/// One face of an installed font family.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Face {
    pub path: PathBuf,
    /// Index of the face in a font collection (`.ttc`).
    pub index: u32,
    pub weight: u16,
    pub italic: bool,
}

/// An installed font family.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Family {
    pub name: String,
    pub monospace: bool,
    pub faces: Vec<Face>,
}

impl Family {
    /// The face nearest to regular (400) or bold (700) weight in the wanted
    /// style; a face of the other style only when the family has none.
    pub fn face(&self, bold: bool, italic: bool) -> Option<&Face> {
        let target: i32 = if bold { 700 } else { 400 };
        self.faces.iter().min_by_key(|f| {
            (
                f.italic != italic,
                (i32::from(f.weight) - target).unsigned_abs(),
                f.weight,
            )
        })
    }
}

/// Fallback families by script, in order of preference: the first installed
/// family of each group that has the group's sample character is used.
pub const FALLBACKS: &[(&str, Option<char>, &[&str])] = &[
    (
        "Chinese, Japanese, Korean",
        Some('中'),
        &[
            "Noto Sans CJK SC",
            "Noto Sans CJK JP",
            "Noto Sans CJK KR",
            "Noto Sans CJK TC",
            "Noto Sans SC",
            "Noto Sans JP",
            "Source Han Sans SC",
            "Source Han Sans",
            "Microsoft YaHei UI",
            "Microsoft YaHei",
            "Yu Gothic UI",
            "Yu Gothic",
            "Meiryo",
            "Malgun Gothic",
            "PingFang SC",
            "Hiragino Sans GB",
            "Hiragino Sans",
            "Apple SD Gothic Neo",
            "WenQuanYi Zen Hei",
            "WenQuanYi Micro Hei",
            "Droid Sans Fallback",
            "IPAGothic",
            "IPAexGothic",
            "MS Gothic",
            "SimSun",
            "Arial Unicode MS",
        ],
    ),
    (
        "Arabic",
        Some('ع'),
        &[
            "Noto Sans Arabic",
            "Noto Naskh Arabic",
            "Segoe UI",
            "Geeza Pro",
            "DejaVu Sans",
            "Tahoma",
            "Arial",
            "FreeSerif",
        ],
    ),
    (
        "Hebrew",
        Some('א'),
        &[
            "Noto Sans Hebrew",
            "Segoe UI",
            "Arial Hebrew",
            "DejaVu Sans",
            "Arial",
            "Tahoma",
            "FreeSans",
        ],
    ),
    (
        "Thai",
        Some('ก'),
        &[
            "Noto Sans Thai",
            "Leelawadee UI",
            "Thonburi",
            "Tahoma",
            "Loma",
            "Garuda",
            "Norasi",
            "FreeSerif",
        ],
    ),
    (
        "Devanagari",
        Some('क'),
        &[
            "Noto Sans Devanagari",
            "Nirmala UI",
            "Kohinoor Devanagari",
            "Mangal",
            "Lohit Devanagari",
            "FreeSans",
        ],
    ),
    (
        "Other scripts and symbols",
        None,
        &[
            "Noto Sans",
            "DejaVu Sans",
            "Segoe UI Symbol",
            "Segoe UI",
            "Arial Unicode MS",
            "FreeSans",
            "Symbola",
        ],
    ),
];

/// The installed font families (sorted by name) and the fallbacks chosen among them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemFonts {
    pub families: Vec<Family>,
    /// `(family, face)` for the scripts of [`FALLBACKS`], each family once.
    pub fallbacks: Vec<(String, Face)>,
}

/// Monospaced families whose files do not say so (FreeMono).
fn looks_monospaced(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "mono",
        "code",
        "courier",
        "consol",
        "terminal",
        "fixed",
        "typewriter",
    ]
    .iter()
    .any(|w| n.contains(w))
}

impl SystemFonts {
    /// Scans the installed fonts and picks the fallbacks (slow: run it on a background thread).
    pub fn scan() -> SystemFonts {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut ids: HashMap<(PathBuf, u32), fontdb::ID> = HashMap::new();
        let faces: Vec<(String, bool, Face)> = db
            .faces()
            .filter_map(|f| {
                let path = match &f.source {
                    fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => p.clone(),
                    fontdb::Source::Binary(_) => return None,
                };
                let name = f.families.first()?.0.clone();
                ids.insert((path.clone(), f.index), f.id);
                Some((
                    name,
                    f.monospaced,
                    Face {
                        path,
                        index: f.index,
                        weight: f.weight.0,
                        italic: f.style != fontdb::Style::Normal,
                    },
                ))
            })
            .collect();
        let mut fonts = Self::from_faces(faces);
        // Coverage is read through fontdb's memory map: no candidate file is read whole.
        fonts.fallbacks = fonts.pick_fallbacks(|face, c| {
            ids.get(&(face.path.clone(), face.index))
                .and_then(|id| db.with_face_data(*id, |data, index| has_char(data, index, c)))
                .unwrap_or(false)
        });
        fonts
    }

    /// Groups faces into families (names compare without case; the first
    /// spelling wins). Hidden system families (`.SF NS`) are left out.
    pub fn from_faces(faces: impl IntoIterator<Item = (String, bool, Face)>) -> SystemFonts {
        let mut by_name: BTreeMap<String, Family> = BTreeMap::new();
        for (name, mono, face) in faces {
            let name = name.trim().to_string();
            if name.is_empty() || name.starts_with('.') {
                continue;
            }
            let family = by_name
                .entry(name.to_lowercase())
                .or_insert_with(|| Family {
                    name: name.clone(),
                    monospace: false,
                    faces: Vec::new(),
                });
            family.monospace |= mono || looks_monospaced(&name);
            if !family
                .faces
                .iter()
                .any(|f| f.weight == face.weight && f.italic == face.italic)
            {
                family.faces.push(face);
            }
        }
        SystemFonts {
            families: by_name.into_values().collect(),
            fallbacks: Vec::new(),
        }
    }

    /// The family called `name` (any case).
    pub fn family(&self, name: &str) -> Option<&Family> {
        let name = name.trim().to_lowercase();
        self.families.iter().find(|f| f.name.to_lowercase() == name)
    }

    /// Family names for a font chooser: only monospaced ones with `monospace_only`.
    /// Emoji fonts (color bitmaps, no letters) are left out.
    pub fn names(&self, monospace_only: bool) -> Vec<String> {
        self.families
            .iter()
            .filter(|f| {
                (!monospace_only || f.monospace) && !f.name.to_ascii_lowercase().contains("emoji")
            })
            .map(|f| f.name.clone())
            .collect()
    }

    /// For each group of [`FALLBACKS`], the regular face of the first
    /// installed family that `covers` the group's sample character.
    pub fn pick_fallbacks(&self, covers: impl Fn(&Face, char) -> bool) -> Vec<(String, Face)> {
        let mut out: Vec<(String, Face)> = Vec::new();
        for (_, sample, candidates) in FALLBACKS {
            let found = candidates
                .iter()
                .filter_map(|c| self.family(c))
                .find_map(|family| {
                    let face = family.face(false, false)?;
                    sample
                        .is_none_or(|c| covers(face, c))
                        .then(|| (family.name.clone(), face.clone()))
                });
            if let Some((name, face)) = found {
                if !out.iter().any(|(n, _)| *n == name) {
                    out.push((name, face));
                }
            }
        }
        out
    }
}

/// Whether face `index` of `font` maps `c` to a glyph.
fn has_char(font: &[u8], index: u32, c: char) -> bool {
    use skrifa::MetadataProvider as _;
    skrifa::FontRef::from_index(font, index).is_ok_and(|f| f.charmap().map(c).is_some())
}

// ----- ligatures -------------------------------------------------------------

/// GSUB features that form ligatures. Required ligatures (`rlig`, which
/// Arabic needs) are not among them.
const LIGATURE_FEATURES: [&[u8; 4]; 4] = [b"liga", b"clig", b"dlig", b"calt"];

/// Byte offsets of the feature tags in the GSUB feature list of face `index`.
fn gsub_feature_tags(font: &[u8], index: u32) -> Option<Vec<usize>> {
    let u16_at = |o: usize| {
        font.get(o..o + 2)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let u32_at = |o: usize| {
        font.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let dir = if font.get(0..4)? == b"ttcf" {
        if index as usize >= u32_at(8)? {
            return None;
        }
        u32_at(12 + 4 * index as usize)?
    } else {
        0
    };
    let tables = u16_at(dir + 4)?;
    let record = (0..tables)
        .map(|i| dir + 12 + 16 * i)
        .find(|&r| font.get(r..r + 4) == Some(&b"GSUB"[..]))?;
    let gsub = u32_at(record + 8)?;
    let list = gsub + u16_at(gsub + 6)?;
    let count = u16_at(list)?;
    let tags: Vec<usize> = (0..count).map(|i| list + 2 + 6 * i).collect();
    (tags.last().is_none_or(|&t| t + 6 <= font.len())).then_some(tags)
}

/// The tags of the GSUB features of face `index` (for tests and diagnostics).
pub fn gsub_features(font: &[u8], index: u32) -> Vec<String> {
    gsub_feature_tags(font, index)
        .unwrap_or_default()
        .into_iter()
        .map(|t| String::from_utf8_lossy(&font[t..t + 4]).into_owned())
        .collect()
}

/// The font with its ligature features switched off: their tags in the GSUB
/// feature list are renamed (the last letter goes one down, `calt` → `cals`,
/// which keeps the list sorted), so the shaper does not apply them. `None`
/// when the face has no ligature features or cannot be read.
pub fn without_ligatures(font: &[u8], index: u32) -> Option<Vec<u8>> {
    let mut out: Option<Vec<u8>> = None;
    for at in gsub_feature_tags(font, index)? {
        if LIGATURE_FEATURES.iter().any(|t| font[at..at + 4] == t[..]) {
            out.get_or_insert_with(|| font.to_vec())[at + 3] -= 1;
        }
    }
    out
}

// ----- loading ---------------------------------------------------------------

/// Font files (and their variants without ligatures) read in this session.
/// egui keeps font data as `'static`: each file is read once and kept, however
/// often the fonts change, so the memory is bounded by the fonts used.
static LOADED: Mutex<Option<HashMap<String, &'static [u8]>>> = Mutex::new(None);

/// Font files at least this large are mapped rather than read: a CJK
/// fallback is 15–25 MB, of which only the tables and glyphs drawn are ever
/// paged in (and those pages stay reclaimable by the system).
const MAP_FONT_BYTES: u64 = 1024 * 1024;

/// The content of a font file or a variant made from one.
enum FontBytes {
    Read(Vec<u8>),
    Mapped(memmap2::Mmap),
}

impl FontBytes {
    /// Kept for the rest of the session.
    fn leak(self) -> &'static [u8] {
        match self {
            FontBytes::Read(bytes) => Box::leak(bytes.into_boxed_slice()),
            FontBytes::Mapped(map) => &Box::leak(Box::new(map))[..],
        }
    }
}

/// Reads font file `path`, mapping it when it is large.
fn read_font_file(path: &Path) -> Option<FontBytes> {
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if meta.is_file() && meta.len() >= MAP_FONT_BYTES {
        // SAFETY: the map is read-only and kept for the session. Font files
        // are replaced (a new inode), not rewritten in place, when packages
        // are updated, so the mapped content does not change underneath.
        if let Ok(map) = unsafe { memmap2::Mmap::map(&file) } {
            return Some(FontBytes::Mapped(map));
        }
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::io::Read::read_to_end(&mut &file, &mut bytes).ok()?;
    Some(FontBytes::Read(bytes))
}

fn cached(key: String, make: impl FnOnce() -> Option<FontBytes>) -> Option<&'static [u8]> {
    if let Some(bytes) = LOADED
        .lock()
        .ok()?
        .get_or_insert_with(HashMap::new)
        .get(&key)
    {
        return Some(bytes);
    }
    // Made without the lock: reading a big font file takes a while.
    let made = make()?;
    let mut guard = LOADED.lock().ok()?;
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(bytes) = map.get(&key) {
        return Some(bytes); // another thread was quicker
    }
    let bytes = made.leak();
    map.insert(key, bytes);
    Some(bytes)
}

/// The bytes of a font file (read once).
fn font_bytes(path: &Path) -> Option<&'static [u8]> {
    cached(path.display().to_string(), || read_font_file(path))
}

/// `bytes` (known as `key`) without ligatures; `bytes` itself when it has none.
fn no_ligatures(key: &str, bytes: &'static [u8], index: u32) -> &'static [u8] {
    cached(format!("{key}#{index}#no-ligatures"), || {
        without_ligatures(bytes, index).map(FontBytes::Read)
    })
    .unwrap_or(bytes)
}

/// egui font data for `face` (as bold, ligatures on or off); `None` when the
/// file is gone or unreadable (egui would panic on it).
fn face_data(face: &Face, bold: bool, ligatures: bool) -> Option<FontData> {
    let mut bytes = font_bytes(&face.path)?;
    if skrifa::FontRef::from_index(bytes, face.index).is_err() {
        log::warn!(
            "font {} (face {}) cannot be read",
            face.path.display(),
            face.index
        );
        return None;
    }
    if !ligatures {
        bytes = no_ligatures(&face.path.display().to_string(), bytes, face.index);
    }
    let mut data = FontData::from_static(bytes);
    data.index = face.index;
    // A variable font without a bold face: its weight axis makes it bold.
    if bold && face.weight < 600 {
        let wght = egui::epaint::text::Tag::new(b"wght");
        if let Some(axis) = data.variation_axes().into_iter().find(|a| a.tag == wght) {
            data.tweak.coords = egui::epaint::text::VariationCoords::new([(
                b"wght",
                700.0_f32.clamp(axis.range.min, axis.range.max),
            )]);
        }
    }
    Some(data)
}

/// What the fonts depend on in the settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontChoice {
    /// Editor font family (empty or `Hack`: the bundled font).
    pub editor_family: String,
    pub editor_fallbacks: Vec<String>,
    pub console_family: String,
    /// UI font family (empty: egui's default).
    pub ui_family: String,
    pub ligatures: bool,
}

impl FontChoice {
    pub fn from_settings(settings: &rustpy_settings::Settings) -> Self {
        Self {
            editor_family: settings
                .editor
                .preferences
                .scheme_fonts
                .get(&settings.editor.color_scheme)
                .filter(|font| font.enabled)
                .map_or_else(
                    || settings.editor.font_family.trim().to_string(),
                    |font| font.family.trim().to_owned(),
                ),
            editor_fallbacks: settings.editor.preferences.font_fallbacks.clone(),
            console_family: settings.run_console.font_family.clone(),
            ui_family: settings.appearance.ui_font_family.trim().to_string(),
            ligatures: settings.editor.enable_ligatures,
        }
    }
}

/// Whether `family` means the bundled editor font.
pub fn is_bundled_editor_font(family: &str) -> bool {
    family.trim().is_empty() || family.trim().eq_ignore_ascii_case(BUNDLED_EDITOR_FONT)
}

/// The font definitions for `choice`: the bundled fonts, the chosen editor
/// and UI families when installed, and (with `fallbacks`) the fallback fonts
/// after every family's own fonts.
pub fn definitions(
    choice: &FontChoice,
    system: Option<&SystemFonts>,
    fallbacks: bool,
) -> FontDefinitions {
    let mut defs = theme::bundled_font_definitions();
    let editor = system
        .filter(|_| !is_bundled_editor_font(&choice.editor_family))
        .and_then(|s| s.family(&choice.editor_family));
    for (slot, bold, italic, bundled) in theme::bundled_mono_faces() {
        if !choice.ligatures {
            let bytes = no_ligatures(slot, bundled, 0);
            defs.font_data
                .insert(slot.to_string(), Arc::new(FontData::from_static(bytes)));
        }
        let Some(data) = editor
            .and_then(|f| f.face(bold, italic))
            .and_then(|face| face_data(face, bold, choice.ligatures))
        else {
            continue;
        };
        if let Some(data) = editor
            .and_then(|family| family.face(bold, italic))
            .and_then(|face| face_data(face, bold, true))
        {
            defs.font_data
                .insert(format!("reader:{slot}"), Arc::new(data));
        }
        // The chosen font first; Hack stays for the characters it lacks.
        let key = format!("editor: {slot}");
        defs.font_data.insert(key.clone(), Arc::new(data));
        if let Some(list) = defs.families.get_mut(&FontFamily::Name(slot.into())) {
            list.insert(0, key.clone());
        }
        if slot == MONO_REGULAR {
            if let Some(list) = defs.families.get_mut(&FontFamily::Monospace) {
                list.insert(0, key);
            }
        }
    }
    for (slot, bold, italic, _) in theme::bundled_mono_faces() {
        for name in choice.editor_fallbacks.iter().take(16).rev() {
            let Some(data) = system
                .and_then(|fonts| fonts.family(name))
                .and_then(|family| family.face(bold, italic))
                .and_then(|face| face_data(face, bold, choice.ligatures))
            else {
                continue;
            };
            let key = format!("secondary:{slot}:{name}");
            defs.font_data.insert(key.clone(), Arc::new(data));
            if let Some(list) = defs.families.get_mut(&FontFamily::Name(slot.into())) {
                let at = usize::from(!list.is_empty());
                list.insert(at, key.clone());
            }
            if slot == MONO_REGULAR {
                if let Some(list) = defs.families.get_mut(&FontFamily::Monospace) {
                    let at = usize::from(!list.is_empty());
                    list.insert(at, key);
                }
            }
        }
    }
    install_console_faces(&mut defs);
    if let Some(family) = system.and_then(|fonts| fonts.family(&choice.console_family)) {
        for (slot, bold, italic, _) in theme::bundled_mono_faces() {
            if let Some(data) = family
                .face(bold, italic)
                .and_then(|face| face_data(face, bold, choice.ligatures))
            {
                let key = format!("console-face:{slot}");
                defs.font_data.insert(key.clone(), Arc::new(data));
                if let Some(list) = defs
                    .families
                    .get_mut(&FontFamily::Name(format!("console:{slot}").into()))
                {
                    list.insert(0, key);
                }
            }
        }
    }
    let ui = system
        .filter(|_| !choice.ui_family.is_empty())
        .and_then(|s| s.family(&choice.ui_family))
        .and_then(|f| f.face(false, false));
    if let Some(data) = ui.and_then(|face| face_data(face, false, true)) {
        defs.font_data.insert("ui".into(), Arc::new(data));
        if let Some(list) = defs.families.get_mut(&FontFamily::Proportional) {
            list.insert(0, "ui".into());
        }
    }
    if fallbacks {
        for (name, face) in system.map(|s| s.fallbacks.as_slice()).unwrap_or_default() {
            let Some(data) = face_data(face, false, true) else {
                continue;
            };
            let key = format!("fallback: {name}");
            defs.font_data.insert(key.clone(), Arc::new(data));
            for list in defs.families.values_mut() {
                list.push(key.clone());
            }
        }
    }
    defs
}

// ----- the application's fonts -----------------------------------------------

/// The installed fonts as far as known, and what was installed into egui.
struct State {
    system: Option<Arc<SystemFonts>>,
    /// The background scan, until it reports.
    scan: Option<Receiver<SystemFonts>>,
    /// The scan has finished: fallbacks can be used (their files are read).
    scanned: bool,
    /// What the installed definitions were built from: `(choice, scan generation)`.
    applied: Option<(FontChoice, u64)>,
    generation: u64,
}

static STATE: Mutex<State> = Mutex::new(State {
    system: None,
    scan: None,
    scanned: false,
    applied: None,
    generation: 0,
});

fn cache_path() -> PathBuf {
    rustpy_settings::Settings::config_dir().join("font_cache.json")
}

fn load_cache() -> Option<SystemFonts> {
    serde_json::from_str(&std::fs::read_to_string(cache_path()).ok()?).ok()
}

fn save_cache(fonts: &SystemFonts) {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(fonts) {
        // Several RustPy windows may write it at once: each through its own file.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// At startup: installs the bundled fonts and the chosen families known from
/// the last run (`font_cache.json`), then scans the installed fonts in the
/// background; [`update`] installs the result.
pub fn start(ctx: &egui::Context, choice: &FontChoice) {
    let Ok(mut st) = STATE.lock() else {
        theme::install_fonts(ctx);
        return;
    };
    st.system = load_cache().map(Arc::new);
    ctx.set_fonts(definitions(choice, st.system.as_deref(), false));
    st.applied = Some((choice.clone(), st.generation));
    let (tx, rx) = channel();
    st.scan = Some(rx);
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("font-scan".into())
        .spawn(move || {
            let fonts = SystemFonts::scan();
            // Read the fallback files here rather than on the UI thread.
            for (_, face) in &fonts.fallbacks {
                let _ = font_bytes(&face.path);
            }
            save_cache(&fonts);
            let _ = tx.send(fonts);
            ctx.request_repaint();
        });
    if let Err(e) = spawned {
        log::warn!("cannot scan the installed fonts: {e}");
        st.scan = None;
    }
}

/// Installs the fonts again when `choice` changed or the background scan
/// finished; cheap otherwise (called every frame).
pub fn update(ctx: &egui::Context, choice: &FontChoice) {
    let Ok(mut st) = STATE.lock() else { return };
    if let Some(fonts) = st.scan.as_ref().and_then(|rx| rx.try_recv().ok()) {
        log::info!(
            "fonts: {} installed families, fallbacks: {}",
            fonts.families.len(),
            fonts
                .fallbacks
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        st.system = Some(Arc::new(fonts));
        st.scan = None;
        st.scanned = true;
        st.generation += 1;
    }
    let wanted = (choice.clone(), st.generation);
    if st.applied.as_ref() == Some(&wanted) {
        return;
    }
    // Before `start` (headless tests) there is nothing to update.
    if st.applied.is_none() {
        return;
    }
    ctx.set_fonts(definitions(choice, st.system.as_deref(), st.scanned));
    st.applied = Some(wanted);
}

/// The installed families for a font chooser (`None` while the first scan runs without a cache).
pub fn family_names(monospace_only: bool) -> Option<Vec<String>> {
    let st = STATE.lock().ok()?;
    st.system.as_ref().map(|s| s.names(monospace_only))
}

/// Whether `family` is installed (the bundled editor font always is); `None` when not known yet.
pub fn is_installed(family: &str) -> Option<bool> {
    if is_bundled_editor_font(family) {
        return Some(true);
    }
    let st = STATE.lock().ok()?;
    st.system.as_ref().map(|s| s.family(family).is_some())
}

/// The fallback families in use (empty until the scan finished).
pub fn fallback_names() -> Vec<String> {
    STATE
        .lock()
        .ok()
        .filter(|st| st.scanned)
        .and_then(|st| {
            st.system
                .as_ref()
                .map(|s| s.fallbacks.iter().map(|(n, _)| n.clone()).collect())
        })
        .unwrap_or_default()
}

/// Whether the installed fonts are still being scanned.
pub fn scanning() -> bool {
    STATE.lock().ok().is_some_and(|st| st.scan.is_some())
}

/// Reader-mode faces retain the installed typeface's ligatures independently of
/// the ordinary editing font's no-ligature byte variants.
pub fn reader_family(bold: bool, italic: bool) -> FontFamily {
    let slot = match (bold, italic) {
        (false, false) => theme::MONO_REGULAR,
        (true, false) => theme::MONO_BOLD,
        (false, true) => theme::MONO_ITALIC,
        (true, true) => theme::MONO_BOLD_ITALIC,
    };
    FontFamily::Name(format!("reader:{slot}").into())
}
pub fn install_reader_faces(defs: &mut FontDefinitions) {
    for (slot, _, _, bytes) in theme::bundled_mono_faces() {
        let key = format!("reader:{slot}");
        defs.font_data
            .insert(key.clone(), Arc::new(FontData::from_static(bytes)));
        defs.families
            .insert(FontFamily::Name(key.clone().into()), vec![key]);
    }
}

pub fn console_family(bold: bool, italic: bool) -> FontFamily {
    let slot = match (bold, italic) {
        (false, false) => theme::MONO_REGULAR,
        (true, false) => theme::MONO_BOLD,
        (false, true) => theme::MONO_ITALIC,
        (true, true) => theme::MONO_BOLD_ITALIC,
    };
    FontFamily::Name(format!("console:{slot}").into())
}
pub fn install_console_faces(defs: &mut FontDefinitions) {
    for (slot, _, _, _) in theme::bundled_mono_faces() {
        if let Some(list) = defs.families.get(&FontFamily::Name(slot.into())).cloned() {
            defs.families
                .insert(FontFamily::Name(format!("console:{slot}").into()), list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(path: &str, weight: u16, italic: bool) -> Face {
        Face {
            path: PathBuf::from(path),
            index: 0,
            weight,
            italic,
        }
    }

    #[test]
    fn large_font_files_are_mapped_with_the_same_content() {
        let dir = tempfile::tempdir().unwrap();
        let (small, large) = (dir.path().join("small.ttf"), dir.path().join("large.ttc"));
        let big: Vec<u8> = (0..MAP_FONT_BYTES as usize + 7).map(|i| i as u8).collect();
        std::fs::write(&small, b"tiny font").unwrap();
        std::fs::write(&large, &big).unwrap();
        assert!(matches!(read_font_file(&small), Some(FontBytes::Read(b)) if b == b"tiny font"));
        let mapped = read_font_file(&large).unwrap();
        assert!(matches!(mapped, FontBytes::Mapped(_)));
        assert_eq!(mapped.leak(), &big[..]);
        assert!(read_font_file(&dir.path().join("missing.ttf")).is_none());
        // Cached once per file, whichever way it was loaded.
        let first = font_bytes(&large).unwrap();
        assert!(std::ptr::eq(first, font_bytes(&large).unwrap()));
        assert_eq!(first, &big[..]);
    }

    fn fonts() -> SystemFonts {
        SystemFonts::from_faces([
            (
                "Fira Code".to_string(),
                true,
                face("/f/FiraCode-Regular.ttf", 400, false),
            ),
            (
                "Fira Code".to_string(),
                true,
                face("/f/FiraCode-Bold.ttf", 700, false),
            ),
            (
                "Fira Code".to_string(),
                true,
                face("/f/FiraCode-Light.ttf", 300, false),
            ),
            (
                "fira code".to_string(),
                true,
                face("/f/FiraCode-Regular-dup.ttf", 400, false),
            ),
            (
                "DejaVu Sans".to_string(),
                false,
                face("/f/DejaVuSans.ttf", 400, false),
            ),
            (
                "DejaVu Sans".to_string(),
                false,
                face("/f/DejaVuSans-Oblique.ttf", 400, true),
            ),
            (
                "FreeMono".to_string(),
                false,
                face("/f/FreeMono.ttf", 400, false),
            ),
            (
                "WenQuanYi Zen Hei".to_string(),
                false,
                face("/f/wqy-zenhei.ttc", 500, false),
            ),
            (
                "Noto Color Emoji".to_string(),
                true,
                face("/f/NotoColorEmoji.ttf", 400, false),
            ),
            (
                ".SF NS".to_string(),
                false,
                face("/f/hidden.ttf", 400, false),
            ),
        ])
    }

    #[test]
    fn families_group_faces_and_pick_the_nearest_face() {
        let f = fonts();
        assert_eq!(
            f.families
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            [
                "DejaVu Sans",
                "Fira Code",
                "FreeMono",
                "Noto Color Emoji",
                "WenQuanYi Zen Hei"
            ]
        );
        let fira = f.family("FIRA CODE").unwrap();
        assert_eq!(
            fira.faces.len(),
            3,
            "a second face with the same weight and style is dropped"
        );
        assert_eq!(fira.face(false, false).unwrap().weight, 400);
        assert_eq!(fira.face(true, false).unwrap().weight, 700);
        assert_eq!(
            fira.face(true, true).unwrap().weight,
            700,
            "no italic: the upright bold face"
        );
        let dejavu = f.family("DejaVu Sans").unwrap();
        assert!(dejavu.face(false, true).unwrap().italic);
        assert!(!dejavu.face(false, false).unwrap().italic);
        assert_eq!(
            f.names(true),
            ["Fira Code", "FreeMono"],
            "FreeMono is monospaced by its name; no emoji font"
        );
        assert_eq!(f.names(false).len(), 4);
        assert!(f.family("Comic Sans").is_none());
    }

    #[test]
    fn fallbacks_are_the_first_installed_family_covering_each_script() {
        let f = fonts();
        // WenQuanYi has CJK; DejaVu Sans has Arabic and Hebrew (and counts once); nothing has Thai.
        let covers = |face: &Face, c: char| match c {
            '中' => face.path.ends_with("wqy-zenhei.ttc"),
            'ع' | 'א' => face.path.ends_with("DejaVuSans.ttf"),
            _ => false,
        };
        let names: Vec<String> = f
            .pick_fallbacks(covers)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["WenQuanYi Zen Hei", "DejaVu Sans"]);
        // A family that lacks the script is skipped for the next one.
        let none = f.pick_fallbacks(|_, _| false);
        assert_eq!(
            none.into_iter().map(|(n, _)| n).collect::<Vec<_>>(),
            ["DejaVu Sans"],
            "only the group without a sample character"
        );
    }

    #[test]
    fn scanning_the_installed_fonts_gives_consistent_families() {
        // Whatever this machine has installed (possibly nothing).
        let f = SystemFonts::scan();
        for family in &f.families {
            assert!(!family.faces.is_empty(), "{}", family.name);
            assert!(f.family(&family.name.to_uppercase()).is_some());
        }
        for (name, face) in &f.fallbacks {
            assert!(
                f.family(name).is_some_and(|fam| fam.faces.contains(face)),
                "{name}"
            );
        }
    }

    #[test]
    fn the_cache_round_trips() {
        let mut f = fonts();
        f.fallbacks = vec![("DejaVu Sans".into(), face("/f/DejaVuSans.ttf", 400, false))];
        let back: SystemFonts = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back, f);
    }

    /// Hack has no ligatures, so the ligature tests use egui's bundled Ubuntu
    /// Light, whose `liga` feature joins `fi` into one glyph.
    fn ligature_font() -> &'static [u8] {
        let font = FontDefinitions::default()
            .font_data
            .get("Ubuntu-Light")
            .expect("egui bundles Ubuntu Light")
            .font
            .clone();
        match font {
            std::borrow::Cow::Borrowed(bytes) => bytes,
            std::borrow::Cow::Owned(bytes) => Box::leak(bytes.into_boxed_slice()),
        }
    }

    #[test]
    fn ligature_features_can_be_switched_off() {
        let regular = ligature_font();
        let before = gsub_features(regular, 0);
        assert!(
            before.iter().any(|t| t == "liga"),
            "the test font has ligatures in liga: {before:?}"
        );
        let patched = without_ligatures(regular, 0).expect("has ligatures");
        let after = gsub_features(&patched, 0);
        assert_eq!(before.len(), after.len());
        for (b, a) in before.iter().zip(&after) {
            if ["liga", "clig", "dlig", "calt"].contains(&b.as_str()) {
                assert_ne!(a, b);
            } else {
                assert_eq!(a, b, "other features stay");
            }
        }
        let mut sorted = after.clone();
        sorted.sort();
        assert_eq!(sorted, after, "the feature list stays sorted");
        assert!(
            without_ligatures(&patched, 0).is_none(),
            "nothing left to switch off"
        );
        assert!(without_ligatures(b"not a font", 0).is_none());
        assert!(
            gsub_features(&regular[..100], 0).is_empty(),
            "a truncated file is no font"
        );
    }

    /// The glyphs egui draws for each of `texts` in a family made of `font`
    /// alone, as their places in the font atlas: within one context the same
    /// glyph always has the same place.
    fn glyphs(font: &'static [u8], texts: &[&str]) -> Vec<Vec<([u16; 2], [u16; 2])>> {
        let ctx = egui::Context::default();
        let mut defs = FontDefinitions::empty();
        // Without subpixel binning a glyph has one place whatever its x position.
        let data = FontData::from_static(font).tweak(egui::FontTweak {
            subpixel_binning: Some(false),
            ..Default::default()
        });
        defs.font_data.insert("t".into(), Arc::new(data));
        for family in [
            FontFamily::Name("t".into()),
            FontFamily::Proportional,
            FontFamily::Monospace,
        ] {
            defs.families.insert(family, vec!["t".into()]);
        }
        ctx.set_fonts(defs);
        let mut out = Vec::new();
        for _ in 0..2 {
            let mut full = ctx.run_ui(Default::default(), |ui| {
                out = texts
                    .iter()
                    .map(|text| {
                        let galley = ui.painter().layout_no_wrap(
                            text.to_string(),
                            egui::FontId::new(14.0, FontFamily::Name("t".into())),
                            egui::Color32::WHITE,
                        );
                        galley
                            .rows
                            .iter()
                            .flat_map(|r| r.glyphs.iter().map(|g| (g.uv_rect.min, g.uv_rect.max)))
                            .collect()
                    })
                    .collect();
            });
            full.textures_delta.clear();
        }
        out
    }

    #[test]
    fn without_ligatures_the_shaper_draws_plain_glyphs() {
        // egui keeps one glyph entry per character: a ligature is its glyph
        // followed by a zero-width continuation with an empty atlas rect.
        let regular = ligature_font();
        let g = glyphs(regular, &["f", "i", "fi"]);
        let empty = ([0u16; 2], [0u16; 2]);
        assert_eq!(g[2].len(), 2);
        assert_eq!(g[2][1], empty, "with ligatures, 'i' joins the 'f' glyph");
        assert_ne!(g[2][0], g[0][0], "the ligature glyph is not a plain 'f'");
        let patched: &'static [u8] =
            Box::leak(without_ligatures(regular, 0).unwrap().into_boxed_slice());
        let g = glyphs(patched, &["f", "i", "fi"]);
        assert_eq!(
            g[2],
            [g[0][0], g[1][0]],
            "without them it is a plain 'f' and 'i'"
        );
    }

    /// Check rasterized glyphs, not just character maps: bitmap emoji fonts
    /// can advertise a character while producing an empty atlas rectangle.
    fn assert_editor_emoji_drawn(mut defs: FontDefinitions) {
        for data in defs.font_data.values_mut() {
            Arc::make_mut(data).tweak.subpixel_binning = Some(false);
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(defs);
        for _ in 0..2 {
            let mut full = ctx.run_ui(Default::default(), |ui| {
                for (name, _, _, _) in theme::bundled_mono_faces() {
                    let id = egui::FontId::new(16.0, FontFamily::Name(name.into()));
                    let layout = |text: &str| {
                        ui.painter()
                            .layout_no_wrap(text.into(), id.clone(), egui::Color32::WHITE)
                    };
                    let atlas = |galley: &egui::Galley| {
                        galley
                            .rows
                            .iter()
                            .flat_map(|row| {
                                row.glyphs.iter().map(|g| (g.uv_rect.min, g.uv_rect.max))
                            })
                            .collect::<Vec<_>>()
                    };
                    let missing = atlas(&layout("\u{10ffff}"));
                    let cell = layout("M").size().x;
                    for text in ["🙂", "🫠", "😀", "🚀", "❤", "👍", "👍🏽", "🇬🇧", "👨‍👩‍👧‍👦"]
                    {
                        let galley = layout(text);
                        let drawn = atlas(&galley);
                        assert_ne!(
                            drawn, missing,
                            "{name}: {text} must not be the missing glyph"
                        );
                        assert!(
                            drawn
                                .iter()
                                .any(|(min, max)| max[0] > min[0] && max[1] > min[1]),
                            "{name}: {text} must occupy the atlas"
                        );
                        assert!(
                            galley.size().x > 0.0 && galley.size().x <= 3.0 * cell,
                            "{name}: {text} has usable dimensions: {:?}",
                            galley.size()
                        );
                    }
                    let width = layout("iiii").size().x;
                    for text in ["WWWW", "0000"] {
                        assert!(
                            (layout(text).size().x - width).abs() < 0.01,
                            "{name}: Latin remains monospaced"
                        );
                    }
                }
            });
            full.textures_delta.clear();
        }
    }

    #[test]
    fn modern_emoji_draw_with_bundled_editor_fonts_before_the_scan() {
        assert_editor_emoji_drawn(theme::bundled_font_definitions());
        for ligatures in [false, true] {
            let defs = definitions(
                &FontChoice {
                    ligatures,
                    ..Default::default()
                },
                None,
                false,
            );
            assert_editor_emoji_drawn(defs);
        }
    }

    #[test]
    fn emoji_fallback_is_idempotent_and_keeps_primary_and_legacy_fonts() {
        let mut defs = theme::bundled_font_definitions();
        install_emoji_fallback(&mut defs);
        let once = defs.families.clone();
        install_emoji_fallback(&mut defs);
        assert_eq!(defs.families, once);
        for (name, _, _, _) in theme::bundled_mono_faces() {
            assert_eq!(
                defs.families[&FontFamily::Name(name.into())],
                [name, BUNDLED_EMOJI, "NotoEmoji-Regular", "emoji-icon-font"]
            );
        }
    }

    #[test]
    fn definitions_add_the_chosen_families_and_fallbacks() {
        // Fonts on disk: the bundled Hack files stand in for installed fonts.
        let dir = std::env::temp_dir().join(format!("rustpy-fonts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut faces = Vec::new();
        for (slot, bold, italic, bytes) in theme::bundled_mono_faces() {
            let path = dir.join(format!("{slot}.ttf"));
            std::fs::write(&path, bytes).unwrap();
            faces.push((
                "Test Mono".to_string(),
                true,
                Face {
                    path,
                    index: 0,
                    weight: if bold { 700 } else { 400 },
                    italic,
                },
            ));
        }
        let broken = dir.join("broken.ttf");
        std::fs::write(&broken, b"not a font").unwrap();
        faces.push((
            "Broken Sans".to_string(),
            false,
            Face {
                path: broken.clone(),
                index: 0,
                weight: 400,
                italic: false,
            },
        ));
        let mut system = SystemFonts::from_faces(faces);
        system.fallbacks = vec![
            (
                "Test Mono".into(),
                system.family("Test Mono").unwrap().faces[0].clone(),
            ),
            (
                "Broken Sans".into(),
                Face {
                    path: broken,
                    index: 0,
                    weight: 400,
                    italic: false,
                },
            ),
        ];
        let choice = FontChoice {
            editor_family: "test mono".into(),
            editor_fallbacks: Vec::new(),
            console_family: String::new(),
            ui_family: "Test Mono".into(),
            ligatures: true,
        };
        let defs = definitions(&choice, Some(&system), true);
        assert_editor_emoji_drawn(defs.clone());
        assert_editor_emoji_drawn(definitions(
            &FontChoice {
                ligatures: false,
                ..choice.clone()
            },
            Some(&system),
            true,
        ));
        let family = |f: FontFamily| defs.families[&f].clone();
        assert_eq!(
            family(FontFamily::Name(MONO_REGULAR.into()))[..2],
            ["editor: Hack".to_string(), MONO_REGULAR.to_string()]
        );
        assert_eq!(
            family(FontFamily::Name(theme::MONO_BOLD.into()))[0],
            "editor: Hack-Bold"
        );
        assert_eq!(family(FontFamily::Monospace)[0], "editor: Hack");
        assert_eq!(family(FontFamily::Proportional)[0], "ui");
        // Fallbacks come last in every family; an unreadable font is left out.
        for list in defs.families.values() {
            assert_eq!(
                list.last().map(String::as_str),
                Some("fallback: Test Mono"),
                "{list:?}"
            );
        }
        assert!(!defs.font_data.contains_key("fallback: Broken Sans"));
        // Without the scan's fallbacks, or with the bundled font: no extra fonts.
        let early = definitions(&choice, Some(&system), false);
        assert!(!early.font_data.keys().any(|k| k.starts_with("fallback")));
        let bundled = definitions(
            &FontChoice {
                editor_family: BUNDLED_EDITOR_FONT.into(),
                editor_fallbacks: Vec::new(),
                console_family: String::new(),
                ui_family: String::new(),
                ligatures: true,
            },
            Some(&system),
            false,
        );
        assert!(!bundled
            .font_data
            .keys()
            .any(|k| k.starts_with("editor") || k == "ui"));
        assert_eq!(
            bundled.font_data[MONO_REGULAR].font.len(),
            theme::bundled_mono_faces()[0].3.len()
        );
        // Ligatures off: the bundled faces have no ligature features (Hack has none).
        let flat = definitions(
            &FontChoice {
                ligatures: false,
                ..Default::default()
            },
            None,
            false,
        );
        assert!(!gsub_features(&flat.font_data[MONO_REGULAR].font, 0)
            .iter()
            .any(|t| LIGATURE_FEATURES
                .iter()
                .any(|l| t.as_bytes() == l.as_slice())));
        let ctx = egui::Context::default();
        ctx.set_fonts(defs);
        let mut full = ctx.run_ui(Default::default(), |ui| {
            ui.label("x");
        });
        full.textures_delta.clear();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
