//! One file browser panel (left or right): folder contents, cursor, selection, history.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::sizes::{DirSizes, ScanHandle};

pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// The ".." row that leads to the parent folder.
    pub is_parent: bool,
    /// macOS application bundle: shown and opened like a file.
    pub is_app: bool,
    pub hidden: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl Entry {
    pub fn is_folder(&self) -> bool { self.is_dir && !self.is_app }

    pub fn kind(&self) -> String {
        if self.is_parent { return String::new(); }
        if self.is_app { return "Application".into(); }
        if self.is_dir { return "Folder".into(); }
        match Path::new(&self.name).extension() {
            Some(ext) if !self.name.starts_with('.') || self.name.matches('.').count() > 1 =>
                format!("{} file", ext.to_string_lossy().to_uppercase()),
            _ => "Document".into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SortKey { Name, Size, Kind, Date }

pub struct Panel {
    pub path: PathBuf,
    entries: Vec<Entry>,
    /// Indices into `entries` after hidden-file and filter rules.
    visible: Vec<usize>,
    /// Row (index into `visible`) with keyboard focus.
    pub cursor: usize,
    /// Anchor for shift-click range selection.
    anchor: usize,
    pub selected: HashSet<PathBuf>,
    pub filter: String,
    pub sort: SortKey,
    pub ascending: bool,
    pub show_hidden: bool,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    pub scroll_to_cursor: bool,
    pub scroll_offset: f32,
    pub view_height: f32,
    dir_mtime: Option<SystemTime>,
    last_poll: Instant,
    sizes: DirSizes,
    scan: ScanHandle,
    seen_sizes: u64,
    last_resort: Instant,
}

impl Panel {
    pub fn new(path: PathBuf, sizes: DirSizes) -> Self {
        let mut p = Panel {
            path, entries: Vec::new(), visible: Vec::new(), cursor: 0, anchor: 0,
            selected: HashSet::new(), filter: String::new(), sort: SortKey::Name, ascending: true,
            show_hidden: false, back: Vec::new(), forward: Vec::new(),
            scroll_to_cursor: false, scroll_offset: 0.0, view_height: 0.0,
            dir_mtime: None, last_poll: Instant::now(),
            sizes, scan: ScanHandle::default(), seen_sizes: 0, last_resort: Instant::now(),
        };
        if p.load().is_err() {
            p.path = PathBuf::from(std::path::MAIN_SEPARATOR_STR);
            let _ = p.load();
        }
        p.scan_sizes(true);
        p
    }

    fn load(&mut self) -> io::Result<()> {
        let mut entries = Vec::new();
        for item in fs::read_dir(&self.path)? {
            let Ok(item) = item else { continue };
            let path = item.path();
            let link_meta = fs::symlink_metadata(&path).ok();
            let meta = fs::metadata(&path).ok().or(link_meta);
            let name = item.file_name().to_string_lossy().into_owned();
            let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
            entries.push(Entry {
                is_app: is_dir && cfg!(target_os = "macos") && name.ends_with(".app"),
                hidden: is_hidden(&name, meta.as_ref()),
                size: if is_dir { 0 } else { meta.as_ref().map_or(0, |m| m.len()) },
                modified: meta.as_ref().and_then(|m| m.modified().ok()),
                is_dir,
                is_parent: false,
                name,
                path,
            });
        }
        if let Some(parent) = self.path.parent() {
            entries.insert(0, Entry {
                name: "..".into(), path: parent.to_path_buf(), is_dir: true, is_parent: true,
                is_app: false, hidden: false, size: 0, modified: None,
            });
        }
        self.entries = entries;
        // The old row list points into the old entries: drop it before anything reads it.
        self.visible.clear();
        self.dir_mtime = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        self.sort_entries();
        Ok(())
    }

    fn sort_entries(&mut self) {
        let (key, asc) = (self.sort, self.ascending);
        let sizes = self.sizes.clone();
        let cache = sizes.cache();
        let size = |e: &Entry| if e.is_dir { cache.get(&e.path).copied() } else { Some(e.size) };
        // ".." stays the first row whatever the sort order.
        let start = usize::from(self.entries.first().is_some_and(|e| e.is_parent));
        self.entries[start..].sort_by(|a, b| {
            let folders_first = b.is_folder().cmp(&a.is_folder());
            let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
            let ord = match key {
                SortKey::Name => by_name(),
                SortKey::Size => size(a).cmp(&size(b)).then_with(by_name),
                SortKey::Kind => a.kind().cmp(&b.kind()).then_with(by_name),
                SortKey::Date => a.modified.cmp(&b.modified).then_with(by_name),
            };
            folders_first.then(if asc { ord } else { ord.reverse() })
        });
        drop(cache);
        self.seen_sizes = self.sizes.version();
        self.rebuild_visible();
    }

    pub fn rebuild_visible(&mut self) {
        let keep = self.current().map(|e| e.path.clone());
        let filter = self.filter.to_lowercase();
        self.visible = (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                e.is_parent || (self.show_hidden || !e.hidden) && (filter.is_empty() || e.name.to_lowercase().contains(&filter))
            })
            .collect();
        let row = keep.and_then(|p| self.row_of_path(&p)).unwrap_or(self.cursor);
        self.set_cursor(row);
    }

    pub fn len(&self) -> usize { self.visible.len() }

    pub fn row(&self, row: usize) -> &Entry { &self.entries[self.visible[row]] }

    pub fn current(&self) -> Option<&Entry> {
        self.visible.get(self.cursor).and_then(|&i| self.entries.get(i))
    }

    fn row_of_path(&self, p: &Path) -> Option<usize> {
        self.visible.iter().position(|&i| self.entries[i].path == p)
    }

    pub fn row_of_name(&self, name: &str) -> Option<usize> {
        self.visible.iter().position(|&i| self.entries[i].name == name)
    }

    pub fn view(&self) -> crate::settings::View {
        crate::settings::View { sort: self.sort, ascending: self.ascending, show_hidden: self.show_hidden }
    }

    pub fn set_view(&mut self, v: crate::settings::View) {
        (self.sort, self.ascending, self.show_hidden) = (v.sort, v.ascending, v.show_hidden);
        self.sort_entries();
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort == key { self.ascending = !self.ascending; } else { self.sort = key; self.ascending = true; }
        self.sort_entries();
        self.scroll_to_cursor = true;
    }

    /// Re-read the folder; keep the cursor on `select`, the current name, or the same row.
    pub fn refresh(&mut self, select: Option<&str>) {
        let keep = select.map(str::to_owned).or_else(|| self.current().map(|e| e.name.clone()));
        let old_row = self.cursor;
        while !self.path.is_dir() {
            if !self.path.pop() { break; }
        }
        if self.load().is_err() { return; }
        self.selected.retain(|p| p.exists());
        let row = keep.and_then(|n| self.row_of_name(&n)).unwrap_or(old_row);
        self.set_cursor(row);
        self.scroll_to_cursor = true;
        self.scan_sizes(false);
    }

    /// Re-read the folder and recalculate every folder size (⌘R, after copy/move/trash).
    pub fn rescan(&mut self) {
        self.refresh(None);
        self.scan_sizes(true);
    }

    /// Start measuring sub-folders in the background. `all = false` only measures
    /// folders without a known size (cheap auto-refresh when something changed).
    fn scan_sizes(&mut self, all: bool) {
        let cache = self.sizes.cache();
        let dirs: Vec<PathBuf> = self.entries.iter()
            .filter(|e| e.is_dir && !e.is_parent && (all || !cache.contains_key(&e.path)))
            .map(|e| e.path.clone())
            .collect();
        drop(cache);
        if all || !dirs.is_empty() { self.sizes.scan(dirs, &self.scan); }
    }

    /// File size, or the calculated folder size (`None` while still calculating).
    pub fn size_of(&self, e: &Entry) -> Option<u64> {
        if e.is_parent { None } else if e.is_dir { self.sizes.get(&e.path) } else { Some(e.size) }
    }

    pub fn is_calculating(&self) -> bool { self.scan.is_running() }

    /// Cheap periodic check whether the folder changed on disk (other apps, other panel).
    pub fn poll_changes(&mut self) {
        // New folder sizes arrived: re-sort when sorting by size (at most 3x per second).
        if self.sort == SortKey::Size && self.sizes.version() != self.seen_sizes
            && self.last_resort.elapsed() > Duration::from_millis(300) {
            self.last_resort = Instant::now();
            self.sort_entries();
        }
        if self.last_poll.elapsed() < Duration::from_millis(1000) { return; }
        self.last_poll = Instant::now();
        let now = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if now != self.dir_mtime || !self.path.is_dir() { self.refresh(None); }
    }

    pub fn navigate(&mut self, dir: &Path, select: Option<&str>) -> io::Result<()> {
        self.go(dir, select)?;
        Ok(())
    }

    fn go(&mut self, dir: &Path, select: Option<&str>) -> io::Result<PathBuf> {
        let old = std::mem::replace(&mut self.path, dir.to_path_buf());
        if let Err(e) = self.load() {
            self.path = old;
            let _ = self.load();
            return Err(e);
        }
        if old != self.path {
            self.back.push(old.clone());
            self.forward.clear();
            self.filter.clear();
            self.rebuild_visible();
        }
        self.selected.clear();
        let row = select.and_then(|n| self.row_of_name(n)).unwrap_or(0);
        self.set_cursor(row);
        self.anchor = row;
        self.scroll_to_cursor = true;
        self.scan_sizes(true);
        Ok(old)
    }

    pub fn go_up(&mut self) -> io::Result<()> {
        let Some(parent) = self.path.parent().map(Path::to_path_buf) else { return Ok(()) };
        let child = self.path.file_name().map(|n| n.to_string_lossy().into_owned());
        self.navigate(&parent, child.as_deref())
    }

    pub fn can_back(&self) -> bool { !self.back.is_empty() }
    pub fn can_forward(&self) -> bool { !self.forward.is_empty() }

    pub fn go_back(&mut self) -> io::Result<()> {
        let Some(dir) = self.back.pop() else { return Ok(()) };
        let fwd = std::mem::take(&mut self.forward);
        let child = self.path.file_name().map(|n| n.to_string_lossy().into_owned());
        let r = self.go(&dir, child.as_deref());
        self.back.pop(); // `go` pushed the folder we just left
        self.forward = fwd;
        if let Ok(old) = &r { self.forward.push(old.clone()); }
        r.map(|_| ())
    }

    pub fn go_forward(&mut self) -> io::Result<()> {
        let Some(dir) = self.forward.pop() else { return Ok(()) };
        let fwd = std::mem::take(&mut self.forward);
        let r = self.go(&dir, None);
        self.forward = fwd;
        r.map(|_| ())
    }

    pub fn set_cursor(&mut self, row: usize) {
        self.cursor = row.min(self.visible.len().saturating_sub(1));
    }

    /// Keyboard movement: plain moves clear the selection; with `extend` it grows from the anchor.
    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        let row = (self.cursor as isize + delta).max(0) as usize;
        self.set_cursor(row);
        if extend { self.select_range(self.cursor); } else { self.selected.clear(); self.anchor = self.cursor; }
        self.scroll_to_cursor = true;
    }

    /// Mouse click on a row: plain = select only it, toggle (Cmd/Ctrl) or range (Shift).
    pub fn click(&mut self, row: usize, toggle: bool, range: bool) {
        if range {
            self.cursor = row;
            self.select_range(row);
        } else if toggle {
            let p = self.row(row).path.clone();
            if self.selected.is_empty() && self.cursor != row
                && let Some(c) = self.current().filter(|c| !c.is_parent) { self.selected.insert(c.path.clone()); }
            if !self.row(row).is_parent && !self.selected.remove(&p) { self.selected.insert(p); }
            self.cursor = row;
            self.anchor = row;
        } else {
            self.selected.clear();
            self.cursor = row;
            self.anchor = row;
        }
    }

    fn select_range(&mut self, to: usize) {
        let (a, b) = if self.anchor <= to { (self.anchor, to) } else { (to, self.anchor) };
        self.selected = (a..=b).filter(|&r| r < self.len() && !self.row(r).is_parent).map(|r| self.row(r).path.clone()).collect();
    }

    /// Commander-style mark (Space / Insert): toggle and move down.
    pub fn toggle_mark(&mut self) {
        let Some(e) = self.current() else { return };
        let (p, is_parent) = (e.path.clone(), e.is_parent);
        if !is_parent && !self.selected.remove(&p) { self.selected.insert(p); }
        self.set_cursor(self.cursor + 1);
        self.anchor = self.cursor;
        self.scroll_to_cursor = true;
    }

    pub fn select_all(&mut self) {
        self.selected = (0..self.len()).filter(|&r| !self.row(r).is_parent).map(|r| self.row(r).path.clone()).collect();
    }

    pub fn is_selected(&self, row: usize) -> bool {
        if self.selected.is_empty() { row == self.cursor } else { self.selected.contains(&self.row(row).path) }
    }

    /// Selected items, or the item under the cursor when nothing is selected.
    pub fn targets(&self) -> Vec<PathBuf> {
        if self.selected.is_empty() {
            return self.current().filter(|e| !e.is_parent).map(|e| vec![e.path.clone()]).unwrap_or_default();
        }
        (0..self.len()).map(|r| self.row(r)).filter(|e| !e.is_parent && self.selected.contains(&e.path)).map(|e| e.path.clone()).collect()
    }

    /// (visible items, selected count, selected bytes)
    pub fn summary(&self) -> (usize, usize, u64) {
        let sel: Vec<&Entry> = (0..self.len()).map(|r| self.row(r)).filter(|e| !e.is_parent && self.selected.contains(&e.path)).collect();
        let items = (0..self.len()).filter(|&r| !self.row(r).is_parent).count();
        (items, sel.len(), sel.iter().map(|e| self.size_of(e).unwrap_or(0)).sum())
    }
}

#[cfg(windows)]
fn is_hidden(name: &str, meta: Option<&fs::Metadata>) -> bool {
    use std::os::windows::fs::MetadataExt;
    name.starts_with('.') || meta.is_some_and(|m| m.file_attributes() & 0x2 != 0)
}

#[cfg(not(windows))]
fn is_hidden(name: &str, _meta: Option<&fs::Metadata>) -> bool {
    name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> PathBuf {
        let t = std::env::temp_dir().join(format!("mc-panel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(t.join("alpha/inner")).unwrap();
        fs::create_dir_all(t.join("beta")).unwrap();
        fs::write(t.join("b.txt"), "12345").unwrap();
        fs::write(t.join("a.txt"), "1").unwrap();
        fs::write(t.join(".hidden"), "").unwrap();
        t
    }

    #[test]
    fn listing_history_filter_selection() {
        let t = tree();
        let mut p = Panel::new(t.clone(), DirSizes::new(None));
        let names: Vec<_> = (0..p.len()).map(|r| p.row(r).name.clone()).collect();
        assert_eq!(names, ["..", "alpha", "beta", "a.txt", "b.txt"], "'..' on top, folders first, hidden excluded");

        p.navigate(&t.join("alpha"), None).unwrap();
        p.go_up().unwrap();
        assert_eq!(p.current().unwrap().name, "alpha", "cursor returns to the folder we left");
        p.go_back().unwrap();
        assert_eq!(p.path, t.join("alpha"));
        p.go_forward().unwrap();
        assert_eq!(p.path, t);

        p.filter = "txt".into();
        p.rebuild_visible();
        assert_eq!(p.len(), 3, "'..' survives the filter");
        p.filter.clear();
        p.rebuild_visible();

        p.set_cursor(0);
        assert!(p.targets().is_empty(), "'..' is never an operation target");
        p.select_all();
        assert!(!p.selected.contains(&p.row(0).path), "select all skips '..'");
        p.set_cursor(3);
        p.selected.clear();
        assert_eq!(p.targets(), vec![t.join("a.txt")], "no selection -> cursor item");
        p.click(3, false, false);
        p.click(4, false, true);
        assert_eq!(p.summary(), (4, 2, 6), "shift-click selects range a.txt..b.txt");
        p.toggle_mark();
        assert_eq!(p.targets(), vec![t.join("a.txt")], "space toggles b.txt off");

        p.show_hidden = true;
        p.rebuild_visible();
        assert_eq!(p.len(), 6);

        // Regression: cursor far down, then open a folder with fewer items (used to panic).
        p.set_cursor(5);
        p.navigate(&t.join("alpha"), None).unwrap();
        assert_eq!(p.current().unwrap().name, "..");
        p.go_up().unwrap();
        p.set_cursor(5);
        p.refresh(None);
        assert!(p.current().is_some());
        fs::remove_dir_all(&t).unwrap();
    }
}
