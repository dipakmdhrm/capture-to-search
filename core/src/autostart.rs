//! Launch-at-login integration.
//!
//! Writes/removes `~/.config/autostart/capture-to-search.desktop`, which starts
//! the daemon at login. The file on disk is the source of truth for whether
//! autostart is enabled - there is no separate state to keep in sync.
//!
//! **Flatpak is detected but not yet supported.** Inside a sandbox that path is
//! the app's private, host-invisible config dir, so writing it does nothing;
//! autostart there has to go through the XDG Background portal instead
//! (`org.freedesktop.portal.Background.RequestBackground`). Rather than fail
//! silently we return an explicit error, and this is the one place to wire the
//! portal call up if Flatpak packaging lands.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::{paths, APP_ID, APP_NAME, APP_SLUG};

/// Whether we're running inside a Flatpak sandbox.
///
/// The runtime bind-mounts `/.flatpak-info` into every sandbox, so its presence
/// is the canonical probe. Also consulted by the tray, which must register under
/// a unique D-Bus name when sandboxed.
pub fn is_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// Path to the autostart entry.
pub fn desktop_path() -> Result<PathBuf> {
    paths::autostart_desktop_path()
}

/// Whether autostart is currently enabled.
pub fn is_enabled() -> bool {
    desktop_path().map(|p| p.exists()).unwrap_or(false)
}

/// Install the autostart entry, launching the resolved daemon binary.
pub fn enable() -> Result<()> {
    if is_flatpak() {
        bail!("autostart inside Flatpak needs the Background portal, which is not wired up yet");
    }
    let exec = daemon_exec();
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={APP_NAME}\n\
         Comment=Capture a region of the screen and search it with Google Lens\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Utility;Graphics;\n\
         X-GNOME-Autostart-enabled=true\n"
    );
    let path = desktop_path()?;
    std::fs::write(&path, content)?;
    tracing::info!("autostart enabled: {}", path.display());
    Ok(())
}

/// Remove the autostart entry, if present.
pub fn disable() -> Result<()> {
    let path = desktop_path()?;
    if path.exists() {
        std::fs::remove_file(&path)?;
        tracing::info!("autostart disabled: {}", path.display());
    }
    Ok(())
}

/// Enable or disable to match `enabled`.
pub fn set_enabled(enabled: bool) -> Result<()> {
    if enabled {
        enable()
    } else {
        disable()
    }
}

/// Resolve the daemon command for the `Exec=` line.
///
/// Prefers an absolute path next to the current executable, so this works
/// whether it is called from the daemon itself or from the GUI (which sits in
/// the same directory). Falls back to the bare name for a PATH install.
fn daemon_exec() -> String {
    let bin = format!("{APP_SLUG}d");
    if let Ok(exe) = std::env::current_exe() {
        if exe.file_name().and_then(|n| n.to_str()) == Some(bin.as_str()) {
            return exe.display().to_string();
        }
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(&bin);
            if candidate.exists() {
                return candidate.display().to_string();
            }
        }
    }
    bin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_entry_starts_the_daemon_headless() {
        // The login entry must bring up the daemon and tray WITHOUT popping the
        // window on every login.
        let exec = daemon_exec();
        assert!(
            exec.ends_with("capture-to-searchd"),
            "Exec must launch the daemon, got {exec}"
        );
        assert!(!exec.contains("--show-window"));
        assert!(!exec.contains("-gui"));
    }

    /// The desktop entry shipped in `data/applications`.
    fn shipped_desktop_entry() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../data/applications/{APP_ID}.desktop"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing {}: {e}", path.display()))
    }

    #[test]
    fn shipped_desktop_entry_has_the_keys_a_launcher_needs() {
        // A missing key here means the app is installed but invisible in the
        // application menu, or launches with the wrong icon.
        let entry = shipped_desktop_entry();
        for key in [
            "[Desktop Entry]",
            "Type=Application",
            &format!("Name={APP_NAME}"),
            &format!("Icon={APP_ID}"),
            "Terminal=false",
        ] {
            assert!(entry.contains(key), "desktop entry is missing `{key}`");
        }
    }

    #[test]
    fn shipped_entry_and_autostart_entry_launch_the_same_binary() {
        // Two places name the daemon: the menu entry we ship and the autostart
        // entry we generate. If they drift, one of them silently stops working.
        let entry = shipped_desktop_entry();
        let exec = entry
            .lines()
            .find_map(|l| l.strip_prefix("Exec="))
            .expect("desktop entry has no Exec line");
        let binary = format!("{APP_SLUG}d");
        assert!(
            exec.split_whitespace().next().unwrap().ends_with(&binary),
            "shipped Exec `{exec}` should launch {binary}"
        );
        assert!(
            daemon_exec().ends_with(&binary),
            "autostart Exec should launch the same binary"
        );
    }

    #[test]
    fn shipped_entry_opens_the_window_but_autostart_does_not() {
        // Clicking the app in the menu should show the window; logging in
        // should only bring up the tray. Same binary, different flags.
        let entry = shipped_desktop_entry();
        let exec = entry.lines().find_map(|l| l.strip_prefix("Exec=")).unwrap();
        assert!(
            exec.contains("--show-window"),
            "the menu entry should open the window, got `{exec}`"
        );
        assert!(
            !daemon_exec().contains("--show-window"),
            "autostart must not pop the window on every login"
        );
    }

    #[test]
    fn entry_filename_is_slug_scoped() {
        // Must not collide with another app's autostart entry.
        let path = desktop_path().unwrap();
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("capture-to-search.desktop")
        );
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "autostart");
    }
}
