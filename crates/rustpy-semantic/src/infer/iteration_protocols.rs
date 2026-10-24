//! Iteration element types: canonical collections by class identity, then the
//! `__iter__`/`__next__` protocol, then the old-style `__getitem__` sequence
//! protocol.
use super::*;

/// Canonical classes whose first type argument is the iterated element.
const FIRST_ARGUMENT_ELEMENT: &[&str] = &[
    "list",
    "set",
    "frozenset",
    "deque",
    "Iterator",
    "Iterable",
    "Sequence",
    "MutableSequence",
    "Collection",
    "AbstractSet",
    "KeysView",
    "ValuesView",
];

/// Canonical mappings: iteration yields their keys.
const MAPPINGS: &[&str] = &[
    "dict",
    "Mapping",
    "MutableMapping",
    "defaultdict",
    "OrderedDict",
];

impl Inferer<'_> {
    /// The class bundled typeshed defines under `c.name` in builtins, typing or
    /// collections: identity, so a user class named `list` is never misread.
    fn canonical_collection_class(&self, class: &ClassRef) -> bool {
        let Some(file) = self.db.file(class.file) else {
            return false;
        };
        let Some(module) = file.module_name.as_deref() else {
            return false;
        };
        if !file.is_stub || !file.path.starts_with("<typeshed>") {
            return false;
        }
        let canonical = match module {
            "builtins" => self.builtin_class(&class.name),
            "typing" | "collections" | "_collections_abc" | "typing_extensions" => {
                self.module_class(module, &class.name)
            }
            _ => None,
        };
        canonical.as_ref() == Some(class)
    }

    /// Element type of a canonical builtin/typing collection instance.
    pub(super) fn canonical_collection_iter(
        &self,
        class: &ClassRef,
        args: &[Type],
    ) -> Option<Type> {
        let name = class.name.as_str();
        let shaped = match name {
            "str" => args.is_empty(),
            "range" => args.is_empty(),
            "tuple" => args.len() > 1,
            "Generator" => !args.is_empty(),
            _ if FIRST_ARGUMENT_ELEMENT.contains(&name) => args.len() == 1,
            _ if MAPPINGS.contains(&name) => args.len() == 2,
            _ => false,
        };
        if !shaped || !self.canonical_collection_class(class) {
            return None;
        }
        Some(match name {
            "str" => self.instance_of("str"),
            "range" => self.instance_of("int"),
            "tuple" => Type::union(args.to_vec()),
            _ => args[0].clone(),
        })
    }

    /// `for x in value` through `__iter__` (then `__next__`), or failing that
    /// the legacy sequence protocol `__getitem__(int)`.
    pub(super) fn protocol_iter_type(&self, value: &Type) -> Type {
        let Some(iterator) = self.dunder_call(value, "__iter__", &[]) else {
            return self
                .dunder_call_checked(value, "__getitem__", &[self.instance_of("int")])
                .unwrap_or(Type::Unknown);
        };
        if iterator.is_unknown() {
            return Type::Unknown;
        }
        if let Type::Instance(class, args) = &iterator {
            if class.name == "Iterator" && args.len() == 1 && self.canonical_collection_class(class)
            {
                return args[0].clone();
            }
        }
        self.dunder_call(&iterator, "__next__", &[])
            .unwrap_or(Type::Unknown)
    }
}
