mod app;
mod artwork;
mod service;
mod state;
mod style;

use std::path::PathBuf;

fn window_icon() -> eframe::egui::IconData {
    let icon = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .expect("bundled icon decodes")
        .into_rgba8();
    let (width, height) = icon.dimensions();
    eframe::egui::IconData {
        rgba: icon.into_raw(),
        width,
        height,
    }
}

/// Lets the C allocator keep two memory pools instead of one per worker thread. Each thread that
/// decodes a track or a cover otherwise gets a pool of its own that holds on to its busiest
/// moment for good, so a long session creeps up by tens of megabytes. `MALLOC_ARENA_MAX` in the
/// environment still wins.
#[cfg(target_os = "linux")]
fn share_allocator_pools() {
    unsafe extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }
    const M_ARENA_MAX: i32 = -8;
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        // SAFETY: called first thing in `main`, before any other thread exists.
        unsafe { mallopt(M_ARENA_MAX, 2) };
    }
}

#[cfg(not(target_os = "linux"))]
fn share_allocator_pools() {}

fn main() -> anyhow::Result<()> {
    share_allocator_pools();
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
            .with_app_id("skratch")
            .with_icon(window_icon())
            .with_decorations(false)
            .with_transparent(true),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "Skratch",
        options,
        Box::new(move |cc| Ok(Box::new(app::Shell::new(cc, data_dir)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
