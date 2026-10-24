//! Proposed app leaf. Root installs Tab.svg and IdeApp.svg_jobs bridges described in BRIDGE.md.
use super::IdeApp;
use crate::editor::svg_preview::{SvgMode, SvgPane, SvgStamp};

impl IdeApp {
    pub(super) fn svg_preview_owns_input_for(&self, index: usize) -> bool {
        self.tabs
            .get(index)
            .and_then(|tab| tab.svg.as_ref())
            .is_some_and(SvgPane::is_preview)
    }
    pub(super) fn stop_svg_previews(&mut self) {
        self.svg_jobs.cancel_and_drain();
        for tab in &mut self.tabs {
            if let Some(pane) = &mut tab.svg {
                pane.retry();
            }
        }
    }
    /// Call before building editor ViewConfig, so Preview cannot receive editor input.
    pub(super) fn svg_controls(&mut self, ui: &mut egui::Ui, index: usize) -> bool {
        let tab = &mut self.tabs[index];
        if tab.image.is_some()
            || tab.notebook.is_some()
            || tab.state.is_large_file()
            || self.permission_viewers.contains(&tab.state.doc.identity())
        {
            return false;
        }
        if !tab
            .state
            .path
            .as_ref()
            .and_then(|path| path.extension())
            .and_then(|extension| extension.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
        {
            return false;
        }
        let pane = tab.svg.get_or_insert_with(SvgPane::default);
        let previous_mode = pane.mode;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut pane.mode, SvgMode::Source, "Source");
            ui.selectable_value(&mut pane.mode, SvgMode::Preview, "Preview");
        });
        if previous_mode != pane.mode && pane.mode == SvgMode::Preview {
            // A deliberate user retry can supersede a cancelled owner while the
            // previous managed worker still drains. Quit UI owns input otherwise.
            self.svg_jobs.resume();
            pane.retry();
        }
        if previous_mode != pane.mode && pane.mode == SvgMode::Source {
            self.focus_editor = true;
        }
        let stamp = SvgStamp {
            view: tab.view.id,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
        };
        pane.refresh(stamp);
        if pane.mode == SvgMode::Preview {
            if pane.needs_request() {
                if u32::from(tab.state.doc.len()) as usize
                    > crate::editor::svg_preview::MAX_SOURCE_BYTES
                {
                    let request = pane.request(String::new());
                    pane.accept(
                        ui.ctx(),
                        stamp,
                        request,
                        Err(crate::editor::svg_preview::SvgError::TooLarge),
                    );
                    return true;
                }
                if !self.svg_jobs.accepting() {
                    return true;
                }
                let request = pane.request(tab.state.doc.text());
                if let Err(request) = self.svg_jobs.submit(request) {
                    if request.source.len() > crate::editor::svg_preview::MAX_SOURCE_BYTES {
                        pane.accept(
                            ui.ctx(),
                            stamp,
                            request,
                            Err(crate::editor::svg_preview::SvgError::TooLarge),
                        );
                    } else {
                        pane.retry();
                    }
                }
            }
            true
        } else {
            false
        }
    }
    /// Poll once per App frame, then only publish to the exact still-open view/source.
    pub(super) fn poll_svg_previews(&mut self, ctx: &egui::Context) {
        if self.svg_jobs.is_active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        self.svg_jobs
            .retain_views(&self.tabs.iter().map(|tab| tab.view.id).collect::<Vec<_>>());
        if let Some((request, raster)) = self.svg_jobs.poll() {
            if let Some(tab) = self
                .tabs
                .iter_mut()
                .find(|tab| tab.view.id == request.stamp.view)
            {
                let current = SvgStamp {
                    view: tab.view.id,
                    document: tab.state.doc.identity(),
                    version: tab.state.doc.version(),
                };
                if let Some(pane) = &mut tab.svg {
                    pane.accept(ctx, current, request, raster);
                }
            }
        }
    }
    pub(super) fn svg_preview_view(&mut self, ui: &mut egui::Ui, index: usize) {
        if let Some(pane) = &mut self.tabs[index].svg {
            pane.show(ui);
        }
    }
}
