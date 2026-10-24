//! SVG rasterization belongs on a worker; `SvgTexture` only publishes accepted pixels.
use resvg::{tiny_skia, usvg};
use std::sync::Arc;

pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_EDGE: u32 = 4096;
pub const MAX_PIXELS: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgError {
    TooLarge,
    Invalid(String),
    Dimensions,
    Allocation,
    ResourceLimit,
    InvalidResource,
    ExternalResource,
    Cancelled,
}

pub struct Raster {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

const MAX_DEPTH: usize = 4;
const MAX_RESOURCE_BYTES: usize = 4 * 1024 * 1024;
#[derive(Default)]
struct Budget {
    bytes: usize,
    pixels: u64,
    error: Option<SvgError>,
    cancelled: rustpy_core::CancellationToken,
}
fn fail(budget: &std::sync::Mutex<Budget>, error: SvgError) {
    budget.lock().unwrap().error.get_or_insert(error);
}
fn charge(budget: &std::sync::Mutex<Budget>, bytes: usize, pixels: u64) -> bool {
    let mut budget = budget.lock().unwrap();
    if budget.cancelled.is_cancelled() {
        budget.error.get_or_insert(SvgError::Cancelled);
        return false;
    }
    let next_bytes = budget.bytes.checked_add(bytes);
    let next_pixels = budget.pixels.checked_add(pixels);
    if next_bytes.is_none_or(|n| n > MAX_RESOURCE_BYTES)
        || next_pixels.is_none_or(|n| n > MAX_PIXELS)
    {
        budget.error.get_or_insert(SvgError::ResourceLimit);
        return false;
    }
    budget.bytes = next_bytes.unwrap();
    budget.pixels = next_pixels.unwrap();
    true
}
fn parse(
    bytes: &[u8],
    fonts: Arc<usvg::fontdb::Database>,
    budget: Arc<std::sync::Mutex<Budget>>,
    depth: usize,
) -> Result<usvg::Tree, SvgError> {
    if depth > MAX_DEPTH {
        return Err(SvgError::ResourceLimit);
    }
    if bytes.len() > MAX_SOURCE_BYTES || !charge(&budget, bytes.len(), 0) {
        return Err(SvgError::ResourceLimit);
    }
    let data_budget = budget.clone();
    let external_budget = budget.clone();
    let child_fonts = fonts.clone();
    let options = usvg::Options {
        fontdb: fonts,
        font_family: crate::fonts::BUNDLED_EDITOR_FONT.into(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(move |mime, bytes, _| {
                if mime == "image/svg+xml" {
                    return match parse(&bytes, child_fonts.clone(), data_budget.clone(), depth + 1)
                    {
                        Ok(tree) => Some(usvg::ImageKind::SVG(tree)),
                        Err(error) => {
                            fail(&data_budget, error);
                            None
                        }
                    };
                }
                let format = match mime {
                    "image/png" => image::ImageFormat::Png,
                    "image/jpeg" | "image/jpg" => image::ImageFormat::Jpeg,
                    "image/gif" => image::ImageFormat::Gif,
                    "image/webp" => image::ImageFormat::WebP,
                    _ => {
                        fail(&data_budget, SvgError::InvalidResource);
                        return None;
                    }
                };
                if bytes.len() > MAX_SOURCE_BYTES {
                    fail(&data_budget, SvgError::ResourceLimit);
                    return None;
                }
                let dimensions =
                    image::ImageReader::with_format(std::io::Cursor::new(bytes.as_slice()), format)
                        .into_dimensions();
                let Ok((width, height)) = dimensions else {
                    fail(&data_budget, SvgError::InvalidResource);
                    return None;
                };
                if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
                    fail(&data_budget, SvgError::ResourceLimit);
                    return None;
                }
                if !charge(
                    &data_budget,
                    bytes.len(),
                    u64::from(width) * u64::from(height),
                ) {
                    return None;
                }
                // Validate the full bounded payload before resvg gets it, so malformed
                // embedded data cannot silently become an omitted image at render time.
                if image::load_from_memory_with_format(&bytes, format).is_err() {
                    fail(&data_budget, SvgError::InvalidResource);
                    return None;
                }
                match format {
                    image::ImageFormat::Png => Some(usvg::ImageKind::PNG(bytes)),
                    image::ImageFormat::Jpeg => Some(usvg::ImageKind::JPEG(bytes)),
                    image::ImageFormat::Gif => Some(usvg::ImageKind::GIF(bytes)),
                    image::ImageFormat::WebP => Some(usvg::ImageKind::WEBP(bytes)),
                    _ => None,
                }
            }),
            resolve_string: Box::new(move |_, _| {
                fail(&external_budget, SvgError::ExternalResource);
                None
            }),
        },
        ..Default::default()
    };
    // Plain UTF-8 SVG only: SVGZ is not enabled, preventing an unbounded inflate.
    let source = std::str::from_utf8(bytes).map_err(|_| SvgError::InvalidResource)?;
    let tree =
        usvg::Tree::from_str(source, &options).map_err(|e| SvgError::Invalid(e.to_string()))?;
    if let Some(error) = budget.lock().unwrap().error.clone() {
        return Err(error);
    }
    let size = tree.size().to_int_size();
    if size.width() > MAX_EDGE || size.height() > MAX_EDGE {
        return Err(SvgError::Dimensions);
    }
    if !charge(
        &budget,
        0,
        u64::from(size.width()) * u64::from(size.height()),
    ) {
        return Err(SvgError::ResourceLimit);
    }
    Ok(tree)
}
/// Call on a worker, using an explicitly loaded font database. No files, URLs,
/// scripts or system-font discovery are accessed during parsing.
#[cfg(test)]
pub fn rasterize(source: &str, fonts: Arc<usvg::fontdb::Database>) -> Result<Raster, SvgError> {
    rasterize_cancellable(source, fonts, rustpy_core::CancellationToken::new())
}
pub(crate) fn rasterize_cancellable(
    source: &str,
    fonts: Arc<usvg::fontdb::Database>,
    cancelled: rustpy_core::CancellationToken,
) -> Result<Raster, SvgError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(SvgError::TooLarge);
    }
    let tree = parse(
        source.as_bytes(),
        fonts,
        Arc::new(std::sync::Mutex::new(Budget {
            cancelled: cancelled.clone(),
            ..Default::default()
        })),
        0,
    )?;
    let size = tree.size().to_int_size();
    let (width, height) = (size.width(), size.height());
    let mut pixels = tiny_skia::Pixmap::new(width, height).ok_or(SvgError::Allocation)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    if cancelled.is_cancelled() {
        return Err(SvgError::Cancelled);
    }
    // tiny-skia stores premultiplied RGBA; unpremultiply for egui's established constructor.
    let mut rgba = pixels.data().to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0.iter_mut() {
        let alpha = u32::from(pixel[3]);
        for component in &mut pixel[..3] {
            if let Some(value) = (u32::from(*component) * 255 + alpha / 2).checked_div(alpha) {
                *component = value.min(255) as u8;
            }
        }
    }
    Ok(Raster {
        size: [width as usize, height as usize],
        rgba,
    })
}

#[derive(Default)]
pub struct SvgTexture {
    texture: Option<egui::TextureHandle>,
    size: [usize; 2],
}
impl SvgTexture {
    /// Caller must check DocumentId, version and request generation before publication.
    pub fn publish(&mut self, ctx: &egui::Context, name: String, raster: Raster) {
        self.size = raster.size;
        self.texture = Some(ctx.load_texture(
            name,
            egui::ColorImage::from_rgba_unmultiplied(raster.size, &raster.rgba),
            egui::TextureOptions::LINEAR,
        ));
    }
    pub fn show(&self, ui: &mut egui::Ui) -> Option<egui::Response> {
        let texture = self.texture.as_ref()?;
        let natural = egui::vec2(self.size[0] as f32, self.size[1] as f32);
        Some(ui.add(egui::Image::new((texture.id(), natural)).fit_to_exact_size(natural)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fonts() -> Arc<usvg::fontdb::Database> {
        let mut database = usvg::fontdb::Database::new();
        database.load_font_data(crate::theme::hack_regular().to_vec());
        Arc::new(database)
    }
    fn svg(body: &str) -> String {
        format!("<svg xmlns='http://www.w3.org/2000/svg' width='64' height='32'>{body}</svg>")
    }
    #[test]
    fn renderer_preserves_fill_and_alpha() {
        let red = rasterize(&svg("<path d='M0 0H64V32H0Z' fill='red'/>"), fonts()).unwrap();
        let blue = rasterize(
            &svg("<rect width='64' height='32' fill='blue' fill-opacity='.5'/>"),
            fonts(),
        )
        .unwrap();
        assert_eq!(&red.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&blue.rgba[..3], &[0, 0, 255]);
        assert!((127..=128).contains(&blue.rgba[3]));
        assert_ne!(red.rgba, blue.rgba);
    }
    #[test]
    fn unicode_text_is_font_rendered_not_discarded() {
        let raster =
            rasterize(&svg("<text x='2' y='22' font-size='18'>πé</text>"), fonts()).unwrap();
        assert!(raster
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] != 0));
        let other =
            rasterize(&svg("<text x='2' y='22' font-size='18'>AB</text>"), fonts()).unwrap();
        assert_ne!(raster.rgba, other.rgba);
    }
    #[test]
    fn malformed_and_oversized_sources_fail_without_texture_allocation() {
        assert!(matches!(
            rasterize("<svg", fonts()),
            Err(SvgError::Invalid(_))
        ));
        assert!(matches!(
            rasterize(&" ".repeat(MAX_SOURCE_BYTES + 1), fonts()),
            Err(SvgError::TooLarge)
        ));
        assert!(matches!(
            rasterize(
                "<svg xmlns='http://www.w3.org/2000/svg' width='99999' height='99999'/>",
                fonts()
            ),
            Err(SvgError::Dimensions)
        ));
    }
    #[test]
    fn file_and_network_resources_are_explicitly_omitted() {
        for href in [
            "file:///etc/passwd",
            "https://example.com/a.png",
            "relative.png",
        ] {
            assert!(matches!(
                rasterize(
                    &svg(&format!("<image href='{href}' width='10' height='10'/>")),
                    fonts()
                ),
                Err(SvgError::ExternalResource)
            ));
        }
    }
    #[test]
    fn texture_widget_paints_the_accepted_raster() {
        let ctx = egui::Context::default();
        let mut widget = SvgTexture::default();
        widget.publish(
            &ctx,
            "svg-fixture".into(),
            rasterize(&svg("<rect width='64' height='32' fill='red'/>"), fonts()).unwrap(),
        );
        let texture = widget.texture.as_ref().unwrap().id();
        let mut shown = false;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            shown = widget.show(ui).is_some();
        });
        // egui 0.36 emits a textured RectShape; inspect the actual tessellated
        // primitive rather than assuming Image paints a raw Shape::Mesh.
        let primitives = ctx.tessellate(output.shapes.clone(), output.pixels_per_point);
        let painted = primitives.iter().any(|primitive| {
            matches!(&primitive.primitive, egui::epaint::Primitive::Mesh(mesh)
                if mesh.texture_id == texture && !mesh.indices.is_empty()
                    && mesh.is_valid() && mesh.calc_bounds().width() > 0.0
                    && mesh.calc_bounds().height() > 0.0
                    && primitive.clip_rect.intersects(mesh.calc_bounds()))
        });
        output.drop_without_applying_deltas();
        assert!(shown);
        assert!(painted);
    }
    fn data_url(mime: &str, bytes: &[u8]) -> String {
        let encoded: String = bytes.iter().map(|byte| format!("%{byte:02X}")).collect();
        format!("data:{mime},{encoded}")
    }
    #[test]
    fn embedded_png_and_nested_svg_render_actual_pixels() {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([0, 255, 0, 255]),
        ));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let png = data_url("image/png", encoded.get_ref());
        let raster = rasterize(
            &svg(&format!(
                "<image href='{png}' width='64' height='32' preserveAspectRatio='none'/>"
            )),
            fonts(),
        )
        .unwrap();
        assert_eq!(&raster.rgba[(16 * 64 + 32) * 4..][..4], &[0, 255, 0, 255]);
        let nested = data_url(
            "image/svg+xml",
            svg("<rect width='64' height='32' fill='blue'/>").as_bytes(),
        );
        let raster = rasterize(
            &svg(&format!("<image href='{nested}' width='64' height='32'/>")),
            fonts(),
        )
        .unwrap();
        assert_eq!(&raster.rgba[(16 * 64 + 32) * 4..][..4], &[0, 0, 255, 255]);
    }
    #[test]
    fn nested_depth_and_aggregate_pixels_fail_the_whole_preview() {
        let mut source = svg("<rect width='64' height='32' fill='red'/>");
        for _ in 0..=MAX_DEPTH {
            let href = data_url("image/svg+xml", source.as_bytes());
            source = svg(&format!("<image href='{href}' width='64' height='32'/>"));
        }
        assert!(matches!(
            rasterize(&source, fonts()),
            Err(SvgError::ResourceLimit)
        ));
        let large = data_url(
            "image/svg+xml",
            b"<svg xmlns='http://www.w3.org/2000/svg' width='2500' height='2500'/>",
        );
        assert!(matches!(
            rasterize(
                &svg(&format!("<image href='{large}'/><image href='{large}'/>")),
                fonts()
            ),
            Err(SvgError::ResourceLimit)
        ));
    }
}

// Stamped per-view presentation and App-owned single renderer worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SvgStamp {
    pub view: egui::Id,
    pub document: rustpy_text::DocumentId,
    pub version: u64,
}
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgMode {
    #[default]
    Source,
    Preview,
}
pub struct SvgRequest {
    pub stamp: SvgStamp,
    pub generation: u64,
    pub source: String,
}
#[derive(Default)]
pub struct SvgPane {
    pub mode: SvgMode,
    stamp: Option<SvgStamp>,
    generation: u64,
    requested: bool,
    texture: SvgTexture,
    error: Option<SvgError>,
}
impl SvgPane {
    pub fn split_view(&self) -> Self {
        Self {
            mode: self.mode,
            ..Default::default()
        }
    }
    pub fn is_preview(&self) -> bool {
        self.mode == SvgMode::Preview
    }
    pub fn has_texture(&self) -> bool {
        self.texture.texture.is_some()
    }
    pub fn refresh(&mut self, stamp: SvgStamp) {
        if self.stamp != Some(stamp) {
            self.stamp = Some(stamp);
            self.generation = self.generation.wrapping_add(1);
            self.requested = false;
            self.texture = SvgTexture::default();
            self.error = None;
        }
    }
    pub fn needs_request(&self) -> bool {
        self.stamp.is_some() && !self.requested
    }
    pub fn retry(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.requested = false;
    }
    pub fn request(&mut self, source: String) -> SvgRequest {
        self.requested = true;
        SvgRequest {
            stamp: self.stamp.expect("refresh before request"),
            generation: self.generation,
            source,
        }
    }
    pub fn accept(
        &mut self,
        ctx: &egui::Context,
        current: SvgStamp,
        request: SvgRequest,
        result: Result<Raster, SvgError>,
    ) -> bool {
        if self.stamp != Some(current)
            || request.stamp != current
            || request.generation != self.generation
        {
            return false;
        }
        match result {
            Ok(raster) => {
                self.error = None;
                self.texture.publish(
                    ctx,
                    format!("svg-{:?}-{}", current.view, self.generation),
                    raster,
                );
            }
            Err(SvgError::Cancelled) => {
                self.texture = SvgTexture::default();
                self.error = None;
                self.requested = false;
            }
            Err(error) => {
                self.texture = SvgTexture::default();
                self.error = Some(error);
            }
        }
        true
    }
    pub fn show(&self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.label(format!(
                "Cannot preview this SVG: {error:?}. Source remains available."
            ));
        } else if self.texture.texture.is_some() {
            egui::ScrollArea::both().show(ui, |ui| {
                self.texture.show(ui);
            });
        } else {
            ui.label("Rendering SVG…");
        }
    }
}

/// Single draining worker across ALL SVG tabs. New snapshots replace queued ones
/// for the same view. Never discard another view's queued request silently.
pub struct SvgJobs {
    fonts: Arc<usvg::fontdb::Database>,
    worker: Option<rustpy_core::TaskHandle<Result<Raster, SvgError>>>,
    queue: std::collections::VecDeque<SvgRequest>,
    running_request: Option<SvgRequest>,
    stopping: bool,
    owner: rustpy_core::CancellationToken,
    owner_epoch: u64,
    running_epoch: u64,
    manager: rustpy_core::TaskManager,
    completed: Option<rustpy_core::TaskResult<Result<Raster, SvgError>>>,
}
impl SvgJobs {
    pub fn new(fonts: Arc<usvg::fontdb::Database>) -> Self {
        Self {
            fonts,
            worker: None,
            queue: Default::default(),
            running_request: None,
            stopping: false,
            owner: rustpy_core::CancellationToken::new(),
            owner_epoch: 0,
            running_epoch: 0,
            manager: rustpy_core::TaskManager::global().clone(),
            completed: None,
        }
    }
    pub fn submit(&mut self, request: SvgRequest) -> Result<(), SvgRequest> {
        if self.stopping {
            return Err(request);
        }
        if request.source.len() > MAX_SOURCE_BYTES
            || self.queue.len() >= 8
                && !self
                    .queue
                    .iter()
                    .any(|old| old.stamp.view == request.stamp.view)
        {
            return Err(request);
        }
        self.queue
            .retain(|old| old.stamp.view != request.stamp.view);
        self.queue.push_back(request);
        self.start_next();
        Ok(())
    }
    fn start_next(&mut self) {
        if self.worker.is_none() && !self.stopping {
            if let Some(request) = self.queue.pop_front() {
                let fonts = self.fonts.clone();
                let source = request.source.clone();
                self.running_request = Some(request);
                self.running_epoch = self.owner_epoch;
                self.worker = Some(
                    self.manager
                        .task("SVG preview")
                        .child_of(&self.owner)
                        .spawn(move |progress| {
                            progress.check()?;
                            let result =
                                rasterize_cancellable(&source, fonts, progress.token().clone());
                            progress.check()?;
                            Ok(result)
                        }),
                );
            }
        }
    }
    pub fn bundled() -> Self {
        let mut fonts = usvg::fontdb::Database::new();
        for (_, _, _, bytes) in crate::theme::bundled_mono_faces() {
            fonts.load_font_data(bytes.to_vec());
        }
        Self::new(Arc::new(fonts))
    }
    /// Quit provider begins cancellation, then waits for actual worker drainage.
    pub fn cancel_and_drain(&mut self) {
        self.stopping = true;
        self.owner.cancel();
        self.queue.clear();
        if let Some(worker) = &self.worker {
            worker.cancel();
        }
    }
    /// Abort quit permits fresh requests; the old worker still drains before replacement.
    pub fn resume(&mut self) {
        self.stopping = false;
        self.owner = rustpy_core::CancellationToken::new();
        self.owner_epoch = self.owner_epoch.wrapping_add(1);
        self.start_next();
    }
    pub fn accepting(&self) -> bool {
        !self.stopping
    }
    pub fn lifecycle_stamp(&self) -> (Option<u64>, bool, usize, bool) {
        (
            self.worker.as_ref().map(|worker| worker.id()),
            self.worker
                .as_ref()
                .is_some_and(|worker| self.manager.is_running(worker.id())),
            self.queue.len(),
            self.stopping,
        )
    }
    pub fn is_active(&self) -> bool {
        self.worker.is_some() || !self.queue.is_empty()
    }
    pub fn retain_views(&mut self, views: &[egui::Id]) {
        self.queue
            .retain(|request| views.contains(&request.stamp.view));
    }
    pub fn poll(&mut self) -> Option<(SvgRequest, Result<Raster, SvgError>)> {
        let worker = self.worker.as_mut()?;
        if worker.is_cancelled() && self.running_epoch == self.owner_epoch {
            self.stopping = true;
            self.queue.clear();
        }
        if self.completed.is_none() {
            self.completed = worker.try_result();
        }
        if self.completed.is_none() || self.manager.is_running(worker.id()) {
            return None;
        }
        let result = self.completed.take().unwrap();
        let request = self
            .running_request
            .take()
            .expect("managed worker owns request");
        self.worker = None;
        let result = match result {
            Ok(result) => result,
            Err(rustpy_core::TaskError::Cancelled) => {
                if self.running_epoch == self.owner_epoch {
                    self.stopping = true;
                    self.queue.clear();
                }
                Err(SvgError::Cancelled)
            }
            Err(error) => Err(SvgError::Invalid(error.to_string())),
        };
        self.start_next();
        Some((request, result))
    }
}
impl Drop for SvgJobs {
    fn drop(&mut self) {
        self.cancel_and_drain();
    }
}
#[cfg(test)]
mod pane_tests {
    use super::*;
    use crate::editor::{EditOptions, EditorState, Language};
    #[test]
    fn explicit_resume_before_old_retirement_preserves_the_new_request() {
        let mut jobs = SvgJobs::bundled();
        let editor = EditorState::from_text("", Language::Other);
        let stamp = SvgStamp {
            view: egui::Id::new("resumed"),
            document: editor.doc.identity(),
            version: editor.doc.version(),
        };
        let red = "<svg xmlns='http://www.w3.org/2000/svg' width='8' height='8'><rect width='8' height='8' fill='red'/></svg>";
        let green = red.replace("red", "lime");
        assert!(jobs
            .submit(SvgRequest {
                stamp,
                generation: 0,
                source: red.into()
            })
            .is_ok());
        let old_id = jobs.worker.as_ref().unwrap().id();
        jobs.cancel_and_drain();
        assert!(
            jobs.worker.is_some(),
            "old result and handle remain owned before poll"
        );
        jobs.resume();
        assert!(jobs
            .submit(SvgRequest {
                stamp,
                generation: 1,
                source: green
            })
            .is_ok());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut new_result = None;
        while jobs.is_active() && std::time::Instant::now() < deadline {
            if let Some((request, result)) = jobs.poll() {
                if request.generation == 1 {
                    new_result = Some(result);
                }
            }
            std::thread::yield_now();
        }
        assert!(!jobs.is_active());
        assert!(jobs.accepting());
        assert!(!rustpy_core::TaskManager::global().is_running(old_id));
        let raster = new_result.unwrap().unwrap();
        assert_eq!(&raster.rgba[..4], &[0, 255, 0, 255]);
    }
    #[test]
    fn managed_svg_owners_cancel_independently_and_acknowledge_retirement() {
        let mut first = SvgJobs::bundled();
        let mut second = SvgJobs::bundled();
        let editor = EditorState::from_text("", Language::Other);
        let stamp = SvgStamp {
            view: egui::Id::new("first"),
            document: editor.doc.identity(),
            version: editor.doc.version(),
        };
        let source = "<svg xmlns='http://www.w3.org/2000/svg' width='8' height='8'/>";
        assert!(first
            .submit(SvgRequest {
                stamp,
                generation: 0,
                source: source.into()
            })
            .is_ok());
        assert!(second
            .submit(SvgRequest {
                stamp: SvgStamp {
                    view: egui::Id::new("second"),
                    ..stamp
                },
                generation: 0,
                source: source.into()
            })
            .is_ok());
        let first_id = first.worker.as_ref().unwrap().id();
        let second_id = second.worker.as_ref().unwrap().id();
        first.cancel_and_drain();
        assert!(!second.worker.as_ref().unwrap().is_cancelled());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut second_result = None;
        while (first.is_active() || second.is_active()) && std::time::Instant::now() < deadline {
            first.poll();
            if let Some(result) = second.poll() {
                second_result = Some(result);
            }
            std::thread::yield_now();
        }
        assert!(!first.is_active());
        assert!(!second.is_active());
        assert!(!rustpy_core::TaskManager::global().is_running(first_id));
        assert!(!rustpy_core::TaskManager::global().is_running(second_id));
        assert!(second_result.unwrap().1.is_ok());
    }
    #[test]
    fn per_view_mode_and_stale_publication_preserve_real_document_history() {
        let source = "<svg xmlns='http://www.w3.org/2000/svg' width='1' height='1'/>";
        let mut editor = EditorState::from_text(source, Language::Other);
        let peer = editor.doc.share();
        let identity = editor.doc.identity();
        let selection = editor.carets.clone();
        let stamp = SvgStamp {
            view: egui::Id::new("svg-a"),
            document: identity,
            version: editor.doc.version(),
        };
        let mut pane = SvgPane::default();
        pane.refresh(stamp);
        pane.mode = SvgMode::Preview;
        let request = pane.request(editor.doc.text());
        let mut other = SvgPane::default();
        other.refresh(SvgStamp {
            view: egui::Id::new("svg-b"),
            ..stamp
        });
        assert_eq!(other.mode, SvgMode::Source);
        assert_eq!(editor.carets, selection);
        assert_eq!(editor.doc.identity(), identity);
        editor.insert_text("<!--π-->", &EditOptions::default());
        assert_eq!(peer.text(), editor.doc.text());
        let current = SvgStamp {
            version: editor.doc.version(),
            ..stamp
        };
        pane.refresh(current);
        assert!(!pane.accept(
            &egui::Context::default(),
            current,
            request,
            Err(SvgError::Allocation)
        ));
        assert!(pane.error.is_none());
        assert!(editor.undo());
        assert_eq!(editor.doc.text(), source);
        assert!(editor.redo());
        assert_eq!(peer.text(), editor.doc.text());
        pane.mode = SvgMode::Source;
        assert_eq!(editor.doc.identity(), identity);
    }
}
