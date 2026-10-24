//! Native KWallet D-Bus fallback for desktops without the Secret Service bridge.
//! Wire signatures: KDE/kwallet src/api/KWallet/org.kde.KWallet.xml.
use crate::{Backend, SafeError};
use std::sync::Arc;
use zbus::blocking::{Connection, Proxy};

const APPLICATION: &str = "RustPy";
const FOLDER: &str = "RustPy";
const ENDPOINTS: &[(&str, &str)] = &[
    ("org.kde.kwalletd6", "/modules/kwalletd6"),
    ("org.kde.kwalletd5", "/modules/kwalletd5"),
    ("org.kde.kwalletd", "/modules/kwalletd"),
];
fn failure(operation: &str, error: impl std::fmt::Display) -> SafeError {
    SafeError::Unavailable(format!("KWallet {operation}: {error}"))
}
pub struct KWallet {
    connection: Connection,
    destination: &'static str,
    path: &'static str,
}
impl KWallet {
    pub fn connect() -> Result<Arc<dyn Backend>, String> {
        let connection =
            Connection::session().map_err(|e| format!("KWallet: no session bus ({e})"))?;
        for &(destination, path) in ENDPOINTS {
            let available = Proxy::new(&connection, destination, path, "org.kde.KWallet")
                .and_then(|proxy| proxy.call("isEnabled", &()))
                .unwrap_or(false);
            if available {
                return Ok(Arc::new(Self {
                    connection,
                    destination,
                    path,
                }));
            }
        }
        Err("No enabled KWallet D-Bus service is available".into())
    }
    fn with_wallet<T>(
        &self,
        operation: impl FnOnce(&Proxy<'_>, i32) -> Result<T, SafeError>,
    ) -> Result<T, SafeError> {
        let proxy = Proxy::new(
            &self.connection,
            self.destination,
            self.path,
            "org.kde.KWallet",
        )
        .map_err(|e| failure("connect", e))?;
        let wallet: String = proxy
            .call("networkWallet", &())
            .map_err(|e| failure("networkWallet", e))?;
        if wallet.is_empty() {
            return Err(failure("open", "No network wallet is configured"));
        }
        let handle: i32 = proxy
            .call("open", &(wallet.as_str(), 0i64, APPLICATION))
            .map_err(|e| failure("open", e))?;
        if handle < 0 {
            return Err(failure(
                "open",
                "The wallet was not unlocked or access was declined",
            ));
        }
        struct Lease<'a, 'p> {
            proxy: &'a Proxy<'p>,
            handle: i32,
        }
        impl Drop for Lease<'_, '_> {
            fn drop(&mut self) {
                // Release only this application handle, never force-close another application.
                let _: Result<i32, _> =
                    self.proxy.call("close", &(self.handle, false, APPLICATION));
            }
        }
        let _lease = Lease {
            proxy: &proxy,
            handle,
        };
        operation(&proxy, handle)
    }
    fn has_folder(proxy: &Proxy<'_>, handle: i32) -> Result<bool, SafeError> {
        proxy
            .call("hasFolder", &(handle, FOLDER, APPLICATION))
            .map_err(|e| failure("hasFolder", e))
    }
    fn has_entry(proxy: &Proxy<'_>, handle: i32, key: &str) -> Result<bool, SafeError> {
        proxy
            .call("hasEntry", &(handle, FOLDER, key, APPLICATION))
            .map_err(|e| failure("hasEntry", e))
    }
}
impl Backend for KWallet {
    fn name(&self) -> String {
        "KWallet (native D-Bus)".into()
    }
    fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
        self.with_wallet(|proxy, handle| {
            if !Self::has_folder(proxy, handle)? || !Self::has_entry(proxy, handle, key)? {
                return Ok(None);
            }
            let value: String = proxy
                .call("readPassword", &(handle, FOLDER, key, APPLICATION))
                .map_err(|e| failure("readPassword", e))?;
            Ok(Some(value))
        })
    }
    fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
        self.with_wallet(|proxy, handle| {
            if !Self::has_folder(proxy, handle)? {
                let created: bool = proxy
                    .call("createFolder", &(handle, FOLDER, APPLICATION))
                    .map_err(|e| failure("createFolder", e))?;
                if !created {
                    return Err(failure(
                        "createFolder",
                        "The RustPy folder could not be created",
                    ));
                }
            }
            let result: i32 = proxy
                .call("writePassword", &(handle, FOLDER, key, secret, APPLICATION))
                .map_err(|e| failure("writePassword", e))?;
            if result != 0 {
                return Err(failure("writePassword", format!("status {result}")));
            }
            let saved: String = proxy
                .call("readPassword", &(handle, FOLDER, key, APPLICATION))
                .map_err(|e| failure("verify", e))?;
            if saved != secret {
                return Err(failure("verify", "The saved password did not match"));
            }
            Ok(())
        })
    }
    fn delete(&self, key: &str) -> Result<(), SafeError> {
        self.with_wallet(|proxy, handle| {
            if !Self::has_folder(proxy, handle)? || !Self::has_entry(proxy, handle, key)? {
                return Ok(());
            }
            let result: i32 = proxy
                .call("removeEntry", &(handle, FOLDER, key, APPLICATION))
                .map_err(|e| failure("removeEntry", e))?;
            if result == 0 {
                Ok(())
            } else {
                Err(failure("removeEntry", format!("status {result}")))
            }
        })
    }
    fn keys(&self) -> Result<Vec<String>, SafeError> {
        self.with_wallet(|proxy, handle| {
            if !Self::has_folder(proxy, handle)? {
                return Ok(Vec::new());
            }
            let mut keys: Vec<String> = proxy
                .call("entryList", &(handle, FOLDER, APPLICATION))
                .map_err(|e| failure("entryList", e))?;
            keys.sort();
            keys.dedup();
            Ok(keys)
        })
    }
}
