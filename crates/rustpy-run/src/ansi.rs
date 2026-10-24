//! Minimal ANSI SGR parsing for console output (roadmap §3.2 "ANSI colors").

/// A run of text with uniform styling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    /// 0-15 standard colors, 16-255 palette; `None` = default.
    pub fg: Option<u8>,
    pub fg_rgb: Option<[u8; 3]>,
    pub bg: Option<u8>,
    pub bg_rgb: Option<[u8; 3]>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SgrState {
    pub fg: Option<u8>,
    pub fg_rgb: Option<[u8; 3]>,
    pub bg: Option<u8>,
    pub bg_rgb: Option<[u8; 3]>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// Splits `line` into styled spans, updating `state` across calls.
pub fn parse_line(line: &str, state: &mut SgrState) -> Vec<StyledSpan> {
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    let flush = |cur: &mut String, spans: &mut Vec<StyledSpan>, st: &SgrState| {
        if !cur.is_empty() {
            spans.push(StyledSpan {
                text: std::mem::take(cur),
                fg: st.fg,
                fg_rgb: st.fg_rgb,
                bg: st.bg,
                bg_rgb: st.bg_rgb,
                bold: st.bold,
                italic: st.italic,
                underline: st.underline,
            });
        }
    };
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                let mut final_byte = None;
                for n in chars.by_ref() {
                    if n.is_ascii_digit() || n == ';' || n == '?' {
                        params.push(n);
                    } else {
                        final_byte = Some(n);
                        break;
                    }
                }
                if final_byte == Some('m') {
                    flush(&mut cur, &mut spans, state);
                    apply_sgr(&params, state);
                }
                // Other CSI sequences (cursor movement, erase) are dropped.
                continue;
            }
            if chars.peek() == Some(&']') {
                // OSC: skip until BEL or ST.
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if n == '\x07' || (prev == '\x1b' && n == '\\') {
                        break;
                    }
                    prev = n;
                }
                continue;
            }
            continue;
        }
        if c == '\r' {
            continue;
        }
        cur.push(c);
    }
    flush(&mut cur, &mut spans, state);
    spans
}

pub(crate) fn apply_sgr(params: &str, st: &mut SgrState) {
    let nums: Vec<u32> = if params.is_empty() {
        vec![0]
    } else {
        params.split(';').filter_map(|p| p.parse().ok()).collect()
    };
    let mut i = 0;
    while i < nums.len() {
        match nums[i] {
            0 => *st = SgrState::default(),
            1 => st.bold = true,
            3 => st.italic = true,
            4 => st.underline = true,
            22 => st.bold = false,
            23 => st.italic = false,
            24 => st.underline = false,
            30..=37 => {
                st.fg = Some((nums[i] - 30) as u8);
                st.fg_rgb = None;
            }
            90..=97 => {
                st.fg = Some((nums[i] - 90 + 8) as u8);
                st.fg_rgb = None;
            }
            39 => {
                st.fg = None;
                st.fg_rgb = None;
            }
            40..=47 => {
                st.bg = Some((nums[i] - 40) as u8);
                st.bg_rgb = None;
            }
            100..=107 => {
                st.bg = Some((nums[i] - 100 + 8) as u8);
                st.bg_rgb = None;
            }
            49 => {
                st.bg = None;
                st.bg_rgb = None;
            }
            38 | 48 => {
                let is_fg = nums[i] == 38;
                if nums.get(i + 1) == Some(&5) {
                    if let Some(&c) = nums.get(i + 2) {
                        let v = Some(c.min(255) as u8);
                        if is_fg {
                            st.fg = v;
                            st.fg_rgb = None;
                        } else {
                            st.bg = v;
                            st.bg_rgb = None;
                        }
                    }
                    i += 2;
                } else if nums.get(i + 1) == Some(&2) {
                    // Preserve exact RGB values independently from palette colors.
                    if let (Some(&r), Some(&g), Some(&b)) =
                        (nums.get(i + 2), nums.get(i + 3), nums.get(i + 4))
                    {
                        let rgb = Some([r.min(255) as u8, g.min(255) as u8, b.min(255) as u8]);
                        if is_fg {
                            st.fg = None;
                            st.fg_rgb = rgb;
                        } else {
                            st.bg = None;
                            st.bg_rgb = rgb;
                        }
                    }
                    i += 4;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Strips all escape sequences.
pub fn strip(line: &str) -> String {
    let mut st = SgrState::default();
    parse_line(line, &mut st)
        .into_iter()
        .map(|s| s.text)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sgr() {
        let mut st = SgrState::default();
        let spans = parse_line("plain \x1b[31;1mred bold\x1b[0m end", &mut st);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].text, "red bold");
        assert_eq!(spans[1].fg, Some(1));
        assert!(spans[1].bold);
        assert_eq!(spans[2].fg, None);
        assert_eq!(strip("a\x1b[32mb\x1b[m\r"), "ab");
        let spans = parse_line("\x1b[38;5;196mX", &mut st);
        assert_eq!(spans[0].fg, Some(196));
    }
    #[test]
    fn truecolor_is_exact_and_palette_default_reset_clears_rgb() {
        let mut state = SgrState::default();
        let spans = parse_line(
            "\x1b[38;2;12;34;56;48;2;65;43;21;1;3;4mX\x1b[31mY\x1b[0mZ",
            &mut state,
        );
        assert_eq!(spans[0].fg_rgb, Some([12, 34, 56]));
        assert_eq!(spans[0].bg_rgb, Some([65, 43, 21]));
        assert!(spans[0].bold && spans[0].italic && spans[0].underline);
        assert_eq!(spans[1].fg_rgb, None);
        assert_eq!(spans[1].fg, Some(1));
        assert_eq!(state, SgrState::default());
    }
}
