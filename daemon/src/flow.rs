//! The capture -> stage -> browser pipeline.
//!
//! This lives in the daemon and nowhere else. Every trigger - tray click, the
//! window's Capture button, and the `capture` subcommand - funnels through
//! [`run`], because the window has to be off-screen before the shutter fires.
//! A window-owned capture would photograph its own window.
//!
//! The daemon does **not** upload. It writes a self-contained HTML page that
//! makes the browser perform the upload, then opens that page. See
//! [`capture_core::lens`] for why: Google scopes an uploaded image to the
//! client session that uploaded it, so a daemon-side upload always renders as
//! an empty query image in the user's browser.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use capture_core::capture::{self, SessionEnv};
use capture_core::ipc::CaptureMode;
use capture_core::{lens, paths, Config};

use crate::notify;
use crate::state::AppCtx;
use crate::window_proc;

/// How long to wait after the window is gone before capturing.
///
/// The child process exiting does not mean its surface has left the screen -
/// the compositor still has to repaint. Without this pause the window shows up
/// in its own screenshot as a ghost frame.
const WINDOW_SETTLE: Duration = Duration::from_millis(250);

/// How long to wait for the window child to actually exit before giving up and
/// capturing anyway. Better a capture with the window in it than no capture.
const WINDOW_EXIT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a staged page is kept before the daemon deletes it. Long enough for
/// a cold browser start to load and submit it, short enough that a capture is
/// not sitting on disk for the rest of the session.
const STAGED_PAGE_TTL: Duration = Duration::from_secs(120);

/// Age at which an abandoned staged page is swept up on the next capture. This
/// is the backstop for one-shot runs, where the process exits before its own
/// delayed cleanup can fire.
const STAGED_PAGE_MAX_AGE: Duration = Duration::from_secs(600);

/// Run one capture end to end, reporting failures to the desktop.
pub async fn run(ctx: &AppCtx, mode: CaptureMode) {
    if !ctx.begin_capture() {
        tracing::warn!("capture already in progress; ignoring request");
        return;
    }
    let cfg = ctx.config_snapshot();
    let result = pipeline(Some(ctx), &cfg, mode).await;
    ctx.end_capture();

    match result {
        Ok(Some(path)) => {
            tracing::info!("handed capture to the browser: {}", path.display());
            schedule_cleanup(path);
        }
        // Dismissing the selection is a normal outcome, not a failure; a
        // notification here would nag the user for doing nothing wrong.
        Ok(None) => tracing::info!("capture cancelled"),
        Err(e) => {
            // `{:#}` prints the whole anyhow context chain, so the notification
            // names the stage that failed rather than just the last error.
            let detail = format!("{e:#}");
            tracing::error!("capture failed: {detail}");
            notify::error(cfg.notify_on_error, &detail);
        }
    }
}

/// Run one capture with no daemon in the picture.
///
/// This is the `capture` subcommand's path when no daemon is running - a
/// hotkey press must still work on a host where the tray never came up.
pub async fn run_standalone(mode: CaptureMode) -> Result<Option<PathBuf>> {
    let cfg = Config::load(&paths::config_path()?).context("loading config")?;
    pipeline(None, &cfg, mode).await
}

/// The shared pipeline. `ctx` is `None` for a standalone one-shot, where there
/// is no window to dismiss and no daemon state to touch.
async fn pipeline(
    ctx: Option<&AppCtx>,
    cfg: &Config,
    mode: CaptureMode,
) -> Result<Option<PathBuf>> {
    if let Some(ctx) = ctx {
        dismiss_window(ctx).await;
    }

    // Sweep abandoned pages from earlier runs before adding another.
    purge_stale_pages().await;

    let env = SessionEnv::detect();
    // Walks the backend chain: a backend that fails hands off to the next, so
    // one broken desktop integration does not strand the user.
    let (backend, bytes) = match capture::capture(&env, cfg.capture_backend.as_deref(), mode).await
    {
        Ok(v) => v,
        Err(capture::CaptureError::Cancelled) => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!(e).context("capturing the screen")),
    };
    tracing::debug!("captured with {}", backend.name());

    // Log what the backend actually handed us. When a user reports "Lens shows
    // an empty image", this line is the difference between guessing and knowing.
    match lens::inspect(&bytes) {
        Ok(facts) => {
            tracing::info!(
                "captured {}x{} ({} bytes, max_alpha={})",
                facts.width,
                facts.height,
                facts.bytes,
                facts.max_alpha
            );
            if facts.looks_blank() {
                tracing::warn!(
                    "capture looks blank (uniform={}, max_alpha={}) - Lens will \
                     show an empty image",
                    facts.uniform,
                    facts.max_alpha
                );
            }
        }
        Err(e) => tracing::warn!("could not inspect capture: {e:#}"),
    }

    if cfg.keep_captures {
        if let Err(e) = keep_copy(&bytes).await {
            // Keeping a copy is a convenience; never fail the search over it.
            tracing::warn!("could not keep a copy of the capture: {e:#}");
        }
    }

    let prepared = lens::downscale(&bytes, cfg.max_upload_edge).context("preparing capture")?;
    let page = lens::upload_page(&prepared, &cfg.lens_endpoint);
    let path = paths::new_upload_page_path()?;
    paths::write_private(&path, page.as_bytes())
        .with_context(|| format!("staging upload page at {}", path.display()))?;

    open::that_detached(&path)
        .with_context(|| format!("opening {} in a browser", path.display()))?;
    Ok(Some(path))
}

/// Get the window off the screen before capturing.
///
/// Kills the child (kill-on-close is this app's model, so there is nothing to
/// preserve), waits for it to actually exit, then lets the compositor repaint.
async fn dismiss_window(ctx: &AppCtx) {
    if !ctx.window_alive() {
        return;
    }
    tracing::debug!("dismissing window before capture");
    let gone = ctx.window_gone.notified();
    window_proc::kill(ctx);
    if tokio::time::timeout(WINDOW_EXIT_TIMEOUT, gone)
        .await
        .is_err()
    {
        tracing::warn!("window did not exit in time; capturing anyway");
    }
    tokio::time::sleep(WINDOW_SETTLE).await;
}

/// Delete a staged page once the browser has had time to submit it.
fn schedule_cleanup(path: PathBuf) {
    tokio::spawn(async move {
        tokio::time::sleep(STAGED_PAGE_TTL).await;
        if tokio::fs::remove_file(&path).await.is_ok() {
            tracing::debug!("removed staged page {}", path.display());
        }
    });
}

/// Remove staged pages left behind by runs that exited before their own
/// cleanup could fire.
async fn purge_stale_pages() {
    let Ok(dir) = paths::upload_page_dir() else {
        return;
    };
    purge_stale_pages_in(&dir, STAGED_PAGE_MAX_AGE).await;
}

/// The sweep itself, against a named directory so it is testable without
/// touching the real runtime dir (and a live daemon's staged pages).
async fn purge_stale_pages_in(dir: &Path, max_age: Duration) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    let now = SystemTime::now();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("html") {
            continue;
        }
        let stale = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > max_age);
        if stale {
            let _ = tokio::fs::remove_file(&path).await;
            tracing::debug!("purged stale staged page {}", path.display());
        }
    }
}

/// Save a copy of the capture when `keep_captures` is on.
async fn keep_copy(bytes: &[u8]) -> Result<()> {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = paths::capture_temp_dir()?.join(format!("kept-{nanos}.png"));
    paths::write_private(&path, bytes)?;
    tracing::info!("kept capture at {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a file and backdate its mtime by `age`.
    fn aged_file(dir: &Path, name: &str, age: Duration) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"<html></html>").unwrap();
        let when = SystemTime::now() - age;
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(when).unwrap();
        path
    }

    #[tokio::test]
    async fn stale_pages_are_swept() {
        // One-shot runs exit before their own cleanup fires, so without this
        // sweep every hotkey capture leaves a copy of the screen in tmpfs for
        // the rest of the session.
        let dir = tempfile::tempdir().unwrap();
        let old = aged_file(dir.path(), "old.html", Duration::from_secs(3600));

        purge_stale_pages_in(dir.path(), Duration::from_secs(600)).await;
        assert!(!old.exists(), "an hour-old page should have been removed");
    }

    #[tokio::test]
    async fn fresh_pages_are_left_alone() {
        // The browser may not have loaded the page yet; deleting it out from
        // under a cold-starting browser would lose the capture.
        let dir = tempfile::tempdir().unwrap();
        let fresh = aged_file(dir.path(), "fresh.html", Duration::from_secs(5));

        purge_stale_pages_in(dir.path(), Duration::from_secs(600)).await;
        assert!(fresh.exists(), "a page from 5 seconds ago must survive");
    }

    #[tokio::test]
    async fn sweep_only_touches_staged_pages() {
        // The sweep runs against a directory; it must not delete anything that
        // is not one of ours.
        let dir = tempfile::tempdir().unwrap();
        let other = aged_file(dir.path(), "notes.txt", Duration::from_secs(3600));
        let png = aged_file(dir.path(), "capture.png", Duration::from_secs(3600));

        purge_stale_pages_in(dir.path(), Duration::from_secs(600)).await;
        assert!(other.exists(), "non-html files must be left alone");
        assert!(png.exists(), "captures must not be swept by the page sweep");
    }

    #[tokio::test]
    async fn sweep_tolerates_a_missing_directory() {
        // First run on a fresh machine, or a runtime dir cleared under us.
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("never-created");
        purge_stale_pages_in(&absent, Duration::from_secs(600)).await;
    }

    #[test]
    fn window_settle_is_long_enough_to_be_meaningful() {
        // The child exiting does not mean its surface has left the screen. A
        // zero or near-zero settle would put a ghost of our own window in the
        // capture, which is the bug this constant exists to prevent.
        assert!(
            WINDOW_SETTLE >= Duration::from_millis(100),
            "settle delay is too short to cover a compositor repaint"
        );
        assert!(
            WINDOW_SETTLE <= Duration::from_millis(1000),
            "settle delay this long is a visible stall before every capture"
        );
    }

    #[test]
    fn staged_pages_outlive_a_browser_cold_start() {
        // Deleting the page before a cold browser has submitted it loses the
        // capture silently - the tab just shows a missing file.
        assert!(STAGED_PAGE_TTL >= Duration::from_secs(60));
        assert!(STAGED_PAGE_MAX_AGE > STAGED_PAGE_TTL);
    }
}
