//! Change Signature (roadmap §2.8): a working copy of a function's
//! parameters that the dialog edits, applied in one step to the definition,
//! its body and docstring, and every call site in the project.
//!
//! Call sites are rebuilt from the original binding of each argument to a
//! parameter: positional arguments stay positional while they can, keyword
//! arguments keep their keyword (renamed with the parameter), removed
//! parameters lose their argument, new ones get the value chosen for call
//! sites (also when they have a default, which then serves new calls only),
//! and a parameter that loses its default gets the old default value at
//! calls that relied on it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

use crate::refactor2::MultiFileRefactoring;
use rustpy_search::usages::function_call_sites;
use rustpy_semantic::calls::CallSite;
use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::edit_helpers::{
    ancestor_at, apply_edits, indent_at, is_identifier, node_with_range, slice, Edit,
};
use rustpy_semantic::scope::BindingKind;
pub use rustpy_semantic::types::ParamKind;

/// One parameter of the edited signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigParam {
    pub name: String,
    pub kind: ParamKind,
    /// Default value in the signature (source text).
    pub default: Option<String>,
    /// Annotation (source text).
    pub annotation: Option<String>,
    /// Index of the parameter in the original signature; `None` when added.
    pub origin: Option<usize>,
    /// For an added parameter: the value existing calls pass (source text).
    /// With a default as well, existing calls still pass this value; the
    /// default serves new calls.
    pub call_value: Option<String>,
}

impl SigParam {
    /// A new parameter (`call_value` is what existing calls will pass).
    pub fn added(name: &str, default: Option<&str>, call_value: Option<&str>) -> Self {
        let clean = |s: Option<&str>| {
            s.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Self {
            name: name.trim().to_string(),
            kind: ParamKind::Normal,
            default: clean(default),
            annotation: None,
            origin: None,
            call_value: clean(call_value),
        }
    }

    fn render(&self) -> String {
        let prefix = match self.kind {
            ParamKind::VarArgs => "*",
            ParamKind::KwArgs => "**",
            _ => "",
        };
        let mut s = format!("{prefix}{}", self.name);
        if let Some(a) = &self.annotation {
            s.push_str(": ");
            s.push_str(a);
        }
        if let Some(d) = &self.default {
            s.push_str(if self.annotation.is_some() {
                " = "
            } else {
                "="
            });
            s.push_str(d.trim());
        }
        s
    }
}

fn positional(kind: ParamKind) -> bool {
    matches!(kind, ParamKind::PositionalOnly | ParamKind::Normal)
}

/// The signature being changed: the original parameters and the edited ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureModel {
    pub func_name: String,
    /// The `def` when the model was taken (to find it again).
    pub def_range: TextRange,
    pub original: Vec<SigParam>,
    pub params: Vec<SigParam>,
    /// The first parameter (`self` / `cls`) is passed implicitly: it stays first.
    pub implicit_first: bool,
    pub original_return_annotation: Option<String>,
    pub return_annotation: Option<String>,
    /// Update resolved overriding declarations together with their calls.
    pub propagate_overrides: bool,
}

impl SignatureModel {
    /// The edited parameter list as it will be written (`a, b=1, *, c`).
    pub fn render(&self) -> String {
        render_params(&self.params).join(", ")
    }

    pub fn is_changed(&self) -> bool {
        self.params != self.original || self.return_annotation != self.original_return_annotation
    }
}

fn render_params(params: &[SigParam]) -> Vec<String> {
    let mut out = Vec::new();
    let has_var = params.iter().any(|p| p.kind == ParamKind::VarArgs);
    let mut star_written = false;
    for (i, p) in params.iter().enumerate() {
        if p.kind == ParamKind::KeywordOnly && !has_var && !star_written {
            out.push("*".to_string());
            star_written = true;
        }
        out.push(p.render());
        if p.kind == ParamKind::PositionalOnly
            && params
                .get(i + 1)
                .is_none_or(|n| n.kind != ParamKind::PositionalOnly)
        {
            out.push("/".to_string());
        }
    }
    out
}

/// The signature of the function around `offset`.
pub fn signature_model(file: &FileInfo, offset: TextSize) -> Result<SignatureModel, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let func = ancestor_at::<ast::FuncDef>(&file.parse.syntax_node(), offset)
        .ok_or("Place the caret inside a function")?;
    model_of(&func)
}

/// Resolve a declaration from a reference (including imported and bound calls),
/// otherwise use the function containing the caret.
pub fn signature_target(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<(Arc<FileInfo>, SignatureModel), String> {
    let mut targets = Vec::new();
    for location in rustpy_semantic::features::definition_at(db, file, offset) {
        let Some(source) = db.file_for_path(&location.path) else {
            continue;
        };
        let Some(function) =
            ancestor_at::<ast::FuncDef>(&source.parse.syntax_node(), location.range.start())
        else {
            continue;
        };
        // A class or a local assigned inside a function is not a function target.
        if function
            .name()
            .is_none_or(|name| name.text_range() != location.range)
        {
            continue;
        }
        if !targets
            .iter()
            .any(|(existing, model): &(Arc<FileInfo>, SignatureModel)| {
                existing.id == source.id && model.def_range == function.syntax().text_range()
            })
        {
            targets.push((source, model_of(&function)?));
        }
    }
    if targets.len() > 1 {
        return Err(
            "The reference resolves to multiple function declarations; choose a declaration first"
                .into(),
        );
    }
    if let Some((source, model)) = targets.pop() {
        if source.is_stub {
            return Err("This declaration is in a stub; navigate to its source before changing the signature".into());
        }
        return Ok((source, model));
    }
    Ok((file.clone(), signature_model(file, offset)?))
}

fn model_of(func: &ast::FuncDef) -> Result<SignatureModel, String> {
    let params = func.params().ok_or("The function has no parameter list")?;
    let mut list: Vec<SigParam> = Vec::new();
    let mut keyword_only = false;
    let mut slash_at = None;
    for p in params.params() {
        let kind = match &p {
            ast::AnyParam::Slash(_) => {
                slash_at = Some(list.len());
                continue;
            }
            ast::AnyParam::StarSep(_) => {
                keyword_only = true;
                continue;
            }
            ast::AnyParam::Star(_) => {
                keyword_only = true;
                ParamKind::VarArgs
            }
            ast::AnyParam::StarStar(_) => ParamKind::KwArgs,
            ast::AnyParam::Plain(_) if keyword_only => ParamKind::KeywordOnly,
            ast::AnyParam::Plain(_) => ParamKind::Normal,
        };
        let name = p
            .name()
            .map(|t| t.text().to_string())
            .ok_or("Malformed parameter list")?;
        let text = |e: ast::Expr| e.syntax().text().to_string().trim().to_string();
        list.push(SigParam {
            name,
            kind,
            default: p.default().map(text),
            annotation: p.annotation().map(text),
            origin: Some(list.len()),
            call_value: None,
        });
    }
    if let Some(n) = slash_at {
        for p in list.iter_mut().take(n) {
            p.kind = ParamKind::PositionalOnly;
        }
    }
    let implicit_first = func.is_method()
        && !func.has_decorator("staticmethod")
        && list.first().is_some_and(|p| positional(p.kind));
    Ok(SignatureModel {
        func_name: func.name_text(),
        def_range: func.syntax().text_range(),
        original: list.clone(),
        params: list,
        implicit_first,
        original_return_annotation: func
            .return_annotation()
            .map(|value| value.syntax().text().to_string()),
        return_annotation: func
            .return_annotation()
            .map(|value| value.syntax().text().to_string()),
        propagate_overrides: true,
    })
}

/// Checks the edited parameter list: valid unique names, the order
/// positional-only, normal, `*args`, keyword-only, `**kwargs`, no required
/// positional parameter after a defaulted one, `self` / `cls` kept first.
pub fn validate(model: &SignatureModel) -> Result<(), String> {
    let mut origins = HashSet::new();
    let mut names = HashSet::new();
    let mut stage = 0;
    let mut seen_default = false;
    for p in &model.params {
        if !is_identifier(&p.name) {
            return Err(format!("'{}' is not a valid parameter name", p.name));
        }
        if !names.insert(p.name.as_str()) {
            return Err(format!("Duplicate parameter '{}'", p.name));
        }
        if let Some(o) = p.origin {
            if !origins.insert(o) {
                return Err("A parameter cannot appear twice in the edited signature".into());
            }
            let was = model
                .original
                .get(o)
                .ok_or("Invalid parameter origin")?
                .kind;
            if matches!(was, ParamKind::VarArgs | ParamKind::KwArgs)
                != matches!(p.kind, ParamKind::VarArgs | ParamKind::KwArgs)
                || (matches!(was, ParamKind::VarArgs | ParamKind::KwArgs) && was != p.kind)
            {
                return Err(format!(
                    "'{}' cannot change between a plain and a star parameter",
                    p.name
                ));
            }
        }
        let s = match p.kind {
            ParamKind::PositionalOnly => 0,
            ParamKind::Normal => 1,
            ParamKind::VarArgs => 2,
            ParamKind::KeywordOnly => 3,
            ParamKind::KwArgs => 4,
        };
        if s < stage || (s == stage && matches!(p.kind, ParamKind::VarArgs | ParamKind::KwArgs)) {
            return Err(format!(
                "'{}' is out of order: positional-only, normal, *args, keyword-only, **kwargs",
                p.name
            ));
        }
        stage = s;
        if matches!(p.kind, ParamKind::VarArgs | ParamKind::KwArgs) && p.default.is_some() {
            return Err(format!("'{}' cannot have a default value", p.name));
        }
        if positional(p.kind) {
            if p.default.is_some() {
                seen_default = true;
            } else if seen_default {
                return Err(format!(
                    "'{}' has no default value but follows a parameter with one",
                    p.name
                ));
            }
        }
    }
    if model.implicit_first
        && model.params.first().is_none_or(|p| {
            p.origin != Some(0)
                || p.name != model.original[0].name
                || p.kind != model.original[0].kind
                || p.default != model.original[0].default
        })
    {
        return Err(format!(
            "'{}' must stay the first parameter",
            model.original[0].name
        ));
    }
    // Validate all editable source fragments as one real Python signature. This
    // rejects hard keywords, broken annotations/defaults and source injection.
    let annotation = model
        .return_annotation
        .as_ref()
        .map(|value| format!(" -> {value}"))
        .unwrap_or_default();
    let source = format!(
        "def __signature_probe({}){annotation}:\n    pass\n",
        model.render()
    );
    let parsed = rustpy_syntax::parse(&source);
    let functions: Vec<_> = parsed
        .syntax_node()
        .children()
        .filter_map(ast::FuncDef::cast)
        .collect();
    if !parsed.errors().is_empty()
        || parsed.syntax_node().children().count() != 1
        || functions.len() != 1
        || functions[0].params().is_none()
    {
        return Err(
            "Parameters, defaults and annotations must form a valid Python signature".into(),
        );
    }
    for parameter in &model.params {
        if let Some(value) = &parameter.call_value {
            let source = format!("__signature_value({value})\n");
            let parsed = rustpy_syntax::parse(&source);
            let root = parsed.syntax_node();
            let calls: Vec<_> = root
                .children()
                .filter_map(ast::ExprStmt::cast)
                .filter_map(|statement| statement.expr())
                .filter_map(|expression| {
                    if let ast::Expr::Call(call) = expression {
                        Some(call)
                    } else {
                        None
                    }
                })
                .collect();
            if !parsed.errors().is_empty()
                || root.children().count() != 1
                || calls.len() != 1
                || calls[0].arg_list().is_none_or(|arguments| {
                    let args: Vec<_> = arguments.args().collect();
                    args.len() != 1 || !matches!(args[0], ast::AnyArg::Positional(_))
                })
            {
                return Err(format!(
                    "The call-site value for '{}' must be one expression",
                    parameter.name
                ));
            }
        }
    }
    Ok(())
}

/// What a Change Signature run touches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSummary {
    /// Calls rewritten.
    pub updated_calls: usize,
    pub updated_overrides: usize,
    /// Files with rewritten calls.
    pub files: usize,
    /// Calls left alone (for a Find window), with the reason.
    pub skipped: Vec<(rustpy_semantic::usages::Usage, String)>,
}

impl ChangeSummary {
    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} call site(s) updated in {} file(s); {} overriding declaration(s)",
            self.updated_calls, self.files, self.updated_overrides
        );
        if !self.skipped.is_empty() {
            s.push_str(&format!(
                "; {} call(s) need a manual look",
                self.skipped.len()
            ));
        }
        s
    }
}

/// Calls of the function the model describes (for the dialog's summary):
/// `(resolved calls, files with resolved calls, calls through untyped receivers)`.
pub fn call_site_counts(
    db: &Analysis,
    file: &Arc<FileInfo>,
    model: &SignatureModel,
    project_files: &[PathBuf],
) -> (usize, usize, usize) {
    let Some(func) = find_function(file, model) else {
        return (0, 0, 0);
    };
    let sites = function_call_sites(db, file, &func, project_files);
    let resolved: Vec<&CallSite> = sites.iter().filter(|s| !s.unresolved).collect();
    let files: HashSet<&std::path::Path> = resolved.iter().map(|s| s.file.path.as_path()).collect();
    (resolved.len(), files.len(), sites.len() - resolved.len())
}

fn find_function(file: &FileInfo, model: &SignatureModel) -> Option<ast::FuncDef> {
    node_with_range(&file.parse.syntax_node(), model.def_range)
        .and_then(ast::FuncDef::cast)
        .filter(|f| f.name_text() == model.func_name)
}

/// Applies the edited signature: the parameter list, renamed parameters in
/// the body and docstring, and every resolved call site in the project.
/// Calls through untyped receivers and calls that unpack `*args` /
/// `**kwargs` it cannot map are left alone and listed in the summary.
pub fn change_signature(
    db: &Analysis,
    file: &Arc<FileInfo>,
    model: &SignatureModel,
    project_files: &[PathBuf],
) -> Result<(MultiFileRefactoring, ChangeSummary), String> {
    change_signature_impl(db, file, model, project_files, false)
}

/// Explicit Safe Delete confirmation may intentionally leave body references dangling.
/// Argument binding, unknown call-site conflicts and receiver locking remain identical.
pub(crate) fn delete_parameter_anyway(
    db: &Analysis,
    file: &Arc<FileInfo>,
    model: &SignatureModel,
    project_files: &[PathBuf],
) -> Result<(MultiFileRefactoring, ChangeSummary), String> {
    change_signature_impl(db, file, model, project_files, true)
}

fn change_signature_impl(
    db: &Analysis,
    file: &Arc<FileInfo>,
    model: &SignatureModel,
    project_files: &[PathBuf],
    allow_removed_uses: bool,
) -> Result<(MultiFileRefactoring, ChangeSummary), String> {
    validate(model)?;
    let func = find_function(file, model).ok_or("The function changed; reopen Change Signature")?;
    let mut family = vec![(file.clone(), model.clone())];
    // Explicit subclass constructors define independent public signatures;
    // update their resolved super calls without rewriting those constructors.
    if model.propagate_overrides
        && func.is_method()
        && !matches!(func.name_text().as_str(), "__init__" | "__new__")
    {
        let offset = func
            .name()
            .ok_or("Missing method name")?
            .text_range()
            .start();
        for location in rustpy_semantic::hierarchy::implementations(db, file, offset, project_files)
        {
            if location.is_stub {
                return Err(format!(
                    "Override in {} is read-only; disable override propagation or edit its source",
                    location.path.display()
                ));
            }
            let other = db
                .file_for_path(&location.path)
                .ok_or_else(|| format!("Cannot load override {}", location.path.display()))?;
            let mut changed = signature_model(&other, location.range.start())?;
            if family
                .iter()
                .any(|(file, model)| file.id == other.id && model.def_range == changed.def_range)
            {
                continue;
            }
            if changed.original.len() != model.original.len()
                || changed.implicit_first != model.implicit_first
                || changed
                    .original
                    .iter()
                    .zip(&model.original)
                    .any(|(left, right)| left.kind != right.kind)
            {
                return Err(format!(
                    "Override '{}' in {} has a different parameter layout; resolve it before propagation",
                    changed.func_name,
                    other.path.display()
                ));
            }
            let original = changed.original.clone();
            changed.params = model
                .params
                .iter()
                .map(|parameter| {
                    let mut mapped = parameter.clone();
                    if let Some(origin) = parameter.origin {
                        let before = &model.original[origin];
                        let local = &original[origin];
                        if parameter.name == before.name {
                            mapped.name = local.name.clone();
                        }
                        if parameter.default == before.default {
                            mapped.default = local.default.clone();
                        }
                        if parameter.annotation == before.annotation {
                            mapped.annotation = local.annotation.clone();
                        }
                    }
                    mapped
                })
                .collect();
            if model.return_annotation != model.original_return_annotation {
                changed.return_annotation = model.return_annotation.clone();
            }
            changed.propagate_overrides = false;
            validate(&changed)?;
            family.push((other, changed));
        }
    }
    let mut merged = std::collections::BTreeMap::<PathBuf, Vec<Edit>>::new();
    let mut summary = ChangeSummary::default();
    let mut call_files = HashSet::new();
    for (index, (source, change)) in family.iter().enumerate() {
        let (multi, part) =
            change_single_signature(db, source, change, project_files, allow_removed_uses)?;
        if index > 0 {
            summary.updated_overrides += 1;
        }
        summary.updated_calls += part.updated_calls;
        summary.skipped.extend(part.skipped);
        for (path, edits) in multi.changes {
            if part.updated_calls > 0 {
                if let Some(function) = find_function(source, change) {
                    for site in function_call_sites(db, source, &function, project_files)
                        .iter()
                        .filter(|site| !site.unresolved && site.file.path == path)
                    {
                        call_files.insert(site.file.path.clone());
                    }
                }
            }
            merged.entry(path).or_default().extend(edits);
        }
    }
    for (path, edits) in &mut merged {
        edits.sort_by_key(|(range, _)| (range.start(), range.end()));
        edits.dedup();
        if edits
            .windows(2)
            .any(|pair| pair[0].0.end() > pair[1].0.start())
        {
            return Err(format!(
                "Signature changes overlap in {}; review recursive calls across overrides separately",
                path.display()
            ));
        }
    }
    summary.files = call_files.len();
    Ok((
        MultiFileRefactoring {
            title: format!("Change signature of '{}'", model.func_name),
            changes: merged.into_iter().collect(),
        },
        summary,
    ))
}

fn change_single_signature(
    db: &Analysis,
    file: &Arc<FileInfo>,
    model: &SignatureModel,
    project_files: &[PathBuf],
    allow_removed_uses: bool,
) -> Result<(MultiFileRefactoring, ChangeSummary), String> {
    validate(model)?;
    let stale = || "The function changed since Change Signature opened; open it again".to_string();
    let func = find_function(file, model).ok_or_else(stale)?;
    let current = model_of(&func)?;
    if current.original != model.original
        || current.original_return_annotation != model.original_return_annotation
    {
        return Err(stale());
    }
    let text: &str = &file.text;
    let mut def_edits: Vec<Edit> = Vec::new();
    // The parameter list.
    let list = func.params().ok_or("The function has no parameter list")?;
    let inner = inner_range(list.syntax()).ok_or("Malformed parameter list")?;
    if model.params != model.original {
        if list
            .syntax()
            .descendants_with_tokens()
            .any(|e| e.kind() == SyntaxKind::COMMENT)
        {
            return Err(
                "The parameter list contains comments, which would be lost; edit it by hand".into(),
            );
        }
        def_edits.push((
            inner,
            format_list(text, inner, &render_params(&model.params)),
        ));
    }
    if model.return_annotation != model.original_return_annotation {
        if let Some(annotation) = func.return_annotation() {
            if let Some(value) = &model.return_annotation {
                def_edits.push((annotation.syntax().text_range(), value.trim().to_owned()));
            } else {
                // RETURN_ANNOTATION begins after the arrow in the parser;
                // remove the whole header suffix, including `->` and its spaces.
                def_edits.push((
                    TextRange::new(
                        list.syntax().text_range().end(),
                        annotation.syntax().text_range().end(),
                    ),
                    String::new(),
                ));
            }
        } else if let Some(value) = &model.return_annotation {
            def_edits.push((
                TextRange::empty(list.syntax().text_range().end()),
                format!(" -> {}", value.trim()),
            ));
        }
    }
    // Renamed and removed parameters in the body and docstring.
    let scope = file
        .scopes
        .scope_for_node(func.syntax().text_range())
        .ok_or("Cannot analyse the function")?;
    let is_param = |k: &BindingKind| {
        matches!(
            k,
            BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam
        )
    };
    for (o, orig) in model.original.iter().enumerate() {
        let ids = file.scopes.bindings_in(scope, &orig.name);
        let Some(pid) = ids
            .iter()
            .copied()
            .find(|id| is_param(&file.scopes.binding(*id).kind))
        else {
            continue;
        };
        let refs = file.scopes.references_to(pid);
        match model.params.iter().find(|p| p.origin == Some(o)) {
            None if !refs.is_empty() && !allow_removed_uses => {
                return Err(format!(
                    "'{}' is used in the body of '{}'; remove those uses first",
                    orig.name, model.func_name
                ));
            }
            Some(p) if p.name != orig.name => {
                let clash = file.scopes.bindings_in(scope, &p.name).iter().any(|id| {
                    let b = file.scopes.binding(*id);
                    // Another parameter that is renamed away frees its name.
                    !is_param(&b.kind)
                        || model.params.iter().any(|q| {
                            q.name == p.name
                                && q.origin.is_some_and(|qo| model.original[qo].name == p.name)
                        })
                });
                if clash {
                    return Err(format!(
                        "'{}' is already used in '{}'",
                        p.name, model.func_name
                    ));
                }
                for r in refs {
                    def_edits.push((r.range, p.name.clone()));
                }
                for id in ids.iter().filter(|id| **id != pid) {
                    let b = file.scopes.binding(*id);
                    if !is_param(&b.kind) {
                        def_edits.push((b.range, p.name.clone()));
                    }
                }
                for r in rustpy_semantic::usages::docstring_param_ranges(
                    file,
                    func.syntax().text_range(),
                    &orig.name,
                ) {
                    def_edits.push((r, p.name.clone()));
                }
            }
            _ => {}
        }
    }
    // Derive the docstring from the exact candidate signature without publishing
    // candidate text into Analysis. Replace only the original literal so later
    // call-site folding continues to operate in the original coordinate space.
    if let Some(original_doc) = func.docstring() {
        let candidate_text = crate::refactor::try_apply_edits(&file.text, &def_edits)?;
        let candidate = std::sync::Arc::new(file.with_candidate_text(candidate_text));
        let at = func
            .name()
            .ok_or("Missing function name")?
            .text_range()
            .start();
        let original_names: Vec<_> = model
            .original
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect();
        if let Some((_, replacement)) = rustpy_semantic::docstrings::synchronize_signature(
            db,
            &candidate,
            at,
            &original_names,
            rustpy_semantic::docstrings::StubOptions::default(),
        ) {
            let range = original_doc.syntax().text_range();
            def_edits.retain(|(edit, _)| !range.contains_range(*edit));
            def_edits.push((range, replacement));
        }
    }
    // Call sites, file by file; edits inside a rewritten argument list
    // (a renamed parameter in a recursive call, a nested call) are folded into it.
    let sites = function_call_sites(db, file, &func, project_files);
    let mut summary = ChangeSummary::default();
    if allow_removed_uses {
        for (origin, original) in model.original.iter().enumerate() {
            if model
                .params
                .iter()
                .any(|parameter| parameter.origin == Some(origin))
            {
                continue;
            }
            for id in file.scopes.bindings_in(scope, &original.name) {
                if !is_param(&file.scopes.binding(*id).kind) {
                    continue;
                }
                for reference in file.scopes.references_to(*id) {
                    summary.skipped.push((
                        rustpy_semantic::usages::usage_at(
                            file,
                            reference.range,
                            reference.is_write,
                            false,
                        ),
                        format!(
                            "Deleted parameter '{}' remains used in '{}'",
                            original.name, model.func_name
                        ),
                    ));
                }
            }
        }
    }
    let mut paths: Vec<PathBuf> = vec![file.path.clone()];
    for s in &sites {
        if !paths.contains(&s.file.path) {
            paths.push(s.file.path.clone());
        }
    }
    let mut changes: Vec<(PathBuf, Vec<Edit>)> = Vec::new();
    for path in paths {
        let mut pending: Vec<Edit> = if path == file.path {
            def_edits.clone()
        } else {
            Vec::new()
        };
        let mut here: Vec<&CallSite> = sites.iter().filter(|s| s.file.path == path).collect();
        here.sort_by_key(|s| s.call.syntax().text_range().len());
        let mut rewrote = false;
        for site in here {
            if site.unresolved {
                summary
                    .skipped
                    .push((site_usage(site), "the receiver's type is unknown".into()));
                continue;
            }
            let Some(args) = site.args() else { continue };
            let Some(arg_inner) = inner_range(args.syntax()) else {
                continue;
            };
            let ftext: &str = &site.file.text;
            match rebuild_call(model, site, &args, ftext, &pending) {
                Ok(Some(items)) => {
                    let new_text = format_list(ftext, arg_inner, &items);
                    pending.retain(|(r, _)| !arg_inner.contains_range(*r));
                    pending.push((arg_inner, new_text));
                    summary.updated_calls += 1;
                    rewrote = true;
                }
                Ok(None) => {}
                Err(reason) => summary.skipped.push((site_usage(site), reason)),
            }
        }
        if rewrote {
            summary.files += 1;
        }
        pending.sort_by_key(|(r, _)| (r.start(), r.end()));
        if pending.windows(2).any(|w| w[0].0.end() > w[1].0.start()) {
            return Err(format!(
                "Overlapping changes in {}; edit it by hand",
                path.display()
            ));
        }
        if !pending.is_empty() {
            changes.push((path, pending));
        }
    }
    Ok((
        MultiFileRefactoring {
            title: format!("Change signature of '{}'", model.func_name),
            changes,
        },
        summary,
    ))
}

fn site_usage(site: &CallSite) -> rustpy_semantic::usages::Usage {
    rustpy_semantic::usages::usage_at(&site.file, site.call.syntax().text_range(), false, false)
}

/// The range between the brackets of a parameter or argument list.
fn inner_range(list: &rustpy_syntax::SyntaxNode) -> Option<TextRange> {
    let open = list
        .first_token()
        .filter(|t| matches!(t.kind(), SyntaxKind::L_PAREN))?;
    let close = list
        .last_token()
        .filter(|t| matches!(t.kind(), SyntaxKind::R_PAREN))?;
    Some(TextRange::new(
        open.text_range().end(),
        close.text_range().start(),
    ))
}

/// `items` joined for the bracket contents at `inner`: one line, or one item
/// per line when the original list started on its own line.
fn format_list(text: &str, inner: TextRange, items: &[String]) -> String {
    let orig = slice(text, inner);
    let vertical = orig
        .trim_start_matches([' ', '\t'])
        .starts_with(['\n', '\r'])
        && !items.is_empty();
    if !vertical {
        return items.join(", ");
    }
    let first = orig
        .find(|c: char| !c.is_whitespace())
        .map(|i| usize::from(inner.start()) + i);
    let indent = first
        .map(|f| indent_at(text, f))
        .unwrap_or_else(|| format!("{}    ", indent_at(text, inner.start().into())));
    let close_indent = orig
        .rfind('\n')
        .map(|i| &orig[i + 1..])
        .filter(|t| t.trim().is_empty())
        .unwrap_or("");
    let comma = if orig.trim_end().ends_with(',') {
        ","
    } else {
        ""
    };
    let body: Vec<String> = items.iter().map(|i| format!("{indent}{i}")).collect();
    format!("\n{}{comma}\n{close_indent}", body.join(",\n"))
}

/// The text of `range` with the `edits` inside it applied.
fn text_with_edits(text: &str, range: TextRange, edits: &[Edit]) -> String {
    let inside: Vec<Edit> = edits
        .iter()
        .filter(|(r, _)| range.contains_range(*r))
        .map(|(r, t)| (*r - range.start(), t.clone()))
        .collect();
    apply_edits(slice(text, range), &inside).trim().to_string()
}

/// An argument value bound to a parameter: its text, whether it was passed
/// by keyword, and its position in the call (`None` for values the
/// refactoring adds), which keeps the evaluation order of what was written.
#[derive(Clone)]
struct Bound {
    text: String,
    keyword: bool,
    at: Option<usize>,
    /// A bare generator expression (`f(x for x in y)`), which needs its own
    /// parentheses next to another argument or after `name=`.
    generator: bool,
}

impl Bound {
    fn added(text: &str) -> Self {
        Self {
            text: text.to_string(),
            keyword: false,
            at: None,
            generator: false,
        }
    }

    /// The value as one argument among several, or after `name=`.
    fn delimited(&self) -> String {
        if self.generator {
            format!("({})", self.text)
        } else {
            self.text.clone()
        }
    }
}

/// The new arguments of one call, or `None` when they do not change.
fn rebuild_call(
    model: &SignatureModel,
    site: &CallSite,
    args: &ast::ArgList,
    text: &str,
    pending: &[Edit],
) -> Result<Option<Vec<String>>, String> {
    let orig = &model.original;
    let imp = site.implicit.min(usize::from(model.implicit_first));
    let value = |r: TextRange| text_with_edits(text, r, pending);
    // Bind the written arguments to the original parameters.
    let mut written: Vec<String> = Vec::new();
    let mut bound: Vec<Option<Bound>> = vec![None; orig.len()];
    // Arguments already written for a parameter created from this call site.
    let mut supplied_new: Vec<Option<Bound>> = vec![None; model.params.len()];
    let added_slots: Vec<_> = model
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| p.origin.is_none() && positional(p.kind))
        .map(|(i, _)| i)
        .collect();
    let mut next_added = 0;
    let mut extra_pos: Vec<Bound> = Vec::new();
    let mut extra_kw: Vec<(usize, String)> = Vec::new();
    let mut unpacks = false;
    let slots: Vec<usize> = (imp..orig.len())
        .filter(|o| positional(orig[*o].kind))
        .collect();
    let mut next_slot = 0;
    for (at, a) in args.args().enumerate() {
        match &a {
            ast::AnyArg::Positional(_) | ast::AnyArg::Generator(_) => {
                let v = value(a.syntax().text_range());
                written.push(v.clone());
                let b = Bound {
                    text: v,
                    keyword: false,
                    at: Some(at),
                    generator: matches!(a, ast::AnyArg::Generator(_)),
                };
                match slots.get(next_slot) {
                    Some(o) => bound[*o] = Some(b),
                    None if orig.iter().any(|p| p.kind == ParamKind::VarArgs) => extra_pos.push(b),
                    None if added_slots.get(next_added).is_some() => {
                        let index = added_slots[next_added];
                        next_added += 1;
                        supplied_new[index] = Some(b);
                    }
                    None => {
                        return Err(
                            "the call passes more positional arguments than the function takes"
                                .into(),
                        );
                    }
                }
                next_slot += 1;
            }
            ast::AnyArg::Keyword(k) => {
                let name = k.name_text();
                let v = k
                    .value()
                    .map(|e| value(e.syntax().text_range()))
                    .unwrap_or_default();
                written.push(format!("{name}={v}"));
                match (imp..orig.len()).find(|o| {
                    orig[*o].name == name
                        && matches!(orig[*o].kind, ParamKind::Normal | ParamKind::KeywordOnly)
                }) {
                    Some(o) if bound[o].is_none() => {
                        bound[o] = Some(Bound {
                            text: v,
                            keyword: true,
                            at: Some(at),
                            generator: false,
                        })
                    }
                    Some(_) => return Err(format!("'{name}' is passed twice")),
                    None if orig.iter().any(|p| p.kind == ParamKind::KwArgs) => {
                        extra_kw.push((at, format!("{name}={v}")))
                    }
                    None => {
                        let index = model
                            .params
                            .iter()
                            .position(|p| {
                                p.origin.is_none()
                                    && p.name == name
                                    && matches!(p.kind, ParamKind::Normal | ParamKind::KeywordOnly)
                            })
                            .ok_or_else(|| {
                                format!("the call passes an unknown keyword '{name}'")
                            })?;
                        if supplied_new[index].is_some() {
                            return Err(format!("'{name}' is passed twice"));
                        }
                        supplied_new[index] = Some(Bound {
                            text: v,
                            keyword: true,
                            at: Some(at),
                            generator: false,
                        });
                    }
                }
            }
            ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_) => {
                unpacks = true;
                written.push(value(a.syntax().text_range()));
            }
        }
    }
    if unpacks {
        // `*args` / `**kwargs` fill parameters this call does not name: only
        // changes that keep every existing parameter in place are safe.
        let kept = model.params.len() >= orig.len()
            && model
                .params
                .iter()
                .take(orig.len())
                .enumerate()
                .all(|(i, p)| {
                    p.origin == Some(i) && p.name == orig[i].name && p.kind == orig[i].kind
                });
        // A value for existing calls cannot be placed next to unpacked arguments.
        let added_optional = model.params.iter().skip(orig.len()).all(|p| {
            (p.default.is_some() && p.call_value.is_none())
                || matches!(p.kind, ParamKind::VarArgs | ParamKind::KwArgs)
        });
        return if kept && added_optional {
            Ok(None)
        } else {
            Err("the call unpacks *args / **kwargs".into())
        };
    }
    // The value of every new parameter.
    let new = &model.params;
    let mut values: Vec<Option<Bound>> = Vec::with_capacity(new.len());
    for (index, p) in new.iter().enumerate() {
        let star = matches!(p.kind, ParamKind::VarArgs | ParamKind::KwArgs);
        let v = match p.origin {
            Some(o) => match &bound[o] {
                Some(b) => Some(b.clone()),
                // The default this call relied on goes away: pass its value.
                None if p.default.is_none() && !star => {
                    orig[o].default.as_deref().map(Bound::added)
                }
                None => None,
            },
            None if supplied_new[index].is_some() => supplied_new[index].clone(),
            None if star => None,
            // A new parameter: existing calls pass the value chosen for them,
            // even when the parameter has a default (which serves new calls).
            None => match (&p.call_value, &p.default) {
                (Some(v), _) => Some(Bound::added(v)),
                (None, Some(_)) => None,
                (None, None) => {
                    return Err(format!(
                        "no value for the new parameter '{}' (set a value for call sites or a default)",
                        p.name
                    ));
                }
            },
        };
        values.push(v);
    }
    // Positional arguments while the parameters can still be filled in order.
    let mut positional_items: Vec<Bound> = Vec::new();
    let mut keywords: Vec<(Option<usize>, usize, String)> = Vec::new();
    let mut by_position = true;
    let last_positional_only = new
        .iter()
        .rposition(|p| p.kind == ParamKind::PositionalOnly);
    for (i, p) in new.iter().enumerate().skip(imp) {
        if !positional(p.kind) {
            continue;
        }
        match &values[i] {
            Some(b) if p.kind == ParamKind::PositionalOnly => {
                if !by_position {
                    return Err(format!(
                        "'{}' would become positional-only after a keyword argument",
                        p.name
                    ));
                }
                positional_items.push(b.clone());
            }
            Some(b) if by_position && !b.keyword => positional_items.push(b.clone()),
            Some(b) => {
                by_position = false;
                keywords.push((b.at, i, format!("{}={}", p.name, b.delimited())));
            }
            None => {
                // Positional-only arguments cannot skip a parameter: pass its default.
                let needed_later = p.kind == ParamKind::PositionalOnly
                    && (i + 1..new.len()).any(|j| {
                        last_positional_only.is_some_and(|l| j <= l) && values[j].is_some()
                    });
                match &p.default {
                    Some(d) if needed_later => positional_items.push(Bound::added(d.trim())),
                    _ => by_position = false,
                }
            }
        }
    }
    if !extra_pos.is_empty() {
        if !by_position
            || !new
                .iter()
                .any(|p| p.kind == ParamKind::VarArgs && p.origin.is_some())
        {
            return Err(
                "the call passes extra positional arguments that can no longer be placed".into(),
            );
        }
        positional_items.extend(extra_pos);
    }
    for (i, p) in new.iter().enumerate().skip(imp) {
        if p.kind == ParamKind::KeywordOnly {
            if let Some(b) = &values[i] {
                keywords.push((b.at, i, format!("{}={}", p.name, b.delimited())));
            }
        }
    }
    if !extra_kw.is_empty() {
        if !new
            .iter()
            .any(|p| p.kind == ParamKind::KwArgs && p.origin.is_some())
        {
            return Err("the call passes keywords that only **kwargs accepted".into());
        }
        keywords.extend(
            extra_kw
                .into_iter()
                .map(|(at, t)| (Some(at), usize::MAX, t)),
        );
    }
    // Keywords keep the order they were written in; added ones follow in parameter order.
    keywords.sort_by_key(|(at, i, _)| (at.unwrap_or(usize::MAX), *i));
    // A bare generator stays bare only as the call's sole argument.
    let alone = positional_items.len() + keywords.len() == 1;
    let mut items: Vec<String> = positional_items
        .iter()
        .map(|b| if alone { b.text.clone() } else { b.delimited() })
        .collect();
    items.extend(keywords.into_iter().map(|(_, _, t)| t));
    if items == written {
        return Ok(None);
    }
    if args
        .syntax()
        .descendants_with_tokens()
        .any(|e| e.kind() == SyntaxKind::COMMENT)
    {
        return Err("comments between the arguments would be lost".into());
    }
    Ok(Some(items))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_semantic::edit_helpers::apply_edits;
    use std::path::Path;

    struct Project {
        db: Analysis,
        files: Vec<(PathBuf, String)>,
    }

    impl Project {
        fn new(files: &[(&str, &str)]) -> Self {
            let db = Analysis::for_project(Path::new("/proj"));
            let files: Vec<(PathBuf, String)> = files
                .iter()
                .map(|(n, t)| (PathBuf::from(format!("/proj/{n}")), t.to_string()))
                .collect();
            for (p, t) in &files {
                db.set_file_text(p, t);
            }
            Self { db, files }
        }

        fn file(&self, name: &str) -> Arc<FileInfo> {
            self.db
                .file_for_path(Path::new(&format!("/proj/{name}")))
                .unwrap()
        }

        fn model(&self, name: &str, needle: &str) -> SignatureModel {
            let f = self.file(name);
            signature_model(
                &f,
                TextSize::new(f.text.find(needle).expect("needle") as u32),
            )
            .unwrap()
        }

        /// Applies the change; returns the new texts by file name and the summary.
        fn apply(
            &self,
            name: &str,
            model: &SignatureModel,
        ) -> Result<(Vec<(String, String)>, ChangeSummary), String> {
            let paths: Vec<PathBuf> = self.files.iter().map(|(p, _)| p.clone()).collect();
            let (m, summary) = change_signature(&self.db, &self.file(name), model, &paths)?;
            let out = self
                .files
                .iter()
                .map(|(p, t)| {
                    let edits = m
                        .changes
                        .iter()
                        .find(|(q, _)| q == p)
                        .map(|(_, e)| e.clone())
                        .unwrap_or_default();
                    (
                        p.file_name().unwrap().to_string_lossy().into_owned(),
                        apply_edits(t, &edits),
                    )
                })
                .collect();
            Ok((out, summary))
        }
    }

    fn get<'a>(out: &'a [(String, String)], name: &str) -> &'a str {
        &out.iter().find(|(n, _)| n == name).unwrap().1
    }

    const LIB: &str = "def connect(host, port, timeout=5):\n    return host, port, timeout\n\n\nconnect('a', 1)\n";
    const USER: &str = "from lib import connect\n\n\nconnect('b', 2, 3)\nconnect('c', port=4)\nconnect(host='d', port=5, timeout=6)\n";

    #[test]
    fn signature_change_synchronizes_structured_doc_fields_and_callers_together() {
        let source = "def calculate(retired, value: int) -> int:\n    \"\"\"Keep this summary.\n\n    @param retired: remove this obsolete explanation\n    @type retired: bool\n    @param value: keep this explanation\n    @type value: int\n    @return: keep the return description\n    @rtype: int\n    \"\"\"\n    return value\n\ncalculate(False, 2)\n";
        let project = Project::new(&[("module.py", source)]);
        let mut model = project.model("module.py", "def calculate");
        model.params.remove(0);
        model.params[0].name = "amount".into();
        model.params[0].annotation = Some("float".into());
        model.params.push(SigParam::added("scale", Some("1"), None));
        model.return_annotation = Some("float".into());
        let (output, _) = project.apply("module.py", &model).unwrap();
        let output = get(&output, "module.py");
        assert!(
            output.contains("@param amount: keep this explanation"),
            "{output}"
        );
        assert!(output.contains("@type amount: float"), "{output}");
        assert!(output.contains("@param scale:"), "{output}");
        assert!(output.contains("@return: keep the return description"));
        assert!(output.contains("@rtype: float"));
        assert!(!output.contains("obsolete explanation"));
        assert!(!output.contains("@type retired"));
        assert!(output.contains("return amount"));
        assert!(!output.contains("calculate(False"));
        assert_eq!(project.file("module.py").text.as_ref(), source);
    }

    #[test]
    fn add_parameter_passes_the_call_site_value_everywhere() {
        let p = Project::new(&[("lib.py", LIB), ("user.py", USER)]);
        let mut m = p.model("lib.py", "def connect");
        m.params
            .insert(2, SigParam::added("retries", None, Some("0")));
        let (out, summary) = p.apply("lib.py", &m).unwrap();
        assert_eq!(
            get(&out, "lib.py"),
            "def connect(host, port, retries, timeout=5):\n    return host, port, timeout\n\n\nconnect('a', 1, 0)\n"
        );
        assert_eq!(
            get(&out, "user.py"),
            "from lib import connect\n\n\nconnect('b', 2, 0, 3)\nconnect('c', port=4, retries=0)\nconnect(host='d', port=5, timeout=6, retries=0)\n"
        );
        assert_eq!((summary.updated_calls, summary.files), (4, 2));
        // Without a value for call sites (and no default) existing calls are left for a manual look.
        let mut m = p.model("lib.py", "def connect");
        m.params.insert(2, SigParam::added("flag", None, None));
        assert_eq!(p.apply("lib.py", &m).unwrap().1.skipped.len(), 4);
        // A required parameter after one with a default is refused.
        let mut m = p.model("lib.py", "def connect");
        m.params.push(SigParam::added("flag", None, Some("1")));
        assert!(p
            .apply("lib.py", &m)
            .unwrap_err()
            .contains("follows a parameter with one"));
    }

    #[test]
    fn calls_through_a_module_import_are_updated() {
        let p = Project::new(&[
            ("lib.py", LIB),
            ("app.py", "import lib\n\n\nlib.connect('x', port=1)\n"),
        ]);
        let mut m = p.model("lib.py", "def connect");
        m.params[1].name = "number".into();
        let (out, summary) = p.apply("lib.py", &m).unwrap();
        assert_eq!(
            get(&out, "app.py"),
            "import lib\n\n\nlib.connect('x', number=1)\n"
        );
        assert!(summary.skipped.is_empty(), "{summary:?}");
    }

    #[test]
    fn remove_reorder_and_rename() {
        let p = Project::new(&[
            (
                "lib.py",
                "def connect(host, port, timeout=5):\n    return host, port\n\n\nconnect('a', 1)\n",
            ),
            ("user.py", USER),
        ]);
        // Remove `timeout` (unused in the body).
        let mut m = p.model("lib.py", "def connect");
        m.params.remove(2);
        let (out, _) = p.apply("lib.py", &m).unwrap();
        assert_eq!(
            get(&out, "user.py"),
            "from lib import connect\n\n\nconnect('b', 2)\nconnect('c', port=4)\nconnect(host='d', port=5)\n"
        );
        // Reorder host and port.
        let mut m = p.model("lib.py", "def connect");
        m.params.swap(0, 1);
        let (out, _) = p.apply("lib.py", &m).unwrap();
        assert_eq!(
            get(&out, "lib.py"),
            "def connect(port, host, timeout=5):\n    return host, port\n\n\nconnect(1, 'a')\n"
        );
        // Arguments that turn into keywords keep the order they are evaluated in.
        assert_eq!(
            get(&out, "user.py"),
            "from lib import connect\n\n\nconnect(2, 'b', 3)\nconnect(host='c', port=4)\nconnect(host='d', port=5, timeout=6)\n"
        );
        // Rename `port` to `number`: keywords and the body follow.
        let mut m = p.model("lib.py", "def connect");
        m.params[1].name = "number".into();
        let (out, _) = p.apply("lib.py", &m).unwrap();
        assert_eq!(
            get(&out, "lib.py"),
            "def connect(host, number, timeout=5):\n    return host, number\n\n\nconnect('a', 1)\n"
        );
        assert_eq!(
            get(&out, "user.py"),
            "from lib import connect\n\n\nconnect('b', 2, 3)\nconnect('c', number=4)\nconnect(host='d', number=5, timeout=6)\n"
        );
        // A removed parameter that the body uses is refused.
        let mut m = p.model("lib.py", "def connect");
        m.params.remove(0);
        assert!(p
            .apply("lib.py", &m)
            .unwrap_err()
            .contains("used in the body"));
    }

    #[test]
    fn keyword_only_positional_only_and_defaults() {
        let p = Project::new(&[("lib.py", LIB), ("user.py", USER)]);
        // Keyword-only `port` (and `timeout` after it).
        let mut m = p.model("lib.py", "def connect");
        m.params[1].kind = ParamKind::KeywordOnly;
        m.params[2].kind = ParamKind::KeywordOnly;
        let (out, _) = p.apply("lib.py", &m).unwrap();
        assert_eq!(
            get(&out, "lib.py"),
            "def connect(host, *, port, timeout=5):\n    return host, port, timeout\n\n\nconnect('a', port=1)\n"
        );
        assert_eq!(
            get(&out, "user.py"),
            "from lib import connect\n\n\nconnect('b', port=2, timeout=3)\nconnect('c', port=4)\nconnect(host='d', port=5, timeout=6)\n"
        );
        // Positional-only `host`.
        let mut m = p.model("lib.py", "def connect");
        m.params[0].kind = ParamKind::PositionalOnly;
        let (out, _) = p.apply("lib.py", &m).unwrap();
        assert_eq!(
            get(&out, "lib.py"),
            "def connect(host, /, port, timeout=5):\n    return host, port, timeout\n\n\nconnect('a', 1)\n"
        );
        assert_eq!(
            get(&out, "user.py"),
            "from lib import connect\n\n\nconnect('b', 2, 3)\nconnect('c', port=4)\nconnect('d', port=5, timeout=6)\n"
        );
        // Removing a default passes the old value where calls relied on it.
        let mut m = p.model("lib.py", "def connect");
        m.params[2].default = None;
        let (out, _) = p.apply("lib.py", &m).unwrap();
        assert_eq!(
            get(&out, "lib.py"),
            "def connect(host, port, timeout):\n    return host, port, timeout\n\n\nconnect('a', 1, 5)\n"
        );
        assert_eq!(
            get(&out, "user.py"),
            "from lib import connect\n\n\nconnect('b', 2, 3)\nconnect('c', port=4, timeout=5)\nconnect(host='d', port=5, timeout=6)\n"
        );
        // Invalid orders are refused.
        let mut m = p.model("lib.py", "def connect");
        m.params[0].default = Some("'x'".into());
        m.params[1].default = None;
        m.params.swap(0, 2);
        assert!(p.apply("lib.py", &m).is_err());
    }

    #[test]
    fn methods_constructors_and_untyped_receivers() {
        let src = "class Box:\n    def __init__(self, size, color='red'):\n        self.size = size\n\n    def grow(self, by, times=1):\n        self.size += by * times\n\n\nb = Box(1, color='blue')\nb.grow(2)\nBox.grow(b, 3, 4)\n\n\ndef use(x):\n    x.grow(5)\n";
        let p = Project::new(&[("m.py", src)]);
        let mut m = p.model("m.py", "def grow");
        assert!(m.implicit_first);
        m.params.swap(1, 2);
        m.params[1].default = None;
        m.params[2].default = Some("1".into());
        let (out, summary) = p.apply("m.py", &m).unwrap();
        assert_eq!(
            get(&out, "m.py"),
            "class Box:\n    def __init__(self, size, color='red'):\n        self.size = size\n\n    def grow(self, times, by=1):\n        self.size += by * times\n\n\nb = Box(1, color='blue')\nb.grow(1, 2)\nBox.grow(b, 4, 3)\n\n\ndef use(x):\n    x.grow(5)\n"
        );
        assert_eq!(summary.skipped.len(), 1, "{summary:?}");
        assert_eq!(summary.skipped[0].0.line + 1, 15);
        // `self` stays first.
        let mut m = p.model("m.py", "def grow");
        m.params.swap(0, 1);
        assert!(p.apply("m.py", &m).is_err());
        // Constructor calls follow `__init__`.
        let mut m = p.model("m.py", "def __init__");
        m.params[2].name = "colour".into();
        m.params.push(SigParam::added("label", Some("''"), None));
        let (out, _) = p.apply("m.py", &m).unwrap();
        assert!(get(&out, "m.py").contains("def __init__(self, size, colour='red', label=''):"));
        assert!(get(&out, "m.py").contains("b = Box(1, colour='blue')\n"));
    }

    #[test]
    fn recursive_calls_and_multiline_lists() {
        let src = "def walk(node, depth):\n    if node:\n        walk(\n            node.next,\n            depth + 1,\n        )\n    return depth\n";
        let p = Project::new(&[("w.py", src)]);
        let mut m = p.model("w.py", "def walk");
        m.params.swap(0, 1);
        m.params[0].name = "level".into();
        let (out, _) = p.apply("w.py", &m).unwrap();
        assert_eq!(
            get(&out, "w.py"),
            "def walk(level, node):\n    if node:\n        walk(\n            level + 1,\n            node.next,\n        )\n    return level\n"
        );
        // Unpacking calls are left alone unless the change keeps every parameter in place.
        let src = "def f(a, b):\n    return a\n\n\nargs = (1, 2)\nf(*args)\n";
        let p = Project::new(&[("u.py", src)]);
        let mut m = p.model("u.py", "def f");
        m.params.push(SigParam::added("c", Some("None"), None));
        let (out, summary) = p.apply("u.py", &m).unwrap();
        assert_eq!(
            get(&out, "u.py"),
            "def f(a, b, c=None):\n    return a\n\n\nargs = (1, 2)\nf(*args)\n"
        );
        assert!(summary.skipped.is_empty());
        let mut m = p.model("u.py", "def f");
        m.params.remove(1);
        let (_, summary) = p.apply("u.py", &m).unwrap();
        assert_eq!(summary.skipped.len(), 1);
    }
    #[test]
    fn add_parameter_from_existing_usage_preserves_supplied_keyword_and_old_defaults() {
        let p = Project::new(&[(
            "main.py",
            "def f(value):\n    return value\nf(1, flag=True)\nf(2)\n",
        )]);
        let mut model = p.model("main.py", "def f");
        let mut added = SigParam::added("flag", Some("None"), None);
        added.kind = ParamKind::KeywordOnly;
        model.params.push(added);
        let (files, _) = p.apply("main.py", &model).unwrap();
        let text = &files[0].1;
        assert!(text.contains("def f(value, *, flag=None):"));
        assert!(text.contains("f(1, flag=True)"));
        assert!(text.contains("f(2)"));
    }
    #[test]
    fn added_positional_parameter_adopts_existing_argument_without_duplicating_it() {
        let p = Project::new(&[(
            "main.py",
            "def f(value):\n    return value\nf(1, 2)\nf(3)\n",
        )]);
        let mut model = p.model("main.py", "def f");
        model
            .params
            .push(SigParam::added("extra", Some("None"), None));
        let (files, _) = p.apply("main.py", &model).unwrap();
        let text = &files[0].1;
        assert!(text.contains("def f(value, extra=None):"));
        assert!(text.contains("f(1, 2)"));
        assert!(text.contains("f(3)"));
    }
    #[test]
    fn return_annotation_insert_replace_remove_and_stale_conflict() {
        let project = Project::new(&[(
            "main.py",
            "def answer(value: int):\n    return value\nanswer(1)\n",
        )]);
        let mut model = project.model("main.py", "def answer");
        model.return_annotation = Some("list[int]".into());
        let (output, summary) = project.apply("main.py", &model).unwrap();
        assert!(get(&output, "main.py").contains("def answer(value: int) -> list[int]:"));
        assert_eq!(summary.updated_calls, 0);
        let project = Project::new(&[("main.py", "def answer(value) -> int:\n    return value\n")]);
        let mut model = project.model("main.py", "def answer");
        model.return_annotation = Some("str".into());
        assert!(get(&project.apply("main.py", &model).unwrap().0, "main.py").contains("-> str:"));
        model.return_annotation = None;
        let output = project.apply("main.py", &model).unwrap().0;
        assert!(rustpy_syntax::parse(get(&output, "main.py"))
            .errors()
            .is_empty());
        assert!(!get(&output, "main.py").contains("->"));
        project.db.set_file_text(
            Path::new("/proj/main.py"),
            "def answer(value) -> bytes:\n    return value\n",
        );
        assert!(project
            .apply("main.py", &model)
            .unwrap_err()
            .contains("changed"));
    }
    #[test]
    fn override_family_propagates_order_renames_defaults_and_calls_without_touching_unrelated_method(
    ) {
        let source = "class Base:\n    def send(self, value, flag=False):\n        return value\nclass Child(Base):\n    def send(self, payload, flag=True):\n        return payload\nclass Other:\n    def send(self, value, flag=False):\n        return value\nBase().send(1)\nChild().send(2)\nOther().send(3)\n";
        let project = Project::new(&[("main.py", source)]);
        let mut model = project.model("main.py", "def send");
        model.params[1].name = "data".into();
        model.params[1].kind = ParamKind::KeywordOnly;
        model.params[2].kind = ParamKind::KeywordOnly;
        model
            .params
            .push(SigParam::added("timeout", Some("None"), Some("30")));
        model.params.last_mut().unwrap().kind = ParamKind::KeywordOnly;
        model.return_annotation = Some("int".into());
        let (output, summary) = project.apply("main.py", &model).unwrap();
        let text = get(&output, "main.py");
        assert_eq!(summary.updated_overrides, 1);
        assert!(text.contains("def send(self, *, data, flag=False, timeout=None) -> int:"));
        assert!(text.contains("def send(self, *, data, flag=True, timeout=None) -> int:"));
        assert!(text.contains("return data"));
        assert!(text.contains("Base().send(data=1, timeout=30)"));
        assert!(text.contains("Child().send(data=2, timeout=30)"));
        assert!(text.contains("Other().send(3)"));
        assert!(text.contains("def send(self, value, flag=False):"));
        model.propagate_overrides = false;
        let (output, summary) = project.apply("main.py", &model).unwrap();
        assert_eq!(summary.updated_overrides, 0);
        assert!(get(&output, "main.py").contains("def send(self, payload, flag=True):"));
    }
    #[test]
    fn incompatible_override_and_invalid_source_are_atomic_conflicts_and_self_is_locked() {
        let project = Project::new(&[(
            "main.py",
            "class Base:\n    def run(self, value):\n        return value\nclass Child(Base):\n    def run(self, value, extra):\n        return value\n",
        )]);
        let mut model = project.model("main.py", "def run");
        model.params[1].name = "payload".into();
        assert!(project
            .apply("main.py", &model)
            .unwrap_err()
            .contains("different parameter layout"));
        model.propagate_overrides = false;
        model.params[0].name = "receiver".into();
        assert!(validate(&model).unwrap_err().contains("first parameter"));
        model.params[0].name = "self".into();
        model.params[1].name = "for".into();
        assert!(validate(&model).is_err());
        model.params[1].name = "payload".into();
        model.params[1].annotation = Some("list[".into());
        assert!(validate(&model).is_err());
        model.params[1].annotation = None;
        model
            .params
            .push(SigParam::added("extra", None, Some("1, 2")));
        assert!(validate(&model).unwrap_err().contains("one expression"));
    }
    #[test]
    fn added_star_parameters_render_and_explicit_arguments_keep_their_binding() {
        let project = Project::new(&[("main.py", "def f(value):\n    return value\nf(1)\n")]);
        let mut model = project.model("main.py", "def f");
        let mut args = SigParam::added("args", None, None);
        args.kind = ParamKind::VarArgs;
        let mut kwargs = SigParam::added("kwargs", None, None);
        kwargs.kind = ParamKind::KwArgs;
        model.params.extend([
            args,
            SigParam {
                kind: ParamKind::KeywordOnly,
                ..SigParam::added("flag", Some("False"), Some("True"))
            },
            kwargs,
        ]);
        let output = project.apply("main.py", &model).unwrap().0;
        assert!(get(&output, "main.py").contains("def f(value, *args, flag=False, **kwargs):"));
        assert!(get(&output, "main.py").contains("f(1, flag=True)"));
    }
}
