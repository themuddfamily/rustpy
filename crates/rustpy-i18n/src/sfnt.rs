//! A minimal reader for the `name` table of TrueType/OpenType fonts and
//! collections (`.ttc`/`.otc`): enough to tell the faces of a collection
//! apart without loading the (often 20–100 MB) file.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// Upper bounds that keep a corrupt header from causing huge reads.
const MAX_FACES: u32 = 64;
const MAX_TABLES: usize = 512;
const MAX_NAME_TABLE: usize = 1 << 20;

/// The names of one face.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FaceNames {
    /// Family, full and typographic family names (name IDs 1, 4, 16), e.g.
    /// `Noto Sans CJK SC`, `Noto Sans CJK SC Regular`.
    pub families: Vec<String>,
    /// Subfamily and typographic subfamily names (name IDs 2, 17), e.g.
    /// `Regular`, `Bold`, `W3`.
    pub styles: Vec<String>,
}

/// The names of every face of the font file at `path`: one entry for a
/// `.ttf`/`.otf`, one per face (in face index order) for a collection.
pub(crate) fn read_face_names(path: &Path) -> io::Result<Vec<FaceNames>> {
    let mut file = File::open(path)?;
    let header = read_at(&mut file, 0, 12)?;
    let offsets: Vec<u64> = match &header[..4] {
        b"ttcf" => {
            let count = u32_at(&header, 8).min(MAX_FACES);
            let table = read_at(&mut file, 12, 4 * count as usize)?;
            table
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u64::from(u32_at(c, 0)))
                .collect()
        }
        [0, 1, 0, 0] | b"OTTO" | b"true" => vec![0],
        _ => return Err(invalid("not a TrueType/OpenType font")),
    };
    offsets
        .into_iter()
        .map(|offset| face_names(&mut file, offset))
        .collect()
}

fn face_names(file: &mut File, offset: u64) -> io::Result<FaceNames> {
    let header = read_at(file, offset, 12)?;
    let tables = usize::from(u16_at(&header, 4)).min(MAX_TABLES);
    let records = read_at(file, offset + 12, 16 * tables)?;
    let Some(record) = records
        .as_chunks::<16>()
        .0
        .iter()
        .find(|r| &r[..4] == b"name")
    else {
        return Ok(FaceNames::default());
    };
    let length = (u32_at(record, 12) as usize).min(MAX_NAME_TABLE);
    let table = read_at(file, u64::from(u32_at(record, 8)), length)?;
    Ok(parse_name_table(&table))
}

/// Decodes the Unicode (platform 0), Windows Unicode (platform 3) and
/// Macintosh Roman (platform 1, ASCII subset) records of a `name` table.
pub(crate) fn parse_name_table(table: &[u8]) -> FaceNames {
    let mut names = FaceNames::default();
    let count = usize::from(u16_at(table, 2));
    let storage = usize::from(u16_at(table, 4));
    for record in table
        .get(6..)
        .unwrap_or_default()
        .as_chunks::<12>()
        .0
        .iter()
        .take(count)
    {
        let (platform, encoding, name_id) =
            (u16_at(record, 0), u16_at(record, 2), u16_at(record, 6));
        let start = storage + usize::from(u16_at(record, 10));
        let Some(bytes) = table.get(start..start + usize::from(u16_at(record, 8))) else {
            continue;
        };
        let text = match (platform, encoding) {
            (0, _) | (3, 0 | 1 | 10) => {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&units)
            }
            (1, 0) => bytes
                .iter()
                .map(|&b| if b.is_ascii() { char::from(b) } else { '?' })
                .collect(),
            _ => continue,
        };
        let list = match name_id {
            1 | 4 | 16 => &mut names.families,
            2 | 17 => &mut names.styles,
            _ => continue,
        };
        let text = text.trim().to_owned();
        if !text.is_empty() && !list.contains(&text) {
            list.push(text);
        }
    }
    names
}

fn read_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map_or(0, |s| u16::from_be_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `name` table with Windows (UTF-16) records for `(name id, text)`.
    pub(crate) fn name_table(records: &[(u16, &str)]) -> Vec<u8> {
        let strings: Vec<Vec<u8>> = records
            .iter()
            .map(|(_, s)| s.encode_utf16().flat_map(u16::to_be_bytes).collect())
            .collect();
        let storage = 6 + 12 * records.len();
        let mut out = Vec::new();
        out.extend(0u16.to_be_bytes());
        out.extend((records.len() as u16).to_be_bytes());
        out.extend((storage as u16).to_be_bytes());
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

    /// A font file whose faces only have a `name` table: a collection when
    /// there are several faces (or `collection` is set), else a plain font.
    pub(crate) fn font_file(faces: &[(&str, &str)], collection: bool) -> Vec<u8> {
        let tables: Vec<Vec<u8>> = faces
            .iter()
            .map(|(family, style)| {
                name_table(&[(1, family), (2, style), (4, &format!("{family} {style}"))])
            })
            .collect();
        let collection = collection || faces.len() > 1;
        let header_len = if collection { 12 + 4 * faces.len() } else { 0 };
        let dirs_len = 28 * faces.len();
        let mut table_offset = header_len + dirs_len;
        let mut out = Vec::new();
        if collection {
            out.extend(b"ttcf");
            out.extend(0x0001_0000u32.to_be_bytes());
            out.extend((faces.len() as u32).to_be_bytes());
            for i in 0..faces.len() {
                out.extend(((header_len + 28 * i) as u32).to_be_bytes());
            }
        }
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

    #[test]
    fn parses_windows_and_mac_records() {
        let table = name_table(&[
            (1, "Noto Sans CJK JP"),
            (2, "Regular"),
            (16, "Noto Sans CJK JP"),
            (6, "NotoSansCJKjp-Regular"),
        ]);
        let names = parse_name_table(&table);
        assert_eq!(
            names.families,
            ["Noto Sans CJK JP"],
            "deduplicated; the PostScript name (ID 6) is ignored"
        );
        assert_eq!(names.styles, ["Regular"]);

        // One Macintosh Roman record: format, count, storage offset, then
        // platform 1, encoding 0, language 0, name ID 1, length, offset.
        let text = b"Hiragino Sans GB W3";
        let mut mac = Vec::new();
        for v in [0u16, 1, 18, 1, 0, 0, 1, text.len() as u16, 0] {
            mac.extend(v.to_be_bytes());
        }
        mac.extend(text);
        assert_eq!(parse_name_table(&mac).families, ["Hiragino Sans GB W3"]);
    }

    #[test]
    fn truncated_tables_do_not_panic() {
        let table = name_table(&[(1, "Family"), (2, "Bold")]);
        for len in 0..table.len() {
            let _ = parse_name_table(&table[..len]);
        }
    }

    #[test]
    fn reads_every_face_of_a_collection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.ttc");
        std::fs::write(
            &path,
            font_file(
                &[
                    ("Noto Sans CJK JP", "Regular"),
                    ("Noto Sans CJK SC", "Bold"),
                ],
                true,
            ),
        )
        .unwrap();
        let faces = read_face_names(&path).unwrap();
        assert_eq!(faces.len(), 2);
        assert_eq!(
            faces[1].families,
            ["Noto Sans CJK SC", "Noto Sans CJK SC Bold"]
        );
        assert_eq!(faces[1].styles, ["Bold"]);

        let single = dir.path().join("single.otf");
        std::fs::write(&single, font_file(&[("Malgun Gothic", "Regular")], false)).unwrap();
        assert_eq!(
            read_face_names(&single).unwrap()[0].families[0],
            "Malgun Gothic"
        );

        let junk = dir.path().join("junk.ttc");
        std::fs::write(&junk, b"definitely not a font").unwrap();
        assert!(read_face_names(&junk).is_err());
        std::fs::write(&junk, b"ttcf\0\x01\0\0\0\0\0\x09").unwrap();
        assert!(read_face_names(&junk).is_err(), "truncated offset table");
    }
}
