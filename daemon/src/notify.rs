//! Desktop notifications for failures.
//!
//! Only failures are announced. A successful capture opens a browser tab, which
//! is its own confirmation - a notification on top of that would be noise.

use capture_core::{APP_ID, APP_NAME};

/// Show an error notification, honouring the `notify_on_error` config.
pub fn error(enabled: bool, body: &str) {
    if !enabled {
        return;
    }
    let result = notify_rust::Notification::new()
        .summary(&format!("{APP_NAME}: capture failed"))
        .body(body)
        .icon(APP_ID)
        .appname(APP_NAME)
        .timeout(notify_rust::Timeout::Milliseconds(6000))
        .show();
    if let Err(e) = result {
        // A missing notification daemon must never take the app down.
        tracing::debug!("could not show notification: {e}");
    }
}
