//! Line endings and character encodings (roadmap §0.2).

use std::fmt;

/// Line separator convention of a file on disk. Documents are always stored
/// with `\n` internally; the original convention is restored on save.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    /// Short label used in the status bar, e.g. `LF`.
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
            LineEnding::Cr => "CR",
        }
    }

    /// The platform default (`CRLF` on Windows, `LF` elsewhere).
    pub fn platform() -> Self {
        if cfg!(windows) {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }

    /// Detects the dominant line ending; returns `(ending, mixed)`.
    pub fn detect(text: &str) -> (Self, bool) {
        let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' => {
                    if bytes.get(i + 1) == Some(&b'\n') {
                        crlf += 1;
                        i += 1;
                    } else {
                        cr += 1;
                    }
                }
                b'\n' => lf += 1,
                _ => {}
            }
            i += 1;
        }
        let kinds = [
            (lf, LineEnding::Lf),
            (crlf, LineEnding::CrLf),
            (cr, LineEnding::Cr),
        ];
        let present = kinds.iter().filter(|(n, _)| *n > 0).count();
        let best = kinds
            .iter()
            .max_by_key(|(n, _)| *n)
            .map(|(_, e)| *e)
            .unwrap_or_else(Self::platform);
        (
            if lf + crlf + cr == 0 {
                Self::platform()
            } else {
                best
            },
            present > 1,
        )
    }

    /// Normalizes every line break in `text` to `\n`.
    pub fn normalize_to_lf(text: &str) -> std::borrow::Cow<'_, str> {
        if !text.contains('\r') {
            return std::borrow::Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\r' {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            } else {
                out.push(c);
            }
        }
        std::borrow::Cow::Owned(out)
    }

    /// Converts LF-normalized text to this convention.
    pub fn apply(self, text: &str) -> std::borrow::Cow<'_, str> {
        match self {
            LineEnding::Lf => std::borrow::Cow::Borrowed(text),
            other => std::borrow::Cow::Owned(text.replace('\n', other.as_str())),
        }
    }
}

impl fmt::Display for LineEnding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Character encoding of a file. Internally everything is UTF-8.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    /// UTF-16 little endian without a byte-order mark.
    Utf16LeNoBom,
    /// UTF-16 big endian without a byte-order mark.
    Utf16BeNoBom,
    Latin1,
}

/// A decoding or serialization failure; no partial text or bytes are returned.
#[derive(Debug, thiserror::Error)]
pub enum EncodingError {
    #[error("invalid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid UTF-16: {0}")]
    Utf16(#[from] std::string::FromUtf16Error),
    #[error("invalid UTF-16: odd byte length")]
    OddUtf16Length,
    #[error("unsupported Python encoding cookie: {0}")]
    UnsupportedCookie(String),
    #[error("Python encoding cookie conflicts with the file encoding")]
    CookieConflict,
    #[error("UTF-16 is supported for text editing but is not a Python source encoding")]
    Utf16PythonSource,
    #[error("encoded bytes would change text or be detected as a different BOM format")]
    ConversionFormatConflict,
    #[error("character {character:?} at UTF-8 byte {offset} cannot be encoded as Latin-1; select UTF-8 and update the encoding cookie")]
    Unrepresentable { character: char, offset: usize },
}

/// Shared frontend policy: Python files must use a Python source encoding.
/// Cython and generic text files keep generic BOM/UTF-16 text support.
pub fn is_python_source_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["py", "pyi", "pyw"]
                .iter()
                .any(|suffix| ext.eq_ignore_ascii_case(suffix))
        })
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Utf16LeNoBom => "UTF-16 LE (no BOM)",
            Encoding::Utf16BeNoBom => "UTF-16 BE (no BOM)",
            Encoding::Latin1 => "ISO-8859-1",
        }
    }

    /// Decodes without replacing malformed declared Unicode. Undeclared
    /// invalid UTF-8 retains the editor's explicit, lossless Latin-1 fallback.
    pub fn decode(bytes: &[u8]) -> Result<(String, Encoding), EncodingError> {
        if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            if python_cookie(rest)?.is_some_and(|encoding| encoding != Encoding::Utf8) {
                return Err(EncodingError::CookieConflict);
            }
            return Ok((std::str::from_utf8(rest)?.to_owned(), Encoding::Utf8Bom));
        }
        for (bom, encoding) in [
            ([0xFF, 0xFE], Encoding::Utf16Le),
            ([0xFE, 0xFF], Encoding::Utf16Be),
        ] {
            if let Some(rest) = bytes.strip_prefix(&bom) {
                if !rest.len().is_multiple_of(2) {
                    return Err(EncodingError::OddUtf16Length);
                }
                let units: Vec<u16> = rest
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| {
                        if encoding == Encoding::Utf16Le {
                            u16::from_le_bytes([c[0], c[1]])
                        } else {
                            u16::from_be_bytes([c[0], c[1]])
                        }
                    })
                    .collect();
                return Ok((String::from_utf16(&units)?, encoding));
            }
        }
        match python_cookie(bytes)? {
            Some(Encoding::Latin1) => {
                return Ok((bytes.iter().map(|&b| b as char).collect(), Encoding::Latin1))
            }
            Some(Encoding::Utf8) => {
                return Ok((std::str::from_utf8(bytes)?.to_owned(), Encoding::Utf8))
            }
            _ => {}
        }
        Ok(match std::str::from_utf8(bytes) {
            Ok(s) => (s.to_owned(), Encoding::Utf8),
            Err(_) => (bytes.iter().map(|&b| b as char).collect(), Encoding::Latin1),
        })
    }

    /// Python source cannot use UTF-16; generic text documents can.
    pub fn decode_python_source(bytes: &[u8]) -> Result<String, EncodingError> {
        let (text, encoding) = Self::decode(bytes)?;
        if matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be) {
            return Err(EncodingError::Utf16PythonSource);
        }
        Ok(text)
    }

    /// Encodes without replacing characters or contradicting a Python cookie.
    pub fn encode(self, text: &str) -> Result<Vec<u8>, EncodingError> {
        if !self.is_utf16() {
            let actual = if self == Encoding::Utf8Bom {
                Encoding::Utf8
            } else {
                self
            };
            if python_cookie(text.as_bytes())?.is_some_and(|declared| declared != actual) {
                return Err(EncodingError::CookieConflict);
            }
        }
        self.encode_raw(text)
    }

    fn encode_raw(self, text: &str) -> Result<Vec<u8>, EncodingError> {
        Ok(match self {
            Encoding::Utf8 => text.as_bytes().to_vec(),
            Encoding::Utf8Bom => {
                let mut v = vec![0xEF, 0xBB, 0xBF];
                v.extend_from_slice(text.as_bytes());
                v
            }
            Encoding::Utf16Le
            | Encoding::Utf16Be
            | Encoding::Utf16LeNoBom
            | Encoding::Utf16BeNoBom => {
                let mut v = match self {
                    Encoding::Utf16Le => vec![0xFF, 0xFE],
                    Encoding::Utf16Be => vec![0xFE, 0xFF],
                    _ => Vec::new(),
                };
                for u in text.encode_utf16() {
                    v.extend_from_slice(&if matches!(
                        self,
                        Encoding::Utf16Le | Encoding::Utf16LeNoBom
                    ) {
                        u.to_le_bytes()
                    } else {
                        u.to_be_bytes()
                    });
                }
                v
            }
            Encoding::Latin1 => {
                let mut bytes = Vec::with_capacity(text.len());
                for (offset, c) in text.char_indices() {
                    if c as u32 > 255 {
                        return Err(EncodingError::Unrepresentable {
                            character: c,
                            offset,
                        });
                    }
                    bytes.push(c as u8);
                }
                bytes
            }
        })
    }
}

/// Text/source interpretation is explicit; a generic comment is never a codec declaration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourcePolicy {
    Generic,
    Python,
}
impl SourcePolicy {
    pub fn for_path(path: &std::path::Path) -> Self {
        if is_python_source_path(path) {
            Self::Python
        } else {
            Self::Generic
        }
    }
}

/// A complete, validated conversion candidate. Callers may inspect it before
/// changing document text, encoding, undo state, or disk bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodingConversion {
    pub text: String,
    pub bytes: Vec<u8>,
    pub encoding: Encoding,
    /// A UTF-8 BOM before a shebang can affect direct script execution.
    pub affects_shebang: bool,
}

impl Encoding {
    pub fn key(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf8Bom => "utf-8-bom",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
            Self::Utf16LeNoBom => "utf-16le-no-bom",
            Self::Utf16BeNoBom => "utf-16be-no-bom",
            Self::Latin1 => "latin1",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "utf-8" => Some(Self::Utf8),
            "utf-8-bom" => Some(Self::Utf8Bom),
            "utf-16le" => Some(Self::Utf16Le),
            "utf-16be" => Some(Self::Utf16Be),
            "utf-16le-no-bom" => Some(Self::Utf16LeNoBom),
            "utf-16be-no-bom" => Some(Self::Utf16BeNoBom),
            "latin1" => Some(Self::Latin1),
            _ => None,
        }
    }
    fn is_utf16(self) -> bool {
        matches!(
            self,
            Self::Utf16Le | Self::Utf16Be | Self::Utf16LeNoBom | Self::Utf16BeNoBom
        )
    }
    fn source_codec(self) -> Self {
        if self == Self::Utf8Bom {
            Self::Utf8
        } else {
            self
        }
    }

    /// BOMs select the actual format before an explicit codec. Without a BOM,
    /// `selected` is authoritative; UTF-16 selections retain BOMless format.
    /// Generic automatic detection keeps the lossless Latin-1 fallback. Python
    /// automatic decoding defaults strictly to UTF-8 and honors only its first
    /// authoritative cookie; no malformed Unicode is replaced or silently retried.
    pub fn decode_with_policy(
        bytes: &[u8],
        policy: SourcePolicy,
        selected: Option<Self>,
    ) -> Result<(String, Self), EncodingError> {
        if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
            if policy == SourcePolicy::Python
                && python_cookie(rest)?.is_some_and(|codec| codec != Self::Utf8)
            {
                return Err(EncodingError::CookieConflict);
            }
            return Ok((std::str::from_utf8(rest)?.to_owned(), Self::Utf8Bom));
        }
        for (bom, encoding) in [([0xff, 0xfe], Self::Utf16Le), ([0xfe, 0xff], Self::Utf16Be)] {
            if let Some(rest) = bytes.strip_prefix(&bom) {
                if policy == SourcePolicy::Python {
                    return Err(EncodingError::Utf16PythonSource);
                }
                return Ok((decode_utf16(rest, encoding == Self::Utf16Le)?, encoding));
            }
        }
        let codec = if policy == SourcePolicy::Python {
            if selected.is_some_and(Self::is_utf16) {
                return Err(EncodingError::Utf16PythonSource);
            }
            let declared = python_cookie(bytes)?;
            if let (Some(selected), Some(declared)) = (selected, declared) {
                if selected.source_codec() != declared {
                    return Err(EncodingError::CookieConflict);
                }
            }
            selected.or(declared).unwrap_or(Self::Utf8)
        } else if let Some(selected) = selected {
            selected
        } else {
            return Ok(match std::str::from_utf8(bytes) {
                Ok(text) => (text.to_owned(), Self::Utf8),
                Err(_) => (latin1_text(bytes), Self::Latin1),
            });
        };
        Ok(match codec {
            Self::Utf8 | Self::Utf8Bom => (std::str::from_utf8(bytes)?.to_owned(), Self::Utf8),
            Self::Latin1 => (latin1_text(bytes), Self::Latin1),
            Self::Utf16Le | Self::Utf16LeNoBom => (decode_utf16(bytes, true)?, Self::Utf16LeNoBom),
            Self::Utf16Be | Self::Utf16BeNoBom => (decode_utf16(bytes, false)?, Self::Utf16BeNoBom),
        })
    }
    pub fn decode_for_path(
        bytes: &[u8],
        path: &std::path::Path,
        selected: Option<Self>,
    ) -> Result<(String, Self), EncodingError> {
        Self::decode_with_policy(bytes, SourcePolicy::for_path(path), selected)
    }

    /// Validates source policy and representability before returning any bytes.
    /// Generic serialization ignores cookie-looking text. Python requires a
    /// matching cookie for Latin-1 because its undeclared default is UTF-8.
    pub fn encode_with_policy(
        self,
        text: &str,
        policy: SourcePolicy,
    ) -> Result<Vec<u8>, EncodingError> {
        if policy == SourcePolicy::Python {
            if self.is_utf16() {
                return Err(EncodingError::Utf16PythonSource);
            }
            let declared = python_cookie(text.as_bytes())?;
            if declared.unwrap_or(Self::Utf8) != self.source_codec() {
                return Err(EncodingError::CookieConflict);
            }
        }
        let bytes = self.encode_raw(text)?;
        let (decoded, actual) = Self::decode_with_policy(&bytes, policy, Some(self))?;
        if decoded != text || actual != self {
            return Err(EncodingError::ConversionFormatConflict);
        }
        Ok(bytes)
    }

    /// Explicit conversion repairs the authoritative cookie lexically, including
    /// an unsupported cookie. Ignored later cookies are left untouched. Existing
    /// separators and shebang remain intact; a required new cookie uses the
    /// first existing separator (the platform convention if none is present).
    pub fn plan_conversion(
        self,
        text: &str,
        policy: SourcePolicy,
    ) -> Result<EncodingConversion, EncodingError> {
        let mut proposed = text.to_owned();
        if policy == SourcePolicy::Python {
            if self.is_utf16() {
                return Err(EncodingError::Utf16PythonSource);
            }
            let name = if self.source_codec() == Self::Latin1 {
                "latin-1"
            } else {
                "utf-8"
            };
            if let Some(range) = cookie_name_range(text.as_bytes()) {
                proposed.replace_range(range, name);
            } else if self == Self::Latin1 {
                let (first, _, next) = physical_line(text.as_bytes());
                let separator = if next > first.len() {
                    &text[first.len()..next]
                } else {
                    LineEnding::platform().as_str()
                };
                let cookie = format!("# coding: {name}{separator}");
                if text.starts_with("#!") {
                    if next == first.len() {
                        proposed.push_str(separator);
                    }
                    proposed.insert_str(
                        if next == first.len() {
                            proposed.len()
                        } else {
                            next
                        },
                        &cookie,
                    );
                } else {
                    proposed.insert_str(0, &cookie);
                }
            }
        }
        let bytes = self.encode_with_policy(&proposed, policy)?;
        Ok(EncodingConversion {
            affects_shebang: self == Self::Utf8Bom && proposed.starts_with("#!"),
            text: proposed,
            bytes,
            encoding: self,
        })
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, EncodingError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(EncodingError::OddUtf16Length);
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            if little_endian {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    Ok(String::from_utf16(&units)?)
}
fn latin1_text(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

// PEP 263's first authoritative comment cookie, before validating its name.
// Returning a lexical span permits explicit conversion to repair an unsupported
// declaration without making automatic decode silently accept it.
fn cookie_name_range(bytes: &[u8]) -> Option<std::ops::Range<usize>> {
    fn line_cookie(line: &[u8]) -> Option<std::ops::Range<usize>> {
        let start = line
            .iter()
            .position(|b| !matches!(b, b' ' | b'\t' | 0x0c))?;
        if line[start] != b'#' {
            return None;
        }
        let comment = &line[start + 1..];
        for i in 0..comment.len().saturating_sub(6) {
            if &comment[i..i + 6] != b"coding" || !matches!(comment[i + 6], b':' | b'=') {
                continue;
            }
            let rest = &comment[i + 7..];
            let spaces = rest
                .iter()
                .position(|b| !matches!(b, b' ' | b'\t'))
                .unwrap_or(rest.len());
            let name = &rest[spaces..];
            let end = name
                .iter()
                .position(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'-' | b'_' | b'.'))
                .unwrap_or(name.len());
            if end > 0 {
                let begin = start + 1 + i + 7 + spaces;
                return Some(begin..begin + end);
            }
        }
        None
    }
    let (first, rest, next) = physical_line(bytes);
    if let Some(range) = line_cookie(first) {
        return Some(range);
    }
    let first = first
        .iter()
        .copied()
        .find(|b| !matches!(b, b' ' | b'\t' | 0x0c));
    if !matches!(first, None | Some(b'#')) {
        return None;
    }
    line_cookie(physical_line(rest).0).map(|range| range.start + next..range.end + next)
}
fn physical_line(bytes: &[u8]) -> (&[u8], &[u8], usize) {
    let end = bytes
        .iter()
        .position(|b| matches!(b, b'\r' | b'\n'))
        .unwrap_or(bytes.len());
    let separator = if bytes.get(end) == Some(&b'\r') && bytes.get(end + 1) == Some(&b'\n') {
        2
    } else {
        usize::from(end < bytes.len())
    };
    (&bytes[..end], &bytes[end + separator..], end + separator)
}
fn python_cookie(bytes: &[u8]) -> Result<Option<Encoding>, EncodingError> {
    let Some(range) = cookie_name_range(bytes) else {
        return Ok(None);
    };
    let raw = &bytes[range];
    let name = std::str::from_utf8(raw)
        .expect("cookie names contain only ASCII")
        .to_ascii_lowercase()
        .replace('_', "-");
    let prefix: String = name.chars().take(12).collect();
    if matches!(name.as_str(), "utf8" | "utf-8" | "u8" | "utf") || prefix.starts_with("utf-8-") {
        Ok(Some(Encoding::Utf8))
    } else if matches!(
        name.as_str(),
        "latin1"
            | "latin-1"
            | "latin"
            | "l1"
            | "iso8859-1"
            | "iso-8859-1"
            | "iso-latin-1"
            | "cp819"
            | "ibm819"
            | "8859"
    ) || ["latin-1-", "iso-8859-1-", "iso-latin-1-"]
        .iter()
        .any(|p| prefix.starts_with(p))
    {
        Ok(Some(Encoding::Latin1))
    } else {
        Err(EncodingError::UnsupportedCookie(
            String::from_utf8_lossy(raw).into_owned(),
        ))
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_line_endings() {
        assert_eq!(LineEnding::detect("a\nb\n"), (LineEnding::Lf, false));
        assert_eq!(LineEnding::detect("a\r\nb\r\n"), (LineEnding::CrLf, false));
        assert_eq!(LineEnding::detect("a\r\nb\n"), (LineEnding::CrLf, true));
        assert_eq!(LineEnding::detect("a\rb\r"), (LineEnding::Cr, false));
    }

    #[test]
    fn normalizes_and_restores() {
        let n = LineEnding::normalize_to_lf("a\r\nb\rc\n");
        assert_eq!(n, "a\nb\nc\n");
        assert_eq!(LineEnding::CrLf.apply(&n), "a\r\nb\r\nc\r\n");
    }

    #[test]
    fn python_cookies_precede_utf8_detection() {
        for alias in [
            "latin-1",
            "LATIN_1",
            "iso-8859-1",
            "iso_latin_1",
            "latin1",
            "l1",
            "cp819",
            "latin-1-extra",
        ] {
            let bytes = format!("# coding: {alias}\ns = \"é\"\n").into_bytes();
            let (text, encoding) = Encoding::decode(&bytes).unwrap();
            assert_eq!(encoding, Encoding::Latin1, "{alias}");
            assert!(text.contains("Ã©"), "{alias}: {text}");
            assert_eq!(encoding.encode(&text).unwrap(), bytes);
        }
        for alias in ["utf-8", "UTF_8", "utf8", "utf-8-extra"] {
            let bytes = format!("# coding={alias}\ns = \"é\"\n");
            assert_eq!(
                Encoding::decode(bytes.as_bytes()).unwrap(),
                (bytes, Encoding::Utf8)
            );
        }
    }

    #[test]
    fn cookie_lines_follow_python_comment_boundaries() {
        for prefix in [
            "",
            "#!/usr/bin/python\n",
            "# comment\n",
            " \t\x0c\n",
            "\r\n",
        ] {
            let bytes = format!("{prefix} \t\x0c# -*- coding: latin-1 -*-\r\ns = \"é\"\r\n");
            assert_eq!(
                Encoding::decode(bytes.as_bytes()).unwrap().1,
                Encoding::Latin1,
                "{prefix:?}"
            );
        }
        for prefix in ["x = 1\n", "# comment\n# another\n", "# coding: utf-8\n"] {
            let bytes = format!("{prefix}# coding: latin-1\ns = \"é\"\n");
            assert_eq!(
                Encoding::decode(bytes.as_bytes()).unwrap().1,
                Encoding::Utf8,
                "{prefix:?}"
            );
        }
        for comment in [
            "# Coding: latin-1",
            "# coding : latin-1",
            "x = '# coding: latin-1'",
            "# coding: ",
        ] {
            let bytes = format!("{comment}\ns = \"é\"\n");
            assert_eq!(
                Encoding::decode(bytes.as_bytes()).unwrap().1,
                Encoding::Utf8,
                "{comment}"
            );
        }
    }

    #[test]
    fn cookies_do_not_change_bom_decoding() {
        let text = "# coding: utf-8\ns = \"😀é\"\r\n";
        for encoding in [Encoding::Utf8Bom, Encoding::Utf16Le, Encoding::Utf16Be] {
            let bytes = encoding.encode(text).unwrap();
            assert_eq!(Encoding::decode(&bytes).unwrap(), (text.into(), encoding));
        }
    }

    #[test]
    fn malformed_declared_unicode_and_cookie_errors_are_not_replaced() {
        for bytes in [
            b"\xef\xbb\xbf\xff".as_slice(),
            b"# coding: utf-8\n\xff",
            b"\xff\xfea",
            b"\xff\xfe\x00\xd8",
            b"\xfe\xff\xdc\x00",
            b"# coding: unknown-codec\n",
            b"# coding: latin-1.extra\n",
            b"\xef\xbb\xbf# coding: latin-1\nx=1",
        ] {
            assert!(Encoding::decode(bytes).is_err(), "{bytes:?}");
        }
        assert!(matches!(
            Encoding::Latin1.encode("é😀"),
            Err(EncodingError::Unrepresentable {
                character: '😀',
                ..
            })
        ));
        assert!(Encoding::Utf8.encode("# coding: latin-1\nx=1").is_err());
        assert_eq!(
            Encoding::decode(b"\xff").unwrap(),
            ("ÿ".into(), Encoding::Latin1)
        );
        let bytes = Encoding::Utf16Le.encode("x=1").unwrap();
        assert!(Encoding::decode(&bytes).is_ok());
        assert!(matches!(
            Encoding::decode_python_source(&bytes),
            Err(EncodingError::Utf16PythonSource)
        ));
    }

    #[test]
    fn decodes_boms() {
        assert_eq!(
            Encoding::decode(&[0xEF, 0xBB, 0xBF, b'h', b'i']).unwrap(),
            ("hi".into(), Encoding::Utf8Bom)
        );
        assert_eq!(
            Encoding::decode(&[0xFF, 0xFE, b'h', 0, b'i', 0]).unwrap(),
            ("hi".into(), Encoding::Utf16Le)
        );
        assert_eq!(
            Encoding::decode(&[0xE9]).unwrap(),
            ("é".into(), Encoding::Latin1)
        );
        assert_eq!(Encoding::Latin1.encode("é").unwrap(), vec![0xE9]);
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn generic_cookie_looking_comments_do_not_control_decoding_or_serialization() {
        for cookie in ["latin-1", "unknown-codec", "utf-8"] {
            let text = format!("# coding: {cookie}\né\r\n");
            assert_eq!(
                Encoding::decode_for_path(text.as_bytes(), Path::new("notes.txt"), None).unwrap(),
                (text.clone(), Encoding::Utf8)
            );
            assert_eq!(
                Encoding::Utf8
                    .encode_with_policy(&text, SourcePolicy::Generic)
                    .unwrap(),
                text.as_bytes()
            );
            assert_eq!(
                Encoding::Latin1
                    .encode_with_policy(&text, SourcePolicy::Generic)
                    .unwrap()
                    .last(),
                Some(&b'\n')
            );
        }
        assert_eq!(
            Encoding::decode_with_policy(b"# coding: utf-8\n\xff", SourcePolicy::Generic, None)
                .unwrap()
                .1,
            Encoding::Latin1
        );
    }

    #[test]
    fn selected_latin1_reinterprets_valid_utf8_without_touching_input() {
        let bytes = "café".as_bytes();
        let (text, actual) =
            Encoding::decode_for_path(bytes, Path::new("notes.txt"), Some(Encoding::Latin1))
                .unwrap();
        assert_eq!(text, "cafÃ©");
        assert_eq!(actual, Encoding::Latin1);
        assert_eq!(
            actual
                .encode_with_policy(&text, SourcePolicy::Generic)
                .unwrap(),
            bytes
        );
        assert_eq!(
            Encoding::decode_for_path(bytes, Path::new("notes.txt"), Some(Encoding::Utf8))
                .unwrap()
                .0,
            "café"
        );
    }

    #[test]
    fn python_default_is_strict_utf8_and_selected_codec_must_match_cookie() {
        assert!(matches!(
            Encoding::decode_for_path(b"\xff", Path::new("main.py"), None),
            Err(EncodingError::Utf8(_))
        ));
        assert!(matches!(
            Encoding::decode_for_path(b"# coding: unavailable\nx=1", Path::new("main.pyi"), None),
            Err(EncodingError::UnsupportedCookie(_))
        ));
        let latin = b"# coding: latin-1\nname='\xe9'";
        assert_eq!(
            Encoding::decode_for_path(latin, Path::new("MAIN.PYW"), None)
                .unwrap()
                .1,
            Encoding::Latin1
        );
        assert!(matches!(
            Encoding::decode_for_path(latin, Path::new("main.py"), Some(Encoding::Utf8)),
            Err(EncodingError::CookieConflict)
        ));
        assert_eq!(
            SourcePolicy::for_path(Path::new("main.pyx")),
            SourcePolicy::Generic
        );
    }

    #[test]
    fn bom_wins_selected_codec_but_python_cookie_conflicts_are_strict() {
        let bytes = Encoding::Utf8Bom
            .encode_with_policy("é", SourcePolicy::Generic)
            .unwrap();
        for selected in [Encoding::Latin1, Encoding::Utf16LeNoBom, Encoding::Utf8] {
            assert_eq!(
                Encoding::decode_with_policy(&bytes, SourcePolicy::Generic, Some(selected))
                    .unwrap(),
                ("é".into(), Encoding::Utf8Bom)
            );
            assert_eq!(
                Encoding::decode_with_policy(&bytes, SourcePolicy::Python, Some(selected)).unwrap(),
                ("é".into(), Encoding::Utf8Bom)
            );
        }
        let conflict = b"\xef\xbb\xbf# coding: latin-1\nx=1";
        assert!(matches!(
            Encoding::decode_with_policy(conflict, SourcePolicy::Python, None),
            Err(EncodingError::CookieConflict)
        ));
        assert_eq!(
            Encoding::decode_with_policy(conflict, SourcePolicy::Generic, None)
                .unwrap()
                .1,
            Encoding::Utf8Bom
        );
        assert!(matches!(
            Encoding::decode_with_policy(
                b"\xef\xbb\xbf\xff",
                SourcePolicy::Generic,
                Some(Encoding::Latin1)
            ),
            Err(EncodingError::Utf8(_))
        ));
    }

    #[test]
    fn all_utf16_formats_round_trip_with_actual_bom_presence() {
        let text = "# coding: unavailable\r\nπ😀é\r\n";
        for encoding in [
            Encoding::Utf16Le,
            Encoding::Utf16Be,
            Encoding::Utf16LeNoBom,
            Encoding::Utf16BeNoBom,
        ] {
            let bytes = encoding
                .encode_with_policy(text, SourcePolicy::Generic)
                .unwrap();
            assert_eq!(
                Encoding::decode_with_policy(&bytes, SourcePolicy::Generic, Some(encoding))
                    .unwrap(),
                (text.into(), encoding)
            );
            assert_eq!(
                bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]),
                matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be)
            );
            assert!(matches!(
                Encoding::decode_with_policy(&bytes, SourcePolicy::Python, Some(encoding)),
                Err(EncodingError::Utf16PythonSource)
            ));
            assert!(matches!(
                encoding.encode_with_policy(text, SourcePolicy::Python),
                Err(EncodingError::Utf16PythonSource)
            ));
            assert!(matches!(
                encoding.plan_conversion(text, SourcePolicy::Python),
                Err(EncodingError::Utf16PythonSource)
            ));
        }
    }

    #[test]
    fn selected_utf16_without_bom_retains_bomless_format_and_detected_bom_wins() {
        for (bom, no_bom, other) in [
            (
                Encoding::Utf16Le,
                Encoding::Utf16LeNoBom,
                Encoding::Utf16BeNoBom,
            ),
            (
                Encoding::Utf16Be,
                Encoding::Utf16BeNoBom,
                Encoding::Utf16LeNoBom,
            ),
        ] {
            let bytes = no_bom
                .encode_with_policy("π😀", SourcePolicy::Generic)
                .unwrap();
            assert_eq!(
                Encoding::decode_with_policy(&bytes, SourcePolicy::Generic, Some(bom)).unwrap(),
                ("π😀".into(), no_bom)
            );
            let bytes = bom
                .encode_with_policy("π😀", SourcePolicy::Generic)
                .unwrap();
            assert_eq!(
                Encoding::decode_with_policy(&bytes, SourcePolicy::Generic, Some(other)).unwrap(),
                ("π😀".into(), bom)
            );
        }
    }

    #[test]
    fn selected_malformed_unicode_never_uses_lossy_or_latin1_retry() {
        assert!(matches!(
            Encoding::decode_with_policy(b"\xff", SourcePolicy::Generic, Some(Encoding::Utf8)),
            Err(EncodingError::Utf8(_))
        ));
        for codec in [Encoding::Utf16LeNoBom, Encoding::Utf16BeNoBom] {
            assert!(matches!(
                Encoding::decode_with_policy(b"x", SourcePolicy::Generic, Some(codec)),
                Err(EncodingError::OddUtf16Length)
            ));
        }
        assert!(matches!(
            Encoding::decode_with_policy(
                &[0, 0xd8],
                SourcePolicy::Generic,
                Some(Encoding::Utf16LeNoBom)
            ),
            Err(EncodingError::Utf16(_))
        ));
        assert!(matches!(
            Encoding::decode_with_policy(
                &[0xdc, 0],
                SourcePolicy::Generic,
                Some(Encoding::Utf16BeNoBom)
            ),
            Err(EncodingError::Utf16(_))
        ));
    }

    #[test]
    fn python_encode_validates_cookie_default_bom_and_representability() {
        assert!(matches!(
            Encoding::Latin1.encode_with_policy("é", SourcePolicy::Python),
            Err(EncodingError::CookieConflict)
        ));
        assert!(matches!(
            Encoding::Utf8Bom.encode_with_policy("# coding: latin-1\nx=1", SourcePolicy::Python),
            Err(EncodingError::CookieConflict)
        ));
        assert!(matches!(
            Encoding::Utf8.encode_with_policy("# coding: unsupported\nx=1", SourcePolicy::Python),
            Err(EncodingError::UnsupportedCookie(_))
        ));
        assert!(matches!(
            Encoding::Latin1.encode_with_policy("# coding: latin-1\né😀", SourcePolicy::Python),
            Err(EncodingError::Unrepresentable {
                character: '😀',
                ..
            })
        ));
        assert!(matches!(
            Encoding::Latin1.encode_with_policy("é😀", SourcePolicy::Generic),
            Err(EncodingError::Unrepresentable {
                character: '😀',
                offset: 2
            })
        ));
    }

    #[test]
    fn conversion_repairs_only_authoritative_cookie_even_if_unsupported() {
        let original = "# -*- coding: unavailable -*-\r\n# coding: latin-1\r\nx='😀'";
        let plan = Encoding::Utf8
            .plan_conversion(original, SourcePolicy::Python)
            .unwrap();
        assert_eq!(
            plan.text,
            "# -*- coding: utf-8 -*-\r\n# coding: latin-1\r\nx='😀'"
        );
        assert_eq!(plan.bytes, plan.text.as_bytes());
        assert_eq!(
            Encoding::decode_with_policy(&plan.bytes, SourcePolicy::Python, None)
                .unwrap()
                .0,
            plan.text
        );
        assert_eq!(
            Encoding::Utf8
                .plan_conversion(&plan.text, SourcePolicy::Python)
                .unwrap(),
            plan
        );
        assert_eq!(
            original,
            "# -*- coding: unavailable -*-\r\n# coding: latin-1\r\nx='😀'"
        );
    }

    #[test]
    fn conversion_inserts_required_cookie_after_shebang_preserving_each_separator() {
        for separator in ["\n", "\r\n", "\r"] {
            let original = format!("#!/usr/bin/python{separator}name='é'");
            let plan = Encoding::Latin1
                .plan_conversion(&original, SourcePolicy::Python)
                .unwrap();
            assert_eq!(
                plan.text,
                format!("#!/usr/bin/python{separator}# coding: latin-1{separator}name='é'")
            );
            assert_eq!(
                Encoding::decode_with_policy(&plan.bytes, SourcePolicy::Python, None).unwrap(),
                (plan.text.clone(), Encoding::Latin1)
            );
            assert!(!plan.affects_shebang);
        }
    }

    #[test]
    fn conversion_updates_second_cookie_and_preserves_mixed_endings_and_final_eof() {
        let original = "#!/bin/python\r\n# coding=latin_1\ré\n# end";
        let plan = Encoding::Utf8Bom
            .plan_conversion(original, SourcePolicy::Python)
            .unwrap();
        assert_eq!(plan.text, "#!/bin/python\r\n# coding=utf-8\ré\n# end");
        assert!(plan.affects_shebang);
        assert_eq!(&plan.bytes[..3], &[0xef, 0xbb, 0xbf]);
        assert_eq!(
            Encoding::decode_with_policy(&plan.bytes, SourcePolicy::Python, None)
                .unwrap()
                .0,
            plan.text
        );
    }

    #[test]
    fn ignored_cookie_after_code_does_not_prevent_new_authoritative_latin1_cookie() {
        let original = "x='é'\n# coding: unavailable\n";
        let plan = Encoding::Latin1
            .plan_conversion(original, SourcePolicy::Python)
            .unwrap();
        assert_eq!(
            plan.text,
            "# coding: latin-1\nx='é'\n# coding: unavailable\n"
        );
        assert_eq!(
            Encoding::decode_with_policy(&plan.bytes, SourcePolicy::Python, None)
                .unwrap()
                .0,
            plan.text
        );
    }

    #[test]
    fn generic_conversion_never_changes_comments_and_python_conversion_errors_are_pure() {
        let original = "# coding: unavailable\r\nπ😀";
        let plan = Encoding::Utf16BeNoBom
            .plan_conversion(original, SourcePolicy::Generic)
            .unwrap();
        assert_eq!(plan.text, original);
        assert_eq!(plan.encoding, Encoding::Utf16BeNoBom);
        assert!(!plan.affects_shebang);
        assert_eq!(
            Encoding::decode_with_policy(&plan.bytes, SourcePolicy::Generic, Some(plan.encoding))
                .unwrap()
                .0,
            original
        );
        assert!(matches!(
            Encoding::Latin1.plan_conversion(original, SourcePolicy::Python),
            Err(EncodingError::Unrepresentable { .. })
        ));
        assert_eq!(original, "# coding: unavailable\r\nπ😀");
    }

    #[test]
    fn shebang_warning_is_exactly_utf8_bom_conversion_without_rejecting_it() {
        for policy in [SourcePolicy::Generic, SourcePolicy::Python] {
            assert!(
                Encoding::Utf8Bom
                    .plan_conversion("#!/bin/python\nx=1", policy)
                    .unwrap()
                    .affects_shebang
            );
            assert!(
                !Encoding::Utf8
                    .plan_conversion("#!/bin/python\nx=1", policy)
                    .unwrap()
                    .affects_shebang
            );
            assert!(
                !Encoding::Utf8Bom
                    .plan_conversion("# comment\nx=1", policy)
                    .unwrap()
                    .affects_shebang
            );
        }
    }

    #[test]
    fn canonical_keys_and_legacy_enum_names_remain_distinct() {
        for (encoding, key, legacy_name) in [
            (Encoding::Utf8, "utf-8", "Utf8"),
            (Encoding::Utf8Bom, "utf-8-bom", "Utf8Bom"),
            (Encoding::Utf16Le, "utf-16le", "Utf16Le"),
            (Encoding::Utf16Be, "utf-16be", "Utf16Be"),
            (Encoding::Utf16LeNoBom, "utf-16le-no-bom", "Utf16LeNoBom"),
            (Encoding::Utf16BeNoBom, "utf-16be-no-bom", "Utf16BeNoBom"),
            (Encoding::Latin1, "latin1", "Latin1"),
        ] {
            assert_eq!(encoding.key(), key);
            assert_eq!(Encoding::from_key(key), Some(encoding));
            let name =
                serde::de::value::StrDeserializer::<serde::de::value::Error>::new(legacy_name);
            assert_eq!(
                <Encoding as serde::Deserialize>::deserialize(name).unwrap(),
                encoding
            );
        }
        assert_eq!(Encoding::from_key("UTF-8"), None);
        assert_eq!(Encoding::from_key("unknown"), None);
    }
}

#[cfg(test)]
mod conversion_format_tests {
    use super::*;

    #[test]
    fn conversion_refuses_leading_content_that_would_be_consumed_as_a_bom() {
        for encoding in [
            Encoding::Utf8,
            Encoding::Utf16LeNoBom,
            Encoding::Utf16BeNoBom,
        ] {
            assert!(matches!(
                encoding.plan_conversion("\u{feff}body", SourcePolicy::Generic),
                Err(EncodingError::ConversionFormatConflict)
            ));
            assert!(matches!(
                encoding.encode_with_policy("\u{feff}body", SourcePolicy::Generic),
                Err(EncodingError::ConversionFormatConflict)
            ));
        }
        assert!(matches!(
            Encoding::Latin1.plan_conversion("ï»¿body", SourcePolicy::Generic),
            Err(EncodingError::ConversionFormatConflict)
        ));
        assert!(matches!(
            Encoding::Latin1.encode_with_policy("ï»¿body", SourcePolicy::Generic),
            Err(EncodingError::ConversionFormatConflict)
        ));
        let bom = Encoding::Utf8Bom
            .plan_conversion("\u{feff}body", SourcePolicy::Generic)
            .unwrap();
        assert_eq!(
            Encoding::decode_with_policy(&bom.bytes, SourcePolicy::Generic, None)
                .unwrap()
                .0,
            bom.text
        );
    }

    #[test]
    fn conversion_validates_exact_requested_bomless_and_bom_formats_before_returning() {
        for encoding in [
            Encoding::Utf8,
            Encoding::Utf8Bom,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
            Encoding::Utf16LeNoBom,
            Encoding::Utf16BeNoBom,
            Encoding::Latin1,
        ] {
            for text in ["", "body\r\né"] {
                let plan = encoding
                    .plan_conversion(text, SourcePolicy::Generic)
                    .unwrap();
                assert_eq!(
                    Encoding::decode_with_policy(
                        &plan.bytes,
                        SourcePolicy::Generic,
                        Some(encoding)
                    )
                    .unwrap(),
                    (text.into(), encoding)
                );
            }
        }
    }
}
