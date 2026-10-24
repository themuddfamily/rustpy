//! Retain every measured run's process and report when a second run starts.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::run_anything::StopItem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Tests,
    Coverage,
    Profile,
    Concurrency,
}
impl Kind {
    fn index(self) -> usize {
        self as usize
    }
    fn label(self) -> &'static str {
        match self {
            Self::Tests => "Tests",
            Self::Coverage => "Coverage",
            Self::Profile => "Profiler",
            Self::Concurrency => "Concurrency",
        }
    }
    fn for_tool(tool: BottomTool) -> Option<Self> {
        match tool {
            BottomTool::Tests => Some(Self::Tests),
            BottomTool::Coverage => Some(Self::Coverage),
            BottomTool::Profiler => Some(Self::Profile),
            BottomTool::Concurrency => Some(Self::Concurrency),
            _ => None,
        }
    }
}
#[derive(Clone)]
struct Owner {
    id: u64,
    name: String,
}
enum Panel {
    Tests(Box<TestPanel>),
    Coverage(Box<CoveragePanel>),
    Profile(Box<ProfilerPanel>),
    Concurrency(Box<ConcurrencyPanel>),
}
impl Panel {
    fn kind(&self) -> Kind {
        match self {
            Self::Tests(_) => Kind::Tests,
            Self::Coverage(_) => Kind::Coverage,
            Self::Profile(_) => Kind::Profile,
            Self::Concurrency(_) => Kind::Concurrency,
        }
    }
    fn running(&self) -> bool {
        match self {
            Self::Tests(p) => p.is_running(),
            Self::Coverage(p) => p.is_running(),
            Self::Profile(p) => p.is_running(),
            Self::Concurrency(p) => p.is_running(),
        }
    }
    fn stop(&mut self) {
        match self {
            Self::Tests(p) => p.stop_quit_process(),
            Self::Coverage(p) => p.stop(),
            Self::Profile(p) => p.stop(),
            Self::Concurrency(p) => p.stop(),
        }
    }
    fn poll(&mut self, allowed: bool) -> bool {
        match self {
            Self::Tests(p) => {
                p.set_execution_allowed(allowed);
                p.poll() | p.wants_repaint()
            }
            Self::Coverage(p) => p.poll(),
            Self::Profile(p) => p.poll(),
            Self::Concurrency(p) => p.poll(),
        }
    }
    fn stamp(&self) -> String {
        match self {
            Self::Tests(p) => p.quit_process_state(),
            Self::Coverage(p) => format!("{:?}", p.run.as_ref().map(|r| (r.id, r.is_running()))),
            Self::Profile(p) => format!("{:?}", p.run.as_ref().map(|r| (r.id, r.is_running()))),
            Self::Concurrency(p) => format!("{:?}", p.run.as_ref().map(|r| r.lifecycle_stamp())),
        }
    }
}
struct Retained {
    owner: Owner,
    panel: Panel,
}
#[derive(Default)]
pub(super) struct State {
    active: [Option<Owner>; 4],
    retained: Vec<Retained>,
}
impl State {
    fn prune(&mut self) {
        // A completed report may be closed; a live process is never evicted.
        let mut completed = self.retained.iter().filter(|r| !r.panel.running()).count();
        self.retained.retain(|r| {
            if !r.panel.running() && completed > 32 {
                completed -= 1;
                false
            } else {
                true
            }
        });
    }
}
impl IdeApp {
    fn active_measurement_running(&self, kind: Kind) -> bool {
        match kind {
            Kind::Tests => self.test_panel.is_running(),
            Kind::Coverage => self.coverage_panel.is_running(),
            Kind::Profile => self.profiler_panel.is_running(),
            Kind::Concurrency => self.concurrency_panel.is_running(),
        }
    }
    fn take_measurement(&mut self, kind: Kind) -> Panel {
        match kind {
            Kind::Tests => Panel::Tests(Box::new(std::mem::take(&mut self.test_panel))),
            Kind::Coverage => Panel::Coverage(Box::new(std::mem::take(&mut self.coverage_panel))),
            Kind::Profile => Panel::Profile(Box::new(std::mem::take(&mut self.profiler_panel))),
            Kind::Concurrency => {
                Panel::Concurrency(Box::new(std::mem::take(&mut self.concurrency_panel)))
            }
        }
    }
    fn swap_measurement(&mut self, panel: &mut Panel) {
        match panel {
            Panel::Tests(p) => std::mem::swap(p.as_mut(), &mut self.test_panel),
            Panel::Coverage(p) => std::mem::swap(p.as_mut(), &mut self.coverage_panel),
            Panel::Profile(p) => std::mem::swap(p.as_mut(), &mut self.profiler_panel),
            Panel::Concurrency(p) => std::mem::swap(p.as_mut(), &mut self.concurrency_panel),
        }
    }
    pub(super) fn begin_measurement(&mut self, kind: Kind, name: &str) {
        if let Some(owner) = self.measured_sessions.active[kind.index()].take() {
            let panel = self.take_measurement(kind);
            self.measured_sessions
                .retained
                .push(Retained { owner, panel });
        } else if self.active_measurement_running(kind) {
            // Runs started from a panel (rather than a stored configuration)
            // receive an owner before being moved out of the visible slot.
            let id = self.next_session_id;
            self.next_session_id += 1;
            let panel = self.take_measurement(kind);
            self.measured_sessions.retained.push(Retained {
                owner: Owner {
                    id,
                    name: kind.label().into(),
                },
                panel,
            });
        }
        let id = self.next_session_id;
        self.next_session_id += 1;
        self.measured_sessions.active[kind.index()] = Some(Owner {
            id,
            name: name.into(),
        });
        if kind == Kind::Tests {
            // Commit preparation and test-debug events must not mistake a
            // fresh panel for the previously selected panel's launch epoch.
            self.test_panel.next_id = self
                .measured_sessions
                .retained
                .iter()
                .filter_map(|r| {
                    if let Panel::Tests(p) = &r.panel {
                        Some(p.next_id)
                    } else {
                        None
                    }
                })
                .max()
                .unwrap_or(0)
                .max(self.test_panel.next_id);
        }
        self.measured_sessions.prune();
    }
    pub(super) fn measured_configuration_running(&self, name: &str) -> bool {
        [
            Kind::Tests,
            Kind::Coverage,
            Kind::Profile,
            Kind::Concurrency,
        ]
        .into_iter()
        .any(|kind| {
            self.active_measurement_running(kind)
                && self.measured_sessions.active[kind.index()]
                    .as_ref()
                    .is_some_and(|owner| owner.name == name)
        }) || self
            .measured_sessions
            .retained
            .iter()
            .any(|r| r.owner.name == name && r.panel.running())
    }
    pub(super) fn poll_retained_measurements(&mut self, ctx: &egui::Context) {
        let mut repaint = false;
        let mut endpoints = Vec::new();
        for retained in &mut self.measured_sessions.retained {
            repaint |= retained.panel.poll(!self.safe_mode);
            repaint |= retained.panel.running();
            if let Panel::Tests(panel) = &mut retained.panel {
                endpoints.extend(std::mem::take(&mut panel.debugger_endpoints));
            }
        }
        for (host, port) in endpoints {
            self.attach_failed_test_debugger(host, port);
        }
        if repaint {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    pub(super) fn measurement_selector(&mut self, ui: &mut egui::Ui, tool: BottomTool) {
        let Some(kind) = Kind::for_tool(tool) else {
            return;
        };
        if !self
            .measured_sessions
            .retained
            .iter()
            .any(|r| r.panel.kind() == kind)
        {
            return;
        }
        let active = self.measured_sessions.active[kind.index()].clone();
        let title = active
            .as_ref()
            .map(|o| o.name.as_str())
            .unwrap_or(kind.label());
        let mut chosen = None;
        egui::ComboBox::from_id_salt(("measured_run", kind.index()))
            .selected_text(title)
            .show_ui(ui, |ui| {
                ui.selectable_label(true, title)
                    .on_hover_text("Selected run");
                for retained in &self.measured_sessions.retained {
                    if retained.panel.kind() != kind {
                        continue;
                    }
                    let label = format!(
                        "{} · {}{}",
                        retained.owner.name,
                        retained.owner.id,
                        if retained.panel.running() {
                            " · running"
                        } else {
                            ""
                        }
                    );
                    if ui.selectable_label(false, label).clicked() {
                        chosen = Some(retained.owner.id);
                    }
                }
            })
            .combo_named(crate::i18n::tr_static("Run session"));
        if let Some(id) = chosen {
            self.select_measurement(id);
        }
    }
    fn select_measurement(&mut self, id: u64) {
        let Some(index) = self
            .measured_sessions
            .retained
            .iter()
            .position(|r| r.owner.id == id)
        else {
            return;
        };
        let mut retained = self.measured_sessions.retained.remove(index);
        let kind = retained.panel.kind();
        self.swap_measurement(&mut retained.panel);
        let previous = self.measured_sessions.active[kind.index()].replace(retained.owner);
        if let Some(owner) = previous {
            retained.owner = owner;
            self.measured_sessions.retained.push(retained);
        }
        if kind == Kind::Coverage {
            self.coverage_project_badges();
        }
    }
    pub(super) fn measured_stop_items(&mut self) -> Vec<StopItem> {
        let mut items = Vec::new();
        for kind in [
            Kind::Tests,
            Kind::Coverage,
            Kind::Profile,
            Kind::Concurrency,
        ] {
            if !self.active_measurement_running(kind) {
                continue;
            }
            if self.measured_sessions.active[kind.index()].is_none() {
                let id = self.next_session_id;
                self.next_session_id += 1;
                self.measured_sessions.active[kind.index()] = Some(Owner {
                    id,
                    name: kind.label().into(),
                });
            }
            if let Some(owner) = &self.measured_sessions.active[kind.index()] {
                items.push(StopItem::Measured(
                    owner.id,
                    format!("{} ({})", owner.name, kind.label()),
                ));
            }
        }
        for retained in &self.measured_sessions.retained {
            if retained.panel.running() {
                items.push(StopItem::Measured(
                    retained.owner.id,
                    format!(
                        "{} ({})",
                        retained.owner.name,
                        retained.panel.kind().label()
                    ),
                ));
            }
        }
        items
    }
    pub(super) fn stop_measurement(&mut self, id: u64) {
        for kind in [
            Kind::Tests,
            Kind::Coverage,
            Kind::Profile,
            Kind::Concurrency,
        ] {
            if self.measured_sessions.active[kind.index()]
                .as_ref()
                .is_some_and(|o| o.id == id)
            {
                match kind {
                    Kind::Tests => self.test_panel.stop(),
                    Kind::Coverage => self.coverage_panel.stop(),
                    Kind::Profile => self.profiler_panel.stop(),
                    Kind::Concurrency => self.concurrency_panel.stop(),
                }
                return;
            }
        }
        if let Some(retained) = self
            .measured_sessions
            .retained
            .iter_mut()
            .find(|r| r.owner.id == id)
        {
            retained.panel.stop();
        }
    }
    pub(super) fn retained_measurement_labels(&self) -> Vec<String> {
        self.measured_sessions
            .retained
            .iter()
            .filter(|r| r.panel.running())
            .map(|r| format!("{} ({})", r.owner.name, r.panel.kind().label()))
            .collect()
    }
    pub(super) fn retained_measurement_stamp(&self) -> String {
        self.measured_sessions
            .retained
            .iter()
            .map(|r| format!("{}:{:?}:{}", r.owner.id, r.panel.kind(), r.panel.stamp()))
            .collect::<Vec<_>>()
            .join(";")
    }
    pub(super) fn stop_retained_measurements(&mut self) {
        for retained in &mut self.measured_sessions.retained {
            retained.panel.stop();
        }
    }
    pub(super) fn cancel_measurement_project(&mut self) {
        self.stop_retained_measurements();
        self.measured_sessions = State::default();
    }
    pub(super) fn measured_service_rows(&self) -> Vec<crate::services_panel::ResourceRow> {
        use crate::services_panel::{ResourceRow, ServiceId};
        let mut rows = self
            .measured_sessions
            .retained
            .iter()
            .map(|retained| ResourceRow {
                id: ServiceId::Measured(retained.owner.id),
                name: retained.owner.name.clone(),
                group: retained.panel.kind().label().into(),
                status: if retained.panel.running() {
                    "running"
                } else {
                    "finished"
                }
                .into(),
                running: retained.panel.running(),
                restartable: false,
            })
            .collect::<Vec<_>>();
        // The selected Tests panel already exposes its native Services row.
        for kind in [Kind::Coverage, Kind::Profile, Kind::Concurrency] {
            if let Some(owner) = &self.measured_sessions.active[kind.index()] {
                let running = self.active_measurement_running(kind);
                rows.push(ResourceRow {
                    id: ServiceId::Measured(owner.id),
                    name: owner.name.clone(),
                    group: kind.label().into(),
                    status: if running { "running" } else { "finished" }.into(),
                    running,
                    restartable: false,
                });
            }
        }
        rows
    }
    pub(super) fn show_measured_service(&mut self, id: u64) -> Option<BottomTool> {
        let kind = self
            .measured_sessions
            .retained
            .iter()
            .find(|r| r.owner.id == id)
            .map(|r| r.panel.kind())
            .or_else(|| {
                [
                    Kind::Tests,
                    Kind::Coverage,
                    Kind::Profile,
                    Kind::Concurrency,
                ]
                .into_iter()
                .find(|kind| {
                    self.measured_sessions.active[kind.index()]
                        .as_ref()
                        .is_some_and(|o| o.id == id)
                })
            })?;
        self.select_measurement(id);
        Some(match kind {
            Kind::Tests => BottomTool::Tests,
            Kind::Coverage => BottomTool::Coverage,
            Kind::Profile => BottomTool::Profiler,
            Kind::Concurrency => BottomTool::Concurrency,
        })
    }
    pub(super) fn active_test_owner(&self) -> Option<u64> {
        self.measured_sessions.active[Kind::Tests.index()]
            .as_ref()
            .map(|o| o.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selecting_retained_run_restores_its_panel_and_captured_owner() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(directory.path().into()));
        app.begin_measurement(Kind::Tests, "first");
        let first = app.active_test_owner().unwrap();
        app.test_panel.next_id = 17;
        app.begin_measurement(Kind::Tests, "second");
        let second = app.active_test_owner().unwrap();
        app.test_panel.next_id = 18;
        assert_ne!(first, second);
        let rows = app.measured_service_rows();
        assert!(rows
            .iter()
            .any(|row| row.id == crate::services_panel::ServiceId::Measured(first)));
        assert_eq!(app.show_measured_service(first), Some(BottomTool::Tests));
        assert_eq!(app.active_test_owner(), Some(first));
        app.select_measurement(first);
        assert_eq!(app.active_test_owner(), Some(first));
        assert_eq!(app.test_panel.next_id, 17);
        app.select_measurement(second);
        assert_eq!(app.active_test_owner(), Some(second));
        assert_eq!(app.test_panel.next_id, 18);
        app.cancel_measurement_project();
        assert!(app.measured_sessions.retained.is_empty());
        assert!(app.active_test_owner().is_none());
    }

    #[test]
    fn parallel_test_launch_keeps_first_process_and_stop_targets_captured_owner() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("test_hold.py"),
            "import pathlib, time, unittest\nclass Hold(unittest.TestCase):\n def test_wait(self):\n  pathlib.Path('ready').touch()\n  while not pathlib.Path('release').exists(): time.sleep(0.02)\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), Some(directory.path().into()));
        app.safe_mode = false;
        app.begin_measurement(Kind::Tests, "held");
        let first = app.active_test_owner().unwrap();
        let python = Path::new(if cfg!(windows) { "python" } else { "python3" });
        app.test_panel
            .start_with(
                rustpy_test::Framework::Unittest,
                python,
                directory.path(),
                vec!["test_hold.py".into()],
                vec![],
                rustpy_test::TestRunOptions::default(),
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !directory.path().join("ready").exists() && std::time::Instant::now() < deadline {
            app.test_panel.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            directory.path().join("ready").exists(),
            "first test reached its actual body"
        );
        app.begin_measurement(Kind::Tests, "second");
        assert!(app.measured_configuration_running("held"));
        assert!(app
            .retained_measurement_labels()
            .iter()
            .any(|label| label.contains("held")));
        let items = app.measured_stop_items();
        assert!(items
            .iter()
            .any(|item| matches!(item, StopItem::Measured(id, _) if *id == first)));
        app.select_measurement(first);
        assert!(app.test_panel.is_running());
        app.stop_measurement(first);
        std::fs::write(directory.path().join("release"), "").unwrap();
        app.cancel_measurement_project();
        app.test_panel.stop_quit_process();
    }
}
