//! Incremental console codecs retain split UTF-8 scalars and UTF-16 surrogates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Ascii,
    Latin1,
    Windows1252,
    Utf16Le,
    Utf16Be,
}
impl Encoding {
    pub fn parse(label: &str) -> Option<Self> {
        match label.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "utf8" | "utf8sig" => Some(Self::Utf8),
            "ascii" | "usascii" => Some(Self::Ascii),
            "latin1" | "iso88591" => Some(Self::Latin1),
            "cp1252" | "windows1252" => Some(Self::Windows1252),
            "utf16le" => Some(Self::Utf16Le),
            "utf16be" => Some(Self::Utf16Be),
            _ => None,
        }
    }
    pub fn encode(self, text: &str) -> Vec<u8> {
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            Self::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            _ => text
                .chars()
                .map(|c| match self {
                    Self::Ascii => {
                        if c.is_ascii() {
                            c as u8
                        } else {
                            b'?'
                        }
                    }
                    Self::Windows1252 => CP1252
                        .iter()
                        .position(|&v| v == c)
                        .map(|i| i as u8 + 0x80)
                        .unwrap_or_else(|| if (c as u32) <= 255 { c as u8 } else { b'?' }),
                    _ => {
                        if (c as u32) <= 255 {
                            c as u8
                        } else {
                            b'?'
                        }
                    }
                })
                .collect(),
        }
    }
}
const CP1252: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
    '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];
pub struct Decoder {
    encoding: Encoding,
    pending: Vec<u8>,
}
impl Decoder {
    pub fn new(encoding: Encoding) -> Self {
        Self {
            encoding,
            pending: Vec::new(),
        }
    }
    pub fn push(&mut self, bytes: &[u8], finish: bool) -> String {
        self.pending.extend_from_slice(bytes);
        match self.encoding {
            Encoding::Utf8 => {
                if finish {
                    String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned()
                } else {
                    crate::process::take_utf8_prefix(&mut self.pending)
                }
            }
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let mut length = self.pending.len() / 2 * 2;
                let unit = |pair: &[u8]| {
                    if self.encoding == Encoding::Utf16Le {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                };
                if !finish
                    && length >= 2
                    && (0xd800..=0xdbff).contains(&unit(&self.pending[length - 2..length]))
                {
                    length -= 2;
                }
                let mut output = String::from_utf16_lossy(
                    &self.pending[..length]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|bytes| unit(bytes))
                        .collect::<Vec<_>>(),
                );
                self.pending.drain(..length);
                if finish && !self.pending.is_empty() {
                    output.push('\u{fffd}');
                    self.pending.clear();
                }
                output
            }
            encoding => std::mem::take(&mut self.pending)
                .into_iter()
                .map(|b| match encoding {
                    Encoding::Ascii => {
                        if b < 128 {
                            b as char
                        } else {
                            '\u{fffd}'
                        }
                    }
                    Encoding::Windows1252 if (0x80..=0x9f).contains(&b) => {
                        CP1252[(b - 0x80) as usize]
                    }
                    _ => b as char,
                })
                .collect(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_scalars_surrogates_and_single_byte_codecs() {
        let mut decoder = Decoder::new(Encoding::Utf8);
        assert_eq!(decoder.push(&[0xf0, 0x9f], false), "");
        assert_eq!(decoder.push(&[0x98, 0x80], false), "😀");
        let mut decoder = Decoder::new(Encoding::Utf16Le);
        assert_eq!(decoder.push(&[0x3d, 0xd8, 0], false), "");
        assert_eq!(decoder.push(&[0xde], false), "😀");
        let mut decoder = Decoder::new(Encoding::Windows1252);
        assert_eq!(decoder.push(&[0x80, 0xe9], true), "€é");
        assert_eq!(Encoding::Latin1.encode("é"), vec![0xe9]);
    }
}
