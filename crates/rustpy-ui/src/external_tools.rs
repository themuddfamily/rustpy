//! External Tools and File Watchers (Settings › Tools, roadmap §5): `$Macro$`
//! expansion and the command line a tool runs.
//!
//! The argument template is split into arguments first (whitespace separates,
//! `"…"` and `'…'` group), and macros are then expanded inside each argument
//! without splitting again. A file path with spaces, quotes or shell
//! characters therefore stays one argument, and the program runs directly
//! (no shell), which also works on Windows.
//!
//! The menus that offer tools (Tools › External Tools, the editor and the
//! Project view context menus) share [`tools_menu`]; the two context menus
//! reach the application through [`request_run`] / [`take_request`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustpy_settings::ExternalTool;

use crate::actions::{KeyChord, Keymap};

// One launch authority shared by the IDE, prerequisite jobs and admitted hosts.
pub use crate::external_tool_workspace::{
    output_path as output_source_path, plan as execution_plan, ConsoleLinks, Plan as ExecutionPlan,
    PlanError as ExecutionPlanError,
};

/// Values the `$Macro$`s expand to, gathered by the application when a tool runs.
#[derive(Clone, Debug, Default)]
pub struct MacroContext {
    /// The file the tool runs on: the active editor's, the Project view
    /// selection or, for a File Watcher, the saved file.
    pub file: Option<PathBuf>,
    pub project_root: Option<PathBuf>,
    /// 1-based caret line and column.
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub selected_text: Option<String>,
    /// 1-based first and last line of the selection.
    pub selection_lines: Option<(usize, usize)>,
    pub clipboard: Option<String>,
    /// The project interpreter (the `python` executable).
    pub interpreter: Option<PathBuf>,
    /// The answer to `$Prompt$`, asked once per run.
    pub prompt: Option<String>,
}

/// What a tool runs: the program, one entry per argument, and its directory
/// (`None`: the project root).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationError {
    /// `$Prompt$` is used and [`MacroContext::prompt`] is not set: ask, then build again.
    NeedsPrompt,
    /// The program field is empty.
    NoProgram,
}

/// Macros shown in Settings › Tools › External Tools: `(name, description)`.
pub const MACROS: &[(&str, &str)] = &[
    ("FilePath", "Absolute path of the file"),
    ("FileDir", "Directory of the file"),
    ("FileName", "File name"),
    (
        "FileNameWithoutExtension",
        "File name without its last extension",
    ),
    (
        "FileNameWithoutAllExtensions",
        "File name up to the first dot",
    ),
    ("FileExt", "Extension of the file"),
    ("FileDirName", "Name of the file's directory"),
    ("FileRelativePath", "File path relative to the project root"),
    (
        "FileRelativeDir",
        "File directory relative to the project root",
    ),
    (
        "FilePathRelativeToProjectRoot",
        "File path relative to the project root",
    ),
    (
        "FileDirRelativeToProjectRoot",
        "File directory relative to the project root",
    ),
    ("ProjectFileDir", "Project root directory"),
    ("ContentRoot", "Project root directory"),
    ("ProjectName", "Name of the project"),
    ("LineNumber", "Caret line (1-based)"),
    ("ColumnNumber", "Caret column (1-based)"),
    ("SelectedText", "Text selected in the editor"),
    ("SelectionStartLine", "First line of the selection"),
    ("SelectionEndLine", "Last line of the selection"),
    ("ClipboardContent", "Text on the clipboard"),
    ("Prompt", "Asks for a value when the tool runs"),
    (
        "PyInterpreterDirectory",
        "Directory of the project interpreter",
    ),
    ("OSName", "Operating system: windows, linux or macos"),
    ("TempDir", "Temporary directory"),
];

fn path_text(p: &Path) -> String {
    p.display().to_string()
}

fn relative(path: &Path, root: Option<&Path>) -> Option<String> {
    let rel = path.strip_prefix(root?).ok()?;
    Some(rel.display().to_string())
}

/// The value of macro `name`, or `None` when the macro is unknown (it then
/// stays in the text as written).
fn macro_value(name: &str, ctx: &MacroContext) -> Option<String> {
    let file = ctx.file.as_deref();
    let root = ctx.project_root.as_deref();
    let file_name = || {
        file.and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let dir = || file.and_then(Path::parent);
    Some(match name {
        "FilePath" => file.map(path_text).unwrap_or_default(),
        "FileDir" => dir().map(path_text).unwrap_or_default(),
        "FileName" => file_name(),
        "FileNameWithoutExtension" => file
            .and_then(Path::file_stem)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "FileNameWithoutAllExtensions" => {
            let n = file_name();
            // A leading dot (".gitignore") is part of the name, not an extension.
            match n.char_indices().skip(1).find(|(_, c)| *c == '.') {
                Some((i, _)) => n[..i].to_string(),
                None => n,
            }
        }
        "FileExt" => file
            .and_then(Path::extension)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "FileDirName" => dir()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "FileRelativePath" | "FilePathRelativeToProjectRoot" => {
            file.and_then(|f| relative(f, root)).unwrap_or_default()
        }
        "FileRelativeDir" | "FileDirRelativeToProjectRoot" => {
            dir().and_then(|d| relative(d, root)).unwrap_or_default()
        }
        "ProjectFileDir" | "ContentRoot" => root.map(path_text).unwrap_or_default(),
        "ProjectName" => root
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "LineNumber" => ctx.line.map(|l| l.to_string()).unwrap_or_default(),
        "ColumnNumber" => ctx.column.map(|c| c.to_string()).unwrap_or_default(),
        "SelectedText" => ctx.selected_text.clone().unwrap_or_default(),
        "SelectionStartLine" => ctx
            .selection_lines
            .map(|(s, _)| s.to_string())
            .unwrap_or_default(),
        "SelectionEndLine" => ctx
            .selection_lines
            .map(|(_, e)| e.to_string())
            .unwrap_or_default(),
        "ClipboardContent" => ctx.clipboard.clone().unwrap_or_default(),
        "Prompt" => ctx.prompt.clone().unwrap_or_default(),
        "PyInterpreterDirectory" => ctx
            .interpreter
            .as_deref()
            .and_then(Path::parent)
            .map(path_text)
            .unwrap_or_default(),
        "OSName" => if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        }
        .to_string(),
        "TempDir" => path_text(&std::env::temp_dir()),
        // Settings › Path Variables (`$USER_HOME$`, user-defined folders).
        _ => {
            return rustpy_settings::path_variables::active()
                .value(name, root)
                .map(|p| path_text(&p))
        }
    })
}

/// Replaces every known `$Name$` in `text`; unknown ones and lone `$` stay.
/// `$$` represents one literal dollar, without interpreting the following text.
pub fn expand_macros(text: &str, ctx: &MacroContext) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(literal) = after.strip_prefix('$') {
            out.push('$');
            rest = literal;
            continue;
        }
        let name_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if name_len > 0 && after[name_len..].starts_with('$') {
            if let Some(v) = macro_value(&after[..name_len], ctx) {
                out.push_str(&v);
                rest = &after[name_len + 1..];
                continue;
            }
        }
        out.push('$');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Splits an argument template like a shell would, without escapes (so
/// Windows paths keep their backslashes): `(argument, was quoted)`.
pub fn split_template(s: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut quoted = false;
    let mut started = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                quoted = true;
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    out.push((std::mem::take(&mut cur), quoted));
                    quoted = false;
                    started = false;
                }
            }
            None => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push((cur, quoted));
    }
    out
}

/// Whether running the tool asks for `$Prompt$` first.
pub fn uses_prompt(program: &str, arguments: &str, working_dir: &str) -> bool {
    [program, arguments, working_dir].iter().any(|text| {
        let mut rest = *text;
        while let Some(start) = rest.find('$') {
            rest = &rest[start..];
            if let Some(after) = rest.strip_prefix("$$") {
                rest = after;
            } else if rest.starts_with("$Prompt$") {
                return true;
            } else {
                rest = &rest[1..];
            }
        }
        false
    })
}

/// Builds the command line of a tool. `exists` tells whether a path is an
/// existing file (it decides how an unquoted program with spaces is read).
pub fn build_invocation(
    program: &str,
    arguments: &str,
    working_dir: &str,
    ctx: &MacroContext,
    exists: impl Fn(&Path) -> bool,
) -> Result<Invocation, InvocationError> {
    if ctx.prompt.is_none() && uses_prompt(program, arguments, working_dir) {
        return Err(InvocationError::NeedsPrompt);
    }
    let template = program.trim();
    let mut args: Vec<String> = Vec::new();
    // The program is one path ("C:\Program Files\Tool\tool.exe" included). An
    // unquoted program that is not an existing file but has spaces, like the
    // `python -m black` earlier versions accepted, is a command line: its
    // first word is the program and the rest are leading arguments.
    let whole = expand_macros(template, ctx);
    let quoted = template.starts_with('"') || template.starts_with('\'');
    let program_path =
        if quoted || (!exists(Path::new(&whole)) && whole.split_whitespace().nth(1).is_some()) {
            let mut words = split_template(template)
                .into_iter()
                .map(|(w, q)| (expand_macros(&w, ctx), q));
            let first = words.next().map(|(w, _)| w).unwrap_or_default();
            args.extend(words.filter(|(w, q)| *q || !w.is_empty()).map(|(w, _)| w));
            first
        } else {
            whole
        };
    if program_path.trim().is_empty() {
        return Err(InvocationError::NoProgram);
    }
    for (word, was_quoted) in split_template(arguments) {
        let value = expand_macros(&word, ctx);
        // An unquoted macro that expands to nothing disappears, as in a shell.
        if value.is_empty() && !was_quoted {
            continue;
        }
        args.push(value);
    }
    let dir = expand_macros(working_dir.trim(), ctx);
    let dir = dir.trim().trim_matches(|c| c == '"' || c == '\'');
    Ok(Invocation {
        program: PathBuf::from(program_path),
        args,
        working_dir: (!dir.is_empty()).then(|| PathBuf::from(dir)),
    })
}

/// [`build_invocation`] for a configured tool.
pub fn tool_invocation(
    tool: &ExternalTool,
    ctx: &MacroContext,
) -> Result<Invocation, InvocationError> {
    build_invocation(
        &tool.program,
        &tool.arguments,
        &tool.working_dir,
        ctx,
        |p| p.is_file(),
    )
}

/// Tools grouped for menus: `(group, tool names)` in the order the groups
/// first appear; tools without a group come first, ungrouped.
pub fn grouped(tools: &[ExternalTool]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for t in tools.iter().filter(|t| !t.name.trim().is_empty()) {
        let group = t.group.trim().to_string();
        match out.iter_mut().find(|(g, _)| *g == group) {
            Some((_, names)) => names.push(t.name.clone()),
            None => out.push((group, vec![t.name.clone()])),
        }
    }
    out.sort_by_key(|(g, _)| !g.is_empty());
    out
}

/// Draws the tools (inside an "External Tools" menu): ungrouped tools first,
/// then one submenu per group. Returns the clicked tool's name.
pub fn tools_menu(ui: &mut egui::Ui, groups: &[(String, Vec<String>)]) -> Option<String> {
    let mut clicked = None;
    for (group, names) in groups {
        let mut items = |ui: &mut egui::Ui| {
            for name in names {
                if ui.button(name).clicked() {
                    clicked = Some(name.clone());
                    ui.close();
                }
            }
        };
        if group.is_empty() {
            items(ui);
        } else {
            ui.menu_button(group, items);
        }
    }
    clicked
}

/// A tool chosen in a context menu: run it on `file` (`None`: the active editor's file).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolRequest {
    pub name: String,
    pub file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuPlace {
    Editor,
    Project,
}
fn project_menu_id() -> egui::Id {
    egui::Id::new("rustpy.external_tools.project-menu")
}
fn menu_id() -> egui::Id {
    egui::Id::new("rustpy.external_tools.menu")
}

fn request_id() -> egui::Id {
    egui::Id::new("rustpy.external_tools.request")
}

fn entries_id() -> egui::Id {
    egui::Id::new("rustpy.external_tools.entries")
}

/// A tool as Find Action lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolEntry {
    pub name: String,
    pub group: String,
    /// The tool's working shortcut ("Ctrl+Alt+Shift+B"; empty when it has none
    /// or the keymap takes the key, see [`tool_chord`]).
    pub shortcut: String,
}

/// Makes the configured tools available to the context menus and Find Action
/// (cheap when unchanged).
pub fn publish(ctx: &egui::Context, tools: &[ExternalTool], keymap: &Keymap) {
    let groups = grouped(
        &tools
            .iter()
            .filter(|tool| tool.options.editor_menu)
            .cloned()
            .collect::<Vec<_>>(),
    );
    let project_groups = grouped(
        &tools
            .iter()
            .filter(|tool| tool.options.project_menu)
            .cloned()
            .collect::<Vec<_>>(),
    );
    ctx.data_mut(|data| data.insert_temp(project_menu_id(), Arc::new(project_groups)));
    let same = ctx.data(|d| {
        d.get_temp::<Arc<Vec<(String, Vec<String>)>>>(menu_id())
            .is_some_and(|g| *g == groups)
    });
    if !same {
        ctx.data_mut(|d| d.insert_temp(menu_id(), Arc::new(groups)));
    }
    let active = tool_shortcuts(tools, keymap);
    let shortcut = |tool: &ExternalTool| {
        active
            .iter()
            .find(|(_, name)| name == &tool.name)
            .map(|(chord, _)| chord.to_string())
            .unwrap_or_default()
    };
    let entries: Vec<ToolEntry> = tools
        .iter()
        .filter(|t| !t.name.trim().is_empty())
        .map(|t| ToolEntry {
            name: t.name.clone(),
            group: t.group.trim().to_string(),
            shortcut: shortcut(t),
        })
        .collect();
    let same = ctx.data(|d| {
        d.get_temp::<Arc<Vec<ToolEntry>>>(entries_id())
            .is_some_and(|e| *e == entries)
    });
    if !same {
        ctx.data_mut(|d| d.insert_temp(entries_id(), Arc::new(entries)));
    }
}

/// The tools for Find Action (see [`publish`]).
pub fn published_tools(ctx: &egui::Context) -> Arc<Vec<ToolEntry>> {
    ctx.data(|d| d.get_temp::<Arc<Vec<ToolEntry>>>(entries_id()))
        .unwrap_or_default()
}

/// A key that types or edits text when pressed without Ctrl or Alt: only the
/// function keys are left for tools.
fn types_text(chord: KeyChord) -> bool {
    let name = chord.key.name();
    let function_key =
        name.len() > 1 && name.starts_with('F') && name[1..].chars().all(|c| c.is_ascii_digit());
    !chord.ctrl && !chord.alt && !function_key
}

/// The key a tool's `shortcut` gives it (`None`: no shortcut), or why it gets
/// none: not a shortcut, a key the keymap gives to an action (the keymap wins,
/// also for editor actions such as Ctrl+D while the editor has no focus), or
/// a typing key.
pub fn tool_chord(shortcut: &str, keymap: &Keymap) -> Result<Option<KeyChord>, String> {
    let s = shortcut.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let Some(chord) = KeyChord::parse(s) else {
        return Err("not a shortcut (e.g. Ctrl+Alt+Shift+B)".into());
    };
    if let Some(a) = keymap.chord_owner(chord) {
        return Err(format!(
            "{chord} is {} in the keymap, which wins",
            a.text().trim_end_matches('…')
        ));
    }
    if types_text(chord) {
        return Err(format!("{chord} types or edits text: add Ctrl or Alt"));
    }
    Ok(Some(chord))
}

/// The tools' working shortcuts: `(shortcut, tool name)` (see [`tool_chord`]).
pub fn tool_shortcuts(tools: &[ExternalTool], keymap: &Keymap) -> Vec<(KeyChord, String)> {
    let candidates: Vec<_> = tools
        .iter()
        .filter(|tool| !tool.name.trim().is_empty())
        .filter_map(|tool| Some((tool_chord(&tool.shortcut, keymap).ok()??, tool.name.clone())))
        .collect();
    let mut counts = std::collections::HashMap::new();
    for (chord, _) in &candidates {
        *counts.entry(*chord).or_insert(0usize) += 1;
    }
    candidates
        .into_iter()
        .filter(|(chord, name)| {
            counts.get(chord) == Some(&1)
                && tools.iter().filter(|tool| &tool.name == name).count() == 1
        })
        .collect()
}

/// What is wrong with a tool's `shortcut`, for the settings page (see [`tool_chord`]).
pub fn shortcut_problem(shortcut: &str, keymap: &Keymap) -> Option<String> {
    tool_chord(shortcut, keymap).err()
}

/// The tool whose shortcut was pressed this frame (the key press is consumed).
/// A key the keymap binds is never taken, even when its action did not run
/// (an editor action is left to the editor, which runs later in the frame).
pub fn take_shortcut(
    ctx: &egui::Context,
    tools: &[ExternalTool],
    keymap: &Keymap,
) -> Option<String> {
    let shortcuts = tool_shortcuts(tools, keymap);
    if shortcuts.is_empty() {
        return None;
    }
    ctx.input_mut(|i| {
        let (pos, name) = i.events.iter().enumerate().find_map(|(pos, e)| match e {
            // Holding the keys does not start the tool again and again.
            egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => {
                if !Keymap::primary_modifiers(*modifiers)
                    || keymap.lookup_key(*key, *physical_key, *modifiers).is_some()
                {
                    return None;
                }
                let chord = KeyChord::new(*key, *modifiers);
                shortcuts
                    .iter()
                    .find(|(c, _)| *c == chord)
                    .map(|(_, name)| (pos, name.clone()))
            }
            _ => None,
        })?;
        i.events.remove(pos);
        Some(name)
    })
}

/// An "External Tools" submenu for a context menu (nothing when no tool is
/// configured); a click asks the application to run the tool on `file`.
pub fn context_submenu(ui: &mut egui::Ui, file: Option<&Path>) {
    context_submenu_at(ui, file, MenuPlace::Editor);
}
pub fn context_submenu_at(ui: &mut egui::Ui, file: Option<&Path>, place: MenuPlace) {
    let Some(groups) = ui.ctx().data(|d| {
        d.get_temp::<Arc<Vec<(String, Vec<String>)>>>(match place {
            MenuPlace::Editor => menu_id(),
            MenuPlace::Project => project_menu_id(),
        })
    }) else {
        return;
    };
    if groups.is_empty() {
        return;
    }
    ui.menu_button("External Tools", |ui| {
        if let Some(name) = tools_menu(ui, &groups) {
            request_run(
                ui.ctx(),
                ToolRequest {
                    name,
                    file: file.map(Path::to_path_buf),
                },
            );
        }
    });
}

/// Queues a tool run for the application (picked up by [`take_request`]).
pub fn request_run(ctx: &egui::Context, req: ToolRequest) {
    ctx.data_mut(|d| d.insert_temp(request_id(), req));
}

/// The tool run a context menu asked for, once.
pub fn take_request(ctx: &egui::Context) -> Option<ToolRequest> {
    ctx.data_mut(|d| d.remove_temp::<ToolRequest>(request_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> MacroContext {
        MacroContext {
            file: Some(PathBuf::from("/home/jane/My Projects/app/pkg/a b.py")),
            project_root: Some(PathBuf::from("/home/jane/My Projects/app")),
            line: Some(12),
            column: Some(5),
            selected_text: Some("x = $HOME; rm -rf /".into()),
            selection_lines: Some((3, 4)),
            clipboard: Some("clip board".into()),
            interpreter: Some(PathBuf::from("/home/jane/My Projects/app/.venv/bin/python")),
            prompt: None,
        }
    }

    fn build(
        program: &str,
        args: &str,
        dir: &str,
        c: &MacroContext,
    ) -> Result<Invocation, InvocationError> {
        build_invocation(program, args, dir, c, |_| false)
    }

    #[test]
    fn path_variables_expand_like_macros() {
        use rustpy_settings::path_variables::{install, PathVariable, PathVariables};
        // A name no other test uses: the definitions are process-wide.
        install(&PathVariables {
            variables: vec![PathVariable {
                name: "PV_TOOLS_SDK".into(),
                path: PathBuf::from("/opt/pv-tools-sdk"),
            }],
            ignored: Vec::new(),
        });
        let ctx = MacroContext {
            project_root: Some(PathBuf::from("/actual")),
            ..Default::default()
        };
        assert_eq!(
            expand_macros(
                "$PV_TOOLS_SDK$/bin --root=$PROJECT_DIR$ $PV_UNDEFINED$",
                &ctx
            ),
            "/opt/pv-tools-sdk/bin --root=/actual $PV_UNDEFINED$"
        );
        let inv = build(
            "$PV_TOOLS_SDK$/bin/tool",
            "$FilePath$",
            "$PV_TOOLS_SDK$",
            &ctx,
        )
        .unwrap();
        assert_eq!(inv.program, PathBuf::from("/opt/pv-tools-sdk/bin/tool"));
        assert_eq!(inv.working_dir, Some(PathBuf::from("/opt/pv-tools-sdk")));
    }

    #[test]
    fn escaped_dollars_and_macro_values_are_never_reexpanded() {
        let ctx = MacroContext {
            file: Some(PathBuf::from("/p/$ProjectFileDir$ $$ 'file'\n.py")),
            project_root: Some(PathBuf::from("/actual")),
            ..Default::default()
        };
        assert!(!uses_prompt(
            "$$Prompt$$/run",
            "--dir=$$Prompt$$",
            "$$Prompt$$"
        ));
        assert!(uses_prompt("run", "$$$Prompt$", ""));
        assert_eq!(expand_macros("$$FilePath$$", &ctx), "$FilePath$");
        assert_eq!(expand_macros("$$$ProjectFileDir$", &ctx), "$/actual");
        assert_eq!(
            expand_macros("$FilePath$", &ctx),
            "/p/$ProjectFileDir$ $$ 'file'\n.py"
        );
    }

    #[test]
    fn paths_with_spaces_stay_one_argument() {
        let inv = build(
            "black",
            "--line-length 88 $FilePath$",
            "$ProjectFileDir$",
            &ctx(),
        )
        .unwrap();
        assert_eq!(inv.program, PathBuf::from("black"));
        assert_eq!(
            inv.args,
            vec![
                "--line-length",
                "88",
                "/home/jane/My Projects/app/pkg/a b.py"
            ]
        );
        assert_eq!(
            inv.working_dir,
            Some(PathBuf::from("/home/jane/My Projects/app"))
        );
        // Quoting the macro (as users used to, for the shell) changes nothing.
        let quoted = build("black", "\"$FilePath$\"", "", &ctx()).unwrap();
        assert_eq!(quoted.args, vec!["/home/jane/My Projects/app/pkg/a b.py"]);
        assert_eq!(quoted.working_dir, None);
        // Macros inside a larger argument.
        let inner = build(
            "tool",
            "--file=$FilePath$:$LineNumber$:$ColumnNumber$",
            "",
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            inner.args,
            vec!["--file=/home/jane/My Projects/app/pkg/a b.py:12:5"]
        );
    }

    #[test]
    fn shell_characters_are_not_interpreted() {
        let inv = build(
            "echo",
            "$SelectedText$ '$ClipboardContent$' $Unknown$ $ 5$",
            "",
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            inv.args,
            vec!["x = $HOME; rm -rf /", "clip board", "$Unknown$", "$", "5$"]
        );
        let quotes = build("echo", "\"it's\" 'say \"hi\"'", "", &ctx()).unwrap();
        assert_eq!(quotes.args, vec!["it's", "say \"hi\""]);
    }

    #[test]
    fn windows_paths_keep_backslashes() {
        let mut c = ctx();
        c.file = Some(PathBuf::from(r"C:\Users\Jane Doe\proj\a.py"));
        c.project_root = None;
        let inv = build_invocation(
            r"C:\Program Files\Black\black.exe",
            "$FilePath$ --check",
            "",
            &c,
            |p| p == Path::new(r"C:\Program Files\Black\black.exe"),
        )
        .unwrap();
        assert_eq!(
            inv.program,
            PathBuf::from(r"C:\Program Files\Black\black.exe")
        );
        let file = c.file.as_ref().unwrap().display().to_string();
        assert_eq!(inv.args, vec![file.as_str(), "--check"]);
        let quoted = build(
            r#""C:\Program Files\Tool\tool.exe" --fast"#,
            r#""C:\out dir\x.txt""#,
            "",
            &c,
        )
        .unwrap();
        assert_eq!(
            quoted.program,
            PathBuf::from(r"C:\Program Files\Tool\tool.exe")
        );
        assert_eq!(quoted.args, vec!["--fast", r"C:\out dir\x.txt"]);
    }

    #[test]
    fn a_command_line_in_the_program_field_still_works() {
        let inv = build("python3 -m black", "-q $FileName$", "", &ctx()).unwrap();
        assert_eq!(inv.program, PathBuf::from("python3"));
        assert_eq!(inv.args, vec!["-m", "black", "-q", "a b.py"]);
        let interp = build("$PyInterpreterDirectory$/black", "", "", &ctx()).unwrap();
        assert_eq!(
            interp.program,
            PathBuf::from("/home/jane/My Projects/app/.venv/bin/black")
        );
        assert_eq!(
            build("  ", "x", "", &ctx()),
            Err(InvocationError::NoProgram)
        );
    }

    #[test]
    fn prompt_is_asked_once_then_used_everywhere() {
        let mut c = ctx();
        assert_eq!(
            build("pytest", "-k $Prompt$", "", &c),
            Err(InvocationError::NeedsPrompt)
        );
        c.prompt = Some("slow and not db".into());
        let inv = build("pytest", "-k $Prompt$ --tb=$Prompt$", "", &c).unwrap();
        assert_eq!(
            inv.args,
            vec!["-k", "slow and not db", "--tb=slow and not db"]
        );
    }

    #[test]
    fn empty_values_drop_unquoted_arguments_only() {
        let mut c = ctx();
        c.selected_text = None;
        let inv = build("tool", "$SelectedText$ \"$SelectedText$\" end", "", &c).unwrap();
        assert_eq!(inv.args, vec!["", "end"]);
    }

    #[test]
    fn file_macros() {
        let c = ctx();
        let e = |m: &str| expand_macros(m, &c);
        assert_eq!(e("$FileDir$"), "/home/jane/My Projects/app/pkg");
        assert_eq!(e("$FileNameWithoutExtension$"), "a b");
        assert_eq!(e("$FileExt$"), "py");
        assert_eq!(e("$FileDirName$"), "pkg");
        // Relative macros always use `/`, whatever the platform separator.
        assert_eq!(e("$FileRelativePath$"), "pkg/a b.py");
        assert_eq!(e("$FileDirRelativeToProjectRoot$"), "pkg");
        assert_eq!(e("$ProjectName$"), "app");
        assert_eq!(e("$SelectionStartLine$-$SelectionEndLine$"), "3-4");
        assert_eq!(
            e("$PyInterpreterDirectory$"),
            "/home/jane/My Projects/app/.venv/bin"
        );
        let mut dotted = c.clone();
        dotted.file = Some(PathBuf::from("/p/archive.tar.gz"));
        assert_eq!(
            expand_macros(
                "$FileNameWithoutAllExtensions$|$FileNameWithoutExtension$",
                &dotted
            ),
            "archive|archive.tar"
        );
        dotted.file = Some(PathBuf::from("/p/.gitignore"));
        assert_eq!(
            expand_macros("$FileNameWithoutAllExtensions$", &dotted),
            ".gitignore"
        );
        // A file outside the project has no relative path.
        dotted.file = Some(PathBuf::from("/elsewhere/x.py"));
        assert_eq!(expand_macros("[$FileRelativePath$]", &dotted), "[]");
    }

    #[test]
    fn tool_shortcuts_are_parsed_and_checked_against_the_keymap() {
        let tool = |name: &str, shortcut: &str| ExternalTool {
            name: name.into(),
            shortcut: shortcut.into(),
            ..Default::default()
        };
        let km = Keymap::default_keymap();
        let tools = vec![
            tool("Black", "Ctrl+Alt+Shift+B"),
            tool("Ruff", ""),
            tool("Bad", "Ctrl+Nope"),
            tool("", "Ctrl+Alt+Shift+K"),
            tool("Dup", "Ctrl+D"),
            tool("Impl", "Ctrl+Alt+B"),
            tool("Letter", "K"),
            tool("Fn", "F24"),
        ];
        let parsed: Vec<(String, String)> = tool_shortcuts(&tools, &km)
            .into_iter()
            .map(|(c, n)| (c.to_string(), n))
            .collect();
        assert_eq!(
            parsed,
            vec![
                ("Ctrl+Alt+Shift+B".to_string(), "Black".to_string()),
                ("F24".to_string(), "Fn".to_string())
            ]
        );
        assert_eq!(shortcut_problem("", &km), None);
        assert_eq!(shortcut_problem("Ctrl+Alt+Shift+F11", &km), None);
        assert!(shortcut_problem("Ctrl+Nope", &km)
            .unwrap()
            .starts_with("not a shortcut"));
        assert_eq!(
            shortcut_problem("Ctrl+S", &km).as_deref(),
            Some("Ctrl+S is Save in the keymap, which wins")
        );
        // Editor actions too: the keymap wins whatever has the focus.
        assert_eq!(
            shortcut_problem("Ctrl+D", &km).as_deref(),
            Some("Ctrl+D is Duplicate Line or Selection in the keymap, which wins")
        );
        assert!(shortcut_problem("Tab", &km)
            .unwrap()
            .ends_with("in the keymap, which wins"));
        assert_eq!(
            shortcut_problem("Shift+K", &km).as_deref(),
            Some("Shift+K types or edits text: add Ctrl or Alt")
        );
        // The selected keymap decides: a key the user unbound is free again.
        let mut custom = km.clone();
        custom.clear_action(crate::actions::Action::DuplicateLine);
        assert_eq!(shortcut_problem("Ctrl+D", &custom), None);
    }

    /// One frame with the key presses `events`: what [`take_shortcut`] took, and
    /// how many key presses it left for the rest of the frame (the editor). The
    /// keys are released in the next frame, as a keyboard does (egui marks a
    /// press of a key that is still down as a repeat).
    fn frame(
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        tools: &[ExternalTool],
        km: &Keymap,
    ) -> (Option<String>, usize) {
        let releases: Vec<egui::Event> = events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key {
                    key,
                    physical_key,
                    modifiers,
                    repeat: false,
                    ..
                } => Some(egui::Event::Key {
                    key: *key,
                    physical_key: *physical_key,
                    pressed: false,
                    repeat: false,
                    modifiers: *modifiers,
                }),
                _ => None,
            })
            .collect();
        let mut result = (None, 0);
        let mut out = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| {
                let taken = take_shortcut(ui.ctx(), tools, km);
                result = (
                    taken,
                    ui.input(|i| {
                        i.events
                            .iter()
                            .filter(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
                            .count()
                    }),
                );
            },
        );
        out.textures_delta.clear();
        let mut out = ctx.run_ui(
            egui::RawInput {
                events: releases,
                ..Default::default()
            },
            |_| {},
        );
        out.textures_delta.clear();
        result
    }

    #[test]
    fn a_tool_shortcut_press_is_taken_once() {
        let ctx = egui::Context::default();
        let km = Keymap::default_keymap();
        let tools = vec![ExternalTool {
            name: "Black".into(),
            shortcut: "Ctrl+Alt+Shift+B".into(),
            ..Default::default()
        }];
        let press = |mods: egui::Modifiers| egui::Event::Key {
            key: egui::Key::B,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: mods,
        };
        let ctrl_alt = egui::Modifiers {
            alt: true,
            ..egui::Modifiers::COMMAND
        };
        // The exact chord only (Ctrl+Alt+B is Go to Implementation); a taken press is consumed.
        let taken: Vec<(Option<String>, usize)> = [
            egui::Modifiers {
                shift: true,
                ..ctrl_alt
            },
            ctrl_alt,
            egui::Modifiers::COMMAND,
        ]
        .into_iter()
        .map(|m| frame(&ctx, vec![press(m)], &tools, &km))
        .collect();
        assert_eq!(
            taken,
            vec![(Some("Black".to_string()), 0), (None, 1), (None, 1)]
        );
    }

    #[test]
    fn keys_the_keymap_binds_stay_with_the_keymap() {
        // Editor actions are left unconsumed by the global shortcuts so that the
        // editor, which runs after the tools, handles them: a tool on the same
        // key used to take Ctrl+D instead of Duplicate Line.
        let ctx = egui::Context::default();
        let km = Keymap::default_keymap();
        let key = |key: egui::Key, modifiers: egui::Modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let ctrl = egui::Modifiers::COMMAND;
        for (shortcut, k, mods) in [
            ("Ctrl+D", egui::Key::D, ctrl),
            ("Ctrl+Y", egui::Key::Y, ctrl),
            ("Ctrl+Slash", egui::Key::Slash, ctrl),
            ("Ctrl+Z", egui::Key::Z, ctrl),
            ("Tab", egui::Key::Tab, egui::Modifiers::NONE),
        ] {
            let tools = vec![ExternalTool {
                name: "EchoTool".into(),
                shortcut: shortcut.into(),
                ..Default::default()
            }];
            assert_eq!(
                frame(&ctx, vec![key(k, mods)], &tools, &km),
                (None, 1),
                "{shortcut}"
            );
        }
        // A key the keymap does not bind (here: unbound by the user) is the tool's,
        // once per press.
        let mut custom = km.clone();
        custom.clear_action(crate::actions::Action::DuplicateLine);
        let tools = vec![ExternalTool {
            name: "EchoTool".into(),
            shortcut: "Ctrl+D".into(),
            ..Default::default()
        }];
        assert_eq!(
            frame(&ctx, vec![key(egui::Key::D, ctrl)], &tools, &custom),
            (Some("EchoTool".into()), 0)
        );
        // Holding the keys (a press while the key is still down) does not start it again.
        let mut taken = Vec::new();
        for _ in 0..2 {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    events: vec![key(egui::Key::D, ctrl)],
                    ..Default::default()
                },
                |ui| taken.push(take_shortcut(ui.ctx(), &tools, &custom)),
            );
            out.textures_delta.clear();
        }
        assert_eq!(taken, vec![Some("EchoTool".to_string()), None]);
        // A typing key is never a tool's.
        let typing = vec![ExternalTool {
            name: "Typing".into(),
            shortcut: "K".into(),
            ..Default::default()
        }];
        assert_eq!(
            frame(
                &ctx,
                vec![key(egui::Key::K, egui::Modifiers::NONE)],
                &typing,
                &km
            ),
            (None, 1)
        );
        // Find Action shows only shortcuts that work.
        let listed = vec![
            ExternalTool {
                name: "Echo".into(),
                shortcut: "Ctrl+D".into(),
                ..Default::default()
            },
            ExternalTool {
                name: "Black".into(),
                shortcut: "ctrl+alt+shift+b".into(),
                ..Default::default()
            },
        ];
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            publish(ui.ctx(), &listed, &km)
        });
        out.textures_delta.clear();
        let shown: Vec<(String, String)> = published_tools(&ctx)
            .iter()
            .map(|t| (t.name.clone(), t.shortcut.clone()))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("Echo".to_string(), String::new()),
                ("Black".to_string(), "Ctrl+Alt+Shift+B".to_string())
            ]
        );
    }

    #[test]
    fn groups_for_menus() {
        let tool = |name: &str, group: &str| ExternalTool {
            name: name.into(),
            group: group.into(),
            ..Default::default()
        };
        let tools = vec![
            tool("Black", "Formatters"),
            tool("Ruff", "Linters"),
            tool("isort", "Formatters"),
            tool("Echo", ""),
            tool("", "x"),
        ];
        assert_eq!(
            grouped(&tools),
            vec![
                ("".to_string(), vec!["Echo".to_string()]),
                (
                    "Formatters".to_string(),
                    vec!["Black".to_string(), "isort".to_string()]
                ),
                ("Linters".to_string(), vec!["Ruff".to_string()])
            ]
        );
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    #[test]
    fn editor_and_project_placements_are_independent_of_shortcut_publication() {
        let ctx = egui::Context::default();
        let mut tool = ExternalTool {
            name: "Tool".into(),
            shortcut: "Ctrl+Alt+Shift+B".into(),
            ..Default::default()
        };
        tool.options.editor_menu = false;
        publish(&ctx, &[tool.clone()], &Keymap::default_keymap());
        assert!(ctx
            .data(|data| data.get_temp::<Arc<Vec<(String, Vec<String>)>>>(menu_id()))
            .unwrap()
            .is_empty());
        assert_eq!(
            ctx.data(|data| data.get_temp::<Arc<Vec<(String, Vec<String>)>>>(project_menu_id()))
                .unwrap()[0]
                .1,
            vec!["Tool"]
        );
        assert_eq!(published_tools(&ctx).len(), 1);
        tool.options.project_menu = false;
        publish(&ctx, &[tool], &Keymap::default_keymap());
        assert!(ctx
            .data(|data| data.get_temp::<Arc<Vec<(String, Vec<String>)>>>(project_menu_id()))
            .unwrap()
            .is_empty());
    }
}

#[cfg(test)]
mod duplicate_shortcut_tests {
    use super::*;
    #[test]
    fn ambiguous_dynamic_bindings_never_run_the_first_tool_accidentally() {
        let tools = vec![
            ExternalTool {
                name: "First".into(),
                shortcut: "Ctrl+Alt+Shift+B".into(),
                ..Default::default()
            },
            ExternalTool {
                name: "Second".into(),
                shortcut: "Ctrl+Alt+Shift+B".into(),
                ..Default::default()
            },
        ];
        assert!(tool_shortcuts(&tools, &Keymap::default_keymap()).is_empty());
        let ctx = egui::Context::default();
        publish(&ctx, &tools, &Keymap::default_keymap());
        assert!(published_tools(&ctx)
            .iter()
            .all(|tool| tool.shortcut.is_empty()));
    }
}
