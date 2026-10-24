//! Authenticated forge requests. The UI dispatches writes only from explicit actions.
pub mod auth;
pub mod client;
pub mod gitlab;
pub mod models;
pub mod requests;
pub use client::{encode, Client, HttpTransport, Request, Response, Server, Transport};
pub use models::*;
pub use requests::*;

impl crate::GitRepo {
    pub fn github_revision(&self, revision: &str) -> anyhow::Result<String> {
        Ok(self
            .repo
            .revparse_single(revision)?
            .peel_to_commit()?
            .id()
            .to_string())
    }
}

#[cfg(test)]
mod fixtures;
