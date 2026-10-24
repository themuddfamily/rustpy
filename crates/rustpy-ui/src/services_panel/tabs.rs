use super::*;
use crate::tool_tabs::{Appearance, Command};
impl ServicesPanel {
    pub(super) fn content_tabs(
        &mut self,
        ui: &mut egui::Ui,
        input: &ServicesInput<'_>,
        events: &mut Vec<ServicesEvent>,
    ) {
        if let Some(id) = &self.selected {
            if !self.tabs.iter().any(|(tab, _)| tab == id) {
                if self.tabs.len() >= 64 {
                    if let Some(index) = self.tabs.iter().position(|(_, tab)| !tab.pinned) {
                        self.tabs.remove(index);
                    }
                }
                if self.tabs.len() < 64 {
                    self.tabs.push((id.clone(), Appearance::default()));
                }
            }
        }
        let mut close = None;
        let mut close_others = None;
        let mut select = None;
        ui.horizontal_wrapped(|ui| {
            for (index, (id, appearance)) in self.tabs.iter_mut().enumerate() {
                let id = id.clone();
                let name = input
                    .resources
                    .iter()
                    .find(|resource| resource.id == id)
                    .map(|resource| resource.name.clone())
                    .or_else(|| match &id {
                        ServiceId::Configuration(name) => Some(name.clone()),
                        ServiceId::Process(id) => input
                            .processes
                            .iter()
                            .find(|process| process.id == *id)
                            .map(|process| process.name.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "Service".into());
                let response = ui
                    .selectable_label(self.selected.as_ref() == Some(&id), appearance.label(&name));
                if response.clicked() {
                    select = Some(id.clone());
                }
                response.context_menu(|ui| match appearance.menu(ui) {
                    Command::Close => close = Some(index),
                    Command::CloseOthers => close_others = Some(index),
                    _ => {}
                });
            }
        });
        if let Some(id) = select {
            self.selected = Some(id.clone());
            events.push(ServicesEvent::Select(id));
        }
        if let Some(index) = close {
            let (id, _) = self.tabs.remove(index);
            if self.selected.as_ref() == Some(&id) {
                self.selected = None;
            }
        }
        if let Some(index) = close_others {
            let keep = self.tabs.get(index).map(|(id, _)| id.clone());
            self.tabs
                .retain(|(id, appearance)| Some(id) == keep.as_ref() || appearance.pinned);
            if self
                .selected
                .as_ref()
                .is_some_and(|selected| !self.tabs.iter().any(|(id, _)| id == selected))
            {
                self.selected = keep;
            }
        }
    }
}
