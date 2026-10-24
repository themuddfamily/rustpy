//! Review markers are valid only for the exact loaded document revision.
#[derive(Clone, Debug)]
pub struct Marker {
    pub offset: rustpy_core::TextSize,
    pub id: u64,
    pub text: String,
    pub version: u64,
    pub document: rustpy_text::DocumentId,
}
