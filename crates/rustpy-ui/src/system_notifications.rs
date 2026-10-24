//! Desktop ("system") notifications for Settings › Appearance & Behavior ›
//! Notifications › Enable system notifications: sent while the IDE window is
//! not focused. On Linux they go to the freedesktop Notifications service over
//! D-Bus, falling back to `notify-send`; elsewhere this is a no-op. Failures
//! are silent: the balloon and the Notifications tool window still have it.

/// Sends a desktop notification in the background (never blocks the UI).
pub fn send(title: &str, body: &str) {
    platform::send(title.to_string(), body.to_string());
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    use zbus::blocking::Connection;
    use zbus::zvariant::Value;

    const APP_NAME: &str = "RustPy";
    /// Milliseconds the desktop shows it (-1 = the server's default).
    const EXPIRE_TIMEOUT: i32 = -1;

    pub fn send(title: String, body: String) {
        let _ = std::thread::Builder::new()
            .name("system-notification".into())
            .spawn(move || {
                if dbus_notify(&title, &body).is_err() {
                    notify_send(&title, &body);
                }
            });
    }

    /// The session bus, connected once (None when there is none).
    fn connection() -> Option<Connection> {
        static CONNECTION: OnceLock<Mutex<Option<Connection>>> = OnceLock::new();
        let slot = CONNECTION.get_or_init(|| Mutex::new(None));
        let mut slot = slot.lock().ok()?;
        if slot.is_none() {
            *slot = Connection::session().ok();
        }
        slot.clone()
    }

    /// `org.freedesktop.Notifications.Notify` (Desktop Notifications Specification).
    fn dbus_notify(title: &str, body: &str) -> Result<(), ()> {
        let conn = connection().ok_or(())?;
        let hints: HashMap<&str, Value<'_>> = HashMap::new();
        conn.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                APP_NAME,
                0u32,
                "",
                title,
                body,
                Vec::<&str>::new(),
                hints,
                EXPIRE_TIMEOUT,
            ),
        )
        .map(|_| ())
        .map_err(|_| ())
    }

    /// `notify-send --app-name=RustPy TITLE BODY`; nothing happens when it is missing.
    fn notify_send(title: &str, body: &str) {
        let _ = std::process::Command::new("notify-send")
            .arg(format!("--app-name={APP_NAME}"))
            .arg("--")
            .arg(title)
            .arg(body)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    pub fn send(_title: String, _body: String) {}
}
