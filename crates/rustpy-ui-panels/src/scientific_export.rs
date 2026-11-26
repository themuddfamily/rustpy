//! Deterministic CSV and Excel XLSX export of the explicitly loaded rectangle.
fn xml(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || *c == '\t' || *c == '\r' || *c >= ' ')
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
pub fn csv(columns: &[String], rows: &[Vec<String>], separator: char) -> String {
    std::iter::once(columns.to_vec())
        .chain(rows.iter().cloned())
        .map(|row| {
            row.iter()
                .map(|cell| format!("\"{}\"", cell.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(&separator.to_string())
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}
fn crc(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
fn u16le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn stored_zip(files: Vec<(&str, String)>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let count = files.len() as u16;
    for (name, text) in files {
        let bytes = text.as_bytes();
        let offset = out.len() as u32;
        let checksum = crc(bytes);
        u32le(&mut out, 0x04034b50);
        for v in [20, 0, 0, 0, 33] {
            u16le(&mut out, v);
        }
        u32le(&mut out, checksum);
        u32le(&mut out, bytes.len() as u32);
        u32le(&mut out, bytes.len() as u32);
        u16le(&mut out, name.len() as u16);
        u16le(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(bytes);
        u32le(&mut central, 0x02014b50);
        for v in [20, 20, 0, 0, 0, 33] {
            u16le(&mut central, v);
        }
        u32le(&mut central, checksum);
        u32le(&mut central, bytes.len() as u32);
        u32le(&mut central, bytes.len() as u32);
        u16le(&mut central, name.len() as u16);
        for _ in 0..4 {
            u16le(&mut central, 0);
        }
        u32le(&mut central, 0);
        u32le(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let start = out.len() as u32;
    let size = central.len() as u32;
    out.extend(central);
    u32le(&mut out, 0x06054b50);
    u16le(&mut out, 0);
    u16le(&mut out, 0);
    u16le(&mut out, count);
    u16le(&mut out, count);
    u32le(&mut out, size);
    u32le(&mut out, start);
    u16le(&mut out, 0);
    out
}
pub fn xlsx(columns: &[String], rows: &[Vec<String>]) -> Vec<u8> {
    let mut sheet=String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>");
    for row in std::iter::once(columns.to_vec()).chain(rows.iter().cloned()) {
        sheet.push_str("<row>");
        for cell in row {
            // Inline strings prevent formula injection and preserve displayed representations.
            sheet.push_str(&format!(
                "<c t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                xml(&cell)
            ));
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData></worksheet>");
    stored_zip(vec![("[Content_Types].xml",r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.into()),("_rels/.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.into()),("xl/workbook.xml",r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="SciView" sheetId="1" r:id="rId1"/></sheets></workbook>"#.into()),("xl/_rels/workbook.xml.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.into()),("xl/worksheets/sheet1.xml",sheet)])
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn export_preserves_quotes_and_never_injects_excel_formulas() {
        let cols = vec!["a&b".into()];
        let rows = vec![vec!["=SUM(1,2)\n\"quoted\"".into()]];
        assert!(csv(&cols, &rows, ',').contains("\"\"quoted\"\""));
        let workbook = xlsx(&cols, &rows);
        assert!(workbook.starts_with(b"PK\x03\x04"));
        let text = String::from_utf8_lossy(&workbook);
        assert!(text.contains("a&amp;b"));
        assert!(text.contains("t=\"inlineStr\""));
        assert!(!text.contains("<f>"));
    }
}
