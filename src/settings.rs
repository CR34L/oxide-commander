//! App settings and per-panel view settings, saved between launches.
//!
//! Stored as simple `key=value` lines in:
//! - macOS:   ~/Library/Application Support/Mac Commander/settings.txt
//! - Windows: %APPDATA%\Mac Commander\settings.txt
//! - Linux:   $XDG_CONFIG_HOME/mac-commander/settings.txt (or ~/.config/…)

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::panel::SortKey;
use crate::theme::Theme;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct View {
    pub sort: SortKey,
    pub ascending: bool,
    pub show_hidden: bool,
}

impl Default for View {
    fn default() -> Self { View { sort: SortKey::Name, ascending: true, show_hidden: false } }
}

const SIDES: [&str; 2] = ["left", "right"];

/// What F1 opens.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TerminalMode {
    /// Built-in terminal below the file list of the current panel.
    Split,
    /// A window of a separate terminal app.
    External,
}

impl TerminalMode {
    pub const ALL: [TerminalMode; 2] = [TerminalMode::Split, TerminalMode::External];
    pub fn label(self) -> &'static str {
        match self {
            TerminalMode::Split => "Built-in, below the file list of the current panel",
            TerminalMode::External => "Separate terminal window",
        }
    }
    fn key(self) -> &'static str {
        match self { TerminalMode::Split => "split", TerminalMode::External => "external" }
    }
    fn from_key(s: &str) -> Option<Self> { Self::ALL.into_iter().find(|m| m.key() == s) }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub theme: Theme,
    /// F8 moves to the Trash (true) or deletes immediately after asking (false).
    pub use_trash: bool,
    /// Reopen the last folders at startup.
    pub remember_folders: bool,
    pub folders: [Option<PathBuf>; 2],
    pub views: [View; 2],
    /// macOS terminal app opened by F1 ("Terminal", "iTerm", …) in External mode.
    pub terminal: String,
    pub terminal_mode: TerminalMode,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::Light, use_trash: true, remember_folders: true,
            folders: [None, None], views: [View::default(); 2], terminal: "Terminal".into(),
            terminal_mode: TerminalMode::Split,
        }
    }
}

pub fn file() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        crate::ops::home_dir()?.join("Library/Application Support/Mac Commander")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?).join("Mac Commander")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
            .or_else(|| crate::ops::home_dir().map(|h| h.join(".config")))?
            .join("mac-commander")
    };
    Some(base.join("settings.txt"))
}

pub fn load() -> Settings {
    file().and_then(|f| fs::read_to_string(f).ok()).map(|t| parse(&t)).unwrap_or_default()
}

pub fn save(s: &Settings) {
    let Some(f) = file() else { return };
    if let Some(dir) = f.parent() { let _ = fs::create_dir_all(dir); }
    let _ = fs::write(f, format(s));
}

fn parse(text: &str) -> Settings {
    // Split at the first '=' only: folder paths may contain '='.
    let map: HashMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim(), v)).collect();
    let d = Settings::default();
    let flag = |k: &str, default: bool| map.get(k).map_or(default, |v| v.trim() == "true");
    Settings {
        theme: map.get("theme").and_then(|v| Theme::from_key(v.trim())).unwrap_or(d.theme),
        use_trash: flag("use_trash", d.use_trash),
        remember_folders: flag("remember_folders", d.remember_folders),
        folders: SIDES.map(|side| map.get(format!("{side}.folder").as_str()).filter(|v| !v.is_empty()).map(PathBuf::from)),
        views: parse_views(&map),
        terminal: map.get("terminal").map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).unwrap_or(d.terminal),
        terminal_mode: map.get("terminal_mode").and_then(|v| TerminalMode::from_key(v.trim())).unwrap_or(d.terminal_mode),
    }
}

fn parse_views(map: &HashMap<&str, &str>) -> [View; 2] {
    let map: HashMap<&str, &str> = map.iter().map(|(k, v)| (*k, v.trim())).collect();
    SIDES.map(|side| {
        let get = |k: &str| map.get(format!("{side}.{k}").as_str()).copied();
        let d = View::default();
        View {
            sort: get("sort").and_then(sort_from).unwrap_or(d.sort),
            ascending: get("ascending").map_or(d.ascending, |v| v == "true"),
            show_hidden: get("show_hidden").map_or(d.show_hidden, |v| v == "true"),
        }
    })
}

fn format(s: &Settings) -> String {
    let mut out = format!("theme={}\nuse_trash={}\nremember_folders={}\nterminal={}\nterminal_mode={}\n",
        s.theme.key(), s.use_trash, s.remember_folders, s.terminal, s.terminal_mode.key());
    for ((side, v), folder) in SIDES.iter().zip(&s.views).zip(&s.folders) {
        out += &format!("{side}.sort={}\n{side}.ascending={}\n{side}.show_hidden={}\n", sort_name(v.sort), v.ascending, v.show_hidden);
        if let Some(f) = folder.as_ref().and_then(|f| f.to_str()).filter(|f| !f.contains('\n')) {
            out += &format!("{side}.folder={f}\n");
        }
    }
    out
}

fn sort_name(k: SortKey) -> &'static str {
    match k { SortKey::Name => "name", SortKey::Size => "size", SortKey::Kind => "kind", SortKey::Date => "date" }
}

fn sort_from(s: &str) -> Option<SortKey> {
    Some(match s { "name" => SortKey::Name, "size" => SortKey::Size, "kind" => SortKey::Kind, "date" => SortKey::Date, _ => return None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let s = Settings {
            theme: Theme::Norton,
            use_trash: false,
            remember_folders: true,
            folders: [Some(PathBuf::from("/tmp/a=b c")), None],
            terminal: "iTerm".into(),
            terminal_mode: TerminalMode::External,
            views: [
                View { sort: SortKey::Size, ascending: false, show_hidden: true },
                View { sort: SortKey::Date, ascending: true, show_hidden: false },
            ],
        };
        assert_eq!(parse(&format(&s)), s);
        assert_eq!(parse("garbage\nleft.sort=bogus\ntheme=purple"), Settings::default());
        // Files written by the previous version (views only) still load.
        assert_eq!(parse("left.sort=size\nleft.ascending=false").views[0].sort, SortKey::Size);
    }
}
