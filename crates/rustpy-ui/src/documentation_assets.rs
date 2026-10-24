//! Bounded local documentation images. No remote fetches or file handlers.
use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone)]
struct Asset {
    path: PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    texture: Option<egui::TextureHandle>,
}
pub fn local_path(source: &Path, target: &str) -> Option<PathBuf> {
    if target.len() > 4096
        || target.contains(['\0', '\n', '\r'])
        || target.contains("://")
        || target.starts_with("data:")
    {
        return None;
    }
    let root = dunce::canonicalize(source.parent()?).ok()?;
    let path = dunce::canonicalize(root.join(target)).ok()?;
    path.starts_with(root).then_some(path)
}
pub fn texture(ctx: &egui::Context, source: &Path, target: &str) -> Option<egui::TextureHandle> {
    let path = local_path(source, target)?;
    let meta = path.metadata().ok()?;
    if !meta.is_file() || meta.len() > 8 * 1024 * 1024 {
        return None;
    }
    let id = egui::Id::new(("doc-image", &path));
    if let Some(cached) = ctx.data(|data| data.get_temp::<Arc<Asset>>(id)) {
        if cached.path == path
            && cached.len == meta.len()
            && cached.modified == meta.modified().ok()
        {
            return cached.texture.clone();
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 8 * 1024 * 1024 {
        return None;
    }
    let after = path.metadata().ok()?;
    if after.len() != meta.len() || after.modified().ok() != meta.modified().ok() {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(8 * 1024 * 1024);
    reader.limits(limits);
    let texture = reader.decode().ok().map(|image| {
        let pixels = image.to_rgba8();
        ctx.load_texture(
            format!("doc:{}", path.display()),
            egui::ColorImage::from_rgba_unmultiplied(
                [pixels.width() as usize, pixels.height() as usize],
                pixels.as_raw(),
            ),
            egui::TextureOptions::LINEAR,
        )
    });
    // Fixed bounded slots per source; egui frees retired texture handles normally.
    let slot = egui::Id::new((
        "doc-image-slot",
        target.bytes().fold(0u8, |a, b| a.wrapping_add(b)) % 16,
    ));
    if let Some(old) = ctx.data(|data| data.get_temp::<egui::Id>(slot)) {
        ctx.data_mut(|data| data.remove::<Arc<Asset>>(old));
    }
    ctx.data_mut(|data| {
        data.insert_temp(slot, id);
        data.insert_temp(
            id,
            Arc::new(Asset {
                path,
                len: meta.len(),
                modified: meta.modified().ok(),
                texture: texture.clone(),
            }),
        );
    });
    texture
}

pub fn fingerprint(source: &Path, target: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    if let Some(path) = local_path(source, target) {
        path.hash(&mut hash);
        if let Ok(meta) = path.metadata() {
            meta.len().hash(&mut hash);
            meta.modified().ok().hash(&mut hash);
        }
    }
    hash.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_images_decode_pixels_and_reject_escape_network_and_oversize() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("module.py");
        std::fs::write(&source, "\"\"\"Doc.\"\"\"\n").unwrap();
        let path = root.path().join("π.png");
        image::RgbaImage::from_pixel(2, 3, image::Rgba([12, 34, 56, 255]))
            .save(&path)
            .unwrap();
        let ctx = egui::Context::default();
        let texture = texture(&ctx, &source, "π.png").unwrap();
        assert_eq!(texture.size(), [2, 3]);
        assert!(local_path(&source, "https://example.org/image.png").is_none());
        assert!(local_path(&source, "javascript:alert(1)").is_none());
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("image.png");
        std::fs::write(&outside_path, b"not-image").unwrap();
        assert!(local_path(&source, outside_path.to_str().unwrap()).is_none());
        let huge = root.path().join("huge.png");
        std::fs::File::create(&huge)
            .unwrap()
            .set_len(8 * 1024 * 1024 + 1)
            .unwrap();
        assert!(super::texture(&ctx, &source, "huge.png").is_none());
    }
}
