//! On-demand window process management.
//!
//! The daemon owns at most one window child. Requests either raise the running
//! window over IPC or spawn a new one. A reaper task clears tracked state when
//! the child exits and signals `window_gone`, which the capture flow waits on.
//!
//! Unlike a general-purpose viewer, this window is deliberately **killed** on
//! close rather than kept resident: it is a single button, so respawning costs
//! almost nothing, and a resident GTK process would dominate the idle footprint
//! of an app whose whole point is to sit quietly in the tray.

use std::path::PathBuf;

use capture_core::ipc::IpcMessage;
use capture_core::APP_SLUG;
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::state::AppCtx;

/// Show the window: raise the running one, or spawn it.
pub fn show(ctx: &AppCtx) {
    if ctx.send_to_window(IpcMessage::ShowWindow) {
        return;
    }
    if ctx.window_alive() {
        // Spawned but not connected back yet; it will present itself on start.
        tracing::debug!("window is starting; ignoring duplicate show request");
        return;
    }
    spawn(ctx);
}

/// Ask the current window child (if any) to terminate.
pub fn kill(ctx: &AppCtx) {
    if let Some(tx) = ctx.window_kill.lock().expect("window_kill poisoned").take() {
        let _ = tx.send(());
    }
}

/// Spawn the window and start a reaper that clears state when it exits.
fn spawn(ctx: &AppCtx) {
    let Some(exe) = gui_path() else {
        tracing::warn!(
            "no GUI binary found; this build or install is daemon-only. \
             Capture still works from the tray and `{APP_SLUG}d capture`."
        );
        return;
    };

    let mut child = match Command::new(&exe).kill_on_drop(false).spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to spawn window {}: {e}", exe.display());
            return;
        }
    };
    tracing::info!("spawned window (pid {:?})", child.id());
    ctx.set_window_alive(true);

    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    *ctx.window_kill.lock().expect("window_kill poisoned") = Some(kill_tx);

    let ctx = ctx.clone();
    tokio::spawn(async move {
        tokio::select! {
            status = child.wait() => tracing::info!("window exited: {status:?}"),
            _ = kill_rx => {
                let _ = child.kill().await;
                tracing::info!("window terminated by daemon");
            }
        }
        ctx.set_window_alive(false);
        *ctx.window.lock().expect("window poisoned") = None;
        *ctx.window_kill.lock().expect("window_kill poisoned") = None;
        // Wake the capture flow, which is waiting for the window to be gone.
        ctx.window_gone.notify_waiters();
    });
}

/// Locate the GUI binary: next to the running daemon, else on PATH.
///
/// `None` is a supported state, not an error: on a host without GTK 4 the GUI
/// package simply isn't installed, and the tray omits its "Open Window" entry
/// rather than offering a menu item that would fail.
pub fn gui_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    find_gui(
        exe.as_deref().and_then(|e| e.parent()),
        std::env::var_os("PATH").as_deref(),
    )
}

/// The pure lookup rule, split out so it can be tested without mutating
/// `PATH` (which races with tests running on other threads).
///
/// A sibling binary wins over `PATH` so a developer running from `target/debug`
/// gets the GUI they just built rather than an installed copy.
fn find_gui(
    exe_dir: Option<&std::path::Path>,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let name = format!("{APP_SLUG}-gui");
    if let Some(dir) = exe_dir {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let path = path_var?;
    std::env::split_paths(path)
        .map(|dir| dir.join(&name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_gui(dir: &std::path::Path) -> PathBuf {
        let path = dir.join(format!("{APP_SLUG}-gui"));
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        path
    }

    #[test]
    fn a_sibling_binary_wins() {
        // Running from target/debug must use the GUI just built, not one
        // installed system-wide.
        let beside = tempfile::tempdir().unwrap();
        let on_path = tempfile::tempdir().unwrap();
        let expected = touch_gui(beside.path());
        touch_gui(on_path.path());

        let found = find_gui(Some(beside.path()), Some(on_path.path().as_os_str())).unwrap();
        assert_eq!(found, expected);
    }

    #[test]
    fn falls_back_to_path() {
        let empty = tempfile::tempdir().unwrap();
        let on_path = tempfile::tempdir().unwrap();
        let expected = touch_gui(on_path.path());

        let found = find_gui(Some(empty.path()), Some(on_path.path().as_os_str())).unwrap();
        assert_eq!(found, expected);
    }

    #[test]
    fn missing_gui_is_none_not_an_error() {
        // A daemon-only install on a host without GTK is a supported state:
        // the tray omits its window entry rather than offering a broken one.
        let empty = tempfile::tempdir().unwrap();
        let also_empty = tempfile::tempdir().unwrap();
        assert!(find_gui(Some(empty.path()), Some(also_empty.path().as_os_str())).is_none());
        assert!(find_gui(None, None).is_none());
    }

    #[test]
    fn a_directory_named_like_the_binary_is_not_mistaken_for_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(format!("{APP_SLUG}-gui"))).unwrap();
        assert!(find_gui(Some(dir.path()), None).is_none());
    }
}
