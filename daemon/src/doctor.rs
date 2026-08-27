//! `doctor`: report what was detected, and why each rejected option was rejected.
//!
//! With this many capture backends across this many desktops, "it doesn't work
//! on my distro" is unanswerable without a way to see the decisions the app
//! made. The rejection reasons come from the same probes the selection chain
//! uses, so this reflects the real behaviour rather than a parallel guess.

use std::time::Duration;

use capture_core::capture::{self, Probe, SessionEnv};
use capture_core::{autostart, paths, Config, APP_SLUG};

use crate::single_instance;
use crate::window_proc;

/// Width of the label column.
const LABEL: usize = 20;

pub async fn run() -> anyhow::Result<()> {
    let cfg = paths::config_path()
        .ok()
        .map(|p| Config::load(&p).unwrap_or_default())
        .unwrap_or_default();
    let env = SessionEnv::detect();

    section("Session");
    row("type", env.session_type.as_str());
    row(
        "desktop",
        if env.desktop.is_empty() {
            "(unset)"
        } else {
            &env.desktop
        },
    );
    match (&env.display, env.is_wayland()) {
        (Some(d), true) => row(
            "DISPLAY",
            &format!("{d}   (XWayland - X11 backends disabled)"),
        ),
        (Some(d), false) => row("DISPLAY", d),
        (None, _) => row("DISPLAY", "(unset)"),
    }
    if let Some(w) = &env.wayland_display {
        row("WAYLAND_DISPLAY", w);
    }

    section("Capture backends");
    let (probes, selected) = capture::report(&env, cfg.capture_backend.as_deref()).await;
    for (backend, probe) in &probes {
        let is_selected = selected.as_ref() == Some(backend);
        let marker = if is_selected { ">" } else { " " };
        let (state, detail) = match probe {
            Probe::Available if is_selected => ("SELECTED", String::new()),
            Probe::Available => ("available", String::new()),
            Probe::Unavailable(why) => ("unavailable", why.clone()),
            Probe::Disabled(why) => ("disabled", why.clone()),
        };
        let line = format!(
            "  {marker} {:<width$}  {:<12} {}",
            backend.name(),
            state,
            detail,
            width = LABEL
        );
        println!("{}", line.trim_end());
    }
    if selected.is_none() {
        println!("\n  No usable capture backend. Install one of the tools above,");
        println!("  or xdg-desktop-portal with a backend for your desktop.");
    }
    if let Some(pinned) = &cfg.capture_backend {
        println!("\n  (pinned via config: capture_backend = \"{pinned}\")");
    }

    section("Tray");
    match sni_watcher_present().await {
        Ok(true) => row("SNI watcher", "present (org.kde.StatusNotifierWatcher)"),
        Ok(false) => row(
            "SNI watcher",
            "ABSENT - no tray icon will appear. On GNOME install the \
             AppIndicator extension, or bind a hotkey to `capture-to-searchd capture`.",
        ),
        Err(e) => row("SNI watcher", &format!("unknown ({e})")),
    }

    section("Window");
    match window_proc::gui_path() {
        Some(p) => row("GUI binary", &p.display().to_string()),
        None => row(
            "GUI binary",
            "not installed (daemon-only; tray and `capture` still work)",
        ),
    }

    section("Browser");
    match std::env::var("BROWSER").ok().filter(|s| !s.is_empty()) {
        Some(b) => row("BROWSER", &b),
        None => row("BROWSER", "(unset)"),
    }
    row(
        "xdg-open",
        &which("xdg-open").unwrap_or_else(|| "NOT FOUND - results cannot be opened".into()),
    );

    section("Autostart");
    let path = autostart::desktop_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|e| format!("(unresolvable: {e})"));
    row(
        "enabled",
        if autostart::is_enabled() { "yes" } else { "no" },
    );
    row("entry", &path);
    if autostart::is_flatpak() {
        row(
            "flatpak",
            "yes - autostart needs the Background portal (not wired up)",
        );
    }

    section("Daemon");
    match paths::daemon_socket_path() {
        Ok(socket) => {
            let alive = single_instance::probe_alive(&socket).await;
            row("status", if alive { "running" } else { "not running" });
            row("socket", &socket.display().to_string());
        }
        Err(e) => row("socket", &format!("(unresolvable: {e})")),
    }

    section("Config");
    row(
        "path",
        &paths::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("(unresolvable: {e})")),
    );
    row("max_upload_edge", &cfg.max_upload_edge.to_string());
    row("lens_endpoint", &cfg.lens_endpoint);

    section("Network");
    // The browser performs the upload, so this only confirms the host is
    // reachable from this machine at all.
    row("lens host", &probe_endpoint(&cfg.lens_endpoint).await);

    println!();
    println!("Bind a desktop hotkey to:  {APP_SLUG}d capture --area");
    println!();
    Ok(())
}

fn section(title: &str) {
    println!("\n{title}");
}

fn row(label: &str, value: &str) {
    println!("  {:<width$}  {}", label, value, width = LABEL);
}

/// Is a StatusNotifier host running? Without one, no tray icon appears no
/// matter how correct our registration is.
async fn sni_watcher_present() -> anyhow::Result<bool> {
    let conn = zbus::Connection::session().await?;
    let dbus = zbus::Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await?;
    let has: bool = dbus
        .call("NameHasOwner", &("org.kde.StatusNotifierWatcher",))
        .await?;
    Ok(has)
}

/// Can we reach the upload host at all? Distinguishes "Lens changed" from
/// "this machine has no network". The actual upload happens in the browser.
async fn probe_endpoint(endpoint: &str) -> String {
    let origin = match endpoint.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split('/').next().unwrap_or(rest);
            format!("{scheme}://{host}/")
        }
        None => endpoint.to_string(),
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
    {
        Ok(c) => c,
        Err(e) => return format!("client error ({e})"),
    };
    match client.get(&origin).send().await {
        Ok(resp) => format!("{origin} reachable (HTTP {})", resp.status().as_u16()),
        Err(e) => format!("{origin} UNREACHABLE ({e})"),
    }
}

fn which(bin: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|c| c.is_file())
        .map(|p| p.display().to_string())
}
