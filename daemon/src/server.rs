//! IPC server: accepts connections on the daemon socket.

use anyhow::Result;
use capture_core::ipc::{self, IpcMessage};
use capture_core::{autostart, paths, Config};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::state::AppCtx;
use crate::window_proc;

/// Accept loop. Runs for the daemon's lifetime.
pub async fn run(listener: UnixListener, ctx: AppCtx) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, ctx).await {
                        tracing::debug!("ipc connection ended: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                // Don't spin hot on a persistent accept error.
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

async fn handle(stream: UnixStream, ctx: AppCtx) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    // Outbound messages go through a channel so the daemon can push to the
    // window (ShowWindow) without owning the socket's write half.
    let (tx, mut rx) = mpsc::unbounded_channel::<IpcMessage>();
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ipc::write_msg(&mut writer, &msg).await.is_err() {
                break;
            }
        }
    });

    let mut registered_window = false;

    while let Some(msg) = ipc::read_msg(&mut reader).await? {
        match msg {
            IpcMessage::Ping => {
                let _ = tx.send(IpcMessage::Pong);
            }

            IpcMessage::WindowHello => {
                tracing::debug!("window connected");
                *ctx.window.lock().expect("window poisoned") = Some(tx.clone());
                registered_window = true;
                let _ = tx.send(IpcMessage::Ack);
            }

            IpcMessage::WindowClosing => {
                tracing::debug!("window reported closing");
                *ctx.window.lock().expect("window poisoned") = None;
                registered_window = false;
                let _ = tx.send(IpcMessage::Ack);
            }

            IpcMessage::CaptureRequest { mode } => {
                // Ack before capturing: the flow dismisses the window first, and
                // a window waiting on its own Ack would deadlock the handshake.
                let _ = tx.send(IpcMessage::Ack);
                let _ = ctx.capture_tx.send(mode);
            }

            IpcMessage::ShowWindow => {
                window_proc::show(&ctx);
                let _ = tx.send(IpcMessage::Ack);
            }

            IpcMessage::SetAutostart { enabled } => {
                let reply = match autostart::set_enabled(enabled) {
                    Ok(()) => {
                        ctx.config.write().expect("config poisoned").autostart = enabled;
                        IpcMessage::Ack
                    }
                    Err(e) => IpcMessage::Error(format!("{e:#}")),
                };
                let _ = tx.send(reply);
            }

            IpcMessage::ReloadConfig => {
                let reply = match reload_config(&ctx) {
                    Ok(()) => IpcMessage::Ack,
                    Err(e) => IpcMessage::Error(format!("{e:#}")),
                };
                let _ = tx.send(reply);
            }

            // Replies and daemon-only messages have no meaning inbound.
            other => {
                tracing::debug!("ignoring unexpected inbound message: {other:?}");
                let _ = tx.send(IpcMessage::Error("unexpected message".into()));
            }
        }
    }

    if registered_window {
        *ctx.window.lock().expect("window poisoned") = None;
    }
    write_task.abort();
    Ok(())
}

fn reload_config(ctx: &AppCtx) -> Result<()> {
    let cfg = Config::load(&paths::config_path()?)?;
    *ctx.config.write().expect("config poisoned") = cfg;
    tracing::info!("config reloaded");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_ctx;
    use capture_core::ipc::CaptureMode;
    use tempfile::TempDir;
    use tokio::sync::mpsc::UnboundedReceiver;

    /// Start the real accept loop on a temp socket and return a client stream.
    async fn connected() -> (
        TempDir,
        UnixStream,
        AppCtx,
        UnboundedReceiver<CaptureMode>,
        tokio::task::JoinHandle<()>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (ctx, capture_rx) = test_ctx();
        let server = tokio::spawn(run(listener, ctx.clone()));
        let client = UnixStream::connect(&socket).await.unwrap();
        (dir, client, ctx, capture_rx, server)
    }

    async fn exchange(client: &mut UnixStream, msg: IpcMessage) -> IpcMessage {
        ipc::write_msg(client, &msg).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), ipc::read_msg(client))
            .await
            .expect("server did not reply in time")
            .unwrap()
            .expect("server closed the connection")
    }

    #[tokio::test]
    async fn ping_is_answered_with_pong() {
        // This is what the stale-socket probe relies on; if it stops working,
        // every second launch mistakes a live daemon for a crashed one.
        let (_dir, mut client, _ctx, _rx, server) = connected().await;
        assert_eq!(
            exchange(&mut client, IpcMessage::Ping).await,
            IpcMessage::Pong
        );
        server.abort();
    }

    #[tokio::test]
    async fn capture_request_is_acked_and_queued() {
        let (_dir, mut client, _ctx, mut capture_rx, server) = connected().await;
        let reply = exchange(
            &mut client,
            IpcMessage::CaptureRequest {
                mode: CaptureMode::Screen,
            },
        )
        .await;
        assert_eq!(reply, IpcMessage::Ack);
        assert_eq!(capture_rx.recv().await, Some(CaptureMode::Screen));
        server.abort();
    }

    #[tokio::test]
    async fn capture_is_acked_before_the_capture_runs() {
        // The daemon dismisses the window as part of servicing this request. If
        // the Ack waited for the capture, the window would be blocked reading a
        // reply that only arrives after it has been killed.
        let (_dir, mut client, _ctx, mut capture_rx, server) = connected().await;
        ipc::write_msg(
            &mut client,
            &IpcMessage::CaptureRequest {
                mode: CaptureMode::Area,
            },
        )
        .await
        .unwrap();

        // Ack arrives while the request is still sitting unread on the queue.
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ipc::read_msg(&mut client),
        )
        .await
        .expect("Ack must not wait on the capture")
        .unwrap();
        assert_eq!(reply, Some(IpcMessage::Ack));
        assert_eq!(capture_rx.recv().await, Some(CaptureMode::Area));
        server.abort();
    }

    #[tokio::test]
    async fn window_hello_registers_and_disconnect_clears() {
        // A window that has said hello can be pushed ShowWindow. When it goes
        // away the registration must be dropped, or the daemon keeps sending
        // into a dead channel instead of spawning a new window.
        let (_dir, mut client, ctx, _rx, server) = connected().await;
        assert_eq!(
            exchange(&mut client, IpcMessage::WindowHello).await,
            IpcMessage::Ack
        );
        assert!(window_registered(&ctx), "should be registered");

        drop(client);
        for _ in 0..50 {
            if !window_registered(&ctx) {
                server.abort();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("registration was not cleared after the window disconnected");
    }

    /// Is a window currently registered? Checked directly rather than by
    /// sending it a message: pushing one would put an extra frame on the
    /// socket that the next `exchange` would read instead of its reply.
    fn window_registered(ctx: &AppCtx) -> bool {
        ctx.window.lock().unwrap().is_some()
    }

    #[tokio::test]
    async fn window_closing_deregisters() {
        let (_dir, mut client, ctx, _rx, server) = connected().await;
        assert_eq!(
            exchange(&mut client, IpcMessage::WindowHello).await,
            IpcMessage::Ack
        );
        assert!(window_registered(&ctx), "hello should register the window");

        assert_eq!(
            exchange(&mut client, IpcMessage::WindowClosing).await,
            IpcMessage::Ack
        );
        assert!(
            !window_registered(&ctx),
            "closing must deregister so the next show spawns a fresh window"
        );
        server.abort();
    }

    #[tokio::test]
    async fn reply_messages_are_rejected_inbound() {
        // Ack/Pong are replies, not commands. Accepting them would let a
        // malformed peer drive daemon state.
        let (_dir, mut client, _ctx, _rx, server) = connected().await;
        let reply = exchange(&mut client, IpcMessage::Ack).await;
        assert!(matches!(reply, IpcMessage::Error(_)), "got {reply:?}");
        server.abort();
    }

    #[tokio::test]
    async fn one_connection_can_carry_several_requests() {
        // The window holds a single connection for its lifetime; framing must
        // keep successive messages apart.
        let (_dir, mut client, _ctx, mut capture_rx, server) = connected().await;
        assert_eq!(
            exchange(&mut client, IpcMessage::Ping).await,
            IpcMessage::Pong
        );
        assert_eq!(
            exchange(
                &mut client,
                IpcMessage::CaptureRequest {
                    mode: CaptureMode::Area
                }
            )
            .await,
            IpcMessage::Ack
        );
        assert_eq!(
            exchange(&mut client, IpcMessage::Ping).await,
            IpcMessage::Pong
        );
        assert_eq!(capture_rx.recv().await, Some(CaptureMode::Area));
        server.abort();
    }

    #[tokio::test]
    async fn a_broken_client_does_not_stop_the_server() {
        // One peer disconnecting mid-frame must not take the accept loop down
        // and leave the tray unable to talk to the daemon.
        let (dir, client, _ctx, _rx, server) = connected().await;
        drop(client);

        let socket = dir.path().join("daemon.sock");
        let mut second = UnixStream::connect(&socket).await.unwrap();
        assert_eq!(
            exchange(&mut second, IpcMessage::Ping).await,
            IpcMessage::Pong
        );
        server.abort();
    }
}
