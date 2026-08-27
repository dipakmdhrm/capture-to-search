//! Daemon <-> window IPC over the `$XDG_RUNTIME_DIR` Unix socket.
//!
//! Framing is a 4-byte big-endian length prefix followed by a `serde_json`
//! body: self-describing, size-tolerant, and free of delimiter-in-payload
//! hazards. The same socket doubles as the single-instance guard.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Upper bound on a single frame (1 MiB). Guards against a corrupt length
/// header triggering a huge allocation. Captures never cross this socket -
/// only paths and result URLs - so the limit is generous.
const MAX_FRAME_LEN: u32 = 1024 * 1024;

/// How the user wants the screen selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CaptureMode {
    /// Let the backend present its own picker (area / window / screen).
    #[default]
    Interactive,
    /// Drag out a region.
    Area,
    /// Whole screen, no prompt.
    Screen,
}

impl CaptureMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Area => "area",
            Self::Screen => "screen",
        }
    }

    /// Whether this mode wants a region rather than the whole screen.
    ///
    /// `Interactive` counts as a region: the desktop's own picker defaults to
    /// area selection, and offers full screen as a choice within it.
    pub fn is_area(&self) -> bool {
        matches!(self, Self::Interactive | Self::Area)
    }
}

/// Messages exchanged between the daemon and the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IpcMessage {
    // --- liveness / handshake ---
    /// Stale-socket probe sent by a would-be second daemon.
    Ping,
    /// Reply to [`IpcMessage::Ping`] from a live daemon.
    Pong,
    /// The window registers itself with the daemon on startup.
    WindowHello,
    /// The window is shutting down.
    WindowClosing,

    // --- window/CLI -> daemon ---
    /// Run the full capture -> upload -> browser flow.
    ///
    /// The daemon owns this even when the window asks for it: the window must be
    /// gone before the shutter fires or it appears in its own screenshot.
    CaptureRequest {
        mode: CaptureMode,
    },
    /// Toggle launch-at-login.
    SetAutostart {
        enabled: bool,
    },
    /// Re-read the config file from disk.
    ReloadConfig,

    // --- daemon -> window ---
    /// Raise/show the window.
    ShowWindow,

    // --- generic replies ---
    Ack,
    Error(String),
}

/// Write one length-prefixed JSON frame.
pub async fn write_msg<W>(w: &mut W, msg: &IpcMessage) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(body.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame length overflows u32",
        )
    })?;
    if len > MAX_FRAME_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds maximum length",
        ));
    }
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await
}

/// Read one frame. `Ok(None)` means the peer closed cleanly.
pub async fn read_msg<R>(r: &mut R) -> std::io::Result<Option<IpcMessage>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds maximum length",
        ));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    let msg = serde_json::from_slice(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn round_trip(msg: IpcMessage) -> IpcMessage {
        let mut buf: Vec<u8> = Vec::new();
        write_msg(&mut buf, &msg).await.unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        read_msg(&mut cursor).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn messages_round_trip() {
        for msg in [
            IpcMessage::Ping,
            IpcMessage::Pong,
            IpcMessage::WindowHello,
            IpcMessage::CaptureRequest {
                mode: CaptureMode::Area,
            },
            IpcMessage::SetAutostart { enabled: true },
            IpcMessage::Error("boom".into()),
        ] {
            assert_eq!(round_trip(msg.clone()).await, msg);
        }
    }

    #[tokio::test]
    async fn back_to_back_frames_stay_aligned() {
        // Length prefixing must let two messages share a stream without the
        // reader running them together.
        let mut buf: Vec<u8> = Vec::new();
        write_msg(&mut buf, &IpcMessage::Ping).await.unwrap();
        write_msg(&mut buf, &IpcMessage::Pong).await.unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(read_msg(&mut cursor).await.unwrap(), Some(IpcMessage::Ping));
        assert_eq!(read_msg(&mut cursor).await.unwrap(), Some(IpcMessage::Pong));
        assert_eq!(read_msg(&mut cursor).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clean_eof_is_none_not_error() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        assert_eq!(read_msg(&mut cursor).await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_length_header_rejected() {
        // A corrupt header must not cause a multi-gigabyte allocation.
        let mut buf = u32::MAX.to_be_bytes().to_vec();
        buf.extend_from_slice(b"junk");
        let mut cursor = std::io::Cursor::new(buf);
        assert!(read_msg(&mut cursor).await.is_err());
    }
}

/// Synchronous framing for the GTK window.
///
/// The window is a GTK main loop, not a tokio one, and its IPC needs amount to
/// "send a message, read the reply". Giving it blocking helpers keeps a second
/// async runtime out of the GUI process entirely - the framing is identical, so
/// both sides stay wire-compatible.
pub mod blocking {
    use std::io::{Read, Write};

    use super::{IpcMessage, MAX_FRAME_LEN};

    pub fn write_msg<W: Write>(w: &mut W, msg: &IpcMessage) -> std::io::Result<()> {
        let body = serde_json::to_vec(msg)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = u32::try_from(body.len()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame length overflows u32",
            )
        })?;
        if len > MAX_FRAME_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame exceeds maximum length",
            ));
        }
        w.write_all(&len.to_be_bytes())?;
        w.write_all(&body)?;
        w.flush()
    }

    /// Read one frame. `Ok(None)` means the peer closed cleanly.
    pub fn read_msg<R: Read>(r: &mut R) -> std::io::Result<Option<IpcMessage>> {
        let mut len_buf = [0u8; 4];
        match r.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = u32::from_be_bytes(len_buf);
        if len > MAX_FRAME_LEN {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame exceeds maximum length",
            ));
        }
        let mut body = vec![0u8; len as usize];
        r.read_exact(&mut body)?;
        let msg = serde_json::from_slice(&body)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(msg))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn blocking_and_async_framing_are_wire_compatible() {
            // The window writes with the blocking helpers and the daemon reads
            // with the async ones; a divergence here would be a silent protocol
            // split that only shows up at runtime.
            let mut buf: Vec<u8> = Vec::new();
            write_msg(
                &mut buf,
                &IpcMessage::CaptureRequest {
                    mode: super::super::CaptureMode::Screen,
                },
            )
            .unwrap();

            let mut cursor = std::io::Cursor::new(buf.clone());
            let via_async = super::super::read_msg(&mut cursor).await.unwrap();
            assert_eq!(
                via_async,
                Some(IpcMessage::CaptureRequest {
                    mode: super::super::CaptureMode::Screen
                })
            );

            let mut cursor = std::io::Cursor::new(buf);
            let via_blocking = read_msg(&mut cursor).unwrap();
            assert_eq!(via_blocking, via_async);
        }
    }
}
