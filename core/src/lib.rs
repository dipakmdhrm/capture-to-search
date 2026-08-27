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

#[cfg(test)]
mod packaging_tests {
    use super::*;

    /// Repo root, from this crate's manifest directory.
    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    fn read(rel: &str) -> String {
        let path = root().join(rel);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing packaging file {}: {e}", path.display()))
    }

    /// Every file that has to name the binaries or the app id.
    const PACKAGING_FILES: &[&str] = &[
        "packaging/deb/control.template",
        "packaging/deb/prerm",
        "packaging/deb/postrm",
        "packaging/rpm/capture-to-search.spec",
        "packaging/arch/PKGBUILD",
        "packaging/arch/capture-to-search.install",
        "packaging/build-local.sh",
        "install.sh",
        "uninstall.sh",
    ];

    #[test]
    fn packaging_installs_the_binaries_we_actually_build() {
        // Renaming a binary without updating packaging produces a package that
        // installs cleanly and then does nothing - the desktop entry points at
        // a path that does not exist. Nothing else catches this.
        let daemon = format!("{APP_SLUG}d");
        for file in [
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/PKGBUILD",
        ] {
            let text = read(file);
            assert!(text.contains(&daemon), "{file} does not install {daemon}");
            assert!(
                text.contains(&format!("{APP_SLUG}-gui")),
                "{file} does not install {APP_SLUG}-gui"
            );
        }
    }

    #[test]
    fn packaging_agrees_on_the_app_id() {
        // The app id is the icon name, the desktop file basename, and the tray
        // id. A package that installs icons under a different id shows a
        // missing-icon placeholder.
        for file in [
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/PKGBUILD",
            "packaging/build-local.sh",
            "install.sh",
            "uninstall.sh",
        ] {
            assert!(
                read(file).contains(APP_ID),
                "{file} does not reference the app id {APP_ID}"
            );
        }
    }

    #[test]
    fn package_names_match_the_slug() {
        assert!(
            read("packaging/deb/control.template").contains(&format!("Package: {APP_SLUG}")),
            "deb package name should be {APP_SLUG}"
        );
        assert!(
            read("packaging/rpm/capture-to-search.spec")
                .contains(&format!("Name:           {APP_SLUG}")),
            "rpm package name should be {APP_SLUG}"
        );
        assert!(
            read("packaging/arch/PKGBUILD").contains(&format!("pkgname={APP_SLUG}")),
            "arch package name should be {APP_SLUG}"
        );
    }

    #[test]
    fn versions_are_templated_not_hardcoded() {
        // A hardcoded version silently ships the wrong number forever, since
        // nothing rebuilds these files.
        for file in [
            "packaging/deb/control.template",
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/PKGBUILD",
        ] {
            let text = read(file);
            assert!(
                text.contains("@VERSION@"),
                "{file} should template the version, not hardcode it"
            );
            assert!(
                !text.contains(env!("CARGO_PKG_VERSION")),
                "{file} hardcodes the current version instead of using @VERSION@"
            );
        }
    }

    #[test]
    fn removal_cleans_up_the_autostart_entry() {
        // The app writes ~/.config/autostart per user, so packaging is the only
        // thing that can remove it. Leaving it behind means an uninstalled app
        // still tries to start at login.
        let entry = "autostart/capture-to-search.desktop";
        for file in [
            "packaging/deb/postrm",
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/capture-to-search.install",
            "uninstall.sh",
        ] {
            assert!(
                read(file).contains(entry),
                "{file} should remove the autostart entry on uninstall"
            );
        }
    }

    #[test]
    fn upgrades_do_not_kill_the_running_daemon() {
        // The daemon re-execs onto a replaced binary itself (self_update), so
        // packaging must only stop it on a real removal. Killing on upgrade
        // would make the tray icon vanish until the next login.
        let prerm = read("packaging/deb/prerm");
        assert!(
            prerm.contains(r#"if [ "$1" = "remove" ]"#),
            "deb prerm must only stop the daemon on removal, not upgrade"
        );
        let arch = read("packaging/arch/capture-to-search.install");
        let post_upgrade = arch
            .split("post_upgrade()")
            .nth(1)
            .expect("PKGBUILD install file needs a post_upgrade hook");
        let body = &post_upgrade[..post_upgrade.find("\n}").unwrap_or(post_upgrade.len())];
        assert!(
            !body.contains("pkill"),
            "arch post_upgrade must not kill the daemon"
        );
    }

    #[test]
    fn the_symbolic_icon_is_packaged_everywhere() {
        // The one asset whose filename is not `<appid>.<ext>`, so every glob
        // written the obvious way misses it. rpmbuild turns that into a hard
        // "installed but unpackaged" error; the other formats just ship without
        // it and the tray icon stops adapting to light and dark panels.
        for file in [
            "packaging/rpm/capture-to-search.spec",
            "packaging/arch/PKGBUILD",
            "packaging/build-local.sh",
            "install.sh",
        ] {
            assert!(
                read(file).contains("-symbolic.svg"),
                "{file} does not handle the symbolic tray icon"
            );
        }
    }

    #[test]
    fn every_packaging_file_is_present() {
        for file in PACKAGING_FILES {
            let path = root().join(file);
            assert!(path.is_file(), "missing {}", path.display());
        }
    }
}
