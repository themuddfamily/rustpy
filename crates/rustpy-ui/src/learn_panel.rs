//! Help › Learn IDE Features: interactive lessons.
//! Each lesson opens a practice file and advances when the user performs the
//! taught action or the document reaches the expected state.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::actions::{Action, Keymap};
use crate::theme::UiTheme;

#[derive(Clone, Copy, Debug)]
pub enum Expect {
    Did(Action),
    DidAny(&'static [Action]),
    Has(&'static str),
    Lacks(&'static str),
}

pub struct Step {
    /// Instruction; `{ActionId}` placeholders become the current shortcut.
    pub text: &'static str,
    pub expect: Expect,
}

pub struct Lesson {
    pub id: &'static str,
    pub module: &'static str,
    pub title: &'static str,
    pub code: &'static str,
    /// 0-based caret line/column when the lesson starts.
    pub caret: (u32, u32),
    pub steps: Vec<Step>,
}

macro_rules! step {
    ($t:expr, $e:expr) => {
        Step {
            text: $t,
            expect: $e,
        }
    };
}

pub fn lessons() -> Vec<Lesson> {
    use Action as A;
    use Expect::*;
    vec![
        Lesson { id: "essentials-discover-save", module: "Essentials", title: "Discover Commands and Save", code: "# A local practice file for finding commands and saving.\nmessage = 'Welcome to RustPy'\n", caret: (1, 0), steps: vec![step!("Press {GotoAction} to find any IDE command by name. Close the popup after exploring it.", Did(A::FindAction)), step!("Press {SaveDocument} to save this practice file. Use Help → Getting Started for project and interpreter setup.", Did(A::Save))] },
        Lesson { id: "context-actions", module: "Editor Basics", title: "Context Actions", code: "import os\nimport sys\n\n\ndef greet(name):\n    print(\"Hello, \" + name, file=sys.stderr)\n\n\ngreet(\"World\")\n", caret: (0, 7), steps: vec![step!("The import of 'os' is unused. Press {ShowIntentionActions} to see what the IDE offers here.", Did(A::ShowIntentionActions)), step!("Pick the fix that removes the unused import and press Enter.", Lacks("import os"))] },
        Lesson { id: "find-action", module: "Editor Basics", title: "Search for Actions", code: "def total(xs):\n  return   sum( xs )\n", caret: (1, 0), steps: vec![step!("Every command can be found by name. Press {GotoAction}.", Did(A::FindAction)), step!("Type 'reformat', select Reformat Code and press Enter.", Did(A::ReformatCode))] },
        Lesson { id: "duplicate-delete", module: "Editor Basics", title: "Duplicate and Delete Lines", code: "colors = [\n    \"red\",\n    \"green\",\n]\n", caret: (1, 4), steps: vec![step!("Press {EditorDuplicate} to duplicate the current line.", Did(A::DuplicateLine)), step!("Press {EditorDeleteLine} to delete the line again.", Did(A::DeleteLine))] },
        Lesson { id: "move-code", module: "Editor Basics", title: "Move Code Fragments", code: "def first():\n    return 1\n\n\ndef second():\n    return 2\n\n\nprint(first(), second())\n", caret: (8, 0), steps: vec![step!("Press {MoveLineUp} to move the line up.", Did(A::MoveLineUp)), step!("Press {MoveLineDown} to move it back down.", Did(A::MoveLineDown)), step!("Put the caret on 'def second' and press {MoveStatementUp} to move the whole function above 'first'.", Did(A::MoveStatementUp))] },
        Lesson { id: "comment", module: "Editor Basics", title: "Comment Lines", code: "debug = True\nverbose = False\nprint(debug, verbose)\n", caret: (1, 0), steps: vec![step!("Press {CommentByLineComment} to comment out the line.", Has("# verbose = False")), step!("Press {CommentByLineComment} again to uncomment it.", Lacks("# verbose"))] },
        Lesson { id: "selection", module: "Editor Basics", title: "Expand and Shrink Selection", code: "result = compute(alpha, beta * (gamma + delta))\n", caret: (0, 33), steps: vec![step!("Press {EditorSelectWord} to select the word at the caret.", Did(A::ExtendSelection)), step!("Press {EditorSelectWord} again to grow the selection to the enclosing expression.", Did(A::ExtendSelection)), step!("Press {EditorUnSelectWord} to shrink it back.", Did(A::ShrinkSelection))] },
        Lesson { id: "multi-caret", module: "Editor Basics", title: "Multiple Selections", code: "value = 10\nvalue = value * 2\nprint(value)\n", caret: (0, 0), steps: vec![step!("Press {SelectNextOccurrence} to select 'value'.", Did(A::AddSelectionForNextOccurrence)), step!("Press {SelectAllOccurrences} to select every occurrence.", Did(A::SelectAllOccurrences)), step!("Type 'amount' to rename all of them at once.", Lacks("value"))] },
        Lesson { id: "folding", module: "Editor Basics", title: "Collapse and Expand", code: "class Shape:\n    def area(self):\n        return 0\n\n    def perimeter(self):\n        return 0\n", caret: (1, 8), steps: vec![step!("Press {CollapseRegionAlt} to collapse the method.", Did(A::CollapseRegion)), step!("Press {ExpandRegion} to expand it.", Did(A::ExpandRegion)), step!("Press {CollapseAllRegions} to collapse everything.", Did(A::CollapseAll)), step!("Press {ExpandAllRegions} to expand everything.", Did(A::ExpandAll))] },
        Lesson { id: "basic-completion", module: "Code Completion", title: "Basic Completion", code: "import math\n\nroot = math.sq\n", caret: (2, 14), steps: vec![step!("Press {CodeCompletion} to list the members of 'math' starting with 'sq'.", Did(A::CodeCompletion)), step!("Choose 'sqrt' and press Enter.", Has("math.sqrt"))] },
        Lesson { id: "postfix", module: "Code Completion", title: "Postfix Completion", code: "count = 5\ncount > 3\n", caret: (1, 9), steps: vec![step!("Type '.if' after the expression and press Tab: the expression becomes an if statement.", Has("if count > 3:"))] },
        Lesson { id: "rename", module: "Refactorings", title: "Rename", code: "def calc(a, b):\n    return a * b\n\n\nprint(calc(2, 3), calc(4, 5))\n", caret: (0, 5), steps: vec![step!("Press {RenameElement} to rename 'calc' everywhere.", Did(A::Rename)), step!("Type a new name such as 'multiply' and press Enter.", Lacks("calc"))] },
        Lesson { id: "extract-variable", module: "Refactorings", title: "Extract Variable", code: "radius = 2.0\nprint(2 * 3.14159 * radius)\n", caret: (1, 6), steps: vec![step!("Press {IntroduceVariable} to extract the expression into a variable.", Did(A::ExtractVariable)), step!("Choose the expression and confirm the name.", Lacks("print(2 * 3.14159 * radius)"))] },
        Lesson { id: "reformat", module: "Refactorings", title: "Reformat Code", code: "def  messy( a,b ):\n  x=a+b\n  return  x\n", caret: (0, 0), steps: vec![step!("Press {ReformatCode} to apply the code style.", Has("def messy(a, b):"))] },
        Lesson { id: "search-everywhere", module: "Navigation", title: "Search Everywhere", code: "# Press Shift twice to search files, classes, symbols and actions.\n", caret: (0, 0), steps: vec![step!("Press Shift twice ({SearchEverywhere}).", Did(A::SearchEverywhere))] },
        Lesson { id: "file-structure", module: "Navigation", title: "File Structure", code: "class Cart:\n    def add(self, item):\n        pass\n\n    def remove(self, item):\n        pass\n\n    def total(self):\n        return 0\n", caret: (0, 0), steps: vec![step!("Press {FileStructurePopup} to see the classes and methods of this file.", Did(A::FileStructure))] },
        Lesson { id: "declaration-usages", module: "Navigation", title: "Declaration and Usages", code: "def area(w, h):\n    return w * h\n\n\nprint(area(2, 3))\nprint(area(4, 5))\n", caret: (4, 7), steps: vec![step!("Press {GotoDeclaration} to jump to the definition of 'area'.", Did(A::GoToDeclaration)), step!("Press {FindUsages} to list all the places that call it.", Did(A::FindUsages))] },
        Lesson { id: "recent-files", module: "Navigation", title: "Recent Files", code: "# Jump between recently opened files.\n", caret: (0, 0), steps: vec![step!("Press {RecentFiles} to open the recent files popup.", Did(A::RecentFiles))] },
        Lesson { id: "run", module: "Run and Debug", title: "Run a Script", code: "print(\"Hello from the run lesson\")\n", caret: (0, 0), steps: vec![step!("Press {RunClass} to run this file (or {Run} to run the selected configuration).", DidAny(&[A::RunContext, A::Run]))] },
        Lesson { id: "debug", module: "Run and Debug", title: "Breakpoints and Debugging", code: "total = 0\nfor i in range(3):\n    total += i\nprint(total)\n", caret: (2, 4), steps: vec![step!("Press {ToggleLineBreakpoint} to put a breakpoint on this line.", Did(A::ToggleLineBreakpoint)), step!("Press {Debug} to start debugging; execution stops at the breakpoint.", Did(A::Debug))] },
    ]
}

pub struct ActiveLesson {
    pub index: usize,
    pub step: usize,
    pub path: PathBuf,
}

#[derive(Default)]
pub struct LearnPanel {
    pub visible: bool,
    pub active: Option<ActiveLesson>,
    pub completed: HashSet<String>,
    lessons_cache: Option<Vec<Lesson>>,
}

/// What the panel asks the app to do.
pub enum LearnEvent {
    Start(usize),
    Close,
}

pub fn render_text(text: &str, keymap: &Keymap) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let id = &rest[open + 1..open + close];
        match Keymap::action_by_id(id) {
            Some(a) => {
                let s = keymap.shortcut_text(a);
                out.push_str(if s.is_empty() { a.text() } else { &s });
            }
            None => out.push_str(&rest[open..=open + close]),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

impl LearnPanel {
    pub fn lessons(&mut self) -> &Vec<Lesson> {
        self.lessons_cache.get_or_insert_with(lessons)
    }

    fn matches(expect: Expect, action: Option<Action>, text: Option<&str>) -> bool {
        match (expect, action, text) {
            (Expect::Did(a), Some(b), _) => a == b,
            (Expect::DidAny(list), Some(b), _) => list.contains(&b),
            (Expect::Has(s), _, Some(t)) => t.contains(s),
            (Expect::Lacks(s), _, Some(t)) => !t.contains(s),
            _ => false,
        }
    }

    /// Advances the active lesson; returns true when it just completed.
    fn advance(&mut self, action: Option<Action>, text: Option<&str>) -> bool {
        let Some(active) = &self.active else {
            return false;
        };
        let (index, step) = (active.index, active.step);
        let lessons = self.lessons_cache.get_or_insert_with(lessons);
        let Some(lesson) = lessons.get(index) else {
            return false;
        };
        let Some(s) = lesson.steps.get(step) else {
            return false;
        };
        if !Self::matches(s.expect, action, text) {
            return false;
        }
        let total = lesson.steps.len();
        let id = lesson.id.to_string();
        let active = self.active.as_mut().unwrap();
        active.step += 1;
        if active.step >= total {
            self.completed.insert(id);
            return true;
        }
        false
    }

    pub fn observe_action(&mut self, a: Action) -> bool {
        self.advance(Some(a), None)
    }

    pub fn observe_text(&mut self, text: &str) -> bool {
        self.advance(None, Some(text))
    }

    pub fn is_lesson_file(&self, path: &std::path::Path) -> bool {
        self.active.as_ref().is_some_and(|a| a.path == path)
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        keymap: &Keymap,
    ) -> Option<LearnEvent> {
        let mut event = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Learn").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("Hide").clicked() {
                    event = Some(LearnEvent::Close);
                }
            });
        });
        ui.separator();
        let completed = self.completed.clone();
        let total = self.lessons().len();
        if let Some(active) = &self.active {
            let (index, step_idx) = (active.index, active.step);
            let lessons = self.lessons_cache.get_or_insert_with(lessons);
            let lesson = &lessons[index];
            ui.label(
                egui::RichText::new(lesson.module)
                    .color(theme.text_dim)
                    .small(),
            );
            ui.label(egui::RichText::new(lesson.title).heading());
            ui.add_space(6.0);
            for (i, s) in lesson.steps.iter().enumerate() {
                let text = render_text(s.text, keymap);
                let (prefix, color) = if i < step_idx {
                    ("done", theme.success)
                } else if i == step_idx {
                    ("now", theme.accent)
                } else {
                    ("", theme.text_dim)
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(format!("{}.", i + 1)).color(color));
                    ui.label(egui::RichText::new(text).color(if i == step_idx {
                        theme.text
                    } else {
                        theme.text_dim
                    }));
                    if !prefix.is_empty() && i < step_idx {
                        ui.label(egui::RichText::new(prefix).color(color).small());
                    }
                });
                ui.add_space(4.0);
            }
            let done = step_idx >= lesson.steps.len();
            if done {
                ui.colored_label(theme.success, "Lesson complete!");
            }
            ui.horizontal(|ui| {
                if ui.button("All lessons").clicked() {
                    self.active = None;
                }
                if ui.button("Restart").clicked() {
                    event = Some(LearnEvent::Start(index));
                }
                if done && index + 1 < total && ui.button("Next lesson").clicked() {
                    event = Some(LearnEvent::Start(index + 1));
                }
            });
            return event;
        }
        ui.label(
            egui::RichText::new(format!(
                "{} of {} lessons completed",
                completed.len().min(total),
                total
            ))
            .color(theme.text_dim),
        );
        egui::ScrollArea::vertical()
            .id_salt("learn_list")
            .show(ui, |ui| {
                let lessons = self.lessons_cache.get_or_insert_with(lessons);
                let mut module = "";
                for (i, l) in lessons.iter().enumerate() {
                    if l.module != module {
                        module = l.module;
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(module).strong());
                    }
                    ui.horizontal(|ui| {
                        if ui.link(l.title).clicked() {
                            event = Some(LearnEvent::Start(i));
                        }
                        if completed.contains(l.id) {
                            ui.label(egui::RichText::new("done").color(theme.success).small());
                        }
                    });
                }
            });
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lessons_are_well_formed() {
        let ls = lessons();
        let mut ids = HashSet::new();
        for l in &ls {
            assert!(ids.insert(l.id), "duplicate lesson id {}", l.id);
            assert!(!l.steps.is_empty());
            let lines: Vec<&str> = l.code.lines().collect();
            assert!(
                (l.caret.0 as usize) < lines.len().max(1),
                "{}: caret line out of range",
                l.id
            );
            for s in &l.steps {
                // Every placeholder must name a real action id.
                let mut rest = s.text;
                while let Some(o) = rest.find('{') {
                    let c = rest[o..].find('}').expect("closing brace");
                    assert!(
                        Keymap::action_by_id(&rest[o + 1..o + c]).is_some(),
                        "{}: unknown action id {}",
                        l.id,
                        &rest[o + 1..o + c]
                    );
                    rest = &rest[o + c + 1..];
                }
            }
        }
    }

    #[test]
    fn steps_advance_and_complete() {
        let mut p = LearnPanel::default();
        let idx = p
            .lessons()
            .iter()
            .position(|l| l.id == "duplicate-delete")
            .unwrap();
        p.active = Some(ActiveLesson {
            index: idx,
            step: 0,
            path: PathBuf::from("/tmp/x.py"),
        });
        assert!(
            !p.observe_action(Action::DeleteLine),
            "out-of-order action is ignored"
        );
        assert!(!p.observe_action(Action::DuplicateLine));
        assert!(p.observe_action(Action::DeleteLine));
        assert!(p.completed.contains("duplicate-delete"));
    }

    #[test]
    fn text_expectations_and_placeholders() {
        let mut p = LearnPanel::default();
        let idx = p.lessons().iter().position(|l| l.id == "comment").unwrap();
        p.active = Some(ActiveLesson {
            index: idx,
            step: 0,
            path: PathBuf::new(),
        });
        assert!(!p.observe_text("debug = True\nverbose = False\n"));
        assert!(!p.observe_text("debug = True\n# verbose = False\n"));
        assert!(p.observe_text("debug = True\nverbose = False\n"));
        let km = Keymap::default_keymap();
        assert_eq!(
            render_text("Press {EditorDuplicate} now", &km),
            "Press Ctrl+D now"
        );
    }
}
