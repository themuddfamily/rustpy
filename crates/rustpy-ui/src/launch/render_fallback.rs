//! Software rendering: forced by `--software-rendering` or Settings ›
//! Appearance › "Use software rendering", and used automatically once when the
//! GPU's OpenGL context cannot be created at startup. winit allows a single
//! event loop per process, so the retry starts this program again with the
//! same arguments and a guard variable that stops a second retry.

use std::ffi::OsString;

/// Set in the restarted process: the retry already happened (holds the reason).
pub(super) const RETRY_GUARD_ENV: &str = "RUSTPY_SOFTWARE_RENDERING_RETRY";

/// Why the IDE draws without the GPU (shown in a notification after startup).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SoftwareReason {
    /// `--software-rendering`.
    Flag,
    /// Settings › Appearance › Use software rendering.
    Setting,
    /// The GPU context failed in the first process; its error.
    Fallback(String),
}

impl SoftwareReason {
    /// The notification body.
    pub(crate) fn message(&self) -> String {
        let how = match self {
            SoftwareReason::Flag => "it was requested with --software-rendering".to_string(),
            SoftwareReason::Setting => {
                "Settings › Appearance › Use software rendering is on".to_string()
            }
            SoftwareReason::Fallback(error) => {
                format!("the graphics driver could not create an OpenGL context ({error})")
            }
        };
        format!(
            "RustPy draws with the CPU because {how}. Scrolling and animations may be slower; \
             update the graphics driver, then turn Use software rendering off and restart."
        )
    }
}

/// The renderer of this process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RenderMode {
    Hardware,
    Software(SoftwareReason),
}

impl RenderMode {
    /// The mode for the command-line flag, the setting and the environment
    /// (`env` reads a variable; injected by tests).
    pub(super) fn select(
        flag: bool,
        setting: bool,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> RenderMode {
        if let Some(reason) = env(RETRY_GUARD_ENV) {
            let reason = reason.to_string_lossy().trim().to_string();
            return RenderMode::Software(SoftwareReason::Fallback(if reason.is_empty() {
                "unknown error".into()
            } else {
                reason
            }));
        }
        if flag {
            RenderMode::Software(SoftwareReason::Flag)
        } else if setting {
            RenderMode::Software(SoftwareReason::Setting)
        } else {
            RenderMode::Hardware
        }
    }

    pub(super) fn software_reason(&self) -> Option<&SoftwareReason> {
        match self {
            RenderMode::Hardware => None,
            RenderMode::Software(reason) => Some(reason),
        }
    }

    /// Applies the mode to the native options and, for software rendering,
    /// to this process's environment before any OpenGL library is loaded.
    pub(super) fn apply(&self, options: &mut eframe::NativeOptions) {
        if self.software_reason().is_none() {
            return;
        }
        // A config without hardware acceleration (EGL/WGL/CGL honour it).
        options.glow_options.hardware_acceleration = eframe::egui_glow::HardwareAcceleration::Off;
        // Mesa: llvmpipe instead of the GPU driver, for GLX and EGL alike.
        #[cfg(all(unix, not(target_os = "macos")))]
        if std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_none() {
            std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
        }
    }
}

/// How starting the native window failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StartFailure {
    /// The OpenGL display, config, context or painter: software rendering may work.
    Gpu(String),
    /// Anything else (no display server, the IDE's own startup).
    Other(String),
}

impl StartFailure {
    pub(super) fn classify(error: &eframe::Error) -> StartFailure {
        let text = error.to_string();
        match error {
            eframe::Error::Glutin(_)
            | eframe::Error::NoGlutinConfigs(..)
            | eframe::Error::OpenGL(_) => StartFailure::Gpu(text),
            _ => StartFailure::Other(text),
        }
    }

    fn text(&self) -> &str {
        match self {
            StartFailure::Gpu(text) | StartFailure::Other(text) => text,
        }
    }
}

/// Whether to start again with software rendering after `failure`: only for
/// a GPU failure before the IDE window was created (`app_started` false; a
/// retry would otherwise open the project twice), and only once (`mode` is
/// software in the restarted process and when software rendering was forced).
pub(super) fn should_retry(failure: &StartFailure, app_started: bool, mode: &RenderMode) -> bool {
    matches!(failure, StartFailure::Gpu(_)) && !app_started && *mode == RenderMode::Hardware
}

/// Runs the window (`run` reports its failure) and retries once in software
/// mode through `restart`, which receives the reason and returns the exit code
/// of the restarted process.
pub(super) fn run_with_fallback(
    mode: &RenderMode,
    app_started: impl Fn() -> bool,
    run: impl FnOnce() -> Result<(), StartFailure>,
    restart: impl FnOnce(&str) -> std::io::Result<i32>,
) -> anyhow::Result<i32> {
    let failure = match run() {
        Ok(()) => return Ok(0),
        Err(failure) => failure,
    };
    if !should_retry(&failure, app_started(), mode) {
        return Err(anyhow::anyhow!("{}", failure.text()));
    }
    log::warn!(
        "the GPU OpenGL context failed ({}); restarting with software rendering",
        failure.text()
    );
    eprintln!(
        "rustpy: the GPU OpenGL context failed ({}); retrying with software rendering",
        failure.text()
    );
    restart(failure.text()).map_err(|e| {
        anyhow::anyhow!(
            "{} (restarting with software rendering failed: {e})",
            failure.text()
        )
    })
}

/// Starts this program again with the same arguments in software mode and
/// waits for it (its exit status becomes this process's).
pub(super) fn restart_with_software_rendering(reason: &str) -> std::io::Result<i32> {
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command
        .args(std::env::args_os().skip(1))
        .env(RETRY_GUARD_ENV, retry_reason(reason));
    #[cfg(all(unix, not(target_os = "macos")))]
    command.env("LIBGL_ALWAYS_SOFTWARE", "1");
    let status = command.status()?;
    Ok(status.code().unwrap_or(1))
}

/// The reason passed to the restarted process: one line, bounded, never empty.
fn retry_reason(reason: &str) -> String {
    let line: String = reason
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect();
    if line.is_empty() {
        "unknown error".into()
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn no_env(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn flag_setting_and_retry_guard_select_software_rendering() {
        assert_eq!(
            RenderMode::select(false, false, no_env),
            RenderMode::Hardware
        );
        assert_eq!(
            RenderMode::select(true, true, no_env),
            RenderMode::Software(SoftwareReason::Flag)
        );
        assert_eq!(
            RenderMode::select(false, true, no_env),
            RenderMode::Software(SoftwareReason::Setting)
        );
        let guard = |name: &str| (name == RETRY_GUARD_ENV).then(|| OsString::from("no EGL config"));
        assert_eq!(
            RenderMode::select(false, false, guard),
            RenderMode::Software(SoftwareReason::Fallback("no EGL config".into()))
        );
        let empty = |name: &str| (name == RETRY_GUARD_ENV).then(OsString::new);
        assert_eq!(
            RenderMode::select(false, false, empty),
            RenderMode::Software(SoftwareReason::Fallback("unknown error".into()))
        );
    }

    #[test]
    fn only_an_early_gpu_failure_in_hardware_mode_retries() {
        let gpu = StartFailure::Gpu("context creation failed".into());
        let other = StartFailure::Other("no display".into());
        let hardware = RenderMode::Hardware;
        assert!(should_retry(&gpu, false, &hardware));
        assert!(
            !should_retry(&gpu, true, &hardware),
            "the IDE already started"
        );
        assert!(!should_retry(&other, false, &hardware));
        for reason in [
            SoftwareReason::Flag,
            SoftwareReason::Setting,
            SoftwareReason::Fallback("x".into()),
        ] {
            assert!(
                !should_retry(&gpu, false, &RenderMode::Software(reason)),
                "software rendering never retries (no restart loop)"
            );
        }
    }

    #[test]
    fn injected_gpu_failure_restarts_once_with_the_reason() {
        let restarts = Cell::new(0);
        let code = run_with_fallback(
            &RenderMode::Hardware,
            || false,
            || Err(StartFailure::Gpu("GL 3.0 unsupported".into())),
            |reason| {
                restarts.set(restarts.get() + 1);
                assert_eq!(reason, "GL 3.0 unsupported");
                Ok(7)
            },
        )
        .unwrap();
        assert_eq!((code, restarts.get()), (7, 1));

        // The restarted process (guard set) reports the failure instead.
        let mode = RenderMode::select(false, false, |n| {
            (n == RETRY_GUARD_ENV).then(|| "GL 3.0 unsupported".into())
        });
        let err = run_with_fallback(
            &mode,
            || false,
            || Err(StartFailure::Gpu("still failing".into())),
            |_| panic!("no second restart"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("still failing"), "{err}");

        // Success and other failures never restart.
        let ok = run_with_fallback(
            &RenderMode::Hardware,
            || false,
            || Ok(()),
            |_| panic!("no restart"),
        );
        assert_eq!(ok.unwrap(), 0);
        let err = run_with_fallback(
            &RenderMode::Hardware,
            || false,
            || Err(StartFailure::Other("no display".into())),
            |_| panic!("no restart"),
        );
        assert!(err.is_err());
        let err = run_with_fallback(
            &RenderMode::Hardware,
            || true,
            || Err(StartFailure::Gpu("lost context".into())),
            |_| panic!("no restart after the IDE started"),
        );
        assert!(err.is_err());
    }

    #[test]
    fn a_failed_restart_keeps_the_original_error() {
        let err = run_with_fallback(
            &RenderMode::Hardware,
            || false,
            || Err(StartFailure::Gpu("bad driver".into())),
            |_| Err(std::io::Error::other("exec failed")),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("bad driver") && err.contains("exec failed"),
            "{err}"
        );
    }

    #[test]
    fn app_creation_errors_are_not_gpu_failures() {
        let error = eframe::Error::AppCreation("startup failed".into());
        assert!(matches!(
            StartFailure::classify(&error),
            StartFailure::Other(_)
        ));
    }

    #[test]
    fn restart_reason_is_one_bounded_line() {
        assert_eq!(retry_reason("a\n  b\tc"), "a b c");
        assert_eq!(retry_reason("   "), "unknown error");
        assert_eq!(retry_reason(&"x".repeat(1000)).len(), 300);
    }

    #[test]
    fn notification_names_the_reason() {
        assert!(SoftwareReason::Flag
            .message()
            .contains("--software-rendering"));
        assert!(SoftwareReason::Setting
            .message()
            .contains("Use software rendering"));
        assert!(SoftwareReason::Fallback("no GLX visual".into())
            .message()
            .contains("no GLX visual"));
    }
}
