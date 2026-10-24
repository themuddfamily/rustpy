use pretty_assertions::assert_eq;

use super::*;

fn fmt(src: &str) -> String {
    format_source(src, &FormatOptions::black()).unwrap()
}

/// Formats `src` and checks the expected output (generated with Black 26 for
/// the default options), idempotency and that the program is unchanged.
fn check(src: &str, expected: &str) {
    let out = fmt(src);
    assert_eq!(out, expected);
    assert_eq!(fmt(&out), expected, "not idempotent");
    assert_eq!(
        semantic_fingerprint(&out),
        semantic_fingerprint(src),
        "program changed"
    );
}

#[test]
fn spacing_operators_commas_brackets() {
    check(
        "x=1+2*3\ny = ( x ,2 )\nprint ( y [ 0 ] , sep = '-' )\n",
        "x = 1 + 2 * 3\ny = (x, 2)\nprint(y[0], sep=\"-\")\n",
    );
    check(
        "def f(a,b = 2,*args,c:int=3,**kw)->int: return a\n",
        "def f(a, b=2, *args, c: int = 3, **kw) -> int:\n    return a\n",
    );
    check(
        "d={'a':1,**e}\ns={1,2}\nl=[*a,*b]\nt=x[1:2,::3]\nu=x[a+1 :b]\n",
        "d = {\"a\": 1, **e}\ns = {1, 2}\nl = [*a, *b]\nt = x[1:2, ::3]\nu = x[a + 1 : b]\n",
    );
    check(
        "if not(x)and y or-z: pass\n",
        "if not (x) and y or -z:\n    pass\n",
    );
    check(
        "print(*a,**k)\nlambda x,y:x\nx = y if z else w\nx**2+y**-1\n(a**b).c\nz = f(x) ** 2\n",
        "print(*a, **k)\nlambda x, y: x\nx = y if z else w\nx**2 + y**-1\n(a**b).c\nz = f(x) ** 2\n",
    );
    check(
        "@ dec\ndef f ( ) : ...\nclass A ( B , metaclass = M ) : ...\n",
        "@dec\ndef f(): ...\nclass A(B, metaclass=M): ...\n",
    );
    check(
        "a.b.c(d)[e]( f )\nimport a.b\nfrom . import x\nfrom .. m import (y)\n",
        "a.b.c(d)[e](f)\nimport a.b\nfrom . import x\nfrom ..m import y\n",
    );
    check("x = a if b else c; y = 2\n", "x = a if b else c\ny = 2\n");
    check(
        "async def f():\n    await g()\n    async with a as b: pass\n    return [i async for i in x]\n",
        "async def f():\n    await g()\n    async with a as b:\n        pass\n    return [i async for i in x]\n",
    );
    check(
        "match p:\n    case Point(x=0,y=0)|[1,*rest]: pass\n    case {'k':v,**r}: pass\n    case -1: pass\n    case _: pass\n",
        "match p:\n    case Point(x=0, y=0) | [1, *rest]:\n        pass\n    case {\"k\": v, **r}:\n        pass\n    case -1:\n        pass\n    case _:\n        pass\n",
    );
    check(
        "x = 0XABcd + 1E5 + 10J + 0O17\n",
        "x = 0xABCD + 1e5 + 10j + 0o17\n",
    );
    check(
        "try:\n    pass\nexcept* ValueError as e:\n    pass\nfinally:\n    pass\n",
        "try:\n    pass\nexcept* ValueError as e:\n    pass\nfinally:\n    pass\n",
    );
    check(
        "x = a[1:2]\ny = a[fn(x) :]\nz = a[-1:]\nw = a[: f(x) : g(y)]\n",
        "x = a[1:2]\ny = a[fn(x) :]\nz = a[-1:]\nw = a[: f(x) : g(y)]\n",
    );
    check(
        "if x: a = 1; b = 2\nwhile y: z()\n",
        "if x:\n    a = 1\n    b = 2\nwhile y:\n    z()\n",
    );
    check(
        "def f(a, /, b, *, c): ...\ntype X = list[int]\nprint(1 .real)\n",
        "def f(a, /, b, *, c): ...\n\n\ntype X = list[int]\nprint((1).real)\n",
    );
    check(
        "x = (yield)\ny = yield z\nif (n := len(a)) > 10: pass\n",
        "x = yield\ny = yield z\nif (n := len(a)) > 10:\n    pass\n",
    );
    check(
        "x = 1 + \\\n    2\nwith a as b, \\\n     c as d: pass\n",
        "x = 1 + 2\nwith a as b, c as d:\n    pass\n",
    );
}

#[test]
fn blank_lines_and_indentation() {
    let src = "import os\nimport sys\ndef f():\n\n\n    x = 1\n\n\n\n    return x\nclass A:\n    y = 1\n    def m(self):\n        pass\n\n\n\n    def n(self):\n\n        pass\nz = 2\n";
    // Black keeps (at most) one user blank line at the start of a block.
    let expected = "import os\nimport sys\n\n\ndef f():\n\n    x = 1\n\n    return x\n\n\nclass A:\n    y = 1\n\n    def m(self):\n        pass\n\n    def n(self):\n\n        pass\n\n\nz = 2\n";
    check(src, expected);
    // Tabs and odd indentation are normalized; decorators stay attached.
    check(
        "if a:\n\tif b:\n\t\tc()\n\n\n@dec\n\n@dec2\ndef f():\n  return 1\n",
        "if a:\n    if b:\n        c()\n\n\n@dec\n@dec2\ndef f():\n    return 1\n",
    );
    // Comments before a def belong to it; the two blank lines go before the comment.
    check(
        "x = 1\n# about f\ndef f():\n    pass\n",
        "x = 1\n\n\n# about f\ndef f():\n    pass\n",
    );
    check(
        "@dec\n# why\ndef f():\n    pass\n",
        "@dec\n# why\ndef f():\n    pass\n",
    );
    // Blank lines at file start are dropped; a module docstring gets a blank line after it.
    check(
        "\n\n\"\"\"Doc.\"\"\"\nimport os\nx = 1\n",
        "\"\"\"Doc.\"\"\"\n\nimport os\n\nx = 1\n",
    );
    // Consecutive stub defs (overloads) stay together with their implementation.
    check(
        "@overload\ndef f(x: int) -> int: ...\n@overload\ndef f(x: str) -> str: ...\ndef f(x):\n    return x\ndef g(): ...\ndef h(): ...\n",
        "@overload\ndef f(x: int) -> int: ...\n@overload\ndef f(x: str) -> str: ...\ndef f(x):\n    return x\n\n\ndef g(): ...\ndef h(): ...\n",
    );
    // Nested function inside a function: one blank line around it.
    check(
        "def outer():\n    x = 1\n    def inner():\n        pass\n    return inner\n",
        "def outer():\n    x = 1\n\n    def inner():\n        pass\n\n    return inner\n",
    );
    // Leaving a nested def through several levels still separates it.
    check(
        "def f():\n    if x:\n        def g(): return 1\n    y = 1\nelse_ = 2\n",
        "def f():\n    if x:\n\n        def g():\n            return 1\n\n    y = 1\n\n\nelse_ = 2\n",
    );
    check(
        "try:\n    import x\n\n\n\nexcept ImportError:\n    x = None\n",
        "try:\n    import x\n\n\nexcept ImportError:\n    x = None\n",
    );
    check(
        "if x:\n    def f(): ...\nelse:\n    def f(): ...\ny = 1\n",
        "if x:\n\n    def f(): ...\n\nelse:\n\n    def f(): ...\n\n\ny = 1\n",
    );
    check(
        "class A: ...\nclass B: ...\nx = 1\n",
        "class A: ...\n\n\nclass B: ...\n\n\nx = 1\n",
    );
}

#[test]
fn trailing_whitespace_and_final_newline() {
    check("x = 1   \ny = 2\t\n\n\n\n", "x = 1\ny = 2\n");
    check("x = 1", "x = 1\n");
    check("", "");
    check("\n\n", "\n");
    // Like Black, the first line's separator is used for the whole output.
    check("x = 1\r\ny = 2\r\n", "x = 1\r\ny = 2\r\n");
    check("x = 1\ny = 2\r\n", "x = 1\ny = 2\n");
}

#[test]
fn quote_normalization() {
    check(
        "a = 'x'\nb = 'it\\'s'\nc = 'say \"hi\"'\nd = r'\\d'\ne = B'bytes'\nf = '''doc'''\ng = u'x'\nh = ''\n",
        "a = \"x\"\nb = \"it's\"\nc = 'say \"hi\"'\nd = r\"\\d\"\ne = b\"bytes\"\nf = \"\"\"doc\"\"\"\ng = \"x\"\nh = \"\"\n",
    );
    // f-strings are left untouched (including nested quotes).
    check(
        "x = f'{a!r:>{w}} {b[\"k\"]}'\n",
        "x = f'{a!r:>{w}} {b[\"k\"]}'\n",
    );
    let single = FormatOptions {
        quote: QuoteStyle::Single,
        ..FormatOptions::black()
    };
    assert_eq!(
        format_source("a = \"x\"\nb = \"it's\"\n", &single).unwrap(),
        "a = 'x'\nb = \"it's\"\n"
    );
    assert_eq!(
        format_source("a = 'x'\n", &FormatOptions::pep8()).unwrap(),
        "a = 'x'\n"
    );
}

#[test]
fn long_lines_split_with_trailing_comma() {
    let src = "result = some_function_name(argument_number_one, argument_number_two, argument_number_three, argument_number_four)\n";
    let expected = "result = some_function_name(\n    argument_number_one,\n    argument_number_two,\n    argument_number_three,\n    argument_number_four,\n)\n";
    check(src, expected);
    // Body fits on one line: no trailing comma (Black style).
    let src2 = "value = compute_something_rather_long(alpha_parameter, beta_parameter, gamma_parameter, delta)\n";
    let expected2 = "value = compute_something_rather_long(\n    alpha_parameter, beta_parameter, gamma_parameter, delta\n)\n";
    check(src2, expected2);
    // Magic trailing comma forces explosion even when short.
    check("x = [1, 2, 3,]\n", "x = [\n    1,\n    2,\n    3,\n]\n");
    check(
        "f(a, [1, 2,])\n",
        "f(\n    a,\n    [\n        1,\n        2,\n    ],\n)\n",
    );
    // A one-tuple's comma is not magic; subscripts never gain commas.
    check("t = (1,)\nv = d[1,]\n", "t = (1,)\nv = d[1,]\n");
    // def parameters.
    let src3 = "def function_with_many_parameters(self, first_parameter, second_parameter, third_parameter=None):\n    pass\n";
    let expected3 = "def function_with_many_parameters(\n    self, first_parameter, second_parameter, third_parameter=None\n):\n    pass\n";
    check(src3, expected3);
    let src4 = "def function_with_many_parameters(self, first_parameter, second_parameter, third_parameter=None, fourth_parameter=1, fifth=2) -> int:\n    pass\n";
    let expected4 = "def function_with_many_parameters(\n    self,\n    first_parameter,\n    second_parameter,\n    third_parameter=None,\n    fourth_parameter=1,\n    fifth=2,\n) -> int:\n    pass\n";
    check(src4, expected4);
    // Nested structures explode recursively.
    let src5 = "config = {\"servers\": [{\"host\": \"example.com\", \"port\": 8080}, {\"host\": \"example.org\", \"port\": 9090}], \"timeout\": 30}\n";
    let out = fmt(src5);
    assert!(out.lines().count() > 3, "{out}");
    assert!(out.lines().all(|l| l.chars().count() <= 88), "{out}");
    assert_eq!(fmt(&out), out);
    assert_eq!(semantic_fingerprint(&out), semantic_fingerprint(src5));
    // Lines without brackets get invisible parentheses made visible (Black).
    let long = format!("x = {}\n", "a + ".repeat(40).trim_end_matches(" + "));
    let expected = format!("x = (\n    a\n{})\n", "    + a\n".repeat(39));
    check(&long, &expected);
    // Multi-line strings don't count towards the width of their first line.
    let s = "x = call(\"\"\"\nvery long text inside a multi line string literal that goes beyond the limit for sure ok\n\"\"\")\n";
    assert_eq!(fmt(s), s);
    // Generator argument: no trailing comma.
    let g = "total = sum_of_everything(element_value_computation(item) for item in some_long_iterable_name)\n";
    check(g, "total = sum_of_everything(\n    element_value_computation(item) for item in some_long_iterable_name\n)\n");
    // Parenthesized imports explode like any other bracket; an over-long
    // import without parentheses gets them (Black).
    check(
        "from package.module import (first_name, second_name, third_name, fourth_name, fifth_name, sixth_name, seventh_name, eighth)\n",
        "from package.module import (\n    first_name,\n    second_name,\n    third_name,\n    fourth_name,\n    fifth_name,\n    sixth_name,\n    seventh_name,\n    eighth,\n)\n",
    );
    check(
        "from package.module import first_name, second_name, third_name, fourth_name, fifth_name, sixth, seventh\n",
        "from package.module import (\n    first_name,\n    second_name,\n    third_name,\n    fourth_name,\n    fifth_name,\n    sixth,\n    seventh,\n)\n",
    );
    // Bodies without commas split at delimiters: comprehension clauses,
    // then boolean operators, then arithmetic.
    check(
        "result = [item.name for item in self.items if item.enabled and item.visible and not item.deleted]\n",
        "result = [\n    item.name\n    for item in self.items\n    if item.enabled and item.visible and not item.deleted\n]\n",
    );
    check(
        "ok = check(first_condition_value and second_condition_value and third_condition_value or fallback)\n",
        "ok = check(\n    first_condition_value\n    and second_condition_value\n    and third_condition_value\n    or fallback\n)\n",
    );
    check(
        "total = compute(first_operand_value * second_operand_value + third_operand_value * fourth_operand_value)\n",
        "total = compute(\n    first_operand_value * second_operand_value\n    + third_operand_value * fourth_operand_value\n)\n",
    );
    check(
        "label = translate(\"a very long first part of the message text \" \"and the second part of the message text\")\n",
        "label = translate(\n    \"a very long first part of the message text \"\n    \"and the second part of the message text\"\n)\n",
    );
    check(
        "value = pick(first_alternative_value_here if some_condition_holds_here else second_alternative_value)\n",
        "value = pick(\n    first_alternative_value_here\n    if some_condition_holds_here\n    else second_alternative_value\n)\n",
    );
    // Keyword arguments split at their own brackets, not at `=`.
    check(
        "obj = build(name=\"value\", callback=make_callback(first_argument_value, second_argument_value, 33333))\n",
        "obj = build(\n    name=\"value\",\n    callback=make_callback(first_argument_value, second_argument_value, 33333),\n)\n",
    );
}

#[test]
fn comments_are_preserved() {
    check(
        "x = 1 # one\ny=2  #two\n#standalone\n  # indented comment\nz = 3\n",
        "x = 1  # one\ny = 2  # two\n# standalone\n# indented comment\nz = 3\n",
    );
    // Comments inside brackets keep the physical line structure.
    check(
        "x = [\n    1,  # first\n    2,\n    # about three\n    3,\n]\n",
        "x = [\n    1,  # first\n    2,\n    # about three\n    3,\n]\n",
    );
    check("call(a,   # c1\n  b)\n", "call(a, b)  # c1\n");
    // Comment after a block keeps block-relative indentation.
    check(
        "def f():\n    x = 1\n    # trailing in block\n# top level\ny = 2\n",
        "def f():\n    x = 1\n    # trailing in block\n\n\n# top level\ny = 2\n",
    );
    check(
        "#!/usr/bin/env python\n#: special\nx = 1\n",
        "#!/usr/bin/env python\n#: special\nx = 1\n",
    );
    check("if x:  # why\n    pass\n", "if x:  # why\n    pass\n");
    // Comments take the indentation of the following line.
    check(
        "def f():\n# before body\n    x = 1\n        # mis-indented\n    y = 2\n",
        "def f():\n    # before body\n    x = 1\n    # mis-indented\n    y = 2\n",
    );
    check(
        "class A:\n    '''Doc.'''\n    x = 1\n\n\n\n    def m(self): pass\n",
        "class A:\n    \"\"\"Doc.\"\"\"\n\n    x = 1\n\n    def m(self):\n        pass\n",
    );
    check("class A:\n    def m(self): pass\n    # about n\n    def n(self): pass\n", "class A:\n    def m(self):\n        pass\n\n    # about n\n    def n(self):\n        pass\n");
}

#[test]
fn fmt_off_and_skip() {
    let src = "x=1\n# fmt: off\ncustom = [\n  1,2,\n     3 ]\n# fmt: on\ny=2\nz  =  3  # fmt: skip\nw=4\n";
    let expected = "x = 1\n# fmt: off\ncustom = [\n  1,2,\n     3 ]\n# fmt: on\ny = 2\nz  =  3  # fmt: skip\nw = 4\n";
    check(src, expected);
    check(
        "def f():\n  a  =  1  # fmt: skip\n  b=2\n",
        "def f():\n    a  =  1  # fmt: skip\n    b = 2\n",
    );
    check(
        "def main():\n    # fmt: off\n    m = [\n      1,2,\n    ]\n    # fmt: on\n    return m\nx = 1\n",
        "def main():\n    # fmt: off\n    m = [\n      1,2,\n    ]\n    # fmt: on\n    return m\n\n\nx = 1\n",
    );
}

#[test]
fn syntax_errors_are_rejected() {
    assert!(matches!(
        format_source("def f(:\n", &FormatOptions::black()),
        Err(FormatError::SyntaxError(_))
    ));
    assert!(format_source("x = (1,\ny = 2\n", &FormatOptions::black()).is_err());
}

#[test]
fn format_range_returns_minimal_edit() {
    let src = "x=1\ny = 2\nz=3\n";
    // The whole file: one edit from the first to the last change.
    let edits = format_range(
        src,
        TextRange::new(TextSize::new(0), TextSize::new(src.len() as u32)),
        &FormatOptions::black(),
    )
    .unwrap();
    assert_eq!(edits.len(), 1);
    let (r, text) = &edits[0];
    let mut out = src.to_string();
    out.replace_range(usize::from(r.start())..usize::from(r.end()), text);
    assert_eq!(out, "x = 1\ny = 2\nz = 3\n");
    assert!(format_range(
        "x = 1\n",
        TextRange::new(TextSize::new(0), TextSize::new(1)),
        &FormatOptions::black()
    )
    .unwrap()
    .is_empty());
}

fn apply_format_edits(src: &str, edits: &[(TextRange, String)]) -> String {
    let mut applied = src.to_string();
    for (range, text) in edits.iter().rev() {
        applied.replace_range(usize::from(range.start())..usize::from(range.end()), text);
    }
    applied
}

/// The guard and formatter must not share the same incorrect interpretation of
/// a literal or a partial edit. These small fixtures also run through CPython.
fn assert_python_equivalent(before: &str, after: &str) {
    let script = r#"
import ast, contextlib, io, sys
before, after = sys.argv[1:]
assert ast.dump(ast.parse(before), include_attributes=False) == ast.dump(ast.parse(after), include_attributes=False)
def run(source):
    namespace, output = {}, io.StringIO()
    with contextlib.redirect_stdout(output):
        exec(compile(source, '<formatter regression>', 'exec'), namespace)
    values = {name: value for name, value in namespace.items() if name != '__builtins__' and not callable(value)}
    return output.getvalue(), values
assert run(before) == run(after)
"#;
    let result = std::process::Command::new("python3")
        .args(["-W", "ignore", "-c", script, before, after])
        .output()
        .expect("Python 3 is required for independent formatter regressions");
    assert!(
        result.status.success(),
        "CPython rejected the transformation: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn partial_ranges_preserve_complete_transformations_and_boundaries() {
    let opts = FormatOptions::black();
    let src = "x = [\n  1,\n  2\n]\nprint(x)\n";
    // A single list item used to become a deletion, silently changing [1, 2]
    // into [2]. The opening line alone used to leave unmatched brackets.
    for (start, end) in [(6, 11), (0, 6)] {
        assert!(matches!(
            format_range(
                src,
                TextRange::new(TextSize::new(start), TextSize::new(end)),
                &opts
            ),
            Err(FormatError::Unsafe(_))
        ));
    }
    let list_end = src.find("print").unwrap();
    let edits = format_range(
        src,
        TextRange::new(TextSize::new(0), TextSize::new(list_end as u32)),
        &opts,
    )
    .unwrap();
    let applied = apply_format_edits(src, &edits);
    assert_eq!(applied, "x = [1, 2]\nprint(x)\n");
    assert_python_equivalent(src, &applied);

    let adjacent = "x=1\ny=2\nz=3\n";
    for (start, end, expected) in [(0, 4, "x = 1\ny=2\nz=3\n"), (4, 8, "x=1\ny = 2\nz=3\n")] {
        let range = TextRange::new(TextSize::new(start), TextSize::new(end));
        let applied = apply_format_edits(adjacent, &format_range(adjacent, range, &opts).unwrap());
        assert_eq!(
            applied, expected,
            "the line at the selection's ending boundary is excluded"
        );
        assert_python_equivalent(adjacent, &applied);
    }
    // The complete selected statement must format independently of an
    // unselected adjacent statement that also happens to need formatting.
    let adjacent_collapse = "x = [\n  1,\n  2\n]\ny=3\n";
    for (start, end, expected) in [
        (0, 17, "x = [1, 2]\ny=3\n"),
        (17, 21, "x = [\n  1,\n  2\n]\ny = 3\n"),
    ] {
        let edits = format_range(
            adjacent_collapse,
            TextRange::new(TextSize::new(start), TextSize::new(end)),
            &opts,
        )
        .unwrap();
        let applied = apply_format_edits(adjacent_collapse, &edits);
        assert_eq!(applied, expected);
        assert_python_equivalent(adjacent_collapse, &applied);
    }
}

#[test]
fn partial_ranges_validate_the_resulting_block_indentation() {
    let src = "if True:\n  x=1\n  y=2\nprint(x,y)\n";
    let opts = FormatOptions::black();
    // The equal-length line pairing is insufficient by itself: this would
    // indent x by four spaces while leaving y at two, producing invalid Python.
    assert!(matches!(
        format_range(
            src,
            TextRange::new(TextSize::new(9), TextSize::new(15)),
            &opts
        ),
        Err(FormatError::Unsafe(_))
    ));
    let block_end = src.find("print").unwrap();
    let edits = format_range(
        src,
        TextRange::new(TextSize::new(0), TextSize::new(block_end as u32)),
        &opts,
    )
    .unwrap();
    let applied = apply_format_edits(src, &edits);
    assert_eq!(applied, "if True:\n    x = 1\n    y = 2\nprint(x,y)\n");
    assert_python_equivalent(src, &applied);
}

#[test]
fn range_edits_use_original_crlf_offsets_and_replacements() {
    let opts = FormatOptions::black();
    let src = "x=1\r\ny=2\r\nprint(x,y)\r\n";
    let whole = TextRange::new(TextSize::new(0), TextSize::new(src.len() as u32));
    let applied = apply_format_edits(src, &format_range(src, whole, &opts).unwrap());
    assert_eq!(applied, "x = 1\r\ny = 2\r\nprint(x, y)\r\n");
    assert_python_equivalent(src, &applied);
    let selected = TextRange::new(TextSize::new(5), TextSize::new(10));
    let applied = apply_format_edits(src, &format_range(src, selected, &opts).unwrap());
    assert_eq!(applied, "x=1\r\ny = 2\r\nprint(x,y)\r\n");
    assert_python_equivalent(src, &applied);
    // Both the middle of CRLF and the next line's start exclude that next line.
    for end in [4, 5] {
        let range = TextRange::new(TextSize::new(0), TextSize::new(end));
        let applied = apply_format_edits(src, &format_range(src, range, &opts).unwrap());
        assert_eq!(applied, "x = 1\r\ny=2\r\nprint(x,y)\r\n");
    }
    let unicode = "é=1\r\nb=2\r\n";
    let selected = TextRange::new(TextSize::new(6), TextSize::new(11));
    let applied = apply_format_edits(unicode, &format_range(unicode, selected, &opts).unwrap());
    assert_eq!(applied, "é=1\r\nb = 2\r\n");
    assert_python_equivalent(unicode, &applied);
    let old_mac = "x=1\ry=2\r";
    let whole = TextRange::new(TextSize::new(0), TextSize::new(old_mac.len() as u32));
    let applied = apply_format_edits(old_mac, &format_range(old_mac, whole, &opts).unwrap());
    assert_eq!(applied, "x = 1\ry = 2\r");
    assert_python_equivalent(old_mac, &applied);
}

#[test]
fn bytes_literals_preserve_cpython_values_and_literal_unicode_spelling() {
    let src = r#"values = [b'\uABCD', B'\U0001FACE', b'\N{en dash}', b'\xAB', b'\00\01\000\101\377\400\777', br'\uABCD\xAB\"', b'a\
b']
"#;
    for quote in [QuoteStyle::Double, QuoteStyle::Preserve, QuoteStyle::Single] {
        let opts = FormatOptions {
            quote,
            ..FormatOptions::black()
        };
        let out = format_source(src, &opts).unwrap();
        assert!(
            out.contains(r"\uABCD") && out.contains(r"\U0001FACE") && out.contains(r"\N{en dash}")
        );
        assert_python_equivalent(src, &out);
    }
    for (before, after) in [
        (r"b'\uABCD'", r"b'\uabcd'"),
        (r"b'\U0001FACE'", r"b'\U0001face'"),
        (r"b'\N{en dash}'", r"b'\N{EN DASH}'"),
    ] {
        let before = format!("x = {before}\n");
        let after = format!("x = {after}\n");
        assert_ne!(semantic_fingerprint(&before), semantic_fingerprint(&after));
        assert!(
            matches!(
                check_equivalent(&rustpy_syntax::parse(&before).syntax_node(), &after),
                Err(FormatError::Unsafe(_))
            ),
            "the independent safety check must catch a broken normalizer"
        );
    }
    for (before, after) in [
        (r"b'\777'", r"b'\xff'"),
        (r"b'\01'", r"b'\x01'"),
        (r"br'\xAB'", r"b'\\xAB'"),
    ] {
        let before = format!("x = {before}\n");
        let after = format!("x = {after}\n");
        assert_eq!(semantic_fingerprint(&before), semantic_fingerprint(&after));
        assert_python_equivalent(&before, &after);
    }
    // Cover every three-digit octal escape, including CPython's byte wrap,
    // against a separate hexadecimal spelling and CPython's actual values.
    let octal = (0..512)
        .map(|n| format!("b'\\{n:03o}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let hex = (0..512)
        .map(|n| format!("b'\\x{:02x}'", n & 255))
        .collect::<Vec<_>>()
        .join(", ");
    let before = format!("values = [{octal}]\n");
    let after = format!("values = [{hex}]\n");
    assert_eq!(semantic_fingerprint(&before), semantic_fingerprint(&after));
    assert_python_equivalent(&before, &after);
}

#[test]
fn idempotent_on_samples() {
    let samples = [
        "import os\n\n\ndef f(a, b=1, *args, **kwargs):\n    \"\"\"Doc.\"\"\"\n    if a and not b:\n        return [x for x in args if x]\n    try:\n        pass\n    except (ValueError, TypeError) as e:\n        raise RuntimeError(\"bad\") from e\n    return {k: v for k, v in kwargs.items()}\n",
        "class A(Base):\n    x: int = 1\n\n    @property\n    def y(self) -> int:\n        return self.x**2\n\n    async def z(self):\n        async for i in self.items():\n            yield i\n",
        "with open(\"f\") as fh, open(\"g\") as gh:\n    data = fh.read()[::2]\nprint(f\"{data!r}\", end=\"\")\nlambda: None\nglobal_var = (yield)\n",
    ];
    for s in samples {
        let once = fmt(s);
        assert_eq!(fmt(&once), once, "not idempotent for {s:?}");
        assert_eq!(semantic_fingerprint(&once), semantic_fingerprint(s));
    }
}

/// Formats the CPython standard library (when present): must not panic, must
/// preserve the program, must produce parseable output and be idempotent.
#[test]
fn stdlib_roundtrip() {
    let dir = std::path::Path::new("/usr/lib/python3.12");
    if !dir.is_dir() {
        return;
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("py"))
        .collect();
    files.sort();
    let opts = FormatOptions::black();
    let mut formatted = 0;
    let mut failures = Vec::new();
    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let out = match format_source(&src, &opts) {
            Ok(o) => o,
            Err(FormatError::SyntaxError(_)) => continue,
            Err(e @ FormatError::Unsafe(_)) => {
                failures.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        formatted += 1;
        if semantic_fingerprint(&out) != semantic_fingerprint(&src) {
            failures.push(format!("{}: program changed", path.display()));
            continue;
        }
        let reparse = rustpy_syntax::parse(&out);
        if let Some(e) = reparse.errors().first() {
            let line = out[..usize::from(e.range.start())].matches('\n').count() + 1;
            failures.push(format!(
                "{}: output does not parse (line {line}: {})",
                path.display(),
                e.message
            ));
            continue;
        }
        let again = format_source(&out, &opts).unwrap_or_default();
        if again != out {
            let first_diff = out
                .lines()
                .zip(again.lines())
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| format!("line {}: {a:?} vs {b:?}", i + 1))
                .unwrap_or_default();
            failures.push(format!("{}: not idempotent ({first_diff})", path.display()));
        }
    }
    assert!(
        formatted > 100,
        "expected to format the stdlib, got {formatted}"
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------------
// Black 26 behaviours (expected outputs generated with `black -q -`)

/// Blank lines: one after imports, two around top-level defs, one between methods.
#[test]
fn black_blank_lines_imports_and_defs() {
    check(
        "import os\nimport sys\nx = 1\ndef f():\n    pass\nclass A:\n    a = 1\n    def m(self):\n        pass\n    def n(self):\n        pass\ny = 2\n",
        "import os\nimport sys\n\nx = 1\n\n\ndef f():\n    pass\n\n\nclass A:\n    a = 1\n\n    def m(self):\n        pass\n\n    def n(self):\n        pass\n\n\ny = 2\n",
    );
    check(
        "import os\ndef f():\n    import sys\n    x = 1\n",
        "import os\n\n\ndef f():\n    import sys\n\n    x = 1\n",
    );
    check(
        "try:\n    import a\nexcept ImportError:\n    a = None\nelse:\n    pass\nx = 1\n",
        "try:\n    import a\nexcept ImportError:\n    a = None\nelse:\n    pass\nx = 1\n",
    );
}

/// User blank lines are kept but capped (2 at top level, 1 inside blocks); none at file start.
#[test]
fn black_blank_lines_user_counts_capped() {
    check(
        "\n\n\nx = 1\n\n\n\n\ny = 2\ndef f():\n\n\n\n    a = 1\n\n\n\n    b = 2\n",
        "x = 1\n\n\ny = 2\n\n\ndef f():\n\n    a = 1\n\n    b = 2\n",
    );
    check(
        "if x:\n\n    y = 1\nwhile z:\n\n\n    pass\n",
        "if x:\n\n    y = 1\nwhile z:\n\n    pass\n",
    );
    check("def f():\n\n    x = 1\n", "def f():\n\n    x = 1\n");
}

/// Module docstrings get one blank line after them; class docstrings too; function docstrings none before.
#[test]
fn black_blank_lines_docstrings() {
    check(
        "\"\"\"Module.\"\"\"\nimport os\n",
        "\"\"\"Module.\"\"\"\n\nimport os\n",
    );
    check(
        "\"\"\"Module.\"\"\"\n\n\n\nx = 1\n",
        "\"\"\"Module.\"\"\"\n\nx = 1\n",
    );
    check(
        "#!/usr/bin/env python\n\"\"\"Module.\"\"\"\nx = 1\n",
        "#!/usr/bin/env python\n\"\"\"Module.\"\"\"\n\nx = 1\n",
    );
    check(
        "class A:\n\n    \"\"\"Doc.\"\"\"\n    x = 1\n",
        "class A:\n    \"\"\"Doc.\"\"\"\n\n    x = 1\n",
    );
    check(
        "def f():\n\n    \"\"\"Doc.\"\"\"\n\n    return 1\n",
        "def f():\n    \"\"\"Doc.\"\"\"\n\n    return 1\n",
    );
    check(
        "class A:\n    \"\"\"Doc.\"\"\"\n    def f(self):\n        pass\n",
        "class A:\n    \"\"\"Doc.\"\"\"\n\n    def f(self):\n        pass\n",
    );
}

/// Comments directly before a def/class belong to it: the blank lines go before the comment block.
#[test]
fn black_blank_lines_comments_before_defs() {
    check(
        "x = 1\n# about f\n# more\ndef f():\n    pass\n",
        "x = 1\n\n\n# about f\n# more\ndef f():\n    pass\n",
    );
    check(
        "x = 1\n\n# unrelated\n\ny = 2\n# about g\ndef g():\n    pass\n",
        "x = 1\n\n# unrelated\n\ny = 2\n\n\n# about g\ndef g():\n    pass\n",
    );
    check(
        "class A:\n    x = 1\n    # about m\n    def m(self):\n        pass\n",
        "class A:\n    x = 1\n\n    # about m\n    def m(self):\n        pass\n",
    );
    check(
        "@decorator\n# comment\ndef f():\n    pass\n",
        "@decorator\n# comment\ndef f():\n    pass\n",
    );
    check(
        "def f():\n    pass\n# trailing comment\nx = 1\n",
        "def f():\n    pass\n\n\n# trailing comment\nx = 1\n",
    );
}

/// Nested defs get one blank line; stub defs without user blank lines stay together (overloads).
#[test]
fn black_blank_lines_nested_and_overloads() {
    check(
        "def f():\n    x = 1\n    def g():\n        pass\n    return g\n",
        "def f():\n    x = 1\n\n    def g():\n        pass\n\n    return g\n",
    );
    check(
        "@overload\ndef f(x: int) -> int: ...\n@overload\ndef f(x: str) -> str: ...\ndef f(x):\n    return x\n",
        "@overload\ndef f(x: int) -> int: ...\n@overload\ndef f(x: str) -> str: ...\ndef f(x):\n    return x\n",
    );
    check(
        "class P(Protocol):\n    def a(self): ...\n    def b(self): ...\n\n    def c(self): ...\n",
        "class P(Protocol):\n    def a(self): ...\n    def b(self): ...\n\n    def c(self): ...\n",
    );
    check(
        "if x:\n    def f(): pass\nelse:\n    def f(): pass\ny = 1\n",
        "if x:\n\n    def f():\n        pass\n\nelse:\n\n    def f():\n        pass\n\n\ny = 1\n",
    );
    check(
        "def f():\n    if x:\n        def g(): pass\n    y = 1\n",
        "def f():\n    if x:\n\n        def g():\n            pass\n\n    y = 1\n",
    );
}

/// Decorators stay attached; dependent clauses after an indented def get one blank line.
#[test]
fn black_blank_lines_decorators_and_flow() {
    check(
        "@a\n\n@b\n\ndef f():\n    pass\n",
        "@a\n@b\ndef f():\n    pass\n",
    );
    check(
        "try:\n    def f():\n        pass\nexcept E:\n    pass\n",
        "try:\n\n    def f():\n        pass\n\nexcept E:\n    pass\n",
    );
    check(
        "x = 1\n@property\ndef f():\n    pass\n",
        "x = 1\n\n\n@property\ndef f():\n    pass\n",
    );
    check(
        "if TYPE_CHECKING:\n    import os\n    def f(): pass\n",
        "if TYPE_CHECKING:\n    import os\n\n    def f():\n        pass\n",
    );
}

/// Whitespace: binary operators, keyword arguments, annotations with defaults, unary operators, star args.
#[test]
fn black_whitespace_operators_and_calls() {
    check(
        "x=1+2*3-4/5//6%7@m\ny = - x\nz = not(x)\nf(a , b = 1 , * args , ** kw)\n",
        "x = 1 + 2 * 3 - 4 / 5 // 6 % 7 @ m\ny = -x\nz = not (x)\nf(a, b=1, *args, **kw)\n",
    );
    check(
        "def f(a,b:int=1,*args:str,c=2,**kw)->int:pass\n",
        "def f(a, b: int = 1, *args: str, c=2, **kw) -> int:\n    pass\n",
    );
    check(
        "lambda x,y=1,*a,**k:x\nprint(*a, ** k)\n{** a, 'b': 1}\n[* a, * b]\n",
        "lambda x, y=1, *a, **k: x\nprint(*a, **k)\n{**a, \"b\": 1}\n[*a, *b]\n",
    );
    check(
        "x[a+1 :]\nx[1:2]\nx[::2]\nx[fn(a) : fn(b)]\nx[a.b : c.d]\nx[-1:]\nx[:-1]\nx[a, b]\n",
        "x[a + 1 :]\nx[1:2]\nx[::2]\nx[fn(a) : fn(b)]\nx[a.b : c.d]\nx[-1:]\nx[:-1]\nx[a, b]\n",
    );
    check(
        "x = a if b else c\ny = a<b<=c!=d\nz = a is not b and c not in d\n",
        "x = a if b else c\ny = a < b <= c != d\nz = a is not b and c not in d\n",
    );
    check(
        "print(a.b.c(d)[e]( f ))\n@ decorator\ndef f(): pass\n",
        "print(a.b.c(d)[e](f))\n\n\n@decorator\ndef f():\n    pass\n",
    );
}

/// Whitespace in imports, dotted names, except*, print chevron-free calls and walrus.
#[test]
fn black_whitespace_imports_and_misc() {
    check(
        "from . import x\nfrom .. m import y\nfrom ...a.b import (c)\nimport a . b as c\n",
        "from . import x\nfrom ..m import y\nfrom ...a.b import c\nimport a.b as c\n",
    );
    check(
        "try:\n    pass\nexcept* ValueError as e:\n    pass\n",
        "try:\n    pass\nexcept* ValueError as e:\n    pass\n",
    );
    check(
        "if (n:=len(a)) > 10: pass\nprint(x:=1)\n",
        "if (n := len(a)) > 10:\n    pass\nprint(x := 1)\n",
    );
    check(
        "def f[T: int, *Ts, **P](x: T) -> T: ...\ntype X[T] = list[T]\n",
        "def f[T: int, *Ts, **P](x: T) -> T: ...\n\n\ntype X[T] = list[T]\n",
    );
}

/// `**` hugs simple operands (names, numbers, attribute chains, unary minus) only.
#[test]
fn black_power_operator_hugging() {
    check(
        "x = a**2 + b ** -c + d.e ** f.g\ny = f(x) ** 2\nz = 5 ** f(x)\nw = (-1) ** n\nv = x[1] ** 2\nu = -x ** 2\n",
        "x = a**2 + b**-c + d.e**f.g\ny = f(x) ** 2\nz = 5 ** f(x)\nw = (-1) ** n\nv = x[1] ** 2\nu = -(x**2)\n",
    );
    check(
        "i = i ** -1\nj = j ** k ** l\nk = lambda x: x ** 2\n",
        "i = i**-1\nj = j**k**l\nk = lambda x: x**2\n",
    );
}

/// `-x ** y` gets explicit parentheses; number literals before an attribute are parenthesized.
#[test]
fn black_unary_power_and_number_attributes() {
    check(
        "x = -2 ** 8\ny = +a ** b\nprint(1 .real, 1.5 .real, 0x1F .real, 1j .real, 1e5 .imag)\n",
        "x = -(2**8)\ny = +(a**b)\nprint((1).real, (1.5).real, 0x1F.real, 1j.real, (1e5).imag)\n",
    );
}

/// Numeric literals: lowercase prefixes/exponents/suffixes, uppercase hex digits, `1.` and `.5` completed.
#[test]
fn black_numeric_literals() {
    check(
        "x = 0XABcd + 0O17 + 0B101 + 1E5 + 1E+5 + 1e-5 + 10J + 1.5J + 1. + .5 + 1_000E1_0\n",
        "x = 0xABCD + 0o17 + 0b101 + 1e5 + 1e5 + 1e-5 + 10j + 1.5j + 1.0 + 0.5 + 1_000e1_0\n",
    );
}

/// String prefixes are lowercased (except R), `u` dropped; double quotes preferred unless they need more escapes.
#[test]
fn black_string_prefix_and_quotes() {
    check(
        "a = 'x'\nb = 'it\\'s'\nc = 'say \"hi\"'\nd = r'\\d'\ne = B'bytes'\nf = U'u'\ng = F'{x}'\nh = Rb'raw'\ni = bR'raw'\nj = ''\nk = \"\\'\"\n",
        "a = \"x\"\nb = \"it's\"\nc = 'say \"hi\"'\nd = r\"\\d\"\ne = b\"bytes\"\nf = \"u\"\ng = f\"{x}\"\nh = Rb\"raw\"\ni = Rb\"raw\"\nj = \"\"\nk = \"'\"\n",
    );
    check(
        "a = '''x'''\nb = '''say \"hi\"'''\nc = 'a\\\\'\nd = 'mixed \\' and \"'\ne = f'{x!r}'\nf = f'{x[\"k\"]}'\ng = rb'\\x'\n",
        "a = \"\"\"x\"\"\"\nb = '''say \"hi\"'''\nc = \"a\\\\\"\nd = \"mixed ' and \\\"\"\ne = f\"{x!r}\"\nf = f'{x[\"k\"]}'\ng = rb\"\\x\"\n",
    );
}

/// Escape sequences: lowercase hex digits, uppercase `\N{...}` names; raw strings untouched.
#[test]
fn black_unicode_escapes() {
    check(
        "x = '\\xAB\\uABCD\\U0001F600\\N{en dash}'\ny = r'\\xAB'\nz = b'\\xAB'\nw = '\\\\xAB'\n",
        "x = \"\\xab\\uabcd\\U0001f600\\N{EN DASH}\"\ny = r\"\\xAB\"\nz = b\"\\xab\"\nw = \"\\\\xAB\"\n",
    );
}

/// Docstrings: re-indented, trailing whitespace and surrounding blank lines stripped, quotes normalized.
#[test]
fn black_docstrings() {
    check(
        "def f():\n    '''   Doc.   '''\n",
        "def f():\n    \"\"\"Doc.\"\"\"\n",
    );
    check(
        "def f():\n    \"\"\"\n       Indented.\n         More.\n    \"\"\"\n",
        "def f():\n    \"\"\"\n    Indented.\n      More.\n    \"\"\"\n",
    );
    check(
        "class A:\n    '''Doc ending with quote\"'''\n",
        "class A:\n    '''Doc ending with quote\"'''\n",
    );
    check(
        "def f():\n    \"\"\"Ends with backslash\\\\\"\"\"\n",
        "def f():\n    \"\"\"Ends with backslash\\\\\"\"\"\n",
    );
    check(
        "def f():\n\t\"\"\"Tabbed.\n\n\tSecond.\n\t\"\"\"\n",
        "def f():\n    \"\"\"Tabbed.\n\n    Second.\n    \"\"\"\n",
    );
    check(
        "def f():\n    \"\"\"\"\"\"\n",
        "def f():\n    \"\"\"\"\"\"\n",
    );
    check("def f():\n    ''\n", "def f():\n    \"\"\"\"\"\"\n");
    check(
        "def f():\n    r'''Raw \\d docstring.'''\n",
        "def f():\n    r\"\"\"Raw \\d docstring.\"\"\"\n",
    );
    check(
        "def f():\n    \"\"\"Docstring with a very long last line that ends right at the limit of the line length xxx.\n    \"\"\"\n",
        "def f():\n    \"\"\"Docstring with a very long last line that ends right at the limit of the line length xxx.\"\"\"\n",
    );
    check(
        "def f():\n    \"\"\"First line.\n    Last line that is long enough to push the closing quotes over the limit abcde.\"\"\"\n",
        "def f():\n    \"\"\"First line.\n    Last line that is long enough to push the closing quotes over the limit abcde.\"\"\"\n",
    );
    check(
        "if x:\n    'block docstring'\n",
        "if x:\n    \"block docstring\"\n",
    );
}

/// Comments: `#x` gets a space, shebang/`#!`/`#:` kept, type comments normalized, trailing comment spacing.
#[test]
fn black_comments() {
    check(
        "#!/usr/bin/env python\n#comment\n#!not shebang\n#:special\nx = 1 #trailing\ny = 2  #type:int\n",
        "#!/usr/bin/env python\n# comment\n#!not shebang\n#:special\nx = 1  # trailing\ny = 2  # type: int\n",
    );
    check(
        "def f():\n    pass\n    # in block\n  # dedented\n# top\nx = 1\n",
        "def f():\n    pass\n    # in block\n\n\n# dedented\n# top\nx = 1\n",
    );
    check(
        "if x:\n    pass\n    # c1\n\n    # c2\nelse:\n    pass\n",
        "if x:\n    pass\n    # c1\n\n    # c2\nelse:\n    pass\n",
    );
}

/// Comments inside brackets: trailing ones move behind the joined line, standalone ones force a split.
#[test]
fn black_comments_inside_brackets() {
    check("call(a,   # c1\n  b)\n", "call(a, b)  # c1\n");
    check(
        "x = [  # opening\n    1, 2,\n]\n",
        "x = [  # opening\n    1,\n    2,\n]\n",
    );
    check(
        "x = [\n    1,\n    # standalone\n    2\n]\n",
        "x = [\n    1,\n    # standalone\n    2,\n]\n",
    );
    check(
        "foo(**{'a': 1,  # one\n       'b': 2})\n",
        "foo(**{\"a\": 1, \"b\": 2})  # one\n",
    );
}

/// Redundant parentheses are removed after statement keywords and `=`; visible ones kept where needed.
#[test]
fn black_redundant_parentheses() {
    check(
        "x = (1)\nreturn_value = (yield)\nfor (a, b) in (c):\n    pass\nif (x):\n    pass\nwhile (y): pass\n",
        "x = 1\nreturn_value = yield\nfor a, b in c:\n    pass\nif x:\n    pass\nwhile y:\n    pass\n",
    );
    check(
        "def f():\n    return (x)\n\n\nprint((1))\ndel (a, b)\ndel (a), (b)\nassert (x, y)\n",
        "def f():\n    return x\n\n\nprint((1))\ndel (a, b)\ndel (a), (b)\nassert (x, y)\n",
    );
    check(
        "with (open(f)) as g:\n    pass\nx = (  # comment\n    1\n)\n",
        "with open(f) as g:\n    pass\nx = 1  # comment\n",
    );
    check(
        "async def f():\n    await (x)\n    await (a ** b)\n    await (a.b())\n",
        "async def f():\n    await x\n    await (a**b)\n    await a.b()\n",
    );
    check(
        "class A():\n    pass\nclass B(object):\n    pass\n",
        "class A:\n    pass\n\n\nclass B(object):\n    pass\n",
    );
}

/// One-tuples keep their comma; magic trailing commas explode brackets; single-element subscripts do not.
#[test]
fn black_one_tuples_and_magic_commas() {
    check(
        "t = (1,)\nfor x in 1,:\n    pass\nv = d[1,]\nw = d[1, 2,]\n",
        "t = (1,)\nfor x in (1,):\n    pass\nv = d[1,]\nw = d[\n    1,\n    2,\n]\n",
    );
    check(
        "x = [1, 2, 3,]\ny = {'a': 1,}\nf(a,)\ndef g(a, b,): pass\n",
        "x = [\n    1,\n    2,\n    3,\n]\ny = {\n    \"a\": 1,\n}\nf(\n    a,\n)\n\n\ndef g(\n    a,\n    b,\n):\n    pass\n",
    );
    check(
        "f(a, [1, 2,])\n",
        "f(\n    a,\n    [\n        1,\n        2,\n    ],\n)\n",
    );
    check(
        "from a import (b,)\nfrom a import b,c\n",
        "from a import (\n    b,\n)\nfrom a import b, c\n",
    );
}

/// Over-long lines split at the last opening bracket; the body is one line when it fits.
#[test]
fn black_split_right_hand() {
    check(
        "result = some_function_name(argument_number_one, argument_number_two, argument_number_three)\n",
        "result = some_function_name(\n    argument_number_one, argument_number_two, argument_number_three\n)\n",
    );
    check(
        "value = compute_something_rather_long(alpha_parameter, beta_parameter, gamma_parameter, delta)\n",
        "value = compute_something_rather_long(\n    alpha_parameter, beta_parameter, gamma_parameter, delta\n)\n",
    );
    check(
        "result = session.query(models.Customer.id).filter(models.Customer.account_id == 10000, models.Customer.email == email_address,).count()\n",
        "result = (\n    session.query(models.Customer.id)\n    .filter(\n        models.Customer.account_id == 10000,\n        models.Customer.email == email_address,\n    )\n    .count()\n)\n",
    );
    check(
        "zero(one,).two(three,).four(five,)\n",
        "zero(\n    one,\n).two(\n    three,\n).four(\n    five,\n)\n",
    );
    check(
        "assert some_type_of_boolean_expression, \"followed by a really really really long string that is used to explain the assertion error message\"\n",
        "assert (\n    some_type_of_boolean_expression\n), \"followed by a really really really long string that is used to explain the assertion error message\"\n",
    );
}

/// Bracket contents split at the loosest delimiter: commas (adding a trailing comma), comprehensions, boolean, arithmetic.
#[test]
fn black_split_delimiters() {
    check(
        "result = [item.name for item in self.items if item.enabled and item.visible and not item.deleted]\n",
        "result = [\n    item.name\n    for item in self.items\n    if item.enabled and item.visible and not item.deleted\n]\n",
    );
    check(
        "ok = check(first_condition_value and second_condition_value and third_condition_value or fallback)\n",
        "ok = check(\n    first_condition_value\n    and second_condition_value\n    and third_condition_value\n    or fallback\n)\n",
    );
    check(
        "total = compute(first_operand_value * second_operand_value + third_operand_value * fourth_operand_value)\n",
        "total = compute(\n    first_operand_value * second_operand_value\n    + third_operand_value * fourth_operand_value\n)\n",
    );
    check(
        "label = translate(\"a very long first part of the message text \" \"and the second part of the message text\")\n",
        "label = translate(\n    \"a very long first part of the message text \"\n    \"and the second part of the message text\"\n)\n",
    );
    check(
        "value = pick(first_alternative_value_here if some_condition_holds_here else second_alternative_value)\n",
        "value = pick(\n    first_alternative_value_here\n    if some_condition_holds_here\n    else second_alternative_value\n)\n",
    );
    check(
        "obj = build(name=\"value\", callback=make_callback(first_argument_value, second_argument_value, 33333))\n",
        "obj = build(\n    name=\"value\",\n    callback=make_callback(first_argument_value, second_argument_value, 33333),\n)\n",
    );
    check(
        "parser.add_argument(\"--flag\", action=\"store_true\", default=False, help=\"some help text that is long\")\n",
        "parser.add_argument(\n    \"--flag\", action=\"store_true\", default=False, help=\"some help text that is long\"\n)\n",
    );
}

/// Long right-hand sides and conditions get invisible parentheses made visible when that is the only way to fit.
#[test]
fn black_split_optional_parentheses() {
    check(
        "some_variable_name = first_operand_value_long + second_operand_value_long + third_operand_x\n",
        "some_variable_name = (\n    first_operand_value_long + second_operand_value_long + third_operand_x\n)\n",
    );
    check(
        "def f():\n    return first_condition_value_long and second_condition_value_long or third_value\n",
        "def f():\n    return first_condition_value_long and second_condition_value_long or third_value\n",
    );
    check(
        "if first_condition_value_long and second_condition_value_long or third_condition_value_x:\n    pass\n",
        "if (\n    first_condition_value_long\n    and second_condition_value_long\n    or third_condition_value_x\n):\n    pass\n",
    );
    check(
        "this_is_a_very_long_variable_name_for_testing = some_object.some_method_name(argument)\n",
        "this_is_a_very_long_variable_name_for_testing = some_object.some_method_name(argument)\n",
    );
    check(
        "x = aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        "x = aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
    check(
        "for element_with_long_name in collection_with_a_long_name.method_call(argument_value_long):\n    pass\n",
        "for element_with_long_name in collection_with_a_long_name.method_call(\n    argument_value_long\n):\n    pass\n",
    );
}

/// Dots after calls are delimiters; subscripts and attribute chains split at their brackets.
#[test]
fn black_split_method_chains_and_subscripts() {
    check(
        "result = some_object.method_one(argument_one).method_two(argument_two).method_three(argument_three_x)\n",
        "result = (\n    some_object.method_one(argument_one)\n    .method_two(argument_two)\n    .method_three(argument_three_x)\n)\n",
    );
    check(
        "value = dictionary_with_long_name[\"first_key_name\"][\"second_key_name\"][\"third_key_name_x\"]\n",
        "value = dictionary_with_long_name[\"first_key_name\"][\"second_key_name\"][\n    \"third_key_name_x\"\n]\n",
    );
    check(
        "x = a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.t.u.v.w.x.y.z.a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p\n",
        "x = a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.t.u.v.w.x.y.z.a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p\n",
    );
}

/// Function definitions split their parameters (left-hand split), adding a trailing comma when exploded.
#[test]
fn black_split_defs() {
    check(
        "def function_with_many_parameters(self, first_parameter, second_parameter, third_parameter=None):\n    pass\n",
        "def function_with_many_parameters(\n    self, first_parameter, second_parameter, third_parameter=None\n):\n    pass\n",
    );
    check(
        "def function_with_many_parameters(self, first_parameter, second_parameter, third_parameter=None, fourth=1) -> int:\n    pass\n",
        "def function_with_many_parameters(\n    self, first_parameter, second_parameter, third_parameter=None, fourth=1\n) -> int:\n    pass\n",
    );
    check(
        "def f(a,) -> Set[\"Muy\", \"Long\", \"Annotation\", \"That\", \"Does\", \"Not\", \"Fit\", \"On\", \"One\", \"Line\"]:\n    pass\n",
        "def f(\n    a,\n) -> Set[\n    \"Muy\", \"Long\", \"Annotation\", \"That\", \"Does\", \"Not\", \"Fit\", \"On\", \"One\", \"Line\"\n]:\n    pass\n",
    );
    check(
        "def xxxxxxxxxxxxxxxxxxxxxxxxxxxx() -> Set[\"Muy\", \"Long\", \"Annotation\", \"That\", \"Does\", \"Not\", \"Fit\"]:\n    pass\n",
        "def xxxxxxxxxxxxxxxxxxxxxxxxxxxx() -> (\n    Set[\"Muy\", \"Long\", \"Annotation\", \"That\", \"Does\", \"Not\", \"Fit\"]\n):\n    pass\n",
    );
    check(
        "class SomeClassName(FirstBaseClassName, SecondBaseClassName, ThirdBaseClassName, Fourth):\n    pass\n",
        "class SomeClassName(\n    FirstBaseClassName, SecondBaseClassName, ThirdBaseClassName, Fourth\n):\n    pass\n",
    );
}

/// Nested brackets explode recursively, inner ones only when they do not fit.
#[test]
fn black_split_nested_brackets() {
    check(
        "config = {\"servers\": [{\"host\": \"example.com\", \"port\": 8080}, {\"host\": \"example.org\", \"port\": 9090}], \"timeout\": 30}\n",
        "config = {\n    \"servers\": [\n        {\"host\": \"example.com\", \"port\": 8080},\n        {\"host\": \"example.org\", \"port\": 9090},\n    ],\n    \"timeout\": 30,\n}\n",
    );
    check(
        "call(arg)(another_argument_value)(yet_another_argument_value)(and_the_last_argument_value_x)\n",
        "call(arg)(another_argument_value)(yet_another_argument_value)(\n    and_the_last_argument_value_x\n)\n",
    );
    check(
        "foo(bar(baz(qux(quux(corge(grault(garply(waldo(fred(plugh(xyzzy(thud(aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)))))))))))))\n",
        "foo(\n    bar(\n        baz(\n            qux(\n                quux(\n                    corge(\n                        grault(\n                            garply(\n                                waldo(\n                                    fred(\n                                        plugh(\n                                            xyzzy(thud(aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa))\n                                        )\n                                    )\n                                )\n                            )\n                        )\n                    )\n                )\n            )\n        )\n    )\n)\n",
    );
}

/// A trailing comma after `*args`/`**kwargs` is only added when the file already needs Python 3.6+ (here: an f-string).
#[test]
fn black_split_trailing_comma_target_versions() {
    check(
        "function_name_long(argument_one, argument_two, argument_three, *args_long_name, **kwargs_long)\n",
        "function_name_long(\n    argument_one, argument_two, argument_three, *args_long_name, **kwargs_long\n)\n",
    );
    check(
        "function_name_long(argument_one, argument_two, argument_three, *args_long_name, **kwargs_long)\nf'{x}'\n",
        "function_name_long(\n    argument_one, argument_two, argument_three, *args_long_name, **kwargs_long\n)\nf\"{x}\"\n",
    );
}

/// Long `with` statements: parenthesized context managers only when the target versions allow them.
#[test]
fn black_split_with_statements() {
    check(
        "with make_context_manager(1) as cm1, make_context_manager(2) as cm2, make_context_manager(3) as cm3:\n    pass\n",
        "with make_context_manager(1) as cm1, make_context_manager(\n    2\n) as cm2, make_context_manager(3) as cm3:\n    pass\n",
    );
    check(
        "with make_context_manager(1) as cm1, make_context_manager(2) as cm2, make_context_manager(3) as cm3:\n    pass\nmatch x:\n    case 1:\n        pass\n",
        "with (\n    make_context_manager(1) as cm1,\n    make_context_manager(2) as cm2,\n    make_context_manager(3) as cm3,\n):\n    pass\nmatch x:\n    case 1:\n        pass\n",
    );
}

/// Multi-line strings: calls with a single triple-quoted argument hug it; others split.
#[test]
fn black_multiline_strings() {
    check(
        "x = call(\"\"\"\nvery long text inside a multi line string literal that goes beyond the limit for sure ok\n\"\"\")\n",
        "x = call(\"\"\"\nvery long text inside a multi line string literal that goes beyond the limit for sure ok\n\"\"\")\n",
    );
    check(
        "parser.usage += textwrap.dedent(\"\"\"\n    text\n\"\"\") + other\n",
        "parser.usage += textwrap.dedent(\"\"\"\n    text\n\"\"\") + other\n",
    );
    check(
        "call(arg1, arg2, \"\"\"\nshort\n\"\"\", arg3=True)\n",
        "call(\n    arg1,\n    arg2,\n    \"\"\"\nshort\n\"\"\",\n    arg3=True,\n)\n",
    );
}

/// Backslash continuations are joined; semicolon-separated statements go on their own lines.
#[test]
fn black_backslashes_and_semicolons() {
    check(
        "x = 1 + \\\n    2\nif a and \\\n   b:\n    pass\n",
        "x = 1 + 2\nif a and b:\n    pass\n",
    );
    check(
        "a = 1; b = 2;\nif x: y = 1; z = 2\n",
        "a = 1\nb = 2\nif x:\n    y = 1\n    z = 2\n",
    );
    check(
        "assert some_type_of_boolean_expression, \\\n    'message'\n",
        "assert some_type_of_boolean_expression, \"message\"\n",
    );
}

/// A line holding only a backslash continuation (one that follows a newline
/// rather than code) is just a line break to Black's tokenizer: it counts as a
/// blank line, makes a following comment standalone, and disappears from
/// `# fmt: off` regions.
#[test]
fn black_backslash_only_lines() {
    check(
        "def bobtwo(): \\\n    \\\n  # some comment here\n    pass\n",
        "def bobtwo():\n\n    # some comment here\n    pass\n",
    );
    check(
        "#\n\n    \x0c    \n#\n\n\x0c\\\n#\npass\n",
        "#\n\n\x0c\n#\n\n\n#\npass\n",
    );
    check("x = [1,  # c\n     \\\n     2]\n", "x = [1, 2]  # c\n");
    check("x = 1\n\\\n# c\ny = 2\n", "x = 1\n\n# c\ny = 2\n");
    check("x = 1 + \\\n    \\\n  2\n", "x = 1 + 2\n");
    check(
        "def f(a,):\n    return a\n    \\\n# c\nx = 1\n",
        "def f(\n    a,\n):\n    return a\n\n\n# c\nx = 1\n",
    );
    check(
        "# fmt: off\nx = [1,\n     \\\n     2]\n# fmt: on\ny = 1\n",
        "# fmt: off\nx = [1,\n\n     2]\n# fmt: on\ny = 1\n",
    );
    check(
        "if x:\n    pass\n\\\n\n# c\nelse:\n    pass\n",
        "if x:\n    pass\n\n\n# c\nelse:\n    pass\n",
    );
}

/// Conditional expressions get invisible parentheses so long ones split inside them.
#[test]
fn black_conditional_expression_parentheses() {
    check(
        "value = some_function_name(argument_one) if some_condition_is_true_here else other_value_x\n",
        "value = (\n    some_function_name(argument_one) if some_condition_is_true_here else other_value_x\n)\n",
    );
    check(
        "result = [item for item in collection_with_long_name if item is not None] if collection_long else []\n",
        "result = (\n    [item for item in collection_with_long_name if item is not None]\n    if collection_long\n    else []\n)\n",
    );
}

/// Commas in lambda parameters and comprehension targets never split a line.
#[test]
fn black_lambda_and_comprehension_targets() {
    check(
        "result = sorted(items_with_a_long_name, key=lambda item, other_param=1: (item.first, item.second, other))\n",
        "result = sorted(\n    items_with_a_long_name,\n    key=lambda item, other_param=1: (item.first, item.second, other),\n)\n",
    );
    check(
        "pairs = {key_name: value_name for key_name, value_name in dictionary_with_long_name.items() if value_name}\n",
        "pairs = {\n    key_name: value_name\n    for key_name, value_name in dictionary_with_long_name.items()\n    if value_name\n}\n",
    );
}

/// Match statements: patterns get Black spacing, long subjects and guards split.
#[test]
fn black_match_statements() {
    check(
        "match command.split():\n    case [action]:\n        pass\n    case [action, obj]:\n        pass\n    case Point(x=0,y=0):\n        pass\n    case {'x': x, **rest}:\n        pass\n    case _:\n        pass\n",
        "match command.split():\n    case [action]:\n        pass\n    case [action, obj]:\n        pass\n    case Point(x=0, y=0):\n        pass\n    case {\"x\": x, **rest}:\n        pass\n    case _:\n        pass\n",
    );
    check(
        "match x:\n    case 1|2|3 if y>0:\n        pass\n",
        "match x:\n    case 1 | 2 | 3 if y > 0:\n        pass\n",
    );
}

/// Return annotations: redundant parentheses removed, long ones split inside their brackets.
#[test]
fn black_return_annotations() {
    check(
        "def f() -> (int):\n    pass\ndef g() -> (\"str\"):\n    pass\n",
        "def f() -> int:\n    pass\n\n\ndef g() -> \"str\":\n    pass\n",
    );
    check(
        "def very_long_function_name_for_testing(argument) -> Dict[str, List[Tuple[int, int]]]:\n    pass\n",
        "def very_long_function_name_for_testing(argument) -> Dict[str, List[Tuple[int, int]]]:\n    pass\n",
    );
}

/// Empty input stays empty; whitespace-only input becomes a single newline; trailing whitespace goes.
#[test]
fn black_empty_and_whitespace_only_files() {
    check("", "");
    check("\n\n", "\n");
    check("   \n", "\n");
    check("x = 1   \ny = 2\t\n\n\n\n", "x = 1\ny = 2\n");
    check("x = 1", "x = 1\n");
}

/// Like Black's `assert_stable`, output that one more pass would change is
/// refused (Black 26 reports "produced different code on the second pass"
/// for this input), so formatting is always idempotent.
#[test]
fn black_unstable_output_is_refused() {
    let src = "class A:\n    def f(self):\n        (np.frombuffer(surface.get_data(), np.uint32)\n         .reshape((sh, sw))[sly, slx]) = region._data\n";
    assert!(
        matches!(
            format_source(src, &FormatOptions::black()),
            Err(FormatError::Unsafe(_))
        ),
        "Black refuses this file"
    );
}

/// Like Black's safety check (which parses with `type_comments=True`),
/// output that would merge a `# type: ignore` or `# type:` comment into
/// another comment is refused. Distinct physical directive comments keep
/// their statement layout; ordinary comments may still share a line.
#[test]
fn black_type_comments_are_not_merged() {
    let merges_ignores = "def f():\n    return (\n        g(a)  # type: ignore[x]\n        or (  # type: ignore[y]\n            b\n        )\n    )\n";
    assert!(
        matches!(
            format_source(merges_ignores, &FormatOptions::black()),
            Err(FormatError::Unsafe(_))
        ),
        "Black refuses this file"
    );
    check(
        "def f():\n    return (\n        g(a)  # noqa\n        or (  # pylint: disable=x\n            b\n        )\n    )\n",
        "def f():\n    return (\n        g(a)  # noqa\n        or (  # pylint: disable=x\n            b\n        )\n    )\n",
    );
    check(
        "def f():\n    return (\n        g(a)  # type: ignore[x]\n        or (  # noqa\n            b\n        )\n    )\n",
        "def f():\n    return (\n        g(a)  # type: ignore[x]\n        or (  # noqa\n            b\n        )\n    )\n",
    );
    // Python does not accept a `# type:` comment here, but its physical
    // ownership is still retained rather than merging it with another directive.
    check(
        "def f():\n    return (\n        g(a)  # type: int\n        or (  # noqa\n            b\n        )\n    )\n",
        "def f():\n    return (\n        g(a)  # type: int\n        or (  # noqa\n            b\n        )\n    )\n",
    );
    check(
        "x = [1,\n 2]  # type: List[int]\n",
        "x = [1, 2]  # type: List[int]\n",
    );
    check(
        "def f(a,  # type: int\n      b):\n    # type: (...) -> None\n    pass\n",
        "def f(\n    a,  # type: int\n    b,\n):\n    # type: (...) -> None\n    pass\n",
    );
    check(
        "x = 1  #type:ignore\ny = 2  # type:  int  \n",
        "x = 1  # type: ignore\ny = 2  # type: int\n",
    );
}

/// Where Python accepts a `# type:` comment (verdicts from CPython 3.12's
/// `ast.parse(src, type_comments=True)`): only then does the fingerprint
/// keep type information, since Black's safety check otherwise falls back to
/// an AST without type comments.
#[test]
fn type_comment_positions_match_cpython() {
    let cases: &[(&str, bool)] = &[
        ("x = 1  # type: int\n", true),
        ("x = y = 1  # type: int\n", true),
        ("x = yield  # type: int\n", true),
        ("x: int = 1  # type: int\n", false),
        ("x += 1  # type: int\n", false),
        ("f(x)  # type: int\n", false),
        ("return_ = 1; y = 2  # type: int\n", true),
        ("x = 1;  # type: int\n", false),
        ("if x: y = 1  # type: int\n", true),
        ("if x:  # type: int\n    pass\n", false),
        ("while x:  # type: int\n    pass\n", false),
        ("class A:  # type: int\n    pass\n", false),
        ("for x in y:  # type: int\n    pass\n", true),
        (
            "async def f():\n    async for x in y:  # type: int\n        pass\n",
            true,
        ),
        ("for x in y: z = 1  # type: int\n", true),
        (
            "for x in y:\n    pass\nelse:  # type: int\n    pass\n",
            false,
        ),
        ("with a as b:  # type: int\n    pass\n", true),
        ("with a as b, c as d:  # type: int\n    pass\n", true),
        ("with (a as b):  # type: int\n    pass\n", false),
        ("with (a):  # type: int\n    pass\n", true),
        ("with (a, b):  # type: int\n    pass\n", true),
        ("with (a as b, c):  # type: int\n    pass\n", false),
        (
            "async def f():\n    async with a as b:  # type: int\n        pass\n",
            true,
        ),
        ("def f():  # type: int\n    pass\n", true),
        ("async def f():  # type: int\n    pass\n", true),
        ("def f():\n    # type: int\n    pass\n", true),
        ("def f():\n    # hello\n    # type: int\n    pass\n", true),
        ("def f():\n\n    # type: int\n    pass\n", true),
        ("def f():  # type: int\n    # type: int\n    pass\n", false),
        (
            "def f():\n    # type: int\n    # type: int\n    pass\n",
            false,
        ),
        ("def f():\n    pass\n    # type: int\n", false),
        ("def f():\n    x = 1\n    # type: int\n", false),
        ("def f(): pass  # type: int\n", false),
        ("def f(): x = 1  # type: int\n", true),
        ("class A:\n    # type: int\n    pass\n", false),
        ("def f(a,  # type: int\n      b):\n    pass\n", true),
        ("def f(a  # type: int\n      ):\n    pass\n", true),
        ("def f(a,  # type: int\n      ):\n    pass\n", true),
        ("def f(  # type: int\n      a):\n    pass\n", false),
        ("def f(a=1,  # type: int\n      b=2):\n    pass\n", true),
        (
            "def f(a: int = 1,  # type: int\n      b=2):\n    pass\n",
            true,
        ),
        ("def f(*args,  # type: int\n      **kw):\n    pass\n", true),
        ("def f(*args  # type: int\n      ):\n    pass\n", true),
        ("def f(**kw  # type: int\n      ):\n    pass\n", true),
        ("def f(**kw,  # type: int\n      ):\n    pass\n", true),
        ("def f(*,  # type: int\n      a):\n    pass\n", false),
        ("def f(a, /,  # type: int\n      b):\n    pass\n", false),
        ("def f(a,  # type: int\n      /, b):\n    pass\n", true),
        ("def f(a, *,  # type: int\n      b):\n    pass\n", false),
        ("def f(a, *, b,  # type: int\n      c):\n    pass\n", true),
        ("f = (lambda a,  # type: int\n  b: 0)\n", false),
        ("x = [1,  # type: int\n  2]\n", false),
        ("x = [\n    1,\n]  # type: int\n", true),
        ("x = [  # type: int\n    1,\n]\n", false),
        ("f(a,  # type: int\n  b)\n", false),
        ("# type: int\nx = 1\n", false),
        ("x = 1\n# type: int\n", false),
        ("x = 1 + \\\n    2  # type: int\n", true),
        ("@dec  # type: int\ndef f():\n    pass\n", false),
        ("def f[T](a,  # type: int\n         b):\n    pass\n", true),
        ("def f(a) -> int:  # type: int\n    pass\n", true),
        ("try:  # type: int\n    pass\nexcept E:\n    pass\n", false),
        (
            "x = 1  # type: ignore\ny = [1,  # type: ignore\n  2]\n",
            true,
        ),
        ("x = 1  # foo  # type: int\n", true),
        ("x = 1  #type:int\n", true),
        ("x = 1  # type: ignoreme\n", true),
        ("x = 1  # type: ignore_me\n", true),
    ];
    let wrong: Vec<_> = cases
        .iter()
        .filter(|(src, cpython)| {
            let fp = semantic_fingerprint(src).unwrap_or_else(|| panic!("does not parse: {src:?}"));
            fp.iter().any(|s| s.starts_with("type-ignores:")) != *cpython
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
    // Values: ignores by count, other type comments by value (trailing
    // whitespace removed), in order.
    let fp = |s: &str| semantic_fingerprint(s).unwrap();
    assert_eq!(
        fp("x = 1  # type: ignore[a]\n"),
        fp("x = 1  #type:ignore\n")
    );
    assert_ne!(fp("x = 1  # type: ignore\n"), fp("x = 1\n"));
    assert_ne!(
        fp("x = 1  # type: ignore\n"),
        fp("x = 1  # type: ignoreme\n")
    );
    assert_eq!(fp("x = 1  # type: int  \n"), fp("x = 1  # type:int\n"));
    assert_ne!(fp("x = 1  # type: int\n"), fp("x = 1  # type: str\n"));
    assert_eq!(fp("x = 1  # noqa  # type: ignore\n"), fp("x = 1  # noqa\n"));
}

/// A UTF-8 byte-order mark is kept in front of the formatted code, like Black
/// does when it reads and writes the file as `utf-8-sig`.
#[test]
fn black_byte_order_mark() {
    check("\u{feff}x  =  1\n", "\u{feff}x = 1\n");
    check(
        "\u{feff}# c\ndef f():\n\treturn  1\n",
        "\u{feff}# c\ndef f():\n    return 1\n",
    );
    check("\u{feff}", "\u{feff}");
    check("\u{feff}\n\n", "\u{feff}\n");
    check("\u{feff}x = 1\r\ny = 2\r\n", "\u{feff}x = 1\r\ny = 2\r\n");
    assert!(semantic_fingerprint("\u{feff}x = 1\n").is_some());
    let src = "\u{feff}x  =  1\n";
    let whole = TextRange::new(TextSize::new(0), TextSize::new(src.len() as u32));
    let edits = format_range(src, whole, &FormatOptions::black()).unwrap();
    let mut applied = src.to_string();
    for (range, text) in edits.iter().rev() {
        applied.replace_range(usize::from(range.start())..usize::from(range.end()), text);
    }
    assert_eq!(applied, "\u{feff}x = 1\n");
}
/// `# fmt: off` / `# fmt: on` regions are kept verbatim (statements, decorators, comment-only blocks, `yapf: disable`).
#[test]
fn black_fmt_off_regions() {
    check(
        "x=1\n# fmt: off\ncustom = [\n  1,2,\n     3 ]\n# fmt: on\ny=2\n",
        "x = 1\n# fmt: off\ncustom = [\n  1,2,\n     3 ]\n# fmt: on\ny = 2\n",
    );
    check(
        "def main():\n    # fmt: off\n    m = [\n      1,2,\n    ]\n    # fmt: on\n    return m\nx = 1\n",
        "def main():\n    # fmt: off\n    m = [\n      1,2,\n    ]\n    # fmt: on\n    return m\n\n\nx = 1\n",
    );
    check(
        "# fmt: off\n@test([\n    1, 2,\n    3, 4,\n])\n# fmt: on\ndef test_fader(test):\n    pass\n",
        "# fmt: off\n@test([\n    1, 2,\n    3, 4,\n])\n# fmt: on\ndef test_fader(test):\n    pass\n",
    );
    check(
        "# fmt: off\n# just comments\n#   in a block\n# fmt: on\nx  = 1\n",
        "# fmt: off\n# just comments\n#   in a block\n# fmt: on\nx = 1\n",
    );
    check(
        "# yapf: disable\nx =  [1,2]\n# yapf: enable\ny =  [1,2]\n",
        "# yapf: disable\nx =  [1,2]\n# yapf: enable\ny = [1, 2]\n",
    );
    check(
        "def f():\n    # fmt: off\n    a  = 1\n    # fmt: on\n    b  = 2\n    # fmt: off\n    c  = 3\n",
        "def f():\n    # fmt: off\n    a  = 1\n    # fmt: on\n    b = 2\n    # fmt: off\n    c  = 3\n",
    );
}

/// `# fmt: off` inside brackets keeps the elements it covers verbatim.
#[test]
fn black_fmt_off_inside_brackets() {
    check(
        "def f(a,):\n    x = [\n        1, 2,\n        # fmt: off\n        3,   4,\n        # fmt: on\n        5, 6,\n    ]\n",
        "def f(\n    a,\n):\n    x = [\n        1,\n        2,\n        # fmt: off\n        3,   4,\n        # fmt: on\n        5,\n        6,\n    ]\n",
    );
    check(
        "foo(a,\n    # fmt: off\n    b,   c,\n    # fmt: on\n    d)\n",
        "foo(\n    a,\n    # fmt: off\n    b,   c,\n    # fmt: on\n    d,\n)\n",
    );
}

/// `# fmt: skip` keeps its line verbatim (statements, headers, decorators, one-line compound statements).
#[test]
fn black_fmt_skip() {
    check(
        "z  =  3  # fmt: skip\nw=4\n",
        "z  =  3  # fmt: skip\nw = 4\n",
    );
    check(
        "if x:  # fmt: skip\n    pass\nclass A :  # fmt: skip\n    x=1\n",
        "if x:  # fmt: skip\n    pass\nclass A :  # fmt: skip\n    x = 1\n",
    );
    check(
        "a,  b = 1 ,2 # fmt: skip\nc = [1,2]\n",
        "a,  b = 1 ,2 # fmt: skip\nc = [1, 2]\n",
    );
    check(
        "x = [  # fmt: skip\n    1,2,\n]\n",
        "x = [  # fmt: skip\n    1,2,\n]\n",
    );
    check(
        "if True: print(\"a\"); print(\"b\")  # fmt: skip\n",
        "if True: print(\"a\"); print(\"b\")  # fmt: skip\n",
    );
    check(
        "print( \"hello\" )  # some comment # fmt: skip\n",
        "print( \"hello\" )  # some comment # fmt: skip\n",
    );
    check(
        "@decorator(  1 )  # fmt: skip\ndef f(): pass\n",
        "@decorator(  1 )  # fmt: skip\ndef f():\n    pass\n",
    );
    check(
        "async def f():\n    async with a  as b:  # fmt: skip\n        pass\n",
        "async def f():\n    async with a  as b:  # fmt: skip\n        pass\n",
    );
}

#[test]
fn black_directive_statement_ownership_does_not_disable_surrounding_formatting() {
    check(
        "before=1\ndef f():\n    return (\n        g(a)  # type: ignore[x]\n        or (  # noqa\n            b\n        )\n    )\nafter=2\n",
        "before = 1\n\n\ndef f():\n    return (\n        g(a)  # type: ignore[x]\n        or (  # noqa\n            b\n        )\n    )\n\n\nafter = 2\n",
    );
    check(
        "def convert(  # first\n    value  # second\n):\n    return value\n",
        "def convert(value):  # first  # second\n    return value\n",
    );
}

#[test]
fn black_standalone_directives_do_not_claim_following_statement_comments() {
    check(
        "#!not shebang\nx=1 #trailing\n",
        "#!not shebang\nx = 1  # trailing\n",
    );
    check(
        "def f():\n    # noqa\n    x=1 #trailing\n",
        "def f():\n    # noqa\n    x = 1  # trailing\n",
    );
}

#[test]
fn black_comment_only_fmt_block_preserves_preceding_on_directive() {
    check(
        "# fmt: on\n# fmt: off\n #raw comment\n# fmt: on\nx=1\n",
        "# fmt: on\n# fmt: off\n #raw comment\n# fmt: on\nx = 1\n",
    );
    check(
        "def f():\n    # fmt: on\n    # fmt: off\n        #raw comment\n    # fmt: on\n    pass\n",
        "def f():\n    # fmt: on\n    # fmt: off\n        #raw comment\n    # fmt: on\n    pass\n",
    );
}
