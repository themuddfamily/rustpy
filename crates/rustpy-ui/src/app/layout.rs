//! Tool-window layout and view modes (roadmap §5.1 "Tool windows framework",
//! "Appearance"). The layout (left and bottom tool windows, sizes, floating or
//! maximized, the last one used) is saved per project in `workspace.json` and
//! restored when the project opens. Alt+1 & co. show or hide the left tool
//! window of this project only; Hide All Tool Windows restores the previous
//! layout on the next press; Jump to Last Tool Window (F12), Commit (Alt+0),
//! Distraction-Free and Presentation modes; a confirmation before exiting
//! while processes run; and the memory indicator in the status bar.

use super::*;
use rustpy_settings::ToolWindowLayout;

/// A tool window, for Jump to Last Tool Window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum ToolRef {
    Ai,
    Learn,
    Left(LeftTool),
    Bottom(BottomTool),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub(super) enum ViewMode {
    #[default]
    Normal,
    /// Only the editor (centered at the right margin) and the menu bar.
    DistractionFree,
    /// Distraction-free, full screen, without the menu bar, with larger editor text.
    Presentation,
    /// Distraction-free and fullscreen without changing the editor font.
    Zen,
}

/// Editor text size in Presentation mode (unless the zoom is already larger).
pub(super) const PRESENTATION_FONT_SIZE: f32 = 24.0;

/// The empty Terminal of an untrusted project (no shell is started for it).
const SAFE_MODE_TERMINAL_NOTE: &str = "Safe mode: no shell was started for this untrusted project. Press + to start one, or trust the project (File › Trust Project).";

/// Tool windows that are shown (stashed by Hide All Tool Windows and the view modes).
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    extra: Vec<String>,
    left: bool,
    bottom: Option<BottomTool>,
    ai: bool,
    learn: bool,
}

impl Shown {
    fn any(&self) -> bool {
        self.left || self.bottom.is_some() || self.ai || self.learn || !self.extra.is_empty()
    }
}

#[derive(Clone, Debug)]
struct ModeStash {
    shown: Shown,
    fullscreen: bool,
    zoom: f32,
}

/// Reversible UI state, separate from process/window ownership.
#[derive(Clone)]
pub(super) struct ViewSnapshot {
    mode: ViewMode,
    mode_stash: Option<ModeStash>,
    hidden_all: Option<Shown>,
    shown: Shown,
    left_tool: LeftTool,
    last_tool: Option<ToolRef>,
    framework: rustpy_settings::tool_windows::Framework,
    left_width: f32,
    bottom_height: f32,
    floating_bottom: bool,
    bottom_maximized: bool,
    zoom: f32,
}

/// An answer to "Save changes to …?".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum CloseAnswer {
    Save,
    Discard,
    Cancel,
}

/// The "Exit RustPy" question.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ExitPrompt {
    /// Processes that would be terminated ("Run 'main'", "2 terminal sessions", …).
    pub processes: Vec<String>,
    /// How many processes the list stands for.
    pub process_count: usize,
    pub dont_ask: bool,
    /// The Cancel button (from the last frame): Enter answers Cancel while it
    /// has the keyboard focus, else Exit.
    pub cancel_id: Option<egui::Id>,
}

pub(super) struct LayoutState {
    pub tools: super::tool_windows::State,
    /// The left tool window (Project, Structure or Bookmarks) is shown.
    pub left_visible: bool,
    /// Sizes saved with the project; applied as the panels' sizes when it opens.
    pub left_width: f32,
    pub bottom_height: f32,
    /// What Hide All Tool Windows hid (the next press brings it back).
    hidden_all: Option<Shown>,
    pub last_tool: Option<ToolRef>,
    /// Tool windows shown last frame (to notice which one opened).
    seen: Option<(bool, LeftTool, Option<BottomTool>)>,
    pub view_mode: ViewMode,
    pub zen: Option<super::zen_mode::State>,
    pub zen_native_restore: Option<bool>,
    mode_stash: Option<ModeStash>,
    pub exit_prompt: Option<ExitPrompt>,
    /// Restored sizes replace egui's remembered panel sizes on the next frame.
    resize_panels: bool,
    /// Memory indicator: (sample time, resident bytes).
    pub(super) memory: Option<(f64, Option<u64>)>,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            tools: Default::default(),
            left_visible: true,
            left_width: 260.0,
            bottom_height: 300.0,
            hidden_all: None,
            last_tool: None,
            seen: None,
            view_mode: ViewMode::Normal,
            zen: None,
            zen_native_restore: None,
            mode_stash: None,
            exit_prompt: None,
            resize_panels: false,
            memory: None,
        }
    }
}

/// Bottom tool windows a saved layout can name (by title).
pub(super) const BOTTOM_TOOLS: &[BottomTool] = &[
    BottomTool::Terminal,
    BottomTool::Run,
    BottomTool::Debug,
    BottomTool::Tests,
    BottomTool::Packages,
    BottomTool::Hierarchy,
    BottomTool::Services,
    BottomTool::Coverage,
    BottomTool::Profiler,
    BottomTool::Concurrency,
    BottomTool::WebPreview,
    BottomTool::HttpClient,
    BottomTool::Database,
    BottomTool::Diagram,
    BottomTool::Endpoints,
    BottomTool::Problems,
    BottomTool::Todo,
    BottomTool::PythonConsole,
    BottomTool::Git,
    BottomTool::Branches,
    BottomTool::OtherVcs,
    BottomTool::PullRequests,
    BottomTool::Find,
    BottomTool::Notifications,
    BottomTool::Documentation,
    BottomTool::ManagePy,
    BottomTool::SciView,
];

pub(super) const LEFT_TOOLS: [(LeftTool, &str); 3] = [
    (LeftTool::Project, "Project"),
    (LeftTool::Structure, "Structure"),
    (LeftTool::Bookmarks, "Bookmarks"),
];

pub(super) fn bottom_tool_by_title(title: &str) -> Option<BottomTool> {
    BOTTOM_TOOLS
        .iter()
        .copied()
        .find(|t| t.title() == title || t.stable_name() == title)
}

pub(super) fn left_tool_name(tool: LeftTool) -> &'static str {
    LEFT_TOOLS
        .iter()
        .find(|(t, _)| *t == tool)
        .map(|(_, n)| *n)
        .unwrap_or("Project")
}

fn left_tool_by_name(name: &str) -> Option<LeftTool> {
    LEFT_TOOLS.iter().find(|(_, n)| *n == name).map(|(t, _)| *t)
}

pub(super) fn tool_ref_id(t: ToolRef) -> String {
    match t {
        ToolRef::Ai => "right:AI Assistant".into(),
        ToolRef::Learn => "right:Learn".into(),
        ToolRef::Left(l) => format!("left:{}", left_tool_name(l)),
        ToolRef::Bottom(b) => format!("bottom:{}", b.stable_name()),
    }
}

pub(super) fn tool_ref_by_id(id: &str) -> Option<ToolRef> {
    match id {
        "right:AI Assistant" => return Some(ToolRef::Ai),
        "right:Learn" => return Some(ToolRef::Learn),
        _ => {}
    }
    if let Some(name) = id.strip_prefix("left:") {
        return left_tool_by_name(name).map(ToolRef::Left);
    }
    id.strip_prefix("bottom:")
        .and_then(bottom_tool_by_title)
        .map(ToolRef::Bottom)
}

/// Zoom for Presentation mode: editor text of at least [`PRESENTATION_FONT_SIZE`]
/// and at least 4 points larger than now.
pub(super) fn presentation_zoom(base_font: f32, zoom: f32) -> f32 {
    (PRESENTATION_FONT_SIZE - base_font).max(zoom + 4.0)
}

/// Resident memory of this process in bytes.
pub(super) fn resident_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        parse_vm_rss(&std::fs::read_to_string("/proc/self/status").ok()?)
    }
    #[cfg(windows)]
    {
        windows_working_set()
    }
    #[cfg(target_os = "macos")]
    {
        macos_resident_size()
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        None
    }
}

/// `VmRSS` of a `/proc/<pid>/status` text, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_vm_rss(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line
        .trim_start_matches("VmRSS:")
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(windows)]
fn windows_working_set() -> Option<u64> {
    /// `PROCESS_MEMORY_COUNTERS` (psapi.h).
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut c = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        ..Default::default()
    };
    // SAFETY: the pseudo handle of the current process and a correctly sized struct.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    (ok != 0).then_some(c.working_set_size as u64)
}

#[cfg(target_os = "macos")]
fn macos_resident_size() -> Option<u64> {
    /// `struct proc_taskinfo` (sys/proc_info.h).
    #[repr(C)]
    #[derive(Default)]
    struct TaskInfo {
        virtual_size: u64,
        resident_size: u64,
        total_user: u64,
        total_system: u64,
        threads_user: u64,
        threads_system: u64,
        policy: i32,
        faults: i32,
        pageins: i32,
        cow_faults: i32,
        messages_sent: i32,
        messages_received: i32,
        syscalls_mach: i32,
        syscalls_unix: i32,
        csw: i32,
        threadnum: i32,
        numrunning: i32,
        priority: i32,
    }
    const PROC_PIDTASKINFO: i32 = 4;
    extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut std::ffi::c_void,
            size: i32,
        ) -> i32;
    }
    let mut info = TaskInfo::default();
    let size = std::mem::size_of::<TaskInfo>() as i32;
    // SAFETY: the buffer is a correctly sized `proc_taskinfo`.
    let n = unsafe {
        proc_pidinfo(
            std::process::id() as i32,
            PROC_PIDTASKINFO,
            0,
            (&mut info as *mut TaskInfo).cast(),
            size,
        )
    };
    (n == size).then_some(info.resident_size)
}

/// Keyboard input a modal question keeps from the window behind it: every
/// key but Tab and Space (they move between and press its buttons), typed
/// text, IME input, paste and clipboard shortcuts.
fn takes_keyboard(e: &egui::Event) -> bool {
    match e {
        egui::Event::Key { key, .. } => !matches!(key, egui::Key::Tab | egui::Key::Space),
        egui::Event::Text(_)
        | egui::Event::Paste(_)
        | egui::Event::Copy
        | egui::Event::Cut
        | egui::Event::Ime(_) => true,
        _ => false,
    }
}

/// "312 MB" / "1.4 GB".
pub(super) fn format_memory(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

impl IdeApp {
    fn shown_tool_windows(&self) -> Shown {
        Shown {
            extra: {
                let mut extra = self.layout.tools.saved.open.clone();
                if self.ai_panel.visible {
                    extra.push(tool_ref_id(ToolRef::Ai));
                }
                if self.learn.visible {
                    extra.push(tool_ref_id(ToolRef::Learn));
                }
                extra.sort();
                extra.dedup();
                extra
            },
            left: self.layout.left_visible,
            bottom: self.bottom,
            ai: self.ai_panel.visible,
            learn: self.learn.visible,
        }
    }

    fn show_tool_windows(&mut self, s: &Shown) {
        self.layout.tools.saved.open = s.extra.clone();
        self.layout.left_visible = s.left;
        self.bottom = s.bottom;
        self.ai_panel.visible = s.ai;
        self.learn.visible = s.learn;
    }

    fn hide_tool_windows(&mut self) {
        self.show_tool_windows(&Shown {
            extra: Vec::new(),
            left: false,
            bottom: None,
            ai: false,
            learn: false,
        });
        self.focus_editor = true;
    }

    /// The layout to save: what Hide All Tool Windows or a view mode hid counts as shown.
    pub(super) fn tool_window_layout(&self) -> ToolWindowLayout {
        let zen_snapshot = self.layout.zen.as_ref().map(|state| &state.snapshot);
        let shown = zen_snapshot
            .map(|snapshot| {
                snapshot
                    .mode_stash
                    .as_ref()
                    .map(|stash| stash.shown.clone())
                    .or_else(|| snapshot.hidden_all.clone())
                    .unwrap_or_else(|| snapshot.shown.clone())
            })
            .or_else(|| {
                self.layout
                    .mode_stash
                    .as_ref()
                    .map(|m| m.shown.clone())
                    .or_else(|| self.layout.hidden_all.clone())
            })
            .unwrap_or_else(|| self.shown_tool_windows());
        let left_tool = zen_snapshot.map_or(self.left_tool, |snapshot| snapshot.left_tool);
        let last_tool = zen_snapshot.map_or(self.layout.last_tool, |snapshot| snapshot.last_tool);
        let bottom_tab = shown.bottom.or(match last_tool {
            Some(ToolRef::Bottom(t)) => Some(t),
            _ => None,
        });
        ToolWindowLayout {
            framework: {
                let mut framework = zen_snapshot.map_or_else(
                    || self.layout.tools.saved.clone(),
                    |snapshot| snapshot.framework.clone(),
                );
                framework.open = shown.extra.clone();
                framework
            },
            project_visible: shown.left,
            project_width: zen_snapshot
                .map_or(self.layout.left_width, |snapshot| snapshot.left_width),
            structure_visible: left_tool == LeftTool::Structure,
            bottom_visible: shown.bottom.is_some(),
            bottom_height: zen_snapshot
                .map_or(self.layout.bottom_height, |snapshot| snapshot.bottom_height),
            bottom_tab: bottom_tab
                .unwrap_or(BottomTool::Terminal)
                .stable_name()
                .to_string(),
            left_tool: left_tool_name(left_tool).to_string(),
            bottom_floating: zen_snapshot
                .map_or(self.floating_bottom, |snapshot| snapshot.floating_bottom),
            bottom_maximized: zen_snapshot
                .map_or(self.bottom_maximized, |snapshot| snapshot.bottom_maximized),
            last_tool: last_tool.map(tool_ref_id).unwrap_or_default(),
        }
    }

    /// Navigation uses the same tool-window layout authority without exiting
    /// the user's current presentation/distraction-free mode.
    pub(super) fn restore_navigation_tool_window_layout(&mut self, layout: &ToolWindowLayout) {
        let mode = self.layout.view_mode;
        let stash = self.layout.mode_stash.clone();
        let zen = self.layout.zen.take();
        let native_restore = self.layout.zen_native_restore.take();
        self.restore_tool_window_layout(layout);
        self.layout.zen = zen;
        self.layout.zen_native_restore = native_restore;
        self.layout.view_mode = mode;
        self.layout.mode_stash = stash;
    }

    /// Applies a project's saved layout (when the project opens).
    pub(super) fn restore_tool_window_layout(&mut self, l: &ToolWindowLayout) {
        // A new project/named layout must not inherit the old project's Zen stash.
        if let Some(zen) = self.layout.zen.take() {
            self.layout.zen_native_restore = Some(zen.fullscreen);
        }
        let debug_tabs = std::mem::take(&mut self.layout.tools.debug_tabs);
        self.layout.tools = super::tool_windows::State::restore(l.framework.clone());
        self.layout.tools.debug_tabs = debug_tabs;
        if l.bottom_maximized || l.framework.maximized.is_some() {
            self.focus_editor = false;
        }
        self.ai_panel.visible = self
            .layout
            .tools
            .saved
            .open
            .contains(&tool_ref_id(ToolRef::Ai));
        self.learn.visible = self
            .layout
            .tools
            .saved
            .open
            .contains(&tool_ref_id(ToolRef::Learn));
        self.layout.view_mode = ViewMode::Normal;
        self.layout.mode_stash = None;
        self.layout.hidden_all = None;
        self.layout.left_visible = l.project_visible;
        self.left_tool = left_tool_by_name(&l.left_tool).unwrap_or(if l.structure_visible {
            LeftTool::Structure
        } else {
            LeftTool::Project
        });
        self.bottom = if l.bottom_visible {
            bottom_tool_by_title(&l.bottom_tab)
        } else {
            None
        };
        self.documentation.docked =
            bottom_tool_by_title(&l.bottom_tab) == Some(BottomTool::Documentation);
        self.floating_bottom = l.bottom_floating && self.bottom.is_some();
        if self.floating_bottom {
            if let Some(tool) = self.bottom {
                let id = tool_ref_id(ToolRef::Bottom(tool));
                if self.layout.tools.saved.placement(&id).mode.docked() {
                    self.layout.tools.saved.placement_mut(&id).mode =
                        rustpy_settings::tool_windows::Mode::Float;
                }
                if l.bottom_maximized {
                    self.layout.tools.saved.window_maximized.insert(id);
                }
            }
        }
        self.bottom_maximized = l.bottom_maximized;
        self.layout.left_width = if l.project_width.is_finite() {
            l.project_width.clamp(140.0, 1200.0)
        } else {
            260.0
        };
        self.layout.bottom_height = if l.bottom_height.is_finite() {
            l.bottom_height.clamp(80.0, 1600.0)
        } else {
            300.0
        };
        self.layout.last_tool = tool_ref_by_id(&l.last_tool);
        self.layout.seen = None;
        self.layout.resize_panels = true;
        // An untrusted project's Terminal (restored from its workspace.json, which
        // a repository can ship) starts no shell by itself: a shell in the project
        // directory can run project code through rc files and directory hooks.
        // Trusting the project or the Terminal action starts it as usual.
        if self.safe_mode {
            self.terminal_panel.hold_shell(SAFE_MODE_TERMINAL_NOTE);
        } else {
            self.terminal_panel
                .set_auto_shell(!self.project_safe_mode());
        }
    }

    /// Alt+1 / Alt+2 / Alt+7: shows `tool` on the left, or hides the left tool
    /// window when `tool` is already shown there.
    pub(super) fn toggle_left_tool(&mut self, tool: LeftTool) {
        if self.layout.left_visible && self.left_tool == tool {
            self.layout.left_visible = false;
            self.focus_editor = true;
        } else {
            self.left_tool = tool;
            self.layout.left_visible = true;
        }
    }

    /// Ctrl+Shift+F12: hides every tool window; the next press restores them.
    pub(super) fn hide_all_tool_windows(&mut self) {
        let shown = self.shown_tool_windows();
        if shown.any() {
            self.layout.hidden_all = Some(shown);
            self.hide_tool_windows();
        } else if let Some(s) = self.layout.hidden_all.take() {
            self.show_tool_windows(&s);
        }
    }

    /// F12: opens and focuses the tool window used last (Project when none was).
    pub(super) fn jump_to_last_tool_window(&mut self) {
        let last = self
            .layout
            .last_tool
            .unwrap_or(ToolRef::Left(LeftTool::Project));
        let id = tool_ref_id(last);
        let slot = self.layout.tools.saved.placement(&id).slot;
        self.layout.tools.saved.selected.insert(slot, id.clone());
        self.layout.tools.focused = Some(id);
        match self
            .layout
            .last_tool
            .unwrap_or(ToolRef::Left(LeftTool::Project))
        {
            ToolRef::Ai => self.ai_panel.visible = true,
            ToolRef::Learn => self.learn.visible = true,
            ToolRef::Left(t) => {
                self.left_tool = t;
                self.layout.left_visible = true;
            }
            ToolRef::Bottom(t) => {
                self.bottom = Some(t);
                match t {
                    BottomTool::Terminal => {
                        // Asked for explicitly: a shell may start even in safe mode.
                        self.terminal_panel
                            .set_auto_shell(!self.project_safe_mode());
                        self.terminal_panel.request_focus();
                    }
                    BottomTool::PythonConsole => self.py_console.request_focus(),
                    _ => {}
                }
            }
        }
        self.focus_editor = false;
    }

    /// Alt+0: the Commit view (Git › Local Changes); hides it when it is shown.
    pub(super) fn toggle_commit_view(&mut self) {
        if self.bottom == Some(BottomTool::Git)
            && self.git_panel.tab == crate::git_panel::GitTab::LocalChanges
        {
            self.bottom = None;
            self.focus_editor = true;
            return;
        }
        self.bottom = Some(BottomTool::Git);
        self.git_panel.tab = crate::git_panel::GitTab::LocalChanges;
        self.git_panel.focus_message = true;
        if let Some(p) = &self.project {
            if let Some(g) = &p.git {
                self.git_panel.refresh(g, 0.0);
            }
        }
    }

    pub(super) fn capture_zen_layout(&self) -> ViewSnapshot {
        ViewSnapshot {
            mode: self.layout.view_mode,
            mode_stash: self.layout.mode_stash.clone(),
            hidden_all: self.layout.hidden_all.clone(),
            shown: self.shown_tool_windows(),
            left_tool: self.left_tool,
            last_tool: self.layout.last_tool,
            framework: self.layout.tools.saved.clone(),
            left_width: self.layout.left_width,
            bottom_height: self.layout.bottom_height,
            floating_bottom: self.floating_bottom,
            bottom_maximized: self.bottom_maximized,
            zoom: self.zoom,
        }
    }
    pub(super) fn hide_for_zen(&mut self) {
        self.hide_tool_windows();
        self.layout.view_mode = ViewMode::Zen;
    }
    pub(super) fn restore_zen_layout(&mut self, snapshot: ViewSnapshot) {
        self.layout.view_mode = snapshot.mode;
        self.layout.mode_stash = snapshot.mode_stash;
        self.layout.hidden_all = snapshot.hidden_all;
        self.layout.tools.saved = snapshot.framework;
        self.show_tool_windows(&snapshot.shown);
        self.left_tool = snapshot.left_tool;
        self.layout.last_tool = snapshot.last_tool;
        self.layout.left_width = snapshot.left_width;
        self.layout.bottom_height = snapshot.bottom_height;
        self.floating_bottom = snapshot.floating_bottom;
        self.bottom_maximized = snapshot.bottom_maximized;
        self.layout.resize_panels = true;
        self.zoom = snapshot.zoom;
        self.focus_editor = true;
    }

    /// View › Appearance › Enter/Exit Distraction Free or Presentation Mode.
    pub(super) fn toggle_view_mode(&mut self, mode: ViewMode, ctx: &egui::Context) {
        if self.layout.view_mode == ViewMode::Zen {
            self.exit_zen_mode(ctx);
            if self.layout.view_mode == mode {
                return;
            }
        }
        if self.layout.view_mode == mode {
            self.exit_view_mode(ctx);
            return;
        }
        if self.layout.view_mode != ViewMode::Normal {
            self.exit_view_mode(ctx);
        }
        let fullscreen = self
            .layout
            .zen_native_restore
            .take()
            .unwrap_or_else(|| ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)));
        self.layout.mode_stash = Some(ModeStash {
            shown: self.shown_tool_windows(),
            fullscreen,
            zoom: self.zoom,
        });
        self.hide_tool_windows();
        if mode == ViewMode::Presentation {
            self.layout.zen_native_restore = Some(true);
            if !fullscreen {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
            let target = self.settings.menus.appearance.presentation_font_size;
            self.zoom = if target.is_finite() {
                (target.clamp(16.0, 60.0) - self.settings.editor.font_size).max(self.zoom + 4.0)
            } else {
                presentation_zoom(self.settings.editor.font_size, self.zoom)
            };
        }
        self.layout.view_mode = mode;
        let name = if mode == ViewMode::Presentation {
            "Presentation mode"
        } else {
            "Distraction-free mode"
        };
        self.set_status(format!(
            "{name}: press Esc or use View › Appearance to exit"
        ));
    }

    /// Leaves Distraction-Free or Presentation mode, restoring the previous layout.
    pub(super) fn exit_view_mode(&mut self, ctx: &egui::Context) {
        if self.layout.view_mode == ViewMode::Zen {
            self.exit_zen_mode(ctx);
            return;
        }
        let mode = std::mem::take(&mut self.layout.view_mode);
        let Some(st) = self.layout.mode_stash.take() else {
            return;
        };
        if mode == ViewMode::Presentation {
            self.layout.zen_native_restore = Some(st.fullscreen);
            if !st.fullscreen {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
            self.zoom = st.zoom;
        }
        self.show_tool_windows(&st.shown);
    }

    /// Menu text for the view-mode actions ("Exit …" while the mode is on).
    pub(super) fn view_mode_menu_text(&self, action: Action) -> &'static str {
        match (action, self.layout.view_mode) {
            (Action::ToggleDistractionFree, ViewMode::DistractionFree) => {
                "Exit Distraction Free Mode"
            }
            (Action::TogglePresentationMode, ViewMode::Presentation) => "Exit Presentation Mode",
            (Action::ToggleZenMode, ViewMode::Zen) => "Exit Zen Mode",
            _ => action.text(),
        }
    }

    /// The menu bar is hidden in Presentation mode; toolbars, status bar and tool-window bars in both modes.
    pub(super) fn shows_menu_bar(&self) -> bool {
        self.settings.chrome.show_main_menu
            && !matches!(
                self.layout.view_mode,
                ViewMode::Presentation | ViewMode::Zen
            )
    }

    pub(super) fn shows_chrome(&self) -> bool {
        self.layout.view_mode == ViewMode::Normal
    }

    /// Nothing else would take an Escape press (popups, dialogs, selections, a focused terminal).
    fn escape_is_free(&self, ctx: &egui::Context) -> bool {
        let popups_closed = matches!(self.dialog, Dialog::None)
            && !self.completion.visible
            && !self.find.visible
            && self.choice.is_none()
            && self.quick_doc.is_none()
            && self.param_info.is_none()
            && self.intentions.is_none()
            && !self.everywhere.visible
            && !self.find_in_files.visible
            && self.template_session.is_none()
            && self.inplace_rename.is_none()
            && self.layout.exit_prompt.is_none();
        let terminal_focused = (self.bottom == Some(BottomTool::Terminal)
            && self.terminal_panel.focused)
            || (self.bottom == Some(BottomTool::PythonConsole) && self.py_console.focused);
        let editor_plain = self
            .active
            .and_then(|i| self.tabs.get(i))
            .is_none_or(|t| t.state.carets.len() == 1 && !t.state.has_selection());
        let text_field = ctx.memory(|m| m.focused()).is_some_and(|id| {
            self.active
                .and_then(|i| self.tabs.get(i))
                .is_none_or(|t| t.view.id != id)
        });
        popups_closed && !terminal_focused && editor_plain && !text_field
    }

    /// Apply a pending layout restore before an explicit keyboard resize. Both
    /// paths share the same one-shot invalidation of egui's remembered sizes.
    pub(super) fn prepare_tool_panel_sizes(&mut self, ctx: &egui::Context) {
        if self.layout.resize_panels {
            self.layout.resize_panels = false;
            ctx.data_mut(|d| {
                d.remove::<egui::containers::panel::PanelState>(egui::Id::new("project_panel"));
                d.remove::<egui::containers::panel::PanelState>(egui::Id::new("bottom_tools"));
                d.remove::<egui::containers::panel::PanelState>(egui::Id::new("right_tools"));
                d.remove::<egui::containers::panel::PanelState>(egui::Id::new("top_tools"));
            });
        }
    }

    /// Start of a frame (before anything reads the keyboard): the exit question
    /// takes its keys, Escape leaves a view mode, and restored panel sizes take effect.
    pub(super) fn layout_frame_start(&mut self, ctx: &egui::Context) {
        self.tool_window_frame_start(ctx);
        self.exit_prompt_keys(ctx);
        self.prepare_tool_panel_sizes(ctx);
        if self.layout.view_mode != ViewMode::Normal
            && ctx.input(|i| i.key_pressed(egui::Key::Escape) && i.modifiers.is_none())
            && self.escape_is_free(ctx)
        {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            self.exit_view_mode(ctx);
        }
    }

    /// End of a frame: notes which tool window opened or was clicked (Jump to Last
    /// Tool Window) and the panel sizes to save.
    pub(super) fn layout_frame_end(&mut self, ctx: &egui::Context) {
        let now = (self.layout.left_visible, self.left_tool, self.bottom);
        if let Some((was_left, was_tool, was_bottom)) = self.layout.seen {
            if let Some(b) = self.bottom.filter(|b| Some(*b) != was_bottom) {
                self.layout.last_tool = Some(ToolRef::Bottom(b));
            } else if self.layout.left_visible && (!was_left || was_tool != self.left_tool) {
                self.layout.last_tool = Some(ToolRef::Left(self.left_tool));
            }
        }
        self.layout.seen = Some(now);
        if self.layout.view_mode != ViewMode::Normal || self.project.is_none() {
            return;
        }
        let left = egui::containers::panel::PanelState::load(ctx, egui::Id::new("project_panel"))
            .map(|s| s.outer_rect);
        let bottom = egui::containers::panel::PanelState::load(ctx, egui::Id::new("bottom_tools"))
            .map(|s| s.outer_rect);
        if let Some(r) = left.filter(|_| self.layout.left_visible) {
            self.layout.left_width = r.width().round();
        }
        let docked_bottom =
            self.bottom.is_some() && !self.floating_bottom && !self.bottom_maximized;
        if let Some(r) = bottom.filter(|_| docked_bottom) {
            self.layout.bottom_height = r.height().round();
        }
        if ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                if let (Some(b), Some(r)) = (
                    self.bottom,
                    bottom.filter(|_| self.bottom.is_some() && !self.floating_bottom),
                ) {
                    if r.contains(pos) {
                        self.layout.last_tool = Some(ToolRef::Bottom(b));
                    }
                }
                if let Some(r) = left.filter(|_| self.layout.left_visible) {
                    if r.contains(pos) {
                        self.layout.last_tool = Some(ToolRef::Left(self.left_tool));
                    }
                }
            }
        }
        self.tool_panel_resize_accessibility(ctx);
    }

    /// The central editor area: centered at the right margin in the view modes.
    pub(super) fn editor_area_for_view_mode(&mut self, ui: &mut egui::Ui) {
        if self.layout.view_mode == ViewMode::Normal {
            self.editor_area(ui);
            return;
        }
        let font = egui::FontId::new(self.font_size(), crate::theme::mono_family(false, false));
        let char_w = ui.ctx().fonts_mut(|f| f.glyph_width(&font, 'm'));
        let columns = self.settings.editor.right_margin.clamp(40, 400) as f32;
        let avail = ui.available_rect_before_wrap();
        let width = (columns * char_w + 8.0 * char_w + 32.0).min(avail.width());
        let rect = egui::Rect::from_center_size(avail.center(), egui::vec2(width, avail.height()));
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            self.editor_area(ui)
        });
    }

    // ----- exit confirmation -------------------------------------------------

    /// Processes that exiting would terminate.
    pub(super) fn running_processes(&mut self) -> (Vec<String>, usize) {
        let mut out: Vec<String> = self
            .run_panel
            .sessions
            .iter()
            .filter(|s| s.is_running())
            .map(|s| format!("Run '{}'", s.name))
            .collect();
        if self.debug.as_ref().is_some_and(|d| d.is_active()) {
            out.push("Debug session".into());
        }
        out.extend(self.retained_measurement_labels());
        out.extend(self.file_watcher_pending_labels());
        out.extend(self.other_vcs_process_labels());
        out.extend(self.web_preview_labels());
        if let Some((_, label, _)) = self.update_transfer_state() {
            out.push(label);
        }
        out.extend(
            self.deploy
                .pending_transfers()
                .into_iter()
                .map(|(_, label, _)| label),
        );
        out.extend(self.notebook_process_labels());
        out.extend(self.database_process_labels());
        let ai = self.ai_worker_ids();
        if !ai.is_empty() {
            out.push(format!("{} AI request readers", ai.len()));
        }
        out.extend(self.web_languages.stamps());
        if self.collaboration_running() {
            out.push("Collaboration session".into());
        }
        out.extend(
            self.settings_sync_pending()
                .into_iter()
                .map(|(_, name, _)| name),
        );
        if self.diagram_panel.capture_owner().is_some() {
            out.push("Diagram model capture".into());
        }
        if let Some((_, title)) = self.ide_capture_owner() {
            out.push(title);
        }
        if self.diagnostic_export.stamp().is_some() {
            out.push("Collecting diagnostic data (must finish before exit)".into());
        }
        if self.network_security_pending() {
            out.push("Password-store operations (must finish before exit)".into());
        }
        if self.data_files.busy() {
            out.push("Data-file operation".into());
        }
        if let Some((_, host)) = self.shared_index_download_owner() {
            out.push(format!("Shared index from {host}"));
        }
        out.extend(
            self.http_panel
                .running()
                .into_iter()
                .map(|request| format!("HTTP '{}'", request.configuration)),
        );
        if self.test_panel.is_running() {
            out.push("Tests".into());
        }
        if self.coverage_panel.is_running() {
            out.push("Coverage run".into());
        }
        if self.profiler_panel.is_running() {
            out.push("Profiler run".into());
        }
        if !self.git_panel.branch_workspace.pending.is_empty() {
            out.push("Branch workspace operation".into());
        }
        if self.concurrency_panel.is_running() {
            out.push("Concurrency capture".into());
        }
        if self.github.busy() {
            out.push("Code hosting request".into());
        }
        if self.py_console.is_running() {
            out.push("Python Console".into());
        }
        if self.directory_comparisons_pending() {
            out.push("Directory comparison".into());
        }
        out.extend(
            rustpy_terminal::poll_retiring_terminals()
                .into_iter()
                .map(|stamp| format!("Terminal cleanup: {stamp}")),
        );
        let mut count = out.len();
        let terminals = self
            .terminal_panel
            .tabs
            .iter_mut()
            .map(|t| t.terminal.is_alive())
            .filter(|alive| *alive)
            .count();
        if terminals > 0 {
            out.push(if terminals == 1 {
                "Terminal session".to_string()
            } else {
                format!("{terminals} terminal sessions")
            });
            count += terminals;
        }
        (out, count)
    }

    /// The question to ask before exiting, if any (Confirm before exiting, or processes running).
    fn exit_prompt_needed(&mut self) -> Option<ExitPrompt> {
        let (processes, process_count) = self.running_processes();
        (self.settings.confirm_exit || !processes.is_empty()).then_some(ExitPrompt {
            processes,
            process_count,
            dont_ask: false,
            cancel_id: None,
        })
    }

    /// An exit is waiting for an answer: the exit question, or the unsaved-changes
    /// question of an editor while exiting.
    pub(super) fn exit_questions_open(&self) -> bool {
        self.layout.exit_prompt.is_some()
            || matches!(
                self.dialog,
                Dialog::ConfirmClose {
                    then_exit: true,
                    ..
                }
            )
    }

    /// The window is closing (title bar, Alt+F4): the same questions as File ›
    /// Exit. True when the close was cancelled to ask them.
    pub(super) fn intercept_close(&mut self, ctx: &egui::Context) -> bool {
        if self.exit_requested {
            return false;
        }
        if !self.exit_questions_open() {
            let unsaved = self.tabs.iter().any(|t| t.has_unsaved_changes());
            if !unsaved && self.exit_prompt_needed().is_none() {
                if !self.workspace_transition(workspace_ui::WorkspaceTransition::Exit) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    return true;
                }
                return false;
            }
            self.request_exit();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        true
    }

    /// File › Exit: asks whether to exit (Confirm before exiting, or processes
    /// that would be terminated), then about each editor with unsaved changes.
    pub(super) fn request_exit(&mut self) {
        if self.exit_questions_open() {
            return;
        }
        match self.exit_prompt_needed() {
            Some(p) => self.layout.exit_prompt = Some(p),
            None => self.continue_exit(),
        }
    }

    /// The exit was confirmed: asks about the next editor with unsaved changes
    /// (Save / Don't Save come back here, Cancel ends the exit), and once none is
    /// left terminates the processes and exits. Nothing is stopped before the
    /// last question is answered, so cancelling any of them keeps every run,
    /// console and terminal session alive.
    pub(super) fn continue_exit(&mut self) {
        match self.tabs.iter().position(|t| t.has_unsaved_changes()) {
            Some(i) => {
                self.dialog = Dialog::ConfirmClose {
                    tab: i,
                    then_exit: true,
                }
            }
            None => {
                if !self.workspace_transition(workspace_ui::WorkspaceTransition::Exit) {
                    return;
                }
                self.terminate_processes();
                self.exit_requested = true;
            }
        }
    }

    /// The unsaved-changes question about `tab` was answered. During an exit
    /// (`then_exit`) Save and Don't Save go on to the next unsaved editor and
    /// Cancel ends the exit with every process still running.
    pub(super) fn answer_confirm_close(
        &mut self,
        tab: usize,
        then_exit: bool,
        answer: CloseAnswer,
    ) {
        match answer {
            CloseAnswer::Save => {
                if self.save_tab(tab) {
                    self.close_tab(tab);
                    if then_exit {
                        self.continue_exit();
                    }
                }
            }
            CloseAnswer::Discard => {
                if let Some(t) = self.tabs.get_mut(tab) {
                    t.state.doc.mark_saved();
                }
                self.close_tab(tab);
                if then_exit {
                    self.continue_exit();
                }
            }
            CloseAnswer::Cancel => {
                if then_exit {
                    self.cancel_pending_update_restart();
                    self.cancel_dialog_restart();
                }
            }
        }
    }

    /// Answers the exit question (`exit`: Exit / Terminate and Exit, else Cancel).
    fn answer_exit_prompt(&mut self, prompt: ExitPrompt, exit: bool) {
        if !exit {
            self.cancel_pending_update_restart();
            self.cancel_dialog_restart();
            self.focus_editor = true;
            return;
        }
        if prompt.dont_ask {
            self.change_setting_now(|s| s.confirm_exit = false);
        }
        self.continue_exit();
    }

    /// Start of a frame with the exit question open: it owns the keyboard. Enter
    /// answers Exit (Cancel while that button has the focus) and Escape Cancel,
    /// and every other key, text, paste or clipboard event is dropped before
    /// the editor, a terminal or a shortcut can take it (Tab and Space stay for
    /// moving between and pressing the question's own buttons, which hold the
    /// keyboard focus).
    fn exit_prompt_keys(&mut self, ctx: &egui::Context) {
        let Some(cancel_id) = self.layout.exit_prompt.as_ref().map(|p| p.cancel_id) else {
            return;
        };
        let (enter, escape) = ctx.input_mut(|i| {
            let enter = i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed: true,
                        ..
                    }
                )
            });
            let escape = i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::Escape,
                        pressed: true,
                        ..
                    }
                )
            });
            i.events.retain(|e| !takes_keyboard(e));
            (enter, escape)
        });
        if enter || escape {
            let on_cancel = cancel_id.is_some_and(|id| ctx.memory(|m| m.has_focus(id)));
            if let Some(prompt) = self.layout.exit_prompt.take() {
                self.answer_exit_prompt(prompt, enter && !escape && !on_cancel);
            }
        }
    }

    pub(super) fn terminate_processes(&mut self) {
        self.services_panel.docker.cancel();
        self.cancel_window_operations();
        self.cancel_plugin_runtime();
        self.stop_all_remote_development();
        self.stop_ai_workers();
        self.stop_web_languages();
        self.stop_collaboration();
        self.cancel_ai_inline();
        self.stop_all_settings_sync();
        self.stop_ide_capture(None);
        self.diagram_panel.cancel_capture();
        self.db_panel.stop_all_database();
        self.stop_update_transfer();
        self.deploy.cancel_all();
        self.data_files.cancel();
        for notebook in self.tabs.iter_mut().filter_map(|tab| tab.notebook.as_mut()) {
            notebook.stop_quit_process();
        }
        self.cancel_web_preview_project();
        self.stop_shared_index_download(None);
        self.stop_file_watchers();
        self.stop_other_vcs_operations();
        self.http_panel.stop_all();
        self.git_panel.stop_log_job();
        for s in self
            .run_panel
            .sessions
            .iter_mut()
            .filter(|s| s.is_running())
        {
            s.stop();
        }
        if let Some(d) = &mut self.debug {
            if d.is_active() {
                d.stop();
            }
        }
        if self.test_panel.is_running() {
            self.test_panel.stop();
        }
        if let Some(run) = self.coverage_panel.run.as_mut().filter(|r| r.is_running()) {
            run.stop();
        }
        if let Some(run) = self.profiler_panel.run.as_mut().filter(|r| r.is_running()) {
            run.stop();
        }
        self.concurrency_panel.stop();
        self.stop_retained_measurements();
        self.git_panel.branch_workspace.cancel_jobs();
        self.github.cancel();
        if self.py_console.is_running() {
            self.py_console.stop();
        }
        for t in &mut self.terminal_panel.tabs {
            t.terminal.kill();
        }
    }

    /// "Exit RustPy?" with the processes that would be terminated: a modal
    /// question (the window behind takes no clicks) whose buttons keep the
    /// keyboard focus, so a focused editor, terminal or console gets no keys
    /// while it is open (Enter and Escape: [`Self::exit_prompt_keys`]).
    pub(super) fn draw_exit_prompt(&mut self, ctx: &egui::Context) {
        let Some(mut prompt) = self.layout.exit_prompt.take() else {
            return;
        };
        let theme = self.theme.clone();
        let mut decision: Option<bool> = None;
        egui::Modal::new(egui::Id::new("exit_prompt")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.label(egui::RichText::new("Exit RustPy").strong().size(15.0));
            ui.add_space(4.0);
            let mut focusable = Vec::new();
            if prompt.processes.is_empty() {
                ui.label("Are you sure you want to exit RustPy?");
                focusable.push(ui.checkbox(&mut prompt.dont_ask, "Do not ask again"));
            } else {
                let n = prompt.process_count;
                ui.label(if n == 1 {
                    "A process is still running:".to_string()
                } else {
                    format!("{n} processes are still running:")
                });
                for p in &prompt.processes {
                    ui.label(egui::RichText::new(format!("  • {p}")).color(theme.text));
                }
                ui.label("Terminate and exit?");
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if prompt.processes.is_empty() {
                    "Exit"
                } else {
                    "Terminate and Exit"
                };
                let exit = ui.button(label);
                let cancel = ui.button("Cancel");
                prompt.cancel_id = Some(cancel.id);
                if exit.clicked() {
                    decision = Some(true);
                } else if cancel.clicked() {
                    decision = Some(false);
                }
                // Keyboard focus stays in the question (Enter: the default button).
                if !exit.has_focus()
                    && !cancel.has_focus()
                    && !focusable.iter().any(|r| r.has_focus())
                {
                    exit.request_focus();
                }
            });
        });
        match decision {
            Some(exit) => self.answer_exit_prompt(prompt, exit),
            None => self.layout.exit_prompt = Some(prompt),
        }
    }

    // ----- status bar ----------------------------------------------------------

    /// Right end of the status bar: the notifications bell and the memory indicator.
    pub(super) fn status_notification_widget(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let unread = self.notifications.unread();
        let color = if self.notifications.unread_errors() > 0 {
            theme.error
        } else if unread > 0 {
            theme.accent
        } else {
            theme.text_dim
        };
        let text = if unread > 0 {
            format!("🔔 {unread}")
        } else {
            "🔔".to_string()
        };
        if self.settings.chrome.status_visible("notifications")
            && ui
                .add(egui::Label::new(egui::RichText::new(text).color(color)).sense(Sense::click()))
                .on_hover_text(if unread > 0 {
                    format!("{unread} unread notification(s): click to show them")
                } else {
                    "Notifications".to_string()
                })
                .clicked()
        {
            self.toggle_bottom(BottomTool::Notifications);
        }
    }
    pub(super) fn status_memory_widget(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        if !self.settings.appearance.show_memory_indicator
            || !self.settings.chrome.status_visible("memory")
        {
            self.layout.memory = None;
            return;
        }
        let now = ui.input(|i| i.time);
        if self.layout.memory.is_none_or(|(at, _)| now - at >= 2.0) {
            self.layout.memory = Some((now, resident_memory_bytes()));
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(2));
        if let Some((_, Some(bytes))) = self.layout.memory {
            ui.label(egui::RichText::new(format_memory(bytes)).color(theme.text_dim))
                .on_hover_text("Memory used by RustPy (resident set size)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_ids_round_trip() {
        for t in BOTTOM_TOOLS {
            assert_eq!(bottom_tool_by_title(t.title()), Some(*t));
            assert_eq!(
                tool_ref_by_id(&tool_ref_id(ToolRef::Bottom(*t))),
                Some(ToolRef::Bottom(*t))
            );
        }
        for (t, _) in LEFT_TOOLS {
            assert_eq!(
                tool_ref_by_id(&tool_ref_id(ToolRef::Left(t))),
                Some(ToolRef::Left(t))
            );
        }
        assert_eq!(bottom_tool_by_title("No Such Tool"), None);
        assert_eq!(tool_ref_by_id(""), None);
    }

    #[test]
    fn presentation_zoom_makes_text_large() {
        assert_eq!(presentation_zoom(13.0, 0.0), 11.0);
        assert_eq!(
            presentation_zoom(13.0, 10.0),
            14.0,
            "at least 4 points more than the current zoom"
        );
        assert_eq!(presentation_zoom(30.0, 0.0), 4.0);
    }

    /// A scratch directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let dir =
                std::env::temp_dir().join(format!("rustpy-layout-test-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dunce::canonicalize(&dir).unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An IDE window without a project (no project folder is read or written,
    /// the settings are not saved).
    fn test_app() -> IdeApp {
        IdeApp::new(Settings::default(), None)
    }

    /// Opens an editor for `path` with an unsaved change.
    fn edited_tab(app: &mut IdeApp, path: &Path) {
        let mut state = EditorState::from_text("x = 1\n", Language::Python);
        state.path = Some(path.to_path_buf());
        app.push_tab(state);
        let state = &mut app.tabs.last_mut().unwrap().state;
        let end = state.doc.len();
        state.apply(
            vec![rustpy_text::Edit::insert(end, "y = 2\n")],
            vec![crate::editor::Caret::at(end)],
            rustpy_text::EditGroup::Separate,
            Some("type"),
        );
        assert!(state.doc.is_modified());
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Runs the start of a frame with `events` as input; returns the events
    /// left for the rest of the frame (editors, terminals, shortcuts).
    fn frame_start(
        app: &mut IdeApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> Vec<egui::Event> {
        let mut left = Vec::new();
        ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| {
                app.layout_frame_start(ui.ctx());
                left = ui.input(|i| i.events.clone());
            },
        )
        .drop_without_applying_deltas();
        left
    }

    #[test]
    fn the_exit_question_owns_the_keyboard() {
        let dir = Scratch::new();
        let mut app = test_app();
        app.settings.confirm_exit = true;
        edited_tab(&mut app, &dir.path().join("main.py"));
        let ctx = egui::Context::default();
        app.request_exit();
        assert!(app.layout.exit_prompt.is_some(), "Confirm before exiting");
        assert!(
            !app.editor_keyboard(),
            "editors take no keys while the question is open"
        );
        // Typing while the question is open reaches nothing behind it (a focused
        // editor or terminal would get `rm -rf x` and the paste).
        let left = frame_start(
            &mut app,
            &ctx,
            vec![
                egui::Event::Text("rm -rf x".into()),
                key(egui::Key::Backspace),
                egui::Event::Paste("p".into()),
                key(egui::Key::Tab),
            ],
        );
        assert_eq!(
            left,
            vec![key(egui::Key::Tab)],
            "only Tab is left, for the question's own buttons"
        );
        assert!(app.layout.exit_prompt.is_some());
        // Enter answers the question and is taken out of the input: the editor
        // gets no newline, a terminal no command.
        let left = frame_start(
            &mut app,
            &ctx,
            vec![key(egui::Key::Enter), egui::Event::Text("\n".into())],
        );
        assert!(left.is_empty(), "{left:?}");
        assert!(app.layout.exit_prompt.is_none());
        assert!(
            matches!(
                app.dialog,
                Dialog::ConfirmClose {
                    tab: 0,
                    then_exit: true
                }
            ),
            "then the unsaved editor is asked about"
        );
        assert_eq!(
            app.tabs[0].state.doc.text(),
            "x = 1\ny = 2\n",
            "the editor text is unchanged"
        );
        assert!(!app.exit_requested);
        // Escape cancels.
        app.dialog = Dialog::None;
        app.request_exit();
        let left = frame_start(&mut app, &ctx, vec![key(egui::Key::Escape)]);
        assert!(
            left.is_empty()
                && app.layout.exit_prompt.is_none()
                && matches!(app.dialog, Dialog::None)
                && !app.exit_requested
        );
        // The question takes the keyboard focus (its Exit button); after Tab to
        // Cancel, Enter presses Cancel.
        app.request_exit();
        ctx.run_ui(Default::default(), |ui| app.draw_exit_prompt(ui.ctx()))
            .drop_without_applying_deltas();
        let cancel = app
            .layout
            .exit_prompt
            .as_ref()
            .and_then(|p| p.cancel_id)
            .expect("the question was drawn");
        assert!(
            ctx.memory(|m| m.focused()).is_some_and(|f| f != cancel),
            "the default button has the focus"
        );
        ctx.memory_mut(|m| m.request_focus(cancel));
        let left = frame_start(&mut app, &ctx, vec![key(egui::Key::Enter)]);
        assert!(
            left.is_empty()
                && app.layout.exit_prompt.is_none()
                && matches!(app.dialog, Dialog::None)
                && !app.exit_requested,
            "Enter on the focused Cancel button cancels"
        );
    }

    #[test]
    #[cfg(unix)] // runs /bin/sh in a terminal session
    fn exiting_asks_about_every_unsaved_editor_before_stopping_anything() {
        let dir = Scratch::new();
        let mut app = test_app();
        app.settings.confirm_exit = false;
        edited_tab(&mut app, &dir.path().join("main.py"));
        edited_tab(&mut app, &dir.path().join("app.py"));
        // A terminal session: exiting would terminate it.
        app.terminal_panel
            .open_command("sh", "/bin/sh", &[], dir.path(), &[])
            .unwrap();
        let alive = |app: &mut IdeApp| app.terminal_panel.tabs[0].terminal.is_alive();
        app.request_exit();
        assert_eq!(
            app.layout
                .exit_prompt
                .as_ref()
                .map(|p| (p.processes.clone(), p.process_count)),
            Some((vec!["Terminal session".to_string()], 1))
        );
        // Terminate and Exit, then Cancel on "Save changes to main.py?": nothing
        // was stopped and nothing exits.
        let prompt = app.layout.exit_prompt.take().unwrap();
        app.answer_exit_prompt(prompt, true);
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmClose {
                tab: 0,
                then_exit: true
            }
        ));
        assert!(
            alive(&mut app),
            "processes run until every question is answered"
        );
        app.dialog = Dialog::None;
        app.answer_confirm_close(0, true, CloseAnswer::Cancel);
        assert!(
            alive(&mut app) && !app.exit_requested && app.tabs.len() == 2,
            "a cancelled exit stops nothing"
        );
        // Again: Don't Save on main.py asks about app.py next (it is not discarded silently).
        app.request_exit();
        let prompt = app.layout.exit_prompt.take().unwrap();
        app.answer_exit_prompt(prompt, true);
        app.dialog = Dialog::None;
        app.answer_confirm_close(0, true, CloseAnswer::Discard);
        assert!(
            matches!(
                app.dialog,
                Dialog::ConfirmClose {
                    tab: 0,
                    then_exit: true
                }
            ),
            "the next unsaved editor is asked about"
        );
        assert_eq!(
            app.tabs[0].state.path.as_deref(),
            Some(dir.path().join("app.py").as_path())
        );
        assert!(
            app.layout.exit_prompt.is_none(),
            "the exit question is not asked twice"
        );
        assert!(alive(&mut app) && !app.exit_requested);
        // The last answer exits and only then terminates the session.
        app.dialog = Dialog::None;
        app.answer_confirm_close(0, true, CloseAnswer::Discard);
        assert!(app.exit_requested);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while alive(&mut app) {
            assert!(
                std::time::Instant::now() < deadline,
                "the terminal session was not stopped"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn closing_the_window_asks_the_same_questions() {
        let dir = Scratch::new();
        let mut app = test_app();
        app.settings.confirm_exit = false;
        let ctx = egui::Context::default();
        let close = |app: &mut IdeApp| {
            let mut cancelled = false;
            ctx.run_ui(Default::default(), |ui| {
                cancelled = app.intercept_close(ui.ctx())
            })
            .drop_without_applying_deltas();
            cancelled
        };
        assert!(!close(&mut app), "nothing to ask: the window closes");
        edited_tab(&mut app, &dir.path().join("main.py"));
        assert!(close(&mut app));
        assert!(matches!(
            app.dialog,
            Dialog::ConfirmClose {
                tab: 0,
                then_exit: true
            }
        ));
        assert!(close(&mut app), "a second click keeps the question");
        assert!(
            matches!(
                app.dialog,
                Dialog::ConfirmClose {
                    tab: 0,
                    then_exit: true
                }
            ) && app.layout.exit_prompt.is_none()
        );
        app.dialog = Dialog::None;
        app.answer_confirm_close(0, true, CloseAnswer::Discard);
        assert!(app.exit_requested);
        assert!(!close(&mut app), "our own close goes through");
    }

    #[test]
    fn a_restored_terminal_starts_no_shell_in_an_untrusted_project() {
        let dir = Scratch::new();
        let mut app = test_app();
        let layout = ToolWindowLayout {
            bottom_visible: true,
            bottom_tab: "Terminal".into(),
            ..Default::default()
        };
        app.safe_mode = true;
        app.restore_tool_window_layout(&layout);
        assert_eq!(
            app.bottom,
            Some(BottomTool::Terminal),
            "the layout is restored"
        );
        assert!(!app.terminal_panel.auto_shell());
        // Showing the Terminal tool window starts nothing.
        let ctx = egui::Context::default();
        let theme = app.theme.clone();
        let draw = |app: &mut IdeApp| {
            ctx.run_ui(Default::default(), |ui| {
                app.terminal_panel
                    .ui(ui, &theme, egui::FontId::monospace(12.0), dir.path(), &[])
            })
            .drop_without_applying_deltas();
        };
        draw(&mut app);
        assert_eq!(
            app.terminal_panel.tab_count(),
            0,
            "no shell runs in the untrusted project"
        );
        // The Terminal action (Alt+F12) asks for one explicitly.
        let ctx2 = egui::Context::default();
        app.handle_action(Action::ToggleTerminal, &ctx2);
        assert!(app.terminal_panel.auto_shell(), "asked for explicitly");
        // A trusted project's restored Terminal starts as before.
        app.safe_mode = false;
        app.terminal_panel.hold_shell("held");
        app.restore_tool_window_layout(&layout);
        assert!(app.terminal_panel.auto_shell());
    }

    #[test]
    fn memory_is_read_and_formatted() {
        assert_eq!(
            parse_vm_rss("Name:\trustpy\nVmRSS:\t  20480 kB\nVmSwap: 0 kB\n"),
            Some(20480 * 1024)
        );
        assert_eq!(parse_vm_rss("Name: x\n"), None);
        assert_eq!(format_memory(312 * 1024 * 1024), "312 MB");
        assert_eq!(format_memory(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
        assert!(
            resident_memory_bytes().is_some_and(|b| b > 1024 * 1024),
            "this test process uses more than 1 MB"
        );
    }
}
