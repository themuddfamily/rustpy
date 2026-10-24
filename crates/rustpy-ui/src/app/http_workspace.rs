//! Captured HTTP configuration, editor and endpoint consumers.
use super::*;
impl IdeApp {
    pub(super) fn handle_http_events(&mut self, events: Vec<HttpEvent>, ctx: &egui::Context) {
        for event in events {
            match event {
                HttpEvent::Status(message) => self.set_status(message),
                HttpEvent::CreateRequest(text) => self.create_http_request_buffer(&text),
                HttpEvent::OpenBrowser(url) => self.open_url(&url),
                HttpEvent::NetworkSettings => self.handle_action(Action::Settings, ctx),
                HttpEvent::Navigate(path, line) => {
                    self.mark_nav_jump();
                    self.open_file(&path);
                    if let Some(tab) = self
                        .opened_editor_index(&path)
                        .and_then(|index| self.tabs.get_mut(index))
                    {
                        tab.view.cancel_session_restore();
                        tab.state.go_to_line(line, None);
                    }
                }
            }
        }
    }
    pub(super) fn poll_http_workspace(&mut self, ctx: &egui::Context) {
        let root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        if self.http_panel.poll(&root) {
            ctx.request_repaint();
        }
        if self.http_panel.is_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    pub(super) fn run_http_configuration(
        &mut self,
        configuration: &RunConfig,
        path: &Path,
        text: &str,
        line: usize,
        root: &Path,
    ) -> Result<u64, String> {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return Err("Trust the project before running HTTP configurations".into());
        }
        self.http_panel.trusted = true;
        let (variables, errors) = configuration.load_env(root);
        if let Some(error) = errors.first() {
            return Err(error.clone());
        }
        let environment = rustpy_http::runtime::ProcessEnvironment {
            inherit: configuration.workflow.inherit_environment,
            variables: variables.into_iter().collect(),
            working_dir: configuration.working_dir.as_ref().map(|path| {
                if path.is_absolute() {
                    path.clone()
                } else {
                    root.join(path)
                }
            }),
        };
        self.http_panel.set_file(path, text);
        if let Some(name) = configuration
            .type_options
            .as_ref()
            .map(|spec| spec.environment.as_str())
            .filter(|name| !name.is_empty())
        {
            if !self
                .http_panel
                .env_names
                .iter()
                .any(|available| available == name)
            {
                return Err(format!(
                    "HTTP environment '{name}' was not found for {}",
                    path.display()
                ));
            }
            self.http_panel.env_name = name.into();
        }
        let id = self.http_panel.run_configuration_with_environment(
            path,
            text,
            line,
            root,
            &configuration.name,
            configuration.workflow.allow_parallel,
            environment,
        )?;
        self.bottom = Some(BottomTool::HttpClient);
        Ok(id)
    }
    pub(super) fn create_http_request_buffer(&mut self, text: &str) {
        if text.len() > 2 * 1024 * 1024 {
            self.set_status("Generated HTTP file exceeds 2 MiB");
            return;
        }
        let directory = crate::scratches::scratch_dir();
        match crate::scratches::create(&directory, "http") {
            Ok(path) => match std::fs::write(&path, text) {
                Ok(()) => {
                    self.open_file(&path);
                    self.http_panel.set_file(&path, text);
                    self.bottom = Some(BottomTool::HttpClient);
                }
                Err(error) => self.set_status(format!("Cannot create HTTP request: {error}")),
            },
            Err(error) => self.set_status(error.to_string()),
        }
    }
    pub(super) fn http_editor_events(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(index) = self.active.filter(|index| {
            self.tabs
                .get(*index)
                .is_some_and(|tab| tab.state.language == Language::Http && tab.view.focused)
        }) else {
            return false;
        };
        if !self.editor_controls_available() {
            return false;
        }
        ui.input_mut(|input| {
            for event in &mut input.events {
                if let egui::Event::Paste(text) = event {
                    if text.trim_start().starts_with("curl ") {
                        if let Ok(request) = rustpy_http::workspace::from_curl(text.trim()) {
                            *text = rustpy_http::workspace::to_http(&request);
                        }
                    }
                }
            }
        });
        if !ui.input(|input| input.key_pressed(egui::Key::Tab) && input.modifiers.is_none()) {
            return false;
        }
        let tab = &self.tabs[index];
        let offset = tab.state.primary().head();
        if tab.state.primary().has_selection() {
            return false;
        }
        let source = tab.state.doc.text();
        let position = usize::from(offset);
        let start = source[..position]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let abbreviation = source[start..position].trim();
        if !["gtr", "ptr", "mptr"].contains(&abbreviation) {
            return false;
        }
        let Ok(expansion) =
            rustpy_http::workspace::template(abbreviation, "http://localhost:8000/")
        else {
            return false;
        };
        if !self.refresh_file_permissions(index) {
            return false;
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
            self.tabs[index].state.apply_text_edits(
                vec![(
                    TextRange::new(TextSize::new(start as u32), offset),
                    expansion,
                )],
                "Expand HTTP request template",
            );
            return true;
        }
        false
    }
    pub(super) fn stop_http_request(&mut self, id: u64) -> bool {
        self.http_panel.stop(id)
    }
    pub(super) fn show_http_request(&mut self, id: u64) -> bool {
        if self.http_panel.show_execution(id) {
            self.bottom = Some(BottomTool::HttpClient);
            true
        } else {
            false
        }
    }
}
