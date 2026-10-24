//! Inert source decoding: no codec imports, filesystem lookups or Python runtime.
#[derive(Debug)]
pub enum DecodeError {
    Invalid(&'static str),
    Unsupported(String),
}
fn cookie(line: &[u8]) -> Option<String> {
    let first = line
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\x0c'))?;
    if line[first] != b'#' {
        return None;
    }
    let tail = &line[first + 1..];
    for i in 0..tail.len().saturating_sub(6) {
        if !tail[i..].starts_with(b"coding") {
            continue;
        }
        let marker = *tail.get(i + 6)?;
        if !matches!(marker, b':' | b'=') {
            continue;
        }
        let mut at = i + 7;
        while tail.get(at).is_some_and(|b| matches!(b, b' ' | b'\t')) {
            at += 1;
        }
        let end = tail[at..]
            .iter()
            .position(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'-' | b'_' | b'.'))
            .map_or(tail.len(), |n| at + n);
        if end > at {
            return Some(String::from_utf8(tail[at..end].to_vec()).expect("ASCII cookie"));
        }
    }
    None
}
pub fn decode(bytes: &[u8]) -> Result<String, DecodeError> {
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    let bytes = if bom { &bytes[3..] } else { bytes };
    let mut header = Vec::with_capacity(bytes.len());
    let mut physical = bytes.iter().peekable();
    while let Some(&b) = physical.next() {
        if b == b'\r' {
            header.push(b'\n');
            if physical.peek().is_some_and(|b| **b == b'\n') {
                physical.next();
            }
        } else {
            header.push(b);
        }
    }
    let mut lines = header.split(|b| *b == b'\n');
    let first = lines.next().unwrap_or_default();
    let mut encoding = cookie(first);
    // CPython accepts a second-line cookie only if first line is blank/comment.
    let first_content = first
        .iter()
        .find(|b| !matches!(b, b' ' | b'\t' | b'\x0c' | b'\r'));
    if encoding.is_none() && first_content.is_none_or(|b| *b == b'#') {
        encoding = lines.next().and_then(cookie);
    }
    let original = encoding.unwrap_or_else(|| "utf-8".into());
    let normalized = original.to_ascii_lowercase().replace('_', "-");
    let utf8 = matches!(normalized.as_str(), "utf-8" | "utf8" | "utf-8-sig")
        || normalized.starts_with("utf-8-");
    let latin1 = matches!(
        normalized.as_str(),
        "latin1" | "latin-1" | "iso-8859-1" | "iso8859-1" | "iso-latin-1"
    ) || normalized.starts_with("latin-1-")
        || normalized.starts_with("iso-8859-1-")
        || normalized.starts_with("iso-latin-1-");
    // CPython's BOM agreement check uses normalized "utf-8", not generic
    // codec alias lookup: bare utf8 is accepted without BOM, rejected with BOM.
    if bom && (!utf8 || normalized == "utf8") {
        return Err(DecodeError::Invalid(
            "UTF8 BOM conflicts with declared source encoding",
        ));
    }
    let text = if utf8 {
        std::str::from_utf8(bytes)
            .map_err(|_| DecodeError::Invalid("Invalid UTF8 source"))?
            .to_owned()
    } else if latin1 {
        bytes.iter().map(|b| char::from(*b)).collect()
    } else if matches!(normalized.as_str(), "ascii" | "us-ascii") {
        if !bytes.is_ascii() {
            return Err(DecodeError::Invalid("Non-ASCII byte in ASCII source"));
        }
        String::from_utf8(bytes.to_vec()).expect("ASCII")
    } else if normalized == "koi8-r" {
        decode_single_byte(bytes, &KOI8_R)?
    } else if matches!(normalized.as_str(), "cp1252" | "windows-1252" | "1252") {
        decode_single_byte(bytes, &CP1252)?
    } else if matches!(normalized.as_str(), "uft-8" | "koi8r") {
        return Err(DecodeError::Invalid(
            "Known invalid source encoding declaration",
        ));
    } else {
        return Err(DecodeError::Unsupported(format!(
            "Independent source codec not mapped: {original}"
        )));
    };
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}
fn decode_single_byte(bytes: &[u8], table: &[u32; 128]) -> Result<String, DecodeError> {
    bytes
        .iter()
        .map(|&b| {
            let scalar = if b < 128 {
                u32::from(b)
            } else {
                table[usize::from(b - 128)]
            };
            char::from_u32(scalar).ok_or(DecodeError::Invalid(
                "Undefined byte in declared source codec",
            ))
        })
        .collect()
}
// Generated from primary CPython3.12 koi8-r strict byte decoding.
const KOI8_R: [u32; 128] = [
    0x2500, 0x2502, 0x250c, 0x2510, 0x2514, 0x2518, 0x251c, 0x2524, 0x252c, 0x2534, 0x253c, 0x2580,
    0x2584, 0x2588, 0x258c, 0x2590, 0x2591, 0x2592, 0x2593, 0x2320, 0x25a0, 0x2219, 0x221a, 0x2248,
    0x2264, 0x2265, 0xa0, 0x2321, 0xb0, 0xb2, 0xb7, 0xf7, 0x2550, 0x2551, 0x2552, 0x451, 0x2553,
    0x2554, 0x2555, 0x2556, 0x2557, 0x2558, 0x2559, 0x255a, 0x255b, 0x255c, 0x255d, 0x255e, 0x255f,
    0x2560, 0x2561, 0x401, 0x2562, 0x2563, 0x2564, 0x2565, 0x2566, 0x2567, 0x2568, 0x2569, 0x256a,
    0x256b, 0x256c, 0xa9, 0x44e, 0x430, 0x431, 0x446, 0x434, 0x435, 0x444, 0x433, 0x445, 0x438,
    0x439, 0x43a, 0x43b, 0x43c, 0x43d, 0x43e, 0x43f, 0x44f, 0x440, 0x441, 0x442, 0x443, 0x436,
    0x432, 0x44c, 0x44b, 0x437, 0x448, 0x44d, 0x449, 0x447, 0x44a, 0x42e, 0x410, 0x411, 0x426,
    0x414, 0x415, 0x424, 0x413, 0x425, 0x418, 0x419, 0x41a, 0x41b, 0x41c, 0x41d, 0x41e, 0x41f,
    0x42f, 0x420, 0x421, 0x422, 0x423, 0x416, 0x412, 0x42c, 0x42b, 0x417, 0x428, 0x42d, 0x429,
    0x427, 0x42a,
];
// Generated from primary CPython3.12 cp1252 strict byte decoding.
const CP1252: [u32; 128] = [
    0x20ac,
    0xffff_ffff,
    0x201a,
    0x192,
    0x201e,
    0x2026,
    0x2020,
    0x2021,
    0x2c6,
    0x2030,
    0x160,
    0x2039,
    0x152,
    0xffff_ffff,
    0x17d,
    0xffff_ffff,
    0xffff_ffff,
    0x2018,
    0x2019,
    0x201c,
    0x201d,
    0x2022,
    0x2013,
    0x2014,
    0x2dc,
    0x2122,
    0x161,
    0x203a,
    0x153,
    0xffff_ffff,
    0x17e,
    0x178,
    0xa0,
    0xa1,
    0xa2,
    0xa3,
    0xa4,
    0xa5,
    0xa6,
    0xa7,
    0xa8,
    0xa9,
    0xaa,
    0xab,
    0xac,
    0xad,
    0xae,
    0xaf,
    0xb0,
    0xb1,
    0xb2,
    0xb3,
    0xb4,
    0xb5,
    0xb6,
    0xb7,
    0xb8,
    0xb9,
    0xba,
    0xbb,
    0xbc,
    0xbd,
    0xbe,
    0xbf,
    0xc0,
    0xc1,
    0xc2,
    0xc3,
    0xc4,
    0xc5,
    0xc6,
    0xc7,
    0xc8,
    0xc9,
    0xca,
    0xcb,
    0xcc,
    0xcd,
    0xce,
    0xcf,
    0xd0,
    0xd1,
    0xd2,
    0xd3,
    0xd4,
    0xd5,
    0xd6,
    0xd7,
    0xd8,
    0xd9,
    0xda,
    0xdb,
    0xdc,
    0xdd,
    0xde,
    0xdf,
    0xe0,
    0xe1,
    0xe2,
    0xe3,
    0xe4,
    0xe5,
    0xe6,
    0xe7,
    0xe8,
    0xe9,
    0xea,
    0xeb,
    0xec,
    0xed,
    0xee,
    0xef,
    0xf0,
    0xf1,
    0xf2,
    0xf3,
    0xf4,
    0xf5,
    0xf6,
    0xf7,
    0xf8,
    0xf9,
    0xfa,
    0xfb,
    0xfc,
    0xfd,
    0xfe,
    0xff,
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn primary_cpython_byte_source_goldens() {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("source-bytes-goldens.json")).unwrap();
        for fixture in fixtures["fixtures"].as_array().unwrap() {
            let hex = fixture["bytes_hex"].as_str().unwrap();
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            match decode(&bytes) {
                Ok(text) => {
                    let ast = crate::cpython_ast::export(&text).unwrap();
                    assert_eq!(
                        ast.is_some(),
                        fixture["oracle"]["accepted"].as_bool().unwrap(),
                        "{hex}"
                    );
                    if let Some(ast) = ast {
                        assert_eq!(ast, fixture["oracle"]["ast"], "{hex}");
                    }
                }
                Err(DecodeError::Invalid(_)) => {
                    assert_eq!(fixture["oracle"]["accepted"], false, "{hex}")
                }
                Err(DecodeError::Unsupported(message)) => {
                    panic!("primary fixture unsupported: {message}")
                }
            }
        }
    }
    #[test]
    fn actual_cookie_scope_and_lossless_decoded_semantics() {
        assert_eq!(
            decode(b"# coding: latin-1\nx='\xff'\r\n").unwrap(),
            "# coding: latin-1\nx='ÿ'\n"
        );
        assert_eq!(
            decode(b"#!/usr/bin/python\n# coding=ascii\nx=1\n").unwrap(),
            "#!/usr/bin/python\n# coding=ascii\nx=1\n"
        );
        assert!(matches!(
            decode(b"x=1\n# coding: latin-1\nx='\xff'"),
            Err(DecodeError::Invalid(_))
        ));
        assert!(matches!(
            decode(b"\xef\xbb\xbf# coding: latin-1\nx=1"),
            Err(DecodeError::Invalid(_))
        ));
        assert_eq!(
            decode(b"# coding: cp1252\nx=1").unwrap(),
            "# coding: cp1252\nx=1"
        );
        assert!(matches!(
            decode(b"# coding: cp932\nx=1"),
            Err(DecodeError::Unsupported(_))
        ));
        assert_eq!(
            decode(b"# ordinary coding mention\nx=1\r").unwrap(),
            "# ordinary coding mention\nx=1\n"
        );
    }
}

#[cfg(test)]
mod added_codec_tests {
    use super::*;
    #[test]
    fn strict_full_primary_byte_tables() {
        let tables: serde_json::Value =
            serde_json::from_str(include_str!("source-codec-tables.json")).unwrap();
        for (name, table) in [("koi8-r", &KOI8_R), ("cp1252", &CP1252)] {
            for b in 0u16..256 {
                let expected = &tables["mapping"][name][usize::from(b)];
                let actual = decode_single_byte(&[b as u8], table);
                if expected.is_null() {
                    assert!(matches!(actual, Err(DecodeError::Invalid(_))));
                } else {
                    assert_eq!(
                        actual.unwrap().chars().next().unwrap() as u32,
                        expected.as_u64().unwrap() as u32
                    );
                }
            }
        }
    }
    #[test]
    fn actual_source_header_aliases_conflict_and_undefined_bytes() {
        assert_eq!(
            decode(b"# coding: cp1252\nx='\x80'\r\n").unwrap(),
            "# coding: cp1252\nx='€'\n"
        );
        assert_eq!(
            decode(b"# coding: KOI8_R\nx='\xf0\xd2\xc9\xd7\xc5\xd4'\n").unwrap(),
            "# coding: KOI8_R\nx='Привет'\n"
        );
        for b in [0x81, 0x8d, 0x8f, 0x90, 0x9d] {
            let mut bytes = b"# coding: windows-1252\nx='".to_vec();
            bytes.extend([b, b'\'', b'\n']);
            assert!(matches!(decode(&bytes), Err(DecodeError::Invalid(_))));
        }
        assert!(matches!(
            decode(b"# coding: uft-8\nx=1"),
            Err(DecodeError::Invalid(_))
        ));
        assert!(matches!(
            decode(b"\xef\xbb\xbf# coding: cp1252\nx=1"),
            Err(DecodeError::Invalid(_))
        ));
        assert!(matches!(
            decode(b"# coding: cp932\nx=1"),
            Err(DecodeError::Unsupported(_))
        ));
    }
    #[test]
    fn primary_additional_byte_source_goldens() {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("source-codecs-goldens.json")).unwrap();
        for fixture in fixtures["fixtures"].as_array().unwrap() {
            let bytes: Vec<u8> = fixture["bytes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_u64().unwrap() as u8)
                .collect();
            match decode(&bytes) {
                Ok(text) => {
                    let ast = crate::cpython_ast::export(&text).unwrap();
                    assert_eq!(ast.is_some(), fixture["accepted"].as_bool().unwrap());
                    if let Some(ast) = ast {
                        assert_eq!(ast, fixture["ast"]);
                    }
                }
                Err(DecodeError::Invalid(_)) => assert_eq!(fixture["accepted"], false),
                Err(DecodeError::Unsupported(message)) => {
                    panic!("unexpected unsupported: {message}")
                }
            }
        }
    }
}
