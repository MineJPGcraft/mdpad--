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

    // A panic inside the render path is swallowed by the event loop: the window
    // stays open but stops drawing, which looks exactly like a blank page. Record
    // it where it can be read back.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(path) = app::App::report_path().map(|p| p.with_file_name("mdpad-panic.log")) {
            let _ = std::fs::write(&path, format!("{info}\n"));
        }
        previous(info);
    }));

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
