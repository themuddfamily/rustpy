//! Local, bounded appearance assets; source/editor documents are never rewritten.
use egui::{Pos2, Rect};
use rustpy_settings::menus::Background;
use std::sync::atomic::{AtomicBool, Ordering};
static DECODING: AtomicBool = AtomicBool::new(false);
struct DecodeLease;
impl Drop for DecodeLease {
    fn drop(&mut self) {
        DECODING.store(false, Ordering::Release);
    }
}
struct Loading {
    path: String,
    task: rustpy_core::TaskHandle<egui::ColorImage>,
}
impl Drop for Loading {
    fn drop(&mut self) {
        self.task.cancel();
    }
}
#[derive(Default)]
pub struct BackgroundAsset {
    requested: String,
    loading: Option<Loading>,
    retiring: Vec<Loading>,
    texture: Option<egui::TextureHandle>,
    pub error: Option<String>,
    cancelled: bool,
    waiting: bool,
}
#[derive(Clone)]
struct Paint {
    texture: egui::TextureHandle,
    preferences: Background,
}
fn key(ctx: &egui::Context) -> egui::Id {
    egui::Id::new(("appearance-background-image", ctx.viewport_id()))
}
impl BackgroundAsset {
    pub fn cancel(&mut self) {
        if let Some(load) = self.loading.take() {
            load.task.cancel();
            self.retiring.push(load);
        }
        for load in &self.retiring {
            load.task.cancel();
        }
        self.waiting = false;
        self.cancelled = true;
    }
    pub fn jobs(&self) -> Vec<(u64, String)> {
        self.loading
            .iter()
            .chain(self.retiring.iter())
            .map(|load| (load.task.id(), "Background image decoding".into()))
            .collect()
    }
    pub fn stop(&mut self, id: u64) {
        if self
            .loading
            .as_ref()
            .is_some_and(|load| load.task.id() == id)
        {
            self.cancel();
        }
        for load in &self.retiring {
            if load.task.id() == id {
                load.task.cancel();
            }
        }
    }
    fn poll_retiring(&mut self, ctx: &egui::Context) {
        self.retiring
            .retain_mut(|load| load.task.try_result().is_none());
        if !self.retiring.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
    }
    pub fn retire(&mut self, ctx: &egui::Context) -> bool {
        self.cancel();
        self.poll_retiring(ctx);
        !self.retiring.is_empty()
    }
    pub fn loading(&self) -> bool {
        self.loading.is_some() || self.waiting
    }
    pub fn update(&mut self, ctx: &egui::Context, preferences: &Background) {
        let key = key(ctx);
        self.poll_retiring(ctx);
        if self.requested != preferences.path {
            self.cancel();
            self.requested = preferences.path.clone();
            self.texture = None;
            self.error = None;
            self.cancelled = false;
            self.waiting = !self.requested.trim().is_empty();
            ctx.data_mut(|data| data.remove::<Paint>(key));
        }
        if self.waiting {
            if DECODING
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                self.waiting = false;
                let lease = DecodeLease;
                let task_path = self.requested.clone();
                let task = rustpy_core::TaskManager::global()
                    .task("Background image decoding")
                    .spawn(move |reporter| {
                        let _lease = lease;
                        decode(&task_path, &|| reporter.is_cancelled())
                            .map_err(rustpy_core::TaskError::Failed)
                    });
                self.loading = Some(Loading {
                    path: self.requested.clone(),
                    task,
                });
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        if let Some(load) = &mut self.loading {
            match load.task.try_result() {
                Some(result) => {
                    let path = load.path.clone();
                    self.loading = None;
                    if path == self.requested && !self.cancelled {
                        match result {
                            Ok(image) => {
                                self.texture = Some(ctx.load_texture(
                                    "appearance-background",
                                    image,
                                    egui::TextureOptions::LINEAR_REPEAT,
                                ))
                            }
                            Err(error) => self.error = Some(format!("{error:?}")),
                        }
                    }
                }
                None => ctx.request_repaint_after(std::time::Duration::from_millis(25)),
            }
        }
        ctx.data_mut(|data| {
            if let Some(texture) = &self.texture {
                data.insert_temp(
                    key,
                    Paint {
                        texture: texture.clone(),
                        preferences: preferences.clone(),
                    },
                );
            } else {
                data.remove::<Paint>(key);
            }
        });
    }
    pub fn retry(&mut self) {
        self.requested = String::new();
    }
}
fn decode(path: &str, cancelled: &impl Fn() -> bool) -> Result<egui::ColorImage, String> {
    use std::io::Read;
    let path = std::path::Path::new(path);
    if !path.extension().and_then(|s| s.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico"
        )
    }) {
        return Err("Choose a local PNG, JPEG, GIF, BMP, WebP or ICO image".into());
    }
    let file = rustpy_core::vfs::open_regular_file(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
        return Err("Background images are limited to 16 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 * 1024 || cancelled() {
        return Err("Background image cancelled or too large".into());
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
    if cancelled() {
        return Err("Background image cancelled".into());
    }
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}
pub fn paint(ctx: &egui::Context, painter: &egui::Painter, rect: Rect, editor: bool) {
    let Some(owner) = crate::custom_menus::appearance_owner(ctx) else {
        return;
    };
    let key = egui::Id::new(("appearance-background-image", owner));
    let Some(image) = ctx.data(|data| data.get_temp::<Paint>(key)) else {
        return;
    };
    let p = &image.preferences;
    if (editor && !p.editor)
        || (!editor && !p.empty_frame)
        || p.opacity <= 0.0
        || !rect.is_positive()
    {
        return;
    }
    let size = image.texture.size_vec2();
    let (target, uv) = match p.placement.as_str() {
        "Stretch" => (rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))),
        "Tile" => (
            rect,
            Rect::from_min_max(
                Pos2::ZERO,
                Pos2::new(
                    rect.width() / size.x.max(1.0),
                    rect.height() / size.y.max(1.0),
                ),
            ),
        ),
        _ => {
            let scale = if p.placement == "Fill" {
                (rect.width() / size.x).max(rect.height() / size.y)
            } else {
                (rect.width() / size.x).min(rect.height() / size.y)
            };
            (
                Rect::from_center_size(rect.center(), size * scale),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            )
        }
    };
    let alpha = if p.opacity.is_finite() {
        (p.opacity.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        0
    };
    painter
        .with_clip_rect(rect.intersect(painter.clip_rect()))
        .image(
            image.texture.id(),
            target,
            uv,
            egui::Color32::from_white_alpha(alpha),
        );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn non_image_paths_are_rejected_before_opening() {
        assert!(decode("never-open-user-data.csv", &|| false)
            .unwrap_err()
            .contains("Choose a local"));
    }
    #[test]
    fn cancellation_retains_the_worker_until_its_actual_result() {
        let manager = rustpy_core::TaskManager::new();
        let (release, wait) = std::sync::mpsc::channel();
        let task = manager.spawn("Held image decoder", move |_| {
            wait.recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|error| rustpy_core::TaskError::Failed(error.to_string()))?;
            Ok(egui::ColorImage::from_rgba_unmultiplied(
                [1, 1],
                &[255, 255, 255, 255],
            ))
        });
        let token = task.token().clone();
        let id = task.id();
        let mut asset = BackgroundAsset {
            loading: Some(Loading {
                path: "image.png".into(),
                task,
            }),
            ..Default::default()
        };
        asset.cancel();
        assert!(token.is_cancelled());
        assert_eq!(
            asset.jobs().iter().map(|job| job.0).collect::<Vec<_>>(),
            vec![id]
        );
        let ctx = egui::Context::default();
        assert!(asset.retire(&ctx));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while asset.retire(&ctx) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(asset.jobs().is_empty());
    }
}
