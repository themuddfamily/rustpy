//! Debug tool window › Console (roadmap §3.3): the program's output as console
//! lines, and what a line typed at the bottom does: program input while the
//! program runs, an expression evaluated in the current frame while it is
//! suspended.

use egui::FontId;
use rustpy_debug::{DebugSession, DebugState};
use rustpy_run::console_buffer::ConsoleBuffer;
use rustpy_run::OutputChunk;

use crate::console_view::{row_job, RowStyle, VisualRow};
use crate::theme::UiTheme;

/// What Enter does with the line typed in the debug console.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ConsoleInput {
    /// The program runs: the line goes to its standard input (`input()`).
    Stdin,
    /// The program is suspended: the line is evaluated in the current frame.
    Evaluate,
    /// The program has ended.
    Closed,
}

impl ConsoleInput {
    pub fn for_state(state: &DebugState) -> ConsoleInput {
        match state {
            DebugState::Stopped { .. } => ConsoleInput::Evaluate,
            DebugState::Starting | DebugState::Running => ConsoleInput::Stdin,
            DebugState::Terminated(_) | DebugState::Failed(_) => ConsoleInput::Closed,
        }
    }

    /// The prompt before the input line.
    pub fn prompt(self) -> &'static str {
        match self {
            ConsoleInput::Evaluate => ">>>",
            ConsoleInput::Stdin | ConsoleInput::Closed => "›",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ConsoleInput::Stdin => "Type input and press Enter to send it to the program",
            ConsoleInput::Evaluate => "Evaluate expression in the current frame",
            ConsoleInput::Closed => "The program is not running",
        }
    }
}

/// Sends a typed line as [`ConsoleInput::for_state`] says; false when it was
/// not used (an empty line is program input, but nothing to evaluate).
pub fn submit(session: &mut DebugSession, line: &str) -> bool {
    match ConsoleInput::for_state(&session.state) {
        ConsoleInput::Stdin => {
            session.write_stdin(&format!("{line}\n"));
            true
        }
        ConsoleInput::Evaluate if !line.trim().is_empty() => {
            session.evaluate_repl(line.trim());
            true
        }
        _ => false,
    }
}

/// The output as console lines: the pieces of a line written separately (a
/// prompt, then the answer typed after it) make one line, as in the Run console.
pub fn console_lines(output: &[OutputChunk]) -> ConsoleBuffer {
    let mut buf = ConsoleBuffer::default();
    for chunk in output {
        buf.push(chunk.kind, &chunk.text);
    }
    buf
}

/// Draws the output lines in the Run console's colors.
pub fn show_output(ui: &mut egui::Ui, theme: &UiTheme, mono: &FontId, output: &[OutputChunk]) {
    let buf = console_lines(output);
    let style = RowStyle::new(theme, mono.clone());
    for (n, line) in buf.lines_from(buf.first_line()) {
        ui.label(row_job(
            line,
            VisualRow {
                line: n,
                start: 0,
                end: line.char_len(),
            },
            &[],
            &[],
            None,
            &style,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_run::OutputKind::{Stderr, Stdout, System};

    fn chunk(kind: rustpy_run::OutputKind, text: &str) -> OutputChunk {
        OutputChunk {
            kind,
            text: text.into(),
        }
    }

    #[test]
    fn typed_lines_are_program_input_while_it_runs() {
        assert_eq!(
            ConsoleInput::for_state(&DebugState::Running),
            ConsoleInput::Stdin
        );
        assert_eq!(
            ConsoleInput::for_state(&DebugState::Starting),
            ConsoleInput::Stdin
        );
        let stopped = DebugState::Stopped {
            thread_id: 1,
            reason: "breakpoint".into(),
            description: String::new(),
        };
        assert_eq!(ConsoleInput::for_state(&stopped), ConsoleInput::Evaluate);
        assert_eq!(
            ConsoleInput::for_state(&DebugState::Terminated(Some(0))),
            ConsoleInput::Closed
        );
        assert_eq!(
            ConsoleInput::for_state(&DebugState::Failed("x".into())),
            ConsoleInput::Closed
        );
        assert_eq!(
            (
                ConsoleInput::Stdin.prompt(),
                ConsoleInput::Evaluate.prompt()
            ),
            ("›", ">>>")
        );
    }

    #[test]
    fn a_prompt_and_the_typed_answer_share_a_line() {
        let output = [
            chunk(System, "python3 -m debugpy … ask.py\n"),
            chunk(Stdout, "Enter your name: "),
            chunk(System, "Bob\n"),
            chunk(Stdout, "Hello, "),
            chunk(Stderr, "warning\n"),
            chunk(Stdout, "Bob!\n"),
            chunk(System, "\nProcess finished with exit code 0\n"),
        ];
        let buf = console_lines(&output);
        let lines: Vec<&str> = buf
            .lines_from(buf.first_line())
            .map(|(_, l)| l.text.as_str())
            .collect();
        assert_eq!(
            lines,
            vec![
                "python3 -m debugpy … ask.py",
                "Enter your name: Bob",
                "Hello, Bob!",
                "warning",
                "",
                "Process finished with exit code 0"
            ]
        );
        // The answer keeps its own (input) color.
        let answer = buf.lines_from(buf.first_line()).nth(1).unwrap().1;
        assert_eq!(
            answer
                .spans()
                .map(|(r, _, kind)| (r, kind))
                .collect::<Vec<_>>(),
            vec![(0..17, Stdout), (17..20, System)]
        );
    }
}
