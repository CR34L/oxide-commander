//! Oxide Commander: a dual-panel file manager (Total Commander style keys,
//! Path Finder style look). Cross-platform via eframe/egui.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod finder;
mod jobs;
mod ops;
mod panel;
mod places;
mod settings;
mod sizes;
mod terminal;
mod theme;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let home = ops::home_dir().unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR_STR));
    // Folders given on the command line win over the remembered ones.
    let mut args = std::env::args().skip(1).map(PathBuf::from);
    let (left_arg, right_arg) = (args.next(), args.next());
    let downloads = home.join("Downloads");
    let defaults = [home.clone(), if downloads.is_dir() { downloads } else { home.clone() }];

    // Window title: "Oxide Commander | 0.1.1.6 | Marko Döring | OVONEX" (version.build).
    const TITLE: &str = concat!("Oxide Commander | ", env!("CARGO_PKG_VERSION"), ".",
        env!("BUILD_NUMBER"), " | Marko Döring | OVONEX");

    // Reopen the window where and as big as it was (App::new puts it back on screen if
    // that place is gone, e.g. a disconnected monitor).
    let saved = settings::load().window;
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(TITLE)
        .with_inner_size(saved.map_or([1280.0, 780.0], |[_, _, w, h]| [w, h]))
        .with_min_inner_size([760.0, 420.0])
        .with_drag_and_drop(true);
    if let Some([x, y, _, _]) = saved { viewport = viewport.with_position([x, y]); }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Oxide Commander", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, [left_arg, right_arg], defaults)))))
}
