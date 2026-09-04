//! Binary entry point.
//!
//! Deliberately thin: it parses the command line, starts logging, and hands
//! over to [`fitsview::ui::FitsViewApp`]. Everything worth testing lives in the
//! library alongside it.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use fitsview::adapter::{self, Candidate, Kind};
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

/// Chooses the graphics adapter, preferring one that can draw to the window
/// over one that is merely fast.
///
/// See [`fitsview::adapter`] for why: the default choice takes the most
/// powerful adapter and gives up if it cannot present, which over Remote
/// Desktop means giving up entirely.
fn presentable_adapter_setup() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::wgpu;

    let mut setup = match eframe::egui_wgpu::WgpuConfiguration::default().wgpu_setup {
        eframe::egui_wgpu::WgpuSetup::CreateNew(setup) => setup,
        other => return other,
    };

    setup.native_adapter_selector = Some(std::sync::Arc::new(|adapters, surface| {
        let candidates: Vec<Candidate> = adapters
            .iter()
            .map(|a| {
                let info = a.get_info();
                Candidate {
                    kind: match info.device_type {
                        wgpu::DeviceType::DiscreteGpu => Kind::DiscreteGpu,
                        wgpu::DeviceType::IntegratedGpu => Kind::IntegratedGpu,
                        wgpu::DeviceType::VirtualGpu => Kind::VirtualGpu,
                        wgpu::DeviceType::Cpu => Kind::Cpu,
                        wgpu::DeviceType::Other => Kind::Other,
                    },
                    // With no surface to check against there is nothing to
                    // rule an adapter out, so all of them stay in the running.
                    presents: surface.is_none_or(|s| a.is_surface_supported(s)),
                }
            })
            .collect();

        for (adapter, candidate) in adapters.iter().zip(&candidates) {
            let info = adapter.get_info();
            log::info!(
                "adapter {} ({:?}, {:?}) {} draw to this window",
                info.name,
                info.backend,
                info.device_type,
                if candidate.presents { "can" } else { "cannot" }
            );
        }

        match adapter::choose(&candidates) {
            Some(index) => {
                let chosen = adapters[index].clone();
                log::info!("drawing with {}", chosen.get_info().name);
                Ok(chosen)
            }
            None => Err(format!(
                "none of the {} graphics adapters found can draw to a window. \
                 Over Remote Desktop this is usual: the graphics card cannot \
                 present to a remote session. Try running with the environment \
                 variable WGPU_BACKEND set to dx12, which uses Microsoft's \
                 software renderer.",
                adapters.len()
            )),
        }
    }));

    setup.into()
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
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: presentable_adapter_setup(),
            ..Default::default()
        },
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

    let started = eframe::run_native(
        "fitsview",
        options,
        Box::new(move |cc| Ok(Box::new(FitsViewApp::with_storage(initial, cc.storage)))),
    );

    // Returning the error would print one line to a console that a desktop
    // launch does not have, and write nothing anywhere. Failing to open the
    // window is the one failure a user cannot work around without being told
    // something.
    if let Err(error) = &started {
        crash::report_startup_failure(&error.to_string());
    }
    started
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
            "Show or hide the image metadata",
        ] {
            assert!(text.contains(expected), "usage is missing {expected}");
        }
        assert!(text.contains("Usage: fitsview"));
        assert!(text.contains("--version"));
    }
}
