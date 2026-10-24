//! CSS (stylesheet) abbreviations: `m10` -> `margin: 10px;`, `p10-20`,
//! `w100p`, `bd1-s-red`, `pos:a`, `d:ib`, `fz14`, `c#f00`, `bgc`, `ta:c`,
//! `m10!` (`!important`), `m10+p5` (several declarations), plus a few
//! at-rule snippets (`@m`, `@i`, `@kf`, `@f`).
//!
//! An abbreviation is a property alias (from Emmet's CSS vocabulary, or a
//! full property name) followed by values: numbers with optional units
//! (`p` = `%`, `e` = `em`, `x` = `ex`, `r` = `rem`; integers default to `px`
//! and decimals to `em`, except on unitless properties), `#` colours
//! (`#f`, `#fc`, `#f00`, `#f00.5`, `#t`), and keyword aliases resolved
//! against the property's keywords (`a` -> `absolute` for `position`, `ib`
//! -> `inline-block`, `sb` -> `space-between`, `r` -> `repeat()` for
//! `grid-template-columns`). Values are separated by `-`; a value that
//! starts with `-` is negative (`m10--5` -> `10px -5px`).
//!
//! Output is CSS (`margin: 10px;`) by default; Sass's indented syntax drops
//! the semicolons and at-rule braces, Stylus writes `margin 10px`, and the
//! inline form (for `style="..."` attributes) puts every declaration on one
//! line.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::expansion::{field, has_marker, Expansion, CARET, FIELD_CLOSE, FIELD_OPEN};

/// Output dialect of [`expand`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// `margin: 10px;` (CSS, SCSS, Less, PostCSS).
    Css,
    /// `margin: 10px` and indented at-rule bodies (Sass's indented syntax).
    Sass,
    /// `margin 10px` (Stylus).
    Stylus,
}

impl Syntax {
    /// The dialect for a normalised language key (see `crate::language_key`).
    pub(crate) fn for_language(key: &str) -> Syntax {
        match key {
            "sass" => Syntax::Sass,
            "stylus" | "styl" => Syntax::Stylus,
            _ => Syntax::Css,
        }
    }

    /// Whether nesting is expressed by indentation rather than braces.
    pub(crate) fn indented(self) -> bool {
        self != Syntax::Css
    }
}

/// Emmet's CSS vocabulary (`snippets/css.json`), lightly curated:
/// `alias|alias`: `property:default|keyword|keyword...` where `${N:text}`
/// marks a placeholder. Entries whose value is not `property[:values]`
/// (at-rules, comments) are inserted verbatim.
static SNIPPETS: &[(&str, &str)] = &[
    ("@f", "@font-face {\n\tfont-family: ${1};\n\tsrc: url(${2});\n}"),
    ("@i|@import", "@import url(${0});"),
    ("@kf", "@keyframes ${1:identifier} {\n\t${0}\n}"),
    ("@m|@media", "@media ${1:screen} {\n\t${0}\n}"),
    ("@c|@charset", "@charset \"${1:UTF-8}\";"),
    ("cm", "/* ${0} */"),
    ("ac", "align-content:start|end|flex-start|flex-end|center|space-between|space-around|stretch|space-evenly"),
    ("ai", "align-items:start|end|flex-start|flex-end|center|baseline|stretch"),
    ("as", "align-self:start|end|auto|flex-start|flex-end|center|baseline|stretch"),
    ("anim", "animation:${1:name} ${2:duration} ${3:timing-function} ${4:delay} ${5:iteration-count} ${6:direction} ${7:fill-mode}"),
    ("animdel", "animation-delay:${1:time}"),
    ("animdir", "animation-direction:normal|reverse|alternate|alternate-reverse"),
    ("animdur", "animation-duration:${1:0}s"),
    ("animfm", "animation-fill-mode:both|forwards|backwards|none"),
    ("animic", "animation-iteration-count:1|infinite"),
    ("animn", "animation-name"),
    ("animps", "animation-play-state:running|paused"),
    ("animtf", "animation-timing-function:linear|ease|ease-in|ease-out|ease-in-out|step-start|step-end|cubic-bezier(${1:0.1}, ${2:0.7}, ${3:1.0}, ${4:0.1})"),
    ("ap", "appearance:none|auto"),
    ("ar", "aspect-ratio"),
    ("b", "bottom"),
    ("bd", "border:${1:1px} ${2:solid} ${3:#000}"),
    ("bdt", "border-top:${1:1px} ${2:solid} ${3:#000}"),
    ("bdr", "border-right:${1:1px} ${2:solid} ${3:#000}"),
    ("bdb", "border-bottom:${1:1px} ${2:solid} ${3:#000}"),
    ("bdl", "border-left:${1:1px} ${2:solid} ${3:#000}"),
    ("bdc", "border-color:${1:#000}"),
    ("bdtc", "border-top-color:${1:#000}"),
    ("bdrc", "border-right-color:${1:#000}"),
    ("bdbc", "border-bottom-color:${1:#000}"),
    ("bdlc", "border-left-color:${1:#000}"),
    ("bdi", "border-image:url(${0})"),
    ("bdcl", "border-collapse:collapse|separate"),
    ("bdrs", "border-radius"),
    ("bdtlrs", "border-top-left-radius"),
    ("bdtrrs", "border-top-right-radius"),
    ("bdblrs", "border-bottom-left-radius"),
    ("bdbrrs", "border-bottom-right-radius"),
    ("bds", "border-style:none|hidden|dashed|dotted|solid|double|groove|ridge|inset|outset"),
    ("bdts", "border-top-style"),
    ("bdrst", "border-right-style"),
    ("bdbs", "border-bottom-style"),
    ("bdls", "border-left-style"),
    ("bdsp", "border-spacing"),
    ("bdw", "border-width"),
    ("bdtw", "border-top-width"),
    ("bdrw", "border-right-width"),
    ("bdbw", "border-bottom-width"),
    ("bdlw", "border-left-width"),
    ("bbs", "border-block-start"),
    ("bbe", "border-block-end"),
    ("bis", "border-inline-start"),
    ("bie", "border-inline-end"),
    ("bfv", "backface-visibility:hidden|visible"),
    ("bg", "background:${1:#000}"),
    ("bga", "background-attachment:fixed|scroll|local"),
    ("bgc", "background-color:${1:#fff}"),
    ("bgcp", "background-clip:padding-box|border-box|content-box|text"),
    ("bgi", "background-image:url(${0})"),
    ("bgo", "background-origin:padding-box|border-box|content-box"),
    ("bgp", "background-position:${1:0} ${2:0}"),
    ("bgpx", "background-position-x"),
    ("bgpy", "background-position-y"),
    ("bgr", "background-repeat:no-repeat|repeat-x|repeat-y|space|round|repeat"),
    ("bgsz", "background-size:contain|cover|auto"),
    ("bs", "block-size"),
    ("bxsh", "box-shadow:${1:inset }${2:hoff} ${3:voff} ${4:blur} ${5:#000}|none"),
    ("bxsz", "box-sizing:border-box|content-box"),
    ("c", "color:${1:#000}"),
    ("cr", "color:rgb(${1:0}, ${2:0}, ${3:0})"),
    ("cra", "color:rgba(${1:0}, ${2:0}, ${3:0}, ${4:.5})"),
    ("cl", "clear:both|left|right|none"),
    ("cnt", "content:'${0}'|normal|open-quote|no-open-quote|close-quote|no-close-quote|attr(${0})|counter(${0})|counters(${0})"),
    ("coi", "counter-increment"),
    ("cor", "counter-reset"),
    ("colm", "columns"),
    ("colmc", "column-count"),
    ("colmf", "column-fill:balance|auto"),
    ("colmg", "column-gap"),
    ("colmr", "column-rule"),
    ("colmrc", "column-rule-color"),
    ("colmrs", "column-rule-style"),
    ("colmrw", "column-rule-width"),
    ("colms", "column-span:none|all"),
    ("colmw", "column-width"),
    ("cp", "clip:auto|rect(${1:top} ${2:right} ${3:bottom} ${4:left})"),
    ("cps", "caption-side:top|bottom"),
    ("cur", "cursor:pointer|auto|default|crosshair|help|move|text|wait|not-allowed|grab|grabbing|progress"),
    ("d", "display:block|none|flex|inline-flex|inline|inline-block|grid|inline-grid|subgrid|list-item|contents|flow-root|table|inline-table|table-cell|table-row|table-caption|table-column|table-column-group|table-header-group|table-footer-group|table-row-group|ruby|run-in"),
    ("ec", "empty-cells:show|hide"),
    ("f", "font:${1:1em} ${2:sans-serif}"),
    ("fd", "font-display:auto|block|swap|fallback|optional"),
    ("ff", "font-family:serif|sans-serif|cursive|fantasy|monospace|system-ui"),
    ("fft", "font-family:\"Times New Roman\", Times, Baskerville, Georgia, serif"),
    ("ffa", "font-family:Arial, \"Helvetica Neue\", Helvetica, sans-serif"),
    ("ffv", "font-family:Verdana, Geneva, sans-serif"),
    ("fl", "float:left|right|none"),
    ("fs", "font-style:italic|normal|oblique"),
    ("fst", "font-stretch:normal|ultra-condensed|extra-condensed|condensed|semi-condensed|semi-expanded|expanded|extra-expanded|ultra-expanded"),
    ("fv", "font-variant:normal|small-caps"),
    ("fvs", "font-variation-settings:normal|inherit|initial|unset"),
    ("fw", "font-weight:normal|bold|bolder|lighter"),
    ("fx", "flex"),
    ("fxb", "flex-basis:fill|max-content|min-content|fit-content|content"),
    ("fxd", "flex-direction:row|row-reverse|column|column-reverse"),
    ("fxf", "flex-flow"),
    ("fxg", "flex-grow"),
    ("fxsh", "flex-shrink"),
    ("fxw", "flex-wrap:nowrap|wrap|wrap-reverse"),
    ("fz|fsz", "font-size"),
    ("fsza", "font-size-adjust"),
    ("g", "gap"),
    ("gtc", "grid-template-columns:repeat(${0})|minmax(${0})"),
    ("gtr", "grid-template-rows:repeat(${0})|minmax(${0})"),
    ("gta", "grid-template-areas"),
    ("gt", "grid-template"),
    ("gg", "grid-gap"),
    ("gcg", "grid-column-gap"),
    ("grg", "grid-row-gap"),
    ("gac", "grid-auto-columns:auto|minmax(${0})"),
    ("gar", "grid-auto-rows:auto|minmax(${0})"),
    ("gaf", "grid-auto-flow:row|column|dense|row-dense|column-dense"),
    ("gd", "grid"),
    ("gc", "grid-column"),
    ("gcs", "grid-column-start"),
    ("gce", "grid-column-end"),
    ("gr", "grid-row"),
    ("grs", "grid-row-start"),
    ("gre", "grid-row-end"),
    ("ga", "grid-area"),
    ("h", "height"),
    ("is", "inline-size"),
    ("jc", "justify-content:start|end|stretch|flex-start|flex-end|center|space-between|space-around|space-evenly"),
    ("ji", "justify-items:start|end|center|stretch"),
    ("js", "justify-self:start|end|center|stretch"),
    ("l", "left"),
    ("lg", "background-image:linear-gradient(${1})"),
    ("lh", "line-height"),
    ("lis", "list-style"),
    ("lisi", "list-style-image"),
    ("lisp", "list-style-position:inside|outside"),
    ("list", "list-style-type:disc|circle|square|decimal|decimal-leading-zero|lower-roman|upper-roman|none"),
    ("lts", "letter-spacing:normal"),
    ("m", "margin"),
    ("mah", "max-height"),
    ("maw", "max-width"),
    ("mb", "margin-bottom"),
    ("mih", "min-height"),
    ("miw", "min-width"),
    ("ml", "margin-left"),
    ("mr", "margin-right"),
    ("mt", "margin-top"),
    ("mbs", "margin-block-start"),
    ("mbe", "margin-block-end"),
    ("mis", "margin-inline-start"),
    ("mie", "margin-inline-end"),
    ("objf", "object-fit:fill|contain|cover|none|scale-down"),
    ("objp", "object-position"),
    ("ol", "outline"),
    ("olc", "outline-color:${1:#000}|invert"),
    ("olo", "outline-offset"),
    ("ols", "outline-style:none|dotted|dashed|solid|double|groove|ridge|inset|outset"),
    ("olw", "outline-width:thin|medium|thick"),
    ("op|opa", "opacity"),
    ("ord", "order"),
    ("orp", "orphans"),
    ("ov", "overflow:hidden|visible|scroll|auto"),
    ("ovx", "overflow-x:hidden|visible|scroll|auto"),
    ("ovy", "overflow-y:hidden|visible|scroll|auto"),
    ("p", "padding"),
    ("pb", "padding-bottom"),
    ("pl", "padding-left"),
    ("pr", "padding-right"),
    ("pt", "padding-top"),
    ("pbs", "padding-block-start"),
    ("pbe", "padding-block-end"),
    ("pis", "padding-inline-start"),
    ("pie", "padding-inline-end"),
    ("pe", "pointer-events:auto|none"),
    ("pgba", "page-break-after:auto|always|left|right"),
    ("pgbb", "page-break-before:auto|always|left|right"),
    ("pgbi", "page-break-inside:auto|avoid"),
    ("pos", "position:relative|absolute|fixed|static|sticky"),
    ("q", "quotes"),
    ("qen", "quotes:'\\201C' '\\201D' '\\2018' '\\2019'"),
    ("qru", "quotes:'\\00AB' '\\00BB' '\\201E' '\\201C'"),
    ("r", "right"),
    ("rsz", "resize:none|both|horizontal|vertical"),
    ("t", "top"),
    ("ta", "text-align:left|center|right|justify|start|end"),
    ("tal", "text-align-last:left|center|right"),
    ("tbl", "table-layout:fixed|auto"),
    ("td", "text-decoration:none|underline|overline|line-through"),
    ("ti", "text-indent"),
    ("tov", "text-overflow:ellipsis|clip"),
    // The leading empty alternative keeps `trf` itself an empty edit point.
    ("trf", "transform:|skewX(${1:angle})|skewY(${1:angle})|scale(${1:x}, ${2:y})|scaleX(${1:x})|scaleY(${1:y})|scaleZ(${1:z})|scale3d(${1:x}, ${2:y}, ${3:z})|rotate(${1:angle})|rotateX(${1:angle})|rotateY(${1:angle})|rotateZ(${1:angle})|translate(${1:x}, ${2:y})|translateX(${1:x})|translateY(${1:y})|translateZ(${1:z})|translate3d(${1:tx}, ${2:ty}, ${3:tz})|perspective(${1:length})|matrix(${0})|none"),
    ("trfo", "transform-origin"),
    ("trfs", "transform-style:preserve-3d|flat"),
    ("trs", "transition:${1:prop} ${2:time}"),
    ("trsde", "transition-delay:${1:time}"),
    ("trsdu", "transition-duration:${1:time}"),
    ("trsp", "transition-property:${1:prop}"),
    ("trstf", "transition-timing-function:ease|linear|ease-in|ease-out|ease-in-out|step-start|step-end|cubic-bezier(${1:0.1}, ${2:0.7}, ${3:1.0}, ${4:0.1})"),
    ("tsh", "text-shadow:${1:hoff} ${2:voff} ${3:blur} ${4:#000}"),
    ("tt", "text-transform:uppercase|lowercase|capitalize|none"),
    ("us", "user-select:none|auto|text|all"),
    ("v", "visibility:hidden|visible|collapse"),
    ("va", "vertical-align:top|super|text-top|middle|baseline|bottom|text-bottom|sub"),
    ("w", "width"),
    ("whs", "white-space:nowrap|pre|pre-wrap|pre-line|normal|break-spaces"),
    ("wido", "widows"),
    ("wm", "writing-mode:horizontal-tb|vertical-rl|vertical-lr"),
    ("wob", "word-break:normal|keep-all|break-all|break-word"),
    ("wos", "word-spacing"),
    ("wow", "word-wrap:normal|break-word"),
    ("z|zi", "z-index"),
    ("zom", "zoom:1"),
    // Newer properties without an Emmet alias: usable by full name
    // (`place-items:c`, `scroll-behavior:s`, `inset0`).
    ("accent-color", "accent-color"),
    ("backdrop-filter", "backdrop-filter"),
    ("caret-color", "caret-color"),
    ("clip-path", "clip-path"),
    ("content-visibility", "content-visibility:visible|hidden|auto"),
    ("filter", "filter"),
    ("hyphens", "hyphens:none|manual|auto"),
    ("inset", "inset"),
    ("isolation", "isolation:auto|isolate"),
    ("mix-blend-mode", "mix-blend-mode:normal|multiply|screen|overlay|darken|lighten|color-dodge|color-burn|difference|exclusion"),
    ("overflow-wrap", "overflow-wrap:normal|break-word|anywhere"),
    ("overscroll-behavior", "overscroll-behavior:auto|contain|none"),
    ("place-content", "place-content:center|start|end|space-between|space-around|space-evenly|stretch"),
    ("place-items", "place-items:center|start|end|stretch|baseline"),
    ("place-self", "place-self:auto|center|start|end|stretch"),
    ("row-gap", "row-gap"),
    ("scroll-behavior", "scroll-behavior:auto|smooth"),
    ("scroll-margin", "scroll-margin"),
    ("scroll-padding", "scroll-padding"),
    ("scroll-snap-align", "scroll-snap-align:none|start|end|center"),
    ("tab-size", "tab-size"),
    ("text-decoration-color", "text-decoration-color"),
    ("text-decoration-line", "text-decoration-line:none|underline|overline|line-through"),
    ("text-decoration-style", "text-decoration-style:solid|double|dotted|dashed|wavy"),
    ("text-underline-offset", "text-underline-offset"),
    ("touch-action", "touch-action:auto|none|manipulation|pan-x|pan-y"),
    ("will-change", "will-change:auto|transform|opacity|scroll-position|contents"),
];

/// Properties that take plain numbers (no automatic `px`/`em`).
#[rustfmt::skip]
const UNITLESS: &[&str] = &[
    "z-index", "line-height", "opacity", "font-weight", "zoom", "flex", "flex-grow", "flex-shrink", "order",
    "orphans", "widows", "column-count", "animation-iteration-count", "fill-opacity", "stroke-opacity", "tab-size",
    "aspect-ratio", "grid-row", "grid-row-start", "grid-row-end", "grid-column", "grid-column-start",
    "grid-column-end", "grid-area", "counter-increment", "counter-reset",
];

/// Value keywords valid for every property.
const GLOBAL_KEYWORDS: &[&str] = &["auto", "inherit", "initial", "unset", "none"];

/// Borders and outlines: style, width and colour keywords.
#[rustfmt::skip]
const LINE_KEYWORDS: &[&str] = &[
    "none", "solid", "dashed", "dotted", "double", "groove", "ridge", "inset", "outset", "hidden", "thin", "medium",
    "thick", "transparent", "currentColor",
];
const COLOR_KEYWORDS: &[&str] = &["transparent", "currentColor"];
const POSITION_KEYWORDS: &[&str] = &["center", "top", "bottom", "left", "right"];
#[rustfmt::skip]
const BACKGROUND_KEYWORDS: &[&str] = &[
    "none", "transparent", "no-repeat", "repeat", "repeat-x", "repeat-y", "space", "round", "fixed", "scroll",
    "local", "center", "top", "bottom", "left", "right", "cover", "contain",
];
#[rustfmt::skip]
const FONT_KEYWORDS: &[&str] = &[
    "normal", "italic", "oblique", "bold", "bolder", "lighter", "small-caps", "serif", "sans-serif", "monospace",
    "cursive", "fantasy", "system-ui",
];
#[rustfmt::skip]
const LIST_KEYWORDS: &[&str] = &[
    "none", "disc", "circle", "square", "decimal", "inside", "outside",
];
#[rustfmt::skip]
const MOTION_KEYWORDS: &[&str] = &[
    "all", "none", "ease", "linear", "ease-in", "ease-out", "ease-in-out", "infinite", "alternate", "reverse",
    "forwards", "backwards", "both", "running", "paused",
];
const FLEX_KEYWORDS: &[&str] = &["none", "auto", "content"];

/// Keywords a property accepts beyond its own vocabulary entry: those a
/// shorthand inherits from its longhands (`bd1-s` -> `solid` comes from
/// `border-style`) and the colour/position keywords of colour and position
/// properties (`bgc:t` -> `transparent`, `bgp:c` -> `center`).
fn extra_keywords(property: &str) -> &'static [&'static str] {
    match property {
        "border-radius" | "border-collapse" | "border-spacing" | "border-image" => &[],
        p if p.ends_with("color") => COLOR_KEYWORDS,
        "outline" | "column-rule" => LINE_KEYWORDS,
        p if p.starts_with("border") && !p.ends_with("radius") => LINE_KEYWORDS,
        "background" => BACKGROUND_KEYWORDS,
        "background-position"
        | "background-position-x"
        | "background-position-y"
        | "object-position"
        | "transform-origin"
        | "perspective-origin" => POSITION_KEYWORDS,
        "font" => FONT_KEYWORDS,
        "list-style" => LIST_KEYWORDS,
        "transition" | "animation" => MOTION_KEYWORDS,
        "flex" | "flex-basis" => FLEX_KEYWORDS,
        _ => &[],
    }
}

const UNIT_ALIASES: &[(&str, &str)] = &[("p", "%"), ("e", "em"), ("x", "ex"), ("r", "rem")];
#[rustfmt::skip]
const UNITS: &[&str] = &[
    "px", "em", "rem", "ex", "ch", "vw", "vh", "vmin", "vmax", "vi", "vb", "svh", "lvh", "dvh", "svw", "lvw", "dvw",
    "cqw", "cqh", "lh", "rlh", "%", "cm", "mm", "q", "in", "pt", "pc", "s", "ms", "deg", "rad", "grad", "turn",
    "fr", "dpi", "dpcm", "dppx", "hz", "khz",
];

struct Prop {
    name: &'static str,
    /// Value alternatives (field markers already in place); the first is the default.
    values: Vec<String>,
    /// The alternatives that are plain keywords, plus the names of the
    /// function-call alternatives (`repeat` for `repeat(${0})`).
    keywords: Vec<&'static str>,
}

impl Prop {
    /// The value alternative for function keyword `name`: `repeat` ->
    /// `repeat(|)` for `grid-template-columns`.
    fn function(&self, name: &str) -> Option<&str> {
        self.values.iter().map(String::as_str).find(|v| {
            v.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('('))
        })
    }
}

#[derive(Clone, Copy)]
enum Snippet {
    Prop(usize),
    Raw(&'static str),
}

struct Vocab {
    props: Vec<Prop>,
    by_key: HashMap<&'static str, Snippet>,
    by_property: HashMap<&'static str, usize>,
}

impl Vocab {
    /// A property by alias (`bdrs`) or full name (`border-radius`).
    fn prop(&self, name: &str) -> Option<&Prop> {
        match self.by_key.get(name) {
            Some(Snippet::Prop(i)) => Some(&self.props[*i]),
            Some(Snippet::Raw(_)) => None,
            None => self.by_property.get(name).map(|i| &self.props[*i]),
        }
    }
}

static VOCAB: LazyLock<Vocab> = LazyLock::new(|| {
    let mut vocab = Vocab {
        props: Vec::new(),
        by_key: HashMap::new(),
        by_property: HashMap::new(),
    };
    for (keys, value) in SNIPPETS {
        let snippet = match parse_property(value) {
            Some(prop) => {
                vocab
                    .by_property
                    .entry(prop.name)
                    .or_insert(vocab.props.len());
                vocab.props.push(prop);
                Snippet::Prop(vocab.props.len() - 1)
            }
            None => Snippet::Raw(value),
        };
        for key in keys.split('|') {
            vocab.by_key.insert(key, snippet);
        }
    }
    vocab
});

/// `property` or `property:alt|alt|...`; `None` for verbatim snippets.
fn parse_property(value: &'static str) -> Option<Prop> {
    let (name, rest) = match value.split_once(':') {
        Some((name, rest)) => (name, Some(rest)),
        None => (value, None),
    };
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
        return None;
    }
    let alternatives = rest.map(split_alternatives).unwrap_or_default();
    let keywords = alternatives
        .iter()
        .filter_map(|a| {
            let a = a.trim();
            if is_plain_keyword(a) {
                Some(a)
            } else {
                function_name(a)
            }
        })
        .collect();
    let values = alternatives
        .iter()
        .map(|a| convert_fields(a.trim()))
        .collect();
    Some(Prop {
        name,
        values,
        keywords,
    })
}

/// A plain keyword alternative: `absolute`, `inline-block`.
fn is_plain_keyword(alternative: &str) -> bool {
    alternative.starts_with(|c: char| c.is_ascii_lowercase())
        && alternative
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The name of a function-call alternative: `repeat(${0})` -> `repeat`,
/// `translateX(${1:x})` -> `translateX`.
fn function_name(alternative: &'static str) -> Option<&'static str> {
    let (name, _) = alternative.split_once('(')?;
    (alternative.ends_with(')')
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    .then_some(name)
}

/// Splits on `|` outside `${...}` and parentheses.
fn split_alternatives(s: &'static str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' | '(' => depth += 1,
            '}' | ')' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Turns `${N}` / `${N:placeholder}` into field markers, keeping literal braces.
fn convert_fields(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut stack: Vec<bool> = Vec::new(); // true = field, false = literal brace
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                while chars.next_if(char::is_ascii_digit).is_some() {}
                chars.next_if_eq(&':');
                stack.push(true);
                out.push(FIELD_OPEN);
            }
            '{' => {
                stack.push(false);
                out.push('{');
            }
            '}' => match stack.pop() {
                Some(true) => out.push(FIELD_CLOSE),
                _ => out.push('}'),
            },
            _ => out.push(c),
        }
    }
    out
}

/// Expands a CSS abbreviation to declarations (`m10` -> `margin: 10px;`).
/// Several `+`-joined abbreviations give one declaration per line. Returns
/// `None` when the abbreviation is not known.
pub fn expand_css(abbr: &str) -> Option<String> {
    expand_css_with_caret(abbr, "\t").map(|e| e.text)
}

/// [`expand_css`] with edit points: the caret goes to an empty value
/// (`m` -> `margin: |;`) or the first placeholder (`bd` -> `border: 1px solid
/// #000;` with `1px`, `solid`, `#000` as stops), else after the text.
/// `indent` indents the body of multi-line at-rule snippets (`@media`).
pub fn expand_css_with_caret(abbr: &str, indent: &str) -> Option<Expansion> {
    expand(abbr, indent, Syntax::Css, false)
}

/// Expands for a stylesheet dialect. `inline` is the `style="..."` form:
/// declarations joined on one line, at-rules and comments refused.
pub(crate) fn expand(abbr: &str, indent: &str, syntax: Syntax, inline: bool) -> Option<Expansion> {
    let abbr = abbr.trim();
    if abbr.is_empty() || has_marker(abbr) || abbr.contains(char::is_whitespace) {
        return None;
    }
    let mut parts = Vec::new();
    for part in abbr.split('+').filter(|p| !p.is_empty()) {
        parts.push(expand_one(part, indent, syntax, inline)?);
    }
    if parts.is_empty() {
        return None;
    }
    let separator = if inline { " " } else { "\n" };
    Some(Expansion::from_marked(&parts.join(separator)))
}

fn expand_one(abbr: &str, indent: &str, syntax: Syntax, inline: bool) -> Option<String> {
    let (body, important) = match abbr.strip_suffix('!') {
        Some(body) => (body, true),
        None => (abbr, false),
    };
    if body.is_empty() || body.contains('!') {
        return None;
    }
    let vocab = &*VOCAB;
    match vocab.by_key.get(body.to_ascii_lowercase().as_str()) {
        Some(Snippet::Raw(text)) => {
            if important || inline {
                return None;
            }
            let text = match syntax {
                Syntax::Sass => indented_block(text),
                _ => (*text).to_string(),
            };
            return Some(convert_fields(&text).replace('\t', indent));
        }
        Some(Snippet::Prop(i)) => {
            return Some(default_declaration(&vocab.props[*i], important, syntax))
        }
        None => {}
    }
    let (prop, value) = split_property(body, vocab)?;
    let tokens = tokenize(&value)?;
    if tokens.is_empty() {
        return Some(default_declaration(prop, important, syntax));
    }
    let value = render_values(&tokens, prop)?;
    Some(declaration(prop.name, &value, important, syntax))
}

/// Rewrites a brace-style snippet for Sass's indented syntax: `@media x {`
/// loses the brace, the closing `}` line goes, and so do the semicolons.
fn indented_block(raw: &str) -> String {
    raw.lines()
        .filter(|line| line.trim() != "}")
        .map(|line| {
            let line = line.strip_suffix(" {").unwrap_or(line);
            line.strip_suffix(';').unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn declaration(name: &str, value: &str, important: bool, syntax: Syntax) -> String {
    let important = if important { " !important" } else { "" };
    match syntax {
        Syntax::Css => format!("{name}: {value}{important};"),
        Syntax::Sass => format!("{name}: {value}{important}"),
        Syntax::Stylus => format!("{name} {value}{important}"),
    }
}

/// The declaration with the property's default value: a placeholder when
/// there is a choice of keywords, an empty edit point when there is none.
fn default_declaration(prop: &Prop, important: bool, syntax: Syntax) -> String {
    let value = match prop.values.as_slice() {
        [] => CARET.to_string(),
        [only] => only.clone(),
        [first, ..] if has_marker(first) => first.clone(),
        [first, ..] => field(first),
    };
    declaration(prop.name, &value, important, syntax)
}

/// Splits `pos:a`, `m10`, `bd1-s-red`, `border-radius5`, `text-align-center`
/// or `dib` into a property and the source of its values.
fn split_property<'v>(body: &str, vocab: &'v Vocab) -> Option<(&'v Prop, String)> {
    if let Some((name, value)) = body.split_once(':') {
        return Some((vocab.prop(&name.to_ascii_lowercase())?, value.to_string()));
    }
    let hyphen_end = body
        .find(|c: char| !(c.is_ascii_alphabetic() || c == '-'))
        .unwrap_or(body.len());
    let alpha_end = body
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(body.len());
    // Longest name first: the whole `a-b-c` run, then each shorter `a-b`
    // prefix, then the leading letters.
    let run = &body[..hyphen_end];
    let mut candidates: Vec<&str> = vec![run.trim_end_matches('-')];
    candidates.extend(
        run.rmatch_indices('-')
            .map(|(i, _)| run[..i].trim_end_matches('-')),
    );
    candidates.push(&body[..alpha_end]);
    for name in candidates {
        if name.is_empty() {
            continue;
        }
        if let Some(prop) = vocab.prop(&name.to_ascii_lowercase()) {
            return Some((prop, body[name.len()..].to_string()));
        }
    }
    // A keyword glued to the alias: `dib`, `posa`, `tac`, `fwb`, `ovh`,
    // `whsnw`. The longest alias that takes the rest as a keyword wins.
    if alpha_end == body.len() {
        let lower = body.to_ascii_lowercase();
        for split in (1..lower.len()).rev() {
            if let Some(prop) = vocab.prop(&lower[..split]) {
                if let Some(keyword) = resolve_keyword(&lower[split..], prop) {
                    return Some((prop, keyword.to_string()));
                }
            }
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Number { text: String, unit: Option<String> },
    Color(String),
    Word(String),
    Slash,
}

fn tokenize(src: &str) -> Option<Vec<Token>> {
    let b = src.as_bytes();
    let digit_at = |k: usize| b.get(k).is_some_and(u8::is_ascii_digit);
    let number_at = |k: usize| digit_at(k) || (b.get(k) == Some(&b'.') && digit_at(k + 1));
    let mut out = Vec::new();
    let mut i = 0;
    // True at the start and after a separator: a `-` there makes a negative number.
    let mut expect_value = true;
    while i < b.len() {
        let c = b[i];
        if c == b'-' && !(expect_value && number_at(i + 1)) {
            i += 1;
            expect_value = true;
            continue;
        }
        if c == b'/' {
            out.push(Token::Slash);
            i += 1;
            expect_value = true;
            continue;
        }
        if c == b'-' || number_at(i) {
            let start = i;
            if c == b'-' {
                i += 1;
            }
            while digit_at(i) {
                i += 1;
            }
            if b.get(i) == Some(&b'.') && digit_at(i + 1) {
                i += 1;
                while digit_at(i) {
                    i += 1;
                }
            }
            let text = src[start..i].to_string();
            let unit_start = i;
            while b
                .get(i)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'%')
            {
                i += 1;
            }
            let unit = (i > unit_start).then(|| src[unit_start..i].to_string());
            out.push(Token::Number { text, unit });
        } else if c == b'#' {
            i += 1;
            let start = i;
            while b.get(i).is_some_and(u8::is_ascii_hexdigit) {
                i += 1;
            }
            let mut hex = &src[start..i];
            if hex.is_empty() && b.get(i) == Some(&b't') {
                i += 1;
                hex = "t";
            }
            let alpha_start = i;
            if b.get(i) == Some(&b'.') && digit_at(i + 1) {
                i += 1;
                while digit_at(i) {
                    i += 1;
                }
            }
            let alpha = (i > alpha_start).then(|| &src[alpha_start..i]);
            out.push(Token::Color(format_color(hex, alpha)?));
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while b.get(i).is_some_and(u8::is_ascii_alphanumeric) {
                i += 1;
            }
            out.push(Token::Word(src[start..i].to_string()));
        } else {
            return None;
        }
        expect_value = false;
    }
    Some(out)
}

/// `#f` -> `#fff`, `#fc` -> `#fcfcfc`, `#ff0000` -> `#f00`, `#f00` + `.5` ->
/// `rgba(255, 0, 0, 0.5)`, `#t` -> `transparent`; `#rgba` / `#rrggbbaa`
/// stay as typed.
fn format_color(hex: &str, alpha: Option<&str>) -> Option<String> {
    if hex == "t" {
        return Some("transparent".into());
    }
    let h = hex.to_ascii_lowercase();
    let b = h.as_bytes();
    let channels: [String; 3] = match b.len() {
        1 => [h.repeat(2), h.repeat(2), h.repeat(2)],
        2 => [h.clone(), h.clone(), h.clone()],
        3 => [h[0..1].repeat(2), h[1..2].repeat(2), h[2..3].repeat(2)],
        6 => [
            h[0..2].to_string(),
            h[2..4].to_string(),
            h[4..6].to_string(),
        ],
        // Already carries an alpha channel: a second one makes no sense.
        4 | 8 => return alpha.is_none().then(|| format!("#{h}")),
        _ => return None,
    };
    if let Some(alpha) = alpha {
        let [r, g, b] = channels.map(|c| u8::from_str_radix(&c, 16).unwrap_or(0));
        let alpha = if alpha.starts_with('.') {
            format!("0{alpha}")
        } else {
            alpha.to_string()
        };
        return Some(format!("rgba({r}, {g}, {b}, {alpha})"));
    }
    if channels.iter().all(|c| c.as_bytes()[0] == c.as_bytes()[1]) {
        Some(format!(
            "#{}",
            channels.iter().map(|c| &c[..1]).collect::<String>()
        ))
    } else {
        Some(format!("#{}", channels.concat()))
    }
}

fn format_number(text: &str, unit: Option<&str>, property: &str) -> Option<String> {
    if let Some(unit) = unit {
        let lower = unit.to_ascii_lowercase();
        let unit = UNIT_ALIASES
            .iter()
            .find(|(alias, _)| *alias == lower)
            .map(|(_, u)| (*u).to_string())
            .or_else(|| UNITS.contains(&lower.as_str()).then_some(lower))?;
        return Some(format!("{text}{unit}"));
    }
    let zero = text.bytes().all(|b| matches!(b, b'0' | b'.' | b'-'));
    if zero || UNITLESS.contains(&property) {
        return Some(text.to_string());
    }
    Some(format!(
        "{text}{}",
        if text.contains('.') { "em" } else { "px" }
    ))
}

fn word(token: &Token) -> &str {
    match token {
        Token::Word(w) => w,
        _ => "",
    }
}

/// Where a property's keywords come from, most specific first.
fn keyword_sources(prop: &Prop) -> [&[&'static str]; 3] {
    [&prop.keywords, extra_keywords(prop.name), GLOBAL_KEYWORDS]
}

/// The keyword spelled exactly `kw` (ignoring case), from any source.
fn known_keyword(kw: &str, prop: &Prop) -> Option<&'static str> {
    keyword_sources(prop)
        .into_iter()
        .flatten()
        .copied()
        .find(|k| k.eq_ignore_ascii_case(kw))
}

/// Resolves a keyword alias, ignoring case: exact match, then the shortest
/// keyword it prefixes, then a hyphen acronym (`ib` = `inline-block`), then
/// for aliases of up to four letters a fuzzy subsequence match (`nw` =
/// `nowrap`). The property's own keywords win over inherited and global ones
/// at every level.
fn resolve_keyword(alias: &str, prop: &Prop) -> Option<&'static str> {
    if alias.is_empty() {
        return None;
    }
    let alias = alias.to_ascii_lowercase();
    if let Some(k) = known_keyword(&alias, prop) {
        return Some(k);
    }
    let sources = keyword_sources(prop);
    for source in sources {
        // `min_by_key` keeps the first of equally short keywords.
        if let Some(k) = source
            .iter()
            .copied()
            .filter(|k| {
                k.get(..alias.len())
                    .is_some_and(|p| p.eq_ignore_ascii_case(&alias))
            })
            .min_by_key(|k| k.len())
        {
            return Some(k);
        }
    }
    for source in sources {
        if let Some(k) = source.iter().copied().find(|k| is_acronym(&alias, k)) {
            return Some(k);
        }
    }
    if alias.len() <= 4 {
        for source in sources {
            let mut best: Option<(&'static str, f64)> = None;
            for k in source.iter().copied() {
                let score = fuzzy_score(&alias, k);
                if score > 0.0 && best.is_none_or(|(_, s)| score > s) {
                    best = Some((k, score));
                }
            }
            if let Some((k, _)) = best {
                return Some(k);
            }
        }
    }
    None
}

/// The output for a resolved keyword: the function-call template for a
/// function keyword (`repeat` -> `repeat(|)`), else the keyword itself.
fn keyword_value(keyword: &str, prop: &Prop) -> String {
    prop.function(keyword).unwrap_or(keyword).to_string()
}

/// `ib` / `inline-block`: one letter per hyphen-separated part.
fn is_acronym(alias: &str, keyword: &str) -> bool {
    let parts: Vec<&str> = keyword.split('-').collect();
    parts.len() > 1
        && parts.len() == alias.len()
        && parts.iter().zip(alias.bytes()).all(|(part, c)| {
            part.as_bytes()
                .first()
                .is_some_and(|f| f.eq_ignore_ascii_case(&c))
        })
}

/// Emmet-style subsequence score in `0..=1` (0 = no match) for a lower-case
/// `alias`: the first letters must agree, earlier matches and matches right
/// after a `-` score higher, shorter keywords score higher.
fn fuzzy_score(alias: &str, keyword: &str) -> f64 {
    let a = alias.as_bytes();
    let keyword = keyword.to_ascii_lowercase();
    let k = keyword.as_bytes();
    if a.is_empty() || k.is_empty() || a[0] != k[0] {
        return 0.0;
    }
    let mut score = k.len() as f64;
    let mut j = 1;
    for &ch in &a[1..] {
        let mut found = false;
        let mut after_dash = false;
        while j < k.len() {
            if k[j] == ch {
                found = true;
                score += (k.len() - j) as f64 * if after_dash { 2.0 } else { 1.0 };
                j += 1;
                break;
            }
            after_dash = k[j] == b'-';
            j += 1;
        }
        if !found {
            return 0.0;
        }
    }
    score / (k.len() * (k.len() + 1) / 2) as f64
}

fn render_values(tokens: &[Token], prop: &Prop) -> Option<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(w) => {
                // A hyphenated keyword typed in full: `inline-block`, `space-between`.
                let run = tokens[i..]
                    .iter()
                    .take_while(|t| matches!(t, Token::Word(_)))
                    .count()
                    .min(4);
                let joined = (2..=run).rev().find_map(|n| {
                    let kw = tokens[i..i + n]
                        .iter()
                        .map(word)
                        .collect::<Vec<_>>()
                        .join("-");
                    known_keyword(&kw, prop).map(|k| (k, n))
                });
                let (keyword, used) = match joined {
                    Some((kw, n)) => (Some(kw), n),
                    None => (resolve_keyword(w, prop), 1),
                };
                // Anything else is kept as typed: `red`, `Georgia`.
                out.push(keyword.map_or_else(|| w.clone(), |k| keyword_value(k, prop)));
                i += used;
            }
            Token::Number { text, unit } => {
                out.push(format_number(text, unit.as_deref(), prop.name)?);
                i += 1;
            }
            Token::Color(c) => {
                out.push(c.clone());
                i += 1;
            }
            Token::Slash => {
                out.push("/".into());
                i += 1;
            }
        }
    }
    Some(out.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css(abbr: &str) -> String {
        expand_css(abbr).unwrap_or_else(|| panic!("{abbr:?} did not expand"))
    }

    #[test]
    fn vocabulary_is_well_formed() {
        let vocab = &*VOCAB;
        for (keys, value) in SNIPPETS {
            for key in keys.split('|') {
                assert!(vocab.by_key.contains_key(key), "{key}");
            }
            if !keys.starts_with('@') && *keys != "cm" {
                assert!(
                    parse_property(value).is_some(),
                    "{keys} = {value:?} is not a property"
                );
            }
        }
        assert_eq!(vocab.prop("position").map(|p| p.name), Some("position"));
        assert_eq!(vocab.prop("pos").unwrap().keywords[0], "relative");
    }

    #[test]
    fn colours() {
        assert_eq!(format_color("f", None).unwrap(), "#fff");
        assert_eq!(format_color("fc", None).unwrap(), "#fcfcfc");
        assert_eq!(format_color("F00", None).unwrap(), "#f00");
        assert_eq!(format_color("ff0000", None).unwrap(), "#f00");
        assert_eq!(format_color("abcdef", None).unwrap(), "#abcdef");
        assert_eq!(
            format_color("f00", Some(".5")).unwrap(),
            "rgba(255, 0, 0, 0.5)"
        );
        assert_eq!(format_color("t", None).unwrap(), "transparent");
        assert_eq!(format_color("ABCD", None).unwrap(), "#abcd");
        assert_eq!(format_color("ff000080", None).unwrap(), "#ff000080");
        assert_eq!(format_color("abcd", Some(".5")), None);
        assert_eq!(format_color("abcde", None), None);
    }

    #[test]
    fn keyword_resolution() {
        let vocab = &*VOCAB;
        let kw = |prop: &str, alias: &str| resolve_keyword(alias, vocab.prop(prop).unwrap());
        assert_eq!(kw("pos", "a"), Some("absolute"));
        assert_eq!(kw("pos", "s"), Some("static"));
        assert_eq!(kw("pos", "sti"), Some("sticky"));
        assert_eq!(kw("pos", "A"), Some("absolute"));
        assert_eq!(kw("d", "i"), Some("inline"));
        assert_eq!(kw("d", "ib"), Some("inline-block"));
        assert_eq!(kw("d", "tc"), Some("table-cell"));
        assert_eq!(kw("fw", "n"), Some("normal"));
        assert_eq!(kw("jc", "sb"), Some("space-between"));
        assert_eq!(kw("whs", "nw"), Some("nowrap"));
        assert_eq!(kw("bd", "s"), Some("solid"));
        assert_eq!(kw("w", "a"), Some("auto"));
        assert_eq!(kw("bgp", "c"), Some("center"));
        assert_eq!(kw("bgc", "t"), Some("transparent"));
        assert_eq!(kw("c", "cc"), Some("currentColor"));
        assert_eq!(kw("trf", "tx"), Some("translateX"));
        assert_eq!(kw("trstf", "cb"), Some("cubic-bezier"));
        assert_eq!(kw("bd", "red"), None);
        assert_eq!(kw("c", "red"), None);
        assert_eq!(kw("c", ""), None);
    }

    #[test]
    fn function_keywords() {
        let vocab = &*VOCAB;
        let gtc = vocab.prop("gtc").unwrap();
        assert_eq!(gtc.keywords, ["repeat", "minmax"]);
        assert_eq!(
            gtc.function("repeat"),
            Some(format!("repeat({CARET})").as_str())
        );
        assert_eq!(gtc.function("rep"), None);
        assert_eq!(function_name("translateX(${1:x})"), Some("translateX"));
        assert_eq!(function_name("'${0}'"), None);
        assert_eq!(function_name("url(${0}) x"), None);
    }

    #[test]
    fn sass_blocks() {
        assert_eq!(
            indented_block("@media ${1:screen} {\n\t${0}\n}"),
            "@media ${1:screen}\n\t${0}"
        );
        assert_eq!(indented_block("@import url(${0});"), "@import url(${0})");
    }

    #[test]
    fn tokens() {
        assert_eq!(
            tokenize("10--5px").unwrap(),
            vec![
                Token::Number {
                    text: "10".into(),
                    unit: None
                },
                Token::Number {
                    text: "-5".into(),
                    unit: Some("px".into())
                }
            ]
        );
        assert_eq!(tokenize("-a").unwrap(), vec![Token::Word("a".into())]);
        assert!(tokenize("10_5").is_none());
    }

    #[test]
    fn fields_and_literal_braces() {
        let raw = convert_fields("@media ${1:screen} {\n\t${0}\n}");
        assert_eq!(raw, format!("@media {} {{\n\t{CARET}\n}}", field("screen")));
        assert_eq!(
            convert_fields("${1:skewX(${2:angle})}"),
            field(&format!("skewX({})", field("angle")))
        );
    }

    #[test]
    fn spot_checks() {
        assert_eq!(css("text-align-center"), "text-align: center;");
        assert_eq!(css("ar16/9"), "aspect-ratio: 16 / 9;");
        assert_eq!(css("gc1/3"), "grid-column: 1 / 3;");
        assert_eq!(css("trs:a-.3s"), "transition: all .3s;");
        assert_eq!(
            css("bxsh0-0-5px-#000.2"),
            "box-shadow: 0 0 5px rgba(0, 0, 0, 0.2);"
        );
    }
}
