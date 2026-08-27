//! StatusNotifierItem system-tray icon via `ksni`.
//!
//! Tied to the *session* lifecycle rather than trying to survive across
//! sessions:
//!
//! - `assume_sni_available(true)` lets us autostart *before* the desktop's tray
//!   host is up: `ksni` holds the item and registers once the watcher appears,
//!   and re-registers across watcher restarts on its own.
//! - Under Flatpak we register with `disable_dbus_name(true)`, since the sandbox
//!   cannot own the well-known `org.kde.StatusNotifierItem-...` name. Native
//!   installs keep the spec-recommended name for maximum host compatibility.
//! - When a logout tears down the D-Bus session bus the daemon shuts down (see
//!   [`wait_for_session_bus_loss`]), so the next login's autostart brings up a
//!   fresh daemon with a working tray.
//!
//! Registration is best-effort. On a host with no SNI tray - stock GNOME
//! without the AppIndicator extension, or an XEmbed-only setup - the daemon
//! keeps running and capture stays reachable via `capture-to-searchd capture`,
//! which is also what a desktop hotkey binding invokes.

use std::sync::{Arc, OnceLock};

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Handle, Icon, Tray, TrayMethods};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Notify;

use capture_core::ipc::CaptureMode;
use capture_core::{APP_ID, APP_NAME};

/// The tray model. Holds only cloneable senders, so menu callbacks hand work
/// off to the daemon without blocking the menu.
pub struct CaptureTray {
    capture_tx: UnboundedSender<CaptureMode>,
    show_tx: UnboundedSender<()>,
    shutdown: Arc<Notify>,
    /// Whether a GUI binary exists; when not, the window entry is omitted
    /// rather than offered and failing.
    has_gui: bool,
    /// Embedded app icon as ARGB pixmaps; empty if decoding failed.
    icons: Vec<Icon>,
}

impl Tray for CaptureTray {
    fn id(&self) -> String {
        APP_ID.to_string()
    }

    fn title(&self) -> String {
        APP_NAME.to_string()
    }

    fn icon_name(&self) -> String {
        // Prefer the embedded ARGB pixmaps; fall back to the themed name only if
        // decoding failed. Several hosts - notably the GNOME AppIndicator
        // extension - prefer IconName and render a placeholder when it doesn't
        // resolve in the icon theme, so send an empty name when we have pixmaps.
        if self.icons.is_empty() {
            APP_ID.to_string()
        } else {
            String::new()
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.clone()
    }

    /// Left-click captures. This is the app's entire purpose, so the primary
    /// click performs it rather than opening a window that holds one button.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.capture_tx.send(CaptureMode::Interactive);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: "Capture".into(),
                icon_name: "camera-photo".into(),
                activate: Box::new(|t: &mut CaptureTray| {
                    let _ = t.capture_tx.send(CaptureMode::Interactive);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Capture full screen".into(),
                icon_name: "video-display".into(),
                activate: Box::new(|t: &mut CaptureTray| {
                    let _ = t.capture_tx.send(CaptureMode::Screen);
                }),
                ..Default::default()
            }
            .into(),
        ];

        if self.has_gui {
            items.push(MenuItem::Separator);
            items.push(
                StandardItem {
                    label: "Open window".into(),
                    icon_name: "window-new".into(),
                    activate: Box::new(|t: &mut CaptureTray| {
                        let _ = t.show_tx.send(());
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut CaptureTray| {
                    t.shutdown.notify_one();
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// Register the tray. Returns the handle - keep it alive for the daemon's
/// lifetime and `shutdown()` it on exit - or `None` if registration failed.
pub async fn spawn(
    capture_tx: UnboundedSender<CaptureMode>,
    show_tx: UnboundedSender<()>,
    shutdown: Arc<Notify>,
    has_gui: bool,
) -> Option<Handle<CaptureTray>> {
    let tray = CaptureTray {
        capture_tx,
        show_tx,
        shutdown,
        has_gui,
        icons: app_icons(),
    };
    let sandboxed = capture_core::autostart::is_flatpak();
    match tray
        .disable_dbus_name(sandboxed)
        .assume_sni_available(true)
        .spawn()
        .await
    {
        Ok(handle) => {
            // "initialized", not "registered": with assume_sni_available this
            // can succeed before a watcher exists, registration following later.
            tracing::info!("system tray initialized");
            Some(handle)
        }
        Err(e) => {
            tracing::warn!(
                "no system tray available ({e}); continuing without it \
                 (use `capture-to-searchd capture`, or bind it to a hotkey)"
            );
            None
        }
    }
}

/// Resolve when the D-Bus session bus becomes unreachable, e.g. a logout that
/// tears it down. `main` uses this as a shutdown trigger so the daemon exits
/// with the session and the next login's autostart starts a fresh one rather
/// than lingering with a dead tray connection.
///
/// We only arm this once a tray has initialized, so a bus was present. A
/// persistent failure to reconnect is therefore itself evidence the bus is
/// gone, and after a brief retry we treat it as session loss rather than
/// hanging around forever.
pub async fn wait_for_session_bus_loss() {
    for attempt in 1..=5 {
        match zbus::Connection::session().await {
            Ok(conn) => {
                conn.closed().await;
                return;
            }
            Err(e) => {
                tracing::debug!("session-bus monitor: connect attempt {attempt} failed ({e})");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
    tracing::warn!("session-bus monitor: bus unreachable after retries; treating as session loss");
}

// --- icon embedding --------------------------------------------------------

/// The embedded app icon decoded into ARGB pixmaps at a few sizes, computed once.
fn app_icons() -> Vec<Icon> {
    static ICONS: OnceLock<Vec<Icon>> = OnceLock::new();
    ICONS
        .get_or_init(|| {
            const PNGS: &[&[u8]] = &[
                include_bytes!(
                    "../../data/icons/hicolor/24x24/apps/io.github.dipakmdhrm.CaptureToSearch.png"
                ),
                include_bytes!(
                    "../../data/icons/hicolor/32x32/apps/io.github.dipakmdhrm.CaptureToSearch.png"
                ),
                include_bytes!(
                    "../../data/icons/hicolor/48x48/apps/io.github.dipakmdhrm.CaptureToSearch.png"
                ),
            ];
            PNGS.iter().filter_map(|b| decode_png_argb(b)).collect()
        })
        .clone()
}

/// Decode a PNG into a ksni `Icon` (ARGB32, network byte order).
fn decode_png_argb(bytes: &[u8]) -> Option<Icon> {
    let mut decoder = png::Decoder::new(bytes);
    // Expand palette/grayscale/low-bit to 8-bit RGB(A); drop 16-bit to 8.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let pixels = &buf[..info.buffer_size()];
    let data = match info.color_type {
        png::ColorType::Rgba => rgba_to_argb(pixels),
        png::ColorType::Rgb => rgb_to_argb(pixels),
        _ => return None,
    };
    Some(Icon {
        width: info.width as i32,
        height: info.height as i32,
        data,
    })
}

/// RGBA8 -> ARGB32 network byte order (bytes `[A, R, G, B]` per pixel).
fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    out
}

/// RGB8 -> ARGB32 network byte order, fully opaque.
fn rgb_to_argb(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks_exact(3) {
        out.extend_from_slice(&[0xff, px[0], px[1], px[2]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_to_argb_reorders_channels() {
        assert_eq!(
            rgba_to_argb(&[0x11, 0x22, 0x33, 0xff]),
            vec![0xff, 0x11, 0x22, 0x33]
        );
    }

    #[test]
    fn rgb_to_argb_is_opaque() {
        assert_eq!(
            rgb_to_argb(&[0x11, 0x22, 0x33]),
            vec![0xff, 0x11, 0x22, 0x33]
        );
    }

    #[test]
    fn embedded_icons_decode() {
        // The bundled PNGs must actually decode, or we would ship a pixmap-less
        // tray that renders as a placeholder on GNOME.
        let icons = app_icons();
        assert!(!icons.is_empty(), "embedded app icons should decode");
        for icon in &icons {
            assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        }
    }

    #[test]
    fn every_hicolor_size_is_shipped_and_matches_its_directory() {
        // Icon themes resolve by directory name, so a 48x48 PNG living in
        // 32x32/ renders blurry or not at all. Packaging installs this tree
        // wholesale, and nothing else would catch a mismatch.
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/icons/hicolor");
        for size in [16u32, 24, 32, 48, 64, 128, 256, 512] {
            let path = data.join(format!("{size}x{size}/apps/{APP_ID}.png"));
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("missing icon {}: {e}", path.display()));
            let icon = decode_png_argb(&bytes)
                .unwrap_or_else(|| panic!("icon {} does not decode", path.display()));
            assert_eq!(
                (icon.width as u32, icon.height as u32),
                (size, size),
                "{} is {}x{} but sits in {size}x{size}/",
                path.display(),
                icon.width,
                icon.height
            );
        }
    }

    #[test]
    fn scalable_icons_are_shipped() {
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../data/icons/hicolor/scalable/apps");
        for name in [
            format!("{APP_ID}.svg"),
            // The symbolic variant is what adapts to light and dark panels.
            format!("{APP_ID}-symbolic.svg"),
        ] {
            let path = data.join(&name);
            let svg = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("missing {}: {e}", path.display()));
            assert!(svg.contains("<svg"), "{} is not an SVG", path.display());
        }
        let symbolic =
            std::fs::read_to_string(data.join(format!("{APP_ID}-symbolic.svg"))).unwrap();
        assert!(
            symbolic.contains("currentColor"),
            "the symbolic icon must inherit the panel's colour, not hardcode one"
        );
    }

    #[test]
    fn embedded_icons_have_transparency() {
        // A tray icon on an opaque square would look wrong on every panel.
        let icons = app_icons();
        let any_transparent = icons
            .iter()
            .any(|i| i.data.chunks_exact(4).any(|px| px[0] != 0xff));
        assert!(any_transparent, "tray icon should have an alpha channel");
    }
}
