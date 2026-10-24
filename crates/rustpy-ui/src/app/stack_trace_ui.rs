//! Explicit trace analysis and opt-in clipboard detection; never executes text.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::stack_trace::{self, Analysis, Resolution};
use std::hash::{Hash, Hasher};
#[derive(Default)]
pub(super) struct State {
    view: Option<View>,
    closed: Option<View>,
    last_clipboard: Option<u64>,
}
struct View {
    text: String,
    parsed: Analysis,
    paths: Vec<Resolution>,
    root: String,
    remote_prefix: String,
    files: Vec<PathBuf>,
    roots: Vec<PathBuf>,
    thread: Option<usize>,
    focus: bool,
    dirty: bool,
    message: Option<String>,
    origin: String,
}
impl State {
    pub(super) fn active(&self) -> bool {
        self.view.is_some()
    }
}
impl View {
    fn analyze(&mut self) {
        self.parsed = stack_trace::parse(&self.text);
        self.paths.clear();
        let mut roots = self.roots.clone();
        let local = PathBuf::from(self.root.trim());
        if local.is_absolute() {
            roots = vec![local.clone()];
        }
        let mapping = (!self.remote_prefix.is_empty() && local.is_absolute())
            .then_some((self.remote_prefix.as_str(), local.as_path()));
        let mut by_name = std::collections::HashMap::<String, Vec<PathBuf>>::new();
        for path in self
            .files
            .iter()
            .filter(|path| !local.is_absolute() || path.starts_with(&local))
        {
            if let Some(name) = path.file_name() {
                by_name
                    .entry(name.to_string_lossy().into_owned())
                    .or_default()
                    .push(path.clone());
            }
        }
        let mut cache = std::collections::HashMap::new();
        self.paths = self
            .parsed
            .frames
            .iter()
            .map(|frame| {
                let key = (
                    frame.language,
                    frame.source.as_ref().map(|source| source.path.clone()),
                    frame.symbol.clone(),
                );
                cache
                    .entry(key)
                    .or_insert_with(|| {
                        let name = frame.source.as_ref().map(|source| {
                            source
                                .path
                                .rsplit(['/', '\\'])
                                .next()
                                .unwrap_or(&source.path)
                        });
                        let files = name
                            .and_then(|name| by_name.get(name))
                            .map(Vec::as_slice)
                            .unwrap_or_default();
                        stack_trace::resolve(frame, &roots, files, mapping)
                    })
                    .clone()
            })
            .collect();
        self.thread = None;
        self.dirty = false;
    }
}
impl IdeApp {
    pub(super) fn open_stack_trace_analysis(
        &mut self,
        text: Option<String>,
        origin: Option<PathBuf>,
        console: bool,
    ) {
        let explicit = text.is_some();
        let text = text.or_else(|| {
            self.clipboard_text()
                .filter(|text| stack_trace::looks_like_dump(text))
        });
        let reuse = !explicit
            && self
                .stack_trace_state
                .closed
                .as_ref()
                .is_some_and(|view| text.as_deref() == Some(view.text.as_str()));
        if text.is_none() || reuse {
            if let Some(view) = self.stack_trace_state.closed.take() {
                self.stack_trace_state.view = Some(view);
                return;
            }
        }
        let text = text.unwrap_or_default();
        if !console && !text.is_empty() {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            stack_trace::bounded_text(&text).0.hash(&mut hasher);
            text.len().hash(&mut hasher);
            self.stack_trace_state.last_clipboard = Some(hasher.finish());
        }
        let (bounded, truncated) = stack_trace::bounded_text(&text);
        let root = origin.or_else(|| {
            if console {
                None
            } else {
                self.project.as_ref().map(|project| project.root.clone())
            }
        });
        let capture_project = self.project.as_ref().is_some_and(|project| {
            root.as_ref()
                .is_some_and(|root| root.starts_with(&project.root))
        });
        let roots = if capture_project {
            self.project
                .as_ref()
                .map(|project| project.tree.import_roots())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let files = if capture_project {
            self.project_files()
        } else {
            Vec::new()
        };
        let mut view = View {
            text: bounded.into(),
            parsed: Analysis::default(),
            paths: Vec::new(),
            root: root
                .map(|root| root.to_string_lossy().into_owned())
                .unwrap_or_default(),
            remote_prefix: String::new(),
            files,
            roots,
            thread: None,
            focus: true,
            dirty: false,
            message: truncated.then(|| "Input was limited to 2 MiB".into()),
            origin: if console {
                "Retained console output".into()
            } else {
                "Pasted text / clipboard".into()
            },
        };
        view.analyze();
        self.stack_trace_state.view = Some(view);
    }
    pub(super) fn stack_trace_keys(&mut self, ctx: &egui::Context) -> bool {
        if !self.stack_trace_state.active() {
            return false;
        }
        self.run_panel.launcher.reset_ctrl_gesture();
        self.search_everywhere_gesture.reset();
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.stack_trace_state.closed = self.stack_trace_state.view.take();
            self.focus_editor = true;
        }
        true
    }
    pub(super) fn detect_clipboard_stack_trace(&mut self, ctx: &egui::Context) {
        if !self.settings.detect_clipboard_stack_traces
            || self.stack_trace_state.active()
            || !self.editor_controls_available()
            || !matches!(self.dialog, Dialog::None)
        {
            return;
        }
        let gained = ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::WindowFocused(true)))
        });
        if !gained {
            return;
        }
        let Some(text) = self.clipboard_text() else {
            return;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stack_trace::bounded_text(&text).0.hash(&mut hasher);
        text.len().hash(&mut hasher);
        let fingerprint = hasher.finish();
        if self.stack_trace_state.last_clipboard == Some(fingerprint) {
            return;
        }
        self.stack_trace_state.last_clipboard = Some(fingerprint);
        if stack_trace::looks_like_dump(&text) {
            self.open_stack_trace_analysis(Some(text), None, false);
        }
    }
    pub(super) fn console_stack_trace_link(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.run_panel.active else {
            return;
        };
        if !ui.link("Open stack trace from console").clicked() {
            return;
        }
        let origin = self
            .run_panel
            .sessions
            .iter()
            .find(|session| session.id == id)
            .and_then(|session| session.config.as_ref())
            .and_then(|config| {
                config
                    .working_dir
                    .clone()
                    .filter(|path| path.is_absolute())
                    .or_else(|| match &config.kind {
                        RunConfigKind::Python { script } if script.is_absolute() => {
                            script.parent().map(Path::to_path_buf)
                        }
                        _ => None,
                    })
            });
        match self.run_panel.retained_text(id) {
            Some(text) => self.open_stack_trace_analysis(Some(text), origin, true),
            None => self.set_status("The console session is no longer available"),
        }
    }
    pub(super) fn stack_trace_analysis_ui(&mut self, ctx: &egui::Context) {
        let Some(mut view) = self.stack_trace_state.view.take() else {
            return;
        };
        let mut open = true;
        let mut analyze = false;
        let mut paste = false;
        let mut navigate = None;
        let mut preference_changed = false;
        egui::Window::new("Analyze Stack Trace or Thread Dump").id(egui::Id::new("analyze-stack-trace")).default_size([900.0,680.0]).resizable(true).open(&mut open).show_modal(ctx,|ui|{
            ui.horizontal(|ui|{ui.label(&view.origin);paste=ui.button("Paste from clipboard").clicked();analyze=ui.button("Analyze").clicked();});
            preference_changed=ui.checkbox(&mut self.settings.detect_clipboard_stack_traces,"Automatically detect clipboard traces when RustPy regains focus").changed();
            let response=egui::ScrollArea::vertical().id_salt("stack-trace-input").max_height(180.0).show(ui,|ui|ui.add(egui::TextEdit::multiline(&mut view.text).font(egui::TextStyle::Monospace).desired_rows(8).desired_width(f32::INFINITY).hint_text("Paste Python tracebacks, Java thread dumps, or native gdb/lldb stacks"))).inner;
            if view.focus {response.request_focus();view.focus=false;}
            if response.changed(){view.dirty=true;}
            ui.horizontal(|ui|{ui.label("Local source root:");if ui.text_edit_singleline(&mut view.root).changed(){view.dirty=true;}});
            ui.horizontal(|ui|{ui.label("Remote path prefix (optional):");if ui.text_edit_singleline(&mut view.remote_prefix).changed(){view.dirty=true;}});
            if let Some(message)=&view.message{ui.colored_label(self.theme.warning,message);}
            if view.dirty{ui.label("Text or source mapping changed. Analyze to refresh clickable frames.");}
            if view.parsed.truncated{ui.colored_label(self.theme.warning,"Analysis is limited to 2 MiB / 20,000 lines");}
            ui.separator();ui.label(format!("{} threads · {} frames",view.parsed.threads.len(),view.parsed.frames.len()));
            if view.parsed.frames.is_empty(){ui.label("No supported stack frames found. Paste a traceback or thread dump and choose Analyze.");}
            egui::ComboBox::from_id_salt("stack-trace-thread").selected_text(view.thread.and_then(|index|view.parsed.threads.get(index)).map(|thread|thread.title.as_str()).unwrap_or("All threads")).show_ui(ui,|ui|{
                ui.selectable_value(&mut view.thread,None,"All threads");
                for(index,thread)in view.parsed.threads.iter().enumerate(){ui.selectable_value(&mut view.thread,Some(index),format!("{}{}",thread.title,thread.state.as_ref().map(|state|format!(" · {state}")).unwrap_or_default()));}
            }).combo_named(crate::i18n::tr_static("Thread"));
            let frames:Vec<_>=view.parsed.frames.iter().enumerate().filter(|(_,frame)|view.thread.is_none_or(|thread|thread==frame.thread)).collect();
            egui::ScrollArea::both().id_salt("analyzed-stack-frames").max_height(340.0).show_rows(ui,ui.text_style_height(&egui::TextStyle::Monospace)+6.0,frames.len(),|ui,range|{
                for visible in range{let(index,frame)=frames[visible];let text=&view.parsed.lines[frame.row];
                    match &view.paths[index]{
                        Resolution::Exact(path)=>{if ui.add_enabled(!view.dirty,egui::Link::new(egui::RichText::new(text).monospace())).on_hover_text(format!("{}:{}",path.display(),frame.source.as_ref().unwrap().line)).clicked(){navigate=Some((path.clone(),frame.source.clone().unwrap()));}},
                        Resolution::Ambiguous(paths)=>{ui.label(egui::RichText::new(text).monospace()).on_hover_text(format!("Ambiguous source: {} matching files. Set a more specific source root or remote mapping.",paths.len()));},
                        Resolution::Unavailable=>{ui.label(egui::RichText::new(text).monospace()).on_hover_text("No exact local source file is available for this frame");},
                    }
                }
            });
        });
        if preference_changed {
            let _ = self.settings.save();
        }
        if paste {
            if let Some(text) = self.clipboard_text() {
                let (bounded, truncated) = stack_trace::bounded_text(&text);
                view.text = bounded.into();
                view.dirty = true;
                view.message = truncated.then(|| "Input was limited to 2 MiB".into());
                analyze = true;
            }
        }
        if view.text.len() > stack_trace::MAX_BYTES {
            view.text = stack_trace::bounded_text(&view.text).0.to_string();
            view.message = Some("Input was limited to 2 MiB".into());
            view.dirty = true;
        }
        if analyze {
            view.analyze();
        }
        if let Some((path, source)) = navigate {
            if path.is_file() {
                self.mark_nav_jump();
                self.open_file(&path);
                let available = self
                    .active
                    .and_then(|index| self.tabs.get(index))
                    .is_some_and(|tab| {
                        tab.state.path.as_ref().is_some_and(|current| {
                            current == &path
                                || current
                                    .canonicalize()
                                    .ok()
                                    .zip(path.canonicalize().ok())
                                    .is_some_and(|(left, right)| left == right)
                        }) && source.line <= tab.state.doc.line_count()
                            && source.column.is_none_or(|column| {
                                column <= u32::MAX as usize
                                    && column.saturating_sub(1)
                                        <= tab
                                            .state
                                            .doc
                                            .line_text(source.line - 1)
                                            .trim_end_matches(['\r', '\n'])
                                            .chars()
                                            .count()
                            })
                    });
                if available {
                    self.open_at(&path, source.line - 1, source.column.unwrap_or(1) - 1);
                    open = false;
                } else {
                    view.message = Some(
                        "That exact source line or column is unavailable in the current file"
                            .into(),
                    );
                }
            } else {
                view.message = Some(
                    "The source file was removed; analyze again after updating the source mapping"
                        .into(),
                );
            }
        }
        if open {
            self.stack_trace_state.view = Some(view);
        } else {
            self.stack_trace_state.closed = Some(view);
        }
    }
}
