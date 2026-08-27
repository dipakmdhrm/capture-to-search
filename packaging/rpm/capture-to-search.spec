# The release profile already strips the binaries (Cargo.toml
# [profile.release] strip = true), so there are no useful symbols to package.
# Disabling the debug packages skips find-debuginfo entirely, which drops the
# unused -debuginfo/-debugsource RPMs and shortens the build.
%global debug_package %{nil}

# Cargo target dir. Defaults to the in-tree "target" (unchanged local build);
# CI can override with --define "cargo_target <path>" to a cached location so
# dependency builds are reused across releases.
%{!?cargo_target: %global cargo_target target}

%global appid io.github.dipakmdhrm.CaptureToSearch

Name:           capture-to-search
Version:        @VERSION@
Release:        1%{?dist}
Summary:        Search any part of your screen with Google Lens
License:        MIT
URL:            https://github.com/dipakmdhrm/capture-to-search
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)

Requires:       gtk4
Requires:       libadwaita

# Capture needs at least one backend. The portal covers most desktops; the
# command-line tools are fallbacks for hosts without one.
Recommends:     xdg-desktop-portal
Recommends:     xdg-utils
Suggests:       gnome-screenshot
Suggests:       grim
Suggests:       slurp

%description
Capture to Search sits in the system tray. Press Capture, drag out a region of
the screen, and your browser opens on the Google Lens results for whatever you
selected.

A small resident daemon (capture-to-searchd) owns the tray icon and the capture
pipeline; the GTK4 window (capture-to-search-gui) is spawned on demand and
closed again, so almost nothing stays resident while the app waits in the tray.

Capture works on Wayland and X11: the XDG screenshot portal is tried first, then
grim, spectacle, gnome-screenshot and other tools as fallbacks.

%prep
%autosetup

%build
cargo build --release --workspace --locked --target-dir %{cargo_target}

%install
install -Dm 755 %{cargo_target}/release/capture-to-searchd \
    %{buildroot}%{_bindir}/capture-to-searchd
install -Dm 755 %{cargo_target}/release/capture-to-search-gui \
    %{buildroot}%{_bindir}/capture-to-search-gui
install -Dm 644 data/applications/%{appid}.desktop \
    %{buildroot}%{_datadir}/applications/%{appid}.desktop
install -Dm 644 data/metainfo/%{appid}.metainfo.xml \
    %{buildroot}%{_datadir}/metainfo/%{appid}.metainfo.xml

for size in 16 24 32 48 64 128 256 512; do
    install -Dm 644 data/icons/hicolor/${size}x${size}/apps/%{appid}.png \
        %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/%{appid}.png
done
install -Dm 644 data/icons/hicolor/scalable/apps/%{appid}.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{appid}.svg
install -Dm 644 data/icons/hicolor/scalable/apps/%{appid}-symbolic.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{appid}-symbolic.svg

%files
%license LICENSE
%doc README.md CHANGELOG.md
%{_bindir}/capture-to-searchd
%{_bindir}/capture-to-search-gui
%{_datadir}/applications/%{appid}.desktop
%{_datadir}/metainfo/%{appid}.metainfo.xml
# Listed explicitly rather than globbed: `%{appid}.*` silently fails to match
# `%{appid}-symbolic.svg`, and rpmbuild turns an unpackaged installed file into
# a hard error.
%{_datadir}/icons/hicolor/*/apps/%{appid}.png
%{_datadir}/icons/hicolor/scalable/apps/%{appid}.svg
%{_datadir}/icons/hicolor/scalable/apps/%{appid}-symbolic.svg

%post
update-desktop-database %{_datadir}/applications 2>/dev/null || true
gtk-update-icon-cache -f -t %{_datadir}/icons/hicolor 2>/dev/null || true

%preun
# $1 == 0 on a full uninstall, not an upgrade. On upgrade the running daemon is
# left alone: it re-execs onto the new binary itself (self_update.rs), so the
# tray icon survives.
if [ "$1" -eq 0 ]; then
    pkill -x capture-to-searchd 2>/dev/null || true
    pkill -x capture-to-search-gui 2>/dev/null || true
fi

%postun
if [ "$1" -eq 0 ]; then
    for home_dir in /home/*; do
        rm -f "$home_dir/.config/autostart/capture-to-search.desktop" 2>/dev/null || true
    done
    update-desktop-database %{_datadir}/applications 2>/dev/null || true
    gtk-update-icon-cache -f -t %{_datadir}/icons/hicolor 2>/dev/null || true
fi

%changelog
* @CHANGELOG_DATE@ dipakmdhrm <dipakmdhrm@gmail.com> - @VERSION@-1
- Release @VERSION@
