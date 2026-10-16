//! Persisted user settings.
//!
//! Stored as JSON under the platform config directory. Every field has a default
//! and unknown fields are ignored, so an older config file never prevents the
//! editor from starting.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::doc::model::CalloutSyntax;

/// How many recently opened files to remember.
const MAX_RECENT: usize = 12;

/// Serialisable settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Dark mode.
    pub dark: bool,
    /// UI zoom multiplier.
    pub zoom: f32,
    /// Reading column width in points; `0` means "fit the window".
    pub measure: f32,
    /// Line spacing multiplier.
    pub line_height: f32,
    /// Syntax used when inserting a *new* callout.
    pub callout_syntax: String,
    /// Show the per-block type gutter.
    pub show_gutter: bool,
    /// Show the block gutter's borders.
    pub show_gutter_border: bool,
    /// Start in source mode.
    pub source_mode: bool,
    /// Recently opened files, most recent first.
    pub recent: Vec<String>,
    /// Directory of the last open/save dialog.
    pub last_dir: Option<String>,
    /// Wrap long lines in source mode.
    pub source_wrap: bool,
    /// Render diagrams at this font scale.
    pub diagram_font_scale: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dark: true,
            zoom: 1.0,
            measure: 760.0,
            line_height: 1.62,
            callout_syntax: "GitHub".to_owned(),
            show_gutter: true,
            show_gutter_border: false,
            source_mode: false,
            recent: Vec::new(),
            last_dir: None,
            source_wrap: true,
            diagram_font_scale: 1.0,
        }
    }
}

impl Config {
    /// Where the config file lives.
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("MarkdownPad").join("config.json"))
    }

    /// Loads the config, falling back to defaults on any error.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(cfg) => cfg.sanitised(),
                Err(err) => {
                    log::warn!("config parse failed ({err}); using defaults");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Writes the config, creating the directory if needed.
    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = Self::path() else {
            anyhow::bail!("no config directory available");
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Clamps values into sane ranges so a hand-edited file cannot break layout.
    pub fn sanitised(mut self) -> Self {
        self.zoom = self.zoom.clamp(0.6, 2.5);
        self.line_height = self.line_height.clamp(1.05, 2.6);
        self.measure = if self.measure <= 0.0 {
            0.0
        } else {
            self.measure.clamp(360.0, 2400.0)
        };
        self.diagram_font_scale = self.diagram_font_scale.clamp(0.6, 2.0);
        if self.callout_syntax_from_str().is_none() {
            self.callout_syntax = "GitHub".to_owned();
        }
        self
    }

    /// The callout syntax used for newly inserted callouts.
    pub fn callout_syntax(&self) -> CalloutSyntax {
        self.callout_syntax_from_str().unwrap_or(CalloutSyntax::GitHub)
    }

    fn callout_syntax_from_str(&self) -> Option<CalloutSyntax> {
        CalloutSyntax::all()
            .into_iter()
            .find(|s| s.label().eq_ignore_ascii_case(self.callout_syntax.trim()))
    }

    pub fn set_callout_syntax(&mut self, syntax: CalloutSyntax) {
        self.callout_syntax = syntax.label().to_owned();
    }

    /// Records `path` as the most recent file.
    pub fn push_recent(&mut self, path: &Path) {
        let text = path.display().to_string();
        self.recent.retain(|p| p != &text);
        self.recent.insert(0, text);
        self.recent.truncate(MAX_RECENT);
        self.last_dir = path
            .parent()
            .map(|p| p.display().to_string())
            .or_else(|| self.last_dir.clone());
    }

    /// Drops an entry that no longer exists on disk.
    pub fn prune_recent(&mut self) {
        self.recent.retain(|p| Path::new(p).is_file());
    }

    /// The directory a file dialog should open in.
    pub fn start_dir(&self) -> PathBuf {
        self.last_dir
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(dirs::document_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_json() {
        let cfg = Config::default();
        let text = serde_json::to_string(&cfg).expect("serialise");
        let back: Config = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back.dark, cfg.dark);
        assert_eq!(back.zoom, cfg.zoom);
        assert_eq!(back.callout_syntax, cfg.callout_syntax);
    }

    #[test]
    fn sanitise_clamps_hostile_values() {
        let cfg = Config {
            zoom: 99.0,
            line_height: 0.1,
            measure: -5.0,
            diagram_font_scale: 40.0,
            callout_syntax: "nonsense".to_owned(),
            ..Config::default()
        }
        .sanitised();
        assert_eq!(cfg.zoom, 2.5);
        assert_eq!(cfg.line_height, 1.05);
        assert_eq!(cfg.measure, 0.0);
        assert_eq!(cfg.diagram_font_scale, 2.0);
        assert_eq!(cfg.callout_syntax(), CalloutSyntax::GitHub);
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let json = r#"{"dark":false,"futureSetting":42}"#;
        let cfg: Config = serde_json::from_str(json).expect("must ignore unknown keys");
        assert!(!cfg.dark);
    }

    #[test]
    fn recent_files_are_deduplicated_and_capped() {
        let mut cfg = Config::default();
        for i in 0..20 {
            cfg.push_recent(Path::new(&format!("/tmp/f{i}.md")));
        }
        assert_eq!(cfg.recent.len(), MAX_RECENT);
        cfg.push_recent(Path::new("/tmp/f19.md"));
        assert_eq!(cfg.recent[0], "/tmp/f19.md");
        assert_eq!(
            cfg.recent.iter().filter(|p| *p == "/tmp/f19.md").count(),
            1
        );
    }

    #[test]
    fn callout_syntax_round_trips_by_label() {
        for syntax in CalloutSyntax::all() {
            let mut cfg = Config::default();
            cfg.set_callout_syntax(syntax);
            assert_eq!(cfg.callout_syntax(), syntax);
        }
    }
}
