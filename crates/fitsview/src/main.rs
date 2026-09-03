//! Binary entry point.
//!
//! Deliberately thin: it parses the command line, starts logging, and hands
//! over to [`fitsview::ui::FitsViewApp`]. Everything worth testing lives in the
//! library alongside it.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use fitsview::{crash, icon, shortcuts, ui::FitsViewApp, version_string};

/// What the command line asked for.
#[derive(Debug, PartialEq)]
enum Invocation {
    /// Print the version and exit.
    Version,
    /// Print usage and exit.
    Help,
    /// Open the window, optionally with a file already loaded.
    Run(Option<PathBuf>),
}

/// Parses the command line.
///
/// Intentionally minimal: one optional path, plus the two flags every command
/// line tool is expected to answer. A real argument parser would be a
/// dependency earning its keep on two flags.
///
/// Flags win wherever they appear, so `fitsview image.fits --version` prints
/// the version rather than opening the window. Only the first path is used;
/// opening several files at once needs the folder model from a later phase.
fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let mut path = None;
    for arg in args {
        match arg.as_str() {
            "-V" | "--version" => return Invocation::Version,
            "-h" | "--help" => return Invocation::Help,
            other if other.starts_with('-') => return Invocation::Help,
            other if path.is_none() => path = Some(PathBuf::from(other)),
            // Extra paths are ignored rather than silently opening the last one.
            _ => {}
        }
    }
    Invocation::Run(path)
}

/// The usage message.
///
/// The key list comes from [`shortcuts`], so it cannot drift out of step with
/// the help overlay or with what the application actually does.
fn usage() -> String {
    format!(
        "Usage: fitsview [PATH]\n\
         \n\
         Opens a FITS image, or a folder of them, for viewing. With no PATH,\n\
         reopens the folder from last time.\n\
         \n\
         Options:\n\
         \x20 -h, --help       Show this message\n\
         \x20 -V, --version    Show the version\n\
         \n\
         Keys:\n\
         {}\n",
        shortcuts::as_text()
    )
}

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Launched from a dock or a file manager there is nowhere to print, so a
    // panic would otherwise close the window with no explanation.
    crash::install();

    let initial = match parse_args(std::env::args().skip(1)) {
        Invocation::Version => {
            println!("{}", version_string());
            return Ok(());
        }
        Invocation::Help => {
            print!("{}", usage());
            return Ok(());
        }
        Invocation::Run(path) => path,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(version_string())
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([600.0, 400.0])
            .with_drag_and_drop(true)
            .with_icon(egui::IconData {
                rgba: icon::rgba(),
                width: icon::SIZE as u32,
                height: icon::SIZE as u32,
            }),
        ..Default::default()
    };

    eframe::run_native(
        "fitsview",
        options,
        Box::new(move |cc| Ok(Box::new(FitsViewApp::with_storage(initial, cc.storage)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_arguments_starts_with_an_empty_window() {
        assert_eq!(parse_args(args(&[])), Invocation::Run(None));
    }

    #[test]
    fn a_path_is_opened_at_startup() {
        assert_eq!(
            parse_args(args(&["/tmp/a.fits"])),
            Invocation::Run(Some(PathBuf::from("/tmp/a.fits")))
        );
    }

    #[test]
    fn version_and_help_flags_are_recognised() {
        for f in ["-V", "--version"] {
            assert_eq!(parse_args(args(&[f])), Invocation::Version, "{f}");
        }
        for f in ["-h", "--help"] {
            assert_eq!(parse_args(args(&[f])), Invocation::Help, "{f}");
        }
    }

    #[test]
    fn an_unknown_flag_shows_usage_rather_than_being_treated_as_a_path() {
        assert_eq!(parse_args(args(&["--nonsense"])), Invocation::Help);
    }

    #[test]
    fn a_flag_after_a_path_still_wins() {
        // The obvious loop here returns on the first argument and silently
        // ignores everything after it. This is the regression test for that.
        assert_eq!(
            parse_args(args(&["/tmp/a.fits", "--version"])),
            Invocation::Version
        );
        assert_eq!(parse_args(args(&["/tmp/a.fits", "-h"])), Invocation::Help);
    }

    #[test]
    fn extra_paths_are_ignored_rather_than_replacing_the_first() {
        assert_eq!(
            parse_args(args(&["/tmp/a.fits", "/tmp/b.fits"])),
            Invocation::Run(Some(PathBuf::from("/tmp/a.fits")))
        );
    }

    #[test]
    fn the_usage_text_lists_every_key_the_viewer_responds_to() {
        // Generated from the shared list, so this checks the wiring rather than
        // a hand-maintained copy.
        let text = usage();
        for expected in [
            "Next file",
            "Toggle the automatic stretch",
            "Show the FITS header",
        ] {
            assert!(text.contains(expected), "usage is missing {expected}");
        }
        assert!(text.contains("Usage: fitsview"));
        assert!(text.contains("--version"));
    }
}
