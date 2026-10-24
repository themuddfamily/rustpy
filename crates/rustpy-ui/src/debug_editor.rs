//! The debugger in the editor (roadmap §3.3): variable values at the end of
//! executed lines, and the value tooltip of the expression under the mouse /
//! Quick Evaluate Expression while the program is suspended.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rustpy_debug::{DebugSession, Variable};

use crate::editor::view::InlineValue;

/// Value types not worth repeating at the end of lines.
const QUIET_TYPES: &[&str] = &[
    "module",
    "function",
    "builtin_function_or_method",
    "type",
    "method",
    "method-wrapper",
    "ABCMeta",
];

/// Lines scanned above the execution point when no `def` is found.
const MAX_SCAN_LINES: usize = 400;

/// Values of one function at one suspension, and at the one before.
#[derive(Default, Clone, Debug)]
struct Snapshot {
    epoch: u64,
    values: HashMap<String, (String, String)>,
    previous: Option<HashMap<String, (String, String)>>,
}

/// Remembers locals per function across suspensions to flag changed values.
#[derive(Default)]
pub struct InlineState {
    /// Suspension, frame and locals fingerprint last recorded.
    seen: Option<(u64, i64, u64)>,
    by_function: HashMap<(PathBuf, String), Snapshot>,
}

/// A cheap fingerprint of locals' names and values (Set Value changes it).
fn fingerprint(locals: &[Variable]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in locals {
        v.name.hash(&mut h);
        v.value.hash(&mut h);
    }
    h.finish()
}

impl InlineState {
    /// Records the locals of the selected frame once they are loaded (again
    /// after Set Value changed them).
    pub fn update(&mut self, session: &DebugSession) {
        if !session.is_stopped() {
            return;
        }
        let Some(frame) = session.current_frame() else {
            return;
        };
        let Some(path) = frame.path.as_deref() else {
            return;
        };
        let Some(locals) = session
            .scopes
            .first()
            .and_then(|s| session.variables.get(&s.variables_reference))
        else {
            return;
        };
        self.record(session.stop_epoch, frame.id, path, &frame.name, locals);
    }

    /// Records `locals` of `function` (frame `frame_id` in `path`) at suspension
    /// `epoch`; what was recorded at an earlier suspension becomes the previous values.
    fn record(
        &mut self,
        epoch: u64,
        frame_id: i64,
        path: &Path,
        function: &str,
        locals: &[Variable],
    ) {
        let key = (epoch, frame_id, fingerprint(locals));
        if self.seen == Some(key) {
            return;
        }
        let values: HashMap<String, (String, String)> = locals
            .iter()
            .map(|v| (v.name.clone(), (v.value.clone(), v.type_name.clone())))
            .collect();
        let snap = self
            .by_function
            .entry((path.to_path_buf(), function.to_string()))
            .or_default();
        if snap.epoch != epoch {
            let had = !snap.values.is_empty() || snap.epoch != 0;
            let old = std::mem::take(&mut snap.values);
            snap.previous = had.then_some(old);
            snap.epoch = epoch;
        }
        snap.values = values;
        self.seen = Some(key);
    }

    /// Locals of the selected frame whose value differs from the previous suspension.
    pub fn changed_names(&self, session: &DebugSession) -> std::collections::HashSet<String> {
        let Some(frame) = session.current_frame() else {
            return Default::default();
        };
        let Some(path) = frame.path.clone() else {
            return Default::default();
        };
        let Some(snap) = self
            .by_function
            .get(&(path, frame.name.clone()))
            .filter(|s| s.epoch == session.stop_epoch)
        else {
            return Default::default();
        };
        let Some(prev) = &snap.previous else {
            return Default::default();
        };
        snap.values
            .iter()
            .filter(|(n, (v, _))| prev.get(*n).map(|p| &p.0) != Some(v))
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Values to paint after the lines of `path` for the selected frame: names
    /// used on each line from the start of its function to the execution line.
    pub fn values_for(
        &self,
        session: &DebugSession,
        path: &Path,
        line_text: impl Fn(usize) -> String,
        line_count: usize,
    ) -> Vec<InlineValue> {
        if !session.is_stopped() {
            return Vec::new();
        }
        let Some(frame) = session
            .current_frame()
            .filter(|f| f.path.as_deref() == Some(path))
        else {
            return Vec::new();
        };
        let Some(snap) = self
            .by_function
            .get(&(path.to_path_buf(), frame.name.clone()))
            .filter(|s| s.epoch == session.stop_epoch)
        else {
            return Vec::new();
        };
        let exec = frame
            .line
            .saturating_sub(1)
            .min(line_count.saturating_sub(1));
        let lines = frame_lines(&line_text, exec, &frame.name);
        inline_values(
            &snap.values,
            snap.previous.as_ref(),
            lines.into_iter().map(|l| (l, line_text(l))),
        )
    }
}

/// A value already rendered for the selected frame. Passive hover never sends
/// an evaluate or variables request: Python attribute access, indexing and even
/// repr of a plain name can execute arbitrary code while the program is stopped.
pub fn cached_hover_variable<'a>(
    session: &'a DebugSession,
    expression: &str,
) -> Option<&'a Variable> {
    if !session.is_stopped() {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    for scope in &session.scopes {
        let mut pending = vec![scope.variables_reference];
        while let Some(reference) = pending.pop() {
            if !seen.insert(reference) {
                continue;
            }
            let Some(vars) = session.variables.get(&reference) else {
                continue;
            };
            for var in vars {
                if var.evaluate_name.as_deref() == Some(expression)
                    || (reference == scope.variables_reference && var.name == expression)
                {
                    return Some(var);
                }
                if var.variables_reference != 0 {
                    pending.push(var.variables_reference);
                }
            }
        }
    }
    None
}

/// Lines (0-based) annotated for a frame of function `name` executing at `exec`:
/// from the start of the function (or of the module code above) to the
/// execution line, without the functions and classes defined in between. Their
/// names belong to other scopes: the parameter `v` of a function above a
/// module-level stop is not the module's `v`. A class line stays (its bases are
/// names of this scope).
fn frame_lines(line_text: &impl Fn(usize) -> String, exec: usize, name: &str) -> Vec<usize> {
    let start = function_start(line_text, exec, name);
    let module = name == "<module>";
    let mut out = Vec::new();
    let mut in_string: Option<String> = None;
    let mut brackets = 0usize;
    let mut backslash = false;
    // Indentation of the `def` / `class` whose body is left out.
    let mut skip_deeper_than: Option<usize> = None;
    // A module scan that starts inside a function body waits for module-level code.
    let mut started = !module || start == 0;
    for l in start..=exec {
        let text = line_text(l);
        // Lines inside a multi-line string, blank lines and comments neither
        // open nor close a block.
        let continued = in_string.is_some() || brackets > 0 || backslash;
        logical_line_state(&text, &mut in_string, &mut brackets, &mut backslash);
        let t = text.trim_start();
        if !continued && !t.is_empty() && !t.starts_with('#') {
            let indent = indent_of(&text);
            if skip_deeper_than.is_some_and(|d| indent <= d) {
                skip_deeper_than = None;
            }
            started |= indent == 0;
            let header = t.strip_prefix("async ").map_or(t, str::trim_start);
            let (def, class) = (header.starts_with("def "), header.starts_with("class "));
            let own_def = !module && l == start;
            if started && skip_deeper_than.is_none() && (def || class) && !own_def {
                skip_deeper_than = Some(indent);
                if class {
                    out.push(l);
                }
                continue;
            }
        }
        if started && skip_deeper_than.is_none() {
            out.push(l);
        }
    }
    out
}

/// Carries Python physical-line continuation across multi-line headers. A
/// closing `):` at the def's indentation still belongs to its header, so it
/// cannot end the skipped body. Brackets in comments and strings do not count.
fn logical_line_state(
    text: &str,
    quote: &mut Option<String>,
    brackets: &mut usize,
    backslash: &mut bool,
) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    *backslash = false;
    while i < chars.len() {
        if let Some(q) = quote.clone() {
            let q: Vec<char> = q.chars().collect();
            if chars[i] == '\\' {
                *backslash = i + 1 == chars.len();
                i += 2;
            } else if chars[i..].starts_with(&q) {
                i += q.len();
                *quote = None;
            } else {
                i += 1;
            }
            continue;
        }
        match chars[i] {
            '#' => break,
            '\'' | '"' => {
                let c = chars[i];
                let n = if chars[i..].starts_with(&[c, c, c]) {
                    3
                } else {
                    1
                };
                *quote = Some(std::iter::repeat_n(c, n).collect());
                i += n;
                continue;
            }
            '(' | '[' | '{' => *brackets += 1,
            ')' | ']' | '}' => *brackets = brackets.saturating_sub(1),
            '\\' if i + 1 == chars.len() => *backslash = true,
            _ => {}
        }
        i += 1;
    }
    if !*backslash && quote.as_ref().is_some_and(|q| q.len() == 1) {
        *quote = None;
    }
}

/// First line of the code of the function `name` executing at `exec` (0-based).
fn function_start(line_text: &impl Fn(usize) -> String, exec: usize, name: &str) -> usize {
    let lowest = exec.saturating_sub(MAX_SCAN_LINES);
    if name == "<module>" {
        return lowest;
    }
    let exec_indent = indent_of(&line_text(exec));
    for l in (lowest..exec).rev() {
        let text = line_text(l);
        let t = text.trim_start();
        let def = t.strip_prefix("async ").unwrap_or(t).trim_start();
        if let Some(rest) = def.strip_prefix("def ") {
            let fname: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if (fname == name || name.starts_with('<')) && indent_of(&text) < exec_indent.max(1) {
                return l;
            }
        }
    }
    exec.saturating_sub(50)
}

fn indent_of(s: &str) -> usize {
    s.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield", "match", "case",
];

/// `name: value` lists for `lines` (0-based number, text) from the locals `values`
/// (`name → (value, type)`); a line is flagged when one of its values differs
/// from `previous`.
pub fn inline_values(
    values: &HashMap<String, (String, String)>,
    previous: Option<&HashMap<String, (String, String)>>,
    lines: impl Iterator<Item = (usize, String)>,
) -> Vec<InlineValue> {
    let mut out = Vec::new();
    let mut in_string: Option<String> = None;
    for (line, text) in lines {
        let mut names: Vec<String> = Vec::new();
        for name in identifiers(&text, &mut in_string) {
            if names.contains(&name) || KEYWORDS.contains(&name.as_str()) {
                continue;
            }
            if let Some((_, ty)) = values.get(&name) {
                if !QUIET_TYPES.contains(&ty.as_str()) {
                    names.push(name);
                }
            }
        }
        if names.is_empty() {
            continue;
        }
        let mut changed = false;
        let parts: Vec<String> = names
            .iter()
            .map(|n| {
                let value = &values[n].0;
                if let Some(prev) = previous {
                    changed |= prev.get(n).map(|p| &p.0) != Some(value);
                }
                format!("{n}: {}", truncate(&value.replace('\n', "⏎"), 50))
            })
            .collect();
        out.push(InlineValue {
            line,
            text: truncate(&parts.join(", "), 110),
            changed,
        });
    }
    out
}

/// At most `max` characters of `s` (with `…` when cut).
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Identifiers of a source line outside strings and comments, skipping
/// attribute names (`obj.attr` gives `obj`). `in_string` carries an open
/// triple-quoted string across lines.
fn identifiers(text: &str, in_string: &mut Option<String>) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut prev_sig: Option<char> = None;
    while i < chars.len() {
        if let Some(q) = in_string.clone() {
            let qc: Vec<char> = q.chars().collect();
            if chars[i] == '\\' {
                i += 2;
                continue;
            }
            if chars[i..].starts_with(&qc) {
                i += qc.len();
                *in_string = None;
                prev_sig = Some('"');
            } else {
                i += 1;
            }
            continue;
        }
        let c = chars[i];
        if c == '#' {
            break;
        }
        if c == '"' || c == '\'' {
            let triple: String = std::iter::repeat_n(c, 3).collect();
            let q = if chars[i..].starts_with(&[c, c, c]) {
                triple
            } else {
                c.to_string()
            };
            i += q.chars().count();
            *in_string = Some(q);
            continue;
        }
        if c.is_ascii_digit() {
            // Numbers (`1e5`, `0x1F`, `1_000.5`) contain no names.
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            prev_sig = Some('0');
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            // A string prefix (`f"…"`, `rb'…'`) is not a name.
            let is_prefix = i < chars.len()
                && (chars[i] == '"' || chars[i] == '\'')
                && word.len() <= 2
                && word.chars().all(|c| "rRbBuUfF".contains(c));
            if prev_sig != Some('.') && !is_prefix {
                out.push(word);
            }
            prev_sig = Some('a');
            continue;
        }
        if !c.is_whitespace() {
            prev_sig = Some(c);
        }
        i += 1;
    }
    // Single-quoted strings end with the line.
    if in_string.as_ref().is_some_and(|q| q.chars().count() == 1) {
        *in_string = None;
    }
    out
}

/// The expression the mouse hover looks up in the frame cache at char `column` of `line`: the
/// identifier there with the dotted / subscripted chain before it
/// (`self.items[0].name` when hovering `name`). None outside identifiers.
///
/// Passive hover uses only cached values (even plain attribute access or repr
/// can run Python code). Call-result chains are also reserved for explicit
/// Quick Evaluate, so this chain holds no calls and subscripts hold only literals and names: hovering
/// `strip` in `f.readline().strip()` gives None, hovering `readline` gives
/// `f.readline`.
pub fn hover_expression(line: &str, column: usize) -> Option<String> {
    expression_at(line, column, false)
}

/// The expression Quick Evaluate Expression (Ctrl+Alt+F8) evaluates for the caret
/// at char `column` of `line`: like [`hover_expression`], but an explicit request
/// may run calls (`f(x).name`, `(a + b).c`).
pub fn quick_evaluate_expression(line: &str, column: usize) -> Option<String> {
    expression_at(line, column, true)
}

fn expression_at(line: &str, column: usize, calls: bool) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    if column >= chars.len() || !is_ident(chars[column]) {
        return None;
    }
    if inside_string_or_comment(&chars[..column]) {
        return None;
    }
    let mut end = column;
    while end < chars.len() && is_ident(chars[end]) {
        end += 1;
    }
    let mut start = column;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    if chars[start].is_ascii_digit() {
        return None;
    }
    let partners = bracket_partners(&chars);
    // Walk left over `.name`, `[...]` and (with `calls`) `(...)` parts.
    loop {
        let mut j = start;
        while j > 0 && chars[j - 1] == ' ' {
            j -= 1;
        }
        if j == 0 || chars[j - 1] != '.' {
            break;
        }
        let mut k = j - 1;
        while k > 0 && chars[k - 1] == ' ' {
            k -= 1;
        }
        // Brackets after a name: `a[0].b`, `f(x).b`.
        let mut bracketed = false;
        while k > 0 && matches!(chars[k - 1], ']' | ')') {
            let close = k - 1;
            if chars[close] == ')' && !calls {
                return None; // the value of a call (or of a parenthesized expression)
            }
            // Unbalanced on this line: no complete expression.
            let open = *partners.get(&close)?;
            if chars[close] == ']' && !calls && !is_plain_subscript(&chars[open + 1..close]) {
                return None;
            }
            k = open;
            bracketed = true;
        }
        let name_end = k;
        while k > 0 && is_ident(chars[k - 1]) {
            k -= 1;
        }
        if k == name_end {
            // No name before the brackets: `(a + b).c` or `[1, 2].count` starts the
            // chain (Quick Evaluate only); a string or dict literal is not followed.
            let literal_before = k > 0 && matches!(chars[k - 1], '"' | '\'' | '}');
            if !(calls && bracketed) || literal_before {
                return None;
            }
            start = k;
            break;
        }
        start = k;
    }
    let expr: String = chars[start..end].iter().collect();
    let first = expr.split(|c: char| !is_ident(c)).next().unwrap_or("");
    if KEYWORDS.contains(&first) && first != "self" {
        return None;
    }
    Some(expr)
}

/// Whether the end of `before` (the start of a line) is inside a string or a comment.
fn inside_string_or_comment(before: &[char]) -> bool {
    let mut quote: Option<Vec<char>> = None;
    let mut i = 0;
    while i < before.len() {
        let c = before[i];
        match &quote {
            Some(q) => {
                if c == '\\' {
                    i += 2;
                    continue;
                }
                if before[i..].starts_with(q) {
                    i += q.len();
                    quote = None;
                    continue;
                }
            }
            None if c == '#' => return true,
            None if c == '"' || c == '\'' => {
                let q = if before[i..].starts_with(&[c, c, c]) {
                    vec![c, c, c]
                } else {
                    vec![c]
                };
                i += q.len();
                quote = Some(q);
                continue;
            }
            None => {}
        }
        i += 1;
    }
    quote.is_some()
}

/// For each closing bracket of a line (outside strings and comments), the index
/// of the bracket that opens it.
fn bracket_partners(chars: &[char]) -> HashMap<usize, usize> {
    let mut out = HashMap::new();
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut quote: Option<Vec<char>> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = &quote {
            if c == '\\' {
                i += 2;
            } else if chars[i..].starts_with(q) {
                i += q.len();
                quote = None;
            } else {
                i += 1;
            }
            continue;
        }
        match c {
            '#' => break,
            '"' | '\'' => {
                let q = if chars[i..].starts_with(&[c, c, c]) {
                    vec![c, c, c]
                } else {
                    vec![c]
                };
                i += q.len();
                quote = Some(q);
                continue;
            }
            '(' | '[' | '{' => stack.push((c, i)),
            ')' | ']' | '}' => {
                let open = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if stack.last().is_some_and(|(o, _)| *o == open) {
                    if let Some((_, at)) = stack.pop() {
                        out.insert(i, at);
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Whether a subscript holds only literals, names and attribute chains (`0`,
/// `-1`, `'key'`, `i`, `self.n`, `1:3`, `i, j`): nothing that calls code.
fn is_plain_subscript(content: &[char]) -> bool {
    let mut any = false;
    // The previous token was a value: a `-` after it would be a subtraction.
    let mut operand = false;
    let mut i = 0;
    while i < content.len() {
        let c = content[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            let q: Vec<char> = if content[i..].starts_with(&[c, c, c]) {
                vec![c, c, c]
            } else {
                vec![c]
            };
            let mut j = i + q.len();
            loop {
                if j >= content.len() {
                    return false;
                }
                if content[j] == '\\' {
                    j += 2;
                } else if content[j..].starts_with(&q) {
                    j += q.len();
                    break;
                } else {
                    j += 1;
                }
            }
            (i, any, operand) = (j, true, true);
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            let s = i;
            let number = c.is_ascii_digit();
            while i < content.len()
                && (content[i].is_alphanumeric()
                    || content[i] == '_'
                    || (number && content[i] == '.'))
            {
                i += 1;
            }
            let word: String = content[s..i].iter().collect();
            if i < content.len() && (content[i] == '"' || content[i] == '\'') {
                // A string prefix: f-strings (and t-strings) run code.
                if word.chars().count() > 2 || !word.chars().all(|ch| "rRbBuU".contains(ch)) {
                    return false;
                }
                continue;
            }
            if !number
                && KEYWORDS.contains(&word.as_str())
                && !matches!(word.as_str(), "True" | "False" | "None")
            {
                return false;
            }
            (any, operand) = (true, true);
            continue;
        }
        match c {
            '.' if operand => {}
            ':' | ',' => operand = false,
            '-' if !operand => {}
            _ => return false,
        }
        i += 1;
    }
    any
}

/// State of the editor's value popup (hover or Quick Evaluate).
#[derive(Clone, Debug, Default)]
pub struct ValuePopup {
    pub expression: String,
    /// Screen position of the popup's top-left corner.
    pub anchor: egui::Pos2,
    /// Quick Evaluate: stays until Escape or a click elsewhere.
    pub pinned: bool,
    /// The popup's rectangle in the last frame (the pointer may move into it).
    pub rect: Option<egui::Rect>,
}

/// The editor hover while suspended: which expression is under the mouse and since when.
#[derive(Clone, Debug, Default)]
pub struct HoverTracker {
    pub expression: Option<String>,
    pub since: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(pairs: &[(&str, &str, &str)]) -> HashMap<String, (String, String)> {
        pairs
            .iter()
            .map(|(n, v, t)| (n.to_string(), (v.to_string(), t.to_string())))
            .collect()
    }

    #[test]
    fn values_follow_the_names_on_each_line() {
        let src = [
            "def f(a, items):",
            "    total = 0  # total of items",
            "    for x in items:",
            "        total += x * a",
            "    s = 'items: ' + str(total)",
            "    return math.floor(total)",
        ];
        let values = vals(&[
            ("a", "2", "int"),
            ("items", "[1, 2, 3]", "list"),
            ("total", "12", "int"),
            ("x", "3", "int"),
            ("math", "<module 'math'>", "module"),
        ]);
        let prev = vals(&[
            ("a", "2", "int"),
            ("items", "[1, 2, 3]", "list"),
            ("total", "6", "int"),
            ("x", "2", "int"),
        ]);
        let out = inline_values(
            &values,
            Some(&prev),
            src.iter().enumerate().map(|(i, s)| (i, s.to_string())),
        );
        let texts: Vec<(usize, &str, bool)> = out
            .iter()
            .map(|v| (v.line, v.text.as_str(), v.changed))
            .collect();
        assert_eq!(
            texts,
            vec![
                (0, "a: 2, items: [1, 2, 3]", false),
                (1, "total: 12", true),
                (2, "x: 3, items: [1, 2, 3]", true),
                (3, "total: 12, x: 3, a: 2", true),
                (4, "total: 12", true),
                (5, "total: 12", true)
            ]
        );
        // Without a previous suspension nothing is flagged; long values are cut.
        let long = vals(&[("big", &"9".repeat(80), "int")]);
        let out = inline_values(&long, None, std::iter::once((0, "print(big)".to_string())));
        assert!(!out[0].changed);
        assert_eq!(out[0].text.chars().count(), "big: ".len() + 50);
        assert!(out[0].text.ends_with('…'));
    }

    #[test]
    fn set_value_refreshes_the_recorded_locals() {
        let var = |n: &str, v: &str| Variable {
            name: n.into(),
            value: v.into(),
            type_name: "int".into(),
            variables_reference: 0,
            evaluate_name: None,
        };
        let (p, key) = (
            Path::new("/p/a.py"),
            (PathBuf::from("/p/a.py"), "f".to_string()),
        );
        let mut st = InlineState::default();
        st.record(1, 7, p, "f", &[var("x", "1")]);
        st.record(2, 7, p, "f", &[var("x", "2")]);
        assert_eq!(
            (
                st.by_function[&key].values["x"].0.as_str(),
                st.by_function[&key].previous.as_ref().unwrap()["x"]
                    .0
                    .as_str()
            ),
            ("2", "1")
        );
        // Set Value in the same suspension: the new value shows; the previous suspension stays the reference.
        st.record(2, 7, p, "f", &[var("x", "40")]);
        assert_eq!(
            (
                st.by_function[&key].values["x"].0.as_str(),
                st.by_function[&key].previous.as_ref().unwrap()["x"]
                    .0
                    .as_str()
            ),
            ("40", "1")
        );
    }

    #[test]
    fn strings_comments_and_attributes_are_skipped() {
        let mut s = None;
        assert_eq!(
            identifiers("x = obj.attr + f'{y}' # z", &mut s),
            vec!["x", "obj"]
        );
        assert_eq!(identifiers("doc = \"\"\"start x", &mut s), vec!["doc"]);
        assert!(s.is_some());
        assert_eq!(identifiers("still y\"\"\" + z", &mut s), vec!["z"]);
        assert_eq!(identifiers("n = 1e5 + v2", &mut None), vec!["n", "v2"]);
    }

    #[test]
    fn hover_expressions() {
        let line = "    total = self.items[0].price * qty  # self.x";
        let col = |needle: &str| line.find(needle).unwrap();
        assert_eq!(
            hover_expression(line, col("price")).as_deref(),
            Some("self.items[0].price")
        );
        assert_eq!(
            hover_expression(line, col("items")).as_deref(),
            Some("self.items")
        );
        assert_eq!(hover_expression(line, col("self")).as_deref(), Some("self"));
        assert_eq!(
            hover_expression(line, col("qty") + 1).as_deref(),
            Some("qty")
        );
        assert_eq!(hover_expression(line, col("=")), None);
        assert_eq!(
            hover_expression(line, line.rfind('x').unwrap()),
            None,
            "comment"
        );
        assert_eq!(hover_expression("print('total is')", 8), None, "string");
        assert_eq!(hover_expression("for x in range(3):", 0), None, "keyword");
        assert_eq!(hover_expression("d[k].v", 5).as_deref(), Some("d[k].v"));
    }

    /// The mouse hover never builds an expression that calls code; Quick Evaluate
    /// (an explicit request) may.
    #[test]
    fn hover_expressions_never_call() {
        let at = |line: &str, needle: &str| line.rfind(needle).unwrap();
        let line = "first = f.readline().strip()";
        assert_eq!(
            hover_expression(line, at(line, "strip")),
            None,
            "would consume a line of the file"
        );
        assert_eq!(
            hover_expression(line, at(line, "readline")).as_deref(),
            Some("f.readline"),
            "the bound method is fine"
        );
        assert_eq!(hover_expression(line, at(line, "f.")).as_deref(), Some("f"));
        assert_eq!(
            quick_evaluate_expression(line, at(line, "strip")).as_deref(),
            Some("f.readline().strip")
        );
        let line = "v = 1 if q.empty() else q.get().x";
        assert_eq!(
            hover_expression(line, at(line, "x")),
            None,
            "q.get() would block the suspended thread"
        );
        assert_eq!(
            hover_expression(line, at(line, "get")).as_deref(),
            Some("q.get")
        );
        assert_eq!(
            hover_expression(line, at(line, "empty")).as_deref(),
            Some("q.empty")
        );
        assert_eq!(
            quick_evaluate_expression(line, at(line, "x")).as_deref(),
            Some("q.get().x")
        );
        for (line, needle) in [
            ("v = items.pop().bit_length()", "bit_length"),
            ("name = next(it).name", "name"),
            ("y = a[f(x)].b", "b"),
            ("y = a[i - 1].b", "b"),
            ("y = a[g[0]].b", "b"),
            ("y = d[f\"{g()}\"].v", "v"),
            ("y = (a + b).c", "c"),
            ("y = a[not k].b", "b"),
            ("y = 'ab'[0].upper", "upper"),
            ("y = f(x)[0].b", "b"),
        ] {
            assert_eq!(hover_expression(line, at(line, needle)), None, "{line}");
        }
        // Subscripts of literals and names stay.
        for (line, needle, expr) in [
            ("y = a[-1].b", "b", "a[-1].b"),
            ("y = d['k(1)'].v", "v", "d['k(1)'].v"),
            ("y = m[i, 0:2].T", "T", "m[i, 0:2].T"),
            ("y = self.rows[self.n].id", "id", "self.rows[self.n].id"),
            ("y = d[None].v", "v", "d[None].v"),
            ("y = d[rb'x'].v", "v", "d[rb'x'].v"),
        ] {
            assert_eq!(
                hover_expression(line, at(line, needle)).as_deref(),
                Some(expr),
                "{line}"
            );
        }
        assert_eq!(
            quick_evaluate_expression("y = (a + b).c", 12).as_deref(),
            Some("(a + b).c")
        );
        assert_eq!(
            quick_evaluate_expression("y = 'ab'[0].upper", 12),
            None,
            "a string literal is not followed"
        );
        assert_eq!(
            quick_evaluate_expression("    arg).y", 9),
            None,
            "the call opens on an earlier line"
        );
        assert_eq!(hover_expression("    x.y)", 6).as_deref(), Some("x.y"));
    }

    fn wait_until(s: &mut DebugSession, pred: impl Fn(&DebugSession) -> bool, secs: u64) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_secs() < secs {
            s.poll();
            if pred(s) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    /// python3 with debugpy installed (the debugpy tests are skipped without it).
    fn python_with_debugpy() -> Option<PathBuf> {
        let py = which::which("python3").ok()?;
        if std::process::Command::new(&py)
            .args(["-c", "import debugpy"])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("debugpy not installed; skipping");
            return None;
        }
        Some(py)
    }

    /// The editor hover evaluates what is under the mouse while the program is
    /// suspended: resting it anywhere on a line must neither change the program's
    /// state (`f.readline().strip` read a line) nor block the suspended thread
    /// (`q.get().x` on an empty queue never answered, and stepping hung after it).
    #[test]
    fn hovering_suspended_lines_runs_no_program_code() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.txt"), "first\nsecond\nthird\n").unwrap();
        let lines = [
            "import queue",
            "q = queue.Queue()",
            "f = open('data.txt')",
            "first = f.readline().strip()",
            "v = 1 if q.empty() else q.get().x",
            "second = f.readline().strip()",
            "with open('result.txt', 'w') as out:",
            "    out.write(first + ',' + second + ',' + str(v))",
        ];
        let script = dir.path().join("hover.py");
        std::fs::write(&script, lines.join("\n") + "\n").unwrap();
        let mut cfg = rustpy_project::RunConfig::for_script(&script);
        cfg.interpreter = Some(py);
        let breakpoints = HashMap::from([(
            script.clone(),
            vec![
                rustpy_debug::Breakpoint::new(4),
                rustpy_debug::Breakpoint::new(5),
            ],
        )]);
        let mut s = DebugSession::launch(
            1,
            &cfg,
            None,
            dir.path(),
            rustpy_debug::LaunchSetup {
                breakpoints,
                ..Default::default()
            },
        )
        .expect("launch");
        let mut hovered = Vec::new();
        for (n, stop) in [(0, 4usize), (1, 5)] {
            if n > 0 {
                s.continue_();
            }
            assert!(
                wait_until(
                    &mut s,
                    |s| s.is_stopped() && s.frames.first().is_some_and(|f| f.line == stop),
                    30
                ),
                "no stop at line {stop}: {:?}",
                s.state
            );
            let text = lines[stop - 1];
            let mut exprs: Vec<String> = Vec::new();
            for col in 0..text.chars().count() {
                if let Some(e) = hover_expression(text, col).filter(|e| !exprs.contains(e)) {
                    exprs.push(e);
                }
            }
            for e in &exprs {
                s.evaluate_hover(e);
                assert!(
                    wait_until(
                        &mut s,
                        |s| s.hover_result.as_ref().is_some_and(|r| &r.expression == e),
                        10
                    ),
                    "the hover of `{e}` never answered (the suspended thread is blocked)"
                );
            }
            hovered.extend(exprs);
        }
        assert!(
            hovered.iter().any(|e| e == "f.readline") && hovered.iter().any(|e| e == "q.get"),
            "{hovered:?}"
        );
        // The program goes on as if nothing had been hovered.
        s.step_over();
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped() && s.frames.first().is_some_and(|f| f.line == 6),
                15
            ),
            "Step Over after hovering: {:?}",
            s.state
        );
        s.continue_();
        assert!(wait_until(&mut s, |s| !s.is_active(), 20), "{:?}", s.state);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("result.txt")).unwrap(),
            "first,second,1",
            "hovered: {hovered:?}"
        );
    }

    /// At a module-level stop the parameter `v` of a function above is not the
    /// module's `v`; in a function a nested function's body is another scope too.
    #[test]
    fn inline_values_stay_in_the_frames_scope() {
        let shown = |src: &[&str],
                     exec: usize,
                     frame: &str,
                     values: &HashMap<String, (String, String)>|
         -> Vec<(usize, String)> {
            let text = |l: usize| src.get(l).unwrap_or(&"").to_string();
            inline_values(
                values,
                None,
                frame_lines(&text, exec, frame)
                    .into_iter()
                    .map(|l| (l, text(l))),
            )
            .into_iter()
            .map(|v| (v.line, v.text))
            .collect()
        };
        let src = [
            "import math",
            "def scale(v):",
            "    return v * 2",
            "",
            "class K(Base):",
            "    v = 3",
            "    def m(self, v):",
            "        return v",
            "",
            "v = 10",
            "w = scale(v)",
            "print(v, w)",
        ];
        let text = |l: usize| src.get(l).unwrap_or(&"").to_string();
        assert_eq!(frame_lines(&text, 11, "<module>"), vec![0, 4, 9, 10, 11]);
        let module = vals(&[
            ("v", "10", "int"),
            ("w", "20", "int"),
            ("Base", "<object>", "object"),
            ("scale", "<function scale>", "function"),
            ("math", "<module 'math'>", "module"),
        ]);
        let expected: Vec<(usize, String)> = [
            (4, "Base: <object>"),
            (9, "v: 10"),
            (10, "w: 20, v: 10"),
            (11, "v: 10, w: 20"),
        ]
        .iter()
        .map(|(l, t)| (*l, t.to_string()))
        .collect();
        assert_eq!(shown(&src, 11, "<module>", &module), expected);
        // In a function: its own parameters show, a nested function's do not.
        let src = [
            "def outer(items):",
            "    def key(v):",
            "        return v.x",
            "    v = 5",
            "    return sorted(items, key=key)",
        ];
        let local = vals(&[
            ("items", "[3, 1]", "list"),
            ("v", "5", "int"),
            ("key", "<function key>", "function"),
        ]);
        let expected: Vec<(usize, String)> =
            [(0, "items: [3, 1]"), (3, "v: 5"), (4, "items: [3, 1]")]
                .iter()
                .map(|(l, t)| (*l, t.to_string()))
                .collect();
        assert_eq!(shown(&src, 4, "outer", &local), expected);
        // A multi-line string inside a skipped body does not end it early.
        let src = [
            "def f():",
            "    s = \"\"\"",
            "x",
            "\"\"\"",
            "    return s",
            "x = 2",
            "print(x)",
        ];
        let text = |l: usize| src.get(l).unwrap_or(&"").to_string();
        assert_eq!(frame_lines(&text, 6, "<module>"), vec![5, 6]);
        // A long module scan that starts inside a function body waits for module-level code.
        let mut src = vec!["    y = 1"; 402];
        src.extend(["x = 2", "print(x)"]);
        let text = |l: usize| src.get(l).unwrap_or(&"").to_string();
        assert_eq!(frame_lines(&text, 403, "<module>"), vec![402, 403]);
    }

    #[test]
    fn multiline_headers_keep_inner_scopes_out() {
        for prefix in ["def scale(", "async def scale(", "class Scale("] {
            let src = [
                prefix,
                "    v, # ) must not close the header",
                "):",
                "    v = 20",
                "v = 10",
                "print(v)",
            ];
            let text = |l: usize| src[l].to_string();
            let expected = if prefix.starts_with("class") {
                vec![0, 4, 5]
            } else {
                vec![4, 5]
            };
            assert_eq!(frame_lines(&text, 5, "<module>"), expected, "{prefix}");
        }
        let src = [
            "def outer():",
            "    def inner(",
            "        v=')',",
            "    ):",
            "        return v",
            "    v = 10",
            "    return v",
        ];
        let text = |l: usize| src[l].to_string();
        assert_eq!(frame_lines(&text, 6, "outer"), vec![0, 5, 6]);
        let src = ["def inner \\", "(v):", "    return v", "v = 10", "print(v)"];
        let text = |l: usize| src[l].to_string();
        assert_eq!(frame_lines(&text, 4, "<module>"), vec![3, 4]);
    }

    /// Render the real passive editor-hover path while debugpy is suspended.
    /// Initial variable rendering may call repr; compare after those values
    /// loaded, so this guards only extra work caused by resting the pointer.
    #[test]
    fn passive_hover_reuses_values_without_running_python() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("passive.py");
        let code = "count = 0\nclass C:\n    def __repr__(self):\n        global count\n        count += 1\n        return 'C instance'\n    @property\n    def value(self):\n        global count\n        count += 100\n        return count\n    def __getitem__(self, key):\n        global count\n        count += 1000\n        return self\nobj = C()\nmarker = 1\nprint(count)\n";
        std::fs::write(&script, code).unwrap();
        let mut cfg = rustpy_project::RunConfig::for_script(&script);
        cfg.interpreter = Some(py);
        let breakpoints =
            HashMap::from([(script.clone(), vec![rustpy_debug::Breakpoint::new(17)])]);
        let mut session = DebugSession::launch(
            1,
            &cfg,
            None,
            dir.path(),
            rustpy_debug::LaunchSetup {
                breakpoints,
                ..Default::default()
            },
        )
        .unwrap();
        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        assert!(wait_until(
            &mut session,
            |s| s.is_stopped() && !s.scopes.is_empty(),
            30
        ));
        // OnDemand is the default loading policy. Explicitly expand Locals
        // before measuring passive hover: repr belongs to this initial load,
        // never to pointer motion or a hidden evaluate request.
        let locals = session.scopes[0].variables_reference;
        session.request_variables(locals);
        while !session.variables.contains_key(&locals) && std::time::Instant::now() < ready_deadline
        {
            session.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(session.variables.contains_key(&locals));
        session.evaluate_hover("count");
        assert!(wait_until(
            &mut session,
            |s| s
                .hover_result
                .as_ref()
                .is_some_and(|r| r.expression == "count"),
            10
        ));
        let before = session.hover_result.clone().unwrap();
        let cached = cached_hover_variable(&session, "obj").cloned().unwrap();
        assert_eq!(cached.value, "C instance");
        let ctx = egui::Context::default();
        let mut panel = crate::debug_panel::DebugPanel::default();
        let theme = crate::theme::UiTheme::dark();
        for (n, expr) in ["obj", "obj.value", "obj[0].value"].into_iter().enumerate() {
            let column = expr.chars().count() - 1;
            for elapsed in [0.0, 0.5, 0.8] {
                let now = n as f64 * 2.0 + elapsed;
                ctx.run_ui(
                    egui::RawInput {
                        time: Some(now),
                        ..Default::default()
                    },
                    |ui| {
                        panel.editor_hover(
                            ui,
                            &theme,
                            &mut session,
                            Some((expr.into(), column)),
                            now,
                        );
                    },
                )
                .drop_without_applying_deltas();
                session.poll();
            }
        }
        // Any accidental evaluate would replace the count result.
        for _ in 0..20 {
            session.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(session.hover_result, Some(before.clone()));
        assert_eq!(cached_hover_variable(&session, "obj"), Some(&cached));
        session.hover_result = None;
        session.evaluate_hover("count");
        assert!(wait_until(&mut session, |s| s.hover_result.is_some(), 10));
        assert_eq!(session.hover_result.as_ref().unwrap().result, before.result);
        // An explicit Quick Evaluate still evaluates properties.
        panel.quick_evaluate(&mut session, "obj.value", egui::Pos2::ZERO);
        assert!(wait_until(
            &mut session,
            |s| s
                .hover_result
                .as_ref()
                .is_some_and(|r| r.expression == "obj.value"),
            10
        ));
        session.stop();
        assert!(wait_until(&mut session, |s| !s.is_active(), 10));
    }

    #[test]
    fn function_start_finds_the_def() {
        let src = [
            "import os",
            "",
            "class A:",
            "    def run(self, n):",
            "        x = n",
            "        if x:",
            "            return x",
            "",
            "def other():",
            "    pass",
        ];
        let text = |l: usize| src.get(l).unwrap_or(&"").to_string();
        assert_eq!(function_start(&text, 6, "run"), 3);
        assert_eq!(function_start(&text, 9, "other"), 8);
        assert_eq!(function_start(&text, 1, "<module>"), 0);
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
