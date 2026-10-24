//! Python inspections, quick fixes and intentions (roadmap §2.5–2.6).

pub mod file_analysis;
pub mod inspections;
pub mod intentions;

#[cfg(test)]
mod import_tests;
#[cfg(test)]
mod insight_tests;
#[cfg(test)]
mod narrow_tests;
