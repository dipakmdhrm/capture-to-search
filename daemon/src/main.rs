//! capture-to-searchd - the Capture to Search daemon and CLI.
//!
//! With no arguments this runs as the resident daemon: it owns the tray icon,
//! the capture pipeline, and the on-demand window child, and stays up until it
//! is killed or Quit is chosen. Two subcommands make the app usable without a
//! tray or a window:
//!
//! - `capture` runs one capture end to end and exits. Bind it to a desktop
//!   keyboard shortcut - that is the portable way to get a global hotkey, since
//!   Wayland offers no reliable way for an application to grab one itself.
//! - `doctor` prints what was detected and why, which is what makes a bug
//!   report from an unfamiliar distro actionable.

mod doctor;
mod flow;
mod notify;
mod server;
mod single_instance;
mod state;
mod tray;
mod window_proc;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use capture_core::ipc::CaptureMode;
use capture_core::{paths, Config};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::Notify;

use single_instance::Acquired;
use state::AppCtx;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `--version` short-circuits before any setup.
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{}", capture_core::version_blurb("capture-to-searchd"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    // `doctor` prints a report; its own INFO lines would interleave with the
    // table it is producing. Everything else defaults to info.
    let default_level = if args.first().map(String::as_str) == Some("doctor") {
        "warn"
    } else {
        "info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_level.into()),
        )
        .init();
    capture_core::install_default_crypto();

    match args.first().map(String::as_str) {
        Some("doctor") => doctor::run().await,
        Some("capture") => run_one_shot(parse_mode(&args)).await,
        _ => run_daemon(args.iter().any(|a| a == "--show-window")).await,
    }
}

fn print_help() {
    println!(
        "\
{name} {version}

USAGE:
    capture-to-searchd [--show-window]   Run the daemon (tray + capture)
    capture-to-searchd capture [MODE]    Capture once and exit
    capture-to-searchd doctor            Print environment diagnostics

CAPTURE MODES:
    --area          Drag out a region
    --screen        Whole screen, no prompt
    (default)       Let the desktop's own picker choose

Bind `capture-to-searchd capture --area` to a keyboard shortcut in your
desktop's settings for a global hotkey.",
        name = capture_core::APP_NAME,
        version = env!("CARGO_PKG_VERSION"),
    );
}

fn parse_mode(args: &[String]) -> CaptureMode {
    if args.iter().any(|a| a == "--screen") {
        CaptureMode::Screen
    } else if args.iter().any(|a| a == "--area") {
        CaptureMode::Area
    } else {
        CaptureMode::Interactive
    }
}

/// `capture`: hand off to a running daemon, or do it here.
///
/// Routing through the daemon when one exists is what keeps a hotkey press and
/// a tray click on the same code path, and stops two captures racing.
async fn run_one_shot(mode: CaptureMode) -> Result<()> {
    let socket = paths::daemon_socket_path()?;
    if single_instance::probe_alive(&socket).await {
        tracing::debug!("daemon is running; delegating capture to it");
        return single_instance::request_capture(&socket, mode).await;
    }
    tracing::debug!("no daemon running; capturing in-process");
    match flow::run_standalone(mode).await? {
        // The staged page is left in place: the browser may not have loaded it
        // yet, and this process is about to exit. The next capture sweeps it up.
        Some(path) => tracing::info!("handed capture to the browser: {}", path.display()),
        None => tracing::info!("capture cancelled"),
    }
    Ok(())
}

async fn run_daemon(show_window: bool) -> Result<()> {
    let socket_path = paths::daemon_socket_path()?;

    // Exactly one daemon. A second launch asks the running one to show its
    // window (this is what clicking the app's desktop entry does) and exits.
    let listener = match single_instance::acquire(&socket_path).await? {
        Acquired::Primary(listener) => listener,
        Acquired::AlreadyRunning => {
            tracing::info!("daemon already running; asking it to show the window");
            single_instance::request_show_window(&socket_path).await?;
            return Ok(());
        }
    };
    tracing::info!(
        "capture-to-searchd {} listening on {}",
        env!("CARGO_PKG_VERSION"),
        socket_path.display()
    );

    let config = Config::load(&paths::config_path()?).context("loading config")?;

    let (capture_tx, capture_rx) = mpsc::unbounded_channel::<CaptureMode>();
    let (show_tx, show_rx) = mpsc::unbounded_channel::<()>();
    let shutdown = Arc::new(Notify::new());

    let ctx = AppCtx {
        config: Arc::new(RwLock::new(config)),
        window: Arc::new(Mutex::new(None)),
        window_alive: Arc::new(AtomicBool::new(false)),
        window_kill: Arc::new(Mutex::new(None)),
        window_gone: Arc::new(Notify::new()),
        capture_tx: capture_tx.clone(),
        capturing: Arc::new(AtomicBool::new(false)),
    };

    // Best-effort tray; the daemon runs fine without one.
    let has_gui = window_proc::gui_path().is_some();
    if !has_gui {
        tracing::info!("no GUI binary found; running daemon-only (tray + capture)");
    }
    let tray_handle = tray::spawn(capture_tx, show_tx, shutdown.clone(), has_gui).await;

    // Exit with the graphical session: a logout that tears down the session bus
    // should end the daemon so the next login's autostart brings up a fresh one,
    // rather than leaving it alive with a dead tray connection. Only armed once
    // a tray initialized, which proves a bus was there to begin with.
    if tray_handle.is_some() {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            tray::wait_for_session_bus_loss().await;
            tracing::info!("D-Bus session bus lost (logout?); shutting down");
            shutdown.notify_one();
        });
    }

    let server_task = tokio::spawn(server::run(listener, ctx.clone()));
    let capture_task = tokio::spawn(run_capture_handler(ctx.clone(), capture_rx));
    let show_task = tokio::spawn(run_show_handler(ctx.clone(), show_rx));

    if show_window {
        window_proc::show(&ctx);
    }

    wait_for_shutdown(&shutdown).await;
    tracing::info!("shutting down");

    window_proc::kill(&ctx);
    if let Some(handle) = tray_handle {
        // Bounded: if we're shutting down *because* the session bus died, the
        // tray's own D-Bus teardown would otherwise hang here.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle.shutdown()).await;
    }
    server_task.abort();
    capture_task.abort();
    show_task.abort();
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// Serialize capture requests: one at a time, in arrival order.
async fn run_capture_handler(ctx: AppCtx, mut rx: UnboundedReceiver<CaptureMode>) {
    while let Some(mode) = rx.recv().await {
        flow::run(&ctx, mode).await;
    }
}

async fn run_show_handler(ctx: AppCtx, mut rx: UnboundedReceiver<()>) {
    while rx.recv().await.is_some() {
        window_proc::show(&ctx);
    }
}

/// Resolve on SIGINT/SIGTERM or the tray's Quit.
async fn wait_for_shutdown(shutdown: &Notify) {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = sigint.recv() => {}
        _ = sigterm.recv() => {}
        _ = shutdown.notified() => {}
    }
}
