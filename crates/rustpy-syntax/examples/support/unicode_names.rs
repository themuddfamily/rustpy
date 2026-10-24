//! Complete Unicode15 canonical scalar names and formal aliases. No runtime Python.
use super::{Result, Unsupported};
const NAMES: &str = include_str!("unicode15-names.tsv");
pub(super) fn lookup(name: &str) -> Result<u32> {
    if name.len() > 256 || !name.is_ascii() {
        return Err(Unsupported(
            "Invalid Unicode name spelling or budget".into(),
        ));
    }
    let name = name.to_ascii_uppercase();
    let mut low = 0;
    let mut high = NAMES.len();
    while low < high {
        let middle = low + (high - low) / 2;
        let start = NAMES[..middle].rfind('\n').map_or(0, |n| n + 1);
        let end = NAMES[start..].find('\n').map_or(NAMES.len(), |n| start + n);
        let (key, value) = NAMES[start..end]
            .split_once('\t')
            .expect("pinned names table");
        match key.cmp(name.as_str()) {
            std::cmp::Ordering::Less => low = end + 1,
            std::cmp::Ordering::Greater => high = start,
            std::cmp::Ordering::Equal => {
                return Ok(u32::from_str_radix(value, 16).expect("pinned scalar"))
            }
        }
    }
    Err(Unsupported(format!(
        "Unknown Unicode15 scalar name or named sequence: {name}"
    )))
}
