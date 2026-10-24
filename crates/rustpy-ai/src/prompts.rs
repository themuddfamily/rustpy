//! Request builders for the AI Assistant actions.
//!
//! Each builder is a pure function returning a [`ChatRequest`] with a focused
//! system prompt and one user turn ([`chat`] also carries the conversation).
//! Inputs longer than [`MAX_INPUT_CHARS`] are shortened in the middle by
//! [`truncate_middle`], which leaves a visible marker so the model knows text
//! is missing. The UI can compare input lengths with [`MAX_INPUT_CHARS`] to tell
//! the user that a selection was too large to send whole.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::types::{normalize_messages, ChatRequest, Message, Role};

/// Longest input (in characters) sent as-is; larger inputs keep their head and tail.
pub const MAX_INPUT_CHARS: usize = 60_000;
/// Error text / traceback budget for [`fix_error`].
const MAX_ERROR_CHARS: usize = 20_000;
/// Budget for earlier turns in [`chat`]; older turns are dropped first.
const MAX_HISTORY_CHARS: usize = 200_000;
/// Recent commit messages shown to [`commit_message`] as style examples.
const MAX_RECENT_COMMITS: usize = 10;
const MAX_COMMIT_CHARS: usize = 1_000;
/// Room kept for the truncation marker so results stay within the limit.
const MARKER_RESERVE: usize = 80;

const ROLE: &str = "You are the AI Assistant built into RustPy, a Python IDE.";
const MARKDOWN: &str = "Write in GitHub-flavored Markdown. Put code in fenced code blocks tagged with their language (for example ```python): the IDE offers to insert a block at the caret or to replace the selection with it, so make each block complete and ready to paste. Be direct and concise.";
const TRUNCATION: &str = "Very large inputs are shortened in the middle, and a line like `[… 1234 characters omitted …]` marks the cut; if the missing part matters for your answer, say so.";

/// Editor context attached to a chat message.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnippet {
    /// Path or name of the file open in the editor (may be empty).
    pub file: String,
    /// The selected code (may be empty, then only the file is mentioned).
    pub selection: String,
    /// Language of the file, e.g. `"python"` (may be empty).
    pub language: String,
}

/// "Explain code": explanation of a selection in `file_name`, written in `language`
/// (either may be empty).
pub fn explain_code(code: &str, language: &str, file_name: &str) -> ChatRequest {
    let system = format!(
        "{ROLE} The developer selected code in the editor and wants to understand it.\n\n\
         Explain what the code does and how it does it. Start with a one- or two-sentence summary, then walk through the important parts in the order a reader needs them. Point out non-obvious behavior, edge cases and likely bugs if you see any. Refer to identifiers in backticks, and quote only the lines you discuss rather than repeating the whole code.\n\n\
         {MARKDOWN}\n\n{TRUNCATION}"
    );
    let language = language.trim();
    let what = if language.is_empty() {
        "code".to_string()
    } else {
        format!("{language} code")
    };
    let from = if file_name.trim().is_empty() {
        String::new()
    } else {
        format!(" from `{}`", file_name.trim())
    };
    let code = truncate_middle(code, MAX_INPUT_CHARS);
    single_turn(
        system,
        format!(
            "Explain this {what}{from}:\n\n{}",
            fenced(&code, &fence_lang(language))
        ),
    )
}

/// "Generate docstring": a request whose answer is only the docstring (with its
/// triple quotes, unindented) for `function_source`.
///
/// `style` is `"google"`, `"numpy"` or `"rest"`; `"epytext"`, `"plain"` and
/// `"auto"` (keep the existing docstring's style) are accepted too, as are the
/// aliases `"numpydoc"`, `"rst"`, `"restructuredtext"` and `"sphinx"`. Unknown
/// values mean Google style.
pub fn generate_docstring(function_source: &str, style: &str) -> ChatRequest {
    let style = docstring_style(style);
    let system = format!(
        "{ROLE} Write a docstring for the Python function, method or class the developer provides, in {style}.\n\n\
         Base it only on what the code shows; don't invent behavior, parameters or exceptions. Start with a one-line summary that ends with a period, add a short description only when the summary isn't enough, then document the parameters, the return value (or yielded values) and the exceptions the code raises explicitly. Use the type annotations when there are any, and leave out `self` and `cls`.\n\n\
         Reply with only the docstring: start with the opening triple quotes and end with the closing triple quotes, with no code fence, no signature or body, and no commentary. Don't indent it; the IDE indents it to match the code.\n\n\
         {TRUNCATION}"
    );
    let source = truncate_middle(function_source, MAX_INPUT_CHARS);
    single_turn(
        system,
        format!(
            "Write a docstring for this code:\n\n{}",
            fenced(&source, "python")
        ),
    )
}

/// "Suggest refactoring": prioritized suggestions plus the refactored code in one block.
pub fn suggest_refactoring(code: &str) -> ChatRequest {
    let system = format!(
        "{ROLE} The developer wants refactoring suggestions for the code they selected.\n\n\
         Look for changes that make the code clearer and easier to maintain: better names, smaller functions, simpler control flow, less duplication, idiomatic Python, sturdier error handling, and performance fixes where they matter. Keep the behavior the same, and label any suggestion that would change it. If the code is already in good shape, say so briefly instead of inventing changes.\n\n\
         Start with a short list of suggestions, most valuable first, each with a one-line reason. Then give the complete refactored code in a single fenced code block that can replace the selection.\n\n\
         {MARKDOWN}\n\n{TRUNCATION}"
    );
    let code = truncate_middle(code, MAX_INPUT_CHARS);
    single_turn(
        system,
        format!(
            "Suggest refactorings for this code:\n\n{}",
            fenced(&code, "")
        ),
    )
}

/// "Write tests": a complete test module for `code`. `framework` is `"pytest"`
/// or `"unittest"`; unknown values mean pytest.
pub fn write_tests(code: &str, framework: &str) -> ChatRequest {
    let (name, conventions) = match framework.trim().to_ascii_lowercase().as_str() {
        "unittest" => (
            "unittest",
            "the standard library `unittest` module: `unittest.TestCase` subclasses with `self.assert*` methods, `setUp` for shared setup, `subTest` for table-driven cases, `unittest.mock` for test doubles, and an `if __name__ == \"__main__\": unittest.main()` guard",
        ),
        _ => (
            "pytest",
            "pytest: plain `assert` statements, fixtures for shared setup, `pytest.mark.parametrize` where it removes repetition, `pytest.raises` for expected exceptions, and `unittest.mock` or `monkeypatch` for test doubles",
        ),
    };
    let system = format!(
        "{ROLE} Write unit tests for the code the developer provides, using {conventions}.\n\n\
         Cover the main behavior, the edge cases and the error handling. Use descriptive test names, and keep the tests independent and deterministic: replace files, network access, time and randomness with test doubles. Test behavior rather than private details. Import the code under test from its module; if the module name isn't known, use a placeholder import with a comment saying to adjust it.\n\n\
         Reply with the complete test module in a single ```python code block, followed by at most three short bullet points about assumptions or anything left untested.\n\n\
         {TRUNCATION}"
    );
    let code = truncate_middle(code, MAX_INPUT_CHARS);
    single_turn(
        system,
        format!(
            "Write {name} tests for this code:\n\n{}",
            fenced(&code, "python")
        ),
    )
}

/// "Generate commit message": a summary line of at most 72 characters
/// (Conventional Commits unless `recent_messages` show another convention), a
/// blank line and a body. `recent_messages` are the repository's latest commit
/// messages, newest first; up to ten are shown to the model as style examples.
/// The reply is meant to be used verbatim (see [`strip_code_fence`](crate::strip_code_fence)).
pub fn commit_message(diff_text: &str, recent_messages: &[String]) -> ChatRequest {
    let system = format!(
        "{ROLE} Write a Git commit message for the changes in the diff the developer provides.\n\n\
         Format:\n\
         - A summary line of at most 72 characters in Conventional Commits form, `type(scope): description`, where the type is one of feat, fix, docs, style, refactor, perf, test, build, ci, chore or revert and the scope is optional. Use the imperative mood and no trailing period.\n\
         - A blank line.\n\
         - A body wrapped at 72 characters that explains what changed and why, using `-` bullets when there are several separate changes. Leave the body out only for trivial changes.\n\n\
         When recent commit messages from the repository are provided, reuse their scopes, wording and level of detail; if they clearly follow a convention other than Conventional Commits, follow theirs instead, still within 72 characters for the summary line. Describe the change the way a reviewer wants to read it rather than listing every file.\n\n\
         Reply with only the commit message: no code fence, no quotes, no commentary.\n\n\
         {TRUNCATION}"
    );
    let mut user = String::new();
    let recent: Vec<Cow<'_, str>> = recent_messages
        .iter()
        .map(|m| m.trim())
        .filter(|m| !m.is_empty())
        .take(MAX_RECENT_COMMITS)
        .map(|m| truncate_middle(m, MAX_COMMIT_CHARS))
        .collect();
    if !recent.is_empty() {
        user.push_str(
            "Recent commit messages in this repository, newest first:\n<recent_commits>\n",
        );
        user.push_str(&recent.join("\n---\n"));
        user.push_str("\n</recent_commits>\n\n");
    }
    let diff = truncate_middle(diff_text.trim_end(), MAX_INPUT_CHARS);
    user.push_str("<diff>\n");
    user.push_str(&diff);
    user.push_str("\n</diff>\n\nWrite the commit message for this diff.");
    single_turn(system, user)
}

/// "Fix error": root cause of `error_text` (message or traceback) in `code`, and
/// the corrected code in one block.
pub fn fix_error(code: &str, error_text: &str) -> ChatRequest {
    let system = format!(
        "{ROLE} The developer's code fails with the error shown. Find the root cause from the error message or traceback and the code.\n\n\
         Explain the cause in two to four sentences, then give the corrected code in a single fenced code block that can replace the original, making the smallest change that fixes the problem. Mention other problems you notice separately and briefly. If the code shown can't explain the error (for example a missing package, the wrong interpreter or bad input data), say what to check instead.\n\n\
         {MARKDOWN}\n\n{TRUNCATION}"
    );
    let code = truncate_middle(code, MAX_INPUT_CHARS);
    let error = truncate_middle(error_text.trim_end(), MAX_ERROR_CHARS);
    single_turn(
        system,
        format!(
            "This code:\n\n{}\n\nfails with this error:\n\n{}\n\nWhat is wrong, and how do I fix it?",
            fenced(&code, ""),
            fenced(&error, "text")
        ),
    )
}

/// Free-form chat. `history` holds the earlier turns as plain text (oldest
/// first); `context` (the open file and selection) is attached to the new user
/// message only. Older turns beyond a budget are dropped, and the conversation
/// is normalized so it starts with a user turn and roles alternate.
pub fn chat(
    history: &[Message],
    user_message: &str,
    context: Option<ContextSnippet>,
) -> ChatRequest {
    let system = format!(
        "{ROLE} You are chatting with a developer about their code.\n\n\
         When a message includes an <editor_context> block, it shows the file the developer has open and the code they selected. Use it to ground your answer, and ignore it when the question is about something else. When you suggest code changes, show complete functions or blocks in fenced code blocks so they can be inserted directly, and say where they go. If the answer depends on details of their project you can't see, say what you're assuming or ask.\n\n\
         {MARKDOWN}\n\n{TRUNCATION}"
    );
    let mut messages = trim_history(history, MAX_HISTORY_CHARS);
    let mut user = String::new();
    if let Some(ctx) = context
        .as_ref()
        .filter(|c| !(c.file.trim().is_empty() && c.selection.trim().is_empty()))
    {
        user.push_str("<editor_context>\n");
        if !ctx.file.trim().is_empty() {
            user.push_str(&format!("File: `{}`\n", ctx.file.trim()));
        }
        if !ctx.language.trim().is_empty() {
            user.push_str(&format!("Language: {}\n", ctx.language.trim()));
        }
        if !ctx.selection.trim().is_empty() {
            let selection = truncate_middle(&ctx.selection, MAX_INPUT_CHARS);
            user.push_str("Selected code:\n");
            user.push_str(&fenced(&selection, &fence_lang(&ctx.language)));
            user.push('\n');
        }
        user.push_str("</editor_context>\n\n");
    }
    user.push_str(&truncate_middle(user_message, MAX_INPUT_CHARS));
    messages.push(Message::user(user));
    ChatRequest {
        system,
        messages: normalize_messages(&messages),
    }
}

/// Shortens `text` to at most `max_chars` characters (for limits of a few
/// hundred characters or more) by keeping its head and tail and replacing the
/// middle with a `[… N characters omitted …]` line. Cuts land on line
/// boundaries when one is close. Returns the text unchanged when it fits.
pub fn truncate_middle(text: &str, max_chars: usize) -> Cow<'_, str> {
    let total = text.chars().count();
    if total <= max_chars {
        return Cow::Borrowed(text);
    }
    let budget = max_chars.saturating_sub(MARKER_RESERVE);
    let head_chars = budget / 2;
    let tail_chars = budget - head_chars;
    let mut head_end = byte_index(text, head_chars);
    let mut tail_start = byte_index(text, total - tail_chars);
    if let Some(nl) = text[..head_end].rfind('\n') {
        if head_end - (nl + 1) <= head_end / 4 {
            head_end = nl + 1;
        }
    }
    let tail_len = text.len() - tail_start;
    if let Some(nl) = text[tail_start..].find('\n') {
        if nl < tail_len / 4 {
            tail_start += nl + 1;
        }
    }
    let (head, tail) = (&text[..head_end], &text[tail_start..]);
    let omitted = text[head_end..tail_start].chars().count();
    let mut out = String::with_capacity(head.len() + tail.len() + MARKER_RESERVE);
    out.push_str(head);
    if !head.is_empty() && !head.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("[… {omitted} characters omitted …]\n"));
    out.push_str(tail);
    Cow::Owned(out)
}

/// Byte offset of the `n`-th character (the length when `n` is past the end).
fn byte_index(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(i, _)| i)
}

fn single_turn(system: String, user: String) -> ChatRequest {
    ChatRequest {
        system,
        messages: vec![Message::user(user)],
    }
}

/// `code` in a fenced block whose fence is longer than any backtick run inside it.
fn fenced(code: &str, lang: &str) -> String {
    let (mut longest, mut run) = (0, 0);
    for c in code.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!(
        "{fence}{lang}\n{}\n{fence}",
        code.trim_end_matches(['\n', '\r'])
    )
}

/// Fence info string for a language name: its first word, lowercased, restricted
/// to characters that are safe in an info string.
fn fence_lang(language: &str) -> String {
    let first = language.split_whitespace().next().unwrap_or("");
    first
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-' | '_' | '.'))
        .collect::<String>()
        .to_ascii_lowercase()
}

fn docstring_style(style: &str) -> String {
    const GOOGLE: &str = "Google style: a summary line, an optional extended description, then `Args:`, `Returns:` (or `Yields:`) and `Raises:` sections whose entries look like `name (type): description`, indented under the section heading";
    match style.trim().to_ascii_lowercase().as_str() {
        "numpy" | "numpydoc" => "NumPy style (numpydoc): a summary line, an optional extended description, then `Parameters`, `Returns` (or `Yields`) and `Raises` sections, each heading underlined with dashes, whose entries look like `name : type` followed by an indented description".to_string(),
        "rest" | "rst" | "restructuredtext" | "sphinx" => "reStructuredText (Sphinx) style: a summary line, an optional extended description, then fields such as `:param name: description`, `:type name: type`, `:returns: description`, `:rtype: type` and `:raises ErrorType: description`".to_string(),
        "epytext" => "Epytext style: a summary line, an optional extended description, then fields such as `@param name: description`, `@type name: type`, `@return: description`, `@rtype: type` and `@raise ErrorType: description`".to_string(),
        "plain" => "plain style: a summary line and, if needed, a short description paragraph, without structured sections".to_string(),
        "auto" | "" => format!("the style of the docstring the code already has, if it has one; otherwise {GOOGLE}"),
        _ => GOOGLE.to_string(),
    }
}

/// The most recent turns of `history` that fit in `budget` characters (each
/// turn shortened to [`MAX_INPUT_CHARS`]), starting with a user turn.
fn trim_history(history: &[Message], budget: usize) -> Vec<Message> {
    let mut kept = Vec::new();
    let mut used = 0;
    for m in history.iter().rev() {
        let content = truncate_middle(&m.content, MAX_INPUT_CHARS);
        let len = content.chars().count();
        if used + len > budget {
            break;
        }
        used += len;
        kept.push(Message {
            role: m.role,
            content: content.into_owned(),
        });
    }
    kept.reverse();
    let first_user = kept
        .iter()
        .position(|m| m.role == Role::User)
        .unwrap_or(kept.len());
    kept.split_off(first_user)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_text(req: &ChatRequest) -> &str {
        assert_eq!(req.messages.last().map(|m| m.role), Some(Role::User));
        &req.messages.last().unwrap().content
    }

    #[test]
    fn explain_code_names_language_and_file() {
        let req = explain_code(
            "def add(a, b):\n    return a + b\n",
            "Python",
            "src/math_utils.py",
        );
        assert_eq!(req.messages.len(), 1);
        assert!(req.system.contains("RustPy"));
        assert!(req.system.contains("summary"));
        let user = user_text(&req);
        assert!(
            user.starts_with("Explain this Python code from `src/math_utils.py`:"),
            "{user}"
        );
        assert!(
            user.contains("```python\ndef add(a, b):\n    return a + b\n```"),
            "{user}"
        );

        let req = explain_code("x = 1", "", "");
        assert!(
            user_text(&req).starts_with("Explain this code:\n\n```\nx = 1\n```"),
            "{}",
            user_text(&req)
        );
    }

    #[test]
    fn docstring_styles() {
        let src = "def area(r: float) -> float:\n    return 3.14159 * r * r";
        let google = generate_docstring(src, "google");
        assert!(google.system.contains("`Args:`"));
        assert!(google.system.contains("only the docstring"));
        assert!(user_text(&google).contains("```python\ndef area"));
        assert!(generate_docstring(src, "NumPy")
            .system
            .contains("`Parameters`"));
        assert!(generate_docstring(src, "rest")
            .system
            .contains(":param name:"));
        assert!(generate_docstring(src, "reStructuredText")
            .system
            .contains(":param name:"));
        assert!(generate_docstring(src, "epytext")
            .system
            .contains("@param name:"));
        assert!(generate_docstring(src, "plain")
            .system
            .contains("without structured sections"));
        assert!(generate_docstring(src, "auto")
            .system
            .contains("already has"));
        assert_eq!(
            generate_docstring(src, "unknown-style").system,
            google.system
        );
    }

    #[test]
    fn refactoring_and_tests_requests() {
        let req = suggest_refactoring("for i in range(len(xs)):\n    print(xs[i])");
        assert!(req.system.contains("Keep the behavior the same"));
        assert!(req.system.contains("single fenced code block"));
        assert!(user_text(&req).contains("print(xs[i])"));

        let pytest = write_tests("def inc(x):\n    return x + 1", "pytest");
        assert!(pytest.system.contains("pytest.mark.parametrize"));
        assert!(user_text(&pytest).starts_with("Write pytest tests"));
        let unit = write_tests("def inc(x):\n    return x + 1", "unittest");
        assert!(unit.system.contains("unittest.TestCase"));
        assert!(user_text(&unit).starts_with("Write unittest tests"));
        assert_eq!(
            write_tests("x", "nose").system,
            write_tests("x", "pytest").system
        );
    }

    #[test]
    fn commit_message_shows_recent_messages_and_diff() {
        let diff = "diff --git a/app.py b/app.py\n--- a/app.py\n+++ b/app.py\n@@ -1 +1 @@\n-print('hi')\n+print('hello')\n";
        let recent = vec![
            "fix(ui): keep focus in dialogs".to_string(),
            "  ".to_string(),
            "feat(vcs): show blame\n\nAdds a gutter.".to_string(),
        ];
        let req = commit_message(diff, &recent);
        assert!(req.system.contains("72 characters"));
        assert!(req.system.contains("Conventional Commits"));
        assert!(req.system.contains("only the commit message"));
        let user = user_text(&req);
        assert!(user.contains("<recent_commits>\nfix(ui): keep focus in dialogs\n---\nfeat(vcs): show blame\n\nAdds a gutter.\n</recent_commits>"), "{user}");
        assert!(user.contains("<diff>\ndiff --git a/app.py b/app.py"));
        assert!(user.contains("+print('hello')\n</diff>"));
        assert!(user.ends_with("Write the commit message for this diff."));

        let no_recent = commit_message(diff, &[]);
        assert!(!user_text(&no_recent).contains("recent_commits"));
        let many: Vec<String> = (0..30).map(|i| format!("chore: tidy {i}")).collect();
        let req = commit_message(diff, &many);
        assert!(user_text(&req).contains("chore: tidy 9\n"));
        assert!(!user_text(&req).contains("chore: tidy 10"));
    }

    #[test]
    fn fix_error_includes_code_and_traceback() {
        let req = fix_error("items = [1, 2]\nprint(items[2])", "Traceback (most recent call last):\n  File \"main.py\", line 2\nIndexError: list index out of range\n");
        assert!(req.system.contains("root cause"));
        let user = user_text(&req);
        assert!(
            user.contains("```\nitems = [1, 2]\nprint(items[2])\n```"),
            "{user}"
        );
        assert!(user.contains("```text\nTraceback"), "{user}");
        assert!(
            user.contains("IndexError: list index out of range\n```"),
            "{user}"
        );
    }

    #[test]
    fn chat_attaches_context_to_the_new_message_only() {
        let history = vec![
            Message::user("What does this project do?"),
            Message::assistant("It is a CLI tool."),
        ];
        let ctx = ContextSnippet {
            file: "cli/main.py".into(),
            selection: "def main():\n    run()".into(),
            language: "Python".into(),
        };
        let req = chat(&history, "Why does main() hang?", Some(ctx));
        assert_eq!(req.messages.len(), 3);
        assert_eq!(&req.messages[..2], &history[..]);
        let user = user_text(&req);
        assert!(user.starts_with("<editor_context>\nFile: `cli/main.py`\nLanguage: Python\nSelected code:\n```python\ndef main():\n    run()\n```\n</editor_context>\n\nWhy does main() hang?"), "{user}");
        assert!(req.system.contains("<editor_context>"));

        // No context, or an empty one, leaves the message as typed.
        assert_eq!(user_text(&chat(&[], "hello", None)), "hello");
        assert_eq!(
            user_text(&chat(&[], "hello", Some(ContextSnippet::default()))),
            "hello"
        );
        let file_only = chat(
            &[],
            "hello",
            Some(ContextSnippet {
                file: "a.py".into(),
                ..Default::default()
            }),
        );
        assert_eq!(
            user_text(&file_only),
            "<editor_context>\nFile: `a.py`\n</editor_context>\n\nhello"
        );
    }

    #[test]
    fn chat_history_is_normalized_and_bounded() {
        // An unanswered user turn (e.g. a failed request) merges with the new one;
        // a leading assistant turn is dropped.
        let history = vec![
            Message::assistant("Hi! How can I help?"),
            Message::user("first try"),
        ];
        let req = chat(&history, "second try", None);
        assert_eq!(req.messages, vec![Message::user("first try\n\nsecond try")]);

        // Old turns beyond the budget are dropped, newest kept, and the result starts with a user turn.
        let big = "y".repeat(50_000);
        let history: Vec<Message> = (0..10)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(format!("{i}{big}"))
                } else {
                    Message::assistant(format!("{i}{big}"))
                }
            })
            .collect();
        let req = chat(&history, "latest", None);
        let total: usize = req.messages.iter().map(|m| m.content.chars().count()).sum();
        assert!(total <= MAX_HISTORY_CHARS + 10, "{total}");
        assert_eq!(req.messages[0].role, Role::User);
        assert!(req.messages.len() >= 3);
        assert!(req.messages[req.messages.len() - 2]
            .content
            .starts_with('9'));
        assert_eq!(req.messages.last().unwrap().content, "latest");
        for pair in req.messages.windows(2) {
            assert_ne!(pair[0].role, pair[1].role);
        }
    }

    #[test]
    fn truncate_middle_keeps_head_and_tail() {
        assert!(matches!(
            truncate_middle("short", 100),
            Cow::Borrowed("short")
        ));
        let text: String = (0..5_000).map(|i| format!("line {i:05}\n")).collect();
        let out = truncate_middle(&text, 2_000);
        assert!(out.chars().count() <= 2_000, "{}", out.chars().count());
        assert!(out.starts_with("line 00000\nline 00001\n"));
        assert!(out.ends_with("line 04998\nline 04999\n"));
        assert!(out.contains(" characters omitted …]\n"));
        // Cuts land on line boundaries.
        for line in out.lines().filter(|l| !l.starts_with('[')) {
            assert_eq!(line.len(), 10, "partial line {line:?}");
        }
        // The marker states exactly how much is missing.
        let marker = out.lines().find(|l| l.starts_with("[… ")).unwrap();
        let omitted: usize = marker
            .trim_start_matches("[… ")
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let kept = out.chars().count() - marker.chars().count() - 1;
        assert_eq!(kept + omitted, text.chars().count());
    }

    #[test]
    fn truncate_middle_is_char_boundary_safe() {
        let text = "é🦀你".repeat(30_000);
        for max in [200, 1_000, 59_999, MAX_INPUT_CHARS] {
            let out = truncate_middle(&text, max);
            assert!(
                out.chars().count() <= max,
                "max {max}: {}",
                out.chars().count()
            );
            assert!(out.starts_with("é🦀"));
            assert!(out.ends_with("🦀你"));
        }
        let req = explain_code(&text, "python", "big.py");
        assert!(user_text(&req).contains("characters omitted"));
        assert!(user_text(&req).chars().count() < MAX_INPUT_CHARS + 200);
    }

    #[test]
    fn fences_outgrow_backticks_in_the_code() {
        let req = explain_code(
            "s = \"\"\"\n```python\nx\n```\n\"\"\"",
            "Markdown text",
            "notes.py",
        );
        let user = user_text(&req);
        assert!(
            user.contains("````markdown\ns = \"\"\"\n```python"),
            "{user}"
        );
        assert!(user.ends_with("\"\"\"\n````"), "{user}");
        assert_eq!(fence_lang("C++ (GCC)"), "c++");
        assert_eq!(fence_lang(""), "");
    }
}
