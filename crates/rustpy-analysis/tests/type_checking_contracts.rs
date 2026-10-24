//! End-to-end inspection contracts. These deliberately exercise existing public
//! analysis/inference routes, not a parallel checker built for the fixtures.
use rustpy_analysis::inspections::{self, InspectionOptions, Problem};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn inspect(source: &str) -> Vec<Problem> {
    let db = Analysis::for_project(Path::new("/type-checking-item"));
    let id = db.set_file_text(Path::new("/type-checking-item/m.py"), source);
    inspections::inspect(&db, &db.file(id).unwrap(), &InspectionOptions::default())
}
fn has(problems: &[Problem], code: &str, message: &str) -> bool {
    problems
        .iter()
        .any(|p| p.diagnostic.code == code && p.diagnostic.message.contains(message))
}
fn no(problems: &[Problem], code: &str) {
    assert!(
        !problems.iter().any(|p| p.diagnostic.code == code),
        "{problems:?}"
    );
}
#[test]
fn calls_await_iteration_contexts_and_indexing_require_real_protocols() {
    let p = inspect(
        "class Plain: pass\nvalue=Plain()\nvalue()\nvalue[0]\nfor item in value: pass\nwith value: pass\nasync def f():\n await value\n async for item in value: pass\n async with value: pass\nlen(value)\n",
    );
    for part in [
        "not callable",
        "not subscriptable",
        "not iterable",
        "not awaitable",
        "not async iterable",
        "__enter__",
        "__aenter__",
        "has no length",
    ] {
        assert!(has(&p, "PyTypeChecker", part), "{part}: {p:?}");
    }
    let p = inspect(
        "class Good:\n def __call__(self)->int: return 1\n def __iter__(self): yield 1\n def __getitem__(self,index:int)->int: return 1\n def __len__(self)->int: return 1\n def __enter__(self): return self\n def __exit__(self,a,b,c): return False\nvalue=Good()\nvalue()\nvalue[0]\nlen(value)\nfor item in value: pass\nwith value: pass\n",
    );
    assert!(
        !has(&p, "PyTypeChecker", "not callable")
            && !has(&p, "PyTypeChecker", "not iterable")
            && !has(&p, "PyTypeChecker", "not subscriptable"),
        "{p:?}"
    );
}
#[test]
fn implicit_protocol_lookup_bypasses_getattribute_and_preserves_unknowns() {
    let p = inspect(
        "class Plain:\n def __getattribute__(self,name)->str: return 'dynamic'\n def __add__(self,other:int)->int: return 1\nvalue=Plain()\nvalue+1\nvalue()\ndef dynamic(value):\n value()\n return value[0]\n",
    );
    assert!(has(&p, "PyTypeChecker", "not callable"), "{p:?}");
    assert!(!has(&p, "PyTypeChecker", "Operator '+'"), "{p:?}");
}
#[test]
fn binary_comparison_membership_unary_and_index_operand_types_are_checked() {
    let p = inspect(
        "bad=1+'x'\nother=1<'x'\nfound=1 in 2\ninverted=~'text'\nwrong=[1,2]['index']\nvalid=[1,2][-1]\n",
    );
    for part in [
        "Operator '+'",
        "Comparison '<'",
        "Comparison 'in'",
        "Unary operator",
        "Sequence index",
    ] {
        assert!(has(&p, "PyTypeChecker", part), "{part}: {p:?}");
    }
    let p = inspect("value=1+2\nordered=1<2\nfound=1 in [1,2]\ninverted=~1\nvalid=[1,2][0]\n");
    no(&p, "PyTypeChecker");
}
#[test]
fn async_protocol_return_shapes_are_not_replaced_by_method_spelling() {
    let p = inspect(
        "class Reader:\n def __aiter__(self)->'Reader': return self\n def __anext__(self)->int: return 1\nclass Manager:\n def __aenter__(self)->int: return 1\n def __aexit__(self,a,b,c)->int: return 1\nasync def work():\n async for item in Reader(): pass\n async with Manager(): pass\n",
    );
    assert!(
        has(&p, "PyTypeChecker", "__anext__ must return an awaitable"),
        "{p:?}"
    );
    assert!(has(
        &p,
        "PyTypeChecker",
        "__aenter__ must return an awaitable"
    ));
}
#[test]
fn optional_member_and_runtime_generic_checks_keep_narrowed_and_shadow_controls() {
    let p = inspect(
        "def f(value:str|None):\n value.strip()\n if value is not None: value.strip()\nisinstance(1,list[int])\n",
    );
    assert!(has(&p, "PyTypeChecker", "Possible None"), "{p:?}");
    assert!(has(&p, "PyTypeChecker", "Parameterized generics"));
    let p = inspect(
        "def isinstance(value,kind): return True\nisinstance(1,list[int])\ndef f(value:str|None):\n if value is not None: return value.strip()\n",
    );
    assert!(
        !has(&p, "PyTypeChecker", "Parameterized generics")
            && !has(&p, "PyTypeChecker", "Possible None"),
        "{p:?}"
    );
}
#[test]
fn literal_callable_tuple_and_typed_return_values_reach_argument_and_return_consumers() {
    let p = inspect(
        "from typing import Literal,Callable\ndef need(value:Literal['yes']): pass\nneed('no')\ndef text()->str: return 'text'\ndef wrong()->int: return text()\ndef typed(value:tuple[int,str])->int: return value\ndef consume(fn:Callable[[int],str]): pass\ndef wrong_fn(value:int)->int: return 1\nconsume(wrong_fn)\n",
    );
    assert!(has(&p, "PyTypeChecker", "Literal"), "{p:?}");
    assert!(has(&p, "PyTypeChecker", "Expected type"));
    let p = inspect(
        "from typing import Literal\ndef need(value:Literal['yes']): pass\nneed('yes')\ndef text()->str: return 'text'\ndef correct()->str: return text()\n",
    );
    no(&p, "PyTypeChecker");
}
#[test]
fn later_stores_obey_variable_and_member_annotations() {
    let p =
        inspect("count:int=1\ncount='bad'\nclass C:\n field:int=1\nvalue=C()\nvalue.field='bad'\n");
    assert_eq!(
        p.iter()
            .filter(|p| p.diagnostic.code == "PyTypeChecker"
                && p.diagnostic.message.contains("in assignment"))
            .count(),
        2,
        "{p:?}"
    );
    let p = inspect("count:int=1\ncount=2\nclass C:\n field:int=1\nvalue=C()\nvalue.field=2\n");
    no(&p, "PyTypeChecker");
}
#[test]
fn typed_dict_schema_reports_missing_extra_wrong_values_and_unknown_reads() {
    let p = inspect(
        "from typing import TypedDict\nclass Movie(TypedDict):\n title:str\n year:int\ndef need(value:Movie): pass\nneed({'title':'ok','year':'bad','extra':1})\nmissing:Movie={'title':'ok'}\nvalue:Movie={'title':'ok','year':2024}\nvalue['missing']\nMovie(title='ok')\n",
    );
    for part in [
        "unknown key",
        "year",
        "missing required key",
        "has no key",
        "Missing required TypedDict",
    ] {
        assert!(has(&p, "PyTypedDict", part), "{part}: {p:?}");
    }
    let p = inspect(
        "from typing import TypedDict\nclass Movie(TypedDict):\n title:str\n year:int\nvalue:Movie={'title':'ok','year':2024}\n",
    );
    no(&p, "PyTypedDict");
}
#[test]
fn plain_overrides_and_canonical_markers_preserve_variance() {
    let p = inspect(
        "from typing import override,final\nclass Base:\n def run(self,value:int)->str: return ''\nclass Child(Base):\n def run(self,value:str)->int: return 1\n @override\n def missing(self): pass\n@final\nclass Sealed: pass\nclass Invalid(Sealed): pass\n",
    );
    assert!(has(&p, "PyIncompatibleOverride", "run"), "{p:?}");
    assert!(has(&p, "PyTypeChecker", "does not override"));
    assert!(has(&p, "PyTypeChecker", "final class"));
    let p = inspect(
        "class Base:\n def run(self,value:str)->object: return value\nclass Child(Base):\n def run(self,value:object)->str: return ''\n",
    );
    no(&p, "PyIncompatibleOverride");
}
#[test]
fn protocol_mismatch_names_missing_members_and_protocol_instantiation() {
    let p = inspect(
        "from typing import Protocol\nclass Runner(Protocol):\n def run(self)->str: ...\nclass Empty: pass\ndef need(value:Runner): pass\nneed(Empty())\nRunner()\n",
    );
    assert!(
        has(&p, "PyTypeChecker", "missing protocol member(s): run"),
        "{p:?}"
    );
    assert!(has(&p, "PyTypeChecker", "Protocols cannot be instantiated"));
    let p = inspect("class Protocol: pass\nclass Runner(Protocol): pass\nRunner()\n");
    assert!(!has(&p, "PyTypeChecker", "Protocols cannot"));
}
#[test]
fn generic_arity_and_type_hint_syntax_have_canonical_and_quoted_controls() {
    let p = inspect(
        "from typing import List,Optional,Annotated,Callable,Union\na:List[int,str]\nb:Optional\nc:Annotated[int]\nd:Callable[int,str]\ne:'list['\nf:int[1]\nclass Pair[T,U]: pass\ng:Pair[int]\n",
    );
    for part in [
        "expects 1",
        "requires type arguments",
        "at least one metadata",
        "Callable's first",
        "Invalid syntax",
        "literal value",
        "expects 2",
    ] {
        assert!(has(&p, "PyTypeHints", part), "{part}: {p:?}");
    }
    let p = inspect(
        "from typing import Callable,Annotated,Optional\na:Callable[[int],str]\nb:Annotated[int,'metadata']\nc:Optional[int]\nd:tuple[()]\ne:'list[int]'\nclass Optional: pass\nf:Optional\n",
    );
    assert!(
        !has(&p, "PyTypeHints", "Callable's first") && !has(&p, "PyTypeHints", "Invalid syntax"),
        "{p:?}"
    );
}
#[test]
fn typevars_self_paramspec_aliases_and_optional_defaults_are_validated() {
    let p = inspect(
        "from typing import TypeVar,Self,ParamSpec,TypeAlias\nT=TypeVar('T')\nP=ParamSpec('P')\nvalue:T\nbad:Self\nAlias:TypeAlias=42\ndef f(value:T)->int: return 1\ndef g(value:int=None): pass\ndef h(value:P.args): pass\n",
    );
    for (code, part) in [
        ("PyTypeVarMisuse", "unbound"),
        ("PyTypeVarMisuse", "only once"),
        ("PyTypeHints", "Self cannot"),
        ("PyTypeHints", "literal value"),
        ("PyTypeHints", "Default None"),
        ("PyTypeHints", "ParamSpec.args"),
    ] {
        assert!(has(&p, code, part), "{code}/{part}: {p:?}");
    }
    let p = inspect(
        "from typing import TypeVar,Self,ParamSpec\nT=TypeVar('T')\nP=ParamSpec('P')\ndef identity(value:T)->T: return value\ndef forward(*args:P.args,**kwargs:P.kwargs): pass\nclass C:\n def clone(self)->Self: return self\n",
    );
    assert!(
        !has(&p, "PyTypeVarMisuse", "unbound")
            && !has(&p, "PyTypeHints", "Self cannot")
            && !has(&p, "PyTypeHints", "ParamSpec.args"),
        "{p:?}"
    );
}
#[test]
fn dataclasses_namedtuples_slots_properties_and_dunders_have_declaration_checks() {
    let p = inspect(
        "from dataclasses import dataclass\nfrom typing import NamedTuple\n@dataclass(order=True)\nclass Data:\n first:int=1\n second:int\n mutable:list[int]=[]\n def __lt__(self,other): return False\nclass Tuple(NamedTuple):\n _private:int\nclass C:\n __slots__=('field',)\n field=1\n @property\n def value(self)->int: return 1\n @value.setter\n def value(self): pass\n def __len__(self)->str: return 'bad'\n def __exit__(self): pass\n",
    );
    for (code, part) in [
        ("PyDataclass", "Non-default"),
        ("PyDataclass", "Mutable"),
        ("PyDataclass", "order=True"),
        ("PyNamedTuple", "underscore"),
        ("PySlotsConflict", "field"),
        ("PyProperty", "2 parameter"),
        ("PyDunderSignature", "must return"),
        ("PyDunderSignature", "4 positional"),
    ] {
        assert!(has(&p, code, part), "{code}/{part}: {p:?}");
    }
    let p = inspect(
        "class C:\n __slots__=('field',)\n field:int\n @property\n def value(self)->int: return 1\n @value.setter\n def value(self,new:int)->None: pass\n def __len__(self)->int: return 1\n",
    );
    no(&p, "PyProperty");
    no(&p, "PySlotsConflict");
    no(&p, "PyDunderSignature");
}
#[test]
fn overload_groups_require_implementations_and_consistent_parameters() {
    let p = inspect(
        "from typing import overload\n@overload\ndef missing(value:int)->int: ...\n@overload\ndef single(value:int)->int: ...\ndef single(value:str)->str: return value\n",
    );
    assert!(has(&p, "PyOverloads", "at least two"));
    assert!(has(&p, "PyOverloads", "no implementation"));
    assert!(has(&p, "PyOverloads", "cannot accept every"));
    let p = inspect(
        "from typing import overload\n@overload\ndef choose(value:int)->int: ...\n@overload\ndef choose(value:str)->str: ...\ndef choose(value:object)->int|str: return 1\n",
    );
    no(&p, "PyOverloads");
}
#[test]
fn exception_types_and_broad_before_specific_order_are_checked() {
    let p = inspect(
        "raise 1\ntry: pass\nexcept 1: pass\ntry: pass\nexcept Exception: pass\nexcept ValueError: pass\n",
    );
    assert!(has(&p, "PyException", "BaseException"));
    assert!(has(&p, "PyException", "exception class"));
    assert!(has(&p, "PyUnreachableExcept", "ValueError"));
    let p = inspect(
        "class Error(Exception): pass\ntry: raise Error()\nexcept (ValueError,Error): pass\nexcept Exception: pass\n",
    );
    no(&p, "PyException");
    no(&p, "PyUnreachableExcept");
}
#[test]
fn percent_formats_enforce_arity_keys_and_conversion_contracts() {
    let p = inspect(
        "a='%s %d' % ('one',)\nb='%(missing)s' % {'other':1}\nc='%z' % 1\nd='%d' % 'text'\ne=b'%s' % 'text'\n",
    );
    for part in [
        "expects 2",
        "Missing percent-format key",
        "Unsupported format",
        "numeric value",
    ] {
        assert!(has(&p, "PyStringFormat", part), "{part}: {p:?}");
    }
    let p = inspect(
        "a='%s %d' % ('one',2)\nb='%(key)s' % {'key':'value'}\nc='%% %*s' % (3,'x')\nd=b'%s' % b'bytes'\n",
    );
    no(&p, "PyStringFormat");
}
#[test]
fn brace_formats_enforce_fields_and_allow_nested_specs_and_escaping() {
    let p = inspect(
        "a='{2}'.format(1)\nb='{missing}'.format(other=1)\nc='{} {0}'.format(1)\nd='plain'.format(1)\ne='}'.format()\n",
    );
    for part in [
        "out of range",
        "Missing format keyword",
        "Cannot mix",
        "unused",
        "Unmatched",
    ] {
        assert!(has(&p, "PyStringFormat", part), "{part}: {p:?}");
    }
    let p = inspect(
        "a='{{{0}}}'.format(1)\nb='{value:{width}}'.format(value=1,width=3)\nc='{name}'.format_map({'name':'value'})\n",
    );
    no(&p, "PyStringFormat");
}
#[test]
fn tuple_balance_mutable_default_calls_and_finally_transfers_are_distinct() {
    let p = inspect(
        "left,right=(1,)\nhead,*tail,last=[]\ndef collect(items=[]): return items\ncollect()\ncollect([])\ndef f():\n try: return 1\n finally: return 2\n",
    );
    assert_eq!(
        p.iter()
            .filter(|p| p.diagnostic.code == "PyTupleAssignmentBalance")
            .count(),
        2,
        "{p:?}"
    );
    assert_eq!(
        p.iter()
            .filter(|p| p.diagnostic.code == "PyMutableDefaultCall")
            .count(),
        1,
        "{p:?}"
    );
    assert!(has(&p, "PyFinallyTransfer", "discard"));
    let p = inspect("head,*tail=(1,2,3)\n(left,(middle,right))=(1,(2,3))\n");
    no(&p, "PyTupleAssignmentBalance");
}
#[test]
fn fixes_and_nonfatal_correctness_suggestions_preserve_owned_source() {
    let p = inspect("items=list()\nsettings=dict()\ncount=1\ncount=count+2\nzero=1//0\nvalue=1,\n");
    assert!(has(&p, "PyCollectionLiteral", "list"));
    assert!(has(&p, "PyCollectionLiteral", "dict"));
    assert!(has(&p, "PyAugmentedAssignment", "Augmented"));
    assert!(has(&p, "PyDivisionByZero", "zero"));
    assert!(has(&p, "PyTrailingCommaTuple", "one-element"));
    assert!(p
        .iter()
        .filter(|p| p.diagnostic.code == "PyCollectionLiteral")
        .all(|p| !p.fixes[0].edits.is_empty()));
}
#[test]
fn literal_collection_fixes_and_finally_loop_boundaries_preserve_runtime_roles() {
    let p = inspect(
        "items=list((1,2))\nunique=set([1,2])\ndef f():\n try: pass\n finally:\n  for item in [1]:\n   break\n return 1\n",
    );
    assert!(has(&p, "PyCollectionLiteral", "list"), "{p:?}");
    assert!(has(&p, "PyCollectionLiteral", "set"), "{p:?}");
    no(&p, "PyFinallyTransfer");
    let p = inspect("def f():\n try: return 1\n finally: return 2\n");
    assert!(has(&p, "PyFinallyTransfer", "Control transfer"), "{p:?}");
}

#[test]
fn generic_aliases_and_union_annotations_do_not_borrow_runtime_operation_checks() {
    let p = inspect(
        "from typing import Generic as Base, TypeVar\nT=TypeVar('T')\nclass Real(Base[T]): pass\ndef keep(value:int|None)->int|None: return value\n",
    );
    no(&p, "PyTypeChecker");
    let p = inspect("class Generic: pass\nBase=Generic()\nBase[int]\n");
    assert!(has(&p, "PyTypeChecker", "not subscriptable"), "{p:?}");
}

#[test]
fn evaluated_annotated_metadata_keeps_runtime_checks_without_checking_type_operators() {
    for source in [
        "from typing import Annotated\nx:Annotated[int, 1 + 's']\n",
        "from typing import Annotated as A\nx:list[A[int|None, marker(1 + 's')]]\n",
        "from typing import Annotated\nx:Annotated[int, (lambda: 1 + 's')()]\n",
        "def Annotated(kind, metadata): return kind\nx:Annotated(int, 1 + 's')\n",
        "class Annotated:\n def __class_getitem__(cls, values): return int\nx:Annotated[int, 1 + 's']\n",
    ] {
        let problems = inspect(source);
        assert!(has(&problems, "PyTypeChecker", "Operator '+'"), "{source}: {problems:?}");
    }
    for source in [
        "from typing import Annotated\nx:Annotated[int|None, 1 + 2]\n",
        "from __future__ import annotations\nfrom typing import Annotated\nx:Annotated[int, 1 + 's']\n",
        "from typing import Annotated\ndef local():\n x:Annotated[int, 1 + 's']\n",
        "from typing import Annotated\ntype Deferred = Annotated[int, 1 + 's']\n",
    ] {
        no(&inspect(source), "PyTypeChecker");
    }
}
#[test]
fn narrowed_member_references_do_not_report_possible_none() {
    let p = inspect(
        "class Node:\n def name(self)->str: return ''\nclass Holder:\n def __init__(self)->None:\n  self.node: Node|None = None\n def use(self)->str:\n  if self.node is not None:\n   return self.node.name()\n  return ''\n def guard(self)->str:\n  if not self.node: return ''\n  return self.node.name()\n",
    );
    assert!(!has(&p, "PyTypeChecker", "Possible None"), "{p:?}");
    let p = inspect(
        "class Node:\n def name(self)->str: return ''\nclass Holder:\n def __init__(self)->None:\n  self.node: Node|None = None\n def use(self, other: 'Holder')->str:\n  if self.node is not None:\n   self = other\n   return self.node.name()\n  return ''\n",
    );
    assert!(has(&p, "PyTypeChecker", "Possible None"), "{p:?}");
}
