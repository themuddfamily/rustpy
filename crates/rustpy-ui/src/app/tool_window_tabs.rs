//! Session tabs retain backend identity and never close another running process.
use super::*;
use crate::tool_tabs::Command;
impl IdeApp {
    pub(super) fn debug_tool_content_tabs(&mut self, ui: &mut egui::Ui) {
        let mut rows: Vec<_> = self
            .debug
            .iter()
            .map(|session| (session.id, session.name.clone(), session.is_active(), true))
            .collect();
        rows.extend(
            self.debug_sessions
                .iter()
                .map(|(session, _)| (session.id, session.name.clone(), session.is_active(), false)),
        );
        let mut select = None;
        let mut close = None;
        let mut close_others = None;
        ui.horizontal_wrapped(|ui| {
            for (id, name, _, selected) in &rows {
                let appearance = self.layout.tools.debug_tabs.entry(*id).or_default();
                let response = ui.selectable_label(*selected, appearance.label(name));
                if response.clicked() {
                    select = Some(*id);
                }
                response.context_menu(|ui| match appearance.menu(ui) {
                    Command::Close => close = Some(*id),
                    Command::CloseOthers => close_others = Some(*id),
                    _ => {}
                });
            }
        });
        if let Some(id) = select {
            self.select_debug_session(id);
        }
        let mut closing = Vec::new();
        if let Some(keep) = close_others {
            closing.extend(
                rows.iter()
                    .filter(|(id, _, running, _)| {
                        *id != keep
                            && !*running
                            && !self
                                .layout
                                .tools
                                .debug_tabs
                                .get(id)
                                .is_some_and(|appearance| appearance.pinned)
                    })
                    .map(|(id, _, _, _)| *id),
            );
        }
        if let Some(id) = close {
            closing.push(id);
        }
        for id in closing {
            if self.debug.as_ref().is_some_and(|session| session.id == id) {
                if let Some(mut session) = self.debug.take() {
                    session.stop();
                }
                self.debug_panel = DebugPanel::default();
                self.exec_point = None;
                self.exec_point_column = None;
            } else if let Some(index) = self
                .debug_sessions
                .iter()
                .position(|(session, _)| session.id == id)
            {
                let (mut session, _) = self.debug_sessions.remove(index);
                session.stop();
            }
            self.layout.tools.debug_tabs.remove(&id);
        }
    }
}
