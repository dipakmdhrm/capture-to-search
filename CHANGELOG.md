# Changelog

All notable changes to Capture to Search are documented here. The format
loosely follows [Keep a Changelog](https://keepachangelog.com/). The project is
pre-release (`0.1.0`).

## Unreleased

### Added

- **Packages for Debian/Ubuntu, Fedora and Arch.** A single package per format
  installs both binaries, the icons, the desktop entry and the AppStream
  metainfo, and removing it also clears the per-user autostart entry that only
  packaging can reach. `packaging/build-local.sh` builds all three without CI:
  the `.deb` natively, the `.rpm` and Arch package inside `fedora` and
  `archlinux` containers, with output in `dist/`.

- **Install without root.** `install.sh` and `uninstall.sh` place everything
  under `~/.local`, and install the window only if it was built, so a host
  without GTK still gets a working tray and hotkey.

- **Upgrades no longer interrupt the tray.** A package upgrade replaces the
  daemon binary underneath the running process, which would otherwise keep
  executing the old code until the next login. The daemon now notices its
  binary changed and re-execs onto the new version a few seconds later, so the
  tray icon stays put and the new version takes effect immediately. Release
  builds only, so a development rebuild does not bounce a daemon being
  debugged.

- **Continuous integration and automated releases.** Every pull request runs
  formatting, lint and the test suite, then builds the `.deb`, `.rpm` and Arch
  packages and installs each one to confirm it works. Merging to main bumps the
  version, stamps the changelog, tags, and publishes a GitHub Release with all
  three packages attached; the bump is taken from a `release:*` label on the
  merged pull request and defaults to a patch.

- The test suite is now **106 tests**, adding coverage for the binary-signature
  check behind the upgrade restart and for drift between the app and its
  packaging - a rename that reaches the code but not the package definitions
  produces a package that installs cleanly and then does nothing.

### Known issues

- **The packages depend on GTK 4.14 and libadwaita 1.5**, because a single
  package ships both binaries. They therefore do not install on Debian 12 or
  Ubuntu 22.04, even though the daemon itself links neither library.

- **There is still no Flatpak package.** Beyond the autostart limitation noted
  under 0.1.0, the staged upload page lives in the sandbox's runtime directory,
  which the host browser cannot read; it may survive the OpenURI portal's
  document-portal re-export, but that is unverified. Inside a sandbox only the
  portal capture backend is reachable.

## 0.1.0

Initial implementation: a tray-resident daemon that captures a region of the
screen and searches it with Google Lens.

### Added

- **Tray-only daemon.** `capture-to-searchd` runs until killed, owning the
  StatusNotifierItem tray icon, the capture pipeline, and an on-demand window
  child. Left-clicking the tray captures; the menu offers Capture, Capture full
  screen, Open window, and Quit. A single Unix socket in `$XDG_RUNTIME_DIR`
  enforces one daemon and carries daemon-to-window IPC. Registration is
  best-effort: on a host with no tray host the daemon keeps running and capture
  stays reachable from the command line.

- **Capture with runtime backend detection.** An ordered chain is probed and the
  first usable backend wins: the `xdg-desktop-portal` Screenshot interface,
  then `grim`+`slurp`, `spectacle`, `gnome-screenshot`, `xfce4-screenshooter`,
  `flameshot`, `maim`, `scrot`, and `import`. The portal is tried first because
  it is the only backend that works under a strict Wayland compositor. A
  backend that fails hands off to the next one; a user cancelling ends the
  attempt instead of cascading through every remaining backend and popping a
  fresh selection overlay for each.

- **GTK4 window, spawned on demand.** `capture-to-search-gui` is a single
  Capture button plus a menu with Preferences (Launch at startup) and About.
  The daemon spawns it on request and terminates it when closed, so nothing
  GTK-sized stays resident while the app waits in the tray. The daemon links no
  GUI toolkit at all, so it builds and runs on hosts without GTK; when the GUI
  binary is absent the tray simply omits its window entry.

- **`capture` subcommand.** Runs one capture end to end and exits. This is how
  you get a global hotkey: bind it in your desktop's keyboard settings, since
  Wayland offers no reliable way for an application to grab one itself. With a
  daemon running the request is handed to it, so a hotkey press and a tray
  click follow the same code path; without one, the capture happens in-process.

- **`doctor` subcommand.** Prints the selected capture backend and, for every
  other one, the reason it was rejected, plus session, tray, browser,
  autostart, daemon, and network state. With this many backends across this
  many desktops, it is the difference between a bug report being actionable and
  being a week of messages.

- **Launch at startup.** A Preferences toggle writing
  `~/.config/autostart/capture-to-search.desktop`. The file on disk is the
  single source of truth, so the switch reflects reality even when changed
  outside the app.

- **Configuration** at `~/.config/capture-to-search/config.toml`:
  `max_upload_edge`, `capture_backend` (pin one, disabling the fallback),
  `keep_captures`, `notify_on_error`, and `lens_endpoint`. A missing file
  yields defaults, and an unknown key from a newer version still loads.

- **Test suite of 92 tests**, hermetic by construction: no display, D-Bus
  session, network, or writable XDG directory required, so it runs unchanged on
  a bare CI runner. Environment-dependent logic is split into pure functions
  taking their inputs as arguments, so rules are asserted directly rather than
  by mutating process state.

### Fixed

These were all found against a real desktop during development. They are
recorded because each encodes a platform behaviour that is easy to reintroduce.

- **Lens showed an empty query image for every capture.** The daemon uploaded
  the capture itself, read the `303` redirect, and opened the resulting URL.
  That produces a valid-looking URL with a correct `vsrid` and `vsdim`, and it
  does not work: Google scopes an uploaded image to the client session that
  uploaded it. Opening the URL from any other client renders the search shell
  with no image, and presenting its `gsessionid`/`lsessionid` without matching
  cookies is refused outright with `403`. Confirmed against a freshly minted
  URL, so it is session binding rather than expiry.

  The daemon no longer uploads. It stages a self-contained HTML page that
  rebuilds the capture as a `File`, attaches it to a real file input, and
  submits a normal multipart form POST, then opens that page. The browser sends
  its own cookies and follows the redirect, exactly as `lens.google.com` does
  natively, so the session that uploads is the session that views.

- **The app deleted the user's screenshot library.** GNOME's portal saves
  interactive screenshots into `~/Pictures/Screenshots` and returns that path.
  Cleaning up "our" staging file therefore destroyed a user-owned screenshot on
  every capture. Cleanup is now restricted to files under `/tmp`, `/var/tmp`,
  `$XDG_RUNTIME_DIR`, or the cache directory.

- **X11 tools were selected on Wayland and returned black images.** `DISPLAY`
  is set even in a Wayland session because of XWayland, so testing it reports
  X11-only tools as available; they then run without error and produce a black
  or XWayland-only capture. Silent wrong output, not a crash. X11-only backends
  are now gated on `XDG_SESSION_TYPE`.

- **`--screen` failed on GNOME.** The portal refuses non-interactive
  screenshots for an unsandboxed application and answers with response code 2.
  Rather than retry through the picker, which would answer a different question
  than the one asked, the failure is reported so the backend chain can hand off
  to a tool that does full-screen without a prompt.

- **Captures could photograph the app's own window.** The window now sends a
  request to the daemon, which kills the window, waits for it to exit, and
  allows 250 ms for the compositor to repaint before the shutter fires. Capture
  is owned by the daemon and never by the window.

- **Staged pages and kept captures were written with the ambient umask.** Both
  embed a picture of the user's screen; they are now written mode `0600`, and
  rewriting an existing world-readable file re-tightens it.

### Known issues

- **The GNOME 46 Screenshot portal is broken on some systems.** GNOME Shell
  captures the image, saves it to `~/Pictures/Screenshots`, and then reports no
  file back to the portal (`InteractiveScreenshot didn't return a file`). The
  backend chain recovers, but for an area capture the portal shows its picker
  before failing, so the user selects a region twice. Set
  `capture_backend = "gnome-screenshot"` to skip the portal on affected systems.

- **Full-screen capture is slow on large multi-monitor setups.** On an
  11520x2400 desktop, roughly 21 s to grab and 18 s to downscale. Area captures
  avoid both, since the image is small and needs no resize.

- **Downscaling uses the longest edge**, which suits a single display but
  squashes a wide multi-monitor capture into an unreadable strip. A pixel
  budget would preserve far more detail.

- **Autostart is not wired up inside Flatpak.** That path is the sandbox's
  private config directory, so writing it does nothing; it needs the XDG
  Background portal instead. Detected and reported as an explicit error rather
  than failing silently.
