use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn compound_delete_and_reexport_mutation_retire_stale_alias_signature() {
    for source in [
        "import helper\nclass Holder: other=0\nholder=Holder()\ndel holder.other, helper.impossible\nfrom helper import impossible as different\ndifferent(1)\n",
        "import helper_alias\ndef ordinary(value:int)->None:pass\nhelper_alias.public=ordinary\nfrom helper_alias import public as different\ndifferent(1)\n",
    ] {
        let db=Analysis::for_project(Path::new("/never-edge-retirement"));
        db.set_file_text(Path::new("/never-edge-retirement/helper.py"),"from typing import Never\ndef impossible(value:Never)->None:pass\n");
        db.set_file_text(Path::new("/never-edge-retirement/helper_alias.py"),"from helper import impossible as public\n");
        let id=db.set_file_text(Path::new("/never-edge-retirement/m.py"),source);
        let file=db.file(id).unwrap();
        assert!(file.parse.errors().is_empty());
        let call=file.parse.syntax_node().descendants().filter_map(ast::CallExpr::cast).find(|node|node.syntax().text()=="different(1)").unwrap();
        let infer=Inferer::new(&db);
        // A physical stale declaration, not a claim the deleted/replaced runtime member exists.
        let ty=infer.type_of_expr(&file,&call.callee().unwrap());
        let sig=ty.signature().unwrap();
        assert_eq!(sig.def.as_ref().unwrap().name,"impossible");
        assert!(matches!(sig.params[0].ty,Type::Never));
        assert!(inspections::inspect(&db,&file,&InspectionOptions::default()).into_iter().all(|problem|problem.diagnostic.code!="PyTypeChecker"));
    }
}
