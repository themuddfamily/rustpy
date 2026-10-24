//! External Tools and File Watchers in the application (Settings › Tools):
//! the macro values of the active editor, the `$Prompt$` dialog, the tool
//! menus, and running a tool directly (no shell script) in the Run tool window.

use super::*;
use crate::a11y::DialogWindow;
use crate::external_tools::{self, MacroContext};
use rustpy_settings::ExternalTool;

/// `$Prompt$`: the value asked before a tool runs.
pub(super) struct ToolPromptState {
    pub tool: ExternalTool,
    pub value: String,
    pub context: MacroContext,
    pub owner: super::external_tool_sessions::Owner,
}

impl IdeApp {
    /// Macro values for `file` (default: the active editor's file). Caret and
    /// selection come from the active editor when it shows that file.
    pub(super) fn macro_context(&self, file: Option<&Path>) -> MacroContext {
        let tab = self.active.and_then(|i| self.tabs.get(i));
        let file = file
            .map(Path::to_path_buf)
            .or_else(|| tab.and_then(|t| t.state.path.clone()));
        let tab = tab.filter(|t| t.state.path.is_some() && t.state.path == file);
        let interpreter = self.project.as_ref().and_then(|project| {
            file.as_ref()
                .and_then(|file| project.tree.model().sdk_for(file).map(Path::to_path_buf))
                .or_else(|| project.interpreter.as_ref().map(|sdk| sdk.path.clone()))
        });
        let mut ctx = MacroContext {
            file,
            interpreter,
            project_root: self.project.as_ref().map(|p| p.root.clone()),
            ..Default::default()
        };
        if let Some(t) = tab {
            let s = &t.state;
            let pos = s.primary_position();
            ctx.line = Some(pos.line as usize + 1);
            ctx.column = Some(pos.column as usize + 1);
            let range = s.primary().range();
            if !range.is_empty() {
                let text = s.doc.text();
                ctx.selected_text = text
                    .get(usize::from(range.start())..usize::from(range.end()))
                    .map(str::to_string);
                ctx.selection_lines = Some((
                    s.doc.line_of(range.start()) + 1,
                    s.doc.line_of(range.end()) + 1,
                ));
            }
        }
        ctx
    }

    /// Runs an External Tool on `file` (default: the active editor's file),
    /// asking for `$Prompt$` first when the tool uses it.
    pub(super) fn run_tool(
        &mut self,
        tool: ExternalTool,
        file: Option<PathBuf>,
        prompt: Option<String>,
    ) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status("Trust the project before running external tools");
            return;
        }
        if !self.save_all() {
            return;
        }
        let mut ctx = self.macro_context(file.as_deref());
        if [&tool.program, &tool.arguments, &tool.working_dir]
            .into_iter()
            .chain(tool.options.environment.values())
            .any(|value| value.contains("$ClipboardContent$"))
        {
            ctx.clipboard = self.clipboard_text();
        }
        ctx.prompt = prompt;
        let owner = self.tool_owner(ctx.file.as_deref());
        self.run_tool_context(tool, ctx, owner);
    }

    /// Runs the configured tool called `name`.
    fn run_tool_named(&mut self, name: &str, file: Option<PathBuf>) {
        if self
            .settings
            .external_tools
            .iter()
            .filter(|tool| tool.name == name)
            .count()
            > 1
        {
            self.set_status(format!(
                "External tool name '{name}' is ambiguous; rename duplicate tools"
            ));
            return;
        }
        match self
            .settings
            .external_tools
            .iter()
            .find(|t| t.name == name)
            .cloned()
        {
            Some(tool) => self.run_tool(tool, file, None),
            None => self.set_status(format!("No external tool named \"{name}\"")),
        }
    }

    /// Spawns a [`RunConfigKind::Program`] configuration as a Run tool window tab.
    pub(super) fn start_program_session(&mut self, cfg: RunConfig, show: bool) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status("Safe mode: external tools are disabled until the project is trusted");
            return;
        }
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            self.set_status("Open a project to run external tools");
            return;
        };
        if show {
            // Documents are saved before a tool runs (a File
            // Watcher runs right after its file was saved).
            if !self.save_all() {
                return;
            }
        }
        let id = self.next_session_id;
        self.next_session_id += 1;
        self.run_panel
            .add_session(RunSession::spawn(id, &cfg, None, &root));
        if show {
            self.bottom = Some(BottomTool::Run);
        }
    }

    /// File Watchers: runs the enabled watchers whose pattern matches the saved file.
    pub(super) fn run_file_watchers(&mut self, path: &Path) {
        self.queue_file_watchers(path, false);
    }

    /// Once per frame: offers the tools to the context menus, runs a tool
    /// chosen in a menu.
    pub(super) fn external_tools_tick(&mut self, ctx: &egui::Context) {
        self.file_watchers_tick(ctx);
        self.poll_external_tool_sessions();
        external_tools::publish(ctx, &self.settings.external_tools, &self.keymap);
        if let Some(t) = self.pending_tool.take() {
            self.run_tool(t, None, None);
        }
        if let Some(req) = external_tools::take_request(ctx) {
            self.run_tool_named(&req.name, req.file);
        }
        // A tool's own shortcut, never a key the keymap binds: those ran their
        // action already, or are editor actions the editor runs later this frame.
        if matches!(self.dialog, Dialog::None) {
            if let Some(name) =
                external_tools::take_shortcut(ctx, &self.settings.external_tools, &self.keymap)
            {
                self.run_tool_named(&name, None);
            }
        }
    }

    /// The `$Prompt$` dialog: OK runs the tool with the value.
    pub(super) fn tool_prompt_dialog(&mut self, ctx: &egui::Context, mut st: Box<ToolPromptState>) {
        if !self.tool_owner_current(&st.owner) {
            self.set_status("External tool context changed; invoke it again");
            return;
        }
        let mut open = true;
        let (mut ok, mut cancel) = (false, false);
        egui::Window::new(format!("Run {}", st.tool.name))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label("Enter the value for $Prompt$:");
                let r = ui.add(egui::TextEdit::singleline(&mut st.value).desired_width(360.0));
                r.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    ok = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        ok = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if ok {
            let ToolPromptState {
                tool,
                value,
                mut context,
                owner,
            } = *st;
            context.prompt = Some(value);
            self.run_tool_context(tool, context, owner);
            self.focus_editor = true;
        } else if open && !cancel {
            self.dialog = Dialog::ToolPrompt(st);
        } else {
            self.focus_editor = true;
        }
    }
}
