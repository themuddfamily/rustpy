//! Line and exception breakpoints of the open project (roadmap §3.3): their
//! properties, gutter glyphs, workspace persistence and what the debugger gets.

use crate::a11y::AccessibleCombo;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rustpy_debug::{Breakpoint, DebugSession, ExceptionBreakpoints, ExceptionClassBreakpoint};
use rustpy_settings::{ExceptionBreakpointSettings, LineBreakpointState, Workspace};

use crate::editor::view::BpGlyph;

/// A line breakpoint's properties (the breakpoint popup and the Breakpoints dialog).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineBp {
    pub metadata: rustpy_settings::debugger_workspace::BreakpointMetadata,
    pub enabled: bool,
    /// Suspend the program when hit; otherwise the breakpoint only logs.
    pub suspend: bool,
    pub condition: String,
    /// "Evaluate and log": `{expr}` parts are evaluated.
    pub log_message: String,
    /// Log a "Breakpoint reached" message.
    pub log_hit: bool,
    /// Remove once hit.
    pub temporary: bool,
    /// Hit count condition such as `5`, `>= 3` or `% 2`.
    pub hit_condition: String,
}

impl Default for LineBp {
    fn default() -> Self {
        LineBp {
            metadata: Default::default(),
            enabled: true,
            suspend: true,
            condition: String::new(),
            log_message: String::new(),
            log_hit: false,
            temporary: false,
            hit_condition: String::new(),
        }
    }
}

impl LineBp {
    /// How the gutter shows it (`verified`: what the running debugger answered).
    pub fn glyph(&self, muted: bool, verified: Option<bool>) -> BpGlyph {
        if !self.enabled {
            BpGlyph::Disabled
        } else if muted {
            BpGlyph::Muted
        } else if verified == Some(false) {
            BpGlyph::Unverified
        } else if !self.suspend {
            BpGlyph::Log
        } else if self.temporary {
            BpGlyph::Temporary
        } else if !self.condition.trim().is_empty() || !self.hit_condition.trim().is_empty() {
            BpGlyph::Conditional
        } else {
            BpGlyph::Enabled
        }
    }

    /// One-line description for lists (`Condition: x > 1, log`).
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.metadata.description.trim().is_empty() {
            parts.push(self.metadata.description.trim().to_owned());
        }
        if let Some((path, line)) = &self.metadata.depends_on {
            parts.push(format!("after {}:{line}", path.display()));
        }
        if !self.condition.trim().is_empty() {
            parts.push(format!("if {}", self.condition.trim()));
        }
        if !self.hit_condition.trim().is_empty() {
            parts.push(format!("hit count {}", self.hit_condition.trim()));
        }
        if self.log_hit || !self.log_message.trim().is_empty() {
            parts.push("log".into());
        }
        if !self.suspend {
            parts.push("no suspend".into());
        }
        if self.temporary {
            parts.push("remove once hit".into());
        }
        parts.join(", ")
    }

    /// These typed fields (condition, hit count, log message) with the switches of `other`.
    fn with_switches_of(&self, other: &LineBp) -> LineBp {
        LineBp {
            condition: self.condition.clone(),
            hit_condition: self.hit_condition.clone(),
            log_message: self.log_message.clone(),
            ..other.clone()
        }
    }

    fn from_state(s: &LineBreakpointState) -> LineBp {
        LineBp {
            metadata: s.metadata.clone(),
            enabled: s.enabled,
            suspend: s.suspend,
            condition: s.condition.clone(),
            log_message: s.log_message.clone(),
            log_hit: s.log_hit,
            temporary: s.temporary,
            hit_condition: s.hit_condition.clone(),
        }
    }

    fn to_state(&self, path: &Path, line: usize) -> LineBreakpointState {
        LineBreakpointState {
            metadata: self.metadata.clone(),
            path: path.to_path_buf(),
            line,
            enabled: self.enabled,
            suspend: self.suspend,
            condition: self.condition.clone(),
            log_message: self.log_message.clone(),
            log_hit: self.log_hit,
            temporary: self.temporary,
            hit_condition: self.hit_condition.clone(),
        }
    }

    /// The debugger's view of this breakpoint at `line` (1-based) of `path`.
    pub fn to_debug(&self, path: &Path, line: usize) -> Breakpoint {
        let mut log = Vec::new();
        if self.log_hit {
            // `{`/`}` would be read as expressions in the log template.
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().replace(['{', '}'], ""))
                .unwrap_or_default();
            log.push(format!("Breakpoint reached: {file}:{line}"));
        }
        if self.metadata.log_stack {
            log.push("{''.join(__import__('traceback').format_stack())}".into());
        }
        if !self.log_message.trim().is_empty() {
            log.push(self.log_message.trim_end().to_string());
        }
        let non_blank = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        Breakpoint {
            line,
            condition: non_blank(&self.condition),
            log_message: (!log.is_empty()).then(|| log.join("\n")),
            hit_condition: non_blank(&self.hit_condition),
            enabled: self.enabled && self.metadata.depends_on.is_none(),
            suspend: self.suspend
                && self.metadata.suspend_policy
                    != rustpy_settings::debugger_workspace::SuspendPolicy::None,
            temporary: self.temporary,
            verified: true,
        }
    }
}

/// All breakpoints of the project.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BreakpointStore {
    /// Per file: 1-based line → breakpoint.
    files: HashMap<PathBuf, BTreeMap<usize, LineBp>>,
    pub exceptions: ExceptionBreakpointSettings,
    /// Mute Breakpoints.
    pub muted: bool,
}

impl BreakpointStore {
    /// Reads the workspace: the detailed list, else the plain lines older versions wrote.
    pub fn from_workspace(ws: &Workspace) -> BreakpointStore {
        let mut store = BreakpointStore {
            files: HashMap::new(),
            exceptions: ws.exception_breakpoints.clone(),
            muted: ws.breakpoints_muted,
        };
        if ws.line_breakpoints.is_empty() {
            for (path, lines) in &ws.breakpoints {
                for l in lines {
                    store.set(path, *l, LineBp::default());
                }
            }
        } else {
            for s in &ws.line_breakpoints {
                store.set(&s.path, s.line.max(1), LineBp::from_state(s));
            }
        }
        store
    }

    /// Writes into the workspace; `breakpoints` keeps the enabled lines for older versions.
    pub fn write_to(&self, ws: &mut Workspace) {
        let mut paths: Vec<&PathBuf> = self
            .files
            .iter()
            .filter(|(_, m)| !m.is_empty())
            .map(|(p, _)| p)
            .collect();
        paths.sort();
        ws.breakpoints = paths
            .iter()
            .map(|p| {
                (
                    (*p).clone(),
                    self.files[*p]
                        .iter()
                        .filter(|(_, b)| b.enabled)
                        .map(|(l, _)| *l)
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, l)| !l.is_empty())
            .collect();
        ws.line_breakpoints = paths
            .iter()
            .flat_map(|p| self.files[*p].iter().map(|(l, b)| b.to_state(p, *l)))
            .collect();
        ws.exception_breakpoints = self.exceptions.clone();
        ws.breakpoints_muted = self.muted;
    }

    pub fn get(&self, path: &Path, line: usize) -> Option<&LineBp> {
        self.files.get(path)?.get(&line)
    }

    pub fn set(&mut self, path: &Path, line: usize, bp: LineBp) {
        self.files
            .entry(path.to_path_buf())
            .or_default()
            .insert(line, bp);
    }

    /// Removes the breakpoint at `line`; returns whether there was one.
    pub fn remove(&mut self, path: &Path, line: usize) -> bool {
        let removed = self
            .files
            .get_mut(path)
            .is_some_and(|m| m.remove(&line).is_some());
        if self.files.get(path).is_some_and(|m| m.is_empty()) {
            self.files.remove(path);
        }
        removed
    }

    /// Moves the breakpoint at `from` to `to` (where the debugger put it); it
    /// merges into a breakpoint already at `to`. Returns whether it moved.
    pub fn move_line(&mut self, path: &Path, from: usize, to: usize) -> bool {
        let Some(map) = self.files.get_mut(path) else {
            return false;
        };
        let Some(bp) = map.remove(&from) else {
            return false;
        };
        map.entry(to).or_insert(bp);
        true
    }

    /// Takes over where the debugger put breakpoints that were set on lines
    /// without code, `(path, from, to)`, except in the files `skip` names (see
    /// [`EditedFiles`]). Returns the moves taken over, each with whether the
    /// breakpoint merged into one already at `to`.
    pub fn adopt_moves(
        &mut self,
        moved: &[(PathBuf, usize, usize)],
        skip: impl Fn(&Path) -> bool,
    ) -> Vec<(PathBuf, usize, usize, bool)> {
        let mut out = Vec::new();
        for (path, from, to) in moved {
            if skip(path) || self.get(path, *from).is_none() {
                continue;
            }
            let merged = self.get(path, *to).is_some();
            self.move_line(path, *from, *to);
            out.push((path.clone(), *from, *to, merged));
        }
        out
    }

    /// Ctrl+F8: adds a plain breakpoint or removes the existing one; true when added.
    pub fn toggle(&mut self, path: &Path, line: usize) -> bool {
        if self.remove(path, line) {
            return false;
        }
        self.set(path, line, LineBp::default());
        true
    }

    /// Ctrl+Alt+Shift+F8: adds a temporary breakpoint or removes the existing one.
    pub fn toggle_temporary(&mut self, path: &Path, line: usize) -> bool {
        if self.remove(path, line) {
            return false;
        }
        self.set(
            path,
            line,
            LineBp {
                temporary: true,
                ..LineBp::default()
            },
        );
        true
    }

    /// 1-based lines with breakpoints in `path`.
    pub fn lines(&self, path: &Path) -> Vec<usize> {
        self.files
            .get(path)
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Every breakpoint, sorted by file and line.
    pub fn all(&self) -> Vec<(PathBuf, usize, LineBp)> {
        let mut out: Vec<(PathBuf, usize, LineBp)> = self
            .files
            .iter()
            .flat_map(|(p, m)| m.iter().map(move |(l, b)| (p.clone(), *l, b.clone())))
            .collect();
        out.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        out
    }

    pub fn is_empty(&self) -> bool {
        self.files.values().all(|m| m.is_empty())
    }

    /// Moves breakpoints after an edit that changed the line count by `delta`
    /// starting at 1-based `first_edit_line`; breakpoints on deleted lines go.
    /// Returns whether anything moved.
    pub fn shift_lines(&mut self, path: &Path, first_edit_line: usize, delta: i64) -> bool {
        let Some(map) = self.files.get_mut(path) else {
            return false;
        };
        let shift = |line: usize| -> Option<usize> {
            if line > first_edit_line {
                let n = line as i64 + delta;
                (n >= first_edit_line as i64 && n >= 1).then_some(n as usize)
            } else {
                Some(line)
            }
        };
        let mut moved: BTreeMap<usize, LineBp> = BTreeMap::new();
        for (l, b) in map.iter() {
            if let Some(n) = shift(*l) {
                moved.entry(n).or_insert_with(|| b.clone());
            }
        }
        if moved == *map {
            return false;
        }
        *map = moved;
        true
    }

    /// Gutter glyphs (0-based lines) for `path`.
    pub fn glyphs(&self, path: &Path, session: Option<&DebugSession>) -> Vec<(usize, BpGlyph)> {
        let Some(map) = self.files.get(path) else {
            return Vec::new();
        };
        map.iter()
            .map(|(l, b)| {
                (
                    l.saturating_sub(1),
                    b.glyph(self.muted, session.and_then(|s| s.is_verified(path, *l))),
                )
            })
            .collect()
    }

    /// The breakpoints of `path` as the debugger gets them.
    pub fn to_debug(&self, path: &Path) -> Vec<Breakpoint> {
        self.files
            .get(path)
            .map(|m| m.iter().map(|(l, b)| b.to_debug(path, *l)).collect())
            .unwrap_or_default()
    }

    /// All line breakpoints for a new debug session.
    pub fn debug_map(&self) -> HashMap<PathBuf, Vec<Breakpoint>> {
        self.files
            .keys()
            .map(|p| (p.clone(), self.to_debug(p)))
            .filter(|(_, l)| !l.is_empty())
            .collect()
    }

    /// The exception breakpoints as the debugger gets them.
    pub fn debug_exceptions(&self) -> ExceptionBreakpoints {
        let e = &self.exceptions;
        ExceptionBreakpoints {
            on_raise: e.on_raise,
            on_terminate: e.on_terminate,
            user_unhandled: e.user_unhandled,
            classes: e
                .classes
                .iter()
                .map(|c| ExceptionClassBreakpoint {
                    name: c.name.clone(),
                    enabled: c.enabled,
                    on_raise: c.on_raise,
                    on_terminate: c.on_terminate,
                })
                .collect(),
        }
    }

    /// Sends the breakpoints of `path` and the global settings to a live session.
    pub fn sync(&self, path: &Path, session: &mut DebugSession) {
        session.set_breakpoints(path, self.to_debug(path));
        self.sync_global(session);
    }

    /// Sends the exception breakpoints and the mute state to a live session.
    pub fn sync_global(&self, session: &mut DebugSession) {
        session.set_exception_breakpoints(self.debug_exceptions());
        session.set_muted(self.muted);
    }

    /// Files whose line breakpoints differ between `self` and `other`.
    pub fn paths_differing(&self, other: &BreakpointStore) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .files
            .keys()
            .chain(other.files.keys())
            .filter(|p| {
                self.files.get(*p).filter(|m| !m.is_empty())
                    != other.files.get(*p).filter(|m| !m.is_empty())
            })
            .cloned()
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }
}

/// Files edited in the IDE during one debug session. debugpy checks breakpoint
/// lines against the file on disk, so in a file whose editor text differs from
/// it (unsaved edits), or was edited since the program started, the line it
/// moves a breakpoint to is not a line of the editor: such moves are not taken over.
#[derive(Clone, Debug, Default)]
pub struct EditedFiles {
    session: u64,
    /// Document version of each open file when the session first saw it.
    versions: HashMap<PathBuf, u64>,
    edited: HashSet<PathBuf>,
}

impl EditedFiles {
    /// Looks at the open documents, `(path, version, modified)`, during session
    /// `session` (a new session starts with nothing edited).
    pub fn observe(&mut self, session: u64, docs: impl IntoIterator<Item = (PathBuf, u64, bool)>) {
        if session != self.session {
            *self = EditedFiles {
                session,
                ..Default::default()
            };
        }
        for (path, version, modified) in docs {
            let first = *self.versions.entry(path.clone()).or_insert(version);
            if modified || first != version {
                self.edited.insert(path);
            }
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.edited.contains(path)
    }
}

/// Breakpoint text typed in the breakpoint popup or the Breakpoints dialog and
/// not applied yet. A condition, hit count, log message or exception class name
/// reaches the breakpoint (and a running program) once it is complete: when its
/// field loses focus (Enter, Tab, a click elsewhere) or the editor closes.
/// Applied per keystroke, typing `i > 30` would first suspend the program on
/// `i`, and `i >` floods the console with evaluation errors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PendingEdits {
    /// A line breakpoint's properties as shown: `(path, line, properties)`.
    pub line: Option<(PathBuf, usize, LineBp)>,
    /// An exception class name as typed: `(index in the class list, name)`.
    pub exception: Option<(usize, String)>,
}

impl PendingEdits {
    /// The properties to show for the breakpoint at `line` of `path`: `stored`
    /// with the text being typed.
    pub fn shown(&self, path: &Path, line: usize, stored: &LineBp) -> LineBp {
        match &self.line {
            Some((p, l, typed)) if p == path && *l == line => typed.with_switches_of(stored),
            _ => stored.clone(),
        }
    }

    /// What to apply to the breakpoint at `line` of `path` (now `stored`) whose
    /// editors show `shown`: switches at once, typed text only once `complete`.
    /// Typed text not applied stays pending.
    pub fn apply(
        &mut self,
        path: &Path,
        line: usize,
        stored: &LineBp,
        shown: LineBp,
        complete: bool,
    ) -> LineBp {
        let apply = if complete {
            shown.clone()
        } else {
            stored.with_switches_of(&shown)
        };
        self.line = (apply != shown).then(|| (path.to_path_buf(), line, shown));
        apply
    }

    /// The name to show for exception class `index`: what is being typed, else `stored`.
    pub fn shown_exception(&self, index: usize, stored: &str) -> String {
        match &self.exception {
            Some((i, name)) if *i == index => name.clone(),
            _ => stored.to_string(),
        }
    }

    /// The name to apply to exception class `index` (now `stored`) whose field
    /// shows `shown`: once `complete`; until then it stays pending.
    pub fn apply_exception(
        &mut self,
        index: usize,
        stored: &str,
        shown: String,
        complete: bool,
    ) -> String {
        if complete || shown == stored {
            self.exception = None;
            return shown;
        }
        self.exception = Some((index, shown));
        stored.to_string()
    }
}

/// What the Breakpoints dialog has selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Line(PathBuf, usize),
    AnyException,
    Exception(usize),
}

/// State of the Breakpoints dialog (View Breakpoints, Ctrl+Shift+F8).
#[derive(Clone, Debug, Default)]
pub struct BreakpointsDialog {
    pub selected: Option<Selection>,
    /// Class name typed for a new exception breakpoint.
    pub new_exception: String,
}

/// The breakpoint popup (right-click on a breakpoint, Ctrl+Shift+F8 on its line).
#[derive(Clone, Debug)]
pub struct BreakpointPopup {
    pub path: PathBuf,
    /// 1-based line.
    pub line: usize,
    pub anchor: egui::Pos2,
    pub focus_condition: bool,
}

/// Editors for a line breakpoint's properties. Returns whether a text field
/// lost focus (Enter, Tab or a click elsewhere): what was typed there is
/// complete (see [`PendingEdits`]).
pub fn properties_ui(
    ui: &mut egui::Ui,
    bp: &mut LineBp,
    theme: &crate::theme::UiTheme,
    focus_condition: bool,
    completion_names: &[String],
) -> bool {
    let mut left_field = false;
    ui.checkbox(&mut bp.enabled, "Enabled");
    ui.checkbox(&mut bp.suspend, "Suspend").on_hover_text(
        "Stop the program when the breakpoint is hit. Without it the breakpoint only logs.",
    );
    egui::Grid::new("bp_props").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("Condition:");
        let r = ui.add(egui::TextEdit::singleline(&mut bp.condition).hint_text("Python expression, e.g. i == 3").desired_width(280.0));
        if focus_condition {
            r.request_focus();
        }
        left_field |= r.lost_focus();
        let prefix=bp.condition.chars().rev().take_while(|character|character.is_alphanumeric()||*character=='_').collect::<String>().chars().rev().collect::<String>();
        ui.menu_button("Complete condition",|ui|{for name in completion_names.iter().filter(|name|name.starts_with(&prefix)).take(80){if ui.button(name).clicked(){let start=bp.condition.len().saturating_sub(prefix.len());bp.condition.replace_range(start..,name);left_field=true;ui.close();}}});

        ui.end_row();
        ui.label("Hit count:");
        left_field |= ui.add(egui::TextEdit::singleline(&mut bp.hit_condition).hint_text("e.g. 3, >= 5, % 2").desired_width(120.0)).on_hover_text("Fire only on these hits: a number (that hit), >= N, > N, == N or % N (every N-th)").lost_focus();
        ui.end_row();
        ui.label("Evaluate and log:");
        left_field |= ui.add(egui::TextEdit::singleline(&mut bp.log_message).hint_text("text with {expressions}").desired_width(280.0)).lost_focus();
        ui.end_row();
    });
    ui.checkbox(&mut bp.log_hit, "Log \"Breakpoint hit\" message");
    ui.checkbox(&mut bp.temporary, "Remove once hit");
    ui.horizontal(|ui| {
        ui.label("Group");
        ui.text_edit_singleline(&mut bp.metadata.group);
    });
    ui.horizontal(|ui| {
        ui.label("Description");
        ui.text_edit_singleline(&mut bp.metadata.description);
    });
    ui.checkbox(&mut bp.metadata.log_stack, "Log stack trace");
    ui.horizontal(|ui| {
        let suspend_label = ui.label("Suspend policy");
        use rustpy_settings::debugger_workspace::SuspendPolicy;
        egui::ComboBox::from_id_salt("bp_suspend_policy")
            .selected_text(format!("{:?}", bp.metadata.suspend_policy))
            .show_ui(ui, |ui| {
                for policy in [
                    SuspendPolicy::All,
                    SuspendPolicy::Thread,
                    SuspendPolicy::None,
                ] {
                    ui.selectable_value(
                        &mut bp.metadata.suspend_policy,
                        policy,
                        format!("{policy:?}"),
                    );
                }
            })
            .combo_labelled(&suspend_label);
    });
    ui.label(
        "Thread suspension policy applies to the session because debugpy configures it globally.",
    );
    let mut dependent = bp.metadata.depends_on.is_some();
    if ui
        .checkbox(&mut dependent, "Enable after another breakpoint is hit")
        .changed()
    {
        bp.metadata.depends_on = dependent.then(|| (std::path::PathBuf::new(), 1));
    }
    if let Some((path, line)) = &mut bp.metadata.depends_on {
        let mut text = path.to_string_lossy().into_owned();
        ui.horizontal(|ui| {
            ui.label("Master file");
            if ui.text_edit_singleline(&mut text).changed() {
                *path = text.clone().into();
            }
            ui.label("Line");
            ui.add(egui::DragValue::new(line).range(1..=usize::MAX));
        });
    }

    if !bp.suspend && !bp.log_hit && bp.log_message.trim().is_empty() {
        ui.label(
            egui::RichText::new("Without Suspend or a log message this breakpoint does nothing.")
                .color(theme.warning)
                .small(),
        );
    }
    if !bp.suspend && bp.temporary {
        ui.label(
            egui::RichText::new("Remove once hit applies when the breakpoint suspends.")
                .color(theme.text_dim)
                .small(),
        );
    }
    left_field
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_and_persistence() {
        let p = PathBuf::from("/p/a.py");
        let mut s = BreakpointStore::default();
        assert!(s.toggle(&p, 3));
        assert!(s.toggle_temporary(&p, 5));
        assert!(!s.toggle(&p, 5), "toggling removes whatever is there");
        assert!(s.toggle_temporary(&p, 7));
        s.set(
            &p,
            9,
            LineBp {
                enabled: false,
                condition: "x > 1".into(),
                ..LineBp::default()
            },
        );
        s.muted = true;
        let mut ws = Workspace::default();
        s.write_to(&mut ws);
        assert_eq!(
            ws.breakpoints,
            vec![(p.clone(), vec![3, 7])],
            "older versions see the enabled lines"
        );
        assert_eq!(ws.line_breakpoints.len(), 3);
        assert!(ws.breakpoints_muted);
        let back = BreakpointStore::from_workspace(&ws);
        assert_eq!(back, s);
        assert!(back.get(&p, 7).unwrap().temporary);
        // Workspaces written by older versions.
        let old = Workspace {
            breakpoints: vec![(p.clone(), vec![2, 4])],
            ..Workspace::default()
        };
        let s = BreakpointStore::from_workspace(&old);
        assert_eq!(s.lines(&p), vec![2, 4]);
        assert_eq!(s.get(&p, 2), Some(&LineBp::default()));
    }

    #[test]
    fn glyphs_follow_the_properties() {
        let p = PathBuf::from("/p/a.py");
        let mut s = BreakpointStore::default();
        s.set(&p, 1, LineBp::default());
        s.set(
            &p,
            2,
            LineBp {
                enabled: false,
                ..LineBp::default()
            },
        );
        s.set(
            &p,
            3,
            LineBp {
                condition: "x".into(),
                ..LineBp::default()
            },
        );
        s.set(
            &p,
            4,
            LineBp {
                suspend: false,
                log_message: "{x}".into(),
                ..LineBp::default()
            },
        );
        s.set(
            &p,
            5,
            LineBp {
                temporary: true,
                ..LineBp::default()
            },
        );
        s.set(
            &p,
            6,
            LineBp {
                hit_condition: ">= 2".into(),
                ..LineBp::default()
            },
        );
        assert_eq!(
            s.glyphs(&p, None),
            vec![
                (0, BpGlyph::Enabled),
                (1, BpGlyph::Disabled),
                (2, BpGlyph::Conditional),
                (3, BpGlyph::Log),
                (4, BpGlyph::Temporary),
                (5, BpGlyph::Conditional)
            ]
        );
        s.muted = true;
        assert_eq!(s.glyphs(&p, None)[0], (0, BpGlyph::Muted));
        assert_eq!(s.glyphs(&p, None)[1], (1, BpGlyph::Disabled));
        assert_eq!(
            LineBp::default().glyph(false, Some(false)),
            BpGlyph::Unverified
        );
    }

    #[test]
    fn debugger_view_of_breakpoints() {
        let p = PathBuf::from("/p/a.py");
        let bp = LineBp {
            suspend: false,
            log_hit: true,
            log_message: "x={x}\n".into(),
            condition: "  ".into(),
            hit_condition: " 3 ".into(),
            ..LineBp::default()
        };
        let d = bp.to_debug(&p, 12);
        assert_eq!(d.line, 12);
        assert_eq!(d.condition, None);
        assert_eq!(d.hit_condition.as_deref(), Some("3"));
        assert_eq!(
            d.log_message.as_deref(),
            Some("Breakpoint reached: a.py:12\nx={x}")
        );
        assert!(!d.suspend);
        assert_eq!(LineBp::default().to_debug(&p, 1), Breakpoint::new(1));
        let mut s = BreakpointStore::default();
        s.exceptions
            .classes
            .push(rustpy_settings::ExceptionClassState {
                name: "ValueError".into(),
                ..Default::default()
            });
        let e = s.debug_exceptions();
        assert!(e.on_terminate && !e.on_raise);
        assert_eq!(
            e.classes,
            vec![ExceptionClassBreakpoint {
                name: "ValueError".into(),
                enabled: true,
                on_raise: false,
                on_terminate: true
            }]
        );
    }

    #[test]
    fn edits_shift_breakpoints() {
        let p = PathBuf::from("/p/a.py");
        let mut s = BreakpointStore::default();
        s.toggle(&p, 2);
        s.set(
            &p,
            5,
            LineBp {
                condition: "c".into(),
                ..LineBp::default()
            },
        );
        s.toggle(&p, 9);
        // Two lines inserted at line 3.
        assert!(s.shift_lines(&p, 3, 2));
        assert_eq!(s.lines(&p), vec![2, 7, 11]);
        assert_eq!(
            s.get(&p, 7).unwrap().condition,
            "c",
            "properties move with the line"
        );
        // Lines 4..=7 deleted: the breakpoint on a deleted line goes.
        assert!(s.shift_lines(&p, 4, -4));
        assert_eq!(s.lines(&p), vec![2, 7]);
        assert!(
            !s.shift_lines(&p, 20, 1),
            "edits below the last breakpoint move nothing"
        );
        assert!(!s.shift_lines(Path::new("/other.py"), 1, 3));
    }

    #[test]
    fn debugger_moves_keep_properties_and_merge() {
        let p = PathBuf::from("/p/a.py");
        let mut s = BreakpointStore::default();
        s.set(
            &p,
            3,
            LineBp {
                condition: "x".into(),
                ..LineBp::default()
            },
        );
        s.set(
            &p,
            8,
            LineBp {
                temporary: true,
                ..LineBp::default()
            },
        );
        s.toggle(&p, 6);
        assert!(s.move_line(&p, 3, 2));
        assert_eq!(s.lines(&p), vec![2, 6, 8]);
        assert_eq!(
            s.get(&p, 2).unwrap().condition,
            "x",
            "properties move with it"
        );
        // Onto an existing breakpoint: that one stays.
        assert!(s.move_line(&p, 8, 6));
        assert_eq!(s.lines(&p), vec![2, 6]);
        assert!(!s.get(&p, 6).unwrap().temporary);
        assert!(!s.move_line(&p, 30, 31) && !s.move_line(Path::new("/b.py"), 1, 2));
    }

    /// The review's repro: two lines typed at the top of a file while debugging
    /// shift its breakpoints 4 → 6 and 7 → 9; debugpy checks those against the
    /// unchanged file on disk and answers 4 and 8. Those moves must not reach
    /// the editor or the workspace; in unedited files they are taken over.
    #[test]
    fn debugger_moves_are_only_taken_over_in_unedited_files() {
        let (a, b) = (PathBuf::from("/p/a.py"), PathBuf::from("/p/b.py"));
        let mut s = BreakpointStore::default();
        s.toggle(&a, 4);
        s.toggle(&a, 7);
        s.toggle(&b, 3);
        let mut edited = EditedFiles::default();
        edited.observe(1, [(a.clone(), 10, false), (b.clone(), 3, false)]);
        assert!(!edited.contains(&a) && !edited.contains(&b));
        // Lines typed into a.py (unsaved): the editor shifts its breakpoints.
        edited.observe(1, [(a.clone(), 11, true), (b.clone(), 3, false)]);
        assert!(s.shift_lines(&a, 1, 2));
        assert_eq!(s.lines(&a), vec![6, 9]);
        let moved = [(a.clone(), 6, 4), (a.clone(), 9, 8), (b.clone(), 3, 2)];
        assert_eq!(
            s.adopt_moves(&moved, |p| edited.contains(p)),
            vec![(b.clone(), 3, 2, false)]
        );
        assert_eq!(s.lines(&a), vec![6, 9], "the editor's lines stay");
        assert_eq!(
            s.lines(&b),
            vec![2],
            "an unedited file follows the debugger"
        );
        let mut ws = Workspace::default();
        s.write_to(&mut ws);
        assert_eq!(
            ws.line_breakpoints
                .iter()
                .map(|b| (b.path.clone(), b.line))
                .collect::<Vec<_>>(),
            vec![(a.clone(), 6), (a.clone(), 9), (b.clone(), 2)]
        );
        // Saved again, a.py still differs from what the program loaded; a new session starts afresh.
        edited.observe(1, [(a.clone(), 11, false)]);
        assert!(edited.contains(&a));
        edited.observe(2, [(a.clone(), 11, false)]);
        assert!(!edited.contains(&a));
        // A file already modified when the session starts counts as edited.
        edited.observe(3, [(b.clone(), 5, true)]);
        assert!(edited.contains(&b));
        // Onto an existing breakpoint it merges; moves of breakpoints that are gone are ignored.
        s.toggle(&b, 5);
        assert_eq!(
            s.adopt_moves(&[(b.clone(), 5, 2), (b.clone(), 7, 6)], |_| false),
            vec![(b.clone(), 5, 2, true)]
        );
        assert_eq!(s.lines(&b), vec![2]);
    }

    /// Typing `i > 30` into a condition used to reach the running program as `i`
    /// (true: it suspended at once) and `i >` (an error per hit): typed text now
    /// applies once complete, switches at once.
    #[test]
    fn typed_breakpoint_text_applies_once_complete() {
        let p = PathBuf::from("/p/a.py");
        let stored = LineBp::default();
        let mut pending = PendingEdits::default();
        let mut applied = Vec::new();
        for typed in ["i", "i ", "i >", "i > 3", "i > 30"] {
            let shown = LineBp {
                condition: typed.into(),
                ..pending.shown(&p, 4, &stored)
            };
            applied.push(pending.apply(&p, 4, &stored, shown, false));
        }
        assert!(
            applied.iter().all(|b| *b == stored),
            "nothing applied while typing: {applied:?}"
        );
        assert_eq!(pending.shown(&p, 4, &stored).condition, "i > 30");
        assert_eq!(
            pending.shown(&p, 5, &stored),
            stored,
            "only that breakpoint"
        );
        // A switch applies at once, with the old condition; the typed one stays pending.
        let shown = LineBp {
            enabled: false,
            ..pending.shown(&p, 4, &stored)
        };
        let now = pending.apply(&p, 4, &stored, shown, false);
        assert_eq!((now.enabled, now.condition.as_str()), (false, ""));
        // Switches changed elsewhere (the dialog's list) win over the draft's.
        let stored = now;
        assert!(
            pending
                .shown(
                    &p,
                    4,
                    &LineBp {
                        enabled: true,
                        ..stored.clone()
                    }
                )
                .enabled
        );
        // Enter, leaving the field or closing completes it.
        let shown = pending.shown(&p, 4, &stored);
        let done = pending.apply(&p, 4, &stored, shown, true);
        assert_eq!((done.enabled, done.condition.as_str()), (false, "i > 30"));
        assert_eq!(pending, PendingEdits::default());
        // Exception class names likewise.
        assert_eq!(pending.apply_exception(0, "", "Val".into(), false), "");
        assert_eq!(pending.shown_exception(0, ""), "Val");
        assert_eq!(pending.shown_exception(1, "KeyError"), "KeyError");
        assert_eq!(
            pending.apply_exception(0, "", "ValueError".into(), true),
            "ValueError"
        );
        assert_eq!(pending.exception, None);
    }

    #[test]
    fn differing_paths() {
        let (a, b) = (PathBuf::from("/p/a.py"), PathBuf::from("/p/b.py"));
        let mut s = BreakpointStore::default();
        s.toggle(&a, 1);
        s.toggle(&b, 2);
        let mut t = s.clone();
        assert!(s.paths_differing(&t).is_empty());
        t.set(
            &b,
            2,
            LineBp {
                enabled: false,
                ..LineBp::default()
            },
        );
        t.toggle(&PathBuf::from("/p/c.py"), 3);
        assert_eq!(
            s.paths_differing(&t),
            vec![b.clone(), PathBuf::from("/p/c.py")]
        );
        t.toggle(&a, 1);
        assert_eq!(t.paths_differing(&s), vec![a, b, PathBuf::from("/p/c.py")]);
    }
}
