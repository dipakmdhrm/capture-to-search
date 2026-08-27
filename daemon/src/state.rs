//! Shared daemon state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use capture_core::ipc::{CaptureMode, IpcMessage};
use capture_core::Config;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{oneshot, Notify};

/// Cloneable handle to everything the daemon's tasks share.
#[derive(Clone)]
pub struct AppCtx {
    pub config: Arc<RwLock<Config>>,
    /// Channel to the connected window, if one has said hello.
    pub window: Arc<Mutex<Option<UnboundedSender<IpcMessage>>>>,
    /// Whether a window child process is running (it may not have connected yet).
    pub window_alive: Arc<AtomicBool>,
    /// Kill switch for the window child.
    pub window_kill: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// Signalled by the reaper when the window child has actually exited, so a
    /// capture can wait for the window to be off-screen before firing.
    pub window_gone: Arc<Notify>,
    /// Queue of capture requests.
    pub capture_tx: UnboundedSender<CaptureMode>,
    /// Guards against overlapping captures - two selection overlays at once is
    /// unusable, and the second upload would race the first.
    pub capturing: Arc<AtomicBool>,
}

impl AppCtx {
    pub fn window_alive(&self) -> bool {
        self.window_alive.load(Ordering::SeqCst)
    }

    pub fn set_window_alive(&self, alive: bool) {
        self.window_alive.store(alive, Ordering::SeqCst);
    }

    /// Send to the connected window. `false` if none is connected.
    pub fn send_to_window(&self, msg: IpcMessage) -> bool {
        let guard = self.window.lock().expect("window lock poisoned");
        match guard.as_ref() {
            Some(tx) => tx.send(msg).is_ok(),
            None => false,
        }
    }

    /// Claim the capture slot. `false` means one is already in flight.
    pub fn begin_capture(&self) -> bool {
        self.capturing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn end_capture(&self) {
        self.capturing.store(false, Ordering::SeqCst);
    }

    pub fn config_snapshot(&self) -> Config {
        self.config.read().expect("config lock poisoned").clone()
    }
}

#[cfg(test)]
pub(crate) fn test_ctx() -> (AppCtx, tokio::sync::mpsc::UnboundedReceiver<CaptureMode>) {
    use std::sync::atomic::AtomicBool;
    let (capture_tx, capture_rx) = tokio::sync::mpsc::unbounded_channel();
    let ctx = AppCtx {
        config: Arc::new(RwLock::new(Config::default())),
        window: Arc::new(Mutex::new(None)),
        window_alive: Arc::new(AtomicBool::new(false)),
        window_kill: Arc::new(Mutex::new(None)),
        window_gone: Arc::new(Notify::new()),
        capture_tx,
        capturing: Arc::new(AtomicBool::new(false)),
    };
    (ctx, capture_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_slot_is_exclusive() {
        // Two overlapping captures would put two selection overlays on screen
        // and race two uploads. The guard is the only thing preventing it.
        let (ctx, _rx) = test_ctx();
        assert!(ctx.begin_capture(), "first claim should succeed");
        assert!(!ctx.begin_capture(), "second claim must be refused");
        ctx.end_capture();
        assert!(ctx.begin_capture(), "slot must be reusable after release");
    }

    #[test]
    fn capture_slot_is_released_even_across_clones() {
        // AppCtx is cloned into every task; the guard must be shared state, not
        // per-clone, or the exclusion silently does nothing.
        let (ctx, _rx) = test_ctx();
        let clone = ctx.clone();
        assert!(ctx.begin_capture());
        assert!(!clone.begin_capture(), "clone must see the same slot");
        clone.end_capture();
        assert!(ctx.begin_capture());
    }

    #[test]
    fn sending_to_an_absent_window_reports_failure() {
        // The caller uses this false to decide whether to spawn a window;
        // returning true with no window would mean requests vanish.
        let (ctx, _rx) = test_ctx();
        assert!(!ctx.send_to_window(IpcMessage::ShowWindow));
    }

    #[test]
    fn sending_to_a_connected_window_succeeds() {
        let (ctx, _rx) = test_ctx();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *ctx.window.lock().unwrap() = Some(tx);

        assert!(ctx.send_to_window(IpcMessage::ShowWindow));
        assert_eq!(rx.try_recv().unwrap(), IpcMessage::ShowWindow);
    }

    #[test]
    fn sending_to_a_dropped_window_reports_failure() {
        // The window process died but its sender is still registered: this must
        // read as "no window", so a fresh one gets spawned.
        let (ctx, _rx) = test_ctx();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *ctx.window.lock().unwrap() = Some(tx);
        drop(rx);
        assert!(!ctx.send_to_window(IpcMessage::ShowWindow));
    }

    #[test]
    fn window_liveness_round_trips() {
        let (ctx, _rx) = test_ctx();
        assert!(!ctx.window_alive());
        ctx.set_window_alive(true);
        assert!(ctx.clone().window_alive(), "must be visible to clones");
        ctx.set_window_alive(false);
        assert!(!ctx.window_alive());
    }

    #[test]
    fn config_snapshot_does_not_hold_the_lock() {
        // pipeline() takes a snapshot and then awaits for a long time; if that
        // held the lock, a concurrent config reload would deadlock the daemon.
        let (ctx, _rx) = test_ctx();
        let snapshot = ctx.config_snapshot();
        ctx.config.write().unwrap().max_upload_edge = 42;
        assert_ne!(snapshot.max_upload_edge, 42, "snapshot must be a copy");
        assert_eq!(ctx.config_snapshot().max_upload_edge, 42);
    }
}
