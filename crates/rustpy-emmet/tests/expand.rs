//! End-to-end checks of the public API, one test per feature class.

use rustpy_emmet::{
    abbreviation_before, css_abbreviation_before, expand_at, expand_css, expand_css_with_caret,
    expand_html, expand_html_in, is_css_context, is_html_context, Expansion,
};

/// The expansion of `abbr` (tab indent) with `|` at the caret.
fn html(abbr: &str) -> String {
    let e = expand_html(abbr, "\t").unwrap_or_else(|| panic!("{abbr:?} did not expand"));
    with_caret(&e)
}

fn with_caret(e: &Expansion) -> String {
    let mut text = e.text.clone();
    text.insert(e.caret, '|');
    text
}

fn css(abbr: &str) -> String {
    expand_css(abbr).unwrap_or_else(|| panic!("{abbr:?} did not expand"))
}

/// Runs `expand_at` on `text` (caret marked with `|`) and returns the edited
/// buffer with the new caret marked.
fn apply(text_with_caret: &str, language: &str) -> Option<String> {
    let caret = text_with_caret.find('|').expect("caret marker");
    let text = text_with_caret.replacen('|', "", 1);
    let (range, e) = expand_at(&text, caret, language, "  ")?;
    Some(format!(
        "{}{}{}",
        &text[..range.start],
        with_caret(&e),
        &text[range.end..]
    ))
}

#[test]
fn elements_ids_and_classes() {
    assert_eq!(html("div"), "<div>|</div>");
    assert_eq!(
        html("div#main.a.b"),
        "<div id=\"main\" class=\"a b\">|</div>"
    );
    assert_eq!(
        html("section.x#y"),
        "<section class=\"x\" id=\"y\">|</section>"
    );
    assert_eq!(html(".box"), "<div class=\"box\">|</div>");
    assert_eq!(html("#app"), "<div id=\"app\">|</div>");
    assert_eq!(
        html("my-widget.btn--primary"),
        "<my-widget class=\"btn--primary\">|</my-widget>"
    );
    assert_eq!(html("div.w-1/2"), "<div class=\"w-1/2\">|</div>");
    assert_eq!(
        html(".hover:bg-black/50"),
        "<div class=\"hover:bg-black/50\">|</div>"
    );
    assert_eq!(html("br"), "<br>|");
    assert_eq!(html("div/"), "<div>|");
}

#[test]
fn attributes() {
    assert_eq!(
        html("a[href=\"x y\" title=t data-k]"),
        "<a href=\"x y\" title=\"t\" data-k=\"|\"></a>"
    );
    assert_eq!(
        html("td[colspan=2 rowspan='3']"),
        "<td colspan=\"2\" rowspan=\"3\">|</td>"
    );
    assert_eq!(html("p[title='say \"hi\"']"), "<p title='say \"hi\"'>|</p>");
    // Both quote kinds in a value: double quotes, inner ones escaped.
    assert_eq!(
        html("p[title=\"it's \\\"q\\\"\"]"),
        "<p title=\"it's &quot;q&quot;\">|</p>"
    );
    // Boolean attributes are written HTML5-style.
    assert_eq!(html("input[disabled]"), "<input type=\"|text\" disabled>");
    assert_eq!(html("div[x.]"), "<div x>|</div>");
    // A value-only attribute fills the snippet's empty attribute.
    assert_eq!(
        html("a[\"https://x.org\"]"),
        "<a href=\"https://x.org\">|</a>"
    );
    // Abbreviation attributes override snippet defaults and keep their place.
    assert_eq!(
        html("a[href=/home].nav"),
        "<a href=\"/home\" class=\"nav\">|</a>"
    );
}

#[test]
fn text_content() {
    assert_eq!(html("p{Hello world}"), "<p>Hello world</p>|");
    assert_eq!(html("a{Click me}"), "<a href=\"|\">Click me</a>");
    assert_eq!(html("p{Price: \\$5}"), "<p>Price: $5</p>|");
    // A template-literal variable is not an edit point.
    assert_eq!(html("p{Total: ${total}}"), "<p>Total: ${total}</p>|");
    // Vue-style attribute names.
    assert_eq!(
        html("button[@click=save :disabled=busy]"),
        "<button @click=\"save\" :disabled=\"busy\">|</button>"
    );
    assert_eq!(html("p{a {nested} b}"), "<p>a {nested} b</p>|");
    assert_eq!(html("{Hello}+p"), "Hello\n<p>|</p>");
    assert_eq!(html("div>{text}+p"), "<div>\n\ttext\n\t<p>|</p>\n</div>");
    assert_eq!(html("div{<p>x</p>}"), "<div>\n\t<p>x</p>\n</div>|");
}

#[test]
fn child_sibling_and_climb_up() {
    assert_eq!(html("ul>li"), "<ul>\n\t<li>|</li>\n</ul>");
    assert_eq!(html("h1+p"), "<h1>|</h1>\n<p></p>");
    assert_eq!(
        html("div>p>span^h2"),
        "<div>\n\t<p><span>|</span></p>\n\t<h2></h2>\n</div>"
    );
    assert_eq!(
        html("div>p>span^^h2"),
        "<div>\n\t<p><span>|</span></p>\n</div>\n<h2></h2>"
    );
    assert_eq!(html("div^^p"), "<div>|</div>\n<p></p>");
    assert_eq!(html("div+"), "<div>|</div>");
}

#[test]
fn multiplication_and_numbering() {
    assert_eq!(
        html("ul>li.item$*3"),
        "<ul>\n\t<li class=\"item1\">|</li>\n\t<li class=\"item2\"></li>\n\t<li class=\"item3\"></li>\n</ul>"
    );
    assert_eq!(
        html("li.x$$*2"),
        "<li class=\"x01\">|</li>\n<li class=\"x02\"></li>"
    );
    assert_eq!(
        html("li.x$@-*3"),
        "<li class=\"x3\">|</li>\n<li class=\"x2\"></li>\n<li class=\"x1\"></li>"
    );
    assert_eq!(
        html("li.x$@3*2"),
        "<li class=\"x3\">|</li>\n<li class=\"x4\"></li>"
    );
    assert_eq!(
        html("li.x$@-5*2"),
        "<li class=\"x6\">|</li>\n<li class=\"x5\"></li>"
    );
    assert_eq!(html("h$*3"), "<h1>|</h1>\n<h2></h2>\n<h3></h3>");
    assert_eq!(
        html("p[data-i=$$$]*2"),
        "<p data-i=\"001\">|</p>\n<p data-i=\"002\"></p>"
    );
    assert_eq!(
        html("ul>li*2>a{Link $}"),
        "<ul>\n\t<li><a href=\"|\">Link 1</a></li>\n\t<li><a href=\"\">Link 2</a></li>\n</ul>"
    );
    // `$@^` counts with the enclosing repeat.
    assert_eq!(
        html("ul*2>li.r$@^c$*2"),
        "<ul>\n\t<li class=\"r1c1\">|</li>\n\t<li class=\"r1c2\"></li>\n</ul>\n<ul>\n\t<li class=\"r2c1\"></li>\n\t<li class=\"r2c2\"></li>\n</ul>"
    );
}

#[test]
fn grouping() {
    assert_eq!(
        html("div>(header>ul>li*2>a)+footer>p"),
        "<div>\n\t<header>\n\t\t<ul>\n\t\t\t<li><a href=\"|\"></a></li>\n\t\t\t<li><a href=\"\"></a></li>\n\t\t</ul>\n\t</header>\n\t<footer>\n\t\t<p></p>\n\t</footer>\n</div>"
    );
    assert_eq!(
        html("(dt+dd)*2"),
        "<dt>|</dt>\n<dd></dd>\n<dt></dt>\n<dd></dd>"
    );
    assert_eq!(
        html("(li.x$)*2"),
        "<li class=\"x1\">|</li>\n<li class=\"x2\"></li>"
    );
    assert_eq!(html("(p)+(p)"), "<p>|</p>\n<p></p>");
}

#[test]
fn implicit_tag_names() {
    assert_eq!(html("ul>.a"), "<ul>\n\t<li class=\"a\">|</li>\n</ul>");
    assert_eq!(
        html("ol>.a*2"),
        "<ol>\n\t<li class=\"a\">|</li>\n\t<li class=\"a\"></li>\n</ol>"
    );
    assert_eq!(
        html("select>.o*2"),
        "<select name=\"|\" id=\"\">\n\t<option class=\"o\"></option>\n\t<option class=\"o\"></option>\n</select>"
    );
    assert_eq!(
        html("table>.row>.cell"),
        "<table>\n\t<tr class=\"row\">\n\t\t<td class=\"cell\">|</td>\n\t</tr>\n</table>"
    );
    assert_eq!(html("tr>.c"), "<tr>\n\t<td class=\"c\">|</td>\n</tr>");
    assert_eq!(
        html("a>.icon"),
        "<a href=\"|\"><span class=\"icon\"></span></a>"
    );
    assert_eq!(html("span>.x"), "<span><span class=\"x\">|</span></span>");
    assert_eq!(html("p>.x"), "<p><span class=\"x\">|</span></p>");
    assert_eq!(
        html("div>[title=t]"),
        "<div>\n\t<div title=\"t\">|</div>\n</div>"
    );
    // The insertion context decides for top-level nameless elements.
    let in_ul = expand_html_in(".item", "\t", Some("ul")).unwrap();
    assert_eq!(in_ul.text, "<li class=\"item\"></li>");
    assert_eq!(
        expand_html_in(".c", "\t", Some("TR")).unwrap().text,
        "<td class=\"c\"></td>"
    );
    assert_eq!(
        expand_html_in(".c", "\t", Some("em")).unwrap().text,
        "<span class=\"c\"></span>"
    );
    assert_eq!(
        expand_html_in(".c", "\t", None).unwrap().text,
        "<div class=\"c\"></div>"
    );
}

#[test]
fn lorem_ipsum() {
    let words = |abbr: &str| {
        expand_html(abbr, "\t")
            .unwrap()
            .text
            .split_whitespace()
            .count()
    };
    let lorem = expand_html("lorem", "\t").unwrap().text;
    assert!(
        lorem.starts_with("Lorem ipsum dolor sit amet, consectetur adipisicing elit."),
        "{lorem}"
    );
    assert_eq!(words("lorem"), 30);
    assert_eq!(words("lorem10"), 10);
    assert_eq!(words("lipsum7"), 7);
    assert_eq!(html("lorem4"), "Lorem ipsum dolor sit.|");
    assert_eq!(html("p>lorem5"), "<p>Lorem ipsum dolor sit amet.</p>|");
    // Repeated inside a list: implicit `li` items, only the first opens with "Lorem".
    let list = expand_html("ul>lorem3.item*2", "\t").unwrap().text;
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 4, "{list}");
    assert_eq!(lines[1], "\t<li class=\"item\">Lorem ipsum dolor.</li>");
    assert!(
        lines[2].starts_with("\t<li class=\"item\">") && !lines[2].contains("Lorem"),
        "{list}"
    );
    // Deterministic output.
    assert_eq!(
        expand_html("p*3>lorem", "\t"),
        expand_html("p*3>lorem", "\t")
    );
    let range = words("lorem3-6");
    assert!((3..=6).contains(&range));
}

#[test]
fn snippets() {
    let doc = html("!");
    assert_eq!(
        doc,
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n\t<meta charset=\"UTF-8\">\n\t<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n\t<title>Document</title>\n</head>\n<body>\n\t|\n</body>\n</html>"
    );
    assert_eq!(html("html:5"), doc);
    assert!(html("!>div.app").contains("<body>\n\t<div class=\"app\">|</div>\n</body>"));
    assert_eq!(
        html("link:css"),
        "<link rel=\"stylesheet\" href=\"|style.css\">"
    );
    assert_eq!(html("script:src"), "<script src=\"|\"></script>");
    assert_eq!(
        html("input:text"),
        "<input type=\"text\" name=\"|\" id=\"\">"
    );
    assert_eq!(
        html("input:checkbox"),
        "<input type=\"checkbox\" name=\"|\" id=\"\">"
    );
    assert_eq!(
        html("input:email#mail"),
        "<input type=\"email\" name=\"|\" id=\"mail\">"
    );
    assert_eq!(html("btn"), "<button>|</button>");
    assert_eq!(html("btn:s"), "<button type=\"submit\">|</button>");
    assert_eq!(html("btn:b"), "<button type=\"button\">|</button>");
    assert_eq!(html("a"), "<a href=\"|\"></a>");
    assert_eq!(
        html("a:blank"),
        "<a href=\"http://|\" target=\"_blank\" rel=\"noopener noreferrer\"></a>"
    );
    assert_eq!(html("img"), "<img src=\"|\" alt=\"\">");
    assert_eq!(
        html("textarea"),
        "<textarea name=\"|\" id=\"\" cols=\"30\" rows=\"10\"></textarea>"
    );
    assert_eq!(html("c"), "<!-- | -->");
    assert_eq!(html("ul+"), "<ul>\n\t<li>|</li>\n</ul>");
    assert_eq!(
        html("table+"),
        "<table>\n\t<tr>\n\t\t<td>|</td>\n\t</tr>\n</table>"
    );
    assert_eq!(html("bq>p"), "<blockquote>\n\t<p>|</p>\n</blockquote>");
    // A label around its control drops the empty `for`/`id`.
    assert_eq!(html("label>input"), "<label><input type=\"|text\"></label>");
    // Attributes on a document snippet go to its element, not the doctype.
    assert!(html("!.app").starts_with("<!DOCTYPE html>\n<html lang=\"en\" class=\"app\">\n"));
}

#[test]
fn snippet_children_fill_the_edit_point() {
    assert_eq!(html("c>p"), "<!-- <p>|</p> -->");
    assert_eq!(html("c{note}"), "<!-- note -->|");
    assert_eq!(html("cc:ie>p"), "<!--[if IE]><p>|</p><![endif]-->");
    assert_eq!(
        html("cc:ie>p+p"),
        "<!--[if IE]>\n<p>|</p>\n<p></p>\n<![endif]-->"
    );
    assert_eq!(
        html("div>c>p+p"),
        "<div>\n\t<!--\n\t<p>|</p>\n\t<p></p>\n\t-->\n</div>"
    );
}

#[test]
fn inline_and_block_formatting() {
    assert_eq!(html("span+em"), "<span>|</span><em></em>");
    assert_eq!(
        html("span*3"),
        "<span>|</span>\n<span></span>\n<span></span>"
    );
    assert_eq!(
        html("p>a*3"),
        "<p>\n\t<a href=\"|\"></a>\n\t<a href=\"\"></a>\n\t<a href=\"\"></a>\n</p>"
    );
    assert_eq!(html("p>span+em"), "<p><span>|</span><em></em></p>");
    assert_eq!(
        html("div>p+span"),
        "<div>\n\t<p>|</p>\n\t<span></span>\n</div>"
    );
    assert_eq!(
        html("div>span+p"),
        "<div>\n\t<span>|</span>\n\t<p></p>\n</div>"
    );
    assert_eq!(
        html("a{Click}+span"),
        "<a href=\"|\">Click</a><span></span>"
    );
}

#[test]
fn indentation_unit_and_base_indent() {
    let e = expand_html("ul>li>a", "  ").unwrap();
    assert_eq!(e.text, "<ul>\n  <li><a href=\"\"></a></li>\n</ul>");
    let rebased = e.with_base_indent("    ");
    assert_eq!(
        rebased.text,
        "<ul>\n      <li><a href=\"\"></a></li>\n    </ul>"
    );
    assert_eq!(&rebased.text[..rebased.caret], "<ul>\n      <li><a href=\"");
    assert_eq!(
        expand_html("div>p", "").unwrap().text,
        "<div>\n<p></p>\n</div>"
    );
}

#[test]
fn caret_and_edit_points() {
    let e = expand_html("a", "\t").unwrap();
    assert_eq!((e.caret, e.stops.clone()), (9, vec![9..9, 11..11]));
    let e = expand_html("link:css", "\t").unwrap();
    assert_eq!(&e.text[e.stops[0].clone()], "style");
    let e = expand_html("p{done}", "\t").unwrap();
    assert_eq!(e.caret, e.text.len());
    assert!(e.stops.is_empty());
    let e = expand_html("ul>li*3", "\t").unwrap();
    assert_eq!(e.stops.len(), 3);
    assert_eq!(&e.text[..e.caret], "<ul>\n\t<li>");
}

#[test]
fn invalid_abbreviations() {
    for bad in [
        "",
        "   ",
        "div[",
        "div]",
        "p{",
        "(div",
        "div)",
        ">p",
        "a b",
        "123",
        "9lives",
        "div.",
        "a[=x]",
        "()",
        "li**2",
        "foo!",
        "div*10001",
        "div*5000>p*5000",
    ] {
        assert_eq!(expand_html(bad, "\t"), None, "{bad:?} should not expand");
    }
}

#[test]
fn node_budget() {
    // Snippet resolution (`a` -> `a[href]`) does not count nodes twice.
    let big = expand_html("ul>li*99>a*99", "\t").unwrap();
    assert_eq!(big.text.matches("<a href=\"\"></a>").count(), 99 * 99);
    assert_eq!(big.stops.len(), 2 * 99 * 99);
    let rebased = big.with_base_indent("  ");
    assert_eq!(rebased.stops.len(), big.stops.len());
    // Every shifted stop still sits in an `href` value or an `<a>` body.
    assert!(rebased.stops.iter().all(|r| {
        let before = &rebased.text[..r.start];
        r.is_empty() && (before.ends_with("<a href=\"") || before.ends_with("<a href=\"\">"))
    }));
    assert_eq!(expand_html("ul>li*200>a*200", "\t"), None);
}

#[test]
fn css_core_vocabulary() {
    assert_eq!(css("m10"), "margin: 10px;");
    assert_eq!(css("p10-20"), "padding: 10px 20px;");
    assert_eq!(css("w100p"), "width: 100%;");
    assert_eq!(css("bd1-s-red"), "border: 1px solid red;");
    assert_eq!(css("pos:a"), "position: absolute;");
    assert_eq!(css("d:f"), "display: flex;");
    assert_eq!(css("fz14"), "font-size: 14px;");
    assert_eq!(css("c#f00"), "color: #f00;");
    assert_eq!(css("bgc"), "background-color: #fff;");
    assert_eq!(css("ta:c"), "text-align: center;");
    assert_eq!(css("m10!"), "margin: 10px !important;");
    assert_eq!(css("mt-5"), "margin-top: -5px;");
    assert_eq!(css("m10--5"), "margin: 10px -5px;");
    assert_eq!(css("m0-a"), "margin: 0 auto;");
    assert_eq!(css("m10-20-30-40"), "margin: 10px 20px 30px 40px;");
    assert_eq!(css("z10"), "z-index: 10;");
    assert_eq!(css("t0"), "top: 0;");
    assert_eq!(css("bdrs50p"), "border-radius: 50%;");
}

#[test]
fn css_units() {
    assert_eq!(css("w10e"), "width: 10em;");
    assert_eq!(css("w2x"), "width: 2ex;");
    assert_eq!(css("w1.5r"), "width: 1.5rem;");
    assert_eq!(css("h50p"), "height: 50%;");
    assert_eq!(css("h100vh"), "height: 100vh;");
    assert_eq!(css("m1.5"), "margin: 1.5em;");
    assert_eq!(css("p0-10p"), "padding: 0 10%;");
    assert_eq!(css("m10p-5p"), "margin: 10% 5%;");
    // Unitless properties keep plain numbers.
    assert_eq!(css("lh1.5"), "line-height: 1.5;");
    assert_eq!(css("op.5"), "opacity: .5;");
    assert_eq!(css("fw700"), "font-weight: 700;");
    assert_eq!(css("fx1"), "flex: 1;");
    assert_eq!(expand_css("w10abc"), None);
    assert_eq!(expand_css("w10zz"), None);
}

#[test]
fn css_colours() {
    assert_eq!(css("c#3"), "color: #333;");
    assert_eq!(css("c#fc"), "color: #fcfcfc;");
    assert_eq!(css("c#ff0000"), "color: #f00;");
    assert_eq!(css("c#AbCdEf"), "color: #abcdef;");
    assert_eq!(css("c#f00.5"), "color: rgba(255, 0, 0, 0.5);");
    assert_eq!(css("bg#t"), "background: transparent;");
    assert_eq!(css("bd2-dashed-#ccc"), "border: 2px dashed #ccc;");
    // Hex with an alpha channel stays as typed.
    assert_eq!(css("c#F008"), "color: #f008;");
    assert_eq!(css("c#00000080"), "color: #00000080;");
    assert_eq!(css("bgc:t"), "background-color: transparent;");
    assert_eq!(css("c:cc"), "color: currentColor;");
}

#[test]
fn css_keywords() {
    assert_eq!(css("pos:r"), "position: relative;");
    assert_eq!(css("pos:f"), "position: fixed;");
    assert_eq!(css("d:ib"), "display: inline-block;");
    assert_eq!(css("d:inline-block"), "display: inline-block;");
    assert_eq!(css("d:n"), "display: none;");
    assert_eq!(css("jc:sb"), "justify-content: space-between;");
    assert_eq!(css("ai:c"), "align-items: center;");
    assert_eq!(css("fw:b"), "font-weight: bold;");
    assert_eq!(css("tt:u"), "text-transform: uppercase;");
    assert_eq!(css("whs:nw"), "white-space: nowrap;");
    assert_eq!(css("bgr:nr"), "background-repeat: no-repeat;");
    assert_eq!(css("w:a"), "width: auto;");
    assert_eq!(css("ov-h"), "overflow: hidden;");
    // Keywords glued to the alias, and full property names.
    assert_eq!(css("dib"), "display: inline-block;");
    assert_eq!(css("posa"), "position: absolute;");
    assert_eq!(css("tac"), "text-align: center;");
    assert_eq!(css("width100p"), "width: 100%;");
    assert_eq!(css("border-radius5"), "border-radius: 5px;");
    assert_eq!(css("text-align-center"), "text-align: center;");
    assert_eq!(css("whsnw"), "white-space: nowrap;");
    assert_eq!(css("bgp:c"), "background-position: center;");
    assert_eq!(css("bgp:t-l"), "background-position: top left;");
    // Values that are not keywords are kept as typed.
    assert_eq!(css("c:red"), "color: red;");
    assert_eq!(css("ff:Georgia"), "font-family: Georgia;");
    // Properties without an alias, by full name.
    assert_eq!(css("place-items:c"), "place-items: center;");
    assert_eq!(css("scroll-behavior:s"), "scroll-behavior: smooth;");
    assert_eq!(css("inset0"), "inset: 0;");
}

#[test]
fn css_function_values() {
    let caret = |abbr: &str| with_caret(&expand_css_with_caret(abbr, "\t").unwrap());
    assert_eq!(caret("gtc"), "grid-template-columns: repeat(|);");
    assert_eq!(caret("gtc:m"), "grid-template-columns: minmax(|);");
    assert_eq!(caret("cnt:a"), "content: attr(|);");
    assert_eq!(caret("trf"), "transform: |;");
    assert_eq!(caret("trf:r"), "transform: rotate(|angle);");
    assert_eq!(caret("trf:tx"), "transform: translateX(|x);");
    assert_eq!(caret("trfsc"), "transform: scale(|x, y);");
    assert_eq!(css("trf:n"), "transform: none;");
    assert_eq!(
        caret("trstf:cb"),
        "transition-timing-function: cubic-bezier(|0.1, 0.7, 1.0, 0.1);"
    );
}

#[test]
fn css_defaults_and_caret() {
    let e = expand_css_with_caret("m", "\t").unwrap();
    assert_eq!(with_caret(&e), "margin: |;");
    let e = expand_css_with_caret("pos", "\t").unwrap();
    assert_eq!(e.text, "position: relative;");
    assert_eq!(&e.text[e.stops[0].clone()], "relative");
    let e = expand_css_with_caret("bd", "\t").unwrap();
    assert_eq!(e.text, "border: 1px solid #000;");
    let stops: Vec<&str> = e.stops.iter().map(|r| &e.text[r.clone()]).collect();
    assert_eq!(stops, ["1px", "solid", "#000"]);
    assert_eq!(
        with_caret(&expand_css_with_caret("m10", "\t").unwrap()),
        "margin: 10px;|"
    );
    assert_eq!(
        with_caret(&expand_css_with_caret("p!", "\t").unwrap()),
        "padding: | !important;"
    );
}

#[test]
fn css_multiple_and_at_rules() {
    assert_eq!(css("m10+p5"), "margin: 10px;\npadding: 5px;");
    assert_eq!(css("pos:a+t0+l0"), "position: absolute;\ntop: 0;\nleft: 0;");
    let e = expand_css_with_caret("@m", "  ").unwrap();
    assert_eq!(with_caret(&e), "@media |screen {\n  \n}");
    assert_eq!(css("@i"), "@import url();");
    assert_eq!(css("cm"), "/*  */");
}

#[test]
fn css_rejects_unknown() {
    for bad in [
        "", "foo", "xyz10", "m10_", "m 10", "fxq", "m10!!", "@m!", "c#ggg", "c#12345", "-m10",
    ] {
        assert_eq!(expand_css(bad), None, "{bad:?} should not expand");
    }
}

#[test]
fn abbreviation_extraction() {
    assert_eq!(abbreviation_before("  ul>li*3", 9), Some((2, "ul>li*3")));
    assert_eq!(abbreviation_before("<div>ul>li", 10), Some((5, "ul>li")));
    assert_eq!(
        abbreviation_before("x p{Hello world}", 16),
        Some((2, "p{Hello world}"))
    );
    assert_eq!(
        abbreviation_before("a[title=\"a b\"]", 14),
        Some((0, "a[title=\"a b\"]"))
    );
    assert_eq!(abbreviation_before("{% block x %}.c", 15), Some((13, ".c")));
    // Auto-inserted closing brace after the caret is part of the abbreviation.
    assert_eq!(abbreviation_before("p{Hi}", 4), Some((0, "p{Hi}")));
    // CSS contexts.
    assert_eq!(abbreviation_before("a { m10", 7), Some((4, "m10")));
    assert_eq!(
        abbreviation_before("color: red; c#f00", 17),
        Some((12, "c#f00"))
    );
    assert_eq!(css_abbreviation_before("a{pos:a}", 7), Some((2, "pos:a")));
    assert_eq!(css_abbreviation_before("  m10!", 6), Some((2, "m10!")));
    // Nothing to expand.
    assert_eq!(abbreviation_before("word ", 5), None);
    assert_eq!(abbreviation_before("", 0), None);
    assert_eq!(abbreviation_before("a]", 2), None);
    assert_eq!(abbreviation_before("ab", 3), None);
}

#[test]
fn language_contexts() {
    assert!(is_css_context("css"));
    assert!(is_css_context("SCSS"));
    assert!(is_css_context(".less"));
    assert!(is_css_context("sass"));
    assert!(!is_css_context("html"));
    assert!(!is_css_context("python"));
    assert!(is_html_context("html"));
    assert!(is_html_context("Django"));
    assert!(!is_html_context("javascript"));
}

#[test]
fn expand_at_in_html_documents() {
    assert_eq!(
        apply("<ul>\n    .item|\n</ul>", "html").as_deref(),
        Some("<ul>\n    <li class=\"item\">|</li>\n</ul>")
    );
    assert_eq!(
        apply("<div>\n  ul>li*2|\n</div>", "html").as_deref(),
        Some("<div>\n  <ul>\n    <li>|</li>\n    <li></li>\n  </ul>\n</div>")
    );
    assert_eq!(
        apply("<p>Text .x|</p>", "html").as_deref(),
        Some("<p>Text <span class=\"x\">|</span></p>")
    );
    assert_eq!(apply("p{Hello|}", "html").as_deref(), Some("<p>Hello</p>|"));
    assert_eq!(
        apply("<style>\n  a { m10| }\n</style>", "html").as_deref(),
        Some("<style>\n  a { margin: 10px;| }\n</style>")
    );
    // No expansion inside a tag, in script content, or in other languages.
    assert_eq!(apply("<a href=\"ul|\">", "html"), None);
    assert_eq!(apply("<script>\n  ul>li|\n</script>", "html"), None);
    assert_eq!(apply("x = ul>li|", "python"), None);
}

#[test]
fn expand_at_in_style_attributes() {
    assert_eq!(
        apply("<div style=\"m10|\"></div>", "html").as_deref(),
        Some("<div style=\"margin: 10px;|\"></div>")
    );
    // Several declarations stay on the attribute's line.
    assert_eq!(
        apply("<p style='color: red; pos:a+t0|'>", "html").as_deref(),
        Some("<p style='color: red; position: absolute; top: 0;|'>")
    );
    assert_eq!(
        apply("<p style=\"m|\">", "html").as_deref(),
        Some("<p style=\"margin: |;\">")
    );
    // Value position, other attributes, at-rules: nothing to do.
    assert_eq!(apply("<div style=\"color: m10|\">", "html"), None);
    assert_eq!(apply("<div class=\"m10|\">", "html"), None);
    assert_eq!(apply("<div style=\"@m|\">", "html"), None);
}

#[test]
fn expand_at_in_templates() {
    // Template tags are not markup.
    assert_eq!(apply("<p>{{ name|}}</p>", "django"), None);
    assert_eq!(apply("{% if user|", "jinja2"), None);
    assert_eq!(apply("<div>{{ msg|}}</div>", "vue"), None);
    // Around them, abbreviations expand as usual.
    assert_eq!(
        apply("<ul>\n  {% for x in xs %}\n  .item|", "django").as_deref(),
        Some("<ul>\n  {% for x in xs %}\n  <li class=\"item\">|</li>")
    );
    assert_eq!(
        apply("{% block body %}p.lead|", "jinja2").as_deref(),
        Some("{% block body %}<p class=\"lead\">|</p>")
    );
}

#[test]
fn expand_at_in_stylesheets() {
    assert_eq!(
        apply("a {\n  m10|\n}", "css").as_deref(),
        Some("a {\n  margin: 10px;|\n}")
    );
    assert_eq!(
        apply("a{bd1-s-red|}", "scss").as_deref(),
        Some("a{border: 1px solid red;|}")
    );
    assert_eq!(
        apply("a {\n  color: red; pos:a|\n}", "css").as_deref(),
        Some("a {\n  color: red; position: absolute;|\n}")
    );
    assert_eq!(
        apply("@m|", "css").as_deref(),
        Some("@media |screen {\n  \n}")
    );
    // Selector position and value position do not expand.
    assert_eq!(apply("p|", "css"), None);
    assert_eq!(apply("a { margin: 0 p| }", "css"), None);
}

#[test]
fn expand_at_in_indented_stylesheets() {
    // Sass: no semicolons, rule bodies by indentation.
    assert_eq!(
        apply(".a\n  m10|", "sass").as_deref(),
        Some(".a\n  margin: 10px|")
    );
    assert_eq!(
        apply(".a\n  pos:a+t0|", "sass").as_deref(),
        Some(".a\n  position: absolute\n  top: 0|")
    );
    assert_eq!(apply("@m|", "sass").as_deref(), Some("@media |screen\n  "));
    // Stylus: `property value`.
    assert_eq!(
        apply(".a\n  d:f|", "stylus").as_deref(),
        Some(".a\n  display flex|")
    );
    assert_eq!(
        apply(".a {\n  m10!|\n}", "styl").as_deref(),
        Some(".a {\n  margin 10px !important|\n}")
    );
    // An unindented line is a selector.
    assert_eq!(apply("m10|", "sass"), None);
}

/// Arbitrary editor text must never panic any entry point, and every result
/// must be internally consistent.
#[test]
fn robustness_on_arbitrary_input() {
    let alphabet: Vec<char> = "abdilpuvxz0129#.*:$-_!@%^+>/[]{}()=\"' \n\t\\<?|;é漢🙂\u{E000}"
        .chars()
        .collect();
    let pieces = [
        "div", "ul>li", "*3", "$$@-", "lorem", "{x}", "[a=b]", "!", "input:", "bd1-s", "#f00",
        "<p>", "</p>", "<style>", "{%", "-->", "a {", "${v}", "${1:x}", "c>", "style=\"", "{{",
        "%}", "trf:r", ".a/b", "\n  ",
    ];
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    for _ in 0..4000 {
        let mut s = String::new();
        for _ in 0..next(12) {
            if next(3) == 0 {
                s.push_str(pieces[next(pieces.len())]);
            } else {
                s.push(alphabet[next(alphabet.len())]);
            }
        }
        let results = [
            expand_html(&s, "\t"),
            expand_html_in(&s, "  ", Some("ul")),
            expand_css_with_caret(&s, "\t"),
        ];
        for e in results.into_iter().flatten() {
            assert!(
                e.caret <= e.text.len() && e.text.is_char_boundary(e.caret),
                "{s:?}"
            );
            assert!(
                e.stops
                    .iter()
                    .all(|r| r.start <= r.end && r.end <= e.text.len()),
                "{s:?}"
            );
            assert!(!e.text.contains(['\u{E000}', '\u{E001}']), "{s:?}");
        }
        for offset in 0..=s.len() + 1 {
            for (start, abbr) in [
                abbreviation_before(&s, offset),
                css_abbreviation_before(&s, offset),
            ]
            .into_iter()
            .flatten()
            {
                assert_eq!(
                    s.get(start..start + abbr.len()),
                    Some(abbr),
                    "{s:?} at {offset}"
                );
            }
            for language in ["html", "css", "sass", "stylus", "django"] {
                if let Some((range, e)) = expand_at(&s, offset, language, "  ") {
                    assert!(
                        range.start < range.end
                            && range.end <= s.len()
                            && s.is_char_boundary(range.start)
                            && s.is_char_boundary(range.end),
                        "{s:?} at {offset}"
                    );
                    assert!(e.text.is_char_boundary(e.caret), "{s:?} at {offset}");
                }
            }
        }
    }
}
