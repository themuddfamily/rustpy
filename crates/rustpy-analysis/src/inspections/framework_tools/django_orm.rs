//! Unknown Django ORM field names and lookups in queryset calls. Resolution and the
//! conservative model-completeness rules live with the shared ORM path model.
use super::*;

pub(super) fn run(inf: &Inferer<'_>, file: &Arc<FileInfo>, sink: &mut Sink) {
    for problem in rustpy_semantic::frameworks::orm_problems(inf.db, file) {
        sink.push(
            problem.range,
            Severity::Warning,
            "PyDjangoOrmLookup",
            problem.message,
            vec![],
        );
    }
}
