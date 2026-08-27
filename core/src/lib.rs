//! Shared library for Capture to Search: paths, config, IPC, autostart,
//! screen capture backends, and the Google Lens upload.
//!
//! Deliberately free of any GUI toolkit dependency so the daemon (which owns
//! the tray, capture, upload, and browser launch) builds and runs on hosts with
//! no GTK at all. The GUI crate is an optional front end over this.

pub mod autostart;
pub mod capture;
pub mod config;
pub mod ipc;
pub mod lens;
pub mod paths;

pub use config::Config;

/// Reverse-DNS application ID: tray id, GTK application id, icon name, and the
/// basename of the `.desktop` files.
pub const APP_ID: &str = "io.github.dipakmdhrm.CaptureToSearch";

/// Human-readable application name, shown in the tray tooltip and About dialog.
pub const APP_NAME: &str = "Capture to Search";

/// Short name used for config/runtime directories and the autostart entry.
pub const APP_SLUG: &str = "capture-to-search";

/// Install the process-wide rustls crypto provider.
///
/// We build reqwest with `rustls-no-provider`, so exactly one provider must be
/// installed before the first TLS handshake. Idempotent: a second call is a
/// no-op rather than an error, so both the daemon and a one-shot `capture` run
/// can call it unconditionally at startup.
pub fn install_default_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// The `--version` blurb shared by every entry point.
pub fn version_blurb(bin: &str) -> String {
    format!("{bin} {} ({APP_NAME})", env!("CARGO_PKG_VERSION"))
}
