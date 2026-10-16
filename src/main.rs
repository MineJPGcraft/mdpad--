//! MarkdownPad — a pure-Rust, fully offline WYSIWYG Markdown editor.
//!
//! ```text
//! main ── App ─┬─ toolbar / status bar / document surface   (src/app.rs)
//!              ├─ live-render block view                    (src/edit.rs)
//!              └─ Document ── block + inline parsers        (src/doc/)
//!
//! rendering engines, all local:
//!   src/tex/      LaTeX -> primitives     (own typesetter)
//!   src/diagram/  Mermaid / PlantUML -> primitives
//!   src/gfx.rs    renderer-agnostic primitives
//!   src/gfx_egui.rs  the only file that touches egui's painter
//! ```

mod app;
mod config;
mod diagram;
mod doc;
mod edit;
mod fonts;
mod gfx;
mod gfx_egui;
mod tex;
mod theme;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1320.0, 900.0])
            .with_min_inner_size([760.0, 520.0])
            .with_title("MarkdownPad"),
        ..Default::default()
    };

    eframe::run_native(
        "MarkdownPad",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
