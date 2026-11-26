//! Dependency-free printable shortcut reference (PDF 1.4, vector text).
use super::{Action, Keymap};
impl Keymap {
    pub fn reference_pdf(&self) -> Vec<u8> {
        fn literal(text: &str) -> String {
            text.chars()
                .flat_map(|c| match c {
                    '(' => "\\(".chars().collect::<Vec<_>>(),
                    ')' => "\\)".chars().collect(),
                    '\\' => "\\\\".chars().collect(),
                    c if c.is_ascii() && !c.is_control() => vec![c],
                    _ => vec!['?'],
                })
                .collect()
        }
        let rows: Vec<_> = Action::ALL
            .iter()
            .flat_map(|a| {
                self.shortcut_definitions(*a)
                    .into_iter()
                    .map(move |s| format!("{s}    {}", a.english_text()))
            })
            .collect();
        let pages = rows.chunks(48).collect::<Vec<_>>();
        let mut objects = vec![
            String::new(),
            String::new(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        ];
        let mut kids = Vec::new();
        for (page, rows) in pages.iter().enumerate() {
            let page_id = objects.len() + 1;
            let stream_id = page_id + 1;
            kids.push(format!("{page_id} 0 R"));
            objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {stream_id} 0 R >>"));
            let mut stream = format!(
                "BT /F1 14 Tf 40 750 Td (RustPy - {} - {}) Tj /F1 10 Tf 0 -24 Td 14 TL\n",
                literal(&self.name),
                page + 1
            );
            for row in *rows {
                stream.push_str(&format!("({}) Tj T*\n", literal(row)));
            }
            stream.push_str("ET\n");
            objects.push(format!(
                "<< /Length {} >>\nstream\n{}endstream",
                stream.len(),
                stream
            ));
        }
        objects[0] = "<< /Type /Catalog /Pages 2 0 R >>".into();
        objects[1] = format!(
            "<< /Type /Pages /Count {} /Kids [{}] >>",
            kids.len(),
            kids.join(" ")
        );
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (i, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, object).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes(),
        );
        for offset in offsets.iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len()
            )
            .as_bytes(),
        );
        bytes
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cross_reference_points_at_each_indirect_object() {
        let bytes = Keymap::default_keymap().reference_pdf();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let xref = text
            .split("startxref\n")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert!(bytes[xref..].starts_with(b"xref\n"));
        for (index, line) in text[xref..]
            .lines()
            .skip(3)
            .take_while(|line| line.ends_with(" n "))
            .enumerate()
        {
            let offset = line[..10].parse::<usize>().unwrap();
            assert!(bytes[offset..].starts_with(format!("{} 0 obj", index + 1).as_bytes()));
        }
        assert!(text.contains("Save"));
        assert!(text.ends_with("%%EOF\n"));
    }
}
