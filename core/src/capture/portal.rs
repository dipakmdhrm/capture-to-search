//! `org.freedesktop.portal.Screenshot` via zbus.
//!
//! The portal is the primary capture backend: it is the only one that works
//! under a strict Wayland compositor, and it works on X11 too, so a single code
//! path covers most modern desktops.
//!
//! Portal methods are an async Request/Response pair: the method returns a
//! request-handle object path, and the real outcome arrives later as a
//! `Response` signal on that handle. To avoid a race where the signal fires
//! before we subscribe, we pass an explicit `handle_token`, precompute the
//! handle path, and start listening *before* issuing the call - the pattern the
//! portal documentation prescribes.

use std::collections::HashMap;

use anyhow::{anyhow, Context};
use futures::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::capture::CaptureError;
use crate::ipc::CaptureMode;

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENSHOT_IFACE: &str = "org.freedesktop.portal.Screenshot";

/// Is the Screenshot portal actually usable on this session?
///
/// Checks that the portal service is on the bus *and* exposes the Screenshot
/// interface. A desktop can have `xdg-desktop-portal` installed with a backend
/// that implements no Screenshot at all, so probing the interface rather than
/// the process is what makes this trustworthy. The error string is surfaced
/// verbatim by `doctor`.
pub async fn probe() -> std::result::Result<(), String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("no D-Bus session bus ({e})"))?;
    let proxy = zbus::Proxy::new(&conn, PORTAL_DEST, PORTAL_PATH, SCREENSHOT_IFACE)
        .await
        .map_err(|e| format!("xdg-desktop-portal not reachable ({e})"))?;
    match proxy.get_property::<u32>("version").await {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("portal has no Screenshot interface ({e})")),
    }
}

/// Take a screenshot through the portal, returning the encoded image bytes.
///
/// `Screen` mode requests a non-interactive shot. Most desktops (GNOME among
/// them) refuse that for an unsandboxed app, and we deliberately do *not* retry
/// interactively here: the caller asked for a capture with no prompt, so
/// popping a selection picker would answer a different question. Reporting the
/// failure lets the backend chain hand off to a tool that can do full-screen
/// without a prompt.
pub async fn capture(mode: CaptureMode) -> std::result::Result<Vec<u8>, CaptureError> {
    capture_with(!matches!(mode, CaptureMode::Screen)).await
}

async fn capture_with(interactive: bool) -> std::result::Result<Vec<u8>, CaptureError> {
    let conn = zbus::Connection::session()
        .await
        .context("connect to session bus")?;

    // Precompute the request-handle path from our unique bus name plus a token,
    // so we can subscribe to Response before the call can emit it.
    let unique = conn
        .unique_name()
        .context("session connection has no unique name")?
        .to_string();
    let sender = unique.trim_start_matches(':').replace('.', "_");
    // Unique per call: two captures in flight must not share a handle.
    let token = format!("cts_{}_{}", std::process::id(), monotonic_token());
    let handle_path = format!("{PORTAL_PATH}/request/{sender}/{token}");

    let request = zbus::Proxy::new(
        &conn,
        PORTAL_DEST,
        handle_path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .await
    .context("build Request proxy")?;
    let mut responses = request
        .receive_signal("Response")
        .await
        .context("subscribe to portal Response")?;

    let screenshot = zbus::Proxy::new(&conn, PORTAL_DEST, PORTAL_PATH, SCREENSHOT_IFACE)
        .await
        .context("build Screenshot proxy")?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("modal", Value::from(true));
    // The portal has no "region only" flag. `interactive` hands over to the
    // desktop's own capture UI, which already offers area / window / screen -
    // so the user's area-vs-screen choice is served without us drawing an
    // overlay.
    options.insert("interactive", Value::from(interactive));

    let returned: OwnedObjectPath = screenshot
        .call("Screenshot", &("", options))
        .await
        .context("Screenshot call failed")?;
    if returned.as_str() != handle_path {
        tracing::debug!(
            "portal returned handle {} (expected {handle_path})",
            returned.as_str()
        );
    }

    let signal = responses
        .next()
        .await
        .context("portal closed without a Response")?;
    let (response, results): (u32, HashMap<String, OwnedValue>) = signal
        .body()
        .deserialize()
        .context("decode portal Response")?;

    // 0 = success, 1 = user cancelled, 2 = other error.
    match response {
        0 => {}
        1 => return Err(CaptureError::Cancelled),
        other => {
            // Seen on GNOME 46: the shell captures the image, saves it to
            // ~/Pictures/Screenshots, and still reports no file back
            // ("InteractiveScreenshot didn't return a file"). Report it as a
            // backend failure so the caller can try the next backend.
            return Err(CaptureError::Failed(anyhow!(
                "portal returned error response {other} (the desktop's screenshot \
                 backend did not hand back a file)"
            )));
        }
    }

    let uri = results
        .get("uri")
        .and_then(|v| String::try_from(v.clone()).ok())
        .context("portal Response carried no uri")
        .map_err(CaptureError::Failed)?;
    let path = uri
        .strip_prefix("file://")
        .map(percent_decode)
        .unwrap_or_else(|| uri.clone());

    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("reading portal screenshot from {path}"))
        .map_err(CaptureError::Failed)?;

    // Only clean up files the portal staged in a temp or cache location.
    //
    // GNOME's portal saves interactive screenshots into the user's
    // ~/Pictures/Screenshots and hands back *that* path - deleting it would
    // silently destroy the user's own screenshot library. Privacy hygiene for
    // our staging files must not extend to files that belong to the user.
    if is_disposable(std::path::Path::new(&path)) {
        let _ = tokio::fs::remove_file(&path).await;
    } else {
        tracing::debug!("leaving portal screenshot in place (user-owned): {path}");
    }

    if bytes.is_empty() {
        return Err(CaptureError::Failed(anyhow!("portal screenshot was empty")));
    }
    tracing::debug!("portal returned {} bytes from {path}", bytes.len());
    Ok(bytes)
}

/// Is this a scratch file we may delete, rather than one belonging to the user?
fn is_disposable(path: &std::path::Path) -> bool {
    let mut roots: Vec<std::path::PathBuf> = vec!["/tmp".into(), "/var/tmp".into()];
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        roots.push(d.into());
    }
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(d) => roots.push(d.into()),
        None => {
            if let Some(home) = std::env::var_os("HOME") {
                roots.push(std::path::PathBuf::from(home).join(".cache"));
            }
        }
    }
    roots.iter().any(|r| path.starts_with(r))
}

/// A per-call counter so concurrent captures get distinct handle tokens.
fn monotonic_token() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Minimal percent-decoding for the `file://` URI the portal hands back.
///
/// Paths with spaces or non-ASCII characters arrive percent-encoded, and a
/// screenshot saved under, say, `~/Pictures/Скриншоты/` would otherwise fail to
/// open with a confusing "no such file" error.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding_handles_spaces_and_utf8() {
        assert_eq!(percent_decode("/tmp/a%20b.png"), "/tmp/a b.png");
        assert_eq!(percent_decode("/tmp/plain.png"), "/tmp/plain.png");
        // Cyrillic 'Ск' - a real path under a non-English locale.
        assert_eq!(percent_decode("/x/%D0%A1%D0%BA.png"), "/x/Ск.png");
    }

    #[test]
    fn percent_decoding_leaves_trailing_stray_percent_alone() {
        // Must not panic or truncate on malformed input.
        assert_eq!(percent_decode("/tmp/100%"), "/tmp/100%");
        assert_eq!(percent_decode("/tmp/%zz.png"), "/tmp/%zz.png");
    }

    #[test]
    fn user_screenshots_are_never_deleted() {
        // GNOME's portal saves interactive screenshots into the user's
        // ~/Pictures/Screenshots and returns that path. Deleting it would
        // destroy the user's own screenshot library.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/someone".into());
        let user_shot = std::path::PathBuf::from(&home)
            .join("Pictures/Screenshots/Screenshot from 2026-08-27.png");
        assert!(
            !is_disposable(&user_shot),
            "must not delete a screenshot in the user's Pictures"
        );
        assert!(!is_disposable(std::path::Path::new(
            "/home/someone/Documents/x.png"
        )));
    }

    #[test]
    fn scratch_files_are_cleaned_up() {
        // Our own staging files are screen contents and must not linger.
        assert!(is_disposable(std::path::Path::new("/tmp/capture-1.png")));
        assert!(is_disposable(std::path::Path::new(
            "/var/tmp/capture-1.png"
        )));
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            let p = std::path::PathBuf::from(rt).join("capture-to-search/x.png");
            assert!(is_disposable(&p));
        }
    }

    #[test]
    fn handle_tokens_are_unique_per_call() {
        // A shared token would cross-wire two concurrent captures onto one
        // request handle.
        assert_ne!(monotonic_token(), monotonic_token());
    }
}
