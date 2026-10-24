//! Windows keyring: generic credentials in the Credential Manager
//! (`CredWriteW` / `CredReadW` / `CredDeleteW` / `CredEnumerateW`), named
//! `RustPy/<key>` and persisted per user on this machine.

use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredEnumerateW, CredFree, CredReadW, CredWriteW, CREDENTIALW,
    CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

use crate::{Backend, SafeError};

const PREFIX: &str = "RustPy/";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated UTF-16 string.
unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: the caller guarantees a NUL terminator.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` elements were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

fn last_error(what: &str) -> SafeError {
    // SAFETY: no preconditions.
    let code = unsafe { GetLastError() };
    SafeError::Failed(format!("Credential Manager: {what} failed (error {code})"))
}

/// The Windows Credential Manager.
pub struct CredentialManager;

impl Backend for CredentialManager {
    fn name(&self) -> String {
        "Windows Credential Manager".into()
    }

    fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
        let target = wide(&format!("{PREFIX}{key}"));
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `target` is NUL-terminated; `cred` receives a buffer freed below.
        let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
        if ok == 0 {
            // SAFETY: no preconditions.
            if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                return Ok(None);
            }
            return Err(last_error("CredReadW"));
        }
        // SAFETY: CredReadW succeeded, so `cred` points to a valid CREDENTIALW
        // whose blob has `CredentialBlobSize` bytes; it is freed with CredFree.
        let bytes = unsafe {
            let c = &*cred;
            let blob = if c.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
            };
            CredFree(cred as *const _);
            blob
        };
        String::from_utf8(bytes).map(Some).map_err(|_| {
            SafeError::Failed("Credential Manager: the stored secret is not UTF-8 text".into())
        })
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
        if secret.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            return Err(SafeError::Failed(format!(
                "Credential Manager: secrets are limited to {CRED_MAX_CREDENTIAL_BLOB_SIZE} bytes"
            )));
        }
        let mut target = wide(&format!("{PREFIX}{key}"));
        let mut user = wide("RustPy");
        let mut comment = wide("Stored by the RustPy IDE");
        let mut blob = secret.as_bytes().to_vec();
        let cred = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            Comment: comment.as_mut_ptr(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: user.as_mut_ptr(),
            ..Default::default()
        };
        // SAFETY: every pointer in `cred` refers to a live buffer of this frame.
        if unsafe { CredWriteW(&cred, 0) } == 0 {
            return Err(last_error("CredWriteW"));
        }
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SafeError> {
        let target = wide(&format!("{PREFIX}{key}"));
        // SAFETY: `target` is NUL-terminated.
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            // SAFETY: no preconditions.
            if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                return Ok(());
            }
            return Err(last_error("CredDeleteW"));
        }
        Ok(())
    }

    fn keys(&self) -> Result<Vec<String>, SafeError> {
        let filter = wide(&format!("{PREFIX}*"));
        let mut count = 0u32;
        let mut creds: *mut *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `filter` is NUL-terminated; the array is freed with CredFree.
        if unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut creds) } == 0 {
            // SAFETY: no preconditions.
            if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                return Ok(Vec::new());
            }
            return Err(last_error("CredEnumerateW"));
        }
        let mut keys = Vec::new();
        // SAFETY: CredEnumerateW returned `count` valid credential pointers.
        unsafe {
            for i in 0..count as usize {
                let c = &**creds.add(i);
                if c.Type == CRED_TYPE_GENERIC {
                    if let Some(k) = from_wide(c.TargetName).strip_prefix(PREFIX) {
                        keys.push(k.to_string());
                    }
                }
            }
            CredFree(creds as *const _);
        }
        keys.sort();
        Ok(keys)
    }
}
