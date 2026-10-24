//! CSS source-color swatches; byte ranges never include selectors or strings.
use super::Language;
use rustpy_core::{TextRange, TextSize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorPreview {
    pub range: TextRange,
    pub rgba: [u8; 4],
}

/// CSS declarations, or HTML style elements/quoted style attributes only.
pub fn previews(source: &str, language: Language) -> Vec<ColorPreview> {
    let mut out = Vec::new();
    match language {
        Language::Css => scan_css(source, 0, false, &mut out),
        Language::Html => scan_html(source, &mut out),
        _ => {}
    }
    out
}
fn emit(out: &mut Vec<ColorPreview>, start: usize, end: usize, rgba: [u8; 4]) {
    out.push(ColorPreview {
        range: TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
        rgba,
    });
}
fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-') || b >= 128
}
fn skip_quote(b: &[u8], mut i: usize) -> usize {
    let quote = b[i];
    i += 1;
    while i < b.len() {
        if b[i] == b'\\' {
            i = (i + 2).min(b.len());
        } else if b[i] == quote {
            return i + 1;
        } else {
            i += 1;
        }
    }
    i
}
fn declaration_boundaries(s: &str) -> Vec<(usize, u8)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut parens = 0usize;
    while i < b.len() {
        if b[i..].starts_with(b"/*") {
            i = s[i + 2..].find("*/").map_or(b.len(), |n| i + n + 4);
            continue;
        }
        match b[i] {
            b'\'' | b'"' => {
                i = skip_quote(b, i);
                continue;
            }
            b'(' => parens += 1,
            b')' => parens = parens.saturating_sub(1),
            c @ (b'{' | b'}' | b';') if parens == 0 => out.push((i, c)),
            _ => {}
        }
        i += 1;
    }
    out
}
fn without_comments(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        out.push(' ');
        let end = rest[start + 2..].find("*/")?;
        rest = &rest[start + end + 4..];
    }
    out.push_str(rest);
    Some(out)
}
fn scan_css(s: &str, base: usize, inline: bool, out: &mut Vec<ColorPreview>) {
    scan_css_inner(s, base, inline, out, 16)
}
fn scan_css_inner(s: &str, base: usize, inline: bool, out: &mut Vec<ColorPreview>, budget: usize) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut depth = usize::from(inline);
    let mut value = false;
    let mut segment = 0;
    let mut named_allowed = false;
    let boundaries = declaration_boundaries(s);
    let mut next_boundary = 0;
    while i < b.len() {
        while next_boundary < boundaries.len() && boundaries[next_boundary].0 <= i {
            next_boundary += 1;
        }
        if b[i..].starts_with(b"/*") {
            let only_prefix_space = !value && s[segment..i].trim().is_empty();
            i = s[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
            if only_prefix_space {
                segment = i;
            }
            continue;
        }
        match b[i] {
            b'\'' | b'"' => {
                i = skip_quote(b, i);
                continue;
            }
            b'{' => {
                depth += 1;
                value = false;
                segment = i + 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                value = false;
                segment = i + 1;
            }
            b';' => {
                value = false;
                segment = i + 1;
            }
            b':' if depth > 0 => {
                let property = s[segment..i].trim();
                value = !property.is_empty() && property.bytes().all(ident);
                let property = property.to_ascii_lowercase();
                named_allowed = property.starts_with("--")
                    || property.contains("color")
                    || matches!(
                        property.as_str(),
                        "background"
                            | "background-image"
                            | "border"
                            | "border-top"
                            | "border-right"
                            | "border-bottom"
                            | "border-left"
                            | "outline"
                            | "box-shadow"
                            | "text-shadow"
                            | "fill"
                            | "stroke"
                    );
                if value
                    && boundaries
                        .get(next_boundary)
                        .is_some_and(|(_, token)| *token == b'{')
                {
                    value = false;
                }
            }
            b'#' if value => {
                let start = i;
                i += 1;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                if (i == b.len() || !ident(b[i])) && matches!(i - start - 1, 3 | 4 | 6 | 8) {
                    if let Some(color) = hex(&s[start + 1..i]) {
                        emit(out, base + start, base + i, color);
                    }
                }
                continue;
            }
            c if ident(c) => {
                let start = i;
                while i < b.len() && ident(b[i]) {
                    i += 1;
                }
                let name = s[start..i].to_ascii_lowercase();
                if value {
                    if i < b.len() && b[i] == b'(' {
                        let open = i;
                        let mut level = 1;
                        i += 1;
                        while i < b.len() && level > 0 {
                            if b[i..].starts_with(b"/*") {
                                i = s[i + 2..].find("*/").map_or(b.len(), |n| i + n + 4);
                                continue;
                            }
                            match b[i] {
                                b'\'' | b'"' => {
                                    i = skip_quote(b, i);
                                    continue;
                                }
                                b'(' => level += 1,
                                b')' => level -= 1,
                                _ => {}
                            };
                            i += 1;
                        }
                        if level == 0 {
                            if let Some(color) = function(&name, &s[open + 1..i - 1]) {
                                emit(out, base + start, base + i, color);
                            } else if budget > 0
                                && matches!(
                                    name.as_str(),
                                    "linear-gradient"
                                        | "radial-gradient"
                                        | "conic-gradient"
                                        | "repeating-linear-gradient"
                                        | "repeating-radial-gradient"
                                        | "repeating-conic-gradient"
                                )
                            {
                                scan_css_value(
                                    &s[open + 1..i - 1],
                                    base + open + 1,
                                    out,
                                    budget - 1,
                                );
                            }
                        }
                    } else if named_allowed {
                        if let Some(color) = named(&name) {
                            emit(out, base + start, base + i, color);
                        }
                    }
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}
fn scan_css_value(s: &str, base: usize, out: &mut Vec<ColorPreview>, budget: usize) {
    // Supply declaration context without changing returned source ranges.
    let wrapped = format!("color:{s}");
    let mut marks = Vec::new();
    scan_css_inner(&wrapped, 0, true, &mut marks, budget);
    for mark in marks {
        let start = usize::from(mark.range.start());
        let end = usize::from(mark.range.end());
        if start >= 6 {
            emit(out, base + start - 6, base + end - 6, mark.rgba);
        }
    }
}
fn hex(s: &str) -> Option<[u8; 4]> {
    let mut out = [0, 0, 0, 255];
    match s.len() {
        3 | 4 => {
            for (i, c) in s.bytes().enumerate() {
                out[i] = (c as char).to_digit(16)? as u8 * 17;
            }
        }
        6 | 8 => {
            for (i, channel) in out.iter_mut().enumerate().take(s.len() / 2) {
                *channel = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
            }
        }
        _ => return None,
    };
    Some(out)
}
fn number(s: &str) -> Option<f64> {
    let n = s.parse::<f64>().ok()?;
    n.is_finite().then_some(n)
}
fn byte(n: f64) -> u8 {
    n.clamp(0.0, 255.0).round() as u8
}
fn alpha(s: &str) -> Option<u8> {
    Some(byte(if let Some(p) = s.strip_suffix('%') {
        number(p)? * 255.0 / 100.0
    } else {
        number(s)? * 255.0
    }))
}
fn channel(s: &str) -> Option<u8> {
    Some(byte(if let Some(p) = s.strip_suffix('%') {
        number(p)? * 255.0 / 100.0
    } else {
        number(s)?
    }))
}
fn function(name: &str, s: &str) -> Option<[u8; 4]> {
    let clean = without_comments(s)?;
    let s = clean.as_str();
    if matches!(name, "lab" | "lch" | "oklab" | "oklch" | "color" | "hwb") {
        return advanced(name, s);
    }
    if !matches!(name, "rgb" | "rgba" | "hsl" | "hsla") {
        return None;
    }
    let normalized = s.replace('/', " / ");
    let comma = s.contains(',');
    let pieces: Vec<_> = if comma {
        s.split(',').map(str::trim).collect()
    } else {
        normalized.split_whitespace().collect()
    };
    let (c, a) = if comma {
        if pieces.len() == 3 {
            (&pieces[..], 255)
        } else if pieces.len() == 4 {
            (&pieces[..3], alpha(pieces[3])?)
        } else {
            return None;
        }
    } else {
        if pieces.len() == 3 {
            (&pieces[..], 255)
        } else if pieces.len() == 5 && pieces[3] == "/" {
            (&pieces[..3], alpha(pieces[4])?)
        } else {
            return None;
        }
    };
    if name.starts_with("rgb") {
        if comma && c.iter().any(|v| v.ends_with('%')) && !c.iter().all(|v| v.ends_with('%')) {
            return None;
        }
        return Some([channel(c[0])?, channel(c[1])?, channel(c[2])?, a]);
    }
    let sat = number(c[1].strip_suffix('%')?)?.clamp(0.0, 100.0) / 100.0;
    let light = number(c[2].strip_suffix('%')?)?.clamp(0.0, 100.0) / 100.0;
    let h = hue(c[0])?.to_degrees() / 60.0;
    let chroma = (1.0 - (2.0 * light - 1.0).abs()) * sat;
    let x = chroma * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h as u8 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = light - chroma / 2.0;
    Some([
        byte((r + m) * 255.0),
        byte((g + m) * 255.0),
        byte((b + m) * 255.0),
        a,
    ])
}
// CSS Color 4 §6.1 https://www.w3.org/TR/css-color-4/#named-colors
const NAMED_COLORS: [(&str, u32); 148] = [
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];
fn named(name: &str) -> Option<[u8; 4]> {
    if name == "transparent" {
        return Some([0, 0, 0, 0]);
    }
    let rgb = NAMED_COLORS.iter().find(|(key, _)| *key == name)?.1;
    Some([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255])
}

fn html_quote(b: &[u8], i: usize) -> usize {
    let quote = b[i];
    b[i + 1..]
        .iter()
        .position(|c| *c == quote)
        .map_or(b.len(), |n| i + n + 2)
}
fn scan_html(s: &str, out: &mut Vec<ColorPreview>) {
    let lower = s.to_ascii_lowercase();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"<!--") {
            i = s[i + 4..].find("-->").map_or(b.len(), |n| i + 4 + n + 3);
            continue;
        }
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_alphanumeric() {
            j += 1;
        }
        let tag = &lower[i + 1..j];
        if tag.is_empty() {
            i += 1;
            continue;
        }
        let mut end = j;
        while end < b.len() {
            if matches!(b[end], b'\'' | b'"') {
                end = html_quote(b, end);
            } else if b[end] == b'>' {
                break;
            } else {
                end += 1;
            }
        }
        if end == b.len() {
            break;
        }
        let mut attr = j;
        while attr < end {
            while attr < end && b[attr].is_ascii_whitespace() {
                attr += 1;
            }
            let start = attr;
            while attr < end && (ident(b[attr]) || b[attr] == b':') {
                attr += 1;
            }
            if start == attr {
                attr += 1;
                continue;
            }
            let name = &lower[start..attr];
            while attr < end && b[attr].is_ascii_whitespace() {
                attr += 1;
            }
            if attr >= end || b[attr] != b'=' {
                continue;
            }
            attr += 1;
            while attr < end && b[attr].is_ascii_whitespace() {
                attr += 1;
            }
            if attr < end && matches!(b[attr], b'\'' | b'"') {
                let value = attr + 1;
                let finish = html_quote(b, attr).min(end);
                if name == "style" && finish > value {
                    scan_css(&s[value..finish - 1], value, true, out);
                }
                attr = finish;
            } else {
                while attr < end && !b[attr].is_ascii_whitespace() {
                    attr += 1;
                }
            }
        }
        i = end + 1;
        if matches!(tag, "style" | "script") {
            let close = format!("</{tag}");
            let finish = lower[i..].find(&close).map_or(b.len(), |n| i + n);
            if tag == "style" {
                scan_css(&s[i..finish], i, false, out);
            }
            i = finish;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn colors(s: &str) -> Vec<[u8; 4]> {
        previews(s, Language::Css)
            .into_iter()
            .map(|p| p.rgba)
            .collect()
    }
    #[test]
    fn hex_alpha_and_exact_utf8_ranges() {
        let s = ".α { color:#aBc; background:#1234; border:1px solid #123456; outline:#abcdef80 }";
        let marks = previews(s, Language::Css);
        assert_eq!(
            marks.iter().map(|p| p.rgba).collect::<Vec<_>>(),
            vec![
                [170, 187, 204, 255],
                [17, 34, 51, 68],
                [18, 52, 86, 255],
                [171, 205, 239, 128]
            ]
        );
        assert_eq!(
            marks
                .iter()
                .map(|p| &s[usize::from(p.range.start())..usize::from(p.range.end())])
                .collect::<Vec<_>>(),
            vec!["#aBc", "#1234", "#123456", "#abcdef80"]
        );
    }
    #[test]
    fn function_formats_percentage_alpha_hue_units_and_clamping() {
        assert_eq!(colors("a { c:rgb(255,0,128); d:rgba(100%, 0%, 50%, 25%); e:hsl(120,100%,50%); f:hsla(.5turn 100% 50% / .5); g:rgb(300 -5 0 / 200%); }"),vec![[255,0,128,255],[255,0,128,64],[0,255,0,255],[0,255,255,128],[255,0,0,255]]);
        assert_eq!(
            colors("a { c:hsl(400grad 100% 50%); }"),
            vec![[255, 0, 0, 255]]
        );
    }
    #[test]
    fn comments_strings_selectors_urls_and_invalid_forms_are_excluded() {
        assert!(colors("#abc { content:'red #fff'; x:url(#fff); c:#12; d:#fffff; e:#fffffff; f:#fffffffff; g:rgb(1,2); h:hsl(0,1,2); i:rgba(NaN,0,0,1); /* c:blue */ }").is_empty());
        assert_eq!(
            colors("a { background:linear-gradient(red, rgba(0,0,255,.5)); }"),
            vec![[255, 0, 0, 255], [0, 0, 255, 128]]
        );
        assert!(previews("# red #abc\nx='rgb(1,2,3)'", Language::Python).is_empty());
    }
    #[test]
    fn html_embedded_style_and_attribute_contexts_only() {
        let s="<!-- <style>a{c:red}</style> --><script>const x='<style>a{c:blue}</style>';</script><div data-color='red' style='color:#0f08; content:\"blue\"'>red</div><style>.x {color:rebeccapurple}</style>";
        let m = previews(s, Language::Html);
        assert_eq!(
            m.iter().map(|p| p.rgba).collect::<Vec<_>>(),
            vec![[0, 255, 0, 136], [102, 51, 153, 255]]
        );
        assert_eq!(
            m.iter()
                .map(|p| &s[usize::from(p.range.start())..usize::from(p.range.end())])
                .collect::<Vec<_>>(),
            vec!["#0f08", "rebeccapurple"]
        );
    }
    #[test]
    fn named_colors_and_identifier_boundaries() {
        assert_eq!(
            colors("a{color:TRANSPARENT;background:grey;outline:orange;fill:aquamarineish;stroke:redness;font-family:red;animation-name:blue}"),
            vec![[0, 0, 0, 0], [128, 128, 128, 255], [255, 165, 0, 255]]
        );
    }
}

// CSS Color 4 §19 conversion matrices. Static numeric colors only; converting
// to an sRGB swatch clips out-of-gamut channels (not a CSS gamut-map engine).
fn scaled(s: &str, percent_scale: f64) -> Option<f64> {
    if s == "none" {
        return Some(0.0);
    }
    if let Some(p) = s.strip_suffix('%') {
        Some(number(p)? * percent_scale / 100.0)
    } else {
        number(s)
    }
}
fn hue(s: &str) -> Option<f64> {
    let s = s.to_ascii_lowercase();
    // Reduce in the source unit before multiplying: finite huge angles must
    // not overflow to infinity and then silently cast NaN channels to bytes.
    let radians = if s == "none" {
        0.0
    } else if let Some(n) = s.strip_suffix("turn") {
        number(n)?.rem_euclid(1.0) * std::f64::consts::TAU
    } else if let Some(n) = s.strip_suffix("grad") {
        number(n)?.rem_euclid(400.0) * (std::f64::consts::TAU / 400.0)
    } else if let Some(n) = s.strip_suffix("rad") {
        number(n)?.rem_euclid(std::f64::consts::TAU)
    } else {
        number(s.strip_suffix("deg").unwrap_or(&s))?
            .rem_euclid(360.0)
            .to_radians()
    };
    radians.is_finite().then_some(radians)
}

fn matrix(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    m.map(|r| r[0] * v[0] + r[1] * v[1] + r[2] * v[2])
}
fn linear(c: f64) -> f64 {
    if c.abs() <= 0.04045 {
        c / 12.92
    } else {
        c.signum() * ((c.abs() + 0.055) / 1.055).powf(2.4)
    }
}
fn gamma(c: f64) -> f64 {
    if c.abs() <= 0.0031308 {
        c * 12.92
    } else {
        c.signum() * (1.055 * c.abs().powf(1.0 / 2.4) - 0.055)
    }
}
fn xyz_srgb(xyz: [f64; 3]) -> [f64; 3] {
    matrix(
        [
            [12831.0 / 3959.0, -329.0 / 214.0, -1974.0 / 3959.0],
            [
                -851781.0 / 878810.0,
                1648619.0 / 878810.0,
                36519.0 / 878810.0,
            ],
            [705.0 / 12673.0, -2585.0 / 12673.0, 705.0 / 667.0],
        ],
        xyz,
    )
    .map(gamma)
}
fn adapt_d50(xyz: [f64; 3]) -> [f64; 3] {
    matrix(
        [
            [0.955473421488075, -0.02309845494876471, 0.06325924320057072],
            [
                -0.0283697093338637,
                1.0099953980813041,
                0.021041441191917323,
            ],
            [
                0.012314014864481998,
                -0.020507649298898964,
                1.330365926242124,
            ],
        ],
        xyz,
    )
}
fn p3_xyz(components: [f64; 3]) -> [f64; 3] {
    matrix(
        [
            [
                608311.0 / 1250200.0,
                189793.0 / 714400.0,
                198249.0 / 1000160.0,
            ],
            [
                35783.0 / 156275.0,
                247089.0 / 357200.0,
                198249.0 / 2500400.0,
            ],
            [0.0, 32229.0 / 714400.0, 5220557.0 / 5000800.0],
        ],
        components,
    )
}
fn advanced(name: &str, s: &str) -> Option<[u8; 4]> {
    if s.contains(',') {
        return None;
    }
    let normalized = s.replace('/', " / ");
    let fields: Vec<_> = normalized.split_whitespace().collect();
    let count = if name == "color" { 4 } else { 3 };
    let alpha = if fields.len() == count {
        255
    } else if fields.len() == count + 2 && fields[count] == "/" {
        if fields[count + 1] == "none" {
            0
        } else {
            alpha(fields[count + 1])?
        }
    } else {
        return None;
    };
    let rgb = if name == "hwb" {
        let h = hue(fields[0])?.to_degrees() / 60.0;
        let percent = |s: &str| -> Option<f64> {
            if s == "none" {
                Some(0.0)
            } else {
                Some(number(s.strip_suffix('%')?)?.clamp(0.0, 100.0) / 100.0)
            }
        };
        let white = percent(fields[1])?;
        let black = percent(fields[2])?;
        if white + black >= 1.0 {
            [white / (white + black); 3]
        } else {
            let x = 1.0 - (h.rem_euclid(2.0) - 1.0).abs();
            let pure = match h as u8 {
                0 => [1.0, x, 0.0],
                1 => [x, 1.0, 0.0],
                2 => [0.0, 1.0, x],
                3 => [0.0, x, 1.0],
                4 => [x, 0.0, 1.0],
                _ => [1.0, 0.0, x],
            };
            pure.map(|c| c * (1.0 - white - black) + white)
        }
    } else if name == "color" {
        let components = [
            scaled(fields[1], 1.0)?,
            scaled(fields[2], 1.0)?,
            scaled(fields[3], 1.0)?,
        ];
        match fields[0].to_ascii_lowercase().as_str() {
            "srgb" => components,
            "srgb-linear" => components.map(gamma),
            // P3 and sRGB share D65 and the transfer curve. Preserve their
            // exact neutral identity instead of introducing matrix roundoff
            // at 8-bit half-step boundaries (e.g. .5 must round to 128).
            "display-p3" if components[0] == components[1] && components[1] == components[2] => {
                components
            }
            "display-p3" => xyz_srgb(p3_xyz(components.map(linear))),
            "display-p3-linear" => xyz_srgb(p3_xyz(components)),
            "a98-rgb" => xyz_srgb(matrix(
                [
                    [
                        573536.0 / 994567.0,
                        263643.0 / 1420810.0,
                        187206.0 / 994567.0,
                    ],
                    [
                        591459.0 / 1989134.0,
                        6239551.0 / 9945670.0,
                        374412.0 / 4972835.0,
                    ],
                    [
                        53769.0 / 1989134.0,
                        351524.0 / 4972835.0,
                        4929758.0 / 4972835.0,
                    ],
                ],
                components.map(|c| c.signum() * c.abs().powf(563.0 / 256.0)),
            )),
            "prophoto-rgb" => xyz_srgb(adapt_d50(matrix(
                [
                    [0.7977666449006423, 0.13518129740053308, 0.0313477341283922],
                    [0.2880748288194013, 0.711835234241873, 0.00008993693872564],
                    [0.0, 0.0, 0.8251046025104602],
                ],
                components.map(|c| {
                    if c.abs() <= 16.0 / 512.0 {
                        c / 16.0
                    } else {
                        c.signum() * c.abs().powf(1.8)
                    }
                }),
            ))),
            // Current CSS Color 4 §19 uses the BT.1886 2.4 transfer function.
            "rec2020" => xyz_srgb(matrix(
                [
                    [
                        63426534.0 / 99577255.0,
                        20160776.0 / 139408157.0,
                        47086771.0 / 278816314.0,
                    ],
                    [
                        26158966.0 / 99577255.0,
                        472592308.0 / 697040785.0,
                        8267143.0 / 139408157.0,
                    ],
                    [0.0, 19567812.0 / 697040785.0, 295819943.0 / 278816314.0],
                ],
                components.map(|c| c.signum() * c.abs().powf(2.4)),
            )),
            "xyz" | "xyz-d65" => xyz_srgb(components),
            "xyz-d50" => xyz_srgb(adapt_d50(components)),
            _ => return None,
        }
    } else {
        let ok = name.starts_with("ok");
        let light = scaled(fields[0], if ok { 1.0 } else { 100.0 })?
            .clamp(0.0, if ok { 1.0 } else { 100.0 });
        let polar = name.ends_with("lch");
        let scale = if ok {
            0.4
        } else if polar {
            150.0
        } else {
            125.0
        };
        let component = scaled(fields[1], scale)?;
        let (a, b) = if polar {
            let angle = hue(fields[2])?;
            let chroma = component.max(0.0);
            (chroma * angle.cos(), chroma * angle.sin())
        } else {
            (component, scaled(fields[2], scale)?)
        };
        if ok {
            let lms = matrix(
                [
                    [1.0, 0.3963377773761749, 0.2158037573099136],
                    [1.0, -0.1055613458156586, -0.0638541728258133],
                    [1.0, -0.0894841775298119, -1.2914855480194092],
                ],
                [light, a, b],
            )
            .map(|v| v.powi(3));
            xyz_srgb(matrix(
                [
                    [1.2268798758459243, -0.5578149944602171, 0.2813910456659647],
                    [-0.0405757452148008, 1.112286803280317, -0.0717110580655164],
                    [-0.0763729366746601, -0.4214933324022432, 1.5869240198367816],
                ],
                lms,
            ))
        } else {
            let fy = (light + 16.0) / 116.0;
            let inv = |f: f64| {
                let cube = f.powi(3);
                if cube > 216.0 / 24389.0 {
                    cube
                } else {
                    (116.0 * f - 16.0) / (24389.0 / 27.0)
                }
            };
            let xyz = [
                inv(fy + a / 500.0) * (0.3457 / 0.3585),
                inv(fy),
                inv(fy - b / 200.0) * ((1.0 - 0.3457 - 0.3585) / 0.3585),
            ];
            xyz_srgb(adapt_d50(xyz))
        }
    };
    if !rgb.iter().all(|c| c.is_finite()) {
        return None;
    }
    Some([
        byte(rgb[0] * 255.0),
        byte(rgb[1] * 255.0),
        byte(rgb[2] * 255.0),
        alpha,
    ])
}

#[cfg(test)]
mod complete_color_tests {
    use super::*;
    #[test]
    fn full_css_named_table_is_case_insensitive_and_has_all_synonyms() {
        assert_eq!(NAMED_COLORS.len(), 148);
        let mut names: Vec<_> = NAMED_COLORS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 148);
        let source = format!(
            "a{{ {} }}",
            NAMED_COLORS
                .iter()
                .map(|(name, _)| format!("color:{};", name.to_ascii_uppercase()))
                .collect::<String>()
        );
        assert_eq!(previews(&source, Language::Css).len(), 148);
        for (a, b) in [
            ("gray", "grey"),
            ("darkgray", "darkgrey"),
            ("dimgray", "dimgrey"),
            ("lightgray", "lightgrey"),
            ("lightslategray", "lightslategrey"),
            ("slategray", "slategrey"),
            ("darkslategray", "darkslategrey"),
            ("aqua", "cyan"),
            ("fuchsia", "magenta"),
        ] {
            assert_eq!(named(a), named(b));
        }
        assert_eq!(named("aliceblue"), Some([240, 248, 255, 255]));
        assert_eq!(named("darkolivegreen"), Some([85, 107, 47, 255]));
        assert_eq!(named("papayawhip"), Some([255, 239, 213, 255]));
        assert_eq!(named("lightslategray"), Some([119, 136, 153, 255]));
    }
    #[test]
    fn color4_static_spaces_use_reference_primaries_neutrals_and_alpha() {
        for (literal, expected) in [
            ("lab(100% 0 0)", [255, 255, 255, 255]),
            ("lab(0 0 0)", [0, 0, 0, 255]),
            ("lab(50% 0 0)", [119, 119, 119, 255]),
            ("lch(50% 0 270deg / 25%)", [119, 119, 119, 64]),
            ("oklab(0.5 0 0)", [99, 99, 99, 255]),
            ("oklch(50% 0 1turn / .5)", [99, 99, 99, 128]),
            ("color(srgb .25 .5 .75 / 50%)", [64, 128, 191, 128]),
            ("color(srgb-linear .5 .5 .5)", [188, 188, 188, 255]),
            ("color(display-p3 .5 .5 .5)", [128, 128, 128, 255]),
            ("color(display-p3 1 0 0)", [255, 0, 0, 255]),
        ] {
            assert_eq!(
                function(
                    literal.split('(').next().unwrap(),
                    literal
                        .strip_suffix(')')
                        .unwrap()
                        .split_once('(')
                        .unwrap()
                        .1
                ),
                Some(expected),
                "{literal}"
            );
        }
        let red = advanced("oklab", "0.6279553606 0.2248630611 0.1258462985").unwrap();
        for (actual, expected) in red[..3].iter().zip([255, 0, 0]) {
            assert!((i32::from(*actual) - expected).abs() <= 1);
        }
        let source="a {color:lab(50% 0 0);background:oklch(.5 0 0);outline:color(display-p3 .5 .5 .5 / .5)}";
        assert_eq!(previews(source, Language::Css).len(), 3);
        for value in [
            "lab(NaN 0 0)",
            "color(unknown 1 0 0)",
            "lab(50, 0, 0)",
            "oklch(.5 calc(1) 20)",
        ] {
            assert!(previews(&format!("a{{color:{value}}}"), Language::Css).is_empty());
        }
    }
    #[test]
    fn quotes_comments_and_many_declarations_have_structural_boundaries() {
        let source="a { background:url(\"x{\") #fff; /* { ; */ color:rgb(255 /* ) { ; */ 0 0); a:is(red) { color:blue; } }";
        assert_eq!(
            previews(source, Language::Css)
                .iter()
                .map(|p| p.rgba)
                .collect::<Vec<_>>(),
            vec![[255, 255, 255, 255], [255, 0, 0, 255], [0, 0, 255, 255]]
        );
        let source = format!("a{{{}}}", "color:red;".repeat(2000));
        assert_eq!(previews(&source, Language::Css).len(), 2000);
        assert_eq!(
            function("rgb", "255/*note*/ 0 0 / 50%"),
            Some([255, 0, 0, 128])
        );
    }
}

#[cfg(test)]
mod hwb_tests {
    use super::*;
    #[test]
    fn hwb_primaries_alpha_and_normalized_gray() {
        for (input, expected) in [
            ("0 20% 30%", [179, 51, 51, 255]),
            ("120 25% 25% / 50%", [64, 191, 64, 128]),
            (".6666666666666666turn 0% 0%", [0, 0, 255, 255]),
            ("0 75% 75%", [128, 128, 128, 255]),
            ("270 20% 80% / .25", [51, 51, 51, 64]),
        ] {
            assert_eq!(function("hwb", input), Some(expected), "{input}");
        }
        assert_eq!(
            previews("a{color:hwb(120 25% 25% / 50%)}", Language::Css)[0].rgba,
            [64, 191, 64, 128]
        );
        for invalid in ["0 20 30", "0,20%,30%", "NaN 0% 0%", "0 20% 30% .5"] {
            assert_eq!(function("hwb", invalid), None);
        }
    }
}

#[cfg(test)]
mod predefined_space_tests {
    use super::*;
    #[test]
    fn remaining_predefined_spaces_white_neutral_primary_and_alpha() {
        for (space, gray) in [
            ("a98-rgb", 129),
            ("prophoto-rgb", 146),
            ("rec2020", 120),
            ("display-p3-linear", 188),
        ] {
            assert_eq!(
                function("color", &format!("{space} 1 1 1")),
                Some([255, 255, 255, 255]),
                "{space}"
            );
            assert_eq!(
                function("color", &format!("{space} 0 0 0")),
                Some([0, 0, 0, 255]),
                "{space}"
            );
            assert_eq!(
                function("color", &format!("{space} .5 .5 .5 / .5")),
                Some([gray, gray, gray, 128]),
                "{space}"
            );
            assert_eq!(
                function("color", &format!("{space} 1 0 0")),
                Some([255, 0, 0, 255]),
                "{space}"
            );
        }
        for space in ["xyz", "xyz-d65"] {
            assert_eq!(
                function(
                    "color",
                    &format!("{space} 0.9504559270516716 1 1.0890577507598784")
                ),
                Some([255, 255, 255, 255])
            );
            let red = function(
                "color",
                &format!("{space} 0.4123907992659595 0.2126390058715104 0.0193308187155918 / 25%"),
            )
            .unwrap();
            assert_eq!(red, [255, 0, 0, 64]);
        }
        assert_eq!(
            function("color", "xyz-d50 0.9642956764295677 1 0.8251046025104602"),
            Some([255, 255, 255, 255])
        );
        assert_eq!(function("color", "xyz-d50 0 0 0"), Some([0, 0, 0, 255]));
        assert_eq!(
            function("color", "prophoto-rgb .02 .02 .02"),
            Some([4, 4, 4, 255])
        );
        let source="a {color:color(a98-rgb 1 0 0); background:color(prophoto-rgb .5 .5 .5); outline:color(rec2020 .5 .5 .5)}";
        assert_eq!(previews(source, Language::Css).len(), 3);
    }
    #[test]
    fn predefined_spaces_reject_malformed_or_nonfinite_components() {
        for input in [
            "a98-rgb 1 0",
            "prophoto-rgb 1, 0, 0",
            "rec2020 1 0 NaN",
            "xyz-d50 0 0 0 / Infinity",
            "xyz-d65 0 0 0 extra",
            "xyz 0 calc(1) 0",
            "unknown 0 0 0",
        ] {
            assert_eq!(function("color", input), None, "{input}");
        }
        assert_eq!(function("color", "a98-rgb -1 -1 -1"), Some([0, 0, 0, 255]));
    }
}

#[cfg(test)]
mod numeric_precision_tests {
    use super::*;
    #[test]
    fn p3_neutrals_match_srgb_at_half_byte_boundaries() {
        for n in [0.5, 0.1, 0.9] {
            assert_eq!(
                function("color", &format!("display-p3 {n} {n} {n}")),
                function("color", &format!("srgb {n} {n} {n}"))
            );
        }
        assert_eq!(
            function("color", "display-p3 .5 .5 .5"),
            Some([128, 128, 128, 255])
        );
    }
    #[test]
    fn huge_finite_hues_reduce_before_turn_and_radian_conversion() {
        assert_eq!(
            function("hsl", "1e308turn 100% 50%"),
            Some([255, 0, 0, 255])
        );
        for token in ["1e308rad", "1e308turn", "-1e308grad"] {
            let reduced = hue(token).unwrap();
            assert!(reduced.is_finite());
            let expected = format!("{reduced}rad 100% 50%");
            assert_eq!(
                function("hsl", &format!("{token} 100% 50%")),
                function("hsl", &expected)
            );
        }
        let rad = function("hsl", "1e308rad 100% 50%").unwrap();
        assert_ne!(
            rad,
            [255, 0, 0, 255],
            "a finite radian remainder must not fabricate red via NaN"
        );
        assert!(function("hsl", "Infinityturn 100% 50%").is_none());
    }
}
