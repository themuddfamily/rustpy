//! Explicit Linux Secret Service acceptance on a Root-owned private session bus.
//! No ambient desktop bus, existing user entries, fake backend or fallback success.
#![cfg(target_os = "linux")]
use rustpy_secrets::{Backend, NativeKeyring, PasswordStorage, Safe, SafeConfig};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use zbus::blocking::{Connection, Proxy};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} admission required"))
}
fn expected_pid(name: &str) -> u32 {
    let pid = required(name)
        .parse::<u32>()
        .expect("admitted PID must be numeric");
    assert!(pid > 1);
    pid
}
fn private_bus(root: &Path) -> Connection {
    let address = required("DBUS_SESSION_BUS_ADDRESS");
    assert_eq!(
        address,
        required("RUSTPY_TEST_PRIVATE_DBUS_ADDRESS"),
        "ambient bus must not substitute the explicitly admitted endpoint"
    );
    assert!(
        !address.contains(';'),
        "exactly one private bus endpoint required"
    );
    let socket = address
        .strip_prefix("unix:path=")
        .expect("a private pathname UNIX socket is required")
        .split(',')
        .next()
        .unwrap();
    assert!(
        !socket.contains('%'),
        "use an unescaped private socket pathname for this fixture"
    );
    let socket = Path::new(socket);
    assert!(socket.is_absolute());
    let resolved = socket
        .canonicalize()
        .expect("private bus socket must exist");
    assert!(
        resolved.starts_with(root) && resolved != root,
        "refuse a bus socket outside the owned root"
    );
    let metadata = std::fs::metadata(&resolved).unwrap();
    assert!(metadata.file_type().is_socket());
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    assert_eq!(
        metadata.uid(),
        uid,
        "private bus socket must belong to fixture user"
    );
    let connection = Connection::session().expect("connect only to admitted private session bus");
    let bus = Proxy::new(
        &connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .unwrap();
    let bus_pid: u32 = bus
        .call("GetConnectionUnixProcessID", &("org.freedesktop.DBus",))
        .unwrap();
    assert_eq!(bus_pid, expected_pid("RUSTPY_TEST_DBUS_PID"));
    let owner: String = bus
        .call("GetNameOwner", &("org.freedesktop.secrets",))
        .expect("admitted Secret Service must already own its name");
    let provider_pid: u32 = bus
        .call("GetConnectionUnixProcessID", &(owner.as_str(),))
        .unwrap();
    assert_eq!(provider_pid, expected_pid("RUSTPY_TEST_SECRET_SERVICE_PID"));
    assert_eq!(
        std::fs::metadata(format!("/proc/{provider_pid}"))
            .unwrap()
            .uid(),
        uid
    );
    let service = Proxy::new(
        &connection,
        "org.freedesktop.secrets",
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
    )
    .unwrap();
    let collection: zbus::zvariant::OwnedObjectPath =
        service.call("ReadAlias", &("default",)).unwrap();
    assert_ne!(
        collection.as_str(),
        "/",
        "provider needs an owned default collection"
    );
    let collection = Proxy::new(
        &connection,
        "org.freedesktop.secrets",
        collection.as_str(),
        "org.freedesktop.Secret.Collection",
    )
    .unwrap();
    assert!(
        !collection.get_property::<bool>("Locked").unwrap(),
        "Root must unlock only the fixture collection before execution"
    );
    drop(collection);
    drop(service);
    drop(bus);
    connection
}
struct Cleanup {
    backend: Arc<dyn Backend>,
    keys: Vec<String>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        for key in &self.keys {
            let _ = self.backend.delete(key);
        }
    }
}
fn no_fallback(config: &SafeConfig) {
    assert!(
        !config.database.exists(),
        "native acceptance must not create fallback database"
    );
    assert!(
        !config.key_file.exists(),
        "native acceptance must not create fallback encryption key"
    );
}
fn native_status(safe: &Safe) {
    let status = safe.status();
    assert!(status.location.starts_with("Secret Service keyring"));
    assert!(status.problem.is_none() && !status.locked);
}

#[test]
#[ignore = "requires explicitly admitted private D-Bus and unlocked real Secret Service; Linux only"]
fn configured_private_secret_service_native_roundtrip_delete_without_fallback() {
    assert_eq!(required("RUSTPY_NATIVE_KEYRING_ADMITTED"), "1");
    let root = PathBuf::from(required("RUSTPY_TEST_NATIVE_KEYRING_ROOT"));
    assert!(root.is_absolute() && root.is_dir());
    let root = root.canonicalize().unwrap();
    assert_ne!(root, Path::new("/"));
    let metadata = std::fs::metadata(&root).unwrap();
    assert_eq!(
        metadata.permissions().mode() & 0o077,
        0,
        "private fixture root must deny group/other access"
    );
    assert_eq!(
        metadata.uid(),
        std::fs::metadata("/proc/self").unwrap().uid()
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".rustpy-secret-service-acceptance"))
            .expect("Root-owned private-bus admission marker required")
            .trim(),
        "owned-private-bus"
    );
    let _bus = private_bus(&root);
    let directory = tempfile::Builder::new()
        .prefix("native-safe-")
        .tempdir_in(&root)
        .unwrap();
    let config = SafeConfig {
        storage: PasswordStorage::Native,
        database: directory.path().join("must-not-exist.db"),
        key_file: directory.path().join("must-not-exist.key"),
    };
    let backend = NativeKeyring::platform()
        .backend()
        .expect("actual platform connection must succeed without fallback");
    assert!(
        backend.name().starts_with("Secret Service keyring"),
        "must use actual Linux Secret Service, not another backend"
    );
    assert!(
        backend.keys().unwrap().is_empty(),
        "refuse touching a provider containing preexisting RustPy entries"
    );
    let namespace = directory.path().file_name().unwrap().to_str().unwrap();
    let first = format!("acceptance/{namespace}/safe-written");
    let second = format!("acceptance/{namespace}/backend-written");
    let _cleanup = Cleanup {
        backend: backend.clone(),
        keys: vec![first.clone(), second.clone()],
    };
    let safe = Safe::new(config.clone());
    let first_secret = format!("rustpy-fixture-only-{namespace}-λ-initial");
    let updated_secret = format!("rustpy-fixture-only-{namespace}-λ-updated");
    safe.set(&first, &first_secret)
        .expect("actual native write");
    native_status(&safe);
    no_fallback(&config);
    // A fresh native connection is independent of Safe's process/session state.
    let fresh = NativeKeyring::platform().backend().unwrap();
    assert!(
        fresh.get(&first).unwrap().as_deref() == Some(first_secret.as_str()),
        "fresh actual native backend must read the Safe write"
    );
    backend
        .set(&second, "rustpy-fixture-only-direct-native")
        .unwrap();
    let reopened = Safe::new(config.clone());
    assert!(
        reopened.get(&second).unwrap().as_deref() == Some("rustpy-fixture-only-direct-native"),
        "reopened Safe must consume actual native storage"
    );
    reopened.set(&first, &updated_secret).unwrap();
    assert!(
        fresh.get(&first).unwrap().as_deref() == Some(updated_secret.as_str()),
        "native replacement must persist across connections"
    );
    assert!(reopened.keys().unwrap().contains(&first));
    assert!(reopened.keys().unwrap().contains(&second));
    native_status(&reopened);
    no_fallback(&config);
    reopened.delete(&first).unwrap();
    reopened.delete(&first).unwrap();
    assert!(
        fresh.get(&first).unwrap().is_none(),
        "actual native deletion must be visible across connections"
    );
    safe.delete(&second).unwrap();
    assert!(fresh.get(&second).unwrap().is_none());
    assert!(backend.keys().unwrap().is_empty());
    native_status(&safe);
    no_fallback(&config);
}
