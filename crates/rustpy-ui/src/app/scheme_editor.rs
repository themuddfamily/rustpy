//! Advanced scheme controls; called from the existing Color Scheme page.
use crate::a11y::AccessibleCombo;
use egui::Ui;
use rustpy_highlight::{ColorScheme, HighlightTag, Rgb};
use std::io::Read;
use std::path::Path;

#[derive(Default)]
pub(super) struct State {
    pub path: String,
    pub message: String,
}

pub(super) fn load(path: &Path) -> Result<ColorScheme, String> {
    let mut bytes = Vec::new();
    let maximum = if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
    {
        8 * 1024 * 1024
    } else {
        1024 * 1024
    };
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > maximum {
        return Err("Scheme exceeds 1 MiB".into());
    }
    if bytes.starts_with(b"PK") {
        let archive = crate::data_archive::Archive::parse(bytes)?;
        let mut schemes: Vec<_> = archive
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.safe && entry.name.ends_with(".icls") && entry.size <= 1024 * 1024
            })
            .map(|(index, entry)| (entry.name.clone(), index))
            .collect();
        schemes.sort();
        let (_, index) = schemes
            .first()
            .ok_or("JAR contains no bounded .icls color scheme")?;
        bytes = archive.member(*index)?;
    }
    let scheme: ColorScheme = if bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<') {
        import_icls(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)?
    } else {
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?
    };
    if scheme.name.trim().is_empty() || scheme.name.len() > 128 {
        return Err("Invalid scheme name".into());
    }
    if scheme.styles.len() > 256 || scheme.inherits.len() > 256 || scheme.context_colors.len() > 128
    {
        return Err("Scheme has too many attributes".into());
    }
    let mut validated = scheme.clone();
    validated.inherits.clear();
    for (tag, parent) in &scheme.inherits {
        if !validated.inherit_style(*tag, *parent) {
            return Err("Cyclic or overly deep attribute inheritance".into());
        }
    }
    Ok(scheme)
}
pub(super) fn save(path: &Path, scheme: &ColorScheme) -> Result<(), String> {
    let bytes = if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
    {
        stored_jar(&export_icls(scheme))?
    } else if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("icls"))
    {
        export_icls(scheme).into_bytes()
    } else {
        serde_json::to_vec_pretty(scheme).map_err(|e| e.to_string())?
    };
    if bytes.len() > 1024 * 1024 {
        return Err("Scheme exceeds 1 MiB".into());
    }
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}
fn color(ui: &mut Ui, label: &str, value: &mut Rgb) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut rgb = [value.0, value.1, value.2];
        if ui.color_edit_button_srgb(&mut rgb).changed() {
            *value = Rgb(rgb[0], rgb[1], rgb[2]);
        }
    });
}

pub(super) fn show(
    ui: &mut Ui,
    scheme: &mut ColorScheme,
    tag: Option<HighlightTag>,
    state: &mut State,
) {
    ui.collapsing("Effects, inheritance and scheme files",|ui|{
        if let Some(tag)=tag {
            let mut style=scheme.style(tag);
            let mut changed=false;
            changed|=ui.checkbox(&mut style.wave,"Wave underline").changed();
            changed|=ui.checkbox(&mut style.bold_underline,"Bold underline").changed();
            changed|=ui.checkbox(&mut style.strikeout,"Strikeout").changed();
            changed|=ui.checkbox(&mut style.border,"Border").changed();
            let mut effect=style.effect_color.unwrap_or(scheme.editor.foreground);
            let old=effect;color(ui,"Effect color",&mut effect);
            if effect!=old{style.effect_color=Some(effect);changed=true;}
            if changed{scheme.inherits.remove(&tag);scheme.styles.insert(tag,style);}
            let mut tags:Vec<_>=scheme.styles.keys().copied().collect();
            tags.extend(rustpy_highlight::PYTHON_ATTRIBUTE_KEYS.iter().map(|(_,tag)|*tag));tags.sort();tags.dedup();
            let current=scheme.inherits.get(&tag).copied();
            egui::ComboBox::from_id_salt("scheme_inherit").selected_text(current.map(|tag|format!("Inherit {tag:?}")).unwrap_or_else(||"Language default / explicit style".into())).show_ui(ui,|ui|{
                if ui.selectable_label(current.is_none(),"Explicit style").clicked(){scheme.inherits.remove(&tag);}
                for parent in tags {if parent!=tag && ui.selectable_label(current==Some(parent),format!("{parent:?}")).clicked() && !scheme.inherit_style(tag,parent){state.message="Inheritance would form a cycle".into();}}
            }).combo_named(crate::i18n::tr_static("Inherit style"));
            if ui.button("Restore language default").clicked(){scheme.reset_style(tag);}
        }
        ui.horizontal(|ui|{ui.text_edit_singleline(&mut state.path);
            if ui.button("Import scheme…").clicked(){match load(Path::new(state.path.trim())).and_then(install_import){Ok(imported)=>{*scheme=imported;state.message="Scheme imported; Apply selects it".into();},Err(error)=>state.message=error}}
            if ui.button("Export scheme…").clicked(){state.message=match save(Path::new(state.path.trim()),scheme){Ok(())=>"Scheme exported".into(),Err(error)=>error};}
        });
        ui.label("Import/export .icls color-scheme files, .jar color-scheme archives or portable JSON. For multi-scheme JARs, the first .icls member in filename order is imported; archive members are never extracted to disk.");
        ui.label(&state.message);
    });
    ui.collapsing("Editor, debugger, diff and VCS colors", |ui| {
        color(ui, "Background", &mut scheme.editor.background);
        color(ui, "Foreground", &mut scheme.editor.foreground);
        color(ui, "Selection", &mut scheme.editor.selection);
        color(ui, "Added line", &mut scheme.editor.added_line);
        color(ui, "Modified line", &mut scheme.editor.modified_line);
        color(ui, "Deleted line", &mut scheme.editor.deleted_line);
        color(ui, "Execution / caret row", &mut scheme.editor.caret_row);
        for key in [
            "console.background",
            "console.selection",
            "console.search",
            "console.search.current",
            "console.text",
            "console.stderr",
            "console.system",
            "console.link",
            "debugger.execution",
            "vcs.added",
            "vcs.modified",
            "vcs.deleted",
            "file.unmodified",
            "file.added",
            "file.deleted",
            "file.renamed",
            "file.conflicted",
            "file.ignored",
            "file.modified",
            "file.untracked",
            "diff.inserted",
            "diff.deleted",
        ] {
            let fallback = crate::diff_view::diff_color_default(key)
                .map_or(scheme.editor.foreground, |[r, g, b]| Rgb(r, g, b));
            let mut value = scheme.context_color(key, fallback);
            let old = value;
            color(ui, key, &mut value);
            if old != value {
                scheme.context_colors.insert(key.into(), value);
            }
        }
    });
}

fn import_icls(source: &str) -> Result<ColorScheme, String> {
    let document = roxmltree::Document::parse_with_options(
        source,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 8192,
        },
    )
    .map_err(|e| e.to_string())?;
    let root = document.root_element();
    if root.tag_name().name() != "scheme" {
        return Err("Expected a scheme document".into());
    }
    // `ColorScheme::by_name` also accepts the parent-scheme values `.icls`
    // files use for the builtin dark and light schemes.
    let base = root.attribute("parent_scheme").unwrap_or("Dark");
    let mut scheme = ColorScheme::by_name(base).ok_or("Unknown parent scheme")?;
    scheme.name = root.attribute("name").unwrap_or("Imported").into();
    for option in root
        .descendants()
        .filter(|node| node.has_tag_name("option"))
    {
        let Some(key) = option.attribute("name") else {
            continue;
        };
        if let Some(mut style) = scheme.python_attribute_style(key) {
            if let Some(base_key) = option.attribute("baseAttributes") {
                if let Some((_, parent)) = rustpy_highlight::PYTHON_ATTRIBUTE_KEYS
                    .iter()
                    .find(|(key, _)| *key == base_key)
                {
                    if let Some((_, tag)) = rustpy_highlight::PYTHON_ATTRIBUTE_KEYS
                        .iter()
                        .find(|(name, _)| *name == key)
                    {
                        if !scheme.inherit_style(*tag, *parent) {
                            return Err("Cyclic inherited attributes".into());
                        }
                    }
                }
                continue;
            }
            for field in option
                .children()
                .filter(|node| node.has_tag_name("value"))
                .flat_map(|value| value.children())
                .filter(|node| node.has_tag_name("option"))
            {
                let value = field.attribute("value").unwrap_or("");
                match field.attribute("name").unwrap_or("") {
                    "FOREGROUND" => style.fg = Some(hex(value)?),
                    "BACKGROUND" => style.bg = Some(hex(value)?),
                    "EFFECT_COLOR" => style.effect_color = Some(hex(value)?),
                    "FONT_TYPE" => {
                        let flags = value.parse::<u8>().map_err(|e| e.to_string())?;
                        style.bold = flags & 1 != 0;
                        style.italic = flags & 2 != 0;
                    }
                    "EFFECT_TYPE" => {
                        style.underline = false;
                        style.bold_underline = false;
                        style.wave = false;
                        style.strikeout = false;
                        style.border = false;
                        match value {
                            "0" => style.border = true,
                            "1" => style.underline = true,
                            "2" => style.wave = true,
                            "3" => style.strikeout = true,
                            "4" => style.bold_underline = true,
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            scheme.set_python_attribute_style(key, style);
        }
    }
    Ok(scheme)
}
fn hex(value: &str) -> Result<Rgb, String> {
    if value.is_empty() || value.len() > 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid RGB value".into());
    }
    u32::from_str_radix(value, 16)
        .map(Rgb::hex)
        .map_err(|e| e.to_string())
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
/// `parent_scheme` values that `.icls` consumers recognise for the builtin dark
/// and light schemes; they must match the external file format byte for byte.
const ICLS_DARK_PARENT: &str = "Dark";
const ICLS_LIGHT_PARENT: &str = "Default";
fn export_icls(scheme: &ColorScheme) -> String {
    let mut out = format!(
        "<scheme name=\"{}\" parent_scheme=\"{}\" version=\"142\"><attributes>",
        xml(&scheme.name),
        if scheme.dark {
            ICLS_DARK_PARENT
        } else {
            ICLS_LIGHT_PARENT
        }
    );
    for (key, tag) in rustpy_highlight::PYTHON_ATTRIBUTE_KEYS {
        let style = scheme.style(*tag);
        out.push_str(&format!("<option name=\"{key}\"><value>"));
        for (name, color) in [
            ("FOREGROUND", style.fg),
            ("BACKGROUND", style.bg),
            ("EFFECT_COLOR", style.effect_color),
        ] {
            if let Some(Rgb(r, g, b)) = color {
                out.push_str(&format!(
                    "<option name=\"{name}\" value=\"{r:02x}{g:02x}{b:02x}\"/>"
                ));
            }
        }
        out.push_str(&format!(
            "<option name=\"FONT_TYPE\" value=\"{}\"/>",
            u8::from(style.bold) + 2 * u8::from(style.italic)
        ));
        if style.wave || style.strikeout || style.border || style.underline || style.bold_underline
        {
            let effect = if style.bold_underline {
                4
            } else if style.wave {
                2
            } else if style.strikeout {
                3
            } else if style.border {
                0
            } else {
                1
            };
            out.push_str(&format!(
                "<option name=\"EFFECT_TYPE\" value=\"{effect}\"/>"
            ));
        }
        out.push_str("</value></option>");
    }
    out.push_str("</attributes></scheme>");
    out
}

fn install_import(mut scheme: ColorScheme) -> Result<ColorScheme, String> {
    if scheme.name.contains(['/', '\\', '\0']) || matches!(scheme.name.as_str(), "." | "..") {
        return Err("Scheme name cannot contain a path".into());
    }
    if ColorScheme::builtin_names().contains(&scheme.name.as_str()) {
        scheme.name.push_str(" Imported");
    }
    let directory = rustpy_settings::Settings::config_dir().join("colors");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    save(&directory.join(format!("{}.json", scheme.name)), &scheme)?;
    Ok(scheme)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unset_diff_keys_fall_back_to_viewer_defaults() {
        use crate::diff_view::{diff_color_default, DIFF_DELETED, DIFF_INSERTED};
        assert_eq!(diff_color_default("diff.inserted"), Some(DIFF_INSERTED.1));
        assert_eq!(diff_color_default("diff.deleted"), Some(DIFF_DELETED.1));
        assert_eq!(diff_color_default("vcs.added"), None);
        let scheme = ColorScheme::dark();
        let [r, g, b] = DIFF_INSERTED.1;
        assert_eq!(
            scheme.context_color("diff.inserted", Rgb(r, g, b)),
            Rgb(r, g, b)
        );
    }
    #[test]
    fn actual_json_files_roundtrip_all_effects_inheritance_and_context_colors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom.jar.json");
        let mut scheme = ColorScheme::dark();
        scheme.name = "Custom".into();
        let mut style = scheme.style(HighlightTag::String);
        style.wave = true;
        style.border = true;
        style.strikeout = true;
        style.effect_color = Some(Rgb::hex(0x123456));
        scheme.styles.insert(HighlightTag::String, style);
        assert!(scheme.inherit_style(HighlightTag::Bytes, HighlightTag::String));
        scheme
            .context_colors
            .insert("console.stderr".into(), Rgb::hex(0x654321));
        save(&path, &scheme).unwrap();
        assert_eq!(load(&path).unwrap(), scheme);
    }
    #[test]
    fn actual_icls_import_and_export_preserve_external_key_effects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom.icls");
        std::fs::write(&path,r#"<scheme name="Custom" parent_scheme="Dark"><attributes><option name="PY.STRING.U"><value><option name="FOREGROUND" value="123456"/><option name="EFFECT_COLOR" value="654321"/><option name="EFFECT_TYPE" value="2"/><option name="FONT_TYPE" value="3"/></value></option></attributes></scheme>"#).unwrap();
        let scheme = load(&path).unwrap();
        let style = scheme.style(HighlightTag::String);
        assert_eq!(style.fg, Some(Rgb::hex(0x123456)));
        assert!(style.wave && style.bold && style.italic);
        assert_eq!(scheme.style(HighlightTag::FString), style);
        save(&path, &scheme).unwrap();
        assert_eq!(load(&path).unwrap().style(HighlightTag::String), style);
    }
    #[test]
    fn malformed_inheritance_and_external_entities_are_refused() {
        assert!(import_icls("<!DOCTYPE scheme [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><scheme name='&x;' parent_scheme='Dark'/>").is_err());
        assert!(import_icls("<not-a-scheme/>").is_err());
    }
}

fn stored_jar(xml: &str) -> Result<Vec<u8>, String> {
    if xml.len() > 1024 * 1024 {
        return Err("Scheme exceeds 1 MiB".into());
    }
    let name = b"colors/RustPy.icls";
    let data = xml.as_bytes();
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
        }
    }
    let crc = !crc;
    fn u16le(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    fn u32le(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    let mut out = Vec::new();
    u32le(&mut out, 0x04034b50);
    for value in [20, 0, 0, 0, 0] {
        u16le(&mut out, value);
    }
    u32le(&mut out, crc);
    u32le(&mut out, data.len() as u32);
    u32le(&mut out, data.len() as u32);
    u16le(&mut out, name.len() as u16);
    u16le(&mut out, 0);
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    let central = out.len();
    u32le(&mut out, 0x02014b50);
    for value in [20, 20, 0, 0, 0, 0] {
        u16le(&mut out, value);
    }
    u32le(&mut out, crc);
    u32le(&mut out, data.len() as u32);
    u32le(&mut out, data.len() as u32);
    u16le(&mut out, name.len() as u16);
    for _ in 0..4 {
        u16le(&mut out, 0);
    }
    u32le(&mut out, 0);
    u32le(&mut out, 0);
    out.extend_from_slice(name);
    let size = out.len() - central;
    u32le(&mut out, 0x06054b50);
    for value in [0, 0, 1, 1] {
        u16le(&mut out, value);
    }
    u32le(&mut out, size as u32);
    u32le(&mut out, central as u32);
    u16le(&mut out, 0);
    Ok(out)
}
#[cfg(test)]
mod archive_fixture {
    use super::*;
    #[test]
    fn actual_jar_roundtrip_and_member_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scheme.jar");
        let scheme = ColorScheme::dark();
        save(&path, &scheme).unwrap();
        assert!(std::fs::read(&path).unwrap().starts_with(b"PK"));
        let imported = load(&path).unwrap();
        assert_eq!(imported.name, scheme.name);
        assert_eq!(imported.editor.foreground, scheme.editor.foreground);
    }
}
