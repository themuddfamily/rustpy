use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn unbound(source: &str) -> usize {
    let db = Analysis::for_project(Path::new("/match-family-exhaustion"));
    let id = db.set_file_text(Path::new("/match-family-exhaustion/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .iter()
        .filter(|problem| problem.diagnostic.code == "PyUnboundLocalVariable")
        .count()
}
#[test]
fn finite_enum_and_scalar_alternatives_cover_only_unguarded_complete_domains() {
    let prefix = "from enum import Enum\nclass Color(Enum):\n    RED=1\n    BLUE=2\ndef use(value:Color,flag:bool):\n    match value:\n        case Color.RED: result=1\n";
    assert_eq!(
        unbound(&format!(
            "{prefix}        case Color.BLUE: result=2\n    return result\n"
        )),
        0
    );
    assert_eq!(unbound(&format!("{prefix}    return result\n")), 1);
    assert_eq!(
        unbound(&format!(
            "{prefix}        case Color.BLUE if flag: result=2\n    return result\n"
        )),
        1
    );
    let bools="def use(value:bool):\n    match value:\n        case True:result=1\n        case False:result=2\n    return result\n";
    assert_eq!(unbound(bools), 0);
    let strings="from typing import Literal\ndef use(value:Literal['a','b']|None):\n    match value:\n        case 'a'|'b'|None:result=1\n    return result\n";
    assert_eq!(unbound(strings), 0);
}
#[test]
fn tuple_nested_star_mapping_and_class_shape_coverage_keep_uncertain_edges() {
    for source in [
        "def use(value:tuple[int,tuple[str],bool]):\n    match value:\n        case (first,(nested,),*rest):result=1\n    return result\n",
        "def use(value:list[int]):\n    match value:\n        case [*rest]:result=1\n    return result\n",
        "def use(value:dict[str,int]):\n    match value:\n        case {**rest}:result=1\n    return result\n",
        "class Point:pass\ndef use(value:Point):\n    match value:\n        case Point():result=1\n    return result\n",
    ] { assert_eq!(unbound(source),0,"{source}"); }
    for source in [
        "def use(value:list[int]):\n    match value:\n        case [only]:result=1\n    return result\n",
        "def use(value:dict[str,int]):\n    match value:\n        case {'present': item}:result=1\n    return result\n",
        "def use(value:tuple[int,str],flag:bool):\n    match value:\n        case (first,second) if flag:result=1\n    return result\n",
    ] { assert_eq!(unbound(source),1,"{source}"); }
}

#[test]
fn inherited_custom_metaclass_does_not_make_same_class_pattern_exhaustive() {
    let source = "class Meta(type):\n    def __instancecheck__(cls,value):return False\nclass Base(metaclass=Meta):pass\nclass C(Base):pass\nclass Child(C):pass\ndef use(value:C):\n    match value:\n        case C():result=1\n    return result\nuse(Child())\n";
    assert_eq!(unbound(source), 1);
    let plain = "class C:pass\ndef use(value:C):\n    match value:\n        case C():result=1\n    return result\n";
    assert_eq!(unbound(plain), 0);
}

#[test]
fn known_physical_inheritance_and_canonical_object_keep_class_coverage() {
    for source in [
        "class Base:pass\nclass C(Base):pass\ndef use(value:C):\n    match value:\n        case C():result=1\n    return result\n",
        "class Base(object):pass\nclass C(Base):pass\nclass Child(C):pass\ndef use(value:C):\n    match value:\n        case C():result=1\n    return result\nuse(Child())\n",
    ] { assert_eq!(unbound(source), 0, "{source}"); }
}
