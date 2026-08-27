//! Restart when the daemon binary is replaced in place.
//!
//! A package upgrade (`apt upgrade`, `dnf upgrade`, `pacman -Syu`) swaps the
//! installed binary out from under the running daemon. Linux keeps the old
//! inode mapped, so the old process happily carries on executing the old code -
//! its tray icon and capture pipeline would stay on the previous version until
//! the next login.
//!
//! Rather than have the packages kill the daemon on upgrade (which makes the
//! tray icon vanish mid-upgrade and only return at the next login), we notice
//! the on-disk binary changed and re-exec the new one in place. The daemon and
//! its tray come back on the new version within a few seconds, in the same
//! session, with no root, systemd, or user action involved.
//!
//! Only the signature comparison is unit-tested; the watch loop and the `exec`
//! handoff are platform glue, exercised by actually upgrading a package.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

/// How often to check whether the on-disk binary changed. One `stat` at this
/// cadence is nothing next to an idle tray app's cost.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Grace period after a change is first seen, so the upgrade - the binary plus
/// any maintainer scripts - finishes before we hand off.
const SETTLE: Duration = Duration::from_secs(3);

/// Identity of the binary file on disk, used to detect in-place replacement.
///
/// A package upgrade unlinks the old file and links a new one, so the inode
/// changes (and usually the size and mtime too). Any field differing means
/// "replaced"; comparing content would mean hashing a 200 MB binary every five
/// seconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BinarySignature {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
}

impl BinarySignature {
    /// Read the current signature of `path`, or `None` if it cannot be stat'd.
    pub fn read(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        Some(Self {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: m.mtime(),
        })
    }
}

/// Poll `exe_path`; once its signature changes, set `reexec` and wake
/// `shutdown` so `main` tears everything down cleanly and re-execs.
///
/// Returns after triggering, or immediately if the path cannot be read at
/// startup (a binary running from a deleted path, say) - a watcher that cannot
/// establish a baseline has nothing to compare against.
pub async fn watch(exe_path: PathBuf, reexec: Arc<AtomicBool>, shutdown: Arc<Notify>) {
    let Some(baseline) = BinarySignature::read(&exe_path) else {
        tracing::debug!(
            "self-update: cannot stat {}; watcher disabled",
            exe_path.display()
        );
        return;
    };

    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if let Some(current) = BinarySignature::read(&exe_path) {
            if current != baseline {
                tracing::info!("daemon binary replaced on disk; restarting onto the new version");
                tokio::time::sleep(SETTLE).await;
                reexec.store(true, Ordering::SeqCst);
                shutdown.notify_one();
                return;
            }
        }
    }
}

/// Replace the current process image with a fresh, headless daemon.
///
/// Passes no arguments deliberately: the daemon comes back resident with just
/// the tray, never popping a window the user did not ask for. Only returns (as
/// an error) if `exec` itself fails.
pub fn reexec_into(exe_path: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe_path).exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_changes_the_signature() {
        // This is the whole mechanism: if a replaced binary compared equal, the
        // daemon would keep running the old code after every upgrade.
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("capture-to-searchd");

        std::fs::write(&binary, b"old").unwrap();
        let before = BinarySignature::read(&binary).expect("signature before");

        // Different length, so size differs regardless of mtime resolution -
        // otherwise this test would be flaky on coarse-grained filesystems.
        std::fs::write(&binary, b"a-much-longer-new-binary-image").unwrap();
        let after = BinarySignature::read(&binary).expect("signature after");

        assert_ne!(before, after, "replacement must change the signature");
    }

    #[test]
    fn an_untouched_binary_compares_equal() {
        // The counterpart: a spurious difference would make the daemon re-exec
        // itself every five seconds.
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("capture-to-searchd");
        std::fs::write(&binary, b"stable").unwrap();

        let first = BinarySignature::read(&binary).unwrap();
        let second = BinarySignature::read(&binary).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn missing_path_is_none() {
        assert!(BinarySignature::read(Path::new("/no/such/capture-to-searchd")).is_none());
    }
}
