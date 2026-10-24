//! Bounded match value/capture projection over the subject acquired before guards.
use super::*;
use crate::classes::MemberKind;
use crate::infer::SynthKind;

/// A memoized source-stability scan: (file, name, owned declaration, stable).
type SourceScan = (usize, Box<str>, Option<Option<TextRange>>, bool);

struct MatchProof {
    steps: usize,
    type_nodes: usize,
    type_bytes: usize,
    sources: Vec<Arc<FileInfo>>,
    source_bytes: usize,
    source_scans: usize,
    /// Source-stability scans already performed, by (file, name, owned declaration):
    /// the files are immutable snapshots, so repeating a scan (once per union arm,
    /// per capture or per authority pass) never re-charges `source_scans`.
    scanned: Vec<SourceScan>,
    /// Classes whose match authority was already proved for a caller file.
    authorities: Vec<(usize, crate::types::ClassRef)>,
}
impl MatchProof {
    fn new() -> Self {
        Self {
            steps: 512,
            type_nodes: 2048,
            type_bytes: 16384,
            sources: Vec::new(),
            source_bytes: 0,
            source_scans: 32,
            scanned: Vec::new(),
            authorities: Vec::new(),
        }
    }
    fn step(&mut self, depth: usize) -> Option<()> {
        if depth >= 16 || self.steps == 0 {
            return None;
        }
        self.steps -= 1;
        Some(())
    }
    fn type_charge(&mut self, amount: usize) -> Option<()> {
        self.type_nodes = self.type_nodes.checked_sub(amount)?;
        Some(())
    }
    fn type_text(&mut self, text: &str) -> Option<()> {
        self.type_bytes = self.type_bytes.checked_sub(text.len())?;
        Some(())
    }
    fn type_width(&mut self, count: usize, limit: usize) -> Option<()> {
        if count > limit {
            return None;
        }
        self.type_charge(count)
    }
    fn params(&mut self, params: &[crate::types::Param], depth: usize) -> Option<()> {
        self.type_width(params.len(), 32)?;
        for parameter in params {
            self.type_text(&parameter.name)?;
            if let Some(default) = &parameter.default_text {
                self.type_text(default)?;
            }
            self.admit_type(&parameter.ty, depth + 1)?;
        }
        Some(())
    }
    fn signature(&mut self, signature: &crate::types::Signature, depth: usize) -> Option<()> {
        if depth >= 16 {
            return None;
        }
        self.type_charge(1)?;
        self.params(&signature.params, depth)?;
        self.admit_type(&signature.ret, depth + 1)?;
        if let Some(definition) = &signature.def {
            self.type_text(&definition.name)?;
        }
        if let Some(docstring) = &signature.docstring {
            self.type_text(docstring)?;
        }
        Some(())
    }
    // Exhaustive borrowed traversal before EVERY new recursive copy/equality
    // join. Shared counters charge repeated copies, not just first admission.
    fn admit_type(&mut self, ty: &Type, depth: usize) -> Option<()> {
        if depth >= 16 {
            return None;
        }
        self.type_charge(1)?;
        match ty {
            Type::Class(class, items) | Type::Instance(class, items) => {
                self.type_text(&class.name)?;
                self.type_width(items.len(), 32)?;
                for item in items {
                    self.admit_type(item, depth + 1)?;
                }
            }
            Type::Tuple(items) | Type::TypePack(items) => {
                self.type_width(items.len(), 256)?;
                for item in items {
                    self.admit_type(item, depth + 1)?;
                }
            }
            Type::Union(items) => {
                self.type_width(items.len(), 32)?;
                for item in items {
                    self.admit_type(item, depth + 1)?;
                }
            }
            Type::Function(signature) | Type::Callable(signature) => {
                self.signature(signature, depth + 1)?
            }
            Type::BoundMethod(signature, receiver) => {
                self.signature(signature, depth + 1)?;
                self.admit_type(receiver, depth + 1)?;
            }
            Type::Overloaded(signatures) => {
                self.type_width(signatures.len(), 32)?;
                for signature in signatures {
                    self.signature(signature, depth + 1)?;
                }
            }
            Type::DeclaredTypeVar(definition) => {
                self.type_text(&definition.key)?;
                self.type_text(&definition.name)?;
                self.type_width(definition.constraints.len(), 32)?;
                for constraint in &definition.constraints {
                    self.admit_type(constraint, depth + 1)?;
                }
                if let Some(bound) = &definition.bound {
                    self.admit_type(bound, depth + 1)?;
                }
                if let Some(default) = &definition.default {
                    self.admit_type(default, depth + 1)?;
                }
            }
            Type::Unpack(value) => self.admit_type(value, depth + 1)?,
            Type::ParameterList(params) => self.params(params, depth)?,
            Type::Concatenate(items, specification) => {
                self.type_text(&specification.key)?;
                self.type_text(&specification.name)?;
                self.type_width(items.len(), 32)?;
                for item in items {
                    self.admit_type(item, depth + 1)?;
                }
            }
            Type::Super { after, receiver } => {
                self.type_text(&after.name)?;
                self.admit_type(receiver, depth + 1)?;
            }
            Type::Literal(LiteralValue::Str(text) | LiteralValue::Bytes(text))
            | Type::TypeVar(text) => self.type_text(text)?,
            Type::Literal(LiteralValue::EnumMember { class, member }) => {
                self.type_text(&class.name)?;
                self.type_text(member)?;
            }
            Type::ParamSpec(definition) => {
                self.type_text(&definition.key)?;
                self.type_text(&definition.name)?;
            }
            Type::TypeVarTuple(definition) => {
                self.type_text(&definition.key)?;
                self.type_text(&definition.name)?;
            }
            Type::SelfType(class) => self.type_text(&class.name)?,
            Type::LiteralString
            | Type::LiteralStringValue
            | Type::Unknown
            | Type::Any
            | Type::Never
            | Type::None
            | Type::Module(_)
            | Type::Literal(LiteralValue::Int(_) | LiteralValue::Bool(_)) => {}
        }
        Some(())
    }
    fn copy(&mut self, ty: &Type) -> Option<Type> {
        self.admit_type(ty, 0)?;
        Some(ty.clone())
    }
    fn copy_types(&mut self, values: &[Type]) -> Option<Vec<Type>> {
        self.type_width(values.len(), 256)?;
        values.iter().map(|value| self.copy(value)).collect()
    }
    fn members(&mut self, ty: &Type) -> Option<Vec<Type>> {
        match ty {
            Type::Union(values) => self.copy_types(values),
            _ => Some(vec![self.copy(ty)?]),
        }
    }
    fn join(&mut self, values: Vec<Type>) -> Option<Type> {
        self.type_width(values.len(), 256)?;
        let mut flattened = 0usize;
        for value in &values {
            self.admit_type(value, 0)?;
            flattened = flattened.checked_add(match value {
                Type::Union(values) => values.len(),
                _ => 1,
            })?;
            if flattened > 256 {
                return None;
            }
        }
        Some(union_live(values))
    }
    fn class_work(&mut self, value: &Type, target: &Type) -> Option<()> {
        // Reserve the maintained class filter's members/to-instance/output
        // copies and its possible per-member target copies before invoking it.
        for _ in 0..4 {
            self.admit_type(value, 0)?;
        }
        let count = match value {
            Type::Union(values) => values.len(),
            _ => 1,
        };
        for _ in 0..=count {
            self.admit_type(target, 0)?;
        }
        Some(())
    }
    fn source(&mut self, inf: &Inferer<'_>, source: &Arc<FileInfo>) -> Option<()> {
        let virtual_builtin = source.is_stub
            && source.module_name.as_deref() == Some("builtins")
            && source.path == std::path::Path::new("<typeshed>/builtins.pyi");
        if virtual_builtin {
            if !inf
                .db
                .module("builtins")
                .is_some_and(|owner| Arc::ptr_eq(&owner, source))
            {
                return None;
            }
        } else if !source.parse.errors().is_empty() || source.scopes.bindings.len() > 4096 {
            return None;
        }
        let current = inf.db.file(source.id)?;
        if !Arc::ptr_eq(source, &current) {
            return None;
        }
        if self.sources.iter().any(|old| Arc::ptr_eq(old, source)) {
            return Some(());
        }
        if self.sources.len() >= 32 || source.text.len() > 1024 * 1024 {
            return None;
        }
        self.source_bytes = self.source_bytes.checked_add(source.text.len())?;
        if self.source_bytes > 4 * 1024 * 1024 {
            return None;
        }
        self.sources.push(source.clone());
        Some(())
    }
    fn stable_member(&mut self, file: &FileInfo, name: &str) -> bool {
        self.scan(file, name, None, || {
            crate::match_support::member_source_stable(file, name)
        })
    }
    fn stable_owned_member(
        &mut self,
        file: &FileInfo,
        name: &str,
        declaration: Option<TextRange>,
    ) -> bool {
        self.scan(file, name, Some(declaration), || {
            crate::match_support::member_source_stable_except(file, name, declaration)
        })
    }
    fn scan(
        &mut self,
        file: &FileInfo,
        name: &str,
        owned: Option<Option<TextRange>>,
        check: impl FnOnce() -> bool,
    ) -> bool {
        let key = std::ptr::from_ref(file) as usize;
        if let Some(&(_, _, _, stable)) =
            self.scanned.iter().find(|(file, scanned, declaration, _)| {
                *file == key && &**scanned == name && *declaration == owned
            })
        {
            return stable;
        }
        if self.source_scans == 0 {
            return false;
        }
        self.source_scans -= 1;
        let stable = check();
        self.scanned.push((key, name.into(), owned, stable));
        stable
    }
    fn current(&self, inf: &Inferer<'_>) -> bool {
        self.sources.iter().all(|source| {
            inf.db
                .file(source.id)
                .is_some_and(|current| Arc::ptr_eq(source, &current))
        })
    }
}

impl Inferer<'_> {
    pub(crate) fn narrow_match_predicate(
        &self,
        file: &Arc<FileInfo>,
        ty: &Type,
        pattern: &SyntaxNode,
        positive: bool,
        name: &str,
    ) -> Type {
        let mut proof = MatchProof::new();
        if proof.admit_type(ty, 0).is_none() {
            return Type::Unknown;
        }
        let Some(case) = pattern.ancestors().take(32).find_map(ast::CaseClause::cast) else {
            return proof.copy(ty).unwrap_or(Type::Unknown);
        };
        let Some(statement) = case
            .syntax()
            .ancestors()
            .take(32)
            .find_map(ast::MatchStmt::cast)
        else {
            return proof.copy(ty).unwrap_or(Type::Unknown);
        };
        // A name or a member reference (`match self.state:`), by reference key.
        let Some(subject) = statement.subject() else {
            return proof.copy(ty).unwrap_or(Type::Unknown);
        };
        if crate::flow::reference_key(&subject).as_deref() != Some(name) {
            return proof.copy(ty).unwrap_or(Type::Unknown);
        }
        if proof.source(self, file).is_none()
            || proof.admit_type(ty, 0).is_none()
            || self.match_type_sources(ty, &mut proof, 0).is_none()
        {
            return proof.copy(ty).unwrap_or(Type::Unknown);
        }
        let result = self
            .match_value(file, ty, pattern, positive, &mut proof, 0)
            .filter(|result| proof.admit_type(result, 0).is_some());
        if proof.current(self) {
            result.unwrap_or_else(|| proof.copy(ty).unwrap_or(Type::Unknown))
        } else {
            proof.copy(ty).unwrap_or(Type::Unknown)
        }
    }
    fn match_type_sources(&self, ty: &Type, proof: &mut MatchProof, depth: usize) -> Option<()> {
        proof.step(depth)?;
        match ty {
            Type::Literal(LiteralValue::EnumMember { class, .. }) => {
                self.match_enum_sources(class, proof)?
            }
            Type::Class(class, args) | Type::Instance(class, args) => {
                let source = self.db.file(class.file)?;
                proof.source(self, &source)?;
                if self.class_info(class)?.class_ref != *class {
                    return None;
                }
                for argument in args {
                    self.match_type_sources(argument, proof, depth + 1)?;
                }
            }
            Type::Union(items) | Type::Tuple(items) => {
                for item in items {
                    self.match_type_sources(item, proof, depth + 1)?;
                }
            }
            Type::Function(signature)
            | Type::Callable(signature)
            | Type::BoundMethod(signature, _) => {
                if let Some(definition) = &signature.def {
                    proof.source(self, &self.db.file(definition.file)?)?;
                }
                for parameter in &signature.params {
                    self.match_type_sources(&parameter.ty, proof, depth + 1)?;
                }
                self.match_type_sources(&signature.ret, proof, depth + 1)?;
            }
            _ => {}
        }
        Some(())
    }
    fn match_value(
        &self,
        file: &Arc<FileInfo>,
        ty: &Type,
        pattern: &SyntaxNode,
        positive: bool,
        proof: &mut MatchProof,
        depth: usize,
    ) -> Option<Type> {
        proof.step(depth)?;
        if pattern.text_range().len() > rustpy_core::TextSize::new(4096) {
            return None;
        }
        let children = || crate::match_support::pattern_children(pattern, 64);
        if let Type::Union(arms) = ty {
            if arms.len() > 32 {
                return None;
            }
            let mut values = Vec::new();
            for arm in arms {
                values.push(self.match_value(file, arm, pattern, positive, proof, depth + 1)?);
            }
            return proof.join(values);
        }
        match pattern.kind() {
            SyntaxKind::MATCH_AS_PATTERN | SyntaxKind::MATCH_GROUP_PATTERN => self.match_value(
                file,
                ty,
                &children()?.into_iter().next()?,
                positive,
                proof,
                depth + 1,
            ),
            SyntaxKind::MATCH_OR_PATTERN => {
                let alternatives = children()?;
                if alternatives.is_empty() || alternatives.len() > 32 {
                    return None;
                }
                if positive {
                    let mut values = Vec::new();
                    for alternative in alternatives {
                        values.push(self.match_value(
                            file,
                            ty,
                            &alternative,
                            true,
                            proof,
                            depth + 1,
                        )?);
                    }
                    proof.join(values)
                } else {
                    let mut value = proof.copy(ty)?;
                    for alternative in alternatives {
                        value =
                            self.match_value(file, &value, &alternative, false, proof, depth + 1)?;
                    }
                    Some(value)
                }
            }
            SyntaxKind::MATCH_WILDCARD_PATTERN | SyntaxKind::MATCH_CAPTURE_PATTERN => {
                Some(if positive {
                    proof.copy(ty)?
                } else {
                    Type::Never
                })
            }
            SyntaxKind::MATCH_VALUE_PATTERN | SyntaxKind::MATCH_SINGLETON_PATTERN => {
                let target = self.match_atom(file, pattern, proof)?;
                if matches!(target, Type::None)
                    && pattern.kind() == SyntaxKind::MATCH_SINGLETON_PATTERN
                {
                    return Some(if positive {
                        if Self::may_include_none(ty, 0) {
                            Type::None
                        } else {
                            Type::Never
                        }
                    } else if matches!(ty, Type::None) {
                        Type::Never
                    } else {
                        proof.copy(ty)?
                    });
                }
                let mut domain = Vec::new();
                for member in proof.members(ty)? {
                    if let Type::Instance(class, _) = &member {
                        self.match_enum_sources(class, proof)?;
                        if let Some(values) = self.enum_member_values(class) {
                            if values.len() > 128 {
                                return None;
                            }
                            for value in &values {
                                proof.admit_type(value, 0)?;
                            }
                            if domain.len().checked_add(values.len())? > 256 {
                                return None;
                            }
                            domain.extend(values);
                            continue;
                        }
                    }
                    domain.push(member);
                }
                let filtered = domain
                    .into_iter()
                    .filter(|member| {
                        let equal = match (member, &target) {
                            (Type::Literal(left), Type::Literal(right)) => {
                                if matches!(left, LiteralValue::EnumMember { .. })
                                    || matches!(right, LiteralValue::EnumMember { .. })
                                {
                                    self.enum_pattern_equal(
                                        left,
                                        right,
                                        pattern.kind() == SyntaxKind::MATCH_SINGLETON_PATTERN,
                                    )
                                } else {
                                    Some(crate::match_support::literal_pattern_equal(
                                        left,
                                        right,
                                        pattern.kind() == SyntaxKind::MATCH_SINGLETON_PATTERN,
                                    ))
                                }
                            }
                            (Type::None, Type::None) => Some(true),
                            (Type::Literal(_), Type::None) | (Type::None, Type::Literal(_)) => {
                                Some(false)
                            }
                            _ => None,
                        };
                        equal.is_none_or(|equal| equal == positive)
                    })
                    .collect();
                proof.join(filtered)
            }
            SyntaxKind::MATCH_CLASS_PATTERN => {
                let target = self.match_class(file, pattern, proof)?;
                if !positive
                    && pattern.children().any(|child| {
                        matches!(
                            child.kind(),
                            SyntaxKind::MATCH_CLASS_ARG | SyntaxKind::MATCH_KEYWORD_PATTERN
                        )
                    })
                {
                    return None;
                }
                proof.class_work(ty, &target)?;
                Some(self.narrow_to_classes(ty, &[target], positive, false))
            }
            SyntaxKind::MATCH_SEQUENCE_PATTERN => {
                let items = children()?;
                let stars: Vec<_> = items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.kind() == SyntaxKind::MATCH_STAR_PATTERN)
                    .map(|(index, _)| index)
                    .collect();
                if stars.len() > 1 {
                    return None;
                }
                let star = stars.first().copied();
                let minimum = items.len() - stars.len();
                if let Type::Tuple(values) = ty {
                    if values.len() < minimum || (star.is_none() && values.len() != minimum) {
                        return Some(if positive {
                            Type::Never
                        } else {
                            proof.copy(ty)?
                        });
                    }
                    let mut refined = proof.copy_types(values)?;
                    let mut covers = true;
                    for (position, item) in items.iter().enumerate() {
                        if star == Some(position) {
                            continue;
                        }
                        let index = if star.is_some_and(|star| position > star) {
                            values.len() - (items.len() - position)
                        } else {
                            position
                        };
                        let value = values.get(index)?;
                        let matched =
                            self.match_value(file, value, item, true, proof, depth + 1)?;
                        if matches!(matched, Type::Never) {
                            return Some(if positive {
                                Type::Never
                            } else {
                                proof.copy(ty)?
                            });
                        }
                        let unmatched =
                            self.match_value(file, value, item, false, proof, depth + 1)?;
                        covers &= matches!(unmatched, Type::Never);
                        refined[index] = matched;
                    }
                    Some(if positive {
                        Type::Tuple(refined)
                    } else if covers {
                        Type::Never
                    } else {
                        proof.copy(ty)?
                    })
                } else if self.match_builtin_container(ty, "list").is_some()
                    || self.match_builtin_container(ty, "tuple").is_some()
                {
                    Some(if !positive && items.len() == 1 && star == Some(0) {
                        Type::Never
                    } else {
                        proof.copy(ty)?
                    })
                } else {
                    None
                }
            }
            SyntaxKind::MATCH_MAPPING_PATTERN => {
                let items: Vec<_> = pattern
                    .children()
                    .filter(|child| child.kind() == SyntaxKind::MATCH_MAPPING_ITEM)
                    .take(65)
                    .collect();
                if items.len() > 64 {
                    return None;
                }
                for item in items {
                    let value = self.match_mapping_item(file, ty, &item, proof)?;
                    let child = crate::match_support::pattern_children(&item, 2)?
                        .into_iter()
                        .next()?;
                    let narrowed =
                        self.match_value(file, &value, &child, true, proof, depth + 1)?;
                    if matches!(narrowed, Type::Never) {
                        return Some(if positive {
                            Type::Never
                        } else {
                            proof.copy(ty)?
                        });
                    }
                }
                // Required-key/optional-key occupancy is not inferred by this
                // value projector; no mapping exhaustiveness is fabricated.
                Some(proof.copy(ty)?)
            }
            _ => None,
        }
    }
    fn match_atom(
        &self,
        file: &Arc<FileInfo>,
        pattern: &SyntaxNode,
        proof: &mut MatchProof,
    ) -> Option<Type> {
        proof.step(0)?;
        if let Some(atom) = crate::match_support::pattern_atom(pattern) {
            return Some(match atom {
                crate::match_support::FinitePatternValue::None => Type::None,
                crate::match_support::FinitePatternValue::Literal(value) => Type::Literal(value),
            });
        }
        // Float/complex literals have no singleton Type representation. Keep
        // the acquired subject's type (and therefore conservative residuals)
        // instead of losing an otherwise known AS capture.
        if pattern.kind() == SyntaxKind::MATCH_VALUE_PATTERN {
            let numbers: Vec<_> = pattern
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| !token.kind().is_trivia())
                .take(8)
                .collect();
            if !numbers.is_empty()
                && numbers.len() <= 7
                && numbers.iter().all(|token| {
                    matches!(
                        token.kind(),
                        SyntaxKind::INT_NUMBER
                            | SyntaxKind::FLOAT_NUMBER
                            | SyntaxKind::COMPLEX_NUMBER
                            | SyntaxKind::PLUS
                            | SyntaxKind::MINUS
                    )
                })
            {
                if numbers
                    .iter()
                    .any(|token| token.kind() == SyntaxKind::COMPLEX_NUMBER)
                {
                    return Some(self.instance_of("complex"));
                }
                if numbers
                    .iter()
                    .any(|token| token.kind() == SyntaxKind::FLOAT_NUMBER)
                {
                    return Some(self.instance_of("float"));
                }
            }
        }
        let names = crate::match_support::dotted_pattern_names(pattern)?;
        if names.len() < 2 {
            return None;
        }
        let scope = self.scope_of(file, pattern);
        let ids = file.scopes.resolve(scope, &names[0])?;
        if ids.len() != 1
            || !matches!(
                file.scopes.binding(ids[0]).kind,
                crate::scope::BindingKind::Class | crate::scope::BindingKind::Import(_)
            )
        {
            return None;
        }
        let definition = self.resolve_name(file, scope, &names[0])?;
        proof.source(self, &definition.file)?;
        let mut owner = self.type_of_binding(&definition.file, definition.binding);
        for name in &names[1..names.len() - 1] {
            proof.step(0)?;
            let source = match &owner {
                Type::Module(id) => self.db.file(*id)?,
                Type::Class(class, _) => self.db.file(class.file)?,
                _ => return None,
            };
            proof.source(self, &source)?;
            if !proof.stable_member(file, name) || !proof.stable_member(&source, name) {
                return None;
            }
            owner = self.lookup_member(&owner, name)?;
        }
        let Type::Class(class, _) = owner else {
            return None;
        };
        self.match_enum_sources(&class, proof)?;
        let name = names.last()?;
        if !proof.stable_member(file, name) {
            return None;
        }
        self.enum_member_value(&class, name)
    }
    fn match_enum_sources(
        &self,
        class: &crate::types::ClassRef,
        proof: &mut MatchProof,
    ) -> Option<()> {
        let owners = self.mro(class);
        if owners.len() > 32 {
            return None;
        }
        for owner in owners.iter() {
            proof.step(0)?;
            proof.source(self, &self.db.file(owner.file)?)?;
        }
        Some(())
    }
    fn match_class(
        &self,
        file: &Arc<FileInfo>,
        pattern: &SyntaxNode,
        proof: &mut MatchProof,
    ) -> Option<Type> {
        let tokens: Vec<_> = pattern
            .children_with_tokens()
            .filter_map(|item| item.into_token())
            .filter(|token| token.kind() == SyntaxKind::IDENT || token.kind() == SyntaxKind::DOT)
            .take(34)
            .collect();
        if tokens.is_empty() || tokens.len() > 17 || tokens.len().is_multiple_of(2) {
            return None;
        }
        for (index, token) in tokens.iter().enumerate() {
            if token.text().len() > 256
                || token.kind()
                    != if index.is_multiple_of(2) {
                        SyntaxKind::IDENT
                    } else {
                        SyntaxKind::DOT
                    }
            {
                return None;
            }
        }
        let name = tokens[0].text();
        if !proof.stable_member(file, name) {
            return None;
        }
        let scope = self.scope_of(file, pattern);
        if let Some(bindings) = file.scopes.resolve(scope, name) {
            if bindings.len() != 1
                || !matches!(
                    file.scopes.binding(bindings[0]).kind,
                    crate::scope::BindingKind::Class | crate::scope::BindingKind::Import(_)
                )
            {
                return None;
            }
        }
        let definition = self.resolve_name(file, scope, name)?;
        proof.source(self, &definition.file)?;
        let mut value = self.type_of_binding(&definition.file, definition.binding);
        for token in tokens.iter().skip(2).step_by(2) {
            proof.step(0)?;
            let owner = match &value {
                Type::Module(id) => self.db.file(*id)?,
                Type::Class(class, _) => self.db.file(class.file)?,
                _ => return None,
            };
            proof.source(self, &owner)?;
            if !proof.stable_member(file, token.text())
                || !proof.stable_member(&owner, token.text())
            {
                return None;
            }
            value = self.lookup_member(&value, token.text())?;
            proof.admit_type(&value, 0)?;
        }
        let Type::Class(class, args) = value else {
            return None;
        };
        self.match_class_authority(file, &class, proof)?;
        Some(Type::Instance(class, args))
    }
    fn match_class_authority(
        &self,
        caller: &Arc<FileInfo>,
        class: &crate::types::ClassRef,
        proof: &mut MatchProof,
    ) -> Option<()> {
        let key = std::ptr::from_ref::<FileInfo>(caller) as usize;
        if proof
            .authorities
            .iter()
            .any(|(file, proved)| *file == key && proved == class)
        {
            return Some(());
        }
        let owners = self.mro(class);
        if owners.is_empty() || owners.len() > 32 {
            return None;
        }
        for owner in owners.iter() {
            proof.step(0)?;
            let source = self.db.file(owner.file)?;
            proof.source(self, &source)?;
            let info = self.class_info(owner)?;
            if info.class_ref != *owner || !proof.stable_member(caller, &owner.name) {
                return None;
            }
            if source.is_stub
                && source.module_name.as_deref() == Some("builtins")
                && source.path == std::path::Path::new("<typeshed>/builtins.pyi")
            {
                continue;
            }
            if source.is_stub
                && source.path == std::path::Path::new("<typeshed>/typing.pyi")
                && owner.name == "NamedTuple"
                && self.is_class_namedtuple(class)
            {
                // The special form behind a class-form NamedTuple; the rest of
                // the MRO is typeshed's `tuple`.
                break;
            }
            if !proof.stable_member(&source, &owner.name) {
                return None;
            }
            if !(info.decorators.is_empty() || self.synthesized_match_authority(owner, &info))
                || !info.keywords.is_empty()
                || info.members.contains_key("__getattribute__")
                || info.members.contains_key("__getattr__")
                || info.members.contains_key("__setattr__")
                || info.members.contains_key("__delattr__")
            {
                return None;
            }
            for base in info.base_exprs.iter().take(33) {
                if info.base_exprs.len() > 32 {
                    return None;
                }
                let expression = expr_at(&source, *base)?;
                if !matches!(expression, ast::Expr::Name(_) | ast::Expr::Attribute(_)) {
                    return None;
                }
                let Type::Class(parent, _) = self.type_of_expr(&source, &expression) else {
                    return None;
                };
                if !owners.iter().any(|candidate| candidate == &parent) {
                    return None;
                }
            }
            if !proof.stable_member(caller, &owner.name)
                || !proof.stable_member(&source, &owner.name)
            {
                return None;
            }
        }
        proof.authorities.push((key, class.clone()));
        Some(())
    }
    fn match_builtin_container<'a>(&self, ty: &'a Type, name: &str) -> Option<&'a [Type]> {
        let Type::Instance(class, args) = ty else {
            return None;
        };
        if class.name != name {
            return None;
        }
        let owner = self.db.file(class.file)?;
        (owner.is_stub && owner.path == std::path::Path::new("<typeshed>/builtins.pyi"))
            .then_some(args.as_slice())
    }
    fn match_list(&self, element: Type) -> Option<Type> {
        Some(Type::Instance(self.builtin_class("list")?, vec![element]))
    }
    pub(crate) fn match_capture_type(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
    ) -> Type {
        let Some(pattern) = crate::match_support::pattern_at(file, binding.node_range) else {
            return Type::Unknown;
        };
        let Some(case) = pattern.ancestors().take(32).find_map(ast::CaseClause::cast) else {
            return Type::Unknown;
        };
        let Some(top) = case.pattern() else {
            return Type::Unknown;
        };
        let Some(statement) = case
            .syntax()
            .ancestors()
            .take(32)
            .find_map(ast::MatchStmt::cast)
        else {
            return Type::Unknown;
        };
        let Some(subject) = statement.subject() else {
            return Type::Unknown;
        };
        // A name or a member/subscript reference (`match self.node:`).
        if crate::flow::reference_key(&subject).is_none() {
            return Type::Unknown;
        }
        let mut proof = MatchProof::new();
        if proof.source(self, file).is_none() {
            return Type::Unknown;
        }
        let ty = self.type_of_expr(file, &subject);
        if proof.admit_type(&ty, 0).is_none()
            || self.match_type_sources(&ty, &mut proof, 0).is_none()
        {
            return Type::Unknown;
        }
        let result = self
            .match_capture(file, &ty, &top, binding.range.start(), &mut proof, 0)
            .filter(|result| proof.admit_type(result, 0).is_some());
        if proof.current(self) {
            result.unwrap_or(Type::Unknown)
        } else {
            Type::Unknown
        }
    }
    fn match_capture(
        &self,
        file: &Arc<FileInfo>,
        ty: &Type,
        pattern: &SyntaxNode,
        target: rustpy_core::TextSize,
        proof: &mut MatchProof,
        depth: usize,
    ) -> Option<Type> {
        proof.step(depth)?;
        if pattern.text_range().len() > rustpy_core::TextSize::new(4096) {
            return None;
        }
        if let Type::Union(arms) = ty {
            if arms.len() > 32 {
                return None;
            }
            let mut captures = Vec::new();
            let mut none_excluded = None;
            for arm in arms {
                if self
                    .match_value(file, arm, pattern, true, proof, depth + 1)
                    .is_some_and(|matched| matches!(matched, Type::Never))
                {
                    continue;
                }
                // A class pattern never matches `None` (in `Foo | None`) unless it
                // names `object`/`NoneType`; that arm contributes no capture.
                // The class is resolved once, on its own budget: the capture
                // below resolves it again on the shared one.
                if matches!(arm, Type::None) {
                    if none_excluded.is_none() {
                        none_excluded = Some(
                            pattern.kind() == SyntaxKind::MATCH_CLASS_PATTERN
                                && self
                                    .match_class(file, pattern, &mut MatchProof::new())
                                    .is_some_and(|expected| {
                                        matches!(&expected, Type::Instance(class, _)
                                            if !matches!(class.name.as_str(), "object" | "NoneType"))
                                    }),
                        );
                    }
                    if none_excluded == Some(true) {
                        continue;
                    }
                }
                let capture = self.match_capture(file, arm, pattern, target, proof, depth + 1)?;
                captures.push(capture);
            }
            return if captures.is_empty() {
                None
            } else {
                proof.join(captures)
            };
        }
        match pattern.kind() {
            SyntaxKind::MATCH_CAPTURE_PATTERN
            | SyntaxKind::MATCH_STAR_PATTERN
            | SyntaxKind::MATCH_MAPPING_REST => {
                (ast::token(pattern, SyntaxKind::IDENT)?.text_range().start() == target)
                    .then_some(())
                    .and_then(|()| proof.copy(ty))
            }
            SyntaxKind::MATCH_AS_PATTERN => {
                let inner = crate::match_support::pattern_children(pattern, 2)?
                    .into_iter()
                    .next()?;
                if pattern
                    .children_with_tokens()
                    .filter_map(|item| item.into_token())
                    .filter(|token| token.kind() == SyntaxKind::IDENT)
                    .last()?
                    .text_range()
                    .start()
                    == target
                {
                    self.match_value(file, ty, &inner, true, proof, depth + 1)
                } else {
                    self.match_capture(file, ty, &inner, target, proof, depth + 1)
                }
            }
            SyntaxKind::MATCH_GROUP_PATTERN => self.match_capture(
                file,
                ty,
                &crate::match_support::pattern_children(pattern, 2)?
                    .into_iter()
                    .next()?,
                target,
                proof,
                depth + 1,
            ),
            SyntaxKind::MATCH_OR_PATTERN => {
                let name = crate::match_support::capture_name_at(pattern, target)?;
                let alternatives = crate::match_support::pattern_children(pattern, 32)?;
                let mut values = Vec::new();
                for alternative in alternatives {
                    let offset = crate::match_support::capture_offset(
                        &alternative,
                        &name,
                        &mut proof.steps,
                    )?;
                    values.push(self.match_capture(
                        file,
                        ty,
                        &alternative,
                        offset,
                        proof,
                        depth + 1,
                    )?);
                }
                if values.is_empty() {
                    None
                } else {
                    proof.join(values)
                }
            }
            SyntaxKind::MATCH_SEQUENCE_PATTERN => {
                let items = crate::match_support::pattern_children(pattern, 64)?;
                let star = items
                    .iter()
                    .position(|item| item.kind() == SyntaxKind::MATCH_STAR_PATTERN);
                if items
                    .iter()
                    .filter(|item| item.kind() == SyntaxKind::MATCH_STAR_PATTERN)
                    .count()
                    > 1
                {
                    return None;
                }
                let selected = items
                    .iter()
                    .position(|item| item.text_range().contains(target))?;
                let item = &items[selected];
                let projected = match ty {
                    Type::Tuple(values) if values.len() <= 256 => {
                        let minimum = items.len() - usize::from(star.is_some());
                        if values.len() < minimum || (star.is_none() && values.len() != minimum) {
                            return None;
                        }
                        if star == Some(selected) {
                            let end = values.len() - (items.len() - selected - 1);
                            self.match_list(if selected == end {
                                Type::Unknown
                            } else {
                                {
                                    let mut copied = Vec::new();
                                    for value in &values[selected..end] {
                                        copied.push(self.widen(proof.copy(value)?));
                                    }
                                    proof.join(copied)?
                                }
                            })?
                        } else {
                            let index = if star.is_some_and(|star| selected > star) {
                                values.len() - (items.len() - selected)
                            } else {
                                selected
                            };
                            proof.copy(values.get(index)?)?
                        }
                    }
                    _ => {
                        let args = self
                            .match_builtin_container(ty, "list")
                            .or_else(|| self.match_builtin_container(ty, "tuple"))?;
                        let element = proof.copy(args.first()?)?;
                        if star == Some(selected) {
                            self.match_list(element)?
                        } else {
                            element
                        }
                    }
                };
                self.match_capture(file, &projected, item, target, proof, depth + 1)
            }
            SyntaxKind::MATCH_MAPPING_PATTERN => {
                let children: Vec<_> = pattern.children().take(65).collect();
                if children.len() > 64 {
                    return None;
                }
                let item = children
                    .iter()
                    .find(|item| item.text_range().contains(target))?;
                if item.kind() == SyntaxKind::MATCH_MAPPING_REST {
                    let value = if self.match_builtin_container(ty, "dict").is_some() {
                        proof.copy(ty)?
                    } else {
                        let mut excluded = Vec::new();
                        for child in &children {
                            if child.kind() == SyntaxKind::MATCH_MAPPING_ITEM {
                                excluded.push(self.match_mapping_key(child)?);
                            }
                        }
                        self.match_mapping_rest(file, ty, &excluded, proof)?
                    };
                    return self.match_capture(file, &value, item, target, proof, depth + 1);
                }
                if item.kind() != SyntaxKind::MATCH_MAPPING_ITEM {
                    return None;
                }
                let value = self.match_mapping_item(file, ty, item, proof)?;
                let child = crate::match_support::pattern_children(item, 2)?
                    .into_iter()
                    .next()?;
                self.match_capture(file, &value, &child, target, proof, depth + 1)
            }
            SyntaxKind::MATCH_CLASS_PATTERN => {
                let expected = self.match_class(file, pattern, proof)?;
                proof.class_work(ty, &expected)?;
                let narrowed =
                    self.narrow_to_classes(ty, std::slice::from_ref(&expected), true, false);
                let Type::Instance(class, _) = &expected else {
                    return None;
                };
                if matches!(narrowed, Type::Never) {
                    return None;
                }
                let args: Vec<_> = pattern
                    .children()
                    .filter(|child| {
                        matches!(
                            child.kind(),
                            SyntaxKind::MATCH_CLASS_ARG | SyntaxKind::MATCH_KEYWORD_PATTERN
                        )
                    })
                    .take(33)
                    .collect();
                if args.len() > 32 {
                    return None;
                }
                let selected = args
                    .iter()
                    .position(|argument| argument.text_range().contains(target))?;
                let argument = &args[selected];
                let child = crate::match_support::pattern_children(argument, 2)?
                    .into_iter()
                    .next()?;
                let source = self.db.file(class.file)?;
                proof.source(self, &source)?;
                let self_match = source.is_stub
                    && source.path == std::path::Path::new("<typeshed>/builtins.pyi")
                    && matches!(
                        class.name.as_str(),
                        "bool"
                            | "bytearray"
                            | "bytes"
                            | "dict"
                            | "float"
                            | "frozenset"
                            | "int"
                            | "list"
                            | "set"
                            | "str"
                            | "tuple"
                    );
                if self_match && argument.kind() == SyntaxKind::MATCH_CLASS_ARG {
                    if args
                        .iter()
                        .filter(|argument| argument.kind() == SyntaxKind::MATCH_CLASS_ARG)
                        .count()
                        != 1
                    {
                        return None;
                    }
                    return self.match_capture(file, &narrowed, &child, target, proof, depth + 1);
                }
                let positional = args
                    .iter()
                    .filter(|argument| argument.kind() == SyntaxKind::MATCH_CLASS_ARG)
                    .count();
                let mut names = Vec::new();
                let mut position = 0;
                for argument in &args {
                    let name = if argument.kind() == SyntaxKind::MATCH_KEYWORD_PATTERN {
                        let token = ast::token(argument, SyntaxKind::IDENT)?;
                        if token.text().len() > 256 {
                            return None;
                        }
                        token.text().to_owned()
                    } else {
                        let name = self.match_positional_name(file, class, position, proof)?;
                        position += 1;
                        name
                    };
                    if names.contains(&name) {
                        return None;
                    }
                    names.push(name);
                }
                if positional > 32 {
                    return None;
                }
                let name = names.get(selected)?;
                let projected = self.match_owned_attribute(file, &narrowed, name, proof)?;
                self.match_capture(file, &projected, &child, target, proof, depth + 1)
            }
            _ => None,
        }
    }
    fn match_positional_name(
        &self,
        caller: &Arc<FileInfo>,
        class: &crate::types::ClassRef,
        index: usize,
        proof: &mut MatchProof,
    ) -> Option<String> {
        for owner in self.mro(class).iter().take(33) {
            proof.step(0)?;
            let info = self.class_info(owner)?;
            let Some(member) = info.members.get("__match_args__") else {
                // dataclass / attrs / class-form NamedTuple generate it.
                if let Some(names) = self.synthesized_match_args(owner) {
                    if names.len() > 32 || !proof.stable_member(caller, "__match_args__") {
                        return None;
                    }
                    return names.get(index).map(|name| name.to_string());
                }
                continue;
            };
            let source = self.db.file(member.file)?;
            proof.source(self, &source)?;
            if !proof.stable_member(caller, "__match_args__")
                || !proof.stable_member(&source, "__match_args__")
            {
                return None;
            }
            let expression = expr_at(&source, member.value?)?;
            let ast::Expr::Tuple(tuple) = expression.unparen() else {
                return None;
            };
            let names: Vec<_> = tuple.elements().take(33).collect();
            if names.len() > 32 {
                return None;
            }
            let mut values = Vec::new();
            for expression in names {
                if expression.text_range().len() > rustpy_core::TextSize::new(1024) {
                    return None;
                }
                let Type::Literal(LiteralValue::Str(name)) =
                    self.type_of_expr(&source, &expression)
                else {
                    return None;
                };
                if name.len() > 256 {
                    return None;
                }
                values.push(name.to_string());
            }
            return values.get(index).cloned();
        }
        None
    }
    fn match_owned_attribute(
        &self,
        caller: &Arc<FileInfo>,
        receiver: &Type,
        name: &str,
        proof: &mut MatchProof,
    ) -> Option<Type> {
        if name.len() > 256 {
            return None;
        }
        let Type::Instance(class, _) = receiver else {
            return None;
        };
        self.match_class_authority(caller, class, proof)?;
        let owners = self.mro(class);
        if owners.len() > 32 {
            return None;
        }
        for owner in owners.iter() {
            proof.step(0)?;
            let info = self.class_info(owner)?;
            let Some(member) = info.members.get(name) else {
                continue;
            };
            if !matches!(member.kind, MemberKind::ClassVar | MemberKind::InstanceVar) {
                return None;
            }
            let source = self.db.file(member.file)?;
            proof.source(self, &source)?;
            let declaration = (member.kind == MemberKind::InstanceVar).then_some(member.range);
            if !proof.stable_owned_member(&source, name, declaration)
                || !proof.stable_owned_member(
                    caller,
                    name,
                    if Arc::ptr_eq(caller, &source) {
                        declaration
                    } else {
                        None
                    },
                )
            {
                return None;
            }
            let value = self.lookup_member(receiver, name)?;
            proof.admit_type(&value, 0)?;
            return Some(value);
        }
        None
    }
    fn match_mapping_item(
        &self,
        caller: &Arc<FileInfo>,
        ty: &Type,
        item: &SyntaxNode,
        proof: &mut MatchProof,
    ) -> Option<Type> {
        // A homogeneous dict's value projection does not depend on the key's
        // spelling. Numeric, bytes and qualified value keys are valid patterns
        // too; only TypedDict field selection requires a known string key.
        if let Some(arguments) = self.match_builtin_container(ty, "dict") {
            if arguments.len() != 2 {
                return None;
            }
            return proof.copy(&arguments[1]);
        }
        let key = self.match_mapping_key(item)?;
        self.match_mapping_field(caller, ty, &key, proof)
    }
    fn match_mapping_key(&self, item: &SyntaxNode) -> Option<String> {
        let expression = item.children().find_map(ast::Expr::cast)?;
        let crate::match_support::FinitePatternValue::Literal(LiteralValue::Str(key)) =
            crate::match_support::expression_atom(&expression)?
        else {
            return None;
        };
        (key.len() <= 256).then(|| key.to_string())
    }
    fn match_mapping_field(
        &self,
        caller: &Arc<FileInfo>,
        ty: &Type,
        key: &str,
        proof: &mut MatchProof,
    ) -> Option<Type> {
        if let Some(arguments) = self.match_builtin_container(ty, "dict") {
            if arguments.len() != 2 {
                return None;
            }
            return proof.copy(&arguments[1]);
        }
        let Type::Instance(class, _) = ty else {
            return None;
        };
        if self.synth_spec(class)?.kind != SynthKind::TypedDict {
            return None;
        }
        let owners = self.mro(class);
        if owners.len() > 32 {
            return None;
        }
        for owner in owners.iter() {
            proof.step(0)?;
            let info = self.class_info(owner)?;
            let source = self.db.file(owner.file)?;
            proof.source(self, &source)?;
            if let Some(member) = info.members.get(key) {
                if !matches!(member.kind, MemberKind::ClassVar | MemberKind::InstanceVar) {
                    return None;
                }
                if !proof.stable_member(caller, key) || !proof.stable_member(&source, key) {
                    return None;
                }
                let value = self.lookup_member(ty, key)?;
                proof.admit_type(&value, 0)?;
                return Some(value);
            }
        }
        None
    }
    fn match_mapping_rest(
        &self,
        caller: &Arc<FileInfo>,
        ty: &Type,
        excluded: &[String],
        proof: &mut MatchProof,
    ) -> Option<Type> {
        let Type::Instance(class, _) = ty else {
            return None;
        };
        if self.synth_spec(class)?.kind != SynthKind::TypedDict {
            return None;
        }
        let owners = self.mro(class);
        if owners.len() > 32 {
            return None;
        }
        let mut values = Vec::new();
        let mut names = Vec::new();
        for owner in owners.iter() {
            proof.step(0)?;
            let source = self.db.file(owner.file)?;
            proof.source(self, &source)?;
            let info = self.class_info(owner)?;
            if !self
                .synth_spec(owner)
                .is_some_and(|spec| spec.kind == SynthKind::TypedDict)
            {
                continue;
            }
            let fields: Vec<_> = info
                .members
                .values()
                .filter(|member| {
                    matches!(member.kind, MemberKind::ClassVar | MemberKind::InstanceVar)
                })
                .take(65)
                .collect();
            if fields.len() > 64 {
                return None;
            }
            for field in fields {
                proof.step(0)?;
                if field.name.len() > 256 || names.len() >= 64 {
                    return None;
                }
                if names.contains(&field.name) {
                    continue;
                }
                proof.type_text(&field.name)?;
                names.push(field.name.clone());
                if excluded.iter().any(|name| name == field.name.as_str()) {
                    continue;
                }
                values.push(self.match_mapping_field(caller, ty, &field.name, proof)?);
            }
        }
        let dictionary = self.builtin_class("dict")?;
        Some(Type::Instance(
            dictionary,
            vec![
                self.instance_of("str"),
                if values.is_empty() {
                    Type::Unknown
                } else {
                    proof.join(values)?
                },
            ],
        ))
    }
}
