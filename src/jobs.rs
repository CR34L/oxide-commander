//! Background copy / move / trash / delete with progress and cancel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::ops;

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

/// `pairs`: (source, target). Target is ignored for Trash/Delete.
pub fn spawn(kind: Kind, pairs: Vec<(PathBuf, PathBuf)>, ctx: eframe::egui::Context) -> Job {
    let progress = Arc::new(Progress::default());
    let job = Job { kind, items: pairs.len(), progress: progress.clone() };
    std::thread::spawn(move || {
        if let Err(e) = run(kind, &pairs, &progress, &ctx) {
            *progress.error.lock().unwrap() = Some(e);
        }
        progress.finished.store(true, Ordering::Relaxed);
        ctx.request_repaint();
    });
    job
}

fn run(kind: Kind, pairs: &[(PathBuf, PathBuf)], p: &Progress, ctx: &eframe::egui::Context) -> Result<(), String> {
    let named = |src: &Path, e: std::io::Error| format!("{}: {e}", src.file_name().unwrap_or_default().to_string_lossy());
    let cancelled = || p.cancel.load(Ordering::Relaxed);
    let mut tick = |path: &Path, bytes: u64| -> std::io::Result<()> {
        if cancelled() { return Err(std::io::Error::other("cancelled")); }
        p.set_current(path);
        p.done.fetch_add(bytes, Ordering::Relaxed);
        ctx.request_repaint();
        Ok(())
    };

    // Moves within one drive are instant renames; only the rest needs copy + delete.
    let to_copy: Vec<&(PathBuf, PathBuf)> = match kind {
        Kind::Copy => pairs.iter().collect(),
        Kind::Move => {
            let mut rest = Vec::new();
            for pair in pairs {
                if cancelled() { return Ok(()); }
                p.set_current(&pair.0);
                match ops::try_rename(&pair.0, &pair.1) {
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
