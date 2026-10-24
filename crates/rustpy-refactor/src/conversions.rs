//! Explicit conversion plans, shared preview and filesystem Undo owned by UI.
use crate::{
    change_signature, movement,
    refactor2::{self, FileOp},
};
use rustpy_core::{TextRange, TextSize};
use rustpy_search::usages::{find_usages, function_call_sites};
use rustpy_semantic::{
    db::{Analysis, FileInfo},
    edit_helpers::{self, ancestor_at, indent_at, node_with_range, Edit},
    infer::Inferer,
    scope::BindingKind,
    types::{LiteralValue, Type},
    usages::{self, Symbol},
};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind,
};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionKind {
    Package,
    Module,
    LocalToField,
    MethodStatic,
    MethodFunction,
    FunctionMethod,
    InvertBoolean,
    Dataclass,
    NamedTuple,
    ExplicitArgs,
    EncapsulateField,
    ConstructorFactory,
    MigrateApi,
    Comprehension,
    IntroduceAlias,
    RenameModule,
}
impl ConversionKind {
    pub const ALL: &'static [Self] = &[
        Self::Package,
        Self::Module,
        Self::LocalToField,
        Self::MethodStatic,
        Self::MethodFunction,
        Self::FunctionMethod,
        Self::InvertBoolean,
        Self::Dataclass,
        Self::NamedTuple,
        Self::ExplicitArgs,
        Self::EncapsulateField,
        Self::ConstructorFactory,
        Self::MigrateApi,
        Self::Comprehension,
        Self::IntroduceAlias,
        Self::RenameModule,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Package => "Convert to Package",
            Self::Module => "Convert to Module",
            Self::LocalToField => "Convert Local Variable to Field",
            Self::MethodStatic => "Make Method Static",
            Self::MethodFunction => "Make Method Top-level Function",
            Self::FunctionMethod => "Make Function Method",
            Self::InvertBoolean => "Invert Boolean and Update Usages",
            Self::Dataclass => "Convert Class to dataclass",
            Self::NamedTuple => "Convert Class to NamedTuple",
            Self::ExplicitArgs => "Convert *args to Explicit Parameters",
            Self::EncapsulateField => "Encapsulate Field",
            Self::ConstructorFactory => "Replace Constructor with Factory",
            Self::MigrateApi => "Migrate API",
            Self::Comprehension => "Convert Comprehension",
            Self::IntroduceAlias => "Introduce Import Alias",
            Self::RenameModule => "Rename Module and Update Imports / __init__",
        }
    }
}
#[derive(Clone, Debug)]
pub struct ConversionOptions {
    pub kind: ConversionKind,
    pub name: String,
    pub target_class: String,
    pub migration: String,
    pub collection: String,
}
impl Default for ConversionOptions {
    fn default() -> Self {
        Self{kind:ConversionKind::LocalToField,name:String::new(),target_class:String::new(),migration:"unittest.TestCase.assertEquals=unittest.TestCase.assertEqual\nunittest.TestCase.assertNotEquals=unittest.TestCase.assertNotEqual\nunittest.TestCase.assertAlmostEquals=unittest.TestCase.assertAlmostEqual\nunittest.TestCase.assertRegexpMatches=unittest.TestCase.assertRegex".into(),collection:"tuple".into()}
    }
}
fn identifier(name: &str) -> Result<(), String> {
    crate::rename::check_new_name(name)
}
fn changes(items: BTreeMap<PathBuf, Vec<Edit>>) -> Vec<FileOp> {
    items.into_iter().map(|(p, e)| FileOp::Edit(p, e)).collect()
}
fn add(
    items: &mut BTreeMap<PathBuf, Vec<Edit>>,
    file: &FileInfo,
    range: TextRange,
    text: impl Into<String>,
) {
    items
        .entry(file.path.clone())
        .or_default()
        .push((range, text.into()));
}
fn one(file: &FileInfo, edits: Vec<Edit>) -> Vec<FileOp> {
    vec![FileOp::Edit(file.path.clone(), edits)]
}
fn class_at(file: &FileInfo, offset: TextSize) -> Result<ast::ClassDef, String> {
    ancestor_at(&file.parse.syntax_node(), offset).ok_or("Place the caret inside a class".into())
}
fn function_at(file: &FileInfo, offset: TextSize) -> Result<ast::FuncDef, String> {
    ancestor_at(&file.parse.syntax_node(), offset)
        .ok_or("Place the caret inside a function or method".into())
}
fn receiver(function: &ast::FuncDef) -> Result<String, String> {
    if !function.is_method() || function.has_decorator("staticmethod") {
        return Err("An instance method is required".into());
    }
    function
        .params()
        .and_then(|p| p.plain_params().next())
        .map(|p| p.name_text())
        .ok_or("Method has no receiver".into())
}
fn external_writer(file: &FileInfo, name: &str) -> bool {
    file.scopes.bindings.iter().any(|binding| {
        binding.name == name && matches!(binding.kind, BindingKind::Global | BindingKind::Nonlocal)
    })
}
fn fresh(file: &FileInfo, prefix: &str) -> String {
    let mut n = 0;
    loop {
        let name = format!("{prefix}{n}");
        if !file.text.contains(&name) {
            return name;
        }
        n += 1
    }
}
fn builtin_import(file: &FileInfo) -> (String, Edit) {
    let alias = fresh(file, "_conversion_builtins_");
    let edit = (
        TextRange::empty(rustpy_semantic::import_placement::import_insert_offset(
            file,
        )),
        format!("import builtins as {alias}\n"),
    );
    (alias, edit)
}
fn class_plain(db: &Analysis, file: &FileInfo, class: &ast::ClassDef) -> Result<(), String> {
    if class.decorators().next().is_some()
        || class.arguments().is_some_and(|a| {
            a.args()
                .any(|arg| !matches!(arg, ast::AnyArg::Positional(_)))
        })
    {
        return Err(
            "Class decorators/metaclasses require resolving their transformation first".into(),
        );
    }
    if class.methods().any(|m| {
        matches!(
            m.name_text().as_str(),
            "__getattribute__" | "__getattr__" | "__setattr__" | "__delattr__"
        )
    }) {
        return Err("Custom attribute dispatch prevents this conversion".into());
    }
    let reference = rustpy_semantic::types::ClassRef {
        file: file.id,
        range: class.text_range(),
        name: class.name_text().into(),
    };
    let inf = Inferer::new(db);
    let owners = inf.mro(&reference);
    if owners.len() > 32 {
        return Err("Class ancestry exceeds the conversion budget".into());
    }
    for owner in owners.iter().skip(1) {
        let source = db
            .file(owner.file)
            .ok_or("Missing inherited class source")?;
        if source.path.starts_with("<typeshed>")
            && source.module_name.as_deref() == Some("builtins")
            && owner.name == "object"
        {
            continue;
        }
        let base = node_with_range(&source.parse.syntax_node(), owner.range)
            .and_then(ast::ClassDef::cast)
            .ok_or("Unknown inherited class")?;
        for expression in base.bases() {
            if !matches!(inf.type_of_expr(&source, &expression), Type::Class(..)) {
                return Err("An inherited base is dynamic or unresolved".into());
            }
        }
        if base.decorators().next().is_some()
            || base
                .arguments()
                .is_some_and(|a| a.args().any(|v| !matches!(v, ast::AnyArg::Positional(_))))
            || base.methods().any(|m| {
                matches!(
                    m.name_text().as_str(),
                    "__getattribute__" | "__getattr__" | "__setattr__" | "__delattr__"
                )
            })
        {
            return Err(
                "Inherited custom class/attribute dispatch prevents this conversion".into(),
            );
        }
    }
    // A missing nominal base must not silently disappear from the MRO proof.
    for base in class.bases() {
        if !matches!(
            inf.type_of_expr(&db.file(file.id).ok_or("Missing source")?, &base),
            Type::Class(..)
        ) {
            return Err("Resolve dynamic or unknown class bases before conversion".into());
        }
    }
    Ok(())
}
pub fn plan(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
    root: &Path,
    options: &ConversionOptions,
) -> Result<Vec<FileOp>, String> {
    if file.is_stub || usages::is_library_file(db, file) {
        return Err("Conversions require writable project source".into());
    }
    if !file.parse.errors().is_empty() {
        return Err("Resolve Python syntax errors before converting this source".into());
    }
    let result = match options.kind {
        ConversionKind::Package | ConversionKind::Module => {
            package_conversion(db, file, options.kind == ConversionKind::Package)?
        }
        ConversionKind::LocalToField => local_to_field(db, file, offset, &options.name)?,
        ConversionKind::MethodStatic => method_static(db, file, offset, files)?,
        ConversionKind::MethodFunction => {
            let function = function_at(file, offset)?;
            let name = if options.name.is_empty() {
                function.name_text()
            } else {
                options.name.clone()
            };
            movement::method_to_function(db, file, offset, &name, files, root, true)?
        }
        ConversionKind::FunctionMethod => {
            function_method(db, file, offset, files, &options.target_class)?
        }
        ConversionKind::InvertBoolean => invert_boolean(db, file, offset, files)?,
        ConversionKind::Dataclass => one(
            file,
            refactor2::convert_to_dataclass(db, file, offset)?.edits,
        ),
        ConversionKind::NamedTuple => named_tuple(db, file, offset)?,
        ConversionKind::ExplicitArgs => explicit_args(db, file, offset, files, &options.name)?,
        ConversionKind::EncapsulateField => {
            encapsulate_field(db, file, offset, files, &options.name)?
        }
        ConversionKind::ConstructorFactory => {
            constructor_factory(db, file, offset, files, &options.name)?
        }
        ConversionKind::MigrateApi => migrate_api(db, file, files, &options.migration)?,
        ConversionKind::Comprehension => comprehension(file, offset, &options.collection)?,
        ConversionKind::IntroduceAlias | ConversionKind::RenameModule => {
            return Err("This conversion uses the existing import/module UI authority".into())
        }
    };
    let mut count = 0usize;
    let mut bytes = 0usize;
    for operation in &result {
        if let FileOp::Edit(_, edits) = operation {
            count += edits.len();
            bytes += edits.iter().map(|(_, text)| text.len()).sum::<usize>();
        }
    }
    if count > 50_000 || bytes > 8 * 1024 * 1024 {
        return Err("Conversion exceeds the bounded preview edit budget".into());
    }
    for operation in &result {
        if let FileOp::Edit(path, edits) = operation {
            let source = db
                .file_for_path(path)
                .ok_or("Missing edited source snapshot")?;
            let updated = edit_helpers::try_apply_edits(&source.text, edits)?;
            if source.parse.errors().is_empty()
                && !rustpy_syntax::parse(&updated).errors().is_empty()
            {
                return Err(
                    "Conversion would produce invalid Python; resolve overlapping syntax first"
                        .into(),
                );
            }
        }
    }
    Ok(result)
}

fn relative_edits(file: &FileInfo, to_package: bool) -> Result<Vec<Edit>, String> {
    let mut edits = Vec::new();
    for statement in file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ImportFromStmt::cast)
    {
        if statement.level() == 0 {
            continue;
        }
        let old = statement.module_path();
        let new = if to_package {
            format!(".{old}")
        } else if statement.level() > 1 {
            old[1..].into()
        } else {
            format!(
                "{}{}",
                file.module_name
                    .as_ref()
                    .ok_or("Cannot resolve package name")?,
                if old == "." { "" } else { old.as_str() }
            )
        };
        let text = statement.syntax().text().to_string();
        let from = text.find("from").ok_or("Malformed import")? + 4;
        let import = text.rfind("import").ok_or("Malformed import")?;
        let prefix = &text[from..import];
        let begin = from
            + prefix
                .find(|c: char| !c.is_whitespace())
                .ok_or("Malformed relative import")?;
        let end = import
            - prefix
                .chars()
                .rev()
                .take_while(|c| c.is_whitespace())
                .map(char::len_utf8)
                .sum::<usize>();
        let start = usize::from(statement.text_range().start());
        edits.push((
            TextRange::new(
                TextSize::new((start + begin) as u32),
                TextSize::new((start + end) as u32),
            ),
            new,
        ));
    }
    Ok(edits)
}
pub fn package_conversion(
    db: &Analysis,
    file: &Arc<FileInfo>,
    to_package: bool,
) -> Result<Vec<FileOp>, String> {
    let path = &file.path;
    let mut operations = Vec::new();
    if to_package {
        let moves = refactor2::convert_to_package(path)?;
        let edits = relative_edits(file, true)?;
        if !edits.is_empty() {
            operations.push(FileOp::Edit(path.clone(), edits));
        }
        operations.extend(moves);
        let stub = path.with_extension("pyi");
        if stub.exists() || db.cached_file_for_path(&stub).is_some() {
            if let Some(source) = db.file_for_path(&stub) {
                let edits = relative_edits(&source, true)?;
                if !edits.is_empty() {
                    operations.push(FileOp::Edit(stub.clone(), edits));
                }
            }
            operations.push(FileOp::Move(
                stub,
                path.with_extension("").join("__init__.pyi"),
            ));
        }
    } else {
        if path.file_name().and_then(|n| n.to_str()) != Some("__init__.py") {
            return Err("Open a package's __init__.py".into());
        }
        let directory = path.parent().ok_or("Missing package directory")?;
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !matches!(
                entry.file_name().to_str(),
                Some("__init__.py" | "__init__.pyi" | "__pycache__")
            ) {
                return Err("Move package child modules/data before converting to a module".into());
            }
        }
        let target = directory.with_extension("py");
        let target_stub = directory.with_extension("pyi");
        if target.exists() || target_stub.exists() {
            return Err("A destination module/stub already exists".into());
        }
        let edits = relative_edits(file, false)?;
        if !edits.is_empty() {
            operations.push(FileOp::Edit(path.clone(), edits));
        }
        operations.push(FileOp::Move(path.clone(), target));
        let stub = directory.join("__init__.pyi");
        if stub.exists() {
            if let Some(source) = db.file_for_path(&stub) {
                let edits = relative_edits(&source, false)?;
                if !edits.is_empty() {
                    operations.push(FileOp::Edit(stub.clone(), edits));
                }
            }
            operations.push(FileOp::Move(stub, target_stub));
        }
        operations.push(FileOp::Delete(directory.into()));
    }
    Ok(operations)
}

fn local_to_field(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    name: &str,
) -> Result<Vec<FileOp>, String> {
    let Symbol::Name { def, .. } =
        usages::symbol_at(db, file, offset).ok_or("Select a local variable")?
    else {
        return Err("Select a local variable".into());
    };
    if def.file.id != file.id || !matches!(def.binding().kind, BindingKind::Assignment) {
        return Err("Select a local assignment".into());
    }
    let binding = def.binding();
    if external_writer(file, &binding.name) {
        return Err("Global/nonlocal writers prevent owning all local uses".into());
    }
    let function = function_at(file, binding.range.start())?;
    let receiver = receiver(&function)?;
    let class = class_at(file, binding.range.start())?;
    class_plain(db, file, &class)?;
    let name = if name.trim().is_empty() {
        binding.name.as_str()
    } else {
        name.trim()
    };
    identifier(name)?;
    if file.scopes.bindings_in(binding.scope, &binding.name).len() != 1 {
        return Err("Resolve reassignments before converting this local to a field".into());
    }
    let reference = rustpy_semantic::types::ClassRef {
        file: file.id,
        range: class.text_range(),
        name: class.name_text().into(),
    };
    if Inferer::new(db)
        .lookup_member(&Type::Instance(reference, Vec::new()), name)
        .is_some()
    {
        return Err("The destination field already exists".into());
    }
    let inf = Inferer::new(db);
    let receiver_def = inf
        .resolve_name(file, binding.scope, &receiver)
        .ok_or("Cannot resolve receiver")?;
    let mut edits = vec![(binding.range, format!("{receiver}.{name}"))];
    for reference in file.scopes.references_to(def.binding) {
        if reference.is_write {
            return Err("A writing reference prevents field conversion".into());
        }
        let actual = inf
            .resolve_name(file, reference.scope, &receiver)
            .ok_or("A nested scope hides the receiver")?;
        if actual.file.id != receiver_def.file.id || actual.binding != receiver_def.binding {
            return Err("A nested scope hides the receiver".into());
        }
        edits.push((reference.range, format!("{receiver}.{name}")));
    }
    Ok(one(file, edits))
}

fn method_static(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
) -> Result<Vec<FileOp>, String> {
    let function = function_at(file, offset)?;
    let receiver = receiver(&function)?;
    class_plain(db, file, &class_at(file, offset)?)?;
    if function.decorators().next().is_some() {
        return Err("Resolve existing decorators before making a method static".into());
    }
    let scope = file
        .scopes
        .scope_for_node(function.text_range())
        .ok_or("Missing method scope")?;
    let ids = file.scopes.bindings_in(scope, &receiver);
    let id = *ids.first().ok_or("Missing receiver parameter")?;
    if !file.scopes.references_to(id).is_empty() {
        return Err("The method uses its receiver; make it a top-level function or remove those dependencies first".into());
    }
    let mut model = change_signature::signature_model(file, offset)?;
    model.params.remove(0);
    model.implicit_first = false;
    let parameters = function.params().ok_or("Missing parameters")?;
    let mut result = BTreeMap::new();
    add(
        &mut result,
        file,
        TextRange::new(
            parameters.text_range().start() + TextSize::new(1),
            parameters.text_range().end() - TextSize::new(1),
        ),
        model.render(),
    );
    let (builtin, import) = builtin_import(file);
    result.entry(file.path.clone()).or_default().push(import);
    let indent = indent_at(&file.text, usize::from(function.text_range().start()));
    add(
        &mut result,
        file,
        TextRange::empty(function.text_range().start()),
        format!("@{builtin}.staticmethod\n{indent}"),
    );
    let calls = function_call_sites(db, file, &function, files);
    for site in &calls {
        if site.unresolved {
            return Err("An unresolved receiver prevents updating all method calls".into());
        }
        if site.implicit != 0 {
            continue;
        }
        let list = site.args().ok_or("Missing arguments")?;
        let first = list
            .args()
            .next()
            .ok_or("An unbound method call has no receiver argument")?;
        let Some(value) = first.value() else {
            return Err("Cannot resolve the receiver argument".into());
        };
        if !matches!(value, ast::Expr::Name(_) | ast::Expr::Literal(_)) {
            return Err("An unbound call's receiver has side effects; rewrite it explicitly before conversion".into());
        }
        let callee = site.call.callee().ok_or("Missing method callee")?;
        let inner = TextRange::new(
            list.text_range().start() + TextSize::new(1),
            list.text_range().end() - TextSize::new(1),
        );
        let removal = edit_helpers::element_with_comma(&site.file.text, first.syntax());
        let mut args =
            site.file.text[usize::from(inner.start())..usize::from(inner.end())].to_string();
        args.replace_range(
            usize::from(removal.start().max(inner.start()) - inner.start())
                ..usize::from(removal.end().min(inner.end()) - inner.start()),
            "",
        );
        add(
            &mut result,
            &site.file,
            site.call.text_range(),
            format!(
                "({}, {}({args}))[-1]",
                value.syntax().text(),
                callee.syntax().text()
            ),
        );
    }
    for usage in find_usages(
        db,
        file,
        function
            .name()
            .ok_or("Missing method name")?
            .text_range()
            .start(),
        files,
    )
    .iter()
    .filter(|u| !u.is_definition)
    {
        if !calls
            .iter()
            .any(|s| s.file.path == usage.path && s.call.text_range().contains_range(usage.range))
        {
            return Err("An escaping method value prevents changing receiver binding".into());
        }
    }
    Ok(changes(result))
}
fn function_method(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
    target_name: &str,
) -> Result<Vec<FileOp>, String> {
    let function = function_at(file, offset)?;
    if function.is_method() || function.decorators().next().is_some() {
        return Err("Select an undecorated top-level function".into());
    }
    if function
        .syntax()
        .parent()
        .is_none_or(|p| p.kind() != SyntaxKind::MODULE)
    {
        return Err("Move a nested function to top level first".into());
    }
    let target = file
        .parse
        .syntax_node()
        .children()
        .filter_map(ast::ClassDef::cast)
        .find(|class| class.name_text() == target_name)
        .ok_or("Choose a top-level destination class in this file")?;
    class_plain(db, file, &target)?;
    let first = function
        .params()
        .and_then(|p| p.plain_params().next())
        .ok_or("The function needs an explicit first receiver parameter")?;
    let target_scope = file
        .scopes
        .scope_for_node(target.text_range())
        .ok_or("Missing destination class scope")?;
    for name in function
        .syntax()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .filter(|name| {
            function
                .body()
                .is_none_or(|body| !body.text_range().contains_range(name.text_range()))
        })
    {
        if !file
            .scopes
            .bindings_in(target_scope, &name.name_text())
            .is_empty()
        {
            return Err(
                "The destination class would capture a header annotation/default name".into(),
            );
        }
    }

    if first.default().is_some() {
        return Err("The receiver parameter cannot have a default".into());
    }
    let class = rustpy_semantic::types::ClassRef {
        file: file.id,
        range: target.text_range(),
        name: target.name_text().into(),
    };
    let inf = Inferer::new(db);
    if inf
        .lookup_member(
            &Type::Instance(class.clone(), Vec::new()),
            &function.name_text(),
        )
        .is_some()
    {
        return Err("The destination class already has that member".into());
    }
    if function
        .syntax()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .any(|n| matches!(n.name_text().as_str(), "super" | "__class__"))
    {
        return Err("Moving this function would change class-cell semantics".into());
    }
    let model = change_signature::signature_model(file, offset)?;
    for p in &model.params {
        if p.default.as_ref().is_some_and(|s| {
            let p = rustpy_syntax::parse(s);
            p.syntax_node()
                .descendants()
                .find_map(ast::Expr::cast)
                .is_none_or(|e| !matches!(e, ast::Expr::Literal(_) | ast::Expr::String(_)))
        }) {
            return Err(
                "Evaluated or mutable defaults require retaining their original definition context"
                    .into(),
            );
        }
    }
    let calls = function_call_sites(db, file, &function, files);
    let mut edits = BTreeMap::new();
    for site in &calls {
        if site.unresolved || site.implicit != 0 {
            return Err("The call does not bind the original free function".into());
        }
        let args = site.args().ok_or("Missing arguments")?;
        let first_arg = args.args().next().ok_or("A call is missing its receiver")?;
        if !matches!(first_arg, ast::AnyArg::Positional(_)) {
            return Err("Pass the receiver first positionally to preserve evaluation order".into());
        }
        let object = first_arg.value().ok_or("Missing receiver")?;
        let Type::Instance(actual, _) = inf.type_of_expr(&site.file, &object) else {
            return Err("A call receiver's class is unknown".into());
        };
        if !inf.mro(&actual).iter().any(|c| c == &class) {
            return Err("A call receiver does not belong to the destination class".into());
        }
        let removal = edit_helpers::element_with_comma(&site.file.text, first_arg.syntax());
        let inner = TextRange::new(
            args.text_range().start() + TextSize::new(1),
            args.text_range().end() - TextSize::new(1),
        );
        let mut text =
            site.file.text[usize::from(inner.start())..usize::from(inner.end())].to_string();
        let start = usize::from(removal.start().max(inner.start()) - inner.start());
        let end = usize::from(removal.end().min(inner.end()) - inner.start());
        text.replace_range(start..end, "");
        add(
            &mut edits,
            &site.file,
            site.call.text_range(),
            format!(
                "({}).{}({text})",
                object.syntax().text(),
                function.name_text()
            ),
        );
    }
    let body_indent = refactor2::class_body_indent(&file.text, &target);
    let method = edit_helpers::reindent(&function.syntax().text().to_string(), "", &body_indent);
    edits
        .entry(file.path.clone())
        .or_default()
        .extend(refactor2::insert_members(
            &file.text,
            &target,
            &[(true, format!("{method}\n"))],
        ));
    let args = model
        .params
        .iter()
        .skip(1)
        .map(|p| match p.kind {
            change_signature::ParamKind::VarArgs => format!("*{}", p.name),
            change_signature::ParamKind::KwArgs => format!("**{}", p.name),
            change_signature::ParamKind::KeywordOnly => format!("{}={}", p.name, p.name),
            _ => p.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let body = function.body().ok_or("Missing function body")?;
    let original_indent = indent_at(&file.text, usize::from(body.text_range().start()));
    let statement = format!(
        "return {}{}.{}({args})",
        if function.is_async() { "await " } else { "" },
        first.name_text(),
        function.name_text()
    );
    let body_statements: Vec<_> = body.statements().map(|s| s.syntax().clone()).collect();
    let from = body_statements
        .first()
        .ok_or("Missing function statement")?
        .text_range()
        .start();
    let to = body_statements.last().unwrap().text_range().end();
    add(
        &mut edits,
        file,
        TextRange::new(from, to),
        format!("{statement}\n{original_indent}"),
    );
    Ok(changes(edits))
}
fn boolean(db: &Analysis, ty: &Type) -> bool {
    match ty {
        Type::Literal(LiteralValue::Bool(_)) => true,
        Type::Instance(c, _) => {
            c.name == "bool"
                && db.file(c.file).is_some_and(|f| {
                    f.path.starts_with("<typeshed>") && f.module_name.as_deref() == Some("builtins")
                })
        }
        _ => false,
    }
}
fn negation(expression: &ast::Expr) -> String {
    if let ast::Expr::Not(not) = expression {
        if let Some(value) = not.operand() {
            return value.syntax().text().to_string();
        }
    }
    format!("not ({})", expression.syntax().text())
}
fn inverted_read(file: &FileInfo, range: TextRange) -> Result<Edit, String> {
    let node = node_with_range(&file.parse.syntax_node(), range).ok_or("Missing boolean usage")?;
    let expression = node
        .ancestors()
        .find_map(ast::Expr::cast)
        .ok_or("Boolean usage is not an expression")?;
    if let Some(parent) = expression.syntax().parent().and_then(ast::NotExpr::cast) {
        return Ok((parent.text_range(), expression.syntax().text().to_string()));
    }
    Ok((
        expression.text_range(),
        format!("not ({})", expression.syntax().text()),
    ))
}
fn invert_boolean(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
) -> Result<Vec<FileOp>, String> {
    // A caret on `def` has no identifier symbol; signature_target below
    // deliberately accepts the containing declaration header.
    let symbol = usages::symbol_at(db, file, offset);
    let mut result = BTreeMap::new();
    let inf = Inferer::new(db);
    if let Some(Symbol::Name { def, .. }) = &symbol {
        if matches!(def.binding().kind, BindingKind::Assignment) {
            let owner = &def.file;
            let b = def.binding();
            if external_writer(owner, &b.name) {
                return Err("Global/nonlocal writers prevent boolean inversion".into());
            }
            if owner.scopes.bindings_in(b.scope, &b.name).len() != 1 {
                return Err("Resolve reassignments before inverting this boolean".into());
            }
            let value = b
                .value
                .and_then(|r| node_with_range(&owner.parse.syntax_node(), r))
                .and_then(ast::Expr::cast)
                .ok_or("Missing boolean initializer")?;
            if !boolean(db, &inf.type_of_expr(owner, &value)) {
                return Err("The initializer must be a proven bool".into());
            }
            add(&mut result, owner, value.text_range(), negation(&value));
            for usage in find_usages(db, owner, b.range.start(), files)
                .iter()
                .filter(|u| !u.is_definition)
            {
                if usage.dynamic || usage.is_write {
                    return Err("Dynamic/writing boolean clients prevent inversion".into());
                }
                let target = db.file_for_path(&usage.path).ok_or("Missing client")?;
                if ancestor_at::<ast::ImportAlias>(&target.parse.syntax_node(), usage.range.start())
                    .is_some()
                {
                    continue;
                }
                result
                    .entry(target.path.clone())
                    .or_default()
                    .push(inverted_read(&target, usage.range)?);
            }
            return Ok(changes(result));
        }
    }
    let (owner, model) = change_signature::signature_target(db, file, offset)?;
    let function = node_with_range(&owner.parse.syntax_node(), model.def_range)
        .and_then(ast::FuncDef::cast)
        .ok_or("Missing function")?;
    if function.is_async() || function.decorators().next().is_some() {
        return Err("Invert the undecorated synchronous implementation".into());
    }
    let body = function.body().ok_or("Missing body")?;
    if body.syntax().descendants().any(|n| {
        matches!(
            n.kind(),
            SyntaxKind::FUNC_DEF
                | SyntaxKind::LAMBDA_EXPR
                | SyntaxKind::YIELD_EXPR
                | SyntaxKind::YIELD_FROM_EXPR
        )
    }) {
        return Err("Nested callable/generator returns need a separate conversion".into());
    }
    let returns: Vec<_> = body
        .syntax()
        .descendants()
        .filter_map(ast::ReturnStmt::cast)
        .collect();
    if returns.is_empty() || !matches!(body.statements().last(), Some(ast::Stmt::Return(_))) {
        return Err("Every normal function exit must return an explicit boolean".into());
    }
    for ret in returns {
        let value = ret.value().ok_or("A bare return is not boolean")?;
        if !boolean(db, &inf.type_of_expr(&owner, &value)) {
            return Err("A return value is not a proven bool".into());
        }
        add(&mut result, &owner, value.text_range(), negation(&value));
    }
    let calls = function_call_sites(db, &owner, &function, files);
    for usage in find_usages(
        db,
        &owner,
        function.name().unwrap().text_range().start(),
        files,
    )
    .iter()
    .filter(|u| !u.is_definition)
    {
        if calls
            .iter()
            .any(|s| s.file.path == usage.path && s.call.text_range().contains_range(usage.range))
        {
            continue;
        }
        let target = db
            .file_for_path(&usage.path)
            .ok_or("Missing function client")?;
        if ancestor_at::<ast::ImportAlias>(&target.parse.syntax_node(), usage.range.start())
            .is_none()
        {
            return Err(
                "The function escapes as a value; its clients cannot all be inverted".into(),
            );
        }
    }
    for site in calls {
        if site.unresolved {
            return Err("An unresolved invocation prevents boolean inversion".into());
        }
        if site.file.id == owner.id && function.text_range().contains_range(site.call.text_range())
        {
            return Err("Resolve recursive boolean calls before inversion".into());
        }
        let expression = ast::Expr::Call(site.call.clone());
        if let Some(parent) = expression.syntax().parent().and_then(ast::NotExpr::cast) {
            add(
                &mut result,
                &site.file,
                parent.text_range(),
                expression.syntax().text().to_string(),
            )
        } else {
            add(
                &mut result,
                &site.file,
                expression.text_range(),
                negation(&expression),
            );
        }
    }
    Ok(changes(result))
}

fn named_tuple(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<Vec<FileOp>, String> {
    let class = class_at(file, offset)?;
    if class.decorators().next().is_some()
        || !class.bases().is_empty()
        || class.type_params().is_some()
    {
        return Err("NamedTuple conversion requires a plain non-generic class without existing bases/decorators".into());
    }
    let init = class
        .methods()
        .find(|m| m.name_text() == "__init__")
        .ok_or("Choose a class with a field-only __init__")?;
    if init.decorators().next().is_some()
        || init
            .syntax()
            .descendants_with_tokens()
            .any(|token| token.kind() == SyntaxKind::COMMENT)
    {
        return Err(
            "Resolve decorated/comment-bearing initializer semantics before NamedTuple conversion"
                .into(),
        );
    }
    if class.body().is_some_and(|body| {
        body.statements()
            .filter(|stmt| !matches!(stmt, ast::Stmt::FuncDef(_)))
            .any(|stmt| {
                stmt.syntax()
                    .descendants_with_tokens()
                    .filter_map(|item| item.into_token())
                    .any(|token| {
                        matches!(
                            token.text(),
                            "__slots__"
                                | "_fields"
                                | "_field_defaults"
                                | "_make"
                                | "_replace"
                                | "_asdict"
                        )
                    })
            })
    }) {
        return Err("The class owns a reserved NamedTuple attribute".into());
    }
    let receiver = receiver(&init)?;
    let model = change_signature::signature_model(file, init.text_range().start())?;
    if model
        .params
        .iter()
        .skip(1)
        .any(|p| !matches!(p.kind, change_signature::ParamKind::Normal))
    {
        return Err(
            "NamedTuple construction cannot preserve keyword-only or variadic parameters".into(),
        );
    }
    let mut assigned = HashSet::new();
    for statement in init.body().ok_or("Missing initializer")?.statements() {
        if let ast::Stmt::Expr(e) = &statement {
            if matches!(e.expr(), Some(ast::Expr::String(_))) {
                continue;
            }
        }
        let ast::Stmt::Assign(assign) = statement else {
            return Err("Initializer has effects beyond storing constructor fields".into());
        };
        let targets = assign.targets();
        let (Some(ast::Expr::Attribute(target)), Some(ast::Expr::Name(value))) =
            (targets.first(), assign.value())
        else {
            return Err("Initializer must store each parameter unchanged".into());
        };
        if targets.len() != 1
            || !matches!(target.object(),Some(ast::Expr::Name(ref n)) if n.name_text()==receiver)
            || target.attr_text() != value.name_text()
        {
            return Err(
                "Constructor parameter/field names must agree for NamedTuple keyword calls".into(),
            );
        }
        assigned.insert(value.name_text());
    }
    if assigned.len() != model.params.len() - 1
        || model
            .params
            .iter()
            .skip(1)
            .any(|p| !assigned.contains(&p.name))
    {
        return Err("Initializer does not store every field exactly once".into());
    }
    for method in class.methods().filter(|m| m.name_text() != "__init__") {
        if matches!(
            method.name_text().as_str(),
            "__new__" | "__slots__" | "_make" | "_replace" | "_asdict"
        ) {
            return Err("The class overrides a NamedTuple reserved member".into());
        }
        for node in method.syntax().descendants() {
            if let Some(attribute) = ast::AttributeExpr::cast(node) {
                if assigned.contains(&attribute.attr_text())
                    && attribute
                        .syntax()
                        .ancestors()
                        .take_while(|n| n.kind() != SyntaxKind::FUNC_DEF)
                        .any(|n| {
                            matches!(
                                n.kind(),
                                SyntaxKind::ASSIGN_STMT
                                    | SyntaxKind::ANN_ASSIGN_STMT
                                    | SyntaxKind::AUG_ASSIGN_STMT
                                    | SyntaxKind::DEL_STMT
                                    | SyntaxKind::FOR_STMT
                                    | SyntaxKind::WITH_STMT
                                    | SyntaxKind::COMP_FOR
                            )
                        })
                {
                    return Err(
                        "A method may mutate a field; NamedTuple would make it immutable".into(),
                    );
                }
            }
        }
    }
    if db.module("typing").is_some_and(|owner| {
        !owner.path.starts_with("<typeshed>") && !usages::is_library_file(db, &owner)
    }) {
        return Err(
            "The project shadows typing; select its canonical NamedTuple provider first".into(),
        );
    }
    let alias = fresh(file, "_namedtuple_typing_");
    let indent = refactor2::class_body_indent(&file.text, &class);
    let mut fields = String::new();
    for parameter in model.params.iter().skip(1) {
        let annotation = parameter.annotation.as_deref().unwrap_or("object");
        let default = parameter
            .default
            .as_ref()
            .map(|value| format!(" = {value}"))
            .unwrap_or_default();
        fields.push_str(&format!(
            "{indent}{}: {annotation}{default}\n",
            parameter.name
        ));
    }
    let mut edits = vec![
        (
            TextRange::empty(class.name().ok_or("Missing class name")?.text_range().end()),
            format!("({alias}.NamedTuple)"),
        ),
        (
            edit_helpers::stmt_lines(&file.text, init.text_range()),
            fields,
        ),
        (
            TextRange::empty(rustpy_semantic::import_placement::import_insert_offset(
                file,
            )),
            format!("import typing as {alias}\n"),
        ),
    ];
    edits.sort_by_key(|(range, _)| range.start());
    Ok(one(file, edits))
}
fn explicit_args(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
    names: &str,
) -> Result<Vec<FileOp>, String> {
    let function = function_at(file, offset)?;
    if function.decorators().next().is_some() {
        return Err("Resolve decorators before changing variadic binding".into());
    }
    let mut model = change_signature::signature_model(file, offset)?;
    let index = model
        .params
        .iter()
        .position(|p| p.kind == change_signature::ParamKind::VarArgs)
        .ok_or("The function has no *args parameter")?;
    let original = model.params[index].name.clone();
    let calls = function_call_sites(db, file, &function, files);
    if calls.is_empty() {
        return Err("Call sites are needed to infer an explicit argument count".into());
    }
    let fixed = model
        .params
        .iter()
        .take(index)
        .filter(|p| {
            matches!(
                p.kind,
                change_signature::ParamKind::Normal | change_signature::ParamKind::PositionalOnly
            )
        })
        .count();
    let mut arity = None;
    let mut actuals = Vec::new();
    for site in &calls {
        if site.unresolved {
            return Err("An unresolved call prevents *args conversion".into());
        }
        let args: Vec<_> = site
            .args()
            .ok_or("Missing call arguments")?
            .args()
            .collect();
        if args.iter().any(|a| {
            matches!(
                a,
                ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_) | ast::AnyArg::Generator(_)
            )
        }) {
            return Err("Expanded call arguments prevent proving a fixed arity".into());
        }
        let positional: Vec<_> = args
            .into_iter()
            .filter(|a| matches!(a, ast::AnyArg::Positional(_)))
            .collect();
        let first = fixed.saturating_sub(site.implicit);
        if positional.len() < first {
            return Err(
                "A call relies on omitted positional defaults; resolve its arguments first".into(),
            );
        }
        let count = positional.len() - first;
        if count > 32 || arity.is_some_and(|n| n != count) {
            return Err("Callers have different or excessive variadic arities".into());
        }
        arity = Some(count);
        actuals.push((
            site.file.clone(),
            positional.into_iter().skip(first).collect::<Vec<_>>(),
        ));
    }
    let count = arity.unwrap_or(0);
    let explicit = if names.trim().is_empty() {
        (0..count)
            .map(|i| fresh(file, &format!("argument_{i}_")))
            .collect::<Vec<_>>()
    } else {
        names.split(',').map(|s| s.trim().to_string()).collect()
    };
    if explicit.len() != count {
        return Err(format!(
            "Provide exactly {count} comma-separated parameter names"
        ));
    }
    let mut unique = HashSet::new();
    for name in &explicit {
        identifier(name)?;
        if !unique.insert(name)
            || model.params.iter().any(|p| p.name == *name)
            || file.scopes.bindings.iter().any(|b| b.name == *name)
        {
            return Err("An explicit parameter name collides with an existing binding".into());
        }
    }
    for site in &calls {
        for arg in site.args().unwrap().args() {
            if let ast::AnyArg::Keyword(keyword) = arg {
                if explicit.iter().any(|name| keyword.name_text() == *name) {
                    return Err(
                        "A caller already uses an explicit parameter name as another keyword"
                            .into(),
                    );
                }
            }
        }
    }
    let annotation = model.params[index].annotation.clone();
    model.params.remove(index);
    for (number, name) in explicit.iter().enumerate() {
        let mut parameter = change_signature::SigParam::added(name, None, None);
        parameter.kind = change_signature::ParamKind::KeywordOnly;
        parameter.annotation = annotation.clone();
        model.params.insert(index + number, parameter);
    }
    let list = function.params().ok_or("Missing parameter list")?;
    if list
        .syntax()
        .descendants_with_tokens()
        .any(|t| t.kind() == SyntaxKind::COMMENT)
    {
        return Err("Changing this signature would discard comments".into());
    }
    let mut result = BTreeMap::new();
    add(
        &mut result,
        file,
        TextRange::new(
            list.text_range().start() + TextSize::new(1),
            list.text_range().end() - TextSize::new(1),
        ),
        model.render(),
    );
    let body = function.body().ok_or("Missing function body")?;
    let first = body.statements().next().ok_or("Missing first statement")?;
    if !file.text[edit_helpers::line_start(&file.text, usize::from(first.text_range().start()))
        ..usize::from(first.text_range().start())]
        .trim()
        .is_empty()
    {
        return Err("Expand the one-line body before converting *args".into());
    }
    let at = if function.docstring().is_some() {
        edit_helpers::after_last_line(&file.text, usize::from(first.text_range().end()))
    } else {
        edit_helpers::line_start(&file.text, usize::from(first.text_range().start()))
    };
    let indent = indent_at(&file.text, usize::from(first.text_range().start()));
    let tuple = if explicit.is_empty() {
        "()".into()
    } else {
        format!("({},)", explicit.join(", "))
    };
    add(
        &mut result,
        file,
        TextRange::empty(TextSize::new(at as u32)),
        format!("{indent}{original} = {tuple}\n"),
    );
    for (target, args) in actuals {
        for (arg, name) in args.iter().zip(&explicit) {
            let value = arg.value().ok_or("Missing argument")?;
            add(
                &mut result,
                &target,
                arg.text_range(),
                format!("{name}={}", value.syntax().text()),
            );
        }
    }
    for usage in find_usages(
        db,
        file,
        function.name().unwrap().text_range().start(),
        files,
    )
    .iter()
    .filter(|u| !u.is_definition)
    {
        if calls.iter().any(|site| {
            site.file.path == usage.path && site.call.text_range().contains_range(usage.range)
        }) {
            continue;
        }
        let target = db
            .file_for_path(&usage.path)
            .ok_or("Missing caller source")?;
        if ancestor_at::<ast::ImportAlias>(&target.parse.syntax_node(), usage.range.start())
            .is_none()
        {
            return Err(
                "An escaping callable prevents enumerating every variadic invocation".into(),
            );
        }
    }
    Ok(changes(result))
}
fn encapsulate_field(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
    backing: &str,
) -> Result<Vec<FileOp>, String> {
    let Symbol::Member {
        decl: Some((owner, range)),
        name,
        ..
    } = usages::symbol_at(db, file, offset).ok_or("Select an instance field")?
    else {
        return Err("Select an instance field".into());
    };
    let class = class_at(&owner, range.start())?;
    class_plain(db, &owner, &class)?;
    let reference = rustpy_semantic::types::ClassRef {
        file: owner.id,
        range: class.text_range(),
        name: class.name_text().into(),
    };
    let inf = Inferer::new(db);
    let info = inf.class_info(&reference).ok_or("Missing class metadata")?;
    if info
        .members
        .get(name.as_str())
        .is_none_or(|member| member.kind != rustpy_semantic::classes::MemberKind::InstanceVar)
    {
        return Err(
            "Only an instance field can be encapsulated without changing class-level access".into(),
        );
    }
    let backing = if backing.trim().is_empty() {
        format!("_{}", name.trim_start_matches('_'))
    } else {
        backing.trim().into()
    };
    identifier(&backing)?;
    if backing == name
        || inf
            .lookup_member(&Type::Instance(reference, Vec::new()), &backing)
            .is_some()
    {
        return Err("The backing field already exists".into());
    }
    let plan = crate::rename::rename_plan(
        db,
        &owner,
        range.start(),
        &backing,
        files,
        crate::rename::RenameOptions::default(),
    )?;
    if !plan.conflicts.is_empty() {
        return Err("The backing name conflicts with existing bindings".into());
    }
    let mut edits = Vec::new();
    for usage in plan
        .code
        .iter()
        .filter(|u| u.path == owner.path && class.text_range().contains_range(u.range))
    {
        edits.push((usage.range, backing.clone()));
    }
    let indent = refactor2::class_body_indent(&owner.text, &class);
    let body = format!("{indent}    ");
    let (builtin, import) = builtin_import(&owner);
    edits.push(import);
    let property=format!("{indent}@{builtin}.property\n{indent}def {name}(self):\n{body}return self.{backing}\n\n{indent}@{name}.setter\n{indent}def {name}(self, value):\n{body}self.{backing} = value\n");
    edits.extend(refactor2::insert_members(
        &owner.text,
        &class,
        &[(true, property)],
    ));
    Ok(one(&owner, edits))
}
fn constructor_factory(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    files: &[PathBuf],
    name: &str,
) -> Result<Vec<FileOp>, String> {
    let class = class_at(file, offset)?;
    class_plain(db, file, &class)?;
    let name = if name.trim().is_empty() {
        "create"
    } else {
        name.trim()
    };
    identifier(name)?;
    let reference = rustpy_semantic::types::ClassRef {
        file: file.id,
        range: class.text_range(),
        name: class.name_text().into(),
    };
    let inf = Inferer::new(db);
    if inf
        .lookup_member(&Type::Instance(reference, Vec::new()), name)
        .is_some()
    {
        return Err("A member already uses the factory name".into());
    }
    let init = class
        .methods()
        .find(|m| m.name_text() == "__init__")
        .ok_or("A source __init__ is required to bind constructor calls")?;
    let calls = function_call_sites(db, file, &init, files);
    let mut result = BTreeMap::new();
    for site in calls {
        if site.unresolved {
            return Err("An unresolved constructor prevents factory migration".into());
        }
        let callee = site.call.callee().ok_or("Missing constructor")?;
        let Type::Class(class_ref, _) = inf.type_of_expr(&site.file, &callee) else {
            continue;
        };
        if inf
            .lookup_member(&Type::Class(class_ref, Vec::new()), name)
            .is_some()
        {
            return Err("An inheritor already has a different factory with that name".into());
        }
        add(
            &mut result,
            &site.file,
            callee.text_range(),
            format!("{}.{name}", callee.syntax().text()),
        );
    }
    let (builtin, import) = builtin_import(file);
    result.entry(file.path.clone()).or_default().push(import);
    let indent = refactor2::class_body_indent(&file.text, &class);
    let factory=format!("{indent}@{builtin}.classmethod\n{indent}def {name}(cls, *args, **kwargs):\n{indent}    return cls(*args, **kwargs)\n");
    result
        .entry(file.path.clone())
        .or_default()
        .extend(refactor2::insert_members(
            &file.text,
            &class,
            &[(true, factory)],
        ));
    Ok(changes(result))
}

fn migrate_api(
    db: &Analysis,
    current: &Arc<FileInfo>,
    files: &[PathBuf],
    mapping: &str,
) -> Result<Vec<FileOp>, String> {
    let mut rules = BTreeMap::new();
    for line in mapping
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        let (old, new) = line
            .split_once('=')
            .ok_or("Use old.qualified.name=new.qualified.name, one mapping per line")?;
        let old = old.trim();
        let new = new.trim();
        if !old.contains('.')
            || !new.contains('.')
            || old
                .split('.')
                .chain(new.split('.'))
                .any(|part| identifier(part).is_err())
        {
            return Err("Migration paths must be qualified Python identifiers".into());
        }
        if rules.insert(old.to_string(), new.to_string()).is_some() {
            return Err("A migration source has multiple destinations".into());
        }
    }
    if rules.is_empty() || rules.len() > 128 {
        return Err("Provide between 1 and 128 migration mappings".into());
    }
    let inf = Inferer::new(db);
    let mut result = BTreeMap::new();
    let mut remaining = 200_000usize;
    for path in files.iter().chain(std::iter::once(&current.path)) {
        if result.contains_key(path) {
            continue;
        }
        let Some(file) = db.file_for_path(path) else {
            continue;
        };
        if file.is_stub || usages::is_library_file(db, &file) {
            continue;
        }
        let mut migrated_bindings = HashSet::new();
        for (index, binding) in file.scopes.bindings.iter().enumerate() {
            let BindingKind::Import(info) = &binding.kind else {
                continue;
            };
            let Some(name) = &info.name else { continue };
            let module = db
                .module_relative(&file, info.level, &info.module)
                .and_then(|module| module.module_name.clone())
                .unwrap_or_else(|| info.module.to_string());
            let qualified = format!("{module}.{name}");
            let Some(destination) = rules.get(&qualified) else {
                continue;
            };
            let (module, name) = destination.rsplit_once('.').unwrap();
            let alias =
                ancestor_at::<ast::ImportAlias>(&file.parse.syntax_node(), binding.range.start())
                    .ok_or("Missing import alias")?;
            let statement = alias
                .syntax()
                .parent()
                .and_then(ast::ImportFromStmt::cast)
                .ok_or("Select a from-import for this mapping")?;
            let indent = indent_at(&file.text, usize::from(statement.text_range().start()));
            let line =
                edit_helpers::line_start(&file.text, usize::from(statement.text_range().start()));
            if !file.text[line..usize::from(statement.text_range().start())]
                .trim()
                .is_empty()
            {
                return Err("Split the one-line import statement before API migration".into());
            }
            let import = format!("from {module} import {name} as {}", binding.name);
            if statement.aliases().count() == 1 {
                add(&mut result, &file, statement.text_range(), import);
            } else {
                add(
                    &mut result,
                    &file,
                    edit_helpers::element_with_comma(&file.text, alias.syntax()),
                    "",
                );
                add(
                    &mut result,
                    &file,
                    TextRange::empty(TextSize::new(line as u32)),
                    format!("{indent}{import}\n"),
                );
            }
            migrated_bindings.insert(rustpy_semantic::scope::BindingId(index as u32));
        }
        for attribute in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::AttributeExpr::cast)
        {
            if remaining == 0 {
                return Err("Migration scan exceeds its structural budget".into());
            }
            remaining -= 1;
            let object = attribute.object().ok_or("Missing API receiver")?;
            if let ast::Expr::Name(name) = &object {
                if file
                    .scopes
                    .reference_at(name.text_range().start())
                    .is_some_and(|r| r.resolved.is_some_and(|id| migrated_bindings.contains(&id)))
                {
                    continue;
                }
            }
            let ty = inf.type_of_expr(&file, &object);
            let mut owners = Vec::new();
            let mut module_receiver = false;
            match ty {
                Type::Module(id) => {
                    if let Some(module) = db.file(id).and_then(|f| f.module_name.clone()) {
                        owners.push(module);
                        module_receiver = true
                    }
                }
                Type::Instance(class, _) | Type::Class(class, _) => {
                    for class in inf.mro(&class).iter().take(32) {
                        if let Some(owner) = db.file(class.file) {
                            let Some(module) = &owner.module_name else {
                                continue;
                            };
                            if class.name == "TestCase" && module.starts_with("unittest") {
                                if owner.path.starts_with("<typeshed>") {
                                    owners.push("unittest.TestCase".into());
                                }
                            } else {
                                owners.push(format!("{module}.{}", class.name));
                            }
                        }
                    }
                }
                _ => {}
            }
            for owner in owners {
                let qualified = format!("{owner}.{}", attribute.attr_text());
                let Some(destination) = rules.get(&qualified) else {
                    continue;
                };
                let (new_owner, new_name) = destination.rsplit_once('.').unwrap();
                if owner == new_owner {
                    add(
                        &mut result,
                        &file,
                        attribute.attr().ok_or("Missing member token")?.text_range(),
                        new_name,
                    );
                } else if module_receiver {
                    let alias = fresh(&file, "_migration_api_");
                    let import = format!("import {new_owner} as {alias}\n");
                    let items = result.entry(file.path.clone()).or_default();
                    if !items.iter().any(|(_, text)| text == &import) {
                        items.push((
                            TextRange::empty(
                                rustpy_semantic::import_placement::import_insert_offset(&file),
                            ),
                            import,
                        ));
                    }
                    add(
                        &mut result,
                        &file,
                        attribute.text_range(),
                        format!("{alias}.{new_name}"),
                    );
                } else {
                    return Err("Moving an instance API to a different owner requires an explicit adapter, not a name-only map".into());
                }
                break;
            }
        }
    }
    if result.is_empty() {
        return Err("No resolved API usages match these mappings".into());
    }
    Ok(changes(result))
}
fn comprehension(
    file: &Arc<FileInfo>,
    offset: TextSize,
    kind: &str,
) -> Result<Vec<FileOp>, String> {
    if !matches!(kind, "tuple" | "list" | "set" | "dict") {
        return Err("Choose tuple, list, set or dict".into());
    }
    let root = file.parse.syntax_node();
    let node = edit_helpers::token_at(&root, offset)
        .and_then(|token| {
            token.parent_ancestors().find(|n| {
                matches!(
                    n.kind(),
                    SyntaxKind::LIST_COMP
                        | SyntaxKind::SET_COMP
                        | SyntaxKind::DICT_COMP
                        | SyntaxKind::GENERATOR_EXPR
                )
            })
        })
        .ok_or("Place the caret in a comprehension or generator expression")?;
    let full = node.text_range();
    let (element, key, value) = if let Some(dict) = ast::DictComp::cast(node.clone()) {
        let item = dict.item().ok_or("Missing dictionary item")?;
        (item.text_range(), item.key(), item.value())
    } else {
        let expression = ast::ListComp::cast(node.clone())
            .and_then(|c| c.element())
            .or_else(|| ast::SetComp::cast(node.clone()).and_then(|c| c.element()))
            .or_else(|| ast::GeneratorExpr::cast(node.clone()).and_then(|c| c.element()))
            .ok_or("Missing comprehension element")?;
        let pair = if let ast::Expr::Tuple(tuple) = &expression {
            let items: Vec<_> = tuple.elements().collect();
            if items.len() != 2 {
                return Err("Dictionary entries must be exact pairs".into());
            }
            Some((items.first().cloned(), items.get(1).cloned()))
        } else {
            None
        };
        let (key, value) = pair.unwrap_or((None, None));
        (expression.text_range(), key, value)
    };
    let raw = &file.text[usize::from(full.start())..usize::from(full.end())];
    let end = usize::from(full.end()) - usize::from(raw.starts_with(['[', '{', '(']));
    let suffix = &file.text[usize::from(element.end())..end];
    let original = &file.text[usize::from(element.start())..usize::from(element.end())];
    let entry = if ast::DictComp::can_cast(node.kind()) {
        format!(
            "({}, {})",
            key.as_ref()
                .ok_or("Missing dictionary key")?
                .syntax()
                .text(),
            value
                .as_ref()
                .ok_or("Missing dictionary value")?
                .syntax()
                .text()
        )
    } else {
        original.into()
    };
    let mut edits = Vec::new();
    let replacement = match kind {
        "list" => format!("[{entry}{suffix}]"),
        "set" => format!("{{{entry}{suffix}}}"),
        "dict" => {
            let key =
                key.ok_or("A dictionary conversion requires two-element (key, value) entries")?;
            let value =
                value.ok_or("A dictionary conversion requires two-element (key, value) entries")?;
            format!(
                "{{{}: {}{suffix}}}",
                key.syntax().text(),
                value.syntax().text()
            )
        }
        _ => {
            let (builtin, import) = builtin_import(file);
            edits.push(import);
            if node
                .descendants()
                .filter_map(ast::CompFor::cast)
                .any(|c| c.is_async())
            {
                format!("{builtin}.tuple([{entry}{suffix}])")
            } else {
                format!("{builtin}.tuple({entry}{suffix})")
            }
        }
    };
    edits.push((full, replacement));
    Ok(one(file, edits))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project(source: &str) -> (Analysis, Arc<FileInfo>, Vec<PathBuf>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let path = PathBuf::from("/proj/main.py");
        db.set_file_text(&path, source);
        let file = db.file_for_path(&path).unwrap();
        (db, file, vec![path])
    }
    fn convert(source: &str, needle: &str, options: ConversionOptions) -> Result<String, String> {
        let (db, file, paths) = project(source);
        let ops = plan(
            &db,
            &file,
            TextSize::new(source.find(needle).unwrap() as u32),
            &paths,
            Path::new("/proj"),
            &options,
        )?;
        let edits: Vec<_> = ops
            .into_iter()
            .filter_map(|op| match op {
                FileOp::Edit(path, edits) if path == file.path => Some(edits),
                _ => None,
            })
            .flatten()
            .collect();
        let output = edit_helpers::try_apply_edits(source, &edits)?;
        assert!(
            rustpy_syntax::parse(&output).errors().is_empty(),
            "{output}"
        );
        Ok(output)
    }
    fn option(kind: ConversionKind) -> ConversionOptions {
        ConversionOptions {
            kind,
            ..Default::default()
        }
    }
    #[test]
    fn local_field_tracks_lexical_receiver_and_declines_hidden_receiver() {
        let src = "class Box:\n    def read(this):\n        token = 'π'\n        return token\n";
        let text = convert(src, "token =", option(ConversionKind::LocalToField)).unwrap();
        assert!(text.contains("this.token = 'π'"));
        assert!(text.contains("return this.token"));
        let src="class Box:\n    def read(self):\n        value = 1\n        def nested(self):\n            return value\n        return nested(None)\n";
        assert!(convert(src, "value =", option(ConversionKind::LocalToField)).is_err());
    }
    #[test]
    fn static_method_keeps_receiver_evaluation_and_refuses_used_receiver() {
        let src="class Box:\n    def answer(self, value):\n        return value\nbox = Box()\nx = box.answer(4)\ny = Box.answer(box, 5)\n";
        let text = convert(src, "def answer", option(ConversionKind::MethodStatic)).unwrap();
        assert!(text.contains(".staticmethod"));
        assert!(text.contains("def answer(value)"));
        assert!(text.contains("box.answer(4)"));
        assert!(text.contains("(box, Box.answer(5))[-1]"));
        assert!(convert(
            "class Box:\n    def answer(self):\n        return self.x\n",
            "def answer",
            option(ConversionKind::MethodStatic)
        )
        .is_err());
    }
    #[test]
    fn variadic_expansion_preserves_local_tuple_and_keyword_evaluation_order() {
        let src = "def collect(head, *tail):\n    return head, tail, tail\nx = collect(1, 2, 3)\n";
        let mut options = option(ConversionKind::ExplicitArgs);
        options.name = "second, third".into();
        let text = convert(src, "def collect", options).unwrap();
        assert!(text.contains("def collect(head, *, second, third)"));
        assert!(text.contains("tail = (second, third,)"));
        assert!(text.contains("collect(1, second=2, third=3)"));
        assert!(convert(
            "def f(*args):\n    return args\nf(1)\nf(1, 2)\n",
            "def f",
            option(ConversionKind::ExplicitArgs)
        )
        .is_err());
    }
    #[test]
    fn namedtuple_fields_retain_annotations_defaults_and_decline_mutators() {
        let src="class Point:\n    def __init__(self, x: int, label: str = 'π'):\n        self.x = x\n        self.label = label\n    def show(self):\n        return self.label\n";
        let text = convert(src, "class Point", option(ConversionKind::NamedTuple)).unwrap();
        assert!(text.contains(".NamedTuple)"));
        assert!(text.contains("x: int"));
        assert!(text.contains("label: str = 'π'"));
        assert!(text.contains("def show"));
        assert!(!text.contains("def __init__"));
        let mutator = src.replace(
            "return self.label",
            "self.label += '!'\n        return self.label",
        );
        assert!(convert(&mutator, "class Point", option(ConversionKind::NamedTuple)).is_err());
    }
    #[test]
    fn comprehension_converts_real_pair_and_async_collection_shapes() {
        let src = "pairs = [(x, x + 1) for x in range(3)]\n";
        let mut opts = option(ConversionKind::Comprehension);
        opts.collection = "dict".into();
        let out = convert(src, "for x", opts).unwrap();
        assert!(out.contains("{x: x + 1 for x in range(3)}"));
        let out = convert(
            "values = [x for x in range(3)]\n",
            "for x",
            option(ConversionKind::Comprehension),
        )
        .unwrap();
        assert!(out.contains(".tuple(x for x in range(3))"));
        let out = convert(
            "async def f(xs):\n    return [x async for x in xs]\n",
            "async for",
            option(ConversionKind::Comprehension),
        )
        .unwrap();
        assert!(out.contains(".tuple([x async for x in xs])"));
    }
    #[test]
    fn package_depth_roundtrip_preserves_exact_relative_import_subjects() {
        let (_, file, _) = project("from . import value\nfrom ..parent import thing\n");
        let edits = relative_edits(&file, true).unwrap();
        let text = edit_helpers::try_apply_edits(&file.text, &edits).unwrap();
        assert!(text.contains("from .. import value"));
        assert!(text.contains("from ...parent import thing"));
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(
            Path::new("/proj/pkg/__init__.py"),
            "from . import value\nfrom .child import thing\nfrom ..parent import other\n",
        );
        let f = db
            .file_for_path(Path::new("/proj/pkg/__init__.py"))
            .unwrap();
        let out =
            edit_helpers::try_apply_edits(&f.text, &relative_edits(&f, false).unwrap()).unwrap();
        assert!(out.contains("from pkg import value"));
        assert!(out.contains("from pkg.child import thing"));
        assert!(out.contains("from .parent import other"));
    }
    #[test]
    fn factory_rewrites_constructor_invocations_through_forwarding_classmethod() {
        let src =
            "class Point:\n    def __init__(self, x=3):\n        self.x = x\np = Point(x=4)\n";
        let out = convert(
            src,
            "class Point",
            option(ConversionKind::ConstructorFactory),
        )
        .unwrap();
        assert!(out.contains("p = Point.create(x=4)"));
        assert!(out.contains("def create(cls, *args, **kwargs)"));
        assert!(out.contains("return cls(*args, **kwargs)"));
    }
    #[test]
    fn boolean_conversion_inverts_actual_clients_and_rejects_escaping_function_values() {
        let src = "flag = True\nresult = flag\nother = not flag\n";
        let text = convert(src, "flag =", option(ConversionKind::InvertBoolean)).unwrap();
        assert!(text.contains("flag = not (True)"));
        assert!(text.contains("result = not (flag)"));
        assert!(text.contains("other = flag"));
        let src = "def allowed():\n    return True\nresult = allowed()\n";
        let text = convert(src, "def allowed", option(ConversionKind::InvertBoolean)).unwrap();
        assert!(text.contains("return not (True)"));
        assert!(text.contains("result = not (allowed())"));
        let src = "def allowed():\n    return True\ncallback = allowed\n";
        assert!(convert(src, "def allowed", option(ConversionKind::InvertBoolean)).is_err());
    }
    #[test]
    fn property_conversion_keeps_external_attribute_api_and_owns_backing_storage() {
        let src="class Point:\n    def __init__(self, x):\n        self.x = x\n    def read(self):\n        return self.x\np = Point(4)\nprint(p.x)\n";
        let text = convert(src, "x = x", option(ConversionKind::EncapsulateField)).unwrap();
        assert!(text.contains("self._x = x"));
        assert!(text.contains("return self._x"));
        assert!(text.contains("def x(self)"));
        assert!(text.contains("@x.setter"));
        assert!(text.contains("print(p.x)"));
    }
    #[test]
    fn import_api_mapping_keeps_binding_aliases_and_real_calls() {
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(
            Path::new("/proj/old_api.py"),
            "def old(value):\n    return value\n",
        );
        db.set_file_text(
            Path::new("/proj/new_api.py"),
            "def modern(value):\n    return value\n",
        );
        let path = PathBuf::from("/proj/main.py");
        let src = "from old_api import old as retained\nprint(retained('π'))\n";
        db.set_file_text(&path, src);
        let file = db.file_for_path(&path).unwrap();
        let options = ConversionOptions {
            kind: ConversionKind::MigrateApi,
            migration: "old_api.old=new_api.modern".into(),
            ..Default::default()
        };
        let ops = plan(
            &db,
            &file,
            TextSize::new(0),
            std::slice::from_ref(&path),
            Path::new("/proj"),
            &options,
        )
        .unwrap();
        let edits = ops
            .into_iter()
            .find_map(|op| match op {
                FileOp::Edit(p, e) if p == path => Some(e),
                _ => None,
            })
            .unwrap();
        let text = edit_helpers::try_apply_edits(src, &edits).unwrap();
        assert!(text.contains("from new_api import modern as retained"));
        assert!(text.contains("print(retained('π'))"));
    }
    #[test]
    fn function_to_method_updates_calls_and_keeps_the_original_callable_bridge() {
        let src="class Box:\n    pass\ndef read(receiver, value):\n    return receiver, value\nbox = Box()\nresult = read(box, 3)\n";
        let mut options = option(ConversionKind::FunctionMethod);
        options.target_class = "Box".into();
        let out = convert(src, "def read", options).unwrap();
        assert!(out.contains("result = (box).read(3)"));
        assert!(out.contains("return receiver.read(value)"));
        assert_eq!(out.matches("def read(receiver, value)").count(), 2);
    }
    #[test]
    fn class_transformations_and_receiver_effects_are_explicit_conflicts() {
        let src = "@transform\nclass Point:\n    def answer(self):\n        return 1\n";
        assert!(convert(src, "def answer", option(ConversionKind::MethodStatic)).is_err());
        let src="class Point:\n    def answer(self):\n        return 1\nx = Point.answer(make_receiver())\n";
        assert!(convert(src, "def answer", option(ConversionKind::MethodStatic)).is_err());
    }
}
