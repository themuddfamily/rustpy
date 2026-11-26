//! Explicit loopback collaboration transport. Remote users connect through an
//! authenticated SSH/VPN local forward; the IDE never exposes a public listener.
mod transport;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use transport::{Event, Session};
pub const MAX_TEXT: usize = 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedDocument {
    pub id: u64,
    pub name: String,
    pub text: String,
    pub version: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Message {
    Welcome {
        peer: u64,
    },
    Snapshot(SharedDocument),
    Removed(u64),
    Edit {
        id: u64,
        base: u64,
        text: String,
    },
    Permission {
        write: bool,
    },
    Conflict {
        id: u64,
        current: SharedDocument,
    },
    Presence {
        peer: u64,
        name: String,
        document: Option<u64>,
        line: usize,
    },
    Left(u64),
    Notice(String),
}
#[derive(Default)]
pub struct Authority {
    pub documents: BTreeMap<u64, SharedDocument>,
    pub permissions: BTreeMap<u64, bool>,
}
impl Authority {
    pub fn edit(
        &mut self,
        peer: u64,
        id: u64,
        base: u64,
        text: String,
    ) -> Result<SharedDocument, String> {
        if !self.permissions.get(&peer).copied().unwrap_or(false) {
            return Err("Participant is read-only".into());
        }
        if text.len() > MAX_TEXT {
            return Err("Document exceeds sharing limit".into());
        }
        let document = self
            .documents
            .get_mut(&id)
            .ok_or("Document is no longer shared")?;
        if document.version != base {
            return Err("Version conflict: authoritative text was retained".into());
        }
        document.version = document
            .version
            .checked_add(1)
            .ok_or("Document version exhausted")?;
        document.text = text;
        Ok(document.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permissions_versions_and_revocation() {
        let mut a = Authority::default();
        a.documents.insert(
            1,
            SharedDocument {
                id: 1,
                name: "file".into(),
                text: "original".into(),
                version: 3,
            },
        );
        assert!(a.edit(2, 1, 3, "attack".into()).is_err());
        a.permissions.insert(2, true);
        assert_eq!(a.edit(2, 1, 3, "accepted".into()).unwrap().version, 4);
        assert!(a.edit(2, 1, 3, "stale".into()).is_err());
        assert_eq!(a.documents[&1].text, "accepted");
        a.permissions.insert(2, false);
        assert!(a.edit(2, 1, 4, "revoked".into()).is_err());
    }
}
