//! Bounded byte/image/notebook comparisons. Call read_pair on the owned worker.
use sha2::{Digest, Sha256};
use std::path::Path;
pub const MAX_ASSET_BYTES: u64 = 32 * 1024 * 1024;
pub struct Bytes {
    pub size: usize,
    pub sha256: String,
    pub first_difference: Option<usize>,
}
pub struct NotebookCell {
    pub label: String,
    pub left: String,
    pub right: String,
}
pub enum Assets {
    Binary {
        left: Bytes,
        right: Bytes,
    },
    Images {
        left: egui::ColorImage,
        right: egui::ColorImage,
        hashes: (Bytes, Bytes),
    },
    Notebook(Vec<NotebookCell>),
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    rustpy_core::vfs::Vfs::new()
        .read(
            path,
            rustpy_core::vfs::ReadLimits {
                max_file_bytes: MAX_ASSET_BYTES as usize,
                max_cached_bytes: 0,
                max_cached_files: 0,
            },
        )
        .map(|snapshot| snapshot.bytes.to_vec())
        .map_err(|error| error.to_string())
}
fn image(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| error.to_string())?
        .into_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}
pub fn read_pair(left: &Path, right: &Path) -> Result<Assets, String> {
    let a = read(left)?;
    let b = read(right)?;
    let offset = a
        .iter()
        .zip(&b)
        .position(|(a, b)| a != b)
        .or_else(|| (a.len() != b.len()).then_some(a.len().min(b.len())));
    let stats = |bytes: &[u8]| Bytes {
        size: bytes.len(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        first_difference: offset,
    };
    if left
        .extension()
        .is_some_and(|extension| extension == "ipynb")
        && right
            .extension()
            .is_some_and(|extension| extension == "ipynb")
    {
        return Ok(Assets::Notebook(notebook(&a, &b)?));
    }
    if let (Ok(left_image), Ok(right_image)) = (image(&a), image(&b)) {
        return Ok(Assets::Images {
            left: left_image,
            right: right_image,
            hashes: (stats(&a), stats(&b)),
        });
    }
    Ok(Assets::Binary {
        left: stats(&a),
        right: stats(&b),
    })
}
fn notebook(a: &[u8], b: &[u8]) -> Result<Vec<NotebookCell>, String> {
    let a: serde_json::Value = serde_json::from_slice(a).map_err(|error| error.to_string())?;
    let b: serde_json::Value = serde_json::from_slice(b).map_err(|error| error.to_string())?;
    let cells = |value: &serde_json::Value| -> Vec<(String, String)> {
        value
            .get("cells")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, cell)| {
                let id = cell
                    .get("id")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("cell {}", index + 1));
                let source = cell
                    .get("source")
                    .map(|value| {
                        if let Some(parts) = value.as_array() {
                            parts
                                .iter()
                                .filter_map(|part| part.as_str())
                                .collect::<String>()
                        } else {
                            value.as_str().unwrap_or("").into()
                        }
                    })
                    .unwrap_or_default();
                let text = format!(
                    "{}\n{}\nOutputs:\n{}",
                    cell.get("cell_type")
                        .and_then(|value| value.as_str())
                        .unwrap_or("unknown"),
                    source,
                    serde_json::to_string_pretty(
                        cell.get("outputs").unwrap_or(&serde_json::Value::Null)
                    )
                    .unwrap_or_default()
                );
                (id, text)
            })
            .collect()
    };
    let a = cells(&a);
    let b = cells(&b);
    let mut ids: Vec<_> = a.iter().map(|(id, _)| id.clone()).collect();
    for (id, _) in &b {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    Ok(ids
        .into_iter()
        .map(|id| NotebookCell {
            left: a
                .iter()
                .find(|(key, _)| *key == id)
                .map(|(_, text)| text.clone())
                .unwrap_or_default(),
            right: b
                .iter()
                .find(|(key, _)| *key == id)
                .map(|(_, text)| text.clone())
                .unwrap_or_default(),
            label: id,
        })
        .collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notebook_cells_align_by_identity() {
        let left=br#"{"cells":[{"id":"a","cell_type":"code","source":["x=1\n"]},{"id":"b","cell_type":"markdown","source":["old"]}]}"#;
        let right=br#"{"cells":[{"id":"b","cell_type":"markdown","source":["new"]},{"id":"a","cell_type":"code","source":["x=1\n"]}]}"#;
        let rows = notebook(left, right).unwrap();
        assert_eq!(rows[0].left, rows[0].right);
        assert_ne!(rows[1].left, rows[1].right);
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;
    #[test]
    fn notebook_fixture_tracks_sources_and_outputs() {
        let cells = notebook(
            include_bytes!("../fixtures/diff_merge/left.ipynb"),
            include_bytes!("../fixtures/diff_merge/right.ipynb"),
        )
        .unwrap();
        assert_eq!(cells.len(), 2);
        assert!(cells[0].left.contains("value = 1"));
        assert!(cells[0].right.contains("value = 2"));
        assert_eq!(cells[1].label, "notes");
    }
    #[test]
    fn binary_hashes_and_first_difference_are_exact() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("a.bin");
        let b = directory.path().join("b.bin");
        std::fs::write(&a, b"a\0b").unwrap();
        std::fs::write(&b, b"a\0c").unwrap();
        match read_pair(&a, &b).unwrap() {
            Assets::Binary { left, right } => {
                assert_eq!(left.size, 3);
                assert_eq!(left.first_difference, Some(2));
                assert_ne!(left.sha256, right.sha256);
            }
            _ => panic!("binary operands"),
        }
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    #[test]
    fn image_pair_decodes_pixels_and_keeps_byte_statistics() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("left.png");
        let b = directory.path().join("right.png");
        image::RgbaImage::from_pixel(2, 1, image::Rgba([255, 0, 0, 255]))
            .save(&a)
            .unwrap();
        image::RgbaImage::from_pixel(2, 1, image::Rgba([0, 255, 0, 255]))
            .save(&b)
            .unwrap();
        match read_pair(&a, &b).unwrap() {
            Assets::Images {
                left,
                right,
                hashes,
            } => {
                assert_eq!(left.size, [2, 1]);
                assert_ne!(left.pixels, right.pixels);
                assert_ne!(hashes.0.sha256, hashes.1.sha256);
            }
            _ => panic!("decoded image pair"),
        }
    }
}
