# Capture to Search

Capture a region of your screen and search it with Google Lens, from a Linux
system tray.

Press Capture, drag out an area, and your browser opens on the Lens results for
what you selected.

## Privacy

Every capture is uploaded to Google by your browser, in your browser's session.
Staged pages embed the capture and are written owner-only, then deleted; kept
copies (`keep_captures`) are owner-only too.

## Usage

```bash
capture-to-searchd                    # run the daemon (tray + capture)
capture-to-searchd --show-window      # ...and open the window
capture-to-searchd capture            # capture once and exit
capture-to-searchd capture --area     # drag out a region
capture-to-searchd capture --screen   # whole screen, no prompt
capture-to-searchd doctor             # print environment diagnostics
```

### Global hotkey

There is no reliable way for an application to grab a global hotkey on Wayland,
so let your desktop own the binding instead. In your keyboard settings, add a
custom shortcut running:

```
capture-to-searchd capture --area
```

If a daemon is running the request is handed to it, so a hotkey press and a tray
click follow the identical code path. If none is running, the capture happens
in-process - a hotkey works even where the tray never came up.

### When something does not work

```bash
capture-to-searchd doctor
```

It prints the selected capture backend and, for every other one, the reason it
was rejected - along with tray, browser, autostart, daemon, and network state.
Include that output in any bug report.

## Configuration

`~/.config/capture-to-search/config.toml`, all fields optional:

```toml
autostart = false          # mirrors the ~/.config/autostart entry
max_upload_edge = 1600     # longest edge before upload; Lens downscales similarly
capture_backend = "portal" # pin a backend; omit to auto-detect
keep_captures = false      # keep a copy of each capture
notify_on_error = true     # desktop notification when a capture fails
lens_endpoint = "https://lens.google.com/v3/upload"
```

`lens_endpoint` is exposed because it is what the Lens web client posts to, not
a documented public API. It becomes the staged page's form action. If Google
moves it, this is a one-line fix rather than a wait for a release.

## Portability

The daemon links no GUI toolkit, so the whole feature works on hosts with no
GTK at all - the GUI is a separate, optional binary and package. When it is
absent the tray simply omits its "Open window" entry.

Capture probes an ordered list of backends and uses the first that works:

| Order | Backend | Covers |
|-------|---------|--------|
| 1 | `xdg-desktop-portal` Screenshot | GNOME, KDE, Cinnamon/XFCE/MATE, wlroots, COSMIC - X11 and Wayland |
| 2 | `grim` + `slurp` | wlroots compositors with no portal |
| 3 | `spectacle` | KDE without portal |
| 4 | `gnome-screenshot` | GNOME without portal |
| 5 | `xfce4-screenshooter` | XFCE |
| 6 | `flameshot` | X11, cross-desktop |
| 7 | `maim` / `scrot` / `import` | generic X11 fallbacks |

The portal is first because it is the only backend that works under a strict
Wayland compositor.

> **The XWayland trap.** `DISPLAY` is set even in a Wayland session. X11-only
> tools are therefore gated on `XDG_SESSION_TYPE`, never on `DISPLAY` - they are
> often installed on Wayland desktops and would silently return a black image
> rather than fail.

## Architecture

A Cargo workspace with three crates:

| Crate | Kind | Responsibility |
|-------|------|----------------|
| `core` (`capture-core`) | library | Paths, config, IPC protocol, autostart, capture backends, staged upload page. No GUI toolkit dependency. |
| `daemon` (`capture-to-searchd`) | binary | Resident daemon: tray icon, capture pipeline, single-instance socket, on-demand window child. Plus the `capture` and `doctor` subcommands. |
| `gui` (`capture-to-search-gui`) | binary | GTK4 + libadwaita window: one Capture button, Preferences, About. |

**Process model.** The daemon is the primary process and stays resident until
killed or Quit. The window is spawned on demand and terminated when closed - it
is a single button, so respawning is cheap and nothing GTK-sized sits in memory
while the app waits in the tray. Exactly one daemon is enforced via a Unix
socket in `$XDG_RUNTIME_DIR`, which also carries daemon-to-window IPC.

**The daemon owns capture, always.** Every trigger - tray click, the window's
Capture button, the `capture` subcommand - routes to the same pipeline in the
daemon, which dismisses the window and waits for the compositor to repaint
before firing. A window-owned capture would photograph its own window.

**The browser performs the upload, not the daemon.** Google scopes an uploaded
image to the client session that uploaded it: the results URL carries
`gsessionid`/`lsessionid` minted for the uploading HTTP client, and opening it
from any other client shows the search shell with an **empty query image**
(presenting those ids with no matching cookies is refused with `403`). This was
verified end to end, including with a freshly minted URL, so it is session
binding rather than expiry.

So the daemon stages a small self-contained HTML page under `$XDG_RUNTIME_DIR`
that rebuilds the capture as a `File`, attaches it to a real file input, and
submits a normal multipart form POST - then opens that page. The browser sends
its own cookies and follows the redirect, exactly as `lens.google.com` does
natively, so the session that uploads is the session that views.

Staged pages are written mode `0600`, deleted 2 minutes after being opened, and
any left behind by a one-shot run are swept on the next capture.

**Storage.**
- Config: `~/.config/capture-to-search/config.toml`
- Runtime socket: `$XDG_RUNTIME_DIR/capture-to-search/daemon.sock`
- Staged upload pages: `$XDG_RUNTIME_DIR/capture-to-search/uploads/` (transient)

App ID: `io.github.dipakmdhrm.CaptureToSearch`.

## Build

```bash
cargo build --workspace      # daemon + GUI
cargo build                  # daemon only (no GTK needed)
cargo test --workspace       # full test suite
cargo test                   # core + daemon only (no GTK toolchain needed)
cargo clippy --workspace --all-targets

# Inspect what a given image would send, and optionally open it in Lens:
cargo run -p capture-core --example upload_file -- shot.png [--open]
```

### Tests

The suite is hermetic: **no display, D-Bus session, network, or writable XDG
directory is required**, so it runs unchanged on a bare CI runner. Verified with:

```bash
env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_SESSION_TYPE -u DBUS_SESSION_BUS_ADDRESS \
    -u XDG_RUNTIME_DIR cargo test --workspace
```

Anything needing real hardware or a live session is tested by construction
instead: environment-dependent logic is split into pure functions that take
their inputs as arguments (`resolve_config_home`, `find_gui`,
`Backend::file_command`, `purge_stale_pages_in`), so the rule is asserted
directly rather than by mutating process state - which would also race with
tests on other threads.

Requires Rust stable. The GUI additionally needs GTK 4.14+ and libadwaita 1.5+.

## License

MIT
