//! Sidebar sources: favourite folders and mounted drives, plus small painted icons.

use std::path::PathBuf;

use eframe::egui::{pos2, vec2, Color32, Painter, Rect, Shape, Stroke, StrokeKind};

use crate::ops::home_dir;

#[derive(Clone, Copy, PartialEq)]
pub enum Icon { Folder, File, App, Home, Desktop, Downloads, Drive, Removable, Network, Up }

pub struct Place {
    pub name: String,
    pub path: PathBuf,
    pub icon: Icon,
    /// (free, total) bytes for drives.
    pub space: Option<(u64, u64)>,
}

pub fn favorites() -> Vec<Place> {
    let Some(home) = home_dir() else { return Vec::new() };
    let mut out = vec![Place { name: user_name(&home), path: home.clone(), icon: Icon::Home, space: None }];
    for (name, icon) in [("Desktop", Icon::Desktop), ("Documents", Icon::Folder), ("Downloads", Icon::Downloads),
                         ("Pictures", Icon::Folder), ("Music", Icon::Folder)] {
        let p = home.join(name);
        if p.is_dir() { out.push(Place { name: name.into(), path: p, icon, space: None }); }
    }
    if cfg!(target_os = "macos") && std::path::Path::new("/Applications").is_dir() {
        out.push(Place { name: "Applications".into(), path: "/Applications".into(), icon: Icon::App, space: None });
    }
    out
}

/// Finder's own sidebar favourites (macOS), with the home folder added on top.
/// `None` when Finder's list can't be read; callers then keep [`favorites`].
pub fn finder_favorites() -> Option<Vec<Place>> {
    let paths = crate::finder::favorite_paths()?;
    let home = home_dir()?;
    let mut out = Vec::new();
    if !paths.contains(&home) {
        out.push(Place { name: user_name(&home), path: home.clone(), icon: Icon::Home, space: None });
    }
    for path in paths.into_iter().filter(|p| p.is_dir()) {
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let icon = if path == home { Icon::Home }
            else if path.extension().is_some_and(|e| e == "app") || path == std::path::Path::new("/Applications") { Icon::App }
            else if path == home.join("Desktop") { Icon::Desktop }
            else if path == home.join("Downloads") { Icon::Downloads }
            else { Icon::Folder };
        let name = if path == home { user_name(&home) }
            else if path.ends_with("Mobile Documents/com~apple~CloudDocs") { "iCloud Drive".into() }
            else if path == std::path::Path::new("/") { "Macintosh HD".into() }
            else { file_name.unwrap_or_else(|| path.display().to_string()) };
        out.push(Place { name, path, icon, space: None });
    }
    Some(out)
}

fn user_name(home: &std::path::Path) -> String {
    home.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Home".into())
}

/// All mounted drives and volumes (internal disks, USB sticks, network shares, disk images).
pub fn drives() -> Vec<Place> {
    let mut out: Vec<Place> = Vec::new();
    let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() else { return out };
    for d in disks.iter() {
        let mount = d.mount_point().to_path_buf();
        if is_system_mount(&mount) || out.iter().any(|p| p.path == mount) { continue; }
        out.push(Place {
            name: drive_name(&mount, &d.name().to_string_lossy()),
            icon: if d.is_removable() { Icon::Removable } else { Icon::Drive },
            space: Some((d.available_space(), d.total_space())),
            path: mount,
        });
    }
    // Network shares (SMB/AFP/NFS) are not reported as disks: add the other mount points in /Volumes.
    #[cfg(target_os = "macos")]
    if let Ok(rd) = std::fs::read_dir("/Volumes") {
        use std::os::unix::fs::MetadataExt;
        let volumes_dev = std::fs::metadata("/Volumes").map(|m| m.dev()).ok();
        for e in rd.flatten() {
            let p = e.path();
            let mounted = std::fs::metadata(&p).is_ok_and(|m| Some(m.dev()) != volumes_dev);
            let is_root_link = std::fs::canonicalize(&p).is_ok_and(|c| c == std::path::Path::new("/"));
            if !mounted || is_root_link || is_system_mount(&p) || out.iter().any(|x| x.path == p) { continue; }
            out.push(Place { name: e.file_name().to_string_lossy().into_owned(), path: p, icon: Icon::Network, space: None });
        }
    }
    out.sort_by_key(|p| (p.path != std::path::Path::new("/"), p.name.to_lowercase()));
    out
}

fn is_system_mount(m: &std::path::Path) -> bool {
    let s = m.to_string_lossy();
    if cfg!(target_os = "macos") {
        // Only "/" and direct children of /Volumes are user-facing; skip snapshots, Recovery, Time Machine.
        if s == "/" { return false; }
        let Some(name) = s.strip_prefix("/Volumes/") else { return true };
        return name.contains('/') || name.starts_with('.') || name.starts_with("com.apple.")
            || name == "Recovery" || name.starts_with("Backups of ");
    }
    if cfg!(target_os = "linux") {
        return ["/boot", "/snap", "/proc", "/sys", "/dev", "/run", "/var/lib", "/tmp"]
            .iter()
            .any(|p| s.starts_with(p) && !s.starts_with("/run/media"));
    }
    false
}

fn drive_name(mount: &std::path::Path, disk_name: &str) -> String {
    if mount == std::path::Path::new("/") {
        return if cfg!(target_os = "macos") { "Macintosh HD".into() } else { "Computer".into() };
    }
    if cfg!(windows) {
        let letter = mount.to_string_lossy().trim_end_matches('\\').to_string();
        return if disk_name.is_empty() { format!("Local Disk ({letter})") } else { format!("{disk_name} ({letter})") };
    }
    mount.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| disk_name.to_string())
}

// ---- icons --------------------------------------------------------------

const BLUE: Color32 = Color32::from_rgb(76, 156, 236);
const BLUE_DARK: Color32 = Color32::from_rgb(52, 124, 205);
const GRAY: Color32 = Color32::from_rgb(140, 146, 156);

/// Paint an icon into a square `r` (about 16 px).
pub fn paint_icon(p: &Painter, r: Rect, icon: Icon) {
    let s = r.width();
    let at = |x: f32, y: f32| pos2(r.left() + x * s, r.top() + y * s);
    match icon {
        Icon::Up => {
            p.rect_filled(Rect::from_min_max(at(0.05, 0.15), at(0.45, 0.32)), 2.0, BLUE_DARK);
            p.rect_filled(Rect::from_min_max(at(0.05, 0.25), at(0.95, 0.85)), 2.0, BLUE);
            let c = at(0.5, 0.55);
            p.line_segment([pos2(c.x, c.y - 0.1 * s), pos2(c.x, c.y + 0.2 * s)], Stroke::new(1.5, Color32::WHITE));
            p.add(Shape::convex_polygon(vec![pos2(c.x, c.y - 0.22 * s), pos2(c.x + 0.15 * s, c.y - 0.02 * s), pos2(c.x - 0.15 * s, c.y - 0.02 * s)], Color32::WHITE, Stroke::NONE));
        }
        Icon::Folder | Icon::Desktop | Icon::Downloads => {
            p.rect_filled(Rect::from_min_max(at(0.05, 0.15), at(0.45, 0.32)), 2.0, BLUE_DARK);
            p.rect_filled(Rect::from_min_max(at(0.05, 0.25), at(0.95, 0.85)), 2.0, BLUE);
            if icon == Icon::Downloads {
                let c = at(0.5, 0.55);
                p.line_segment([pos2(c.x, c.y - 0.18 * s), pos2(c.x, c.y + 0.12 * s)], Stroke::new(1.5, Color32::WHITE));
                p.add(Shape::convex_polygon(vec![pos2(c.x - 0.14 * s, c.y), pos2(c.x + 0.14 * s, c.y), pos2(c.x, c.y + 0.2 * s)], Color32::WHITE, Stroke::NONE));
            } else if icon == Icon::Desktop {
                p.rect_filled(Rect::from_min_max(at(0.3, 0.42), at(0.7, 0.68)), 1.0, Color32::WHITE);
            }
        }
        Icon::File => {
            let body = Rect::from_min_max(at(0.18, 0.05), at(0.82, 0.95));
            p.rect_filled(body, 1.5, Color32::WHITE);
            p.rect_stroke(body, 1.5, Stroke::new(1.0, GRAY), StrokeKind::Inside);
            for y in [0.38, 0.52, 0.66] {
                p.line_segment([at(0.3, y), at(0.7, y)], Stroke::new(1.0, Color32::from_gray(200)));
            }
        }
        Icon::App => {
            let body = Rect::from_min_max(at(0.08, 0.08), at(0.92, 0.92));
            p.rect_filled(body, 0.22 * s, Color32::from_rgb(120, 94, 214));
            p.circle_stroke(body.center(), 0.22 * s, Stroke::new(1.5, Color32::WHITE));
        }
        Icon::Home => {
            let roof = vec![at(0.5, 0.08), at(0.95, 0.5), at(0.05, 0.5)];
            p.add(Shape::convex_polygon(roof, BLUE_DARK, Stroke::NONE));
            p.rect_filled(Rect::from_min_max(at(0.18, 0.45), at(0.82, 0.92)), 1.0, BLUE);
            p.rect_filled(Rect::from_min_max(at(0.42, 0.62), at(0.58, 0.92)), 0.0, Color32::WHITE);
        }
        Icon::Network => {
            let c = Color32::from_rgb(64, 160, 150);
            p.rect_filled(Rect::from_min_max(at(0.12, 0.1), at(0.88, 0.58)), 2.0, c);
            p.rect_filled(Rect::from_min_max(at(0.2, 0.18), at(0.8, 0.5)), 1.0, Color32::from_rgb(200, 240, 235));
            p.line_segment([at(0.5, 0.58), at(0.5, 0.82)], Stroke::new(1.5, c));
            p.line_segment([at(0.15, 0.85), at(0.85, 0.85)], Stroke::new(1.5, c));
        }
        Icon::Drive | Icon::Removable => {
            let color = if icon == Icon::Drive { Color32::from_rgb(130, 138, 150) } else { Color32::from_rgb(232, 148, 52) };
            let body = Rect::from_min_max(at(0.05, 0.28), at(0.95, 0.78));
            p.rect_filled(body, 3.0, color);
            p.circle_filled(at(0.78, 0.53), 0.06 * s, Color32::from_rgb(120, 230, 120));
            p.line_segment([at(0.18, 0.53), at(0.55, 0.53)], Stroke::new(1.0, Color32::from_white_alpha(160)));
        }
    }
}

/// A thin capacity bar, as shown under each drive.
pub fn paint_capacity(p: &Painter, r: Rect, used_fraction: f32) {
    p.rect_filled(r, 2.0, Color32::from_gray(214));
    let w = r.width() * used_fraction.clamp(0.0, 1.0);
    let color = if used_fraction > 0.9 { Color32::from_rgb(222, 76, 64) } else { BLUE };
    p.rect_filled(Rect::from_min_size(r.min, vec2(w, r.height())), 2.0, color);
}

pub fn icon_for(e: &crate::panel::Entry) -> Icon {
    if e.is_parent { Icon::Up } else if e.is_app { Icon::App } else if e.is_dir { Icon::Folder } else { Icon::File }
}
