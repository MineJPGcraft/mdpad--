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

/// Which graphics API the window is created with.
///
/// The default is [`Backend::Auto`], which resolves to the platform's native
/// API: Direct3D 12 on Windows, Vulkan on Linux, Metal on macOS. Anything the
/// page draws is still painted by `egui`; the choice only decides which driver
/// stack gets loaded, and those are not interchangeable in cost — on a machine
/// with only Vulkan installed, asking for Direct3D fails outright.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// The platform's own API (see above).
    #[default]
    Auto,
    /// Direct3D 12. Windows only.
    DirectX,
    /// Vulkan. Linux and Windows.
    Vulkan,
    /// Metal. macOS only.
    Metal,
    /// OpenGL, through `glow` rather than `wgpu`.
    OpenGl,
}

impl Backend {
    /// Every backend, in the order the settings menu lists them.
    pub fn all() -> [Self; 5] {
        [
            Self::Auto,
            Self::DirectX,
            Self::Vulkan,
            Self::Metal,
            Self::OpenGl,
        ]
    }

    /// Name shown in the settings menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动（跟随系统）",
            Self::DirectX => "Direct3D 12",
            Self::Vulkan => "Vulkan",
            Self::Metal => "Metal",
            Self::OpenGl => "OpenGL",
        }
    }

    /// The API this platform should use when nothing else is asked for.
    ///
    /// Order matters: the first entry is the default, the rest are fallbacks.
    /// Windows lists OpenGL first because the Direct3D 12 stack costs roughly
    /// 340 MB of private working set against OpenGL's 120 MB on the same
    /// machine — for a 2D text editor that difference buys nothing.
    pub fn platform_order() -> &'static [Self] {
        #[cfg(target_os = "windows")]
        {
            &[Self::OpenGl, Self::DirectX, Self::Vulkan]
        }
        #[cfg(target_os = "macos")]
        {
            &[Self::Metal, Self::OpenGl]
        }
        #[cfg(target_os = "linux")]
        {
            &[Self::Vulkan, Self::OpenGl]
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            &[Self::OpenGl]
        }
    }

    /// The API this platform should use when nothing else is asked for.
    pub fn platform_default() -> Self {
        Self::platform_order()[0]
    }

    /// What to switch to when `self` failed to create a window.
    ///
    /// A failed start leaves no UI to change the setting from, and the event
    /// loop cannot be created twice in one process — so the recovery is to write
    /// the next candidate to the config and let the following launch use it.
    pub fn fallback(self) -> Self {
        Self::platform_order()
            .iter()
            .copied()
            .find(|b| *b != self && b.is_usable())
            .unwrap_or(Self::Auto)
    }

    /// Resolves [`Self::Auto`] and demotes a request this machine cannot honour.
    ///
    /// Demoting here rather than only at window creation matters: a failed
    /// window leaves the user with no UI to change the setting back, and the
    /// event loop cannot be created twice in one process, so retrying in-process
    /// is not an option.
    pub fn effective(self) -> Self {
        match self {
            Self::Auto => Self::platform_default(),
            other if other.unavailable_reason().is_none() => other,
            _ => Self::platform_default(),
        }
    }

    /// Why this backend cannot be used here, or `None` if it can.
    ///
    /// Deliberately cheap: it looks for the platform's loader rather than
    /// creating an instance, because probing by *loading* the driver would add
    /// exactly the memory cost this setting exists to avoid.
    pub fn unavailable_reason(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::DirectX if !cfg!(target_os = "windows") => Some("仅 Windows 可用"),
            Self::Metal if !cfg!(target_os = "macos") => Some("仅 macOS 可用"),
            Self::Vulkan if cfg!(target_os = "macos") => Some("macOS 请用 Metal"),
            Self::Vulkan if !vulkan_loader_present() => Some("系统未安装 Vulkan 驱动"),
            _ => None,
        }
    }

    /// `true` when the window can actually be created with this backend.
    pub fn is_usable(self) -> bool {
        self.unavailable_reason().is_none()
    }
}

/// Whether the platform's Vulkan loader is installed at all.
///
/// The loader is a separate install on Windows (shipped with the GPU driver),
/// so its absence means no Vulkan, whatever the hardware can do.
fn vulkan_loader_present() -> bool {
    #[cfg(target_os = "windows")]
    {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        std::path::Path::new(&root).join("System32/vulkan-1.dll").is_file()
    }
    #[cfg(not(target_os = "windows"))]
    {
        const CANDIDATES: &[&str] = &[
            "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
            "/usr/lib64/libvulkan.so.1",
            "/usr/lib/libvulkan.so.1",
            "/usr/local/lib/libvulkan.so.1",
        ];
        CANDIDATES.iter().any(|p| std::path::Path::new(p).exists())
    }
}

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
    /// Graphics API for the window. Takes effect on the next start.
    pub renderer: Backend,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dark: true,
            zoom: 1.0,
            measure: 760.0,
            line_height: 1.62,
            callout_syntax: "GitHub".to_owned(),
            source_mode: false,
            recent: Vec::new(),
            last_dir: None,
            source_wrap: true,
            diagram_font_scale: 1.0,
            renderer: Backend::Auto,
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
    fn config_files_written_by_older_versions_still_load() {
        // `showGutter` / `showGutterBorder` were never read by the renderer; they
        // are gone now, and an existing config file must not be rejected for
        // still carrying them.
        let json = r#"{"dark":false,"showGutter":true,"showGutterBorder":true}"#;
        let cfg: Config = serde_json::from_str(json).expect("legacy keys must be ignored");
        assert!(!cfg.dark);
        assert_eq!(cfg.callout_syntax(), CalloutSyntax::GitHub);
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

    #[test]
    fn auto_resolves_to_the_platform_api() {
        let expected = if cfg!(target_os = "windows") {
            Backend::OpenGl
        } else if cfg!(target_os = "macos") {
            Backend::Metal
        } else {
            Backend::Vulkan
        };
        assert_eq!(Backend::platform_default(), expected);
        assert_eq!(Backend::Auto.effective(), expected);
        // The default has to be something the machine can serve.
        assert!(Backend::platform_default().is_usable());
    }

    #[test]
    fn a_failed_backend_falls_back_to_a_different_one() {
        for backend in Backend::all() {
            let next = backend.effective().fallback();
            assert_ne!(next, backend.effective(), "{backend:?} fell back to itself");
            assert!(next.is_usable() || next == Backend::Auto);
        }
    }

    #[test]
    fn another_platforms_api_is_reported_as_unavailable() {
        #[cfg(target_os = "windows")]
        assert!(Backend::Metal.unavailable_reason().is_some());
        #[cfg(target_os = "macos")]
        {
            assert!(Backend::DirectX.unavailable_reason().is_some());
            assert!(Backend::Vulkan.unavailable_reason().is_some());
        }
        #[cfg(target_os = "linux")]
        assert!(Backend::DirectX.unavailable_reason().is_some());
    }

    #[test]
    fn an_impossible_request_never_reaches_the_window_builder() {
        // Whatever the user asked for, `effective` has to hand back something
        // this machine can serve — the failure mode is a window that never
        // appears, with no UI left to change the setting back.
        for backend in Backend::all() {
            let effective = backend.effective();
            assert!(
                effective.is_usable() || effective == Backend::platform_default(),
                "{backend:?} resolved to unusable {effective:?}"
            );
        }
    }

    #[test]
    fn renderer_setting_round_trips_and_old_configs_still_load() {
        let cfg = Config {
            renderer: Backend::OpenGl,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"renderer\":\"opengl\""), "{json}");
        let parsed: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.renderer, Backend::OpenGl);

        // A file written before the field existed must not break start-up.
        let parsed: Config = serde_json::from_str(r#"{"dark":true,"zoom":1.0}"#).unwrap();
        assert_eq!(parsed.renderer, Backend::Auto);
    }
}
