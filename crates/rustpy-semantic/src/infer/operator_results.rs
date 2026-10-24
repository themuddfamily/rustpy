//! Value results of arithmetic and rich comparison protocols.
use super::*;

const OPERATOR_WIDTH: usize = 64;
const OPERATOR_DEPTH: usize = 16;
const COMPARISON_PAIRS: usize = 32;
const OPERATOR_TEXT_BYTES: u32 = 16;

struct OperatorWork {
    remaining: usize,
    bytes: usize,
}
impl OperatorWork {
    fn new() -> Self {
        Self {
            remaining: 2048,
            bytes: 16_384,
        }
    }
    fn charge(&mut self, amount: usize) -> Result<(), ()> {
        self.remaining = self.remaining.checked_sub(amount).ok_or(())?;
        Ok(())
    }
    fn width(&mut self, count: usize) -> Result<(), ()> {
        if count > OPERATOR_WIDTH {
            return Err(());
        }
        self.charge(count)
    }
    fn extend(&mut self, destination: &mut Vec<Type>, values: Vec<Type>) -> Result<(), ()> {
        self.width(destination.len().checked_add(values.len()).ok_or(())?)?;
        destination.extend(values);
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<(), ()> {
        self.bytes = self.bytes.checked_sub(text.len()).ok_or(())?;
        Ok(())
    }
    fn params(&mut self, params: &[Param], depth: usize) -> Result<(), ()> {
        self.width(params.len())?;
        for parameter in params {
            self.text(&parameter.name)?;
            if let Some(default) = &parameter.default_text {
                self.text(default)?;
            }
            self.owned(&parameter.ty, depth + 1)?;
        }
        Ok(())
    }
    fn signature(&mut self, signature: &Signature, depth: usize) -> Result<(), ()> {
        if depth > OPERATOR_DEPTH {
            return Err(());
        }
        self.charge(1)?;
        self.params(&signature.params, depth)?;
        self.owned(&signature.ret, depth + 1)?;
        if let Some(definition) = &signature.def {
            self.text(&definition.name)?;
        }
        if let Some(docstring) = &signature.docstring {
            self.text(docstring)?;
        }
        Ok(())
    }
    // Borrowed admission precedes every new recursive Type copy and equality
    // join. Nominal refs terminate here; their identity text is also bounded.
    fn owned(&mut self, value: &Type, depth: usize) -> Result<(), ()> {
        if depth > OPERATOR_DEPTH {
            return Err(());
        }
        self.charge(1)?;
        match value {
            Type::Class(class, values) | Type::Instance(class, values) => {
                self.text(&class.name)?;
                self.width(values.len())?;
                for value in values {
                    self.owned(value, depth + 1)?;
                }
            }
            Type::Tuple(values) | Type::Union(values) | Type::TypePack(values) => {
                self.width(values.len())?;
                for value in values {
                    self.owned(value, depth + 1)?;
                }
            }
            Type::Function(signature) | Type::Callable(signature) => {
                self.signature(signature, depth + 1)?
            }
            Type::BoundMethod(signature, receiver) => {
                self.signature(signature, depth + 1)?;
                self.owned(receiver, depth + 1)?;
            }
            Type::Overloaded(signatures) => {
                self.width(signatures.len())?;
                for signature in signatures {
                    self.signature(signature, depth + 1)?;
                }
            }
            Type::DeclaredTypeVar(definition) => {
                self.text(&definition.key)?;
                self.text(&definition.name)?;
                self.width(definition.constraints.len())?;
                for constraint in &definition.constraints {
                    self.owned(constraint, depth + 1)?;
                }
                if let Some(bound) = &definition.bound {
                    self.owned(bound, depth + 1)?;
                }
                if let Some(default) = &definition.default {
                    self.owned(default, depth + 1)?;
                }
            }
            Type::Unpack(value) => self.owned(value, depth + 1)?,
            Type::ParameterList(params) => self.params(params, depth)?,
            Type::Concatenate(values, specification) => {
                self.text(&specification.key)?;
                self.text(&specification.name)?;
                self.width(values.len())?;
                for value in values {
                    self.owned(value, depth + 1)?;
                }
            }
            Type::Super { after, receiver } => {
                self.text(&after.name)?;
                self.owned(receiver, depth + 1)?;
            }
            Type::Literal(LiteralValue::Str(text) | LiteralValue::Bytes(text))
            | Type::TypeVar(text) => self.text(text)?,
            Type::ParamSpec(definition) => {
                self.text(&definition.key)?;
                self.text(&definition.name)?;
            }
            Type::TypeVarTuple(definition) => {
                self.text(&definition.key)?;
                self.text(&definition.name)?;
            }
            Type::SelfType(class) => self.text(&class.name)?,
            Type::Literal(LiteralValue::EnumMember { class, member }) => {
                self.text(&class.name)?;
                self.text(member)?;
            }
            Type::LiteralString
            | Type::LiteralStringValue
            | Type::Unknown
            | Type::Any
            | Type::Never
            | Type::None
            | Type::Module(_)
            | Type::Literal(LiteralValue::Int(_) | LiteralValue::Bool(_)) => {}
        }
        Ok(())
    }
    fn join(&mut self, values: Vec<Type>) -> Result<Type, ()> {
        self.width(values.len())?;
        let mut flattened = 0usize;
        for value in &values {
            self.owned(value, 0)?;
            flattened = flattened
                .checked_add(match value {
                    Type::Union(values) => values.len(),
                    _ => 1,
                })
                .ok_or(())?;
            if flattened > OPERATOR_WIDTH {
                return Err(());
            }
        }
        Ok(Type::union(values))
    }
}

impl Inferer<'_> {
    fn strict_operator_subclass(
        &self,
        left: &Type,
        right: &Type,
        work: &mut OperatorWork,
    ) -> Result<Option<bool>, ()> {
        let (Some(left), Some(right)) = (left.class_ref(), right.class_ref()) else {
            return Ok(Some(false));
        };
        if left == right {
            return Ok(Some(false));
        }
        for class in [left, right] {
            let hierarchy = self.mro(class);
            work.width(hierarchy.len())?;
            for base in hierarchy.iter() {
                let Some(info) = self.class_info(base) else {
                    return Ok(None);
                };
                work.width(info.base_exprs.len())?;
                work.width(info.keywords.len())?;
                let Some(file) = self.db.file(base.file) else {
                    return Ok(None);
                };
                for (name, range) in &info.keywords {
                    if name == "metaclass" {
                        work.charge(1)?;
                        let Type::Class(meta, _) = self.type_of_range(&file, *range) else {
                            return Ok(None);
                        };
                        if self.builtin_class("type").as_ref() != Some(&meta) {
                            return Ok(None);
                        }
                    }
                }
            }
            for base in hierarchy.iter() {
                let Some(info) = self.class_info(base) else {
                    return Ok(None);
                };
                let Some(file) = self.db.file(base.file) else {
                    return Ok(None);
                };
                for range in &info.base_exprs {
                    work.charge(1)?;
                    if u32::from(range.len()) > 4096 {
                        return Err(());
                    }
                    let root = file.parse.syntax_node();
                    let start = match root.covering_element(*range) {
                        rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
                        rustpy_syntax::rowan::NodeOrToken::Token(token) => {
                            let Some(parent) = token.parent() else {
                                return Ok(None);
                            };
                            parent
                        }
                    };
                    let mut expression = None;
                    for node in start.ancestors().take(OPERATOR_DEPTH + 1) {
                        work.charge(1)?;
                        if node.text_range() == *range {
                            expression = ast::Expr::cast(node);
                            break;
                        }
                    }
                    let Some(expression) = expression else {
                        return Ok(None);
                    };
                    let (canonical_marker, _marker_spelling) =
                        self.operator_marker_base(&file, &expression, work)?;
                    if canonical_marker {
                        continue;
                    }
                    // Existing nominal MRO ignores these spellings. A project
                    // value/class named Generic/Protocol cannot certify that
                    // omitted edge merely by borrowing the marker's label.
                    let value = self.annotation_type(&file, &expression);
                    work.owned(&value, 0)?;
                    let (Type::Instance(resolved, _) | Type::Class(resolved, _)) = value else {
                        return Ok(None);
                    };
                    if resolved == *base || !hierarchy.contains(&resolved) {
                        return Ok(None);
                    }
                }
            }
        }
        Ok(Some(self.mro(right).iter().any(|base| base == left)))
    }

    fn operator_marker_base(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        work: &mut OperatorWork,
    ) -> Result<(bool, bool), ()> {
        work.charge(1)?;
        let head = match expression.clone().unparen() {
            ast::Expr::Subscript(subscript) => subscript.object(),
            expression => Some(expression),
        };
        let Some(head) = head else {
            return Ok((false, false));
        };
        let (definition, spelling) = match head {
            ast::Expr::Name(name) => {
                let text = name.name_text();
                work.text(&text)?;
                (
                    self.resolve_name(file, self.scope_of(file, name.syntax()), &text),
                    matches!(text.as_str(), "Generic" | "Protocol"),
                )
            }
            ast::Expr::Attribute(attribute) => {
                let text = attribute.attr_text();
                work.text(&text)?;
                let definition = attribute.object().and_then(|object| {
                    let Type::Module(id) = self.type_of_expr(file, &object) else {
                        return None;
                    };
                    let module = self.db.file(id)?;
                    self.resolve_in_module(&module, &text, 0)
                });
                (definition, matches!(text.as_str(), "Generic" | "Protocol"))
            }
            _ => return Ok((false, false)),
        };
        let Some(definition) = definition.map(|definition| self.follow_imports(definition)) else {
            return Ok((false, spelling));
        };
        let owner = &definition.file;
        let canonical = owner.is_stub
            && matches!(
                owner.module_name.as_deref(),
                Some("typing" | "typing_extensions")
            )
            && (owner.path == std::path::Path::new("<typeshed>/typing.pyi")
                || owner.path == std::path::Path::new("<typeshed>/typing_extensions.pyi"))
            && matches!(definition.binding().name.as_str(), "Generic" | "Protocol")
            && !Self::namespace_member_retired(
                file,
                definition.binding().name.as_str(),
                expression.text_range().start(),
            );
        Ok((canonical, spelling))
    }

    fn overridden_reflected_slot(
        &self,
        left: &Type,
        right: &Type,
        reflected: &str,
        work: &mut OperatorWork,
    ) -> Result<Option<bool>, ()> {
        let Some(strict) = self.strict_operator_subclass(left, right, work)? else {
            return Ok(None);
        };
        if !strict {
            return Ok(Some(false));
        }
        let Some(right_slot) = self.implicit_slot(right, reflected) else {
            return Ok(Some(false));
        };
        let right_definition = match right_slot {
            Type::Function(signature) | Type::BoundMethod(signature, _) => signature.def.clone(),
            _ => return Ok(None),
        };
        let Some(right_definition) = right_definition else {
            return Ok(None);
        };
        Ok(match self.implicit_slot(left, reflected) {
            None => Some(true),
            Some(Type::Function(signature) | Type::BoundMethod(signature, _)) => signature
                .def
                .as_ref()
                .map(|left_definition| left_definition != &right_definition),
            _ => None,
        })
    }

    fn operator_not_implemented(&self, value: &Type) -> bool {
        let Type::Instance(class, _) = value else {
            return false;
        };
        class.name == "NotImplementedType"
            && self.db.file(class.file).is_some_and(|owner| {
                owner.is_stub
                    && owner.module_name.as_deref() == Some("types")
                    && owner.path == std::path::Path::new("<typeshed>/types.pyi")
            })
    }

    fn operator_result_alternatives(
        &self,
        value: Type,
        work: &mut OperatorWork,
    ) -> Result<(Vec<Type>, bool), ()> {
        work.owned(&value, 0)?;
        let alternatives = match value {
            Type::Union(values) => values,
            value => vec![value],
        };
        work.width(alternatives.len())?;
        let mut ordinary = Vec::new();
        let mut declined = false;
        for value in alternatives {
            if self.operator_not_implemented(&value) {
                declined = true;
            } else {
                ordinary.push(value);
            }
        }
        Ok((ordinary, declined))
    }

    pub(super) fn binary_protocol_result(
        &self,
        left: &Type,
        right: &Type,
        name: &str,
        reflected: &str,
    ) -> Result<Type, ()> {
        let mut work = OperatorWork::new();
        work.owned(left, 0)?;
        work.owned(right, 0)?;
        let left = self.widen(left.clone());
        let right = self.widen(right.clone());
        self.binary_protocol_bounded(&left, &right, name, reflected, &mut work, 0)
    }

    fn binary_protocol_bounded(
        &self,
        left: &Type,
        right: &Type,
        name: &str,
        reflected: &str,
        work: &mut OperatorWork,
        depth: usize,
    ) -> Result<Type, ()> {
        if depth > OPERATOR_DEPTH {
            return Err(());
        }
        work.charge(1)?;
        if let Type::Union(values) = left {
            work.width(values.len())?;
            let mut results = Vec::with_capacity(values.len());
            for value in values {
                results.push(self.binary_protocol_bounded(
                    value,
                    right,
                    name,
                    reflected,
                    work,
                    depth + 1,
                )?);
            }
            return work.join(results);
        }
        if let Type::Union(values) = right {
            work.width(values.len())?;
            let mut results = Vec::with_capacity(values.len());
            for value in values {
                results.push(self.binary_protocol_bounded(
                    left,
                    value,
                    name,
                    reflected,
                    work,
                    depth + 1,
                )?);
            }
            return work.join(results);
        }
        work.charge(3)?;
        let Some(priority) = self.overridden_reflected_slot(left, right, reflected, work)? else {
            return Ok(Type::Unknown);
        };
        let same_type = left
            .class_ref()
            .is_some_and(|class| right.class_ref() == Some(class));
        let mut results = Vec::new();
        let slots = if priority {
            vec![(right, reflected, left), (left, name, right)]
        } else if same_type {
            vec![(left, name, right)]
        } else {
            vec![(left, name, right), (right, reflected, left)]
        };
        for (receiver, slot, argument) in slots {
            work.charge(1)?;
            if let Some(value) =
                self.dunder_call_checked(receiver, slot, std::slice::from_ref(argument))
            {
                let (values, declined) = self.operator_result_alternatives(value, work)?;
                work.extend(&mut results, values)?;
                if !declined {
                    return work.join(results);
                }
            }
        }
        if results.is_empty() {
            // Preserve the existing conservative result for signatures whose
            // argument metadata cannot establish applicability.
            work.charge(1)?;
            if let Some(value) = self.dunder_call(left, name, std::slice::from_ref(right)) {
                let (values, _) = self.operator_result_alternatives(value, work)?;
                work.extend(&mut results, values)?;
            }
        }
        if results.is_empty() {
            Ok(Type::Unknown)
        } else {
            work.join(results)
        }
    }

    fn rich_comparison_result(
        &self,
        operator: &str,
        left: &Type,
        right: &Type,
        work: &mut OperatorWork,
        depth: usize,
    ) -> Result<Type, ()> {
        if depth > OPERATOR_DEPTH {
            return Err(());
        }
        work.charge(1)?;
        if matches!(operator, "is" | "is not" | "in" | "not in") {
            return Ok(self.instance_of("bool"));
        }
        if matches!(left, Type::Unknown) || matches!(right, Type::Unknown) {
            return Ok(Type::Unknown);
        }
        if matches!(left, Type::Any) || matches!(right, Type::Any) {
            return Ok(Type::Any);
        }
        if matches!(left, Type::Never) || matches!(right, Type::Never) {
            return Ok(Type::Never);
        }

        if let Type::Union(values) = left {
            work.width(values.len())?;
            let mut results = Vec::with_capacity(values.len());
            for value in values {
                results.push(self.rich_comparison_result(
                    operator,
                    value,
                    right,
                    work,
                    depth + 1,
                )?);
            }
            return work.join(results);
        }
        if let Type::Union(values) = right {
            work.width(values.len())?;
            let mut results = Vec::with_capacity(values.len());
            for value in values {
                results.push(self.rich_comparison_result(
                    operator,
                    left,
                    value,
                    work,
                    depth + 1,
                )?);
            }
            return work.join(results);
        }
        let (name, reflected) = match operator {
            "<" => ("__lt__", "__gt__"),
            "<=" => ("__le__", "__ge__"),
            ">" => ("__gt__", "__lt__"),
            ">=" => ("__ge__", "__le__"),
            "==" => ("__eq__", "__eq__"),
            "!=" => ("__ne__", "__ne__"),
            _ => return Ok(Type::Unknown),
        };
        work.charge(1)?;
        let Some(strict) = self.strict_operator_subclass(left, right, work)? else {
            return Ok(Type::Unknown);
        };
        let slots = if strict {
            [(right, reflected, left), (left, name, right)]
        } else {
            [(left, name, right), (right, reflected, left)]
        };
        let mut results = Vec::new();
        for (receiver, slot, argument) in slots {
            work.charge(1)?;
            if let Some(value) =
                self.dunder_call_checked(receiver, slot, std::slice::from_ref(argument))
            {
                let (values, declined) = self.operator_result_alternatives(value, work)?;
                work.extend(&mut results, values)?;
                if !declined {
                    return work.join(results);
                }
            }
        }
        if matches!(operator, "==" | "!=") {
            work.extend(&mut results, vec![self.instance_of("bool")])?;
        }
        if results.is_empty() {
            Ok(Type::Unknown)
        } else {
            work.join(results)
        }
    }

    pub(super) fn comparison_value_result(
        &self,
        file: &Arc<FileInfo>,
        comparison: &ast::CompareExpr,
    ) -> Type {
        self.comparison_value_bounded(file, comparison, &mut OperatorWork::new())
            .unwrap_or(Type::Unknown)
    }

    fn comparison_value_bounded(
        &self,
        file: &Arc<FileInfo>,
        comparison: &ast::CompareExpr,
        work: &mut OperatorWork,
    ) -> Result<Type, ()> {
        let operands: Vec<_> = comparison.operands().take(COMPARISON_PAIRS + 2).collect();
        let operators: Vec<_> = comparison.ops().take(COMPARISON_PAIRS + 1).collect();
        if operators.len() > COMPARISON_PAIRS || operands.len() > COMPARISON_PAIRS + 1 {
            return Err(());
        }
        work.charge(operands.len() + operators.len())?;
        if operands.len() != operators.len() + 1 || operators.is_empty() {
            return Ok(Type::Unknown);
        }
        let mut results = Vec::new();
        work.charge(1)?;
        let mut left = self.type_of_expr(file, &operands[0]);
        for (index, operator) in operators.iter().enumerate() {
            if u32::from(operator.syntax().text_range().len()) > OPERATOR_TEXT_BYTES {
                return Err(());
            }
            work.charge(1)?;
            let right = self.type_of_expr(file, &operands[index + 1]);
            work.owned(&left, 0)?;
            work.owned(&right, 0)?;
            let value = self.rich_comparison_result(
                &operator.text(),
                &self.widen(left),
                &self.widen(right.clone()),
                work,
                0,
            )?;
            // A chain returns an earlier false-valued result object or its final
            // result, not a forced bool. Never has no normal result on this path.
            if matches!(value, Type::Never) {
                return if results.is_empty() {
                    Ok(Type::Never)
                } else {
                    work.join(results)
                };
            }
            work.extend(&mut results, vec![value])?;
            left = right;
        }
        work.join(results)
    }
}
