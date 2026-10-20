mod app;
mod artwork;
mod service;
mod state;
mod style;

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let data_dir = if let Some(path) = std::env::var_os("SKRATCH_DATA_DIR") {
        PathBuf::from(path)
    } else {
        directories::ProjectDirs::from("", "", "skratch")
            .ok_or_else(|| anyhow::anyhow!("Cannot find application data directory"))?
            .data_dir()
            .to_path_buf()
    };
    std::fs::create_dir_all(&data_dir)?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 600.0])
            .with_decorations(false)
            .with_transparent(true),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "Skratch",
        options,
        Box::new(move |cc| Ok(Box::new(app::Skratch::new(cc, data_dir)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
