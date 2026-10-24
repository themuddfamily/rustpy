//! Slice values and bounded fixed-tuple slice projections.
use super::*;

#[derive(Clone, Copy)]
enum SliceBound {
    Missing,
    Known(i64),
    Dynamic,
    Invalid,
}

impl Inferer<'_> {
    fn slicing_builtin(&self, name: &str) -> Option<ClassRef> {
        let class = self.builtin_class(name)?;
        let file = self.db.file(class.file)?;
        (file.is_stub && file.path == std::path::Path::new("<typeshed>/builtins.pyi"))
            .then_some(class)
    }

    pub(super) fn slice_value_type(&self, file: &Arc<FileInfo>, slice: &ast::SliceExpr) -> Type {
        // Constructing a slice does not apply its bounds. Even slice(0, 1, 0)
        // is a real slice value; a custom __getitem__ may accept it.
        let (start, stop, step) = slice.parts();
        for value in [start, stop, step].into_iter().flatten() {
            let _ = self.type_of_expr(file, &value);
        }
        self.slicing_builtin("slice")
            .map(|class| Type::Instance(class, Vec::new()))
            .unwrap_or(Type::Unknown)
    }

    pub(super) fn slice_subscript_result(
        &self,
        file: &Arc<FileInfo>,
        subscript: &ast::SubscriptExpr,
        base: &Type,
    ) -> Option<Type> {
        if let Some(item) = subscript
            .index_expr()
            .and_then(|index| self.typed_dict_item(file, base, &index))
        {
            return Some(item);
        }
        let index = subscript.index()?;
        if index.kind() == SyntaxKind::SLICE_LIST {
            let parts: Vec<_> = index
                .children()
                .filter_map(ast::Expr::cast)
                .take(33)
                .collect();
            if parts.len() > 32 {
                return Some(Type::Unknown);
            }
            // Python passes a tuple for a comma-separated index list. Builtin
            // fixed tuples reject that key; custom receivers retain real dispatch.
            let mut budget = 1024;
            if !charge_slice_type(base, &mut budget, 0) {
                return Some(Type::Unknown);
            }
            let mut values = Vec::new();
            for part in parts {
                let value = self.type_of_expr(file, &part);
                if !charge_slice_type(&value, &mut budget, 0) {
                    return Some(Type::Unknown);
                }
                values.push(value);
            }
            let key = Type::Tuple(values);
            let result = self.multiple_index_result(base, &key, &mut budget, 0);
            return Some(if budget == 0 { Type::Unknown } else { result });
        }
        let ast::Expr::Slice(slice) = ast::Expr::cast(index)?.unparen() else {
            return None;
        };
        let mut budget = 1024;
        if !charge_slice_type(base, &mut budget, 0) {
            return Some(Type::Unknown);
        }
        let (start, stop, step) = slice.parts();
        let bounds = [
            self.slice_bound(file, start),
            self.slice_bound(file, stop),
            self.slice_bound(file, step),
        ];
        let key = self
            .slicing_builtin("slice")
            .map(|class| Type::Instance(class, Vec::new()))
            .unwrap_or(Type::Unknown);
        if key.is_unknown() {
            return Some(Type::Unknown);
        }
        let result = self.sliced_value_result(base, &key, bounds, &mut budget, 0);
        Some(if budget == 0 { Type::Unknown } else { result })
    }

    fn slice_bound(&self, file: &Arc<FileInfo>, expression: Option<ast::Expr>) -> SliceBound {
        let Some(expression) = expression else {
            return SliceBound::Missing;
        };
        match self.type_of_expr(file, &expression) {
            Type::None => SliceBound::Missing,
            Type::Literal(LiteralValue::Int(value)) => SliceBound::Known(value),
            Type::Literal(LiteralValue::Bool(value)) => SliceBound::Known(i64::from(value)),
            Type::Literal(LiteralValue::EnumMember { class, member }) => {
                match self.enum_primitive_value(&class, &member) {
                    Some(LiteralValue::Int(value)) => SliceBound::Known(value),
                    Some(LiteralValue::Bool(value)) => SliceBound::Known(i64::from(value)),
                    _ => SliceBound::Invalid,
                }
            }
            Type::Literal(_) => SliceBound::Invalid,
            Type::Any | Type::Unknown => SliceBound::Dynamic,
            Type::Instance(class, _) if self.slicing_builtin("int").as_ref() == Some(&class) => {
                SliceBound::Dynamic
            }
            value => {
                let indexed = self.dunder_call_checked(&value, "__index__", &[]);
                match indexed {
                    Some(Type::Literal(LiteralValue::Int(_))) => SliceBound::Dynamic,
                    Some(Type::Instance(class, _))
                        if self.slicing_builtin("int").as_ref() == Some(&class) =>
                    {
                        SliceBound::Dynamic
                    }
                    Some(Type::Any | Type::Unknown) => SliceBound::Dynamic,
                    _ => SliceBound::Invalid,
                }
            }
        }
    }

    fn sliced_value_result(
        &self,
        base: &Type,
        key: &Type,
        bounds: [SliceBound; 3],
        budget: &mut usize,
        depth: usize,
    ) -> Type {
        if *budget == 0 || depth >= 16 {
            *budget = 0;
            return Type::Unknown;
        }
        *budget -= 1;
        if let Type::Instance(class, _) = base {
            if ["list", "str", "bytes", "bytearray", "range", "tuple"]
                .iter()
                .any(|name| self.slicing_builtin(name).as_ref() == Some(class))
                && (bounds
                    .iter()
                    .any(|bound| matches!(bound, SliceBound::Invalid))
                    || matches!(bounds[2], SliceBound::Known(0)))
            {
                return Type::Unknown;
            }
        }
        if matches!(base, Type::LiteralString | Type::LiteralStringValue) {
            return if bounds
                .iter()
                .any(|bound| matches!(bound, SliceBound::Invalid))
                || matches!(bounds[2], SliceBound::Known(0))
            {
                Type::Unknown
            } else {
                Type::LiteralStringValue
            };
        }
        match base {
            Type::Tuple(elements) => {
                if elements.len() > 256 {
                    *budget = 0;
                    return Type::Unknown;
                }
                if !elements
                    .iter()
                    .all(|element| charge_slice_type(element, budget, 0))
                {
                    return Type::Unknown;
                }
                if bounds
                    .iter()
                    .any(|bound| matches!(bound, SliceBound::Invalid))
                    || matches!(bounds[2], SliceBound::Known(0))
                {
                    return Type::Unknown;
                }
                if bounds
                    .iter()
                    .any(|bound| matches!(bound, SliceBound::Dynamic))
                {
                    if !elements
                        .iter()
                        .all(|element| charge_slice_type(element, budget, 0))
                    {
                        return Type::Unknown;
                    }
                    return self
                        .slicing_builtin("tuple")
                        .map(|class| Type::Instance(class, vec![Type::union(elements.clone())]))
                        .unwrap_or(Type::Unknown);
                }
                let n = elements.len() as i128;
                let step = match bounds[2] {
                    SliceBound::Known(value) => i128::from(value),
                    _ => 1,
                };
                let bound = |value: SliceBound, default: i128| match value {
                    SliceBound::Known(value) => {
                        let mut value = i128::from(value);
                        if value < 0 {
                            value += n;
                        }
                        if step < 0 {
                            value.clamp(-1, n - 1)
                        } else {
                            value.clamp(0, n)
                        }
                    }
                    _ => default,
                };
                let mut index = bound(bounds[0], if step < 0 { n - 1 } else { 0 });
                let stop = bound(bounds[1], if step < 0 { -1 } else { n });
                let mut result = Vec::new();
                while if step < 0 { index > stop } else { index < stop } {
                    if !charge_slice_type(&elements[index as usize], budget, 0) {
                        return Type::Unknown;
                    }
                    result.push(elements[index as usize].clone());
                    index += step;
                }
                Type::Tuple(result)
            }
            Type::Union(members) if members.len() <= 128 => {
                let mut result = Vec::new();
                for member in members {
                    let value = self.sliced_value_result(member, key, bounds, budget, depth + 1);
                    if *budget == 0 {
                        return Type::Unknown;
                    }
                    result.push(value);
                }
                Type::union(result)
            }
            Type::Literal(_) => {
                self.sliced_value_result(&self.widen(base.clone()), key, bounds, budget, depth + 1)
            }
            Type::Union(_) => {
                *budget = 0;
                Type::Unknown
            }
            Type::Any | Type::Unknown => base.clone(),
            _ => self
                .dunder_call_checked(base, "__getitem__", std::slice::from_ref(key))
                .unwrap_or(Type::Unknown),
        }
    }

    fn multiple_index_result(
        &self,
        base: &Type,
        key: &Type,
        budget: &mut usize,
        depth: usize,
    ) -> Type {
        if *budget == 0 || depth >= 16 {
            *budget = 0;
            return Type::Unknown;
        }
        *budget -= 1;
        match base {
            Type::Tuple(_) => Type::Unknown,
            Type::Union(members) if members.len() <= 128 => {
                let mut result = Vec::new();
                for member in members {
                    let value = self.multiple_index_result(member, key, budget, depth + 1);
                    if *budget == 0 {
                        return Type::Unknown;
                    }
                    result.push(value);
                }
                Type::union(result)
            }
            Type::Literal(_) => {
                self.multiple_index_result(&self.widen(base.clone()), key, budget, depth + 1)
            }
            Type::Union(_) => {
                *budget = 0;
                Type::Unknown
            }
            Type::Any | Type::Unknown => base.clone(),
            _ => self
                .dunder_call_checked(base, "__getitem__", std::slice::from_ref(key))
                .unwrap_or(Type::Unknown),
        }
    }
}

// Charge owned type trees before a local scan/union/copy. Arc-backed callable
// and declared-parameter metadata is shared; owned argument/tuple/receiver
// trees and overloaded Arc lists are charged for their actual copied shape.
fn charge_slice_type(ty: &Type, budget: &mut usize, depth: usize) -> bool {
    if *budget == 0 || depth >= 16 {
        *budget = 0;
        return false;
    }
    *budget -= 1;
    match ty {
        Type::Class(_, values)
        | Type::Instance(_, values)
        | Type::Tuple(values)
        | Type::Union(values)
        | Type::TypePack(values) => values
            .iter()
            .all(|value| charge_slice_type(value, budget, depth + 1)),
        Type::BoundMethod(_, receiver) | Type::Unpack(receiver) | Type::Super { receiver, .. } => {
            charge_slice_type(receiver, budget, depth + 1)
        }
        Type::Concatenate(values, _) => values
            .iter()
            .all(|value| charge_slice_type(value, budget, depth + 1)),
        Type::ParameterList(parameters) => parameters
            .iter()
            .all(|parameter| charge_slice_type(&parameter.ty, budget, depth + 1)),
        Type::Overloaded(signatures) => {
            if signatures.len() > *budget {
                *budget = 0;
                false
            } else {
                *budget -= signatures.len();
                true
            }
        }
        _ => true,
    }
}
