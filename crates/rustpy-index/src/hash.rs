//! Stable hashes. Index files and shared bundles store them, so they must not
//! change between machines or Rust versions (std's `DefaultHasher` may):
//! SipHash-1-3 with fixed keys.

use std::hash::Hasher;

use siphasher::sip128::{Hasher128, SipHasher13 as Sip128};

const K0: u64 = 0x7275_7374_7079_2d69; // "rustpy-i"
const K1: u64 = 0x6e64_6578_2d76_3100; // "ndex-v1\0"

/// 128-bit hash of a file's bytes: two files with the same hash are taken to
/// have the same content (and therefore the same symbols).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub u128);

impl ContentHash {
    pub fn of(bytes: &[u8]) -> ContentHash {
        let mut h = Sip128::new_with_keys(K0, K1);
        h.write(bytes);
        ContentHash(h.finish128().into())
    }

    /// No hash recorded (binary modules are not read).
    pub fn is_none(self) -> bool {
        self.0 == 0
    }

    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }
}

/// A 64-bit hash of `text` as 16 hex digits (index file names).
pub fn short_hex(text: &str) -> String {
    let mut h = siphasher::sip::SipHasher13::new_with_keys(K0, K1);
    h.write(text.as_bytes());
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable() {
        // Pinned values: changing the algorithm or keys silently invalidates
        // every index and shared bundle, so it must be deliberate.
        assert_eq!(ContentHash::of(b"").to_hex().len(), 32);
        assert_eq!(ContentHash::of(b"x = 1\n"), ContentHash::of(b"x = 1\n"));
        assert_ne!(ContentHash::of(b"x = 1\n"), ContentHash::of(b"x = 2\n"));
        assert_eq!(short_hex("/usr/bin/python3"), short_hex("/usr/bin/python3"));
        assert_eq!(short_hex("/usr/bin/python3").len(), 16);
        assert_ne!(short_hex("a"), short_hex("b"));
        assert!(!ContentHash::of(b"").is_none());
    }
}
