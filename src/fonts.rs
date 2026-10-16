//! Font registration.
//!
//! egui ships Latin-only fonts, which is not good enough for a Markdown editor:
//! the document will contain CJK text, and the LaTeX typesetter needs a real
//! mathematical font for radicals, summation signs and Greek letters. This module
//! therefore registers the platform's own faces as explicit fallback chains.
//!
//! Every family is *fallback-ordered*: egui walks the list per glyph, so listing
//! the Latin face first and a CJK face second gives mixed-script text correct
//! shaping without any segmentation on our side. Missing files are reported in a
//! [`Report`] rather than being fatal, so the editor still runs on a machine
//! without (say) Cambria Math — only the maths would degrade.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily, FontId};

use crate::gfx::{Family, Font, Slant, Weight};

// Registered font keys.
const SANS: &str = "mdp-sans";
const SANS_BOLD: &str = "mdp-sans-bold";
const CJK: &str = "mdp-cjk";
const CJK_BOLD: &str = "mdp-cjk-bold";
const MONO: &str = "mdp-mono";
const SERIF: &str = "mdp-serif";
const SERIF_BOLD: &str = "mdp-serif-bold";
const MATH: &str = "mdp-math";
const SYMBOL: &str = "mdp-symbol";

// Named egui families created for the engine layer.
const FAM_SANS: &str = "mdp-sans";
const FAM_SANS_BOLD: &str = "mdp-sans-bold";
const FAM_SERIF: &str = "mdp-serif";
const FAM_SERIF_BOLD: &str = "mdp-serif-bold";
const FAM_MATH: &str = "mdp-math";

/// What happened while installing fonts.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Font keys that loaded successfully.
    pub loaded: Vec<String>,
    /// Human-readable names of faces that were unavailable.
    pub missing: Vec<String>,
    /// `true` when a CJK-capable face was found.
    pub has_cjk: bool,
    /// `true` when a mathematical face was found.
    pub has_math: bool,
}

impl Report {
    /// One-line summary for the status bar / diagnostics log.
    pub fn summary(&self) -> String {
        let cjk = if self.has_cjk { "CJK ok" } else { "CJK missing" };
        let math = if self.has_math {
            "math ok"
        } else {
            "math missing"
        };
        format!(
            "{} fonts loaded · {cjk} · {math}",
            self.loaded.len()
        )
    }
}

/// A font file to try, in priority order.
struct Candidate {
    key: &'static str,
    /// File names to search for in the platform font directories.
    files: &'static [&'static str],
    /// Face index inside a TrueType collection.
    index: u32,
    cjk: bool,
    math: bool,
}

/// Faces to look for. The first file that exists wins.
const CANDIDATES: &[Candidate] = &[
    Candidate {
        key: SANS,
        files: &["segoeui.ttf", "SegoeUI.ttf", "arial.ttf", "DejaVuSans.ttf"],
        index: 0,
        cjk: false,
        math: false,
    },
    Candidate {
        key: SANS_BOLD,
        files: &["segoeuib.ttf", "SegoeUI-Bold.ttf", "arialbd.ttf"],
        index: 0,
        cjk: false,
        math: false,
    },
    Candidate {
        key: CJK,
        files: &["msyh.ttc", "msyh.ttf", "simsun.ttc", "Deng.ttf"],
        index: 0,
        cjk: true,
        math: false,
    },
    Candidate {
        key: CJK_BOLD,
        files: &["msyhbd.ttc", "msyhbd.ttf", "simhei.ttf"],
        index: 0,
        cjk: true,
        math: false,
    },
    Candidate {
        key: MONO,
        files: &["consola.ttf", "CascadiaMono.ttf", "CascadiaCode.ttf", "DejaVuSansMono.ttf"],
        index: 0,
        cjk: false,
        math: false,
    },
    Candidate {
        key: SERIF,
        files: &["cambria.ttc", "georgia.ttf", "DejaVuSerif.ttf", "times.ttf"],
        index: 0,
        cjk: false,
        math: false,
    },
    Candidate {
        key: SERIF_BOLD,
        files: &["cambriab.ttf", "georgiab.ttf", "timesbd.ttf"],
        index: 0,
        cjk: false,
        math: false,
    },
    // Cambria Math lives at face index 1 of `cambria.ttc`.
    Candidate {
        key: MATH,
        files: &["cambria.ttc"],
        index: 1,
        cjk: false,
        math: true,
    },
    Candidate {
        key: SYMBOL,
        files: &["seguisym.ttf", "DejaVuSans.ttf", "ArialUnicode.ttf"],
        index: 0,
        cjk: false,
        math: true,
    },
];

/// Directories searched for font files.
fn font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(root) = std::env::var("SystemRoot") {
            dirs.push(std::path::PathBuf::from(root).join("Fonts"));
        }
        dirs.push(std::path::PathBuf::from("C:\\Windows\\Fonts"));
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            dirs.push(
                std::path::PathBuf::from(local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(std::path::PathBuf::from("/System/Library/Fonts"));
        dirs.push(std::path::PathBuf::from("/Library/Fonts"));
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join("Library/Fonts"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        dirs.push(std::path::PathBuf::from("/usr/share/fonts"));
        dirs.push(std::path::PathBuf::from("/usr/local/share/fonts"));
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".local/share/fonts"));
        }
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

/// Loads one candidate, returning `(key, bytes, index)`.
fn load_candidate(cand: &Candidate, dirs: &[std::path::PathBuf]) -> Option<(String, Vec<u8>, u32)> {
    for dir in dirs {
        for file in cand.files {
            let path = dir.join(file);
            if let Ok(bytes) = std::fs::read(&path)
                && !bytes.is_empty()
            {
                return Some((cand.key.to_owned(), bytes, cand.index));
            }
        }
    }
    None
}

/// Registers the platform fonts and returns a report.
///
/// Safe to call once, at start-up. Calling it again simply re-registers.
pub fn install(ctx: &egui::Context) -> Report {
    let dirs = font_dirs();
    let mut defs = FontDefinitions::default();
    let mut report = Report::default();

    // The lists egui ships with, used as the tail of every chain so emoji and
    // anything we could not source still resolve.
    let base_sans = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let base_mono = defs
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    let mut found: Vec<(&'static str, String, u32)> = Vec::new();
    for cand in CANDIDATES {
        match load_candidate(cand, &dirs) {
            Some((key, bytes, index)) => {
                if cand.cjk {
                    report.has_cjk = true;
                }
                if cand.math {
                    report.has_math = true;
                }
                report.loaded.push(key.clone());
                found.push((cand.key, key, index));
                // Store the bytes.
                let mut data = FontData::from_owned(bytes);
                data.index = index;
                defs.font_data.insert(cand.key.to_owned(), Arc::new(data));
            }
            None => report
                .missing
                .push(cand.files.first().copied().unwrap_or(cand.key).to_owned()),
        }
    }

    if !report.has_cjk {
        report
            .missing
            .push("no CJK face found; Chinese text will not render".to_owned());
    }
    if !report.has_math {
        report
            .missing
            .push("no mathematical face found; formulas may show gaps".to_owned());
    }

    // Helper: build a family chain from keys that actually loaded.
    let present = |key: &str| defs.font_data.contains_key(key);
    let chain = |keys: &[&str], tail: &[String]| -> Vec<String> {
        let mut out: Vec<String> = keys
            .iter()
            .filter(|k| present(k))
            .map(|k| (*k).to_owned())
            .collect();
        out.extend(tail.iter().cloned());
        out
    };

    defs.families.insert(
        FontFamily::Proportional,
        chain(&[SANS, CJK, SYMBOL], &base_sans),
    );
    defs.families.insert(
        FontFamily::Monospace,
        chain(&[MONO, CJK], &base_mono),
    );
    defs.families.insert(
        FontFamily::Name(FAM_SANS.into()),
        chain(&[SANS, CJK], &base_sans),
    );
    defs.families.insert(
        FontFamily::Name(FAM_SANS_BOLD.into()),
        chain(&[SANS_BOLD, CJK_BOLD, SANS, CJK], &base_sans),
    );
    defs.families.insert(
        FontFamily::Name(FAM_SERIF.into()),
        chain(&[SERIF, CJK, SYMBOL], &base_sans),
    );
    defs.families.insert(
        FontFamily::Name(FAM_SERIF_BOLD.into()),
        chain(&[SERIF_BOLD, CJK_BOLD, SERIF, CJK], &base_sans),
    );
    // The maths chain is the important one: a real maths face, then a broad
    // symbol font, then CJK for any stray CJK in a formula.
    defs.families.insert(
        FontFamily::Name(FAM_MATH.into()),
        chain(&[MATH, SYMBOL, SERIF, CJK], &base_sans),
    );

    ctx.set_fonts(defs);
    let _ = found;
    log::info!("fonts: {}", report.summary());
    for missing in &report.missing {
        log::warn!("font unavailable: {missing}");
    }
    report
}

/// The egui font family for an engine-layer [`Font`].
pub fn family_for(font: Font) -> FontFamily {
    match (font.family, font.weight) {
        (Family::Sans, Weight::Bold) => FontFamily::Name(FAM_SANS_BOLD.into()),
        (Family::Sans, Weight::Regular) => FontFamily::Name(FAM_SANS.into()),
        (Family::Serif, Weight::Bold) => FontFamily::Name(FAM_SERIF_BOLD.into()),
        (Family::Serif, Weight::Regular) => FontFamily::Name(FAM_SERIF.into()),
        (Family::Mono, _) => FontFamily::Monospace,
        (Family::Math, _) => FontFamily::Name(FAM_MATH.into()),
    }
}

/// The egui [`FontId`] for an engine-layer [`Font`].
///
/// Slant is *not* part of `FontId` — egui applies italics as a shear inside
/// [`egui::TextFormat`], which [`text_format`] sets.
pub fn font_id(font: Font) -> FontId {
    FontId::new(font.size, family_for(font))
}

/// A complete `TextFormat` for an engine-layer font and colour.
pub fn text_format(font: Font, color: egui::Color32) -> egui::TextFormat {
    egui::TextFormat {
        font_id: font_id(font),
        color,
        italics: font.slant == Slant::Italic,
        ..Default::default()
    }
}

/// Family names created by [`install`], for diagnostics.
pub fn family_names() -> [&'static str; 5] {
    [FAM_SANS, FAM_SANS_BOLD, FAM_SERIF, FAM_MATH, FAM_SERIF_BOLD]
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    fn headless_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx
    }

    /// Measures one line and returns its width.
    fn width(ctx: &egui::Context, text: &str, size: f32) -> f32 {
        ctx.fonts_mut(|f| {
            f.layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::WHITE)
                .size()
                .x
        })
    }

    /// Runs a headless pass and discards the texture deltas.
    ///
    /// epaint asserts that texture deltas are applied before they are dropped,
    /// so a test that lays out glyphs (and therefore grows the font atlas) must
    /// take responsibility for them.
    fn pass(ctx: &egui::Context, mut body: impl FnMut(&egui::Context)) {
        let output = ctx.run_pass(egui::RawInput::default(), body);
        output.drop_without_applying_deltas();
    }

    #[test]
    fn installing_fonts_does_not_panic_without_a_window() {
        let ctx = headless_ctx();
        pass(&ctx, |ctx| {
            assert!(width(ctx, "Hello", 16.0) > 10.0, "latin text did not measure");
        });
    }

    #[test]
    fn cjk_glyphs_are_actually_present() {
        let ctx = headless_ctx();
        pass(&ctx, |ctx| {
            // A real ideograph occupies about one em. A missing glyph falls back
            // to egui's narrow replacement box, so the width check is a genuine
            // signal rather than a tautology.
            let w = width(ctx, "\u{4E2D}\u{6587}", 16.0);
            assert!(
                w >= 24.0,
                "CJK text measured only {w}pt at 16pt — likely no CJK face"
            );
        });
    }

    #[test]
    fn mathematical_symbols_are_present() {
        let ctx = headless_ctx();
        pass(&ctx, |ctx| {
            for symbol in ["\u{2211}", "\u{221A}", "\u{222B}", "\u{03B1}"] {
                let w = width(ctx, symbol, 16.0);
                assert!(
                    w >= 7.0,
                    "math symbol U+{:04X} measured only {w}pt",
                    symbol.chars().next().map(|c| c as u32).unwrap_or(0)
                );
            }
        });
    }

    #[test]
    fn engine_fonts_map_to_distinct_families() {
        let mono = family_for(Font::mono(12.0));
        let sans = family_for(Font::new(12.0));
        let bold = family_for(Font::new(12.0).bold());
        let math = family_for(Font::math(12.0));
        assert_ne!(mono, sans);
        assert_ne!(sans, bold);
        assert_ne!(sans, math);
    }

    #[test]
    fn italic_is_carried_by_text_format() {
        let fmt = text_format(
            Font::new(14.0).italic(),
            Color32::from_rgb(1, 2, 3),
        );
        assert!(fmt.italics);
        assert_eq!(fmt.font_id.size, 14.0);
    }
}
