//! Single-instance enforcement via the daemon's Unix socket.
//!
//! The socket is both the IPC endpoint and the "there can be only one daemon"
//! guard. Binding it succeeds for the first daemon; a would-be second daemon
//! probes the existing socket to tell a live daemon apart from a stale file left
//! behind by a crash.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use capture_core::ipc::{self, CaptureMode, IpcMessage};
use tokio::net::{UnixListener, UnixStream};

/// Outcome of trying to become the daemon.
pub enum Acquired {
    /// We bound the socket and are now the sole daemon.
    Primary(UnixListener),
    /// A live daemon already owns the socket.
    AlreadyRunning,
}

/// Try to acquire the single-instance socket at `path`.
pub async fn acquire(path: &Path) -> Result<Acquired> {
    match UnixListener::bind(path) {
        Ok(listener) => Ok(Acquired::Primary(listener)),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if probe_alive(path).await {
                Ok(Acquired::AlreadyRunning)
            } else {
                tracing::warn!("removing stale socket at {}", path.display());
                std::fs::remove_file(path)
                    .with_context(|| format!("removing stale socket {}", path.display()))?;
                let listener = UnixListener::bind(path)
                    .with_context(|| format!("binding socket {}", path.display()))?;
                Ok(Acquired::Primary(listener))
            }
        }
        Err(e) => Err(e).with_context(|| format!("binding socket {}", path.display())),
    }
}

/// Probe whether a live daemon answers: connect, send `Ping`, await `Pong`.
/// Any failure means "not alive", i.e. a stale socket.
pub async fn probe_alive(path: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(path).await else {
        return false;
    };
    if ipc::write_msg(&mut stream, &IpcMessage::Ping)
        .await
        .is_err()
    {
        return false;
    }
    matches!(
        tokio::time::timeout(Duration::from_secs(2), ipc::read_msg(&mut stream)).await,
        Ok(Ok(Some(IpcMessage::Pong)))
    )
}

/// Ask a running daemon to show its window. Used by a second daemon launch.
pub async fn request_show_window(path: &Path) -> Result<()> {
    send_and_ack(path, IpcMessage::ShowWindow).await
}

/// Ask a running daemon to run a capture. Used by the `capture` subcommand, so
/// a hotkey press reuses the daemon's single code path rather than racing it
/// with a second, independent capture.
pub async fn request_capture(path: &Path, mode: CaptureMode) -> Result<()> {
    send_and_ack(path, IpcMessage::CaptureRequest { mode }).await
}

async fn send_and_ack(path: &Path, msg: IpcMessage) -> Result<()> {
    let mut stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("connecting to daemon at {}", path.display()))?;
    ipc::write_msg(&mut stream, &msg).await?;
    // Best-effort: wait for the Ack so the daemon has taken the request before
    // we exit. Not fatal if it doesn't arrive.
    let _ = tokio::time::timeout(Duration::from_secs(2), ipc::read_msg(&mut stream)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use capture_core::ipc;

    /// Answer `Ping` with `Pong` once, like a live daemon would.
    fn spawn_responder(listener: UnixListener) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                if let Ok(Some(IpcMessage::Ping)) = ipc::read_msg(&mut stream).await {
                    let _ = ipc::write_msg(&mut stream, &IpcMessage::Pong).await;
                }
            }
        })
    }

    #[tokio::test]
    async fn first_daemon_becomes_primary() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        assert!(matches!(
            acquire(&socket).await.unwrap(),
            Acquired::Primary(_)
        ));
        assert!(socket.exists());
    }

    #[tokio::test]
    async fn second_daemon_defers_to_a_live_one() {
        // The single-instance guarantee. Without this a second launch would
        // bind nothing, run headless, and race the first over captures.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let Acquired::Primary(listener) = acquire(&socket).await.unwrap() else {
            panic!("first acquire should be primary");
        };
        let responder = spawn_responder(listener);

        assert!(matches!(
            acquire(&socket).await.unwrap(),
            Acquired::AlreadyRunning
        ));
        responder.abort();
    }

    #[tokio::test]
    async fn stale_socket_from_a_crash_is_reclaimed() {
        // A crashed daemon leaves the socket file behind. Treating that as
        // "already running" would make the app permanently unstartable until
        // someone deleted the file by hand.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        {
            let Acquired::Primary(listener) = acquire(&socket).await.unwrap() else {
                panic!("expected primary");
            };
            drop(listener); // nobody is listening now, but the file remains
        }
        assert!(socket.exists(), "precondition: stale file present");

        assert!(
            matches!(acquire(&socket).await.unwrap(), Acquired::Primary(_)),
            "a stale socket must be reclaimed, not mistaken for a live daemon"
        );
    }

    #[tokio::test]
    async fn probe_reports_dead_for_a_silent_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        // Bound but never answering: this is the stale case.
        let _listener = UnixListener::bind(&socket).unwrap();
        assert!(!probe_alive(&socket).await);
    }

    #[tokio::test]
    async fn probe_reports_dead_for_a_missing_socket() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!probe_alive(&dir.path().join("absent.sock")).await);
    }

    #[tokio::test]
    async fn probe_reports_alive_when_the_daemon_answers() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let responder = spawn_responder(listener);

        assert!(probe_alive(&socket).await);
        responder.abort();
    }

    #[tokio::test]
    async fn capture_request_reaches_the_daemon() {
        // What a hotkey press does when a daemon is already running: the mode
        // must survive the trip, or `--area` would silently become full screen.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let received = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let msg = ipc::read_msg(&mut stream).await.unwrap();
            let _ = ipc::write_msg(&mut stream, &IpcMessage::Ack).await;
            msg
        });

        request_capture(&socket, CaptureMode::Area).await.unwrap();
        assert_eq!(
            received.await.unwrap(),
            Some(IpcMessage::CaptureRequest {
                mode: CaptureMode::Area
            })
        );
    }

    #[tokio::test]
    async fn requesting_a_dead_daemon_errors_rather_than_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("absent.sock");
        assert!(request_show_window(&socket).await.is_err());
    }
}
