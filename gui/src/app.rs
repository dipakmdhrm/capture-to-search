//! The window: a Capture button, and a gear menu with Preferences and About.

use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use capture_core::ipc::{CaptureMode, IpcMessage};
use capture_core::{autostart, APP_ID, APP_NAME};

use crate::daemon_link;

pub fn build(application: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title(APP_NAME)
        .default_width(420)
        .default_height(340)
        .resizable(false)
        .build();

    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main menu")
        .menu_model(&app_menu())
        .build();

    let header = adw::HeaderBar::new();
    header.pack_end(&menu_button);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&content(&window, &toasts)));

    let layout = adw::ToolbarView::new();
    layout.add_top_bar(&header);
    layout.set_content(Some(&toasts));
    window.set_content(Some(&layout));

    install_menu_actions(&window);
    wire_daemon_messages(&window);

    // Closing frees the process. This app is one button, so respawning costs
    // almost nothing, and a resident GTK process would dominate the idle
    // footprint of something whose whole purpose is to wait in the tray.
    window.connect_close_request(|_| {
        // Best-effort: let the daemon drop its handle promptly rather than
        // waiting to reap us.
        let _ = daemon_link::request(IpcMessage::WindowClosing);
        glib::Propagation::Proceed
    });

    window.present();
}

/// The centred icon, title, and Capture button.
fn content(window: &adw::ApplicationWindow, toasts: &adw::ToastOverlay) -> gtk::Box {
    let icon = gtk::Image::from_icon_name(APP_ID);
    icon.set_pixel_size(96);

    let title = gtk::Label::new(Some(APP_NAME));
    title.add_css_class("title-1");

    let subtitle = gtk::Label::new(Some(
        "Capture part of your screen and search it with Google Lens",
    ));
    subtitle.add_css_class("dim-label");
    subtitle.set_wrap(true);
    subtitle.set_justify(gtk::Justification::Center);
    subtitle.set_max_width_chars(38);

    let capture = gtk::Button::with_label("Capture");
    capture.add_css_class("suggested-action");
    capture.add_css_class("pill");
    capture.set_halign(gtk::Align::Center);

    let win = window.clone();
    let overlay = toasts.clone();
    capture.connect_clicked(move |button| {
        // Disable immediately: the daemon is about to close this window, and a
        // second click in that gap would queue a capture that is then dropped.
        button.set_sensitive(false);
        match daemon_link::request(IpcMessage::CaptureRequest {
            mode: CaptureMode::Interactive,
        }) {
            // The daemon dismisses this window before capturing, so on success
            // there is nothing further to do here.
            Ok(_) => {}
            Err(e) => {
                button.set_sensitive(true);
                let toast = adw::Toast::new(&format!("Could not start capture: {e}"));
                toast.set_timeout(6);
                overlay.add_toast(toast);
                tracing::warn!("capture request failed: {e:#}");
            }
        }
        let _ = &win;
    });

    let hint = gtk::Label::new(Some(
        "Tip: bind a keyboard shortcut to  capture-to-searchd capture --area",
    ));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_wrap(true);
    hint.set_justify(gtk::Justification::Center);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_valign(gtk::Align::Center);
    column.set_halign(gtk::Align::Center);
    column.set_margin_top(24);
    column.set_margin_bottom(24);
    column.set_margin_start(24);
    column.set_margin_end(24);
    column.append(&icon);
    column.append(&title);
    column.append(&subtitle);
    column.append(&capture);
    column.append(&hint);
    column
}

fn app_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("Preferences"), Some("appmenu.preferences"));
    menu.append(Some(&format!("About {APP_NAME}")), Some("appmenu.about"));
    menu
}

/// Install the gear menu's action group so its menu model can resolve
/// `appmenu.preferences` / `appmenu.about`.
fn install_menu_actions(window: &adw::ApplicationWindow) {
    let group = gio::SimpleActionGroup::new();

    let preferences = gio::SimpleAction::new("preferences", None);
    let win = window.clone();
    preferences.connect_activate(move |_, _| show_preferences(&win));
    group.add_action(&preferences);

    let about = gio::SimpleAction::new("about", None);
    let win = window.clone();
    about.connect_activate(move |_, _| show_about(&win));
    group.add_action(&about);

    window.insert_action_group("appmenu", Some(&group));
}

/// Preferences: launch at startup.
fn show_preferences(window: &adw::ApplicationWindow) {
    let startup = adw::SwitchRow::new();
    startup.set_title("Launch at startup");
    startup.set_subtitle("Start the tray icon when you log in");
    // The autostart .desktop file on disk is the source of truth, so the switch
    // reflects reality even if it was changed outside the app.
    startup.set_active(autostart::is_enabled());

    let group = adw::PreferencesGroup::new();
    group.set_title("General");
    group.add(&startup);

    let page = adw::PreferencesPage::new();
    page.add(&group);

    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);

    let dlg = dialog.clone();
    startup.connect_active_notify(move |row| {
        let enabled = row.is_active();
        if let Err(e) = autostart::set_enabled(enabled) {
            // Put the switch back: leaving it on while the file was never
            // written would misreport the actual state.
            row.set_active(!enabled);
            let toast = adw::Toast::new(&format!("Could not change autostart: {e}"));
            toast.set_timeout(6);
            dlg.add_toast(toast);
            tracing::warn!("autostart toggle failed: {e:#}");
        }
    });

    dialog.present(Some(window));
}

fn show_about(window: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name(APP_NAME)
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("dipakmdhrm")
        .license_type(gtk::License::MitX11)
        .website("https://github.com/dipakmdhrm/capture-to-search")
        .issue_url("https://github.com/dipakmdhrm/capture-to-search/issues")
        .comments(
            "Capture a region of your screen and search it with Google Lens.\n\n\
             A tray-resident daemon owns the capture, the upload, and opening \
             your browser; this window is spawned on demand and closed again to \
             keep the idle footprint small.",
        )
        .build();
    about.add_credit_section(
        Some("Built on"),
        &["GTK4", "libadwaita", "ksni", "xdg-desktop-portal"],
    );
    about.present(Some(window));
}

/// Drain messages the daemon pushes to us.
fn wire_daemon_messages(window: &adw::ApplicationWindow) {
    let (tx, rx) = async_channel::unbounded::<IpcMessage>();
    daemon_link::listen(tx);

    let win = window.clone();
    glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            match msg {
                // The tray's "Open window" while we are already open.
                IpcMessage::ShowWindow => win.present(),
                other => tracing::debug!("ignoring daemon message: {other:?}"),
            }
        }
    });
}
