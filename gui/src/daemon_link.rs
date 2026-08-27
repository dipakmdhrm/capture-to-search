//! The window's link to the daemon.
//!
//! Two shapes of connection, both over the same socket:
//!
//! - [`request`] opens a short-lived connection for one round trip. Used by the
//!   Capture button, where the daemon may well terminate this process as part of
//!   servicing the request - the window has to be off-screen before the shutter
//!   fires, so there is deliberately nothing to keep open.
//! - [`listen`] holds a connection for the window's lifetime so the daemon can
//!   push `ShowWindow` and raise an already-open window.

use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context, Result};
use capture_core::ipc::{blocking, IpcMessage};
use capture_core::paths;

/// One request, one reply.
pub fn request(msg: IpcMessage) -> Result<IpcMessage> {
    let socket = paths::daemon_socket_path()?;
    let mut stream = UnixStream::connect(&socket)
        .with_context(|| format!("connecting to the daemon at {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    blocking::write_msg(&mut stream, &msg).context("sending request to daemon")?;
    let reply = blocking::read_msg(&mut stream)
        .context("reading daemon reply")?
        .context("daemon closed the connection without replying")?;
    match reply {
        IpcMessage::Error(e) => Err(anyhow::anyhow!(e)),
        other => Ok(other),
    }
}

/// Hold a connection open and forward daemon-pushed messages onto `tx`.
///
/// Runs on its own thread; the GTK main loop drains `tx`. Failure is silent by
/// design: a window launched with no daemon running should still open and show
/// a useful error when the user actually presses Capture, rather than refusing
/// to start.
pub fn listen(tx: async_channel::Sender<IpcMessage>) {
    std::thread::spawn(move || {
        let Ok(socket) = paths::daemon_socket_path() else {
            return;
        };
        let Ok(mut stream) = UnixStream::connect(&socket) else {
            tracing::debug!("no daemon to listen to; window is running standalone");
            return;
        };
        if blocking::write_msg(&mut stream, &IpcMessage::WindowHello).is_err() {
            return;
        }
        loop {
            match blocking::read_msg(&mut stream) {
                Ok(Some(msg)) => {
                    if tx.send_blocking(msg).is_err() {
                        return; // GTK side is gone
                    }
                }
                // Clean close or error: the daemon is done with us.
                Ok(None) | Err(_) => return,
            }
        }
    });
}
