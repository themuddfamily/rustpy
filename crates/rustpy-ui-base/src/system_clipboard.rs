//! The one seam through which the UI crates touch the operating-system
//! clipboard. Unit tests never read or write the user's real clipboard: in a
//! unit-test process (see [`crate::test_support`]) opening it always fails as
//! "not supported", so callers fall back to their in-IDE state (e.g. *Paste
//! from History*).

/// Opens the system clipboard.
pub fn open() -> Result<arboard::Clipboard, arboard::Error> {
    if crate::test_support::is_unit_test() {
        return Err(arboard::Error::ClipboardNotSupported);
    }
    arboard::Clipboard::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn unit_tests_never_reach_the_system_clipboard() {
        assert!(super::open().is_err());
    }
}
