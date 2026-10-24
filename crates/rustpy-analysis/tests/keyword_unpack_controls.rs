use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn codes(source: &str) -> Vec<String> {
    let db = Analysis::for_project(Path::new("/keyword-unpack-controls"));
    let id = db.set_file_text(Path::new("/keyword-unpack-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|problem| {
            matches!(
                problem.diagnostic.code.as_str(),
                "PyArgumentList" | "PyTypeChecker"
            )
        })
        .map(|problem| problem.diagnostic.code.to_string())
        .collect()
}
#[test]
fn imported_aliases_and_inherited_required_fields() {
    let prefix="from typing import TypedDict as TD,Unpack as UP,NotRequired as NR\nclass Base(TD):\n name:str\nclass Movie(Base,total=False):\n year:int\n note:NR[str]\ndef consume(**kwargs:UP[Movie])->None:...\n";
    assert!(codes(&format!("{prefix}consume(name='x')\n")).is_empty());
    assert_eq!(
        codes(&format!("{prefix}consume(year=2026)\n")),
        ["PyArgumentList"]
    );
}
#[test]
fn required_optional_and_readonly_qualifiers() {
    let prefix="from typing import TypedDict,Unpack,Required,NotRequired\nfrom typing_extensions import ReadOnly\nclass Movie(TypedDict,total=False):\n name:Required[str]\n note:ReadOnly[NotRequired[str]]\ndef consume(**kwargs:Unpack[Movie])->None:...\n";
    assert!(codes(&format!("{prefix}consume(name='x')\n")).is_empty());
    assert_eq!(codes(&format!("{prefix}consume()\n")), ["PyArgumentList"]);
    assert_eq!(
        codes(&format!("{prefix}consume(name='x',note=1)\n")),
        ["PyTypeChecker"]
    );
}
#[test]
fn quoted_unpack_and_optional_fields_keep_source_scope() {
    let prefix="from typing import TypedDict,Unpack,NotRequired\nclass Movie(TypedDict):\n name:str\n note:'NotRequired[str]'\ndef consume(**kwargs:'Unpack[Movie]')->None:...\n";
    assert!(codes(&format!("{prefix}consume(name='x')\n")).is_empty());
    assert_eq!(codes(&format!("{prefix}consume()\n")), ["PyArgumentList"]);
}
#[test]
fn positional_only_collision_is_a_distinct_keyword_field() {
    let prefix="from typing import TypedDict,Unpack\nclass Fields(TypedDict):\n name:str\ndef consume(name:int,/,**kwargs:Unpack[Fields])->None:...\n";
    assert!(codes(&format!("{prefix}consume(1,name='x')\n")).is_empty());
    assert_eq!(codes(&format!("{prefix}consume(1)\n")), ["PyArgumentList"]);
}
#[test]
fn typed_dictionary_star_and_standard_dictionary_refusal() {
    let prefix="from typing import TypedDict,Unpack,Any\nclass Movie(TypedDict):\n name:str\n year:int\ndef consume(**kwargs:Unpack[Movie])->None:...\n";
    assert!(codes(&format!(
        "{prefix}def forward(data:Movie):\n consume(**data)\n"
    ))
    .is_empty());
    assert_eq!(
        codes(&format!(
            "{prefix}def forward(data:dict[str,object]):\n consume(**data)\n"
        )),
        ["PyTypeChecker"]
    );
    assert!(codes(&format!(
        "{prefix}def forward(data:Any):\n consume(**data)\n"
    ))
    .is_empty());
}
#[test]
fn unknown_unpack_and_mutated_module_member_never_grant_field_authority() {
    assert!(codes("from typing import TypedDict\nclass Movie(TypedDict):\n name:str\nclass Unpack:\n def __class_getitem__(cls,item):return object\ndef consume(**kwargs:Unpack[Movie])->None:...\nconsume(extra=1)\n").is_empty());
    assert!(codes("import typing\nclass Movie(typing.TypedDict):\n name:str\ntyping.Unpack=object\ndef consume(**kwargs:typing.Unpack[Movie])->None:...\nconsume(extra=1)\n").is_empty());
}
#[test]
fn fake_signature_preserving_decorator_does_not_authorize_unpack() {
    assert!(codes("from typing import TypedDict,Unpack\nclass Movie(TypedDict):\n name:str\ndef lru_cache(function):return lambda **kwargs:None\n@lru_cache\ndef consume(**kwargs:Unpack[Movie])->None:...\nconsume(extra=1)\n").is_empty());
}

#[test]
fn unknown_schema_keeps_independently_known_normal_parameters() {
    assert_eq!(codes("from typing import TypedDict\nclass Movie(TypedDict):\n name:str\nclass Unpack:\n def __class_getitem__(cls,item):return object\ndef consume(prefix:int,**kwargs:Unpack[Movie])->None:...\nconsume('bad',extra=1)\n"),["PyTypeChecker"]);
}
#[test]
fn typed_star_reports_wrong_or_optional_required_field() {
    let prefix="from typing import TypedDict,Unpack,NotRequired\nclass Movie(TypedDict):\n name:str\n year:int\ndef consume(**kwargs:Unpack[Movie])->None:...\n";
    assert_eq!(codes(&format!("{prefix}class Bad(TypedDict):\n name:str\n year:str\ndef forward(data:Bad):\n consume(**data)\n")),["PyTypeChecker"]);
    assert_eq!(codes(&format!("{prefix}class Partial(TypedDict):\n name:str\n year:NotRequired[int]\ndef forward(data:Partial):\n consume(**data)\n")),["PyArgumentList"]);
}

#[test]
fn compound_and_aliased_module_writes_retire_unpack_authority() {
    assert!(codes("import typing\nclass Movie(typing.TypedDict):\n name:str\n(typing.Unpack,other)=(object,1)\ndef consume(**kwargs:typing.Unpack[Movie])->None:...\nconsume(extra=1)\n").is_empty());
    assert!(codes("import typing\nclass Movie(typing.TypedDict):\n name:str\nalias=typing\n(alias.Unpack,other)=(object,1)\ndef consume(**kwargs:typing.Unpack[Movie])->None:...\nconsume(extra=1)\n").is_empty());
}
#[test]
fn required_typed_star_keyword_collisions_are_errors() {
    let prefix="from typing import TypedDict,Unpack\nclass Name(TypedDict):\n name:str\ndef consume(**kwargs:Unpack[Name])->None:...\n";
    assert_eq!(
        codes(&format!(
            "{prefix}def forward(data:Name):\n consume(**data,name='explicit')\n"
        )),
        ["PyArgumentList"]
    );
    assert_eq!(
        codes(&format!(
            "{prefix}def forward(left:Name,right:Name):\n consume(**left,**right)\n"
        )),
        ["PyArgumentList"]
    );
}

#[test]
fn imported_unpack_after_module_replacement_remains_unknown() {
    assert!(codes("import typing\ntyping.Unpack=object\nfrom typing import TypedDict,Unpack\nclass Movie(TypedDict):\n name:str\ndef consume(**kwargs:Unpack[Movie])->None:...\nconsume(extra=1)\n").is_empty());
    assert!(codes("import typing\ntyping.Unpack=object\nfrom typing import TypedDict,Unpack as UP\nclass Movie(TypedDict):\n name:str\ndef consume(**kwargs:UP[Movie])->None:...\nconsume(extra=1)\n").is_empty());
}
