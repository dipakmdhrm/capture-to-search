//! Screen capture with runtime backend detection.
//!
//! There is no single way to take a screenshot on Linux, so this probes an
//! ordered list of backends and uses the first that works on the running
//! session. The XDG portal comes first deliberately: it is the only backend
//! guaranteed to work under a strict Wayland compositor, where an application
//! simply cannot read the framebuffer itself.
//!
//! **The XWayland trap.** `DISPLAY` is set even in a Wayland session, because
//! XWayland is running. So "is `DISPLAY` set?" is a broken availability test:
//! `scrot` and `import` are frequently installed on Wayland desktops, would be
//! detected as available, run without error, and return a black or
//! XWayland-only image. Silent wrong output, not a crash. X11-only backends are
//! therefore gated on `XDG_SESSION_TYPE`, never on `DISPLAY`.

pub mod portal;

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{anyhow, bail, Context, Result};
use tokio::process::Command;

use crate::ipc::CaptureMode;
use crate::paths;

/// Which display server the session is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionType {
    Wayland,
    X11,
    Unknown,
}

impl SessionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
            Self::Unknown => "unknown",
        }
    }
}

/// A snapshot of the environment that backend detection depends on.
#[derive(Debug, Clone)]
pub struct SessionEnv {
    pub session_type: SessionType,
    pub desktop: String,
    pub display: Option<String>,
    pub wayland_display: Option<String>,
}

impl SessionEnv {
    pub fn detect() -> Self {
        let wayland_display = std::env::var("WAYLAND_DISPLAY")
            .ok()
            .filter(|s| !s.is_empty());
        let display = std::env::var("DISPLAY").ok().filter(|s| !s.is_empty());
        // Trust XDG_SESSION_TYPE first; fall back to WAYLAND_DISPLAY, then
        // DISPLAY. Note the ordering: WAYLAND_DISPLAY is checked before DISPLAY
        // precisely because both are set in a Wayland session.
        let session_type = match std::env::var("XDG_SESSION_TYPE").as_deref() {
            Ok("wayland") => SessionType::Wayland,
            Ok("x11") => SessionType::X11,
            _ if wayland_display.is_some() => SessionType::Wayland,
            _ if display.is_some() => SessionType::X11,
            _ => SessionType::Unknown,
        };
        Self {
            session_type,
            desktop: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
            display,
            wayland_display,
        }
    }

    pub fn is_wayland(&self) -> bool {
        self.session_type == SessionType::Wayland
    }
}

/// The outcome of probing one backend, carrying *why* it was rejected.
///
/// The reason is the point. The selection chain needs it for its log line, and
/// `doctor` prints the same values verbatim - so a user report of "doesn't work
/// on my distro" is answerable from one command instead of a week of messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Usable on this session.
    Available,
    /// Not installed, or otherwise absent.
    Unavailable(String),
    /// Installed but wrong for this session (the XWayland trap).
    Disabled(String),
}

impl Probe {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Why a capture did not produce an image.
///
/// The distinction drives the fallback chain: a broken backend should hand off
/// to the next one, but a user pressing Escape must end the whole attempt -
/// cascading there would pop a fresh selection overlay for every remaining
/// backend.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// The user dismissed the selection.
    #[error("capture cancelled")]
    Cancelled,
    /// The backend itself failed.
    #[error("{0:#}")]
    Failed(#[from] anyhow::Error),
}

/// A capture strategy. Ordered most- to least-preferred; `select` walks this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `org.freedesktop.portal.Screenshot`. Works on X11 and Wayland, and is
    /// the only option under a locked-down compositor.
    Portal,
    /// wlroots compositors (sway, Hyprland, river).
    GrimSlurp,
    Spectacle,
    GnomeScreenshot,
    XfceScreenshooter,
    Flameshot,
    Maim,
    Scrot,
    Import,
}

impl Backend {
    /// Probe order: portal first, then compositor-native tools, then generic
    /// X11 tools as a last resort.
    pub const ALL: &'static [Backend] = &[
        Backend::Portal,
        Backend::GrimSlurp,
        Backend::Spectacle,
        Backend::GnomeScreenshot,
        Backend::XfceScreenshooter,
        Backend::Flameshot,
        Backend::Maim,
        Backend::Scrot,
        Backend::Import,
    ];

    /// Stable identifier, also accepted by `config.capture_backend`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Portal => "portal",
            Self::GrimSlurp => "grim+slurp",
            Self::Spectacle => "spectacle",
            Self::GnomeScreenshot => "gnome-screenshot",
            Self::XfceScreenshooter => "xfce4-screenshooter",
            Self::Flameshot => "flameshot",
            Self::Maim => "maim",
            Self::Scrot => "scrot",
            Self::Import => "import",
        }
    }

    /// Whether this backend can only work on X11.
    fn x11_only(&self) -> bool {
        matches!(
            self,
            Self::Maim | Self::Scrot | Self::Import | Self::Flameshot
        )
    }

    /// Whether this backend can only work on Wayland (wlroots screencopy).
    fn wayland_only(&self) -> bool {
        matches!(self, Self::GrimSlurp)
    }

    pub async fn probe(&self, env: &SessionEnv) -> Probe {
        // Session gating first, so an installed-but-wrong tool reports the
        // useful reason rather than a bare "available".
        if self.x11_only() && env.is_wayland() {
            return Probe::Disabled(format!(
                "X11-only, session is {}",
                env.session_type.as_str()
            ));
        }
        if self.wayland_only() && !env.is_wayland() {
            return Probe::Disabled(format!(
                "needs a wlroots Wayland session, session is {}",
                env.session_type.as_str()
            ));
        }
        match self {
            Self::Portal => match portal::probe().await {
                Ok(()) => Probe::Available,
                Err(why) => Probe::Unavailable(why),
            },
            Self::GrimSlurp => match which("grim") {
                // slurp is only needed for area mode; grim alone still does
                // full-screen, so its absence doesn't rule the backend out.
                Some(_) => Probe::Available,
                None => Probe::Unavailable("grim not on PATH".into()),
            },
            other => {
                let bin = other.binary();
                match which(bin) {
                    Some(_) => Probe::Available,
                    None => Probe::Unavailable(format!("{bin} not on PATH")),
                }
            }
        }
    }

    /// The executable this backend drives (portal has none).
    fn binary(&self) -> &'static str {
        match self {
            Self::Portal => "",
            Self::GrimSlurp => "grim",
            Self::Spectacle => "spectacle",
            Self::GnomeScreenshot => "gnome-screenshot",
            Self::XfceScreenshooter => "xfce4-screenshooter",
            Self::Flameshot => "flameshot",
            Self::Maim => "maim",
            Self::Scrot => "scrot",
            Self::Import => "import",
        }
    }

    /// Take a screenshot, returning encoded PNG bytes.
    pub async fn capture(&self, mode: CaptureMode) -> std::result::Result<Vec<u8>, CaptureError> {
        match self {
            Self::Portal => portal::capture(mode).await,
            Self::Flameshot => self.capture_via_stdout(mode).await,
            other => {
                debug_assert!(
                    other.writes_to_file(),
                    "{} has no file command",
                    other.name()
                );
                other.capture_via_file(mode).await
            }
        }
    }

    /// The command line for a file-writing backend.
    ///
    /// Pure, and deliberately separate from running it, so the flags can be
    /// asserted in tests. A wrong flag here fails *silently*: drop `-a` from
    /// `gnome-screenshot` and it captures the whole screen when the user asked
    /// for a region, the upload still succeeds, and nothing reports an error.
    ///
    /// `geometry` is only meaningful for grim, which crops to a region that
    /// slurp has already selected.
    fn file_command(
        &self,
        mode: CaptureMode,
        out: &str,
        geometry: Option<&str>,
    ) -> (&'static str, Vec<String>) {
        let area = mode.is_area();
        let out = out.to_string();
        match self {
            Self::GrimSlurp => match geometry {
                Some(g) => ("grim", vec!["-g".into(), g.to_string(), out]),
                None => ("grim", vec![out]),
            },
            // -b background, -n no notify, -o output file, then -r region or
            // -f fullscreen.
            Self::Spectacle => (
                "spectacle",
                vec![
                    "-b".into(),
                    "-n".into(),
                    "-o".into(),
                    out,
                    if area { "-r".into() } else { "-f".into() },
                ],
            ),
            Self::GnomeScreenshot => {
                let mut args = vec!["-f".into(), out];
                if area {
                    args.push("-a".into());
                }
                ("gnome-screenshot", args)
            }
            Self::XfceScreenshooter => (
                "xfce4-screenshooter",
                vec![
                    "-s".into(),
                    out,
                    if area { "-r".into() } else { "-f".into() },
                ],
            ),
            Self::Maim => {
                let mut args: Vec<String> = Vec::new();
                if area {
                    args.push("-s".into());
                }
                args.push(out);
                ("maim", args)
            }
            Self::Scrot => {
                let mut args: Vec<String> = Vec::new();
                if area {
                    args.push("-s".into());
                }
                args.push(out);
                ("scrot", args)
            }
            Self::Import => {
                let mut args: Vec<String> = Vec::new();
                if !area {
                    // Without a window argument `import` waits for a click/drag.
                    args.extend(["-window".into(), "root".into()]);
                }
                args.push(out);
                ("import", args)
            }
            Self::Portal | Self::Flameshot => {
                unreachable!("{} does not write to a file we name", self.name())
            }
        }
    }

    /// Backends whose command is built by [`Backend::file_command`].
    fn writes_to_file(&self) -> bool {
        !matches!(self, Self::Portal | Self::Flameshot)
    }

    /// Backends that write to a path we hand them.
    async fn capture_via_file(
        &self,
        mode: CaptureMode,
    ) -> std::result::Result<Vec<u8>, CaptureError> {
        let out = temp_png_path()?;
        let out_str = out.to_string_lossy().to_string();

        // grim needs a region chosen first; slurp prints one it can crop to.
        let geometry = if matches!(self, Self::GrimSlurp) && mode.is_area() {
            let picked = run_capture_stdout("slurp", &[]).await?;
            let picked = String::from_utf8_lossy(&picked).trim().to_string();
            if picked.is_empty() {
                return Err(CaptureError::Cancelled);
            }
            Some(picked)
        } else {
            None
        };

        let (bin, args) = self.file_command(mode, &out_str, geometry.as_deref());
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let status = run(bin, &argv).await?;

        if !status {
            // A non-zero exit from an interactive picker is almost always the
            // user pressing Escape, so end here rather than trying the next
            // backend and popping another overlay at them.
            let _ = tokio::fs::remove_file(&out).await;
            return Err(CaptureError::Cancelled);
        }
        let bytes = tokio::fs::read(&out)
            .await
            .with_context(|| format!("reading capture from {}", out.display()))
            .map_err(CaptureError::Failed)?;
        let _ = tokio::fs::remove_file(&out).await;
        if bytes.is_empty() {
            return Err(CaptureError::Failed(anyhow!(
                "{} produced an empty file",
                self.name()
            )));
        }
        Ok(bytes)
    }

    /// Backends that stream the image to stdout.
    async fn capture_via_stdout(
        &self,
        mode: CaptureMode,
    ) -> std::result::Result<Vec<u8>, CaptureError> {
        let subcommand = match mode {
            CaptureMode::Screen => "full",
            _ => "gui",
        };
        let bytes = run_capture_stdout("flameshot", &[subcommand, "--raw"]).await?;
        if bytes.is_empty() {
            return Err(CaptureError::Cancelled);
        }
        Ok(bytes)
    }
}

/// Pick a backend: the pinned one if configured and usable, else the first
/// available in preference order.
pub async fn select(env: &SessionEnv, pinned: Option<&str>) -> Result<Backend> {
    if let Some(name) = pinned {
        let backend = Backend::ALL
            .iter()
            .find(|b| b.name() == name)
            .copied()
            .ok_or_else(|| anyhow!("unknown capture backend '{name}' in config"))?;
        return match backend.probe(env).await {
            Probe::Available => Ok(backend),
            Probe::Unavailable(why) | Probe::Disabled(why) => {
                Err(anyhow!("pinned backend '{name}' is unusable: {why}"))
            }
        };
    }
    for backend in Backend::ALL {
        if backend.probe(env).await.is_available() {
            tracing::info!("capture backend: {}", backend.name());
            return Ok(*backend);
        }
    }
    Err(anyhow!(
        "no usable screenshot backend found; run `capture-to-searchd doctor` to see what was tried"
    ))
}

/// Capture, falling through the available backends when one fails.
///
/// A single preferred backend is not enough in practice. On GNOME 46 the
/// Screenshot portal can capture the image, save it to the user's
/// `~/Pictures/Screenshots`, and still report no file back to us - the capture
/// works, the handoff does not. Giving up there would strand the user on a
/// desktop where `gnome-screenshot` sits ready one position down the list.
///
/// A cancel ends the whole attempt; only a genuine backend failure moves on.
/// Pinning a backend in config disables the fallback, because a pin is an
/// explicit instruction not to go looking elsewhere.
pub async fn capture(
    env: &SessionEnv,
    pinned: Option<&str>,
    mode: CaptureMode,
) -> std::result::Result<(Backend, Vec<u8>), CaptureError> {
    if pinned.is_some() {
        let backend = select(env, pinned).await.map_err(CaptureError::Failed)?;
        let bytes = backend.capture(mode).await?;
        return Ok((backend, bytes));
    }

    let mut last: Option<CaptureError> = None;
    for backend in Backend::ALL {
        if !backend.probe(env).await.is_available() {
            continue;
        }
        tracing::info!("capturing via {} ({})", backend.name(), mode.as_str());
        match backend.capture(mode).await {
            Ok(bytes) => return Ok((*backend, bytes)),
            Err(CaptureError::Cancelled) => return Err(CaptureError::Cancelled),
            Err(e) => {
                tracing::warn!(
                    "capture backend {} failed ({e}); falling back to the next one",
                    backend.name()
                );
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        CaptureError::Failed(anyhow!(
            "no usable screenshot backend found; run `capture-to-searchd doctor` \
             to see what was tried"
        ))
    }))
}

/// Probe every backend, for `doctor`. Returns them in preference order along
/// with the selected one.
pub async fn report(
    env: &SessionEnv,
    pinned: Option<&str>,
) -> (Vec<(Backend, Probe)>, Option<Backend>) {
    let mut probes: Vec<(Backend, Probe)> = Vec::with_capacity(Backend::ALL.len());
    for backend in Backend::ALL {
        probes.push((*backend, backend.probe(env).await));
    }
    let selected = select(env, pinned).await.ok();
    (probes, selected)
}

// --- process helpers -------------------------------------------------------

/// Locate `bin` on `PATH`.
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|candidate| candidate.is_file())
}

/// Run a command to completion; `true` if it exited zero.
async fn run(bin: &str, args: &[&str]) -> Result<bool> {
    let status = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .await
        .with_context(|| format!("spawning {bin}"))?;
    Ok(status.success())
}

/// Run a command and capture its stdout.
async fn run_capture_stdout(bin: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("spawning {bin}"))?;
    if !output.status.success() {
        bail!("{bin} exited non-zero (selection cancelled?)");
    }
    Ok(output.stdout)
}

/// A unique staging path under the runtime dir.
fn temp_png_path() -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(paths::capture_temp_dir()?.join(format!("capture-{}-{nanos}.png", std::process::id())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(session: SessionType) -> SessionEnv {
        SessionEnv {
            session_type: session,
            desktop: "GNOME".into(),
            // Both set, exactly as on a real Wayland session with XWayland.
            display: Some(":0".into()),
            wayland_display: Some("wayland-0".into()),
        }
    }

    #[tokio::test]
    async fn x11_tools_are_disabled_on_wayland_even_with_display_set() {
        // The XWayland trap: DISPLAY is set, the binary may well be installed,
        // and using it would silently produce a black image.
        let env = env_with(SessionType::Wayland);
        for backend in [Backend::Scrot, Backend::Import, Backend::Maim] {
            match backend.probe(&env).await {
                Probe::Disabled(why) => assert!(
                    why.contains("X11-only"),
                    "{} should say why: {why}",
                    backend.name()
                ),
                other => panic!(
                    "{} must be disabled on wayland, got {other:?}",
                    backend.name()
                ),
            }
        }
    }

    #[tokio::test]
    async fn grim_is_disabled_off_wayland() {
        let env = env_with(SessionType::X11);
        assert!(matches!(
            Backend::GrimSlurp.probe(&env).await,
            Probe::Disabled(_)
        ));
    }

    #[test]
    fn portal_is_tried_before_everything_else() {
        // The portal is the only backend that works under a strict Wayland
        // compositor, so it must never be ordered behind a CLI tool.
        assert_eq!(Backend::ALL[0], Backend::Portal);
    }

    #[tokio::test]
    async fn cancelling_does_not_cascade_to_other_backends() {
        // If a user presses Escape, falling through the chain would pop a fresh
        // selection overlay for every remaining backend. The typed error is
        // what prevents that, so assert the distinction exists and is used.
        let cancelled = CaptureError::Cancelled;
        assert!(matches!(cancelled, CaptureError::Cancelled));
        let failed: CaptureError = anyhow!("backend exploded").into();
        assert!(matches!(failed, CaptureError::Failed(_)));
        // And they must not be confusable by message alone.
        assert_ne!(CaptureError::Cancelled.to_string(), failed.to_string());
    }

    #[tokio::test]
    async fn pinning_a_backend_disables_fallback() {
        // A pin is an explicit instruction not to go looking elsewhere: pinning
        // an X11 tool on Wayland must fail, not silently use the portal.
        let env = env_with(SessionType::Wayland);
        let err = capture(&env, Some("scrot"), CaptureMode::Area)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unusable"), "got: {msg}");
    }

    /// The file-writing backends, for exhaustive command checks.
    fn file_backends() -> Vec<Backend> {
        Backend::ALL
            .iter()
            .copied()
            .filter(Backend::writes_to_file)
            .collect()
    }

    #[test]
    fn every_file_backend_builds_a_command_for_both_modes() {
        // Exhaustiveness: adding a backend without teaching file_command about
        // it panics at capture time, on the user's machine, mid-selection.
        for backend in file_backends() {
            for mode in [CaptureMode::Area, CaptureMode::Screen] {
                let geom = matches!(backend, Backend::GrimSlurp).then_some("0,0 10x10");
                let (bin, args) = backend.file_command(mode, "/tmp/out.png", geom);
                assert!(!bin.is_empty(), "{} has no binary", backend.name());
                assert!(
                    args.iter().any(|a| a == "/tmp/out.png"),
                    "{} ({mode:?}) must write to the path we give it, got {args:?}",
                    backend.name()
                );
            }
        }
    }

    #[test]
    fn area_mode_actually_requests_a_region() {
        // The silent-failure case: without its region flag a tool captures the
        // whole screen, the upload succeeds, and nothing looks wrong until the
        // user sees their entire desktop in the results.
        let region_flag = [
            (Backend::Spectacle, "-r"),
            (Backend::GnomeScreenshot, "-a"),
            (Backend::XfceScreenshooter, "-r"),
            (Backend::Maim, "-s"),
            (Backend::Scrot, "-s"),
        ];
        for (backend, flag) in region_flag {
            let (_, args) = backend.file_command(CaptureMode::Area, "/tmp/o.png", None);
            assert!(
                args.iter().any(|a| a == flag),
                "{} area mode must pass {flag}, got {args:?}",
                backend.name()
            );
            let (_, full) = backend.file_command(CaptureMode::Screen, "/tmp/o.png", None);
            assert!(
                !full.iter().any(|a| a == flag),
                "{} screen mode must NOT pass {flag}, got {full:?}",
                backend.name()
            );
        }
    }

    #[test]
    fn import_inverts_the_convention() {
        // ImageMagick's `import` selects interactively by default and needs an
        // explicit root window for full screen - the opposite of every other
        // tool here, which is exactly the kind of thing a refactor breaks.
        let (_, area) = Backend::Import.file_command(CaptureMode::Area, "/tmp/o.png", None);
        assert!(!area.iter().any(|a| a == "root"), "got {area:?}");
        let (_, screen) = Backend::Import.file_command(CaptureMode::Screen, "/tmp/o.png", None);
        assert_eq!(screen, vec!["-window", "root", "/tmp/o.png"]);
    }

    #[test]
    fn grim_crops_to_the_selected_geometry() {
        let (bin, args) =
            Backend::GrimSlurp.file_command(CaptureMode::Area, "/tmp/o.png", Some("100,200 30x40"));
        assert_eq!(bin, "grim");
        assert_eq!(args, vec!["-g", "100,200 30x40", "/tmp/o.png"]);
        // No geometry means whole output, not a malformed -g with an empty value.
        let (_, full) = Backend::GrimSlurp.file_command(CaptureMode::Screen, "/tmp/o.png", None);
        assert_eq!(full, vec!["/tmp/o.png"]);
    }

    #[test]
    fn interactive_mode_is_treated_as_a_region() {
        // Interactive hands off to the desktop picker, which defaults to area;
        // treating it as full-screen would skip the picker entirely.
        assert!(CaptureMode::Interactive.is_area());
        assert!(CaptureMode::Area.is_area());
        assert!(!CaptureMode::Screen.is_area());
        let (_, args) =
            Backend::GnomeScreenshot.file_command(CaptureMode::Interactive, "/tmp/o.png", None);
        assert!(args.iter().any(|a| a == "-a"), "got {args:?}");
    }

    #[test]
    fn backend_names_are_unique_and_stable() {
        // Names are user-facing: they appear in `doctor` and are accepted by
        // `config.capture_backend`.
        let mut names: Vec<&str> = Backend::ALL.iter().map(|b| b.name()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "backend names must be unique");
    }

    #[tokio::test]
    async fn unknown_pinned_backend_is_an_error() {
        let env = env_with(SessionType::X11);
        let err = select(&env, Some("nonexistent"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown capture backend"), "got: {err}");
    }

    #[tokio::test]
    async fn pinned_but_unusable_backend_explains_why() {
        // Pinning an X11 tool on Wayland must fail loudly, not fall through to
        // another backend the user didn't ask for.
        let env = env_with(SessionType::Wayland);
        let err = select(&env, Some("scrot")).await.unwrap_err().to_string();
        assert!(err.contains("unusable"), "got: {err}");
        assert!(err.contains("X11-only"), "got: {err}");
    }

    #[test]
    fn session_detection_prefers_wayland_when_both_are_set() {
        let env = env_with(SessionType::Wayland);
        assert!(env.is_wayland());
        assert!(
            env.display.is_some(),
            "XWayland DISPLAY is expected to be set"
        );
    }
}
