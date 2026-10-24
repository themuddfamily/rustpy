//! Multiple independently owned Python consoles; existing editor sends target the selected tab.
use crate::console_panel::PythonConsole;
use std::ops::{Deref, DerefMut};
pub struct ConsoleWorkspace {
    tabs: Vec<(String, PythonConsole)>,
    active: usize,
    next: usize,
}
impl Default for ConsoleWorkspace {
    fn default() -> Self {
        Self {
            tabs: vec![("Console 1".into(), PythonConsole::default())],
            active: 0,
            next: 2,
        }
    }
}
impl Deref for ConsoleWorkspace {
    type Target = PythonConsole;
    fn deref(&self) -> &PythonConsole {
        &self.tabs[self.active].1
    }
}
impl DerefMut for ConsoleWorkspace {
    fn deref_mut(&mut self) -> &mut PythonConsole {
        &mut self.tabs[self.active].1
    }
}
impl ConsoleWorkspace {
    pub(crate) fn service_rows(&self) -> Vec<(u64, String, bool)> {
        self.tabs
            .iter()
            .filter_map(|(name, console)| {
                Some((console.service_id()?, name.clone(), console.is_running()))
            })
            .collect()
    }
    pub(crate) fn select_service(&mut self, id: u64) -> bool {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|(_, console)| console.service_id() == Some(id))
        {
            if self.active != index {
                self.select(index);
            }
            true
        } else {
            false
        }
    }
    pub fn add(&mut self, label: &str) {
        self.tabs
            .push((format!("{label} {}", self.next), PythonConsole::default()));
        self.next += 1;
        self.active = self.tabs.len() - 1;
    }
    pub fn take_plot_request(&mut self) -> bool {
        let mut requested = false;
        for (_, console) in &mut self.tabs {
            requested |= std::mem::take(&mut console.plots_requested);
        }
        requested
    }
    pub fn scientific_debug_ready(&mut self) -> Option<(u64, String, u16, String)> {
        for (_, console) in &mut self.tabs {
            if console.scientific_debug_code.is_some() {
                if let Some((host, port)) = console.debugger_endpoint.take() {
                    let code = console.scientific_debug_code.take().unwrap();
                    return Some((console.service_id().unwrap_or_default(), host, port, code));
                }
            }
        }
        None
    }
    pub fn scientific_execute(&mut self, owner: u64, code: &str) -> bool {
        if let Some((_, console)) = self
            .tabs
            .iter_mut()
            .find(|(_, console)| console.service_id() == Some(owner) && console.is_running())
        {
            console.execute_fragment(code);
            true
        } else {
            false
        }
    }
    pub fn scientific_capture(&mut self, enabled: bool, interactive: bool) {
        for (_, console) in &mut self.tabs {
            console.configure_scientific_capture(enabled, interactive);
        }
    }
    pub fn scientific_outputs(
        &mut self,
    ) -> (
        Vec<(String, crate::scientific_panel::Origin, serde_json::Value)>,
        Vec<crate::scientific_panel::PlotPayload>,
    ) {
        use base64::Engine;
        let mut tables = Vec::new();
        let mut plots = Vec::new();
        for (label, console) in &mut self.tabs {
            let id = console.service_id().unwrap_or_default();
            if let Some((name, json)) = console.table_json.take() {
                if let Ok(page) = serde_json::from_str(&json) {
                    tables.push((name, crate::scientific_panel::Origin::Console(id), page));
                }
            }
            for (mime, data) in console.take_scientific_plots() {
                if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data) {
                    plots.push(crate::scientific_panel::PlotPayload {
                        source: format!("{label} · {id}"),
                        title: format!("{label} plot"),
                        mime,
                        data,
                    });
                }
            }
        }
        (tables, plots)
    }
    pub fn scientific_refresh(
        &mut self,
        owner: u64,
        name: &str,
        options: serde_json::Value,
    ) -> bool {
        self.tabs
            .iter_mut()
            .find(|(_, console)| console.service_id() == Some(owner))
            .is_some_and(|(_, console)| console.scientific_refresh(name, options))
    }
    pub fn scientific_owner_running(&self, owner: u64) -> bool {
        self.tabs
            .iter()
            .any(|(_, console)| console.service_id() == Some(owner) && console.is_running())
    }
    pub fn names(&self) -> Vec<String> {
        self.tabs.iter().map(|(name, _)| name.clone()).collect()
    }
    pub fn active(&self) -> usize {
        self.active
    }
    pub fn select(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.tabs[self.active].1.focused = false;
            self.active = index;
            self.request_focus();
        }
    }
    pub fn close_active(&mut self) {
        self.tabs[self.active].1.stop();
        if self.tabs.len() > 1 {
            self.tabs.remove(self.active);
            self.active = self.active.min(self.tabs.len() - 1);
        } else {
            self.tabs[0].1 = PythonConsole::default();
        }
    }
    pub(crate) fn configuration_stop_items(&self) -> Vec<crate::run_anything::StopItem> {
        self.tabs
            .iter()
            .filter_map(|(_, console)| {
                Some(crate::run_anything::StopItem::Console(
                    console.service_id()?,
                    console.pending_configuration_name()?.into(),
                ))
            })
            .collect()
    }
    pub(crate) fn running_configuration(&self, name: &str) -> bool {
        self.tabs
            .iter()
            .any(|(_, console)| console.running_configuration(name))
    }
    pub fn any_running(&self) -> bool {
        self.tabs.iter().any(|(_, console)| console.is_running())
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for (_, console) in &mut self.tabs {
            changed |= console.poll();
        }
        changed
    }
    pub(crate) fn stop_quit_process(&mut self) {
        for (_, console) in &mut self.tabs {
            console.stop_quit_process();
        }
    }
    pub(crate) fn quit_process_state(&self) -> String {
        self.tabs
            .iter()
            .map(|(name, console)| format!("{name}:{}", console.quit_process_state()))
            .collect::<Vec<_>>()
            .join("; ")
    }
    pub fn reset(&mut self) {
        self.stop_quit_process();
        *self = Self::default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plot_notification_from_inactive_console_routes_once_without_losing_selection() {
        let mut workspace = ConsoleWorkspace::default();
        let (_, console) = &mut workspace.tabs[0];
        console.plots_requested = true;
        workspace.add("Second");
        assert_eq!(workspace.active(), 1);
        assert!(workspace.take_plot_request());
        assert!(!workspace.take_plot_request());
        assert_eq!(workspace.active(), 1);
    }
    #[test]
    fn independent_prompt_selection_close_and_quit_inventory_preserve_other_consoles() {
        let mut workspace = ConsoleWorkspace::default();
        let (_, console) = &mut workspace.tabs[0];
        console.input = "first = 1".into();
        workspace.add("Django");
        workspace.input = "second = 2".into();
        workspace.select(0);
        assert_eq!(workspace.input, "first = 1");
        assert_eq!(workspace.names(), vec!["Console 1", "Django 2"]);
        workspace.close_active();
        assert_eq!(workspace.input, "second = 2");
        assert!(!workspace.any_running());
        workspace.reset();
        assert_eq!(workspace.names().len(), 1);
        assert!(workspace.input.is_empty());
    }
}

#[cfg(test)]
mod services_tests {
    use super::*;
    #[test]
    fn unstarted_tabs_do_not_claim_a_process_and_stale_ids_do_not_change_selection() {
        let mut workspace = ConsoleWorkspace::default();
        workspace.add("Second");
        assert!(workspace.service_rows().is_empty());
        assert!(!workspace.select_service(u64::MAX));
        assert_eq!(workspace.active(), 1);
        workspace.close_active();
        assert!(!workspace.select_service(u64::MAX));
        assert_eq!(workspace.active(), 0);
    }
}
