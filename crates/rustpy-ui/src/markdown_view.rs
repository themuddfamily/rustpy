//! Markdown preview rendering (pulldown-cmark events → egui widgets).

use egui::{Color32, FontId, RichText, Stroke, Vec2};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::theme::UiTheme;

#[derive(Default, Clone)]
struct Inline {
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: Option<String>,
}

struct Block {
    job: egui::text::LayoutJob,
    has_text: bool,
    links: Vec<(std::ops::Range<usize>, String)>,
}

fn new_block() -> Block {
    Block {
        job: egui::text::LayoutJob::default(),
        has_text: false,
        links: Vec::new(),
    }
}

pub fn render_markdown(ui: &mut egui::Ui, theme: &UiTheme, text: &str, base_size: f32) {
    let mut resources = Resources::default();
    let _ = render_document(
        ui,
        theme,
        text,
        base_size,
        None,
        &rustpy_settings::markdown::Settings::default(),
        &mut resources,
    );
}
pub fn render_document(
    ui: &mut egui::Ui,
    theme: &UiTheme,
    text: &str,
    base_size: f32,
    file: Option<&std::path::Path>,
    settings: &rustpy_settings::markdown::Settings,
    resources: &mut Resources,
) -> Vec<PreviewEvent> {
    let mut events = Vec::new();
    let headings = crate::markdown_workspace::headings(text);
    let mut heading_index = 0;

    let opts = crate::markdown_workspace::options();
    let parser = Parser::new_ext(text, opts);
    let code_bg = if theme.dark {
        Color32::from_rgb(0x2B, 0x2D, 0x30)
    } else {
        Color32::from_rgb(0xF0, 0xF0, 0xF0)
    };
    let link_color = theme.accent;
    let mut inline = Inline::default();
    let mut block = new_block();
    let mut heading: Option<HeadingLevel> = None;
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut in_code_block: Option<String> = None;
    let mut code_text = String::new();
    let mut quote_depth = 0usize;
    let mut table: Option<(Vec<Vec<String>>, Vec<String>, bool)> = None; // rows, current row cells, in header
    let mut cell_text = String::new();
    let mut pending_item_prefix: Option<String> = None;

    let flush = |ui: &mut egui::Ui,
                 block: &mut Block,
                 quote_depth: usize,
                 events: &mut Vec<PreviewEvent>| {
        if block.has_text {
            let mut job = std::mem::take(&mut block.job);
            let content = job.text.clone();
            job.wrap.max_width =
                (ui.available_width() - if quote_depth > 0 { 13. } else { 0. }).max(1.);
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            let response = if quote_depth > 0 {
                ui.horizontal(|ui| {
                    ui.add_space(4.);
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(3., 18.), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 1., theme.text_dim);
                    ui.add_space(6.);
                    ui.add(egui::Label::new(galley.clone()).sense(egui::Sense::click()))
                })
                .inner
            } else {
                ui.add(egui::Label::new(galley.clone()).sense(egui::Sense::click()))
            };
            if let Some(position) = response.hover_pos() {
                let index = galley.cursor_from_pos(position - response.rect.min).index.0;
                if let Some((_, url)) = block.links.iter().find(|(range, _)| range.contains(&index))
                {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    response.clone().on_hover_text(url);
                    if response.clicked() {
                        events.push(PreviewEvent::Link(url.clone()));
                    }
                }
            }
            if !block.links.is_empty() {
                response.context_menu(|ui| {
                    for (range, url) in &block.links {
                        let label = content
                            .chars()
                            .skip(range.start)
                            .take(range.len())
                            .collect::<String>();
                        if ui
                            .button(format!("Open {label}"))
                            .on_hover_text(url)
                            .clicked()
                        {
                            events.push(PreviewEvent::Link(url.clone()));
                            ui.close();
                        }
                    }
                });
            }
            ui.add_space(4.);
        }
        block.links.clear();
        block.has_text = false;
    };

    resources.lines.clear();
    let content_top = ui.cursor().top();
    for (ev, range) in parser.into_offset_iter() {
        if matches!(
            &ev,
            Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::CodeBlock(_) | Tag::Table(_))
        ) {
            resources.lines.push((
                text[..range.start].bytes().filter(|b| *b == b'\n').count(),
                ui.cursor().top() - content_top,
            ));
        }
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::FootnoteDefinition(reference) => {
                    flush(ui, &mut block, quote_depth, &mut events);
                    if resources.anchor_target.as_deref()
                        == Some(format!("fn-{reference}").as_str())
                    {
                        ui.scroll_to_cursor(Some(egui::Align::TOP));
                        resources.anchor_target = None;
                    }
                    ui.label(RichText::new(format!("[{reference}]")).strong());
                }
                Tag::Heading { level, .. } => {
                    flush(ui, &mut block, quote_depth, &mut events);
                    heading = Some(level);
                    ui.add_space(6.0);
                }
                Tag::BlockQuote(_) => {
                    flush(ui, &mut block, quote_depth, &mut events);
                    quote_depth += 1;
                }
                Tag::CodeBlock(kind) => {
                    flush(ui, &mut block, quote_depth, &mut events);
                    in_code_block = Some(match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    });
                    code_text.clear();
                }
                Tag::List(start) => {
                    flush(ui, &mut block, quote_depth, &mut events);
                    list_stack.push(start);
                }
                Tag::Item => {
                    flush(ui, &mut block, quote_depth, &mut events);
                    let depth = list_stack.len().max(1);
                    let prefix = match list_stack.last_mut() {
                        Some(Some(n)) => {
                            let s = format!("{n}. ");
                            *n += 1;
                            s
                        }
                        _ => "• ".to_string(),
                    };
                    pending_item_prefix = Some(format!("{}{}", "    ".repeat(depth - 1), prefix));
                }
                Tag::Emphasis => inline.italic = true,
                Tag::Strong => inline.bold = true,
                Tag::Strikethrough => inline.strike = true,
                Tag::Link { dest_url, .. } => inline.link = Some(dest_url.to_string()),
                Tag::Image { dest_url, .. } => {
                    flush(ui, &mut block, quote_depth, &mut events);
                    resources.image(ui, file, &dest_url, settings.remote_images);
                }
                Tag::Table(_) => {
                    flush(ui, &mut block, quote_depth, &mut events);
                    table = Some((Vec::new(), Vec::new(), false));
                }
                Tag::TableHead => {
                    if let Some(t) = &mut table {
                        t.2 = true;
                    }
                }
                Tag::TableRow => {}
                Tag::TableCell => cell_text.clear(),
                _ => {}
            },
            Event::End(tag) => {
                match tag {
                    TagEnd::Paragraph => flush(ui, &mut block, quote_depth, &mut events),
                    TagEnd::Heading(_) => {
                        flush(ui, &mut block, quote_depth, &mut events);
                        if let Some(h) = headings.get(heading_index) {
                            if resources.anchor_target.as_deref() == Some(h.id.as_str()) {
                                ui.scroll_to_cursor(Some(egui::Align::TOP));
                                resources.anchor_target = None;
                            }
                        }
                        heading_index += 1;
                        heading = None;
                        ui.add_space(2.0);
                    }
                    TagEnd::BlockQuote(_) => {
                        flush(ui, &mut block, quote_depth, &mut events);
                        quote_depth = quote_depth.saturating_sub(1);
                    }
                    TagEnd::CodeBlock => {
                        let lang = in_code_block.take().unwrap_or_default();
                        egui::Frame::new()
                        .fill(code_bg)
                        .inner_margin(egui::Margin::same(8))
                        .corner_radius(4)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            if !lang.is_empty() {
                                ui.label(RichText::new(&lang).color(theme.text_dim).small());
                            }
                            ui.label(highlighted_code(&lang,code_text.trim_end_matches('\n'),base_size,theme));
                            if matches!(lang.split_whitespace().next(),Some("python"|"python3"|"py"))&&ui.small_button("Run Python block").clicked() {events.push(PreviewEvent::RunPython(code_text.clone()));}
                            if matches!(lang.as_str(),"mermaid"|"plantuml") {ui.small("Diagram rendering is available in enhanced browser preview");}
                        });
                        ui.add_space(6.0);
                    }
                    TagEnd::List(_) => {
                        flush(ui, &mut block, quote_depth, &mut events);
                        list_stack.pop();
                        if list_stack.is_empty() {
                            ui.add_space(4.0);
                        }
                    }
                    TagEnd::Item => flush(ui, &mut block, quote_depth, &mut events),
                    TagEnd::Emphasis => inline.italic = false,
                    TagEnd::Strong => inline.bold = false,
                    TagEnd::Strikethrough => inline.strike = false,
                    TagEnd::Link => inline.link = None,
                    TagEnd::TableCell => {
                        if let Some(t) = &mut table {
                            t.1.push(std::mem::take(&mut cell_text));
                        }
                    }
                    TagEnd::TableRow | TagEnd::TableHead => {
                        if let Some(t) = &mut table {
                            let row = std::mem::take(&mut t.1);
                            t.0.push(row);
                            t.2 = false;
                        }
                    }
                    TagEnd::Table => {
                        if let Some((rows, _, _)) = table.take() {
                            egui::Grid::new(ui.next_auto_id())
                                .striped(true)
                                .spacing([16.0, 4.0])
                                .show(ui, |ui| {
                                    for (i, row) in rows.iter().enumerate() {
                                        for cell in row {
                                            let t = RichText::new(cell).size(base_size);
                                            ui.label(if i == 0 { t.strong() } else { t });
                                        }
                                        ui.end_row();
                                    }
                                });
                            ui.add_space(6.0);
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if in_code_block.is_some() {
                    code_text.push_str(&t);
                } else if table.is_some() {
                    cell_text.push_str(&t);
                } else {
                    if let Some(prefix) = pending_item_prefix.take() {
                        append(
                            &mut block,
                            &prefix,
                            &Inline::default(),
                            None,
                            base_size,
                            theme,
                            code_bg,
                            link_color,
                        );
                    }
                    if inline.link.is_none() {
                        let mut offset = 0;
                        for (span, url) in crate::markdown_workspace::autolinks(&t) {
                            append(
                                &mut block,
                                &t[offset..span.start],
                                &inline,
                                heading,
                                base_size,
                                theme,
                                code_bg,
                                link_color,
                            );
                            let style = Inline {
                                link: Some(url),
                                ..inline.clone()
                            };
                            append(
                                &mut block,
                                &t[span.clone()],
                                &style,
                                heading,
                                base_size,
                                theme,
                                code_bg,
                                link_color,
                            );
                            offset = span.end;
                        }
                        append(
                            &mut block,
                            &t[offset..],
                            &inline,
                            heading,
                            base_size,
                            theme,
                            code_bg,
                            link_color,
                        );
                    } else {
                        append(
                            &mut block, &t, &inline, heading, base_size, theme, code_bg, link_color,
                        );
                    }
                }
            }
            Event::Code(t) => {
                if table.is_some() {
                    cell_text.push_str(&t);
                } else {
                    if let Some(prefix) = pending_item_prefix.take() {
                        append(
                            &mut block,
                            &prefix,
                            &Inline::default(),
                            None,
                            base_size,
                            theme,
                            code_bg,
                            link_color,
                        );
                    }
                    let mut st = Inline {
                        code: true,
                        ..Default::default()
                    };
                    st.bold = inline.bold;
                    st.link = inline.link.clone();
                    append(
                        &mut block, &t, &st, heading, base_size, theme, code_bg, link_color,
                    );
                }
            }
            Event::SoftBreak => append(
                &mut block, " ", &inline, heading, base_size, theme, code_bg, link_color,
            ),
            Event::HardBreak => append(
                &mut block, "\n", &inline, heading, base_size, theme, code_bg, link_color,
            ),
            Event::Rule => {
                flush(ui, &mut block, quote_depth, &mut events);
                let (r, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 8.0),
                    egui::Sense::hover(),
                );
                ui.painter().line_segment(
                    [
                        egui::Pos2::new(r.left(), r.center().y),
                        egui::Pos2::new(r.right(), r.center().y),
                    ],
                    Stroke::new(1.0, theme.border),
                );
            }
            Event::FootnoteReference(reference) => {
                let style = Inline {
                    link: Some(format!("#fn-{reference}")),
                    ..Default::default()
                };
                append(
                    &mut block,
                    &format!("[{reference}]"),
                    &style,
                    heading,
                    base_size,
                    theme,
                    code_bg,
                    link_color,
                );
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                append(
                    &mut block,
                    &math,
                    &Inline {
                        italic: true,
                        ..Default::default()
                    },
                    heading,
                    base_size,
                    theme,
                    code_bg,
                    link_color,
                );
            }
            Event::TaskListMarker(done) => {
                if let Some(prefix) = pending_item_prefix.take() {
                    append(
                        &mut block,
                        &prefix,
                        &Inline::default(),
                        None,
                        base_size,
                        theme,
                        code_bg,
                        link_color,
                    );
                }
                append(
                    &mut block,
                    if done { "☑ " } else { "☐ " },
                    &Inline::default(),
                    None,
                    base_size,
                    theme,
                    code_bg,
                    link_color,
                );
            }
            Event::Html(h) | Event::InlineHtml(h) => append(
                &mut block,
                &h,
                &Inline {
                    code: true,
                    ..Default::default()
                },
                None,
                base_size,
                theme,
                code_bg,
                link_color,
            ),
        }
    }
    flush(ui, &mut block, quote_depth, &mut events);
    events
}

#[allow(clippy::too_many_arguments)]
fn append(
    block: &mut Block,
    text: &str,
    st: &Inline,
    heading: Option<HeadingLevel>,
    base: f32,
    theme: &UiTheme,
    code_bg: Color32,
    link_color: Color32,
) {
    let size = match heading {
        Some(HeadingLevel::H1) => base * 2.0,
        Some(HeadingLevel::H2) => base * 1.6,
        Some(HeadingLevel::H3) => base * 1.3,
        Some(HeadingLevel::H4) => base * 1.15,
        Some(_) => base * 1.05,
        None => base,
    };
    let font_id = if st.code {
        FontId::monospace(size * 0.95)
    } else {
        FontId::proportional(size)
    };
    let strong = st.bold || heading.is_some();
    let color = if st.link.is_some() {
        link_color
    } else if strong {
        if theme.dark {
            Color32::WHITE
        } else {
            Color32::BLACK
        }
    } else {
        theme.text
    };
    let fmt = egui::text::TextFormat {
        font_id,
        color,
        italics: st.italic,
        underline: if st.link.is_some() {
            Stroke::new(1.0, link_color)
        } else {
            Stroke::NONE
        },
        strikethrough: if st.strike {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        background: if st.code {
            code_bg
        } else {
            Color32::TRANSPARENT
        },
        ..Default::default()
    };
    if let Some(url) = &st.link {
        let start = block.job.text.chars().count();
        block
            .links
            .push((start..start + text.chars().count(), url.clone()));
    }
    block.job.append(text, 0.0, fmt);
    block.has_text = true;
}

#[derive(Clone, Debug)]
pub enum PreviewEvent {
    Link(String),
    RunPython(String),
}
static IMAGE_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
// Compare/exchange keeps the two-worker bound on the declared MSRV without
// deprecated fetch_update or the newer try_update API.
fn acquire_image_permit() -> bool {
    use std::sync::atomic::Ordering;
    let mut count = IMAGE_WORKERS.load(Ordering::Acquire);
    loop {
        if count >= 2 {
            return false;
        }
        match IMAGE_WORKERS.compare_exchange_weak(
            count,
            count + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(actual) => count = actual,
        }
    }
}
struct ImagePermit;
impl Drop for ImagePermit {
    fn drop(&mut self) {
        IMAGE_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
enum ImageState {
    Loading(std::sync::mpsc::Receiver<Result<egui::ColorImage, String>>),
    Ready(egui::TextureHandle),
    Error(String),
}
#[derive(Default)]
pub struct Resources {
    images: std::collections::BTreeMap<String, ImageState>,
    pub anchor_target: Option<String>,
    pub lines: Vec<(usize, f32)>,
}
impl Resources {
    fn image(
        &mut self,
        ui: &mut egui::Ui,
        file: Option<&std::path::Path>,
        url: &str,
        remote_allowed: bool,
    ) {
        let remote = url.starts_with("https://") || url.starts_with("http://");
        if !crate::markdown_workspace::safe_url(url) {
            ui.label("Blocked image scheme");
            return;
        }
        if remote && !remote_allowed {
            ui.label("Remote image disabled in Markdown settings");
            return;
        }
        let local = file
            .and_then(|file| crate::markdown_workspace::local_link(file, url))
            .map(|(p, _)| p);
        let identity = if remote {
            url.into()
        } else if let Some(path) = &local {
            format!(
                "{}:{:?}",
                path.display(),
                std::fs::metadata(path)
                    .ok()
                    .map(|m| (m.len(), m.modified().ok()))
            )
        } else {
            ui.label(format!("Image: {url}"));
            return;
        };
        if !self.images.contains_key(&identity) {
            if self.images.len() >= 64 {
                let evict = self
                    .images
                    .iter()
                    .find(|(_, entry)| !matches!(entry, ImageState::Loading(_)))
                    .map(|(key, _)| key.clone());
                if let Some(key) = evict {
                    self.images.remove(&key);
                } else {
                    ui.label("Markdown image cache is busy");
                    return;
                }
            }
            if !acquire_image_permit() {
                crate::a11y::spinner(ui);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            let permit = ImagePermit;
            let (tx, rx) = std::sync::mpsc::channel();
            let url = url.to_string();
            std::thread::spawn(move || {
                let _permit = permit;
                let result = (|| -> Result<egui::ColorImage, String> {
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    if remote {
                        let response = rustpy_net::agent_builder(&url)
                            .timeout_read(std::time::Duration::from_secs(8))
                            .timeout_connect(std::time::Duration::from_secs(5))
                            .build()
                            .get(&url)
                            .call()
                            .map_err(|e| e.to_string())?;
                        response
                            .into_reader()
                            .take(8 * 1024 * 1024 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|e| e.to_string())?;
                    } else {
                        std::fs::File::open(local.unwrap())
                            .map_err(|e| e.to_string())?
                            .take(8 * 1024 * 1024 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|e| e.to_string())?;
                    }
                    if bytes.len() > 8 * 1024 * 1024 {
                        return Err("Image exceeds 8 MiB".into());
                    }
                    if let Ok(source) = std::str::from_utf8(&bytes) {
                        if source.trim_start().starts_with("<svg")
                            || source.trim_start().starts_with("<?xml")
                        {
                            let raster = crate::editor::svg_preview::rasterize_cancellable(
                                source,
                                std::sync::Arc::new(resvg::usvg::fontdb::Database::new()),
                                rustpy_core::CancellationToken::new(),
                            )
                            .map_err(|e| format!("{e:?}"))?;
                            return Ok(egui::ColorImage::from_rgba_unmultiplied(
                                raster.size,
                                &raster.rgba,
                            ));
                        }
                    }
                    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
                        .with_guessed_format()
                        .map_err(|e| e.to_string())?;
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(8192);
                    limits.max_image_height = Some(8192);
                    limits.max_alloc = Some(64 * 1024 * 1024);
                    reader.limits(limits);
                    let image = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
                    Ok(egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    ))
                })();
                let _ = tx.send(result);
            });
            self.images
                .insert(identity.clone(), ImageState::Loading(rx));
        }
        if let Some(ImageState::Loading(rx)) = self.images.get(&identity) {
            match rx.try_recv() {
                Ok(Ok(image)) => {
                    let texture = ui.ctx().load_texture(&identity, image, Default::default());
                    self.images
                        .insert(identity.clone(), ImageState::Ready(texture));
                }
                Ok(Err(error)) => {
                    self.images
                        .insert(identity.clone(), ImageState::Error(error));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.images.insert(
                        identity.clone(),
                        ImageState::Error("Image worker disconnected".into()),
                    );
                }
                _ => {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        match self.images.get(&identity) {
            Some(ImageState::Ready(texture)) => {
                ui.add(egui::Image::new(texture).max_width(ui.available_width()));
            }
            Some(ImageState::Error(error)) => {
                ui.label(format!("Image {url}: {error}"));
            }
            _ => {
                crate::a11y::spinner(ui);
            }
        }
    }
}
fn highlighted_code(
    language: &str,
    text: &str,
    size: f32,
    theme: &UiTheme,
) -> egui::text::LayoutJob {
    let scheme = if theme.dark {
        rustpy_highlight::ColorScheme::dark()
    } else {
        rustpy_highlight::ColorScheme::light()
    };
    let mut spans =
        rustpy_highlight::highlight_for(language.split_whitespace().next().unwrap_or(""), text);
    spans.sort_by_key(|s| s.range.start());
    let mut job = egui::text::LayoutJob::default();
    let mut offset = 0;
    for span in spans {
        let start = usize::from(span.range.start()).max(offset);
        let end = usize::from(span.range.end()).min(text.len());
        if start > end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        let plain = egui::TextFormat {
            font_id: FontId::monospace(size),
            color: theme.text,
            ..Default::default()
        };
        if start > offset {
            job.append(&text[offset..start], 0., plain.clone());
        }
        let style = scheme.style(span.tag);
        let color = style
            .fg
            .map(|c| Color32::from_rgb(c.0, c.1, c.2))
            .unwrap_or(theme.text);
        job.append(
            &text[start..end],
            0.,
            egui::TextFormat {
                color,
                italics: style.italic,
                ..plain
            },
        );
        offset = end;
    }
    if offset < text.len() {
        job.append(
            &text[offset..],
            0.,
            egui::TextFormat {
                font_id: FontId::monospace(size),
                color: theme.text,
                ..Default::default()
            },
        );
    }
    job
}
