use rustpy_semantic::{db::Analysis, scope::BindingKind};
use std::path::Path;
#[test]
fn literal_row_definition_forms_have_owned_named_bindings() {
    let source="import os as imported\nglobal_name=0\ndef declared(param):\n global global_name\n assigned=param\n assigned+=1\n annotated:int=1\n bare:int\n for loop in (0,): pass\n with imported.devnull as context: pass\n try: pass\n except Exception as error: pass\n match param:\n  case {'k': captured}: pass\n class Nested: pass\n def inner(): pass\n return assigned\n";
    let db = Analysis::for_project(Path::new("/definition-inventory"));
    let id = db.set_file_text(Path::new("/definition-inventory/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let expected = [
        ("declared", BindingKind::Function),
        ("param", BindingKind::Param),
        ("assigned", BindingKind::Assignment),
        ("assigned", BindingKind::AugAssignment),
        ("annotated", BindingKind::Assignment),
        ("bare", BindingKind::Annotation),
        ("loop", BindingKind::For),
        ("context", BindingKind::With),
        ("error", BindingKind::Except),
        ("captured", BindingKind::MatchCapture),
        ("Nested", BindingKind::Class),
        ("inner", BindingKind::Function),
        ("global_name", BindingKind::Global),
    ];
    for (name, kind) in expected {
        let found: Vec<_> = file
            .scopes
            .bindings
            .iter()
            .filter(|binding| binding.name == name && binding.kind == kind)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "exact named definition kind {name}/{kind:?}"
        );
        assert_eq!(
            &source[usize::from(found[0].range.start())..usize::from(found[0].range.end())],
            name
        );
    }
    let imports: Vec<_> = file
        .scopes
        .bindings
        .iter()
        .filter(|binding| {
            binding.name == "imported" && matches!(binding.kind, BindingKind::Import(_))
        })
        .collect();
    assert_eq!(imports.len(), 1);
}
#[test]
fn assignment_augmented_delete_and_receiver_modes_keep_exact_read_write_ranges() {
    let source =
        "def f(param,obj):\n value=param\n value+=1\n obj.attr=value\n obj[0]=value\n del value\n";
    let db = Analysis::for_project(Path::new("/reference-inventory"));
    let id = db.set_file_text(Path::new("/reference-inventory/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let at = |needle: &str| source.find(needle).unwrap();
    for (needle, kind) in [
        ("value=", BindingKind::Assignment),
        ("value+=", BindingKind::AugAssignment),
    ] {
        assert!(
            file.scopes
                .bindings
                .iter()
                .any(|binding| binding.kind == kind
                    && usize::from(binding.range.start()) == at(needle)),
            "owned write definition {needle}"
        );
    }
    assert!(
        file.scopes
            .references
            .iter()
            .any(|reference| reference.name == "value"
                && !reference.is_write
                && usize::from(reference.range.start()) == at("value+=")),
        "augmented target read before write"
    );
    let deletion = at("del value") + 4;
    assert!(file
        .scopes
        .references
        .iter()
        .any(|reference| reference.name == "value"
            && reference.is_write
            && usize::from(reference.range.start()) == deletion));
    for needle in ["obj.attr", "obj[0]"] {
        assert!(
            file.scopes
                .references
                .iter()
                .any(|reference| reference.name == "obj"
                    && !reference.is_write
                    && usize::from(reference.range.start()) == at(needle)),
            "target receiver is read, not rebound"
        );
        assert!(!file.scopes.bindings.iter().any(
            |binding| binding.name == "obj" && usize::from(binding.range.start()) == at(needle)
        ));
    }
}
