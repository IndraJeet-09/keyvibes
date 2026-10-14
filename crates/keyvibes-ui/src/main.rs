//! KeyVibes desktop control surface.
//!
//! A native GTK4 + libadwaita window that drives the running engine over the
//! local control protocol. It owns no audio, no input device and no pack
//! cache: everything it shows is something the engine reports, and
//! everything it does is a control request.

mod autostart;
mod packs;
mod screenshot;
mod session;
mod style;
mod tray;
mod window;

use adw::prelude::*;

/// Reverse-DNS application id: the D-Bus name, the desktop-file name the
/// icon theme looks for, and the single-instance key, all in one string.
pub const APP_ID: &str = window::APP_ID;

/// The version shown in the about dialog, taken from the crate metadata so
/// it can never disagree with the release tag.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the source and the issue tracker live.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

fn main() -> adw::glib::ExitCode {
    let application = adw::Application::builder()
        .application_id(window::APP_ID)
        .build();

    application.connect_startup(|_| style::install());
    // Single-instance by application id: a second launch presents the
    // existing window rather than starting a second control surface.
    application.connect_activate(window::activate);

    application.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_about_dialog_reads_the_crate_metadata() {
        assert!(!VERSION.is_empty());
        assert!(REPOSITORY.starts_with("http"), "the link must be usable");
    }
}
