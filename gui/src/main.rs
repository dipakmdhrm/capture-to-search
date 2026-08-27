//! capture-to-search-gui - the Capture to Search window.
//!
//! A deliberately tiny GTK4 + libadwaita front end: one Capture button, plus
//! Preferences and About. It is spawned on demand by the daemon and terminated
//! when closed, so nothing GTK-sized stays resident while the app sits in the
//! tray.
//!
//! It owns no capture logic. Pressing Capture sends a request to the daemon,
//! which dismisses this window before taking the shot - a window that captured
//! its own screen would photograph itself.

mod app;
mod daemon_link;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use capture_core::APP_ID;

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", capture_core::version_blurb("capture-to-search-gui"));
        return glib::ExitCode::SUCCESS;
    }

    // NON_UNIQUE: the daemon arbitrates single-instance via its socket, not
    // GApplication. Two windows can never exist because the daemon only ever
    // spawns one child.
    let application = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    application.connect_activate(app::build);

    // argv is already consumed above; don't let GApplication reparse it.
    let no_args: [&str; 0] = [];
    application.run_with_args(&no_args)
}
