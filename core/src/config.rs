//! On-disk configuration (`~/.config/capture-to-search/config.toml`).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Longest edge, in pixels, that a capture is scaled down to before upload.
///
/// The Lens web client downscales similarly before posting. A raw 4K PNG is
/// several megabytes and is the most likely cause of a rejected upload, so we
/// match that behaviour rather than sending the full-resolution frame.
pub const DEFAULT_MAX_UPLOAD_EDGE: u32 = 1600;

/// Lower bound on `max_upload_edge`: below this, Lens has too little to work
/// with and results get noticeably worse.
pub const MIN_UPLOAD_EDGE: u32 = 400;

/// The Lens upload endpoint.
///
/// This is what the Lens web client posts to, not a documented public API, so
/// Google can change it without notice. It lives in config precisely so a
/// breakage is a one-line fix for the user rather than a wait for a release.
pub const DEFAULT_LENS_ENDPOINT: &str = "https://lens.google.com/v3/upload";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("max_upload_edge must be at least {MIN_UPLOAD_EDGE}")]
    UploadEdgeTooSmall,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Mirror of whether the autostart `.desktop` file is installed. The file
    /// on disk stays the source of truth; this is for display only.
    pub autostart: bool,

    /// Longest edge the capture is scaled to before upload.
    pub max_upload_edge: u32,

    /// Pin a specific capture backend by name (see `capture::Backend::name`).
    /// `None` means auto-detect, which is what almost everyone wants; this is an
    /// escape hatch for a host where detection picks a backend that misbehaves.
    pub capture_backend: Option<String>,

    /// Keep the staged PNG after a successful upload instead of deleting it.
    /// Off by default: captures are screen contents and shouldn't linger.
    pub keep_captures: bool,

    /// Show a desktop notification when a capture fails. Successes are silent
    /// because the browser tab opening is its own confirmation.
    pub notify_on_error: bool,

    /// Where captures are uploaded. See [`DEFAULT_LENS_ENDPOINT`] - overridable
    /// because it is an undocumented endpoint that may move.
    pub lens_endpoint: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            autostart: false,
            max_upload_edge: DEFAULT_MAX_UPLOAD_EDGE,
            capture_backend: None,
            keep_captures: false,
            notify_on_error: true,
            lens_endpoint: DEFAULT_LENS_ENDPOINT.to_string(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_upload_edge < MIN_UPLOAD_EDGE {
            return Err(ConfigError::UploadEdgeTooSmall);
        }
        Ok(())
    }

    /// Load from `path`. A missing file yields defaults rather than an error, so
    /// a first run needs no setup step.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let cfg: Self = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Write to `path`, creating parent directories as needed.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn missing_file_yields_defaults() {
        // A first run must not fail just because no config has been written.
        let cfg = Config::load(Path::new("/nonexistent/capture-to-search.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn unknown_and_missing_fields_tolerated() {
        // Forward compatibility: a config written by a newer version must still
        // load, and a field added later must fall back to its default.
        let cfg: Config = toml::from_str("autostart = true\nfuture_option = 42\n").unwrap();
        assert!(cfg.autostart);
        assert_eq!(cfg.max_upload_edge, DEFAULT_MAX_UPLOAD_EDGE);
    }

    #[test]
    fn tiny_upload_edge_rejected() {
        let cfg = Config {
            max_upload_edge: 10,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = Config {
            autostart: true,
            max_upload_edge: 1200,
            capture_backend: Some("portal".into()),
            keep_captures: true,
            notify_on_error: false,
            lens_endpoint: "https://example.invalid/upload".into(),
        };
        let back: Config = toml::from_str(&toml::to_string_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(cfg, back);
    }
}
