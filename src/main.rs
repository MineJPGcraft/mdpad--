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
mod find;
mod fonts;
mod gfx;
mod gfx_egui;
mod icon;
mod icon_data;
// The MDX / Markdoc runtime: expression evaluation over document data.
mod mdx;
mod recovery;
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

    // The backend is picked before the window exists, so a setting changed in the
    // UI only takes effect on the next start. `effective` also demotes a request
    // this machine cannot honour (Direct3D off Windows, Vulkan with no loader).
    let mut config = config::Config::load();
    let backend = config.renderer.effective();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1320.0, 900.0])
            .with_min_inner_size([760.0, 520.0])
            .with_title("MarkdownPad"),
        renderer: if backend == config::Backend::OpenGl {
            eframe::Renderer::Glow
        } else {
            eframe::Renderer::Wgpu
        },
        wgpu_options: wgpu_config(backend),
        ..Default::default()
    };

    let result = eframe::run_native(
        "MarkdownPad",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    );

    // A backend that cannot create a window leaves no UI to switch back from,
    // and the event loop cannot be created twice in one process. Write the next
    // candidate to the config so the following launch can start.
    if result.is_err() {
        let next = backend.fallback();
        eprintln!(
            "无法用 {} 创建窗口，已把渲染后端切换为 {}，请重新启动。",
            backend.label(),
            next.label()
        );
        config.renderer = next;
        let _ = config.save();
    }
    result
}

/// Restricts `wgpu` to a single API.
///
/// Left alone, `wgpu` enumerates every backend it can: on an Intel Arc machine
/// that means loading the Direct3D 12 user-mode driver, the shader compiler *and*
/// the Vulkan ICD plus WARP, none of which a 2D editor ever draws with.
fn wgpu_config(backend: config::Backend) -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::wgpu;

    let backends = match backend {
        config::Backend::DirectX => wgpu::Backends::DX12,
        config::Backend::Vulkan => wgpu::Backends::VULKAN,
        config::Backend::Metal => wgpu::Backends::METAL,
        // Opening the window through `glow`; this branch is never reached, but
        // leaving `wgpu` unrestricted would keep the old behaviour.
        config::Backend::OpenGl | config::Backend::Auto => wgpu::Backends::all(),
    };

    let mut options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_setup {
        setup.instance_descriptor.backends = backends;
    }
    options
}
