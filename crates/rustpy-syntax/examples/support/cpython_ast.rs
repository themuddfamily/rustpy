//! Independent CST mapping. Unsupported is a hard exporter error, never a null AST.
use rustpy_syntax::{ast::*, parse, SyntaxKind, SyntaxNode};
use serde_json::{json, Value};
#[path = "fstrings.rs"]
mod fstrings;
#[path = "literals.rs"]
mod literals;
#[path = "unicode15_ident.rs"]
mod unicode15_ident;

#[derive(Debug)]
pub struct Unsupported(pub String);
type Result<T> = std::result::Result<T, Unsupported>;
fn node(kind: &str, fields: Value) -> Value {
    json!({"node":kind,"fields":fields})
}
fn empty(kind: &str) -> Value {
    node(kind, json!({}))
}
fn missing<T>(value: Option<T>, field: &str) -> Result<T> {
    value.ok_or_else(|| Unsupported(format!("Missing typed field: {field}")))
}
fn unsupported(n: &SyntaxNode) -> Unsupported {
    Unsupported(format!("Unmapped {:?} at {:?}", n.kind(), n.text_range()))
}
fn name(text: String) -> Result<String> {
    literals::identifier(&text)
}
pub fn export(source: &str) -> Result<Option<Value>> {
    let parsed = parse(source);
    if !parsed.errors().is_empty() {
        return Ok(None);
    }
    let root = parsed.syntax_node();
    for token in root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if (token.kind() == SyntaxKind::IDENT && !unicode15_ident::identifier(token.text()))
            || (token.kind() == SyntaxKind::INT_NUMBER
                && literals::decimal_limit_exceeded(token.text()))
        {
            return Ok(None);
        }
    }
    let module = missing(Module::cast(root.clone()), "Module")?;
    // Ensure typed filtering cannot silently omit an unrecognized top-level node.
    for child in root.children() {
        if !Stmt::can_cast(child.kind()) {
            return Err(unsupported(&child));
        }
    }
    Ok(Some(node(
        "Module",
        json!({"body":statements(module.statements())?,"type_ignores":[]}),
    )))
}
fn statements(iter: impl Iterator<Item = Stmt>) -> Result<Vec<Value>> {
    iter.map(statement).collect()
}
fn body(block: Option<Block>) -> Result<Vec<Value>> {
    let block = missing(block, "body")?;
    for child in block.syntax().children() {
        if !Stmt::can_cast(child.kind()) {
            return Err(unsupported(&child));
        }
    }
    statements(block.statements())
}
fn otherwise(clause: Option<ElseClause>) -> Result<Vec<Value>> {
    clause
        .map(|clause| body(clause.body()))
        .transpose()
        .map(|v| v.unwrap_or_default())
}
fn optional(expr: Option<Expr>) -> Result<Value> {
    expr.map(|e| expression(e, "Load"))
        .transpose()
        .map(|v| v.unwrap_or(Value::Null))
}
fn required(expr: Option<Expr>, ctx: &str) -> Result<Value> {
    expression(missing(expr, "expression")?, ctx)
}

fn aliases(iter: impl Iterator<Item = ImportAlias>) -> Result<Vec<Value>> {
    iter.map(|alias| {
        let dotted = alias
            .name()
            .split('.')
            .map(|part| name(part.to_owned()))
            .collect::<Result<Vec<_>>>()?
            .join(".");
        let asname = alias
            .alias()
            .map(|token| name(token.text().to_owned()))
            .transpose()?;
        Ok(node("alias", json!({"name":dotted,"asname":asname})))
    })
    .collect()
}
fn decorators(iter: impl Iterator<Item = Decorator>) -> Result<Vec<Value>> {
    iter.map(|decorator| required(decorator.expr(), "Load"))
        .collect()
}
fn type_parameters(list: Option<TypeParamList>) -> Result<Vec<Value>> {
    let Some(list) = list else {
        return Ok(Vec::new());
    };
    list.syntax()
        .children()
        .map(|param| {
            // Python3.12 has no default_value field; newer defaults must not disappear.
            if param
                .descendants_with_tokens()
                .filter_map(|e| e.into_token())
                .any(|t| t.kind() == SyntaxKind::EQ)
            {
                return Err(Unsupported(
                    "Type parameter defaults exceed CPython3.12 target".into(),
                ));
            }
            let id = name(
                missing(token(&param, SyntaxKind::IDENT), "type parameter name")?
                    .text()
                    .to_owned(),
            )?;
            match param.kind() {
                SyntaxKind::TYPE_PARAM => {
                    let annotations: Vec<_> = children::<Annotation>(&param).collect();
                    if annotations.len() > 1 {
                        return Err(unsupported(&param));
                    }
                    let bound = annotations
                        .first()
                        .map(|a| required(a.expr(), "Load"))
                        .transpose()?
                        .unwrap_or(Value::Null);
                    Ok(node("TypeVar", json!({"name":id,"bound":bound})))
                }
                SyntaxKind::TYPE_VAR_TUPLE_PARAM => Ok(node("TypeVarTuple", json!({"name":id}))),
                SyntaxKind::PARAM_SPEC_PARAM => Ok(node("ParamSpec", json!({"name":id}))),
                _ => Err(unsupported(&param)),
            }
        })
        .collect()
}
fn argument(param: &AnyParam) -> Result<Value> {
    Ok(node(
        "arg",
        json!({"arg":name(missing(param.name(),"argument name")?.text().to_owned())?,"annotation":optional(param.annotation())?,"type_comment":null}),
    ))
}
fn arguments(list: ParamList) -> Result<Value> {
    for child in list.syntax().children() {
        if !AnyParam::can_cast(child.kind()) {
            return Err(unsupported(&child));
        }
    }
    let mut posonly = Vec::new();
    let mut args = Vec::new();
    let mut kwonly = Vec::new();
    let mut defaults = Vec::new();
    let mut kw_defaults = Vec::new();
    let mut vararg = Value::Null;
    let mut kwarg = Value::Null;
    let mut keyword_phase = false;
    let mut saw_default = false;
    let mut saw_slash = false;
    for param in list.params() {
        match &param {
            AnyParam::Plain(_) => {
                let default = param.default();
                if keyword_phase {
                    kwonly.push(argument(&param)?);
                    kw_defaults.push(optional(default)?);
                } else {
                    args.push(argument(&param)?);
                    if let Some(value) = default {
                        saw_default = true;
                        defaults.push(expression(value, "Load")?);
                    } else if saw_default {
                        return Err(Unsupported("Non-trailing positional default".into()));
                    }
                }
            }
            AnyParam::Slash(_) => {
                if saw_slash || keyword_phase {
                    return Err(Unsupported("Invalid positional-only boundary".into()));
                }
                saw_slash = true;
                posonly.append(&mut args);
            }
            AnyParam::Star(_) => {
                if keyword_phase {
                    return Err(Unsupported("Repeated star argument boundary".into()));
                }
                vararg = argument(&param)?;
                keyword_phase = true;
            }
            AnyParam::StarSep(_) => {
                if keyword_phase {
                    return Err(Unsupported("Repeated keyword-only boundary".into()));
                }
                keyword_phase = true;
            }
            AnyParam::StarStar(_) => {
                if !kwarg.is_null() {
                    return Err(Unsupported("Repeated kwargs".into()));
                }
                kwarg = argument(&param)?;
                keyword_phase = true;
            }
        }
    }
    Ok(node(
        "arguments",
        json!({"posonlyargs":posonly,"args":args,"vararg":vararg,"kwonlyargs":kwonly,"kw_defaults":kw_defaults,"kwarg":kwarg,"defaults":defaults}),
    ))
}
fn class_arguments(list: Option<ArgList>) -> Result<(Vec<Value>, Vec<Value>)> {
    let Some(list) = list else {
        return Ok((Vec::new(), Vec::new()));
    };
    for child in list.syntax().children() {
        if !AnyArg::can_cast(child.kind()) {
            return Err(unsupported(&child));
        }
    }
    let mut bases = Vec::new();
    let mut keywords = Vec::new();
    for arg in list.args() {
        match arg {
            AnyArg::Positional(a) => bases.push(required(a.expr(), "Load")?),
            AnyArg::Star(a) => bases.push(node(
                "Starred",
                json!({"value":required(child::<Expr>(a.syntax()),"Load")?,"ctx":empty("Load")}),
            )),
            AnyArg::Keyword(a) => keywords.push(node(
                "keyword",
                json!({"arg":name(a.name_text())?,"value":required(a.value(),"Load")?}),
            )),
            AnyArg::StarStar(a) => keywords.push(node(
                "keyword",
                json!({"arg":null,"value":required(child::<Expr>(a.syntax()),"Load")?}),
            )),
            other => return Err(unsupported(other.syntax())),
        }
    }
    Ok((bases, keywords))
}

fn comprehensions(iter: impl Iterator<Item = CompFor>) -> Result<Vec<Value>> {
    iter.map(|clause| Ok(node("comprehension",json!({"target":required(clause.target(),"Store")?,"iter":required(clause.iter(),"Load")?,"ifs":clause.conditions().map(|condition|expression(condition,"Load")).collect::<Result<Vec<_>>>()?,"is_async":i32::from(clause.is_async())})))).collect()
}

fn statement(stmt: Stmt) -> Result<Value> {
    Ok(match stmt {
        Stmt::Expr(s) => node("Expr", json!({"value":required(s.expr(),"Load")?})),
        Stmt::Assign(s) => node(
            "Assign",
            json!({"targets":s.targets().into_iter().map(|e|expression(e,"Store")).collect::<Result<Vec<_>>>()?,"value":required(s.value(),"Load")?,"type_comment":null}),
        ),
        Stmt::AugAssign(s) => node(
            "AugAssign",
            json!({"target":required(s.target(),"Store")?,"op":operator(missing(s.op(),"augmented operator")?.text().trim_end_matches('='))?,"value":required(s.value(),"Load")?}),
        ),
        Stmt::AnnAssign(s) => {
            let target = missing(s.target(), "annotated target")?;
            let simple = i32::from(matches!(&target, Expr::Name(_)));
            node(
                "AnnAssign",
                json!({"target":expression(target,"Store")?,"annotation":required(s.annotation(),"Load")?,"value":optional(s.value())?,"simple":simple}),
            )
        }
        Stmt::For(s) => node(
            if s.is_async() { "AsyncFor" } else { "For" },
            json!({"target":required(s.target(),"Store")?,"iter":required(s.iter(),"Load")?,"body":body(s.body())?,"orelse":otherwise(s.else_clause())?,"type_comment":null}),
        ),
        Stmt::Return(s) => node("Return", json!({"value":optional(s.value())?})),
        Stmt::Pass(_) => empty("Pass"),
        Stmt::Break(_) => empty("Break"),
        Stmt::Continue(_) => empty("Continue"),
        Stmt::Del(s) => node(
            "Delete",
            json!({"targets":s.targets().into_iter().map(|e|expression(e,"Del")).collect::<Result<Vec<_>>>()?}),
        ),
        Stmt::Assert(s) => node(
            "Assert",
            json!({"test":required(s.test(),"Load")?,"msg":optional(s.message())?}),
        ),
        Stmt::Raise(s) => node(
            "Raise",
            json!({"exc":optional(s.exception())?,"cause":optional(s.cause())?}),
        ),
        Stmt::Global(s) => node(
            "Global",
            json!({"names":s.names().map(|t|name(t.text().to_string())).collect::<Result<Vec<_>>>()?}),
        ),
        Stmt::Nonlocal(s) => node(
            "Nonlocal",
            json!({"names":s.names().map(|t|name(t.text().to_string())).collect::<Result<Vec<_>>>()?}),
        ),
        Stmt::If(s) => {
            let mut tail = otherwise(s.else_clause())?;
            let clauses: Vec<_> = s.elif_clauses().collect();
            for clause in clauses.into_iter().rev() {
                tail = vec![node(
                    "If",
                    json!({"test":required(clause.condition(),"Load")?,"body":body(clause.body())?,"orelse":tail}),
                )];
            }
            node(
                "If",
                json!({"test":required(s.condition(),"Load")?,"body":body(s.body())?,"orelse":tail}),
            )
        }
        Stmt::While(s) => node(
            "While",
            json!({"test":required(s.condition(),"Load")?,"body":body(s.body())?,"orelse":otherwise(s.else_clause())?}),
        ),
        Stmt::Import(s) => node("Import", json!({"names":aliases(s.aliases())?})),
        Stmt::ImportFrom(s) => {
            let module = s
                .module()
                .map(|module| {
                    module
                        .segments()
                        .map(|segment| name(segment.text().to_owned()))
                        .collect::<Result<Vec<_>>>()
                        .map(|parts| parts.join("."))
                })
                .transpose()?;
            let names = if s.is_star() {
                vec![node("alias", json!({"name":"*","asname":null}))]
            } else {
                aliases(s.aliases())?
            };
            node(
                "ImportFrom",
                json!({"module":module,"names":names,"level":s.level()}),
            )
        }
        Stmt::FuncDef(s) => node(
            if s.is_async() {
                "AsyncFunctionDef"
            } else {
                "FunctionDef"
            },
            json!({"name":name(s.name_text())?,"args":arguments(missing(s.params(),"function parameters")?)?,"body":body(s.body())?,"decorator_list":decorators(s.decorators())?,"returns":optional(s.return_annotation())?,"type_comment":null,"type_params":type_parameters(s.type_params())?}),
        ),
        Stmt::ClassDef(s) => {
            let (bases, keywords) = class_arguments(s.arguments())?;
            node(
                "ClassDef",
                json!({"name":name(s.name_text())?,"bases":bases,"keywords":keywords,"body":body(s.body())?,"decorator_list":decorators(s.decorators())?,"type_params":type_parameters(s.type_params())?}),
            )
        }
        Stmt::With(s) => node(
            if s.is_async() { "AsyncWith" } else { "With" },
            json!({"items":s.items().map(|item|Ok(node("withitem",json!({"context_expr":required(item.context_expr(),"Load")?,"optional_vars":item.target().map(|target|expression(target,"Store")).transpose()?.unwrap_or(Value::Null)})))).collect::<Result<Vec<_>>>()?,"body":body(s.body())?,"type_comment":null}),
        ),
        Stmt::Try(s) => {
            let clauses: Vec<_> = s.except_clauses().collect();
            let starred = clauses.iter().any(ExceptClause::is_star);
            if starred && clauses.iter().any(|clause| !clause.is_star()) {
                return Err(Unsupported("Mixed except/except* clauses".into()));
            }
            let handlers=clauses.into_iter().map(|clause|Ok(node("ExceptHandler",json!({"type":optional(clause.exception_type())?,"name":clause.name().map(|token|name(token.text().to_owned())).transpose()?,"body":body(clause.body())?})))).collect::<Result<Vec<_>>>()?;
            let finalbody = s
                .finally_clause()
                .map(|clause| body(clause.body()))
                .transpose()?
                .unwrap_or_default();
            node(
                if starred { "TryStar" } else { "Try" },
                json!({"body":body(s.body())?,"handlers":handlers,"orelse":otherwise(s.else_clause())?,"finalbody":finalbody}),
            )
        }
        Stmt::Match(s) => node(
            "Match",
            json!({"subject":required(s.subject(),"Load")?,"cases":s.cases().map(|case|Ok(node("match_case",json!({"pattern":pattern(&missing(case.pattern(),"case pattern")?)?,"guard":optional(case.guard())?,"body":body(case.body())?})))).collect::<Result<Vec<_>>>()?}),
        ),
        Stmt::TypeAlias(s) => node(
            "TypeAlias",
            json!({"name":node("Name",json!({"id":name(missing(s.name(),"typealias name")?.text().to_owned())?,"ctx":empty("Store")})),"type_params":type_parameters(child::<TypeParamList>(s.syntax()))?,"value":required(s.value(),"Load")?}),
        ),
    })
}
// Pattern wrappers carry direct tokens unlike ordinary expression nodes.
// Walk their actual children in source order; never reconstruct a Python AST
// by calling Python or silently omit an unrecognized pattern family.
fn pattern_child(n: &SyntaxNode) -> Result<SyntaxNode> {
    let mut children = n.children().filter(|c| c.kind().is_pattern());
    let first = missing(children.next(), "nested pattern")?;
    if children.next().is_some() {
        return Err(Unsupported("Expected one nested pattern".into()));
    }
    Ok(first)
}
fn direct_tokens(n: &SyntaxNode) -> Vec<rustpy_syntax::SyntaxToken> {
    n.children_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia())
        .collect()
}
fn pattern_name(n: &SyntaxNode) -> Result<String> {
    name(
        missing(token(n, SyntaxKind::IDENT), "pattern name")?
            .text()
            .to_owned(),
    )
}
fn dotted_value(tokens: &[rustpy_syntax::SyntaxToken]) -> Result<Value> {
    if tokens.is_empty() || tokens.len().is_multiple_of(2) {
        return Err(Unsupported("Malformed dotted pattern value".into()));
    }
    let mut value = Value::Null;
    for (i, token) in tokens.iter().enumerate() {
        if !i.is_multiple_of(2) {
            if token.kind() != SyntaxKind::DOT {
                return Err(Unsupported("Expected pattern attribute separator".into()));
            }
        } else {
            if token.kind() != SyntaxKind::IDENT {
                return Err(Unsupported("Expected pattern attribute name".into()));
            }
            let id = name(token.text().to_owned())?;
            value = if i == 0 {
                node("Name", json!({"id":id,"ctx":empty("Load")}))
            } else {
                node(
                    "Attribute",
                    json!({"value":value,"attr":id,"ctx":empty("Load")}),
                )
            };
        }
    }
    Ok(value)
}
fn pattern_value(n: &SyntaxNode, mapping_key: bool) -> Result<Value> {
    let mut tokens = Vec::new();
    let mut expr = None;
    for element in n.children_with_tokens() {
        if let Some(token) = element.as_token() {
            if mapping_key && token.kind() == SyntaxKind::COLON {
                break;
            }
            if !token.kind().is_trivia() {
                tokens.push(token.clone());
            }
        } else if let Some(child) = element.as_node() {
            if expr.is_some() {
                return Err(Unsupported("Multiple pattern value expressions".into()));
            }
            expr = Some(missing(
                Expr::cast(child.clone()),
                "pattern literal expression",
            )?);
        }
    }
    if let Some(expr) = expr {
        if !tokens.is_empty() {
            return Err(Unsupported(
                "Mixed pattern literal and direct tokens".into(),
            ));
        }
        return expression(expr, "Load");
    }
    if tokens
        .first()
        .is_some_and(|t| t.kind() == SyntaxKind::IDENT)
    {
        return dotted_value(&tokens);
    }
    let negative = tokens
        .first()
        .is_some_and(|t| t.kind() == SyntaxKind::MINUS);
    let number = &tokens[usize::from(negative)..];
    if number.len() == 3
        && matches!(number[1].kind(), SyntaxKind::PLUS | SyntaxKind::MINUS)
        && number[2].kind() == SyntaxKind::COMPLEX_NUMBER
    {
        let real = match number[0].kind() {
            SyntaxKind::INT_NUMBER => literals::integer(number[0].text())?,
            SyntaxKind::FLOAT_NUMBER => literals::float(number[0].text(), false)?,
            _ => return Err(unsupported(n)),
        };
        let left = node("Constant", json!({"value":real,"kind":null}));
        let left = if negative {
            node("UnaryOp", json!({"op":empty("USub"),"operand":left}))
        } else {
            left
        };
        let right = node(
            "Constant",
            json!({"value":literals::float(number[2].text(),true)?,"kind":null}),
        );
        return Ok(node(
            "BinOp",
            json!({"left":left,"op":empty(if number[1].kind()==SyntaxKind::PLUS{"Add"}else{"Sub"}),"right":right}),
        ));
    }
    if number.len() != 1 {
        return Err(Unsupported(
            "Complex/signed pattern literal not mapped".into(),
        ));
    }
    let token = &number[0];
    let value = match token.kind() {
        SyntaxKind::TRUE_KW if !negative => json!(true),
        SyntaxKind::FALSE_KW if !negative => json!(false),
        SyntaxKind::NONE_KW if !negative => Value::Null,
        SyntaxKind::INT_NUMBER => literals::integer(token.text())?,
        SyntaxKind::FLOAT_NUMBER => literals::float(token.text(), false)?,
        SyntaxKind::COMPLEX_NUMBER => literals::float(token.text(), true)?,
        _ => return Err(unsupported(n)),
    };
    let constant = node("Constant", json!({"value":value,"kind":null}));
    Ok(if negative {
        node("UnaryOp", json!({"op":empty("USub"),"operand":constant}))
    } else {
        constant
    })
}
fn pattern(n: &SyntaxNode) -> Result<Value> {
    use SyntaxKind::*;
    Ok(match n.kind() {
        MATCH_CAPTURE_PATTERN => node("MatchAs", json!({"pattern":null,"name":pattern_name(n)?})),
        MATCH_WILDCARD_PATTERN => node("MatchAs", json!({"pattern":null,"name":null})),
        MATCH_SINGLETON_PATTERN => node(
            "MatchSingleton",
            json!({"value":match direct_tokens(n).first().map(|t|t.kind()) {
                Some(NONE_KW)=>Value::Null, Some(TRUE_KW)=>json!(true), Some(FALSE_KW)=>json!(false), _=>return Err(unsupported(n))
            }}),
        ),
        MATCH_VALUE_PATTERN => node("MatchValue", json!({"value":pattern_value(n,false)?})),
        MATCH_GROUP_PATTERN => pattern(&pattern_child(n)?)?,
        MATCH_AS_PATTERN => node(
            "MatchAs",
            json!({"pattern":pattern(&pattern_child(n)?)?,"name":pattern_name(n)?}),
        ),
        MATCH_STAR_PATTERN => node(
            "MatchStar",
            json!({"name":if token(n,UNDERSCORE_KW).is_some(){Value::Null}else{json!(pattern_name(n)?)}}),
        ),
        MATCH_SEQUENCE_PATTERN | MATCH_OR_PATTERN => {
            let patterns = n
                .children()
                .map(|child| pattern(&child))
                .collect::<Result<Vec<_>>>()?;
            if n.kind() == MATCH_OR_PATTERN && patterns.len() < 2 {
                return Err(Unsupported("OR pattern cardinality".into()));
            }
            node(
                if n.kind() == MATCH_OR_PATTERN {
                    "MatchOr"
                } else {
                    "MatchSequence"
                },
                json!({"patterns":patterns}),
            )
        }
        MATCH_MAPPING_PATTERN => {
            let (mut keys, mut patterns, mut rest) = (Vec::new(), Vec::new(), None);
            for child in n.children() {
                match child.kind() {
                    MATCH_MAPPING_ITEM if rest.is_none() => {
                        keys.push(pattern_value(&child, true)?);
                        patterns.push(pattern(&pattern_child(&child)?)?);
                    }
                    MATCH_MAPPING_REST if rest.is_none() => rest = Some(pattern_name(&child)?),
                    _ => return Err(unsupported(&child)),
                }
            }
            node(
                "MatchMapping",
                json!({"keys":keys,"patterns":patterns,"rest":rest}),
            )
        }
        MATCH_CLASS_PATTERN => {
            let tokens = direct_tokens(n);
            let end = missing(
                tokens.iter().position(|t| t.kind() == L_PAREN),
                "class pattern delimiter",
            )?;
            let cls = dotted_value(&tokens[..end])?;
            let (mut patterns, mut attrs, mut kw_patterns) = (Vec::new(), Vec::new(), Vec::new());
            for child in n.children() {
                match child.kind() {
                    MATCH_CLASS_ARG if attrs.is_empty() => {
                        patterns.push(pattern(&pattern_child(&child)?)?)
                    }
                    MATCH_KEYWORD_PATTERN => {
                        attrs.push(pattern_name(&child)?);
                        kw_patterns.push(pattern(&pattern_child(&child)?)?);
                    }
                    _ => return Err(unsupported(&child)),
                }
            }
            node(
                "MatchClass",
                json!({"cls":cls,"patterns":patterns,"kwd_attrs":attrs,"kwd_patterns":kw_patterns}),
            )
        }
        _ => return Err(unsupported(n)),
    })
}

fn operator(text: &str) -> Result<Value> {
    let kind = match text {
        "+" => "Add",
        "-" => "Sub",
        "*" => "Mult",
        "/" => "Div",
        "//" => "FloorDiv",
        "%" => "Mod",
        "**" => "Pow",
        "@" => "MatMult",
        "<<" => "LShift",
        ">>" => "RShift",
        "&" => "BitAnd",
        "|" => "BitOr",
        "^" => "BitXor",
        _ => return Err(Unsupported(format!("Unmapped binary operator {text}"))),
    };
    Ok(empty(kind))
}
fn concat_strings(expr: ConcatStringExpr) -> Result<literals::Decoded> {
    let mut result = None;
    for part in expr.parts() {
        let value = match part {
            Expr::String(s) => literals::string(&s.raw_text())?,
            other => return Err(unsupported(other.syntax())),
        };
        result = Some(match result {
            None => value,
            Some(previous) => literals::append(previous, value)?,
        });
    }
    missing(result, "concatenated strings")
}
fn subscript_index(index: SyntaxNode) -> Result<Value> {
    if index.kind() == SyntaxKind::SLICE_LIST {
        let elts = index
            .children()
            .map(|child| expression(missing(Expr::cast(child), "slice-list item")?, "Load"))
            .collect::<Result<Vec<_>>>()?;
        return Ok(node("Tuple", json!({"elts":elts,"ctx":empty("Load")})));
    }
    let expr = missing(Expr::cast(index), "subscript expression")?;
    let value = expression(expr.clone(), "Load")?;
    if matches!(expr.unparen(), Expr::Star(_)) {
        Ok(node("Tuple", json!({"elts":[value],"ctx":empty("Load")})))
    } else {
        Ok(value)
    }
}

fn formatted_string(value: Expr) -> Result<Value> {
    fstrings::expression(
        value,
        &mut |value| expression(value, "Load"),
        &mut |payload, raw| literals::decode_text_value(payload, raw),
        &mut |value| match value {
            Expr::String(s) => match literals::string(&s.raw_text())? {
                literals::Decoded::Text(text, kind) => Ok((Value::String(text), kind)),
                literals::Decoded::SurrogateText(points, kind) => {
                    Ok((json!({"literal_type":"str","codepoints":points}), kind))
                }
                literals::Decoded::Bytes(_) => {
                    Err(Unsupported("Bytes mixed with formatted text".into()))
                }
            },
            _ => Err(Unsupported("Non-literal adjacent formatted part".into())),
        },
    )
}

fn expression(expr: Expr, ctx: &str) -> Result<Value> {
    let expr = expr.unparen();
    if ctx != "Load"
        && !matches!(
            &expr,
            Expr::Name(_)
                | Expr::Tuple(_)
                | Expr::List(_)
                | Expr::Attribute(_)
                | Expr::Subscript(_)
                | Expr::Star(_)
        )
    {
        return Err(unsupported(expr.syntax()));
    }
    Ok(match expr {
        Expr::Name(e) => node("Name", json!({"id":name(e.name_text())?,"ctx":empty(ctx)})),
        Expr::Tuple(e) => node(
            "Tuple",
            json!({"elts":e.elements().map(|e|expression(e,ctx)).collect::<Result<Vec<_>>>()?,"ctx":empty(ctx)}),
        ),
        Expr::List(e) => node(
            "List",
            json!({"elts":e.elements().map(|e|expression(e,ctx)).collect::<Result<Vec<_>>>()?,"ctx":empty(ctx)}),
        ),
        Expr::Attribute(e) => node(
            "Attribute",
            json!({"value":required(e.object(),"Load")?,"attr":name(e.attr_text())?,"ctx":empty(ctx)}),
        ),
        Expr::Subscript(e) => node(
            "Subscript",
            json!({"value":required(e.object(),"Load")?,"slice":subscript_index(missing(e.index(),"subscript index")?)?,"ctx":empty(ctx)}),
        ),
        Expr::Star(e) => node(
            "Starred",
            json!({"value":required(e.expr(),ctx)?,"ctx":empty(ctx)}),
        ),
        Expr::Literal(e) => {
            let token = missing(e.token(), "literal token")?;
            let value = match token.kind() {
                SyntaxKind::TRUE_KW => json!(true),
                SyntaxKind::FALSE_KW => json!(false),
                SyntaxKind::NONE_KW => Value::Null,
                SyntaxKind::ELLIPSIS => json!({"literal_type":"ellipsis","repr":"Ellipsis"}),
                SyntaxKind::INT_NUMBER => literals::integer(token.text())?,
                SyntaxKind::FLOAT_NUMBER => literals::float(token.text(), false)?,
                SyntaxKind::COMPLEX_NUMBER => literals::float(token.text(), true)?,
                _ => return Err(unsupported(e.syntax())),
            };
            node("Constant", json!({"value":value,"kind":null}))
        }
        Expr::String(e) => node(
            "Constant",
            literals::constant(literals::string(&e.raw_text())?),
        ),
        Expr::FString(e) => formatted_string(Expr::FString(e))?,
        Expr::ConcatString(e) if e.parts().any(|p| matches!(p, Expr::FString(_))) => {
            formatted_string(Expr::ConcatString(e))?
        }
        Expr::ConcatString(e) => node("Constant", literals::constant(concat_strings(e)?)),
        Expr::BoolOp(e) => {
            // Parser conjunction/disjunction already collect one flat syntactic chain.
            // Explicit parentheses create PAREN_EXPR and must retain nested BoolOps.
            let values = e
                .operands()
                .map(|value| expression(value, "Load"))
                .collect::<Result<Vec<_>>>()?;
            if values.len() < 2 {
                return Err(Unsupported("Boolean operator operand cardinality".into()));
            }
            node(
                "BoolOp",
                json!({"op":empty(if e.is_or(){"Or"}else{"And"}),"values":values}),
            )
        }
        Expr::Set(e) => node(
            "Set",
            json!({"elts":e.elements().map(|value|expression(value,"Load")).collect::<Result<Vec<_>>>()?}),
        ),
        Expr::Dict(e) => {
            let mut keys = Vec::new();
            let mut values = Vec::new();
            for entry in e.syntax().children() {
                match entry.kind() {
                    SyntaxKind::DICT_ITEM => {
                        let item = missing(DictItem::cast(entry), "dictionary item")?;
                        keys.push(required(item.key(), "Load")?);
                        values.push(required(item.value(), "Load")?);
                    }
                    SyntaxKind::DICT_UNPACK => {
                        let unpack = missing(DictUnpack::cast(entry), "dictionary unpack")?;
                        keys.push(Value::Null);
                        values.push(required(unpack.expr(), "Load")?);
                    }
                    _ => return Err(unsupported(&entry)),
                }
            }
            node("Dict", json!({"keys":keys,"values":values}))
        }
        Expr::Lambda(e) => node(
            "Lambda",
            json!({"args":arguments(missing(e.params(),"lambda parameters")?)?,"body":required(e.body(),"Load")?}),
        ),
        Expr::Named(e) => node(
            "NamedExpr",
            json!({"target":expression(Expr::Name(missing(e.target(),"named expression target")?),"Store")?,"value":required(e.value(),"Load")?}),
        ),
        Expr::ListComp(e) => node(
            "ListComp",
            json!({"elt":required(e.element(),"Load")?,"generators":comprehensions(e.clauses())?}),
        ),
        Expr::SetComp(e) => node(
            "SetComp",
            json!({"elt":required(e.element(),"Load")?,"generators":comprehensions(e.clauses())?}),
        ),
        Expr::Generator(e) => node(
            "GeneratorExp",
            json!({"elt":required(e.element(),"Load")?,"generators":comprehensions(e.clauses())?}),
        ),
        Expr::DictComp(e) => {
            let item = missing(e.item(), "dictionary comprehension item")?;
            node(
                "DictComp",
                json!({"key":required(item.key(),"Load")?,"value":required(item.value(),"Load")?,"generators":comprehensions(e.clauses())?}),
            )
        }
        Expr::Call(e) => {
            let mut args = Vec::new();
            let mut keywords = Vec::new();
            let list = missing(e.arg_list(), "call arguments")?;
            for child in list.syntax().children() {
                if !AnyArg::can_cast(child.kind()) {
                    return Err(unsupported(&child));
                }
            }
            for arg in list.args() {
                match arg {
                AnyArg::Positional(a)=>args.push(required(a.expr(),"Load")?),
                AnyArg::Star(a)=>args.push(node("Starred",json!({"value":required(child::<Expr>(a.syntax()),"Load")?,"ctx":empty("Load")}))),
                AnyArg::Keyword(a)=>keywords.push(node("keyword",json!({"arg":name(a.name_text())?,"value":required(a.value(),"Load")?}))),
                AnyArg::StarStar(a)=>keywords.push(node("keyword",json!({"arg":null,"value":required(child::<Expr>(a.syntax()),"Load")?}))),
                AnyArg::Generator(generator)=>args.push(expression(Expr::Generator(generator),"Load")?),
            }
            }
            node(
                "Call",
                json!({"func":required(e.callee(),"Load")?,"args":args,"keywords":keywords}),
            )
        }
        Expr::Compare(e) => {
            let mut operands = e.operands();
            let first = required(operands.next(), "Load")?;
            let ops = e
                .ops()
                .map(|op| {
                    let text = op.text();
                    Ok(empty(match text.as_str() {
                        "==" => "Eq",
                        "!=" => "NotEq",
                        "<" => "Lt",
                        "<=" => "LtE",
                        ">" => "Gt",
                        ">=" => "GtE",
                        "is" => "Is",
                        "is not" => "IsNot",
                        "in" => "In",
                        "not in" => "NotIn",
                        _ => return Err(Unsupported(format!("Unmapped comparison {text}"))),
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            let comparators = operands
                .map(|e| expression(e, "Load"))
                .collect::<Result<Vec<_>>>()?;
            if ops.len() != comparators.len() {
                return Err(Unsupported("Comparison operand cardinality".into()));
            }
            node(
                "Compare",
                json!({"left":first,"ops":ops,"comparators":comparators}),
            )
        }
        Expr::Binary(e) => node(
            "BinOp",
            json!({"left":required(e.lhs(),"Load")?,"op":operator(missing(e.op(),"binary operator")?.text())?,"right":required(e.rhs(),"Load")?}),
        ),
        Expr::Unary(e) => {
            let token = missing(e.op(), "unary operator")?;
            let kind = match token.text() {
                "+" => "UAdd",
                "-" => "USub",
                "~" => "Invert",
                _ => return Err(unsupported(e.syntax())),
            };
            node(
                "UnaryOp",
                json!({"op":empty(kind),"operand":required(e.operand(),"Load")?}),
            )
        }
        Expr::Not(e) => node(
            "UnaryOp",
            json!({"op":empty("Not"),"operand":required(e.operand(),"Load")?}),
        ),
        Expr::Conditional(e) => node(
            "IfExp",
            json!({"test":required(e.condition(),"Load")?,"body":required(e.true_expr(),"Load")?,"orelse":required(e.false_expr(),"Load")?}),
        ),
        Expr::Await(e) => node("Await", json!({"value":required(e.expr(),"Load")?})),
        Expr::Yield(e) => node("Yield", json!({"value":optional(e.value())?})),
        Expr::YieldFrom(e) => node("YieldFrom", json!({"value":required(e.expr(),"Load")?})),
        Expr::Slice(e) => {
            let (lower, upper, step) = e.parts();
            node(
                "Slice",
                json!({"lower":optional(lower)?,"upper":optional(upper)?,"step":optional(step)?}),
            )
        }
        other => return Err(unsupported(other.syntax())),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn primary_rejected_identifier_and_decimal_admission() {
        let fixtures: Value = serde_json::from_str(include_str!("rejected-goldens.json")).unwrap();
        for fixture in fixtures["fixtures"].as_array().unwrap() {
            assert!(
                export(fixture["source"].as_str().unwrap())
                    .unwrap()
                    .is_none(),
                "{}",
                fixture["reason"]
            );
        }
    }
    #[test]
    fn independent_fixed_cpython_goldens_preserve_context_and_statement_structure() {
        let fixtures: Value = serde_json::from_str(include_str!("fixed-goldens.json")).unwrap();
        for fixture in fixtures.as_array().unwrap() {
            assert_eq!(
                export(fixture["source"].as_str().unwrap())
                    .unwrap()
                    .unwrap(),
                fixture["ast"],
                "{}",
                fixture["source"]
            );
        }
    }
    #[test]
    fn unsupported_semantics_are_errors_not_fabricated_null_fields() {
        for source in ["x = '\\U00110000'"] {
            assert!(export(source).is_err(), "{source}");
        }
        assert!(export("x = (").unwrap().is_none());
    }
}
