//! Linux/BSD keyring: the freedesktop Secret Service API over D-Bus (GNOME
//! Keyring, KWallet 5.97+, KeePassXC, …).
//!
//! Items live in the default collection with the attributes
//! `{application: rustpy, key: <key>}` and the label `RustPy: <key>`. Secrets
//! travel over the session bus with the `plain` algorithm (the bus is local to
//! the user). A locked collection or item is unlocked through the provider's
//! prompt; prompts that get no answer time out.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::{Backend, SafeError};

const DEST: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const PROMPT: &str = "org.freedesktop.Secret.Prompt";
/// How long to wait for the user to answer an unlock prompt.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const APPLICATION: &str = "rustpy";

fn fail(what: &str, e: impl std::fmt::Display) -> SafeError {
    SafeError::Failed(format!("Secret Service: {what}: {e}"))
}

/// A connection to the Secret Service.
pub struct SecretService {
    conn: Connection,
}

impl SecretService {
    /// Connects to the session bus and checks that a Secret Service provider is
    /// running or can be started.
    pub fn connect() -> Result<SecretService, String> {
        let conn = Connection::session().map_err(|e| format!("no D-Bus session bus ({e})"))?;
        let dbus = Proxy::new(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .map_err(|e| e.to_string())?;
        let running: bool = dbus
            .call("NameHasOwner", &(DEST,))
            .map_err(|e| e.to_string())?;
        if !running {
            let activatable: Vec<String> = dbus
                .call("ListActivatableNames", &())
                .map_err(|e| e.to_string())?;
            if !activatable.iter().any(|n| n == DEST) {
                return Err(
                    "no Secret Service provider (GNOME Keyring, KWallet, KeePassXC) is running"
                        .into(),
                );
            }
        }
        let ss = SecretService { conn };
        let default = ss.default_collection().map_err(|e| e.to_string())?;
        if default.as_str() == "/" {
            return Err("the keyring has no default collection".into());
        }
        Ok(ss)
    }

    fn proxy<'p>(
        &self,
        path: impl TryInto<ObjectPath<'p>, Error = zbus::zvariant::Error>,
        iface: &'static str,
    ) -> Result<Proxy<'p>, SafeError> {
        let path = path.try_into().map_err(|e| fail("bad object path", e))?;
        zbus::blocking::proxy::Builder::new(&self.conn)
            .destination(DEST)
            .and_then(|b| b.path(path))
            .and_then(|b| b.interface(iface))
            .map(|b| b.cache_properties(CacheProperties::No))
            .and_then(|b| b.build())
            .map_err(|e| fail("proxy", e))
    }

    fn service(&self) -> Result<Proxy<'static>, SafeError> {
        self.proxy(SERVICE_PATH, SERVICE)
    }

    fn default_collection(&self) -> Result<OwnedObjectPath, SafeError> {
        self.service()?
            .call("ReadAlias", &("default",))
            .map_err(|e| fail("ReadAlias", e))
    }

    fn open_session(&self) -> Result<OwnedObjectPath, SafeError> {
        let (_, session): (OwnedValue, OwnedObjectPath) = self
            .service()?
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(|e| fail("OpenSession", e))?;
        Ok(session)
    }

    fn close_session(&self, session: &OwnedObjectPath) {
        if let Ok(p) = self.proxy(
            session.as_str().to_string(),
            "org.freedesktop.Secret.Session",
        ) {
            let _ = p.call_method("Close", &());
        }
    }

    /// Runs a prompt and waits for `Completed`; Ok(false) when dismissed.
    fn prompt(&self, prompt: &OwnedObjectPath) -> Result<bool, SafeError> {
        if prompt.as_str() == "/" {
            return Ok(true);
        }
        let proxy = self.proxy(prompt.as_str().to_string(), PROMPT)?;
        // Subscribe before starting the prompt so the signal cannot be missed.
        let mut signals = proxy
            .receive_signal("Completed")
            .map_err(|e| fail("prompt", e))?;
        proxy
            .call_method("Prompt", &("",))
            .map_err(|e| fail("prompt", e))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Some(msg) = signals.next() {
                let _ = tx.send(
                    msg.body()
                        .deserialize::<(bool, OwnedValue)>()
                        .map(|(dismissed, _)| dismissed),
                );
            }
        });
        match rx.recv_timeout(PROMPT_TIMEOUT) {
            Ok(Ok(dismissed)) => Ok(!dismissed),
            Ok(Err(e)) => Err(fail("prompt", e)),
            Err(_) => Err(SafeError::Failed(
                "Secret Service: the keyring unlock prompt got no answer".into(),
            )),
        }
    }

    fn unlock(&self, objects: &[OwnedObjectPath]) -> Result<(), SafeError> {
        if objects.is_empty() {
            return Ok(());
        }
        let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = self
            .service()?
            .call("Unlock", &(objects,))
            .map_err(|e| fail("Unlock", e))?;
        if !self.prompt(&prompt)? {
            return Err(SafeError::Failed("the keyring was not unlocked".into()));
        }
        Ok(())
    }

    fn search(&self, attributes: &HashMap<&str, &str>) -> Result<Vec<OwnedObjectPath>, SafeError> {
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = self
            .service()?
            .call("SearchItems", &(attributes,))
            .map_err(|e| fail("SearchItems", e))?;
        self.unlock(&locked)?;
        Ok(unlocked.into_iter().chain(locked).collect())
    }

    fn attributes(key: &str) -> HashMap<&str, &str> {
        HashMap::from([("application", APPLICATION), ("key", key)])
    }
}

impl Backend for SecretService {
    fn name(&self) -> String {
        match self.collection_label().filter(|l| !l.trim().is_empty()) {
            Some(label) => format!("Secret Service keyring (collection \"{}\")", label.trim()),
            None => "Secret Service keyring".into(),
        }
    }

    fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
        let items = self.search(&Self::attributes(key))?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let session = self.open_session()?;
        let result: Result<(OwnedObjectPath, Vec<u8>, Vec<u8>, String), _> = self
            .proxy(item.as_str().to_string(), ITEM)?
            .call("GetSecret", &(&session,));
        self.close_session(&session);
        let (_, _, value, _) = result.map_err(|e| fail("GetSecret", e))?;
        String::from_utf8(value)
            .map(Some)
            .map_err(|_| SafeError::Failed("Secret Service: the stored secret is not text".into()))
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
        let collection = self.default_collection()?;
        if collection.as_str() == "/" {
            return Err(SafeError::Unavailable(
                "the keyring has no default collection".into(),
            ));
        }
        let coll = self.proxy(collection.as_str().to_string(), COLLECTION)?;
        if coll.get_property::<bool>("Locked").unwrap_or(false) {
            self.unlock(std::slice::from_ref(&collection))?;
        }
        let label = format!("RustPy: {key}");
        let attrs = Self::attributes(key);
        let properties: HashMap<&str, Value> = HashMap::from([
            (
                "org.freedesktop.Secret.Item.Label",
                Value::from(label.as_str()),
            ),
            ("org.freedesktop.Secret.Item.Attributes", Value::from(attrs)),
        ]);
        let session = self.open_session()?;
        let secret_struct = (
            session.clone(),
            Vec::<u8>::new(),
            secret.as_bytes().to_vec(),
            "text/plain; charset=utf8",
        );
        let result: Result<(OwnedObjectPath, OwnedObjectPath), _> =
            coll.call("CreateItem", &(properties, secret_struct, true));
        self.close_session(&session);
        let (_, prompt) = result.map_err(|e| fail("CreateItem", e))?;
        if !self.prompt(&prompt)? {
            return Err(SafeError::Failed(
                "storing the password in the keyring was cancelled".into(),
            ));
        }
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SafeError> {
        for item in self.search(&Self::attributes(key))? {
            let prompt: OwnedObjectPath = self
                .proxy(item.as_str().to_string(), ITEM)?
                .call("Delete", &())
                .map_err(|e| fail("Delete", e))?;
            self.prompt(&prompt)?;
        }
        Ok(())
    }

    fn keys(&self) -> Result<Vec<String>, SafeError> {
        let items = self.search(&HashMap::from([("application", APPLICATION)]))?;
        let mut keys = Vec::new();
        for item in items {
            let attrs: HashMap<String, String> = self
                .proxy(item.as_str().to_string(), ITEM)?
                .get_property("Attributes")
                .map_err(|e| fail("Attributes", e))?;
            if let Some(k) = attrs.get("key") {
                keys.push(k.clone());
            }
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }
}

impl SecretService {
    /// The default collection's label (e.g. "Login"), for status lines.
    pub fn collection_label(&self) -> Option<String> {
        let path = self.default_collection().ok()?;
        self.proxy(path.as_str().to_string(), COLLECTION)
            .ok()?
            .get_property::<String>("Label")
            .ok()
    }
}
