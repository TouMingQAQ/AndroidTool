#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adb;
mod app;
mod branding;
mod config;
mod device;
mod gamepad;
mod gamepad_hid;
mod gamepad_view;
mod installer;
mod logcat;
mod mirror;
#[cfg(windows)]
mod mirror_window;
mod screen;
mod theme;
mod tools;
mod util;
mod wireless;

use app::AndroidToolApp;
use config::AppConfig;
use eframe::egui;

fn main() -> eframe::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--tool-worker") {
        tools::worker_main();
        return Ok(());
    }
    let config = AppConfig::load();
    let viewport = egui::ViewportBuilder::default()
        .with_title("AndroidTool")
        .with_icon(branding::window_icon())
        .with_transparent(true)
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([1080.0, 700.0])
        .with_app_id("androidtool");

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "AndroidTool",
        options,
        Box::new(move |cc| {
            theme::setup(&cc.egui_ctx);
            config.theme.apply(&cc.egui_ctx);
            Ok(Box::new(AndroidToolApp::new(cc, config)))
        }),
    )
}
