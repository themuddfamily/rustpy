//! The one seam through which rustpy-ui touches the operating-system
//! clipboard. Unit tests never read or write the user's real clipboard:
//! under `cfg(test)` opening it always fails as "not supported", so callers
//! fall back to their in-IDE state (e.g. *Paste from History*).

/// Opens the system clipboard.
pub(crate) fn open() -> Result<arboard::Clipboard, arboard::Error> {
    #[cfg(test)]
    {
        Err(arboard::Error::ClipboardNotSupported)
    }
    #[cfg(not(test))]
    {
        arboard::Clipboard::new()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unit_tests_never_reach_the_system_clipboard() {
        assert!(super::open().is_err());
    }
}
