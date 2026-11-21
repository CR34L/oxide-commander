//! Mac Commander: a dual-panel file manager (Total Commander style keys,
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

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Mac Commander")
            .with_inner_size([1280.0, 780.0])
            .with_min_inner_size([760.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Mac Commander", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, [left_arg, right_arg], defaults)))))
}
