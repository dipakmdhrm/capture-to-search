# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Read `README.md` first. It documents the process model, why the browser performs
the Lens upload, the backend probe order, config keys, and the CLI surface. This
file covers what the README does not: the cross-file wiring, the invariants that
are easy to break, and the commands.

## Git workflow - IMPORTANT

**Never push directly to `main`.** Always work on a feature branch and open a
pull request, so lint and tests run against the change before it merges.

1. Create a branch from the latest `main`:
   ```bash
   git checkout main && git pull
   git checkout -b <descriptive-branch-name>
   ```
2. Commit changes on the branch.
3. Push the branch and open a PR targeting `main`:
   ```bash
   git push -u origin <descriptive-branch-name>
   gh pr create --base main --title "..." --body "..."
   ```
4. **Never merge a PR - merging is always the user's decision and action**, even
   when everything is green and all review comments are addressed. Stop when the
   PR is ready and report its URL.

CI runs these on every pull request, but run them locally before opening one
and report the results - a red PR wastes a review cycle:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

**One PR per prompt:** create exactly one pull request per user request, even
when the work is large. Use multiple commits on the same branch for
reviewability instead of fanning out into many small PRs - only split when the
user explicitly asks.

This applies to all agents (Claude, Gemini, etc.) - no direct pushes to `main`,
and no merges, under any circumstances.

## Keep documentation in sync - IMPORTANT

Whenever a change affects user-facing behavior, features, architecture,
commands, conventions, or test boundaries, update the relevant docs **in the
same PR** so they never drift from the code:

- `README.md` - user-facing features, usage, and configuration
- `CHANGELOG.md` - what shipped, in the reader's terms
- `CLAUDE.md` - architecture, commands, conventions, and test-coverage boundaries

`CHANGELOG.md` loosely follows [Keep a Changelog](https://keepachangelog.com/)
and currently holds a single released `## 0.1.0` section. Add new work under an
`## Unreleased` heading (create it if absent) in the right `### Added` /
`### Changed` / `### Fixed` subsection - never edit a released section to
describe work that has not shipped. Two rules specific to this file:

- **Fixing a documented known issue means deleting its bullet from
  `### Known issues`** and describing the fix under `### Fixed`. A stale known
  issue is worse than none: it tells users to work around something that no
  longer happens. The current entries cover the GNOME 46 portal handoff,
  full-screen capture cost on multi-monitor setups, longest-edge downscaling,
  and Flatpak autostart.
- Entries say what changed *for the user* and why, matching the existing prose
  style. This changelog is not a commit log, and internal refactors with no
  observable effect do not belong in it.

Before opening a PR, re-read all three and reconcile anything the change made
inaccurate. The things that drift most easily in this repo:

- the backend table under **Portability** and `Backend::ALL`, which must stay in
  the same order (the table is numbered by probe order)
- the config block under **Configuration** and the fields on `Config`
- the command list under **Usage** and what `main.rs` actually parses
- the invariants and IPC notes here, when a message, guard, or fallback changes
- the test count quoted in `CHANGELOG.md` (currently 92), which every added test
  invalidates: `cargo test --workspace` prints the real number

Doc comments count as documentation. The module headers carry this codebase's
reasoning (`lens.rs` on session-bound uploads, `capture/mod.rs` on the XWayland
trap, `window_proc.rs` on kill-on-close); if you change the behavior one
describes, update it in the same change. Treat doc updates as part of "done,"
not a follow-up.

## Keep tests meaningful - IMPORTANT

For every change, add or update tests when doing so is meaningful - treat it as
part of "done," not a follow-up. "Meaningful" means the test would actually
catch a regression in the behavior you changed:

- New or changed logic with a testable contract (parsing, decisions, data
  transforms, command construction, IPC handling) -> add or update unit tests
  covering the new behavior and its edge cases.
- Fixing a bug -> add a test that fails without the fix, so it cannot silently
  regress.
- When the meaningful logic is tangled with hard-to-test platform code (GTK4
  widgets, zbus portal calls, spawning screenshot tools), **extract the pure
  logic into a standalone function and test that.** This codebase already does
  it throughout: `resolve_config_home`, `Backend::file_command`,
  `purge_stale_pages_in`, `find_gui`, `lens::upload_page`, and the IPC framing
  are all pure and directly tested, while the GTK UI in `gui/` is not
  unit-tested at all.
- Prefer a test that pins the *reason* a thing is the way it is, not just its
  current value. `portal_is_tried_before_everything_else` and
  `x11_tools_are_disabled_on_wayland_even_with_display_set` exist because those
  behaviors fail silently in production - a wrong result, not a crash. New code
  with a silent failure mode deserves the same treatment.
- Run the suite before opening a PR: `cargo test --workspace`, plus
  `cargo fmt --check` and
  `cargo clippy --workspace --all-targets -- -D warnings`.

Skip new tests only when a change genuinely has no testable behavior (docs,
comments, pure formatting, trivial constant tweaks) - and say so briefly rather
than silently omitting them.

## Commands

```bash
cargo build                      # core + daemon only (workspace default-members)
cargo build --workspace          # ...plus the GUI; needs GTK 4.14+ / libadwaita 1.5+
cargo test --workspace           # no network and no display server needed
cargo clippy --workspace --all-targets
cargo fmt

cargo test -p capture-core                       # one crate
cargo test -p capture-core downscale             # tests matching a substring
cargo test -p capture-to-searchd single_instance # one module

# Inspect what a given image would send, and optionally open it in Lens:
cargo run -p capture-core --example upload_file -- shot.png [--open]

# Runtime diagnostics: selected backend, and why every other one was rejected.
cargo run -p capture-to-searchd -- doctor
RUST_LOG=debug cargo run -p capture-to-searchd -- capture --area
```

`cargo build` deliberately skips the GUI (see the comment on `default-members`
in the root `Cargo.toml`) so the daemon builds on hosts with no GTK development
libraries. Never move `gui` into `default-members`.

Logging is `tracing` + `EnvFilter`; `RUST_LOG` overrides the default level
(`info`, or `warn` for `doctor` so its report is not interleaved).

## Architecture

Three crates: `core` (`capture-core`, library), `daemon`
(`capture-to-searchd`, the primary binary), `gui` (`capture-to-search-gui`,
optional GTK4/libadwaita window). See the README table for responsibilities.

### The capture path

Every trigger converges on `daemon/src/flow.rs::pipeline`:

```
tray click / window Capture button / `capture` subcommand
  -> IPC CaptureRequest (or direct, when no daemon is running)
  -> daemon capture_tx channel -> run_capture_handler (serializes requests)
  -> flow::pipeline
       dismiss window -> wait for exit -> WINDOW_SETTLE repaint pause
       purge stale staged pages
       capture::capture (walks the backend chain)
       lens::inspect (blank detection, logged only)
       lens::downscale -> lens::upload_page -> paths::write_private (0600)
       open::that_detached(page) -> browser POSTs to Lens itself
```

`flow::run` is the daemon path (holds the `capturing` guard, notifies on
failure); `flow::run_standalone` is the no-daemon one-shot path. Both share
`pipeline`, which takes `ctx: Option<&AppCtx>`.

### Invariants worth knowing before editing

- **Capture never moves out of the daemon.** The window must be off-screen
  before the shutter fires. Adding capture logic to `gui/` would photograph the
  window itself.
- **`CaptureError::Cancelled` must not cascade.** `capture::capture` falls
  through to the next backend on `Failed` but returns immediately on
  `Cancelled` - otherwise pressing Escape pops a fresh selection overlay for
  every remaining backend. Any new backend must return `Cancelled` for a
  dismissed picker, not a generic error.
- **X11-only backends are gated on `XDG_SESSION_TYPE`, never on `DISPLAY`.**
  `DISPLAY` is set under Wayland too (XWayland), so a `DISPLAY` check makes
  `scrot`/`maim`/`import` look available and silently return a black image.
  `Backend::x11_only`/`wayland_only` encode this; there is a regression test.
- **A pinned `capture_backend` disables fallback.** A pin is an explicit
  instruction not to look elsewhere, so it errors rather than quietly using the
  portal.
- **`Backend::file_command` is pure and separately tested.** A wrong flag fails
  *silently* (drop `-a` from `gnome-screenshot` and a region request captures
  the whole screen with no error), so keep command construction out of the code
  that runs it.
- **Anything derived from a capture is written with `paths::write_private`**
  (mode 0600): staged pages and `keep_captures` copies both embed a picture of
  the user's screen.
- **The staged upload page must stay self-contained.** No external script, link,
  or `@import` - it is a `file://` page holding a screen capture. The endpoint
  is user-configurable and lands in an HTML attribute, so it must stay escaped.
  Tests cover all of this.
- **`config_snapshot()` copies rather than holding the lock**, because
  `pipeline` awaits for a long time afterwards and a concurrent `ReloadConfig`
  would deadlock the daemon.

### IPC

`core/src/ipc.rs` holds one `IpcMessage` enum and two framing implementations
over the same wire format (4-byte big-endian length + `serde_json`): async for
the daemon, `ipc::blocking` for the GTK window, which has a GTK main loop rather
than a tokio one. A test asserts the two stay wire-compatible - keep it that way
rather than letting the blocking module drift.

Adding a message means touching: the enum, the daemon's match arm in
`daemon/src/server.rs::handle`, and the caller (`gui/src/daemon_link.rs` for
window-initiated, `daemon/src/single_instance.rs` for CLI-initiated). Unknown
and reply-only messages arriving inbound get an `Error` reply, not a panic.

`CaptureRequest` is Acked *before* the capture runs: the flow kills the window
as its first step, and a window waiting on its own Ack would deadlock.

The socket at `$XDG_RUNTIME_DIR/capture-to-search/daemon.sock` is both the IPC
endpoint and the single-instance guard. `single_instance::acquire` distinguishes
a live daemon from a socket left by a crash with a `Ping`/`Pong` probe; a stale
file is removed and rebound.

### Window lifecycle

The daemon owns at most one window child (`daemon/src/window_proc.rs`) and
**kills it on close** rather than keeping it resident - it is one button, so
respawning is cheap and a resident GTK process would dominate an idle tray app's
footprint. A reaper task clears `window`/`window_alive`/`window_kill` and fires
`window_gone`, which `flow::dismiss_window` waits on. `gui_path()` returning
`None` is a supported state, not an error: the tray just omits its window entry.

## CI and releases

Four workflows in `.github/workflows`:

- **`ci.yml`** - on every PR: `fmt`, `clippy -D warnings`, `cargo test
  --workspace`, then every package build. The package job is gated on the check
  job so a failing lint does not burn three container builds.
- **`build-packages.yml`** - reusable, called by both CI and the release. Builds
  the `.deb` (amd64, arm64), `.rpm` (x86_64, aarch64) and Arch package, and
  **installs each one and runs `capture-to-searchd --version`** before uploading
  it. Because CI and the release call the same workflow, a tag build repeats an
  already-validated build rather than trying for the first time.
- **`release.yml`** - on a `v*` tag push, or called by auto-release. Builds the
  packages and attaches them to a GitHub Release.
- **`auto-release.yml`** - on merge to main: picks the bump from the merged PR's
  `release:*` label (default patch), computes the next version from the newest
  `v*` tag, rewrites `[workspace.package] version`, syncs `Cargo.lock`, stamps
  `CHANGELOG.md` (`## Unreleased` becomes the version, a fresh empty one is left
  on top), commits, tags, pushes, then calls `release.yml`.

Things to know before editing these:

- **Label a PR `release:skip`** to merge without cutting a release;
  `release:minor` / `release:major` change the bump.
- A merge touching only `.github/` or `*.md` does **not** release - there is
  nothing user-facing to ship. An explicit `release:major`/`release:minor` label
  overrides that.
- **No release loop**: the bump commit and tag are pushed with the default
  `GITHUB_TOKEN`, and pushes made with that token do not trigger workflows. That
  is also why `auto-release.yml` invokes `release.yml` via `workflow_call`
  instead of relying on its `push: tags` trigger, which the token-pushed tag
  would never fire.
- **`packaging/stage-tree.sh` and `packaging/source-tarball.sh` are shared** by
  `build-local.sh` and CI, deliberately. A private copy of the file layout in
  either place is how an asset ends up in one package and not the other; that
  has already happened once with the symbolic tray icon. Tests assert both paths
  use the scripts.
- Lint workflow changes before pushing - GitHub is the only other place they
  run: `docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest`.

## Packaging

`packaging/` holds one definition per format - `deb/` (control template plus
maintainer scripts), `rpm/` (a single spec), `arch/` (PKGBUILD plus install
hooks) - and `build-local.sh`, which builds all three without CI: deb natively
via `dpkg-deb`, rpm and arch inside `fedora`/`archlinux` docker containers.
Output goes to `dist/`. Flatpak is not packaged yet; see the notes below.

A **single package** ships both binaries, so it depends on GTK4 and libadwaita
even though the daemon itself links neither. That is a deliberate choice: it
keeps packaging simple at the cost of excluding hosts without libadwaita 1.5.
The crate split still matters for `cargo build` and for `install.sh`, which
installs the GUI only if it was built.

Things that will bite you here:

- **The version is templated as `@VERSION@`** in all three formats and
  substituted at build time from the workspace `Cargo.toml`. A test asserts the
  files never hardcode it, because nothing else would notice a stale version.
- **Arch must unset `RUSTFLAGS`/`CFLAGS`/`LDFLAGS`.** makepkg's hardening flags
  garbage-collect `ring`'s static asm objects at link time (`undefined symbol:
  ring_core_*`); `ring` reaches us through rustls. The PKGBUILD documents this.
- **Maintainer scripts must not kill the daemon on upgrade** - only on removal.
  See the self-update invariant below. There is a test for this.
- **Removal cleans up `~/.config/autostart/capture-to-search.desktop`** for every
  user, since the app writes it per-user and only packaging can remove it.
  Tested.
- `packaging_tests` in `core/src/lib.rs` pins binary names, the app id, package
  names, and the above rules against the packaging files. It exists because a
  rename in the app produces a package that installs cleanly and then does
  nothing.

**Flatpak is deliberately not packaged yet.** Two blockers, both specific to
this app: the staged `file://` upload page lives in the sandbox's runtime dir,
which the host browser cannot read (it may survive the OpenURI portal's
document-portal re-export, but that is unverified), and autostart needs the
Background portal, which `core/src/autostart.rs` currently rejects with an
explicit error. Inside a sandbox only the portal capture backend is reachable.

### Self-update

`daemon/src/self_update.rs` polls the daemon's own binary and, when a package
upgrade replaces it, re-execs onto the new image. This is why the maintainer
scripts leave a running daemon alone on upgrade - without it the tray icon
would disappear until the next login. Release builds only, so a dev rebuild does
not bounce a daemon you are debugging. `main` performs the handoff after the
socket, tray, and window are torn down, with a one-second pause so the SNI
watcher processes the deregistration before the same PID re-registers the same
well-known name.

## Conventions

- **Doc comments explain why, not what.** Module headers carry the reasoning
  (`lens.rs` on session-bound uploads, `capture/mod.rs` on the XWayland trap,
  `window_proc.rs` on kill-on-close). Preserve and extend that when you change
  the behaviour they describe.
- **Test names are behavioural sentences** with a comment stating the failure
  they prevent - `x11_tools_are_disabled_on_wayland_even_with_display_set`,
  `stale_socket_from_a_crash_is_reclaimed`. Match this style.
- Tests live in inline `#[cfg(test)] mod tests` blocks; `core/tests/` is empty.
  Everything is designed to pass headless with no network - keep it that way by
  splitting pure logic out of the code that talks to D-Bus or spawns processes
  (`resolve_config_home`, `file_command`, `purge_stale_pages_in` are the
  existing examples).
- `data/icons/**` is embedded into the daemon binary with `include_bytes!` in
  `daemon/src/tray.rs`; those paths are compile-time, so moving the files breaks
  the build.
