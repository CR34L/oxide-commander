//! Background copy / move / trash / delete with progress and cancel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::ops::{self, Clash};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind { Copy, Move, Trash, Delete }

impl Kind {
    pub fn done_verb(self) -> &'static str {
        match self { Kind::Copy => "Copied", Kind::Move => "Moved", Kind::Trash => "Moved to Trash", Kind::Delete => "Deleted" }
    }
    pub fn ing(self) -> &'static str {
        match self { Kind::Copy => "Copying", Kind::Move => "Moving", Kind::Trash => "Moving to Trash", Kind::Delete => "Deleting" }
    }
}

#[derive(Default)]
pub struct Progress {
    /// Progress units done / total: bytes when `in_bytes`, otherwise items.
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub in_bytes: AtomicBool,
    /// Still adding up sizes before copying starts.
    pub measuring: AtomicBool,
    pub items_done: AtomicUsize,
    pub current: Mutex<String>,
    pub finished: AtomicBool,
    pub cancel: AtomicBool,
    pub error: Mutex<Option<String>>,
}

impl Progress {
    fn set_current(&self, path: &Path) {
        *self.current.lock().unwrap() = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    }
}

pub struct Job {
    pub kind: Kind,
    pub items: usize,
    pub progress: Arc<Progress>,
}

/// `pairs`: (source, target). Target is ignored for Trash/Delete. `clash`: what happens to
/// targets that already exist (the user chose it before; Rename already changed the targets).
pub fn spawn(kind: Kind, pairs: Vec<(PathBuf, PathBuf)>, clash: Clash, ctx: eframe::egui::Context) -> Job {
    let progress = Arc::new(Progress::default());
    let job = Job { kind, items: pairs.len(), progress: progress.clone() };
    std::thread::spawn(move || {
        if let Err(e) = run(kind, &pairs, clash, &progress, &ctx) {
            *progress.error.lock().unwrap() = Some(e);
        }
        progress.finished.store(true, Ordering::Relaxed);
        ctx.request_repaint();
    });
    job
}

fn run(kind: Kind, pairs: &[(PathBuf, PathBuf)], clash: Clash, p: &Progress, ctx: &eframe::egui::Context) -> Result<(), String> {
    let named = |src: &Path, e: std::io::Error| format!("{}: {e}", src.file_name().unwrap_or_default().to_string_lossy());
    let cancelled = || p.cancel.load(Ordering::Relaxed);
    let mut tick = |path: &Path, bytes: u64| -> std::io::Result<()> {
        if cancelled() { return Err(std::io::Error::other("cancelled")); }
        p.set_current(path);
        p.done.fetch_add(bytes, Ordering::Relaxed);
        ctx.request_repaint();
        Ok(())
    };

    // Replace: existing targets go first, so the result is exactly the source.
    if clash == Clash::Replace && matches!(kind, Kind::Copy | Kind::Move) {
        for (src, dst) in pairs {
            if cancelled() { return Ok(()); }
            if ops::exists(dst) && !ops::same_file(src, dst) {
                p.set_current(dst);
                ops::remove_path(dst).map_err(|e| named(dst, e))?;
            }
        }
    }

    // Moves within one drive are instant renames; only the rest needs copy + delete.
    let to_copy: Vec<&(PathBuf, PathBuf)> = match kind {
        Kind::Copy => pairs.iter().collect(),
        Kind::Move => {
            let mut rest = Vec::new();
            for pair in pairs {
                if cancelled() { return Ok(()); }
                p.set_current(&pair.0);
                // Merge a folder into an existing one: rename item by item (same drive).
                let merge = clash == Clash::Merge && pair.0.is_dir() && pair.1.is_dir() && !ops::same_file(&pair.0, &pair.1);
                let moved = if merge { ops::merge_rename(&pair.0, &pair.1) } else { ops::try_rename(&pair.0, &pair.1) };
                match moved {
                    Ok(true) => { p.items_done.fetch_add(1, Ordering::Relaxed); }
                    Ok(false) => rest.push(pair),
                    Err(e) => return Err(named(&pair.0, e)),
                }
            }
            rest
        }
        Kind::Trash | Kind::Delete => {
            p.total.store(pairs.len() as u64, Ordering::Relaxed);
            for (src, _) in pairs {
                if cancelled() { return Ok(()); }
                p.set_current(src);
                ctx.request_repaint();
                let r = if kind == Kind::Trash { trash::delete(src).map_err(std::io::Error::other) } else { ops::remove_path(src) };
                r.map_err(|e| named(src, e))?;
                p.done.fetch_add(1, Ordering::Relaxed);
                p.items_done.fetch_add(1, Ordering::Relaxed);
            }
            return Ok(());
        }
    };
    if to_copy.is_empty() { return Ok(()); }

    // Byte-based progress for the actual copying.
    p.in_bytes.store(true, Ordering::Relaxed);
    p.measuring.store(true, Ordering::Relaxed);
    ctx.request_repaint();
    let total: u64 = to_copy.iter().map(|(s, _)| ops::measure(s)).sum();
    p.total.store(total.max(1), Ordering::Relaxed);
    p.measuring.store(false, Ordering::Relaxed);

    for (src, dst) in to_copy {
        if cancelled() { return Ok(()); }
        let existed = ops::exists(dst);
        match ops::copy_path(src, dst, &mut tick) {
            Err(_) if cancelled() => {
                // Source stays untouched. Remove the partial copy, but only if we created it:
                // an existing target folder (confirmed merge) keeps its previous content.
                if !existed { let _ = ops::remove_path(dst); }
                return Ok(());
            }
            Err(e) => return Err(named(src, e)),
            Ok(()) => {}
        }
        if kind == Kind::Move { ops::remove_path(src).map_err(|e| named(src, e))?; }
        p.items_done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup(name: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("mc-jobs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(t.join("src/a")).unwrap();
        fs::write(t.join("src/a/new.txt"), "new").unwrap();
        fs::write(t.join("src/a/same.txt"), "from src").unwrap();
        fs::create_dir_all(t.join("dst/a")).unwrap();
        fs::write(t.join("dst/a/old.txt"), "old").unwrap();
        fs::write(t.join("dst/a/same.txt"), "from dst").unwrap();
        t
    }

    fn go(kind: Kind, pairs: Vec<(PathBuf, PathBuf)>, clash: Clash) {
        let p = Progress::default();
        run(kind, &pairs, clash, &p, &eframe::egui::Context::default()).unwrap();
    }

    #[test]
    fn replace_merge_and_rename() {
        // Replace: the old folder content is gone.
        let t = setup("replace");
        go(Kind::Copy, vec![(t.join("src/a"), t.join("dst/a"))], Clash::Replace);
        assert!(!t.join("dst/a/old.txt").exists());
        assert_eq!(fs::read_to_string(t.join("dst/a/same.txt")).unwrap(), "from src");
        fs::remove_dir_all(&t).unwrap();

        // Merge (move on the same drive): both sides' files, same names from the source.
        let t = setup("merge");
        go(Kind::Move, vec![(t.join("src/a"), t.join("dst/a"))], Clash::Merge);
        assert!(!t.join("src/a").exists());
        assert_eq!(fs::read_to_string(t.join("dst/a/old.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(t.join("dst/a/new.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(t.join("dst/a/same.txt")).unwrap(), "from src");
        fs::remove_dir_all(&t).unwrap();

        // Rename: the app picks "a (1)" (see App::keep_both); the old folder stays as it was.
        let t = setup("rename");
        let target = ops::unique_target(&t.join("dst/a"), &Default::default());
        assert_eq!(target, t.join("dst/a (1)"));
        go(Kind::Copy, vec![(t.join("src/a"), target)], Clash::Replace);
        assert_eq!(fs::read_to_string(t.join("dst/a/same.txt")).unwrap(), "from dst");
        assert_eq!(fs::read_to_string(t.join("dst/a (1)/same.txt")).unwrap(), "from src");
        fs::remove_dir_all(&t).unwrap();
    }
}
