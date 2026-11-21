//! One file browser panel (left or right): folder contents, cursor, selection, history.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use crate::sizes::{DirSizes, ScanHandle};

pub struct Entry {
    pub name: String,
    /// Lower-case name, computed once (sorting compares it many times).
    lower: String,
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
    /// Folder being read in the background (big or slow folders, e.g. on a network share).
    pending: Option<Pending>,
    /// Error from a background read, for the app to show.
    error: Option<io::Error>,
    /// How long a read may block before it continues in the background (tests use 0).
    sync_wait: Duration,
}

/// How long a folder read may block the UI before it continues in the background.
const SYNC_WAIT: Duration = Duration::from_millis(150);

struct Pending {
    rx: Receiver<io::Result<(Vec<Entry>, Option<SystemTime>)>>,
    /// Navigation (start at `select` or the top) or refresh (keep the cursor on the same name or row).
    navigate: bool,
    select: Option<String>,
    row: usize,
    /// Folder to return to if the new one cannot be read.
    prev: Option<PathBuf>,
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
            pending: None, error: None, sync_wait: SYNC_WAIT,
        };
        if p.start_load(true, None, 0, None).is_err() {
            p.path = PathBuf::from(std::path::MAIN_SEPARATOR_STR);
            let _ = p.start_load(true, None, 0, None);
        }
        p
    }

    /// Read `self.path` on a worker thread. Small folders finish within `SYNC_WAIT` and are
    /// applied at once (an error is returned then); bigger or slow ones finish later in
    /// `poll_changes`, and the panel shows "Loading…" meanwhile (navigation only: a refresh
    /// keeps the old list until the new one is there).
    fn start_load(&mut self, navigate: bool, select: Option<String>, row: usize, prev: Option<PathBuf>) -> io::Result<()> {
        let (tx, rx) = mpsc::channel();
        let path = self.path.clone();
        std::thread::spawn(move || { let _ = tx.send(read_entries(&path)); });
        self.pending = Some(Pending { rx, navigate, select, row, prev });
        match self.pending.as_ref().unwrap().rx.recv_timeout(self.sync_wait) {
            Ok(result) => self.finish_load(result),
            Err(_) => {
                if navigate {
                    // Only the ".." row until the folder is read.
                    self.entries = self.path.parent().map(parent_entry).into_iter().collect();
                    self.visible.clear();
                    self.selected.clear();
                    self.rebuild_visible();
                    self.set_cursor(0);
                }
                Ok(())
            }
        }
    }

    fn finish_load(&mut self, result: io::Result<(Vec<Entry>, Option<SystemTime>)>) -> io::Result<()> {
        let Some(p) = self.pending.take() else { return Ok(()) };
        let (mut entries, mtime) = match result {
            Ok(r) => r,
            Err(e) => {
                // Back to the folder we came from (which could be read before).
                if let Some(prev) = p.prev {
                    if self.back.last() == Some(&prev) { self.back.pop(); }
                    // The cursor goes to the folder that could not be opened.
                    let failed = self.path.file_name().map(|n| n.to_string_lossy().into_owned());
                    self.path = prev;
                    let _ = self.start_load(true, failed, 0, None);
                }
                return Err(e);
            }
        };
        if let Some(parent) = self.path.parent() { entries.insert(0, parent_entry(parent)); }
        self.entries = entries;
        // The old row list points into the old entries: drop it before anything reads it.
        self.visible.clear();
        self.dir_mtime = mtime;
        self.sort_entries();
        if p.navigate {
            self.selected.clear();
            let row = p.select.and_then(|n| self.row_of_name(&n)).unwrap_or(0);
            self.set_cursor(row);
            self.anchor = row;
            self.scan_sizes(true);
        } else {
            self.selected.retain(|p| p.exists());
            let row = p.select.and_then(|n| self.row_of_name(&n)).unwrap_or(p.row);
            self.set_cursor(row);
            self.scan_sizes(false);
        }
        self.scroll_to_cursor = true;
        Ok(())
    }

    /// The folder is still being read in the background.
    pub fn is_loading(&self) -> bool { self.pending.is_some() }

    /// Error from a background folder read (shown once by the app).
    pub fn take_error(&mut self) -> Option<io::Error> { self.error.take() }

    fn sort_entries(&mut self) {
        let (key, asc) = (self.sort, self.ascending);
        let sizes = self.sizes.clone();
        let cache = sizes.cache();
        let size = |e: &Entry| if e.is_dir { cache.get(&e.path).copied() } else { Some(e.size) };
        // ".." stays the first row whatever the sort order.
        let start = usize::from(self.entries.first().is_some_and(|e| e.is_parent));
        self.entries[start..].sort_by(|a, b| {
            let folders_first = b.is_folder().cmp(&a.is_folder());
            let by_name = || a.lower.cmp(&b.lower);
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
        let _ = self.start_load(false, keep, old_row, None);
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
        // A background folder read finished.
        if let Some(p) = &self.pending {
            match p.rx.try_recv() {
                Ok(result) => if let Err(e) = self.finish_load(result) { self.error = Some(e); },
                Err(mpsc::TryRecvError::Disconnected) => self.pending = None,
                Err(mpsc::TryRecvError::Empty) => return,
            }
        }
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
        if old != self.path {
            self.back.push(old.clone());
            self.forward.clear();
            self.filter.clear();
        }
        // On failure (no access, gone) `finish_load` returns to `old`.
        let back_to = Some(old.clone()).filter(|o| *o != self.path);
        self.start_load(true, select.map(str::to_owned), 0, back_to)?;
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

    /// PgDn / PgUp (Total Commander style): first to the last / first row fully visible,
    /// then a page further each press. `row_h`: row height in points.
    pub fn page(&mut self, down: bool, row_h: f32, extend: bool) {
        let rows = ((self.view_height / row_h).floor() as usize).max(1);
        let first = (self.scroll_offset / row_h).ceil() as usize;
        let last = (((self.scroll_offset + self.view_height) / row_h).floor() as usize).saturating_sub(1).max(first);
        let target = match down {
            true if self.cursor < last => last,
            true => self.cursor + rows - 1,
            false if self.cursor > first => first,
            false => self.cursor.saturating_sub(rows - 1),
        };
        self.move_cursor(target as isize - self.cursor as isize, extend);
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

fn parent_entry(parent: &Path) -> Entry {
    Entry {
        name: "..".into(), lower: "..".into(), path: parent.to_path_buf(), is_dir: true, is_parent: true,
        is_app: false, hidden: false, size: 0, modified: None,
    }
}

/// Read a folder: one `stat` per item (two only for symbolic links, which are followed).
/// Runs on a worker thread. Also returns the folder's own modification time.
fn read_entries(dir: &Path) -> io::Result<(Vec<Entry>, Option<SystemTime>)> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let path = item.path();
        let is_link = item.file_type().is_ok_and(|t| t.is_symlink());
        let meta = if is_link { fs::metadata(&path).or_else(|_| item.metadata()).ok() } else { item.metadata().ok() };
        let name = item.file_name().to_string_lossy().into_owned();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        entries.push(Entry {
            is_app: is_dir && cfg!(target_os = "macos") && name.ends_with(".app"),
            hidden: is_hidden(&name, meta.as_ref()),
            size: if is_dir { 0 } else { meta.as_ref().map_or(0, |m| m.len()) },
            modified: meta.as_ref().and_then(|m| m.modified().ok()),
            is_dir,
            is_parent: false,
            lower: name.to_lowercase(),
            name,
            path,
        });
    }
    let mtime = fs::metadata(dir).and_then(|m| m.modified()).ok();
    Ok((entries, mtime))
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

    /// Wait until a background folder read has been applied.
    fn settle(p: &mut Panel) {
        for _ in 0..500 {
            p.poll_changes();
            if !p.is_loading() { return; }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("folder read did not finish");
    }

    #[test]
    fn background_loading() {
        let t = std::env::temp_dir().join(format!("mc-bg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(t.join("big")).unwrap();
        for i in 0..2000 { fs::write(t.join("big").join(format!("f{i:04}.txt")), "x").unwrap(); }
        let mut p = Panel::new(t.clone(), DirSizes::new(None));
        p.sync_wait = Duration::ZERO; // every read continues in the background

        p.navigate(&t.join("big"), None).unwrap();
        assert_eq!(p.path, t.join("big"), "the path changes at once");
        settle(&mut p);
        assert_eq!(p.len(), 2001, "'..' plus all files once loaded");
        assert_eq!(p.row(1).name, "f0000.txt");

        // PgDn / PgUp: to the edge of the visible page first, then page by page.
        (p.view_height, p.scroll_offset) = (220.0, 0.0); // 10 rows of 22 points
        p.page(true, 22.0, false);
        assert_eq!(p.cursor, 9, "PgDn: last visible row");
        p.page(true, 22.0, false);
        assert_eq!(p.cursor, 18, "PgDn again: one page further");
        p.scroll_offset = 9.0 * 22.0; // the list scrolled so row 18 is the last visible one
        p.page(false, 22.0, false);
        assert_eq!(p.cursor, 9, "PgUp: first visible row");
        p.page(false, 22.0, false);
        assert_eq!(p.cursor, 0, "PgUp again: one page up");

        p.go_up().unwrap();
        settle(&mut p);
        assert_eq!(p.current().unwrap().name, "big", "cursor returns to the folder we left");

        // A folder that cannot be read: back where we were, error reported once.
        let gone = t.join("missing");
        let _ = p.navigate(&gone, None);
        settle(&mut p);
        assert_eq!(p.path, t, "back in the previous folder");
        p.go_back().ok();
        settle(&mut p);
        assert_ne!(p.path, gone);
        fs::remove_dir_all(&t).unwrap();
    }
}
