//! Python source refactorings (roadmap §2.9).

pub mod change_signature;
pub mod conversions;
pub mod extraction;
pub mod generation;
pub mod inline;
pub mod members;
pub mod movement;
pub mod refactor;
pub mod refactor2;
pub mod rename;
pub mod rename_family;
pub mod rename_framework;
pub mod safe_delete;

/// The captured project boundary and file inventory for a multi-file edit plan.
#[derive(Clone, Copy)]
pub struct ProjectEditContext<'a> {
    pub root: &'a std::path::Path,
    pub files: &'a [std::path::PathBuf],
}

#[cfg(test)]
mod refactor_tests;
