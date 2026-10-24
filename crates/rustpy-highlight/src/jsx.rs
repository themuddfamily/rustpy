//! JSX/TSX lexical colors. Type checking and component resolution belong to the
//! installed TypeScript service; this scanner stays useful while code is broken.
use crate::{
    common::{finish, Sink},
    HighlightTag, Span,
};

pub fn highlight_jsx(source: &str) -> Vec<Span> {
    let base = crate::highlight_javascript(source);
    let bytes = source.as_bytes();
    let mut markup = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let lexical = base.partition_point(|span| usize::from(span.range.end()) <= cursor);
        if bytes[cursor] != b'<'
            || base.get(lexical).is_some_and(|span| {
                usize::from(span.range.start()) <= cursor
                    && matches!(
                        span.tag,
                        HighlightTag::String | HighlightTag::Comment | HighlightTag::Docstring
                    )
            })
        {
            cursor += 1;
            continue;
        }
        let name = cursor + 1 + usize::from(bytes.get(cursor + 1) == Some(&b'/'));
        if !bytes
            .get(name)
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'>')
        {
            cursor += 1;
            continue;
        }
        let start = cursor;
        let mut at = cursor + 1;
        let mut quote = None;
        let mut depth = 0usize;
        while at < bytes.len() {
            let byte = bytes[at];
            if let Some(delimiter) = quote {
                if byte == b'\\' {
                    at = (at + 2).min(bytes.len());
                    continue;
                }
                if byte == delimiter {
                    quote = None;
                }
            } else {
                match byte {
                    b'\'' | b'"' | b'`' => quote = Some(byte),
                    b'{' => depth += 1,
                    b'}' => depth = depth.saturating_sub(1),
                    b'>' if depth == 0 => {
                        at += 1;
                        break;
                    }
                    b'<' if depth == 0 => break,
                    _ => {}
                }
            }
            at += 1;
        }
        if at > start && bytes.get(at.saturating_sub(1)) == Some(&b'>') {
            markup.push((start, at));
            cursor = at;
        } else {
            cursor += 1;
        }
    }
    let mut sink = Sink::default();
    sink.spans.extend(base.into_iter().filter(|span| {
        let index = markup.partition_point(|(_, end)| *end <= usize::from(span.range.start()));
        !markup.get(index).is_some_and(|(start, end)| {
            usize::from(span.range.start()) < *end && *start < usize::from(span.range.end())
        })
    }));
    for (start, end) in markup {
        let mut segment = start;
        let mut at = start;
        while at < end {
            if bytes[at] != b'{' {
                at += 1;
                continue;
            }
            sink.push_offset(crate::highlight_html(&source[segment..at]), segment);
            let begin = at;
            at += 1;
            let mut depth = 1usize;
            let mut quote = None;
            while at < end && depth > 0 {
                let byte = bytes[at];
                if let Some(delimiter) = quote {
                    if byte == b'\\' {
                        at = (at + 2).min(end);
                        continue;
                    }
                    if byte == delimiter {
                        quote = None;
                    }
                } else {
                    match byte {
                        b'\'' | b'"' | b'`' => quote = Some(byte),
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                }
                at += 1;
            }
            sink.push_offset(crate::highlight_javascript(&source[begin..at]), begin);
            segment = at;
        }
        sink.push_offset(crate::highlight_html(&source[segment..end]), segment);
    }
    finish(source, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn component_attributes_and_unicode_expressions_keep_byte_ranges() {
        let source = "const title = 'λ'; const view = <Panel title={title} enabled><input value=\"é\" /></Panel>;";
        let spans = highlight_jsx(source);
        assert!(spans
            .iter()
            .any(|span| span.tag == HighlightTag::Tag && source[span.range].contains("Panel")));
        assert!(spans
            .iter()
            .any(|span| span.tag == HighlightTag::Keyword && &source[span.range] == "const"));
        assert!(spans
            .windows(2)
            .all(|pair| pair[0].range.end() <= pair[1].range.start()));
        for span in spans {
            assert!(
                source.is_char_boundary(usize::from(span.range.start()))
                    && source.is_char_boundary(usize::from(span.range.end()))
            );
        }
    }
}
