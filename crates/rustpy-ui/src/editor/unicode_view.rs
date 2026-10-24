//! Presentation-only UAX#9 ordering. Neither normalization nor reshaping reaches the document.
use egui::text::LayoutJob;
use unicode_bidi::BidiInfo;
use unicode_segmentation::UnicodeSegmentation;
#[derive(Clone, Debug)]
pub struct VisualMap {
    pub logical_to_visual: Vec<usize>,
    pub visual_to_logical: Vec<usize>,
    pub glyph_start: Vec<usize>,
    pub glyph_len: Vec<usize>,
}
/// Arabic presentation forms keep one scalar per original scalar: ligatures and
/// deletion of marks/tatweel are disabled, retaining exact logical source mapping.
pub fn shape(job: LayoutJob) -> LayoutJob {
    if !job.text.chars().any(|c| matches!(c as u32, 0x600..=0x8ff)) {
        return job;
    }
    static SHAPER: std::sync::OnceLock<arabic_reshaper::ArabicReshaper<'static>> =
        std::sync::OnceLock::new();
    let shaper = SHAPER.get_or_init(|| {
        let mut s = arabic_reshaper::ArabicReshaper::new();
        for (key, value) in [
            ("support_ligatures", false),
            ("delete_harakat", false),
            ("delete_tatweel", false),
            ("shift_harakat_position", false),
        ] {
            s.configuration.insert(key, value);
        }
        s
    });
    let text = shaper.reshape(&job.text);
    if text.chars().count() != job.text.chars().count() {
        return job;
    }
    let mut output = job.clone();
    output.text.clear();
    output.sections.clear();
    let shaped = text.chars().collect::<Vec<_>>();
    for section in &job.sections {
        let start = job.text[..section.byte_range.start.0].chars().count();
        let end = start
            + job.text[section.byte_range.start.0..section.byte_range.end.0]
                .chars()
                .count();
        let part = shaped[start..end].iter().collect::<String>();
        output.append(&part, section.leading_space, section.format.clone());
    }
    output
}
/// Rows come from the shaped logical galley. Each row is reordered independently
/// using the full paragraph levels, then kept as an explicit visual line.
pub fn reorder(
    job: LayoutJob,
    rows: &[(usize, usize)],
    base: Option<bool>,
) -> (LayoutJob, Option<VisualMap>) {
    let info = BidiInfo::new(
        &job.text,
        base.map(|rtl| {
            if rtl {
                unicode_bidi::Level::rtl()
            } else {
                unicode_bidi::Level::ltr()
            }
        }),
    );
    let chars = job
        .text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(job.text.len()))
        .collect::<Vec<_>>();
    let total = chars.len() - 1;
    // Emergency soft wraps must not split an extended grapheme. The original
    // galley measures widths; only a boundary inside a cluster moves here.
    let boundaries = job
        .text
        .grapheme_indices(true)
        .map(|(byte, _)| chars.binary_search(&byte).unwrap_or(0))
        .chain(std::iter::once(total))
        .collect::<Vec<_>>();
    let splits_cluster = rows
        .iter()
        .any(|(_, end)| boundaries.binary_search(end).is_err());
    if !info.has_rtl() && !splits_cluster {
        return (job, None);
    }
    let mut safe_rows = Vec::new();
    let mut next_start = 0;
    for (_, end) in rows.iter().copied() {
        let end = end.min(total);
        if end < next_start {
            continue;
        }
        let safe_end = match boundaries.binary_search(&end) {
            Ok(_) => end,
            Err(index) => {
                let before = boundaries[index.saturating_sub(1)];
                if before > next_start {
                    before
                } else {
                    boundaries.get(index).copied().unwrap_or(total)
                }
            }
        };
        if safe_end > next_start || total == 0 {
            safe_rows.push((next_start, safe_end));
            next_start = safe_end;
        }
    }
    if next_start < total {
        safe_rows.push((next_start, total));
    }
    let rows = safe_rows.as_slice();

    let mut output = job.clone();
    output.text.clear();
    output.sections.clear();
    output.wrap.max_width = f32::INFINITY;
    let mut logical_to_visual = vec![0; total + 1];
    let mut glyph_start = vec![0; total + 1];
    let mut glyph_len = vec![0; total + 1];
    let mut visual_to_logical = vec![0];
    let mut visual = 0;
    for (row_index, (start, end)) in rows.iter().copied().enumerate() {
        let start = start.min(total);
        let end = end.min(total);
        if start > end {
            continue;
        }
        let bytes = chars[start]..chars[end];
        let para = info
            .paragraphs
            .iter()
            .find(|p| p.range.start <= bytes.start && bytes.start <= p.range.end);
        let levels = para
            .map(|p| info.reordered_levels(p, bytes.clone()))
            .unwrap_or_else(|| info.levels.clone());
        let clusters = job.text[bytes.clone()]
            .grapheme_indices(true)
            .map(|(i, text)| {
                let byte = bytes.start + i;
                let source = chars.binary_search(&byte).unwrap_or(start);
                (byte, source, text)
            })
            .collect::<Vec<_>>();
        let cluster_levels = clusters
            .iter()
            .map(|(byte, _, _)| {
                levels
                    .get(*byte)
                    .copied()
                    .unwrap_or(unicode_bidi::Level::ltr())
            })
            .collect::<Vec<_>>();
        for index in BidiInfo::reorder_visual(&cluster_levels) {
            let (byte, source, text) = clusters[index];
            let rtl = cluster_levels[index].is_rtl();
            let count = text.chars().count();
            let format = job
                .sections
                .iter()
                .find(|s| s.byte_range.start.0 <= byte && byte < s.byte_range.end.0)
                .map(|s| s.format.clone())
                .unwrap_or_default();
            let display = text
                .chars()
                .map(|c| {
                    if rtl {
                        unicode_bidi_mirroring::get_mirrored(c).unwrap_or(c)
                    } else {
                        c
                    }
                })
                .collect::<String>();
            output.append(&display, 0.0, format);
            for offset in 0..count {
                logical_to_visual[source + offset] = if rtl { visual + count } else { visual };
                glyph_start[source + offset] = visual;
                glyph_len[source + offset] = count;
            }
            if source + count == total {
                logical_to_visual[total] = if rtl { visual } else { visual + count };
                glyph_start[total] = visual + count;
            }
            visual_to_logical.resize(visual + count + 1, source);
            visual_to_logical[visual] = if rtl { source + count } else { source };
            for offset in 1..count {
                visual_to_logical[visual + offset] = if rtl { source + count } else { source };
            }
            visual_to_logical[visual + count] = if rtl { source } else { source + count };
            visual += count;
        }
        if row_index + 1 < rows.len() {
            let format = output
                .sections
                .last()
                .map(|s| s.format.clone())
                .unwrap_or_default();
            output.append("\n", 0.0, format);
            visual += 1;
            visual_to_logical.resize(visual + 1, end);
        }
    }
    (
        output,
        Some(VisualMap {
            logical_to_visual,
            visual_to_logical,
            glyph_start,
            glyph_len,
        }),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rtl_reordering_keeps_grapheme_clusters_and_roundtrip_byte_spelling() {
        let source = "x = 'אבג e\u{301} 👩🏽‍💻'";
        let mut job = LayoutJob::default();
        job.append(source, 0.0, Default::default());
        let (output, map) = reorder(job, &[(0, source.chars().count())], Some(false));
        let map = map.unwrap();
        assert!(output.text.contains("גבא"));
        assert!(output.text.contains("e\u{301}"));
        assert!(output.text.contains("👩🏽‍💻"));
        assert_eq!(map.logical_to_visual.len(), source.chars().count() + 1);
        assert!(map
            .visual_to_logical
            .iter()
            .all(|i| *i <= source.chars().count()));
        assert_eq!(source.as_bytes(), "x = 'אבג e\u{301} 👩🏽‍💻'".as_bytes());
    }
    #[test]
    fn wrapped_mixed_paragraph_has_an_explicit_visual_row_mapping() {
        let source = "abc אבג def דהו";
        let mut job = LayoutJob::default();
        job.append(source, 0.0, Default::default());
        let (output, map) = reorder(job, &[(0, 8), (8, source.chars().count())], Some(false));
        assert_eq!(output.text.lines().count(), 2);
        assert_eq!(
            map.unwrap().visual_to_logical.len(),
            output.text.chars().count() + 1
        );
    }
    #[test]
    fn arabic_shaping_keeps_diacritics_and_source_scalar_identity() {
        let source = "سَلَام";
        let mut job = LayoutJob::default();
        job.append(source, 0.0, Default::default());
        let shaped = shape(job);
        assert_eq!(shaped.text.chars().count(), source.chars().count());
        assert!(shaped.text.contains('\u{64e}'));
    }
    #[test]
    fn rtl_boundary_map_retains_exact_logical_positions() {
        let mut job = LayoutJob::default();
        job.append("אבג", 0.0, Default::default());
        let (output, map) = reorder(job, &[(0, 3)], Some(true));
        let map = map.unwrap();
        assert_eq!(output.text, "גבא");
        assert_eq!(map.logical_to_visual, vec![3, 2, 1, 0]);
        assert_eq!(map.visual_to_logical, vec![3, 2, 1, 0]);
    }
    #[test]
    fn emergency_wrap_cannot_split_a_combining_cluster_or_emoji() {
        for source in ["e\u{301}x", "👩🏽‍💻x"] {
            let mut job = LayoutJob::default();
            job.append(source, 0.0, Default::default());
            let (output, map) = reorder(job, &[(0, 1), (1, source.chars().count())], Some(false));
            assert!(map.is_some());
            assert!(!output.text.starts_with("e\n"));
            assert!(!output.text.starts_with("👩\n"));
            assert_eq!(output.text.replace('\n', ""), source);
        }
    }
}
