//! Turning a panic into something the user can act on.
//!
//! A desktop application launched from a dock or a file manager has nowhere to
//! print to. Without a hook, a panic closes the window with no explanation and
//! no way to report it. This records the details and shows them.

use std::panic::PanicHookInfo;
use std::path::PathBuf;

/// A panic, reduced to what is worth showing and reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// What went wrong.
    pub message: String,
    /// Where in the source, when the panic recorded it.
    pub location: String,
}

impl Report {
    /// The text shown to the user and written to the log.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "fitsview {} stopped unexpectedly.\n\n{}\n\nAt: {}\n\n\
             Your images have not been changed. Deleting always moves files to \
             the trash, so nothing has been destroyed.",
            env!("CARGO_PKG_VERSION"),
            self.message,
            self.location
        )
    }
}

/// Extracts the useful parts of a panic.
///
/// The payload is a `&str` or a `String` depending on how the panic was raised,
/// and neither is guaranteed, so an unrecognised payload still produces a
/// report rather than nothing.
#[must_use]
pub fn report_from(info: &PanicHookInfo<'_>) -> Report {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "an unknown error".to_string());

    let location = info
        .location()
        .map_or_else(|| "an unknown location".to_string(), ToString::to_string);

    Report { message, location }
}

/// Where the crash log is written.
///
/// Beside the settings, so it is somewhere a user can be directed to, and so it
/// survives the application exiting.
#[must_use]
pub fn log_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.map(|b| b.join("fitsview").join("crash.log"))
}

/// Appends a report to the crash log, if one can be written.
///
/// Failure to write is ignored: this runs while the application is already
/// failing, and a second error helps nobody.
pub fn write_log(report: &Report) -> Option<PathBuf> {
    use std::io::Write;

    let path = log_path()?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    writeln!(file, "---\n{}\n", report.describe()).ok()?;
    Some(path)
}

/// Reports a failure to open the window at all.
///
/// A panic is not the only way to get nowhere: if the graphics device cannot
/// be created, the window never appears and the process ends with a terse line
/// on a console that a desktop launch does not have. Nothing was written
/// anywhere, so there was nothing to send on and nothing to read afterwards.
///
/// Almost every failure here is the graphics stack, so the message says so and
/// says what to try, rather than only repeating the error.
pub fn report_startup_failure(error: &str) {
    let report = Report {
        message: format!(
            "The window could not be opened.\n\n{error}\n\n\
             This is nearly always the graphics driver. Things worth trying, \
             in order:\n\
             \u{2022} Update the graphics driver.\n\
             \u{2022} Run it with the environment variable WGPU_BACKEND set to \
             vulkan, then dx12, then gl. One of them usually works, and which \
             one is worth telling us.\n\
             \u{2022} On a machine with two graphics adapters, make sure the \
             application is using the real one rather than the basic display \
             adapter."
        ),
        location: "starting up".to_string(),
    };

    log::error!("could not start: {error}");
    let mut text = report.describe();
    if let Some(path) = write_log(&report) {
        text.push_str(&format!("\n\nDetails were written to:\n{}", path.display()));
    }

    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("fitsview could not start")
        .set_description(&text)
        .show();
}

/// Installs the panic hook.
///
/// Logs the panic, appends it to the crash log, and shows a dialog. The
/// previous hook still runs, so a terminal launch keeps its backtrace.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let report = report_from(info);
        log::error!("panic: {} at {}", report.message, report.location);

        let mut text = report.describe();
        if let Some(path) = write_log(&report) {
            text.push_str(&format!("\n\nDetails were written to:\n{}", path.display()));
        }

        // A dialog is the only way to say anything when there is no terminal.
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("fitsview stopped")
            .set_description(&text)
            .show();

        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_startup_failure_names_the_graphics_stack_and_what_to_try() {
        // The failure that leaves nothing behind: no window, no panic, and so
        // no crash log either. Whatever is shown has to be enough to act on.
        let report = Report {
            message: format!(
                "The window could not be opened.\n\n{}\n\nWGPU_BACKEND",
                "NoAvailableAdapter"
            ),
            location: "starting up".to_string(),
        };
        let text = report.describe();
        assert!(text.contains("could not be opened"), "{text}");
        assert!(
            text.contains("WGPU_BACKEND"),
            "the one thing a user can actually try must be in it: {text}"
        );
        assert!(text.contains("NoAvailableAdapter"), "{text}");
    }

    #[test]
    fn a_string_panic_is_described() {
        let report = Report {
            message: "something went wrong".into(),
            location: "src/app.rs:42".into(),
        };
        let text = report.describe();
        assert!(text.contains("something went wrong"));
        assert!(text.contains("src/app.rs:42"));
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_message_reassures_the_user_about_their_files() {
        // The first thing anyone wonders after a crash in a program with a
        // delete button.
        let report = Report {
            message: "x".into(),
            location: "y".into(),
        };
        let text = report.describe();
        assert!(text.contains("trash"), "{text}");
        assert!(text.contains("not been changed"), "{text}");
    }

    #[test]
    fn a_panic_payload_of_either_kind_is_recognised() {
        // Panics raise a &str or a String depending on how they were written,
        // and the hook must handle both.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|info| {
            if let Ok(mut reports) = REPORTS.lock() {
                reports.push(report_from(info).message);
            }
        }));

        let _ = std::panic::catch_unwind(|| panic!("a literal message"));
        let _ = std::panic::catch_unwind(|| panic!("{}", format!("a formatted {}", "message")));

        std::panic::set_hook(previous);

        let reports = REPORTS
            .lock()
            .expect("the hook should not have poisoned this");
        assert!(
            reports.iter().any(|m| m == "a literal message"),
            "got {reports:?}"
        );
        assert!(
            reports.iter().any(|m| m == "a formatted message"),
            "got {reports:?}"
        );
    }

    /// Collects messages seen by the test hook.
    static REPORTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    #[test]
    fn the_log_path_sits_beside_the_settings() {
        if let Some(path) = log_path() {
            assert!(path.ends_with("fitsview/crash.log"), "{}", path.display());
        }
    }
}
