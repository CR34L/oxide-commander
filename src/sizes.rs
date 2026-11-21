//! Background calculation of folder sizes, shared by both panels.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

const WORKERS: usize = 4;

#[derive(Clone)]
pub struct DirSizes {
    inner: Arc<Inner>,
}

struct Inner {
    cache: Mutex<HashMap<PathBuf, u64>>,
    /// Bumped on every new result, so panels know when to re-sort.
    version: AtomicU64,
    ctx: Option<eframe::egui::Context>,
}

/// One panel's scan state: bumping `generation` cancels the running scan.
#[derive(Default)]
pub struct ScanHandle {
    generation: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
}

impl ScanHandle {
    pub fn is_running(&self) -> bool { self.pending.load(Ordering::Relaxed) > 0 }

    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.pending.store(0, Ordering::Relaxed);
    }
}

impl DirSizes {
    pub fn new(ctx: Option<eframe::egui::Context>) -> Self {
        DirSizes { inner: Arc::new(Inner { cache: Mutex::default(), version: AtomicU64::new(0), ctx }) }
    }

    pub fn get(&self, p: &Path) -> Option<u64> { self.cache().get(p).copied() }

    pub fn cache(&self) -> MutexGuard<'_, HashMap<PathBuf, u64>> { self.inner.cache.lock().unwrap() }

    pub fn version(&self) -> u64 { self.inner.version.load(Ordering::Relaxed) }

    /// Measure `dirs` on worker threads. Cancels whatever `handle` was scanning before.
    pub fn scan(&self, dirs: Vec<PathBuf>, handle: &ScanHandle) {
        handle.cancel();
        if dirs.is_empty() { return; }
        let my_gen = handle.generation.load(Ordering::Relaxed);
        handle.pending.store(dirs.len(), Ordering::Relaxed);
        let queue = Arc::new(Mutex::new(VecDeque::from(dirs)));
        for _ in 0..WORKERS.min(queue.lock().unwrap().len()) {
            let (queue, me) = (queue.clone(), self.clone());
            let (generation, pending) = (handle.generation.clone(), handle.pending.clone());
            std::thread::spawn(move || {
                let cancelled = || generation.load(Ordering::Relaxed) != my_gen;
                loop {
                    let Some(dir) = queue.lock().unwrap().pop_front() else { break };
                    let Some(size) = dir_size(&dir, &cancelled) else { break };
                    if cancelled() { break; }
                    me.cache().insert(dir, size);
                    me.inner.version.fetch_add(1, Ordering::Relaxed);
                    pending.fetch_sub(1, Ordering::Relaxed);
                    if let Some(ctx) = &me.inner.ctx { ctx.request_repaint(); }
                }
            });
        }
    }
}

/// Total size of all files below `root`. Does not follow symlinks or cross into other
/// file systems (so scanning "/" does not walk network shares). `None` when cancelled.
pub fn dir_size(root: &Path, cancelled: &dyn Fn() -> bool) -> Option<u64> {
    let root_dev = device(root);
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            seen += 1;
            if seen.is_multiple_of(512) && cancelled() { return None; }
            let Ok(meta) = entry.metadata() else { continue }; // does not follow symlinks
            if meta.is_dir() {
                let p = entry.path();
                if root_dev.is_none() || device(&p) == root_dev { stack.push(p); }
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    Some(total)
}

#[cfg(unix)]
fn device(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(p).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device(_p: &Path) -> Option<u64> { None }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_nested_files_and_scans_in_background() {
        let t = std::env::temp_dir().join(format!("mc-sizes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(t.join("a/b/c")).unwrap();
        fs::write(t.join("a/one"), vec![0u8; 1000]).unwrap();
        fs::write(t.join("a/b/c/two"), vec![0u8; 234]).unwrap();
        assert_eq!(dir_size(&t.join("a"), &|| false), Some(1234));
        assert_eq!(dir_size(&t.join("a"), &|| true), Some(1234), "small trees finish before the first cancel check");

        let sizes = DirSizes::new(None);
        let handle = ScanHandle::default();
        sizes.scan(vec![t.join("a"), t.join("a/b")], &handle);
        let start = std::time::Instant::now();
        while handle.is_running() && start.elapsed().as_secs() < 5 { std::thread::sleep(std::time::Duration::from_millis(5)); }
        assert_eq!(sizes.get(&t.join("a")), Some(1234));
        assert_eq!(sizes.get(&t.join("a/b")), Some(234));
        fs::remove_dir_all(&t).unwrap();
    }
}
