//! XDG base-directory paths for config, runtime socket, and the autostart entry.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::APP_SLUG;

/// `~/.config/capture-to-search`, created if absent.
///
/// Derived from the slug rather than a reverse-DNS app id, so the directory
/// matches the autostart entry's name and is guessable from the command name.
pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs_config_home()?.join(APP_SLUG);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating config dir {}", dir.display()))?;
    Ok(dir)
}

/// `~/.config/capture-to-search/config.toml`.
pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Directory for the daemon's runtime socket.
///
/// Prefers `$XDG_RUNTIME_DIR` (tmpfs, user-private, cleared on logout), which is
/// where a socket belongs. Falls back to `/tmp/<slug>-<uid>` on systems that
/// don't set it, so the daemon still starts rather than refusing to run.
pub fn runtime_dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(base) => PathBuf::from(base).join(APP_SLUG),
        None => {
            let uid = unsafe { libc_getuid() };
            PathBuf::from(format!("/tmp/{APP_SLUG}-{uid}"))
        }
    };
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating runtime dir {}", dir.display()))?;
    Ok(dir)
}

/// The daemon's Unix socket: IPC endpoint and single-instance guard in one.
pub fn daemon_socket_path() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("daemon.sock"))
}

/// Where captures are staged before upload.
pub fn capture_temp_dir() -> Result<PathBuf> {
    let dir = runtime_dir()?.join("captures");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating capture dir {}", dir.display()))?;
    Ok(dir)
}

/// Where the staged upload pages live.
///
/// Under `$XDG_RUNTIME_DIR` (tmpfs, mode 0700, cleared at logout) because each
/// page embeds a screen capture. These are handed to the browser and deleted
/// shortly after.
pub fn upload_page_dir() -> Result<PathBuf> {
    let dir = runtime_dir()?.join("uploads");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating upload staging dir {}", dir.display()))?;
    Ok(dir)
}

/// A unique staged-page path.
pub fn new_upload_page_path() -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(upload_page_dir()?.join(format!("upload-{}-{nanos}.html", std::process::id())))
}

/// Write a file owner-only.
///
/// Used for anything derived from a capture - staged upload pages and kept
/// copies both embed a picture of the user's screen, so they must not inherit a
/// permissive umask.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting permissions on {}", path.display()))?;
    }
    Ok(())
}

/// `~/.config/autostart/capture-to-search.desktop`.
pub fn autostart_desktop_path() -> Result<PathBuf> {
    let dir = dirs_config_home()?.join("autostart");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating autostart dir {}", dir.display()))?;
    Ok(dir.join(format!("{APP_SLUG}.desktop")))
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
fn dirs_config_home() -> Result<PathBuf> {
    resolve_config_home(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// The pure resolution rule, split out so it can be tested without mutating
/// process environment (which races with tests on other threads).
///
/// Per the XDG spec a relative `XDG_CONFIG_HOME` is invalid and must be
/// ignored rather than resolved against the current directory - otherwise
/// config location would depend on where the binary happened to be launched.
fn resolve_config_home(
    xdg_config_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Result<PathBuf> {
    if let Some(dir) = xdg_config_home {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let home = home.context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config"))
}

/// `getuid(2)` without pulling in the `libc` crate for a single call.
unsafe fn libc_getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn absolute_xdg_config_home_wins() {
        let got = resolve_config_home(Some(OsStr::new("/custom/cfg")), Some(OsStr::new("/home/u")))
            .unwrap();
        assert_eq!(got, PathBuf::from("/custom/cfg"));
    }

    #[test]
    fn relative_xdg_config_home_is_ignored() {
        // The spec says a relative value is invalid. Honouring it would make
        // the config path depend on the process's working directory.
        let got = resolve_config_home(
            Some(OsStr::new("relative/cfg")),
            Some(OsStr::new("/home/u")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/home/u/.config"));
    }

    #[test]
    fn falls_back_to_home_dot_config() {
        let got = resolve_config_home(None, Some(OsStr::new("/home/u"))).unwrap();
        assert_eq!(got, PathBuf::from("/home/u/.config"));
    }

    #[test]
    fn no_home_is_an_error_not_a_panic() {
        assert!(resolve_config_home(None, None).is_err());
    }

    #[test]
    fn write_private_is_owner_only() {
        // Staged upload pages and kept captures both embed a picture of the
        // user's screen. A permissive umask must not leak them to other users
        // on a shared machine.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.png");
        write_private(&path, b"pixels").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        assert_eq!(std::fs::read(&path).unwrap(), b"pixels");
    }

    #[test]
    fn write_private_overwrites_and_retightens() {
        // Rewriting an existing, world-readable file must not inherit its
        // permissions.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("was-public.png");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, b"new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn write_private_reports_a_bad_path() {
        assert!(write_private(Path::new("/nonexistent-dir/x.png"), b"x").is_err());
    }
}
