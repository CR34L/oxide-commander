//! Basic file operations: copy, move/rename, mkdir, delete.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Expand a leading `~` and make `input` absolute relative to `base`.
pub fn resolve_input(input: &str, base: &Path) -> PathBuf {
    let input = input.trim();
    let expanded = if input == "~" || input.starts_with("~/") || input.starts_with("~\\") {
        match home_dir() {
            Some(h) => h.join(input[1..].trim_start_matches(['/', '\\'])),
            None => PathBuf::from(input),
        }
    } else {
        PathBuf::from(input)
    };
    if expanded.is_absolute() { expanded } else { base.join(expanded) }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Work out (source, target) pairs for a copy/move to `dest_input`.
/// Existing directory, trailing separator, or several sources => "into that folder".
/// Otherwise a single source is copied/moved to exactly that path (i.e. rename).
/// A single source whose own name ends the path (the F5/F6 default "<folder>/<name>")
/// always means exactly that path, even when a folder of that name exists there.
pub fn plan_targets(sources: &[PathBuf], dest_input: &str, base: &Path) -> Vec<(PathBuf, PathBuf)> {
    let dest = resolve_input(dest_input, base);
    let trailing_sep = dest_input.trim_end().ends_with('/') || dest_input.trim_end().ends_with('\\');
    let own_name = sources.len() == 1 && !trailing_sep && dest.file_name().is_some() && dest.file_name() == sources[0].file_name();
    let into_dir = !own_name && (dest.is_dir() || sources.len() > 1 || trailing_sep);
    if into_dir {
        sources
            .iter()
            .map(|s| (s.clone(), dest.join(s.file_name().unwrap_or_default())))
            .collect()
    } else {
        sources.iter().map(|s| (s.clone(), dest.clone())).collect()
    }
}

pub fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// True when both paths name the same existing file (e.g. case-only rename on macOS).
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Reject operations that would copy or move a folder into itself.
pub fn check_pair(src: &Path, dst: &Path) -> io::Result<()> {
    let s = fs::canonicalize(src)?;
    let parent = dst.parent().and_then(|p| fs::canonicalize(p).ok());
    if let Some(parent) = parent {
        let d = parent.join(dst.file_name().unwrap_or_default());
        if d != s && d.starts_with(&s) {
            return Err(io::Error::other(format!("cannot put “{}” inside itself", src.display())));
        }
    }
    Ok(())
}

/// Progress callback: `(file being copied, bytes finished since the last call)`.
/// It is called with 0 bytes when a file starts; returning an error aborts (cancel).
pub type Progress<'a> = &'a mut dyn FnMut(&Path, u64) -> io::Result<()>;

/// Total size in bytes of the files below `p` (symlinks are not followed).
pub fn measure(p: &Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| measure(&e.path())).sum())
            .unwrap_or(0),
        Ok(m) if m.is_file() => m.len(),
        _ => 0,
    }
}

pub fn copy_path(src: &Path, dst: &Path, progress: Progress) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        progress(src, 0)?;
        if exists(dst) { remove_path(dst)?; }
        return copy_symlink(src, dst);
    }
    if meta.is_dir() {
        if exists(dst) && !dst.is_dir() { remove_path(dst)?; }
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_path(&entry.path(), &dst.join(entry.file_name()), progress)?;
        }
        Ok(())
    } else {
        progress(src, 0)?;
        if dst.is_dir() { remove_path(dst)?; }
        copy_file(src, dst, meta.len(), progress)
    }
}

/// Files above this size report progress while they are being copied.
const BIG_FILE: u64 = 8 * 1024 * 1024;

/// Copy one file with the OS copy routine (keeps permissions, dates, tags; clones on
/// APFS). For big files the copy runs on a helper thread while we watch the target grow,
/// so the progress bar moves during the file and Cancel takes effect right away.
fn copy_file(src: &Path, dst: &Path, size: u64, progress: Progress) -> io::Result<()> {
    // On an error (including cancel) the target file is never left behind.
    let finish = |progress: Progress, bytes: u64| progress(src, bytes).inspect_err(|_| { let _ = fs::remove_file(dst); });
    if size < BIG_FILE {
        fs::copy(src, dst)?;
        return finish(progress, size);
    }
    let (s, d) = (src.to_path_buf(), dst.to_path_buf());
    // (copy finished, cancelled). Whichever side comes second removes the target file,
    // so a cancelled copy never leaves a file behind, however the two race.
    let state = std::sync::Arc::new(std::sync::Mutex::new((false, false)));
    let state2 = state.clone();
    let worker = std::thread::spawn(move || {
        let r = fs::copy(&s, &d).map(|_| ());
        let mut st = state2.lock().unwrap();
        st.0 = true;
        if st.1 { let _ = fs::remove_file(&d); }
        r
    });
    let mut reported = 0u64;
    while !worker.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let now = fs::metadata(dst).map(|m| m.len()).unwrap_or(reported).min(size);
        if now > reported {
            if let Err(e) = progress(src, now - reported) {
                let mut st = state.lock().unwrap();
                st.1 = true;
                if st.0 { let _ = fs::remove_file(dst); }
                return Err(e);
            }
            reported = now;
        }
    }
    worker.join().map_err(|_| io::Error::other("copy thread panicked"))??;
    finish(progress, size.saturating_sub(reported))
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(src)?, dst)
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    // Windows symlinks need privileges; copy what the link points to instead.
    if src.is_dir() { copy_path(&fs::canonicalize(src)?, dst, &mut |_, _| Ok(())) } else { fs::copy(src, dst).map(|_| ()) }
}

/// Move by renaming, which is instant on the same drive. Replaces an existing target.
/// `Ok(false)` means renaming is not possible (usually another drive): copy + delete instead.
pub fn try_rename(src: &Path, dst: &Path) -> io::Result<bool> {
    if same_file(src, dst) {
        // Case-only rename, or a no-op: never delete the "existing" target here.
        return fs::rename(src, dst).map(|_| true);
    }
    if exists(dst) { remove_path(dst)?; }
    Ok(fs::rename(src, dst).is_ok())
}

/// What to do when a copy/move target already exists.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Clash {
    /// Remove the existing item first: afterwards the target is exactly the source.
    Replace,
    /// Folders: combine the contents (same-named files inside are overwritten).
    Merge,
}

/// Merge-move folder `src` into existing folder `dst` by renaming, which is instant on
/// the same drive. `Ok(false)`: not on the same drive (nothing changed): copy instead.
pub fn merge_rename(src: &Path, dst: &Path) -> io::Result<bool> {
    if !same_device(src, dst) { return Ok(false); }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let both_dirs = entry.file_type()?.is_dir() && fs::symlink_metadata(&to).is_ok_and(|m| m.is_dir());
        if both_dirs {
            merge_rename(&from, &to)?;
        } else {
            if exists(&to) { remove_path(&to)?; }
            fs::rename(&from, &to)?;
        }
    }
    fs::remove_dir(src)
        .map(|_| true)
}

#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    matches!((fs::symlink_metadata(a), fs::metadata(b)), (Ok(x), Ok(y)) if x.dev() == y.dev())
}

#[cfg(not(unix))]
fn same_device(_: &Path, _: &Path) -> bool { false }

/// First free "name (1).ext", "name (2).ext", … next to `dst` (folders: "name (1)"),
/// also avoiding names in `taken` (targets planned in the same operation).
pub fn unique_target(dst: &Path, taken: &std::collections::HashSet<PathBuf>) -> PathBuf {
    let name = dst.file_name().unwrap_or_default().to_string_lossy().into_owned();
    // "archive.tar.gz" keeps ".gz" only; ".zshrc" has no extension.
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !dst.is_dir() => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.clone(), String::new()),
    };
    (1..).map(|n| dst.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| !exists(p) && !taken.contains(p))
        .unwrap()
}

pub fn move_path(src: &Path, dst: &Path, progress: Progress) -> io::Result<()> {
    if try_rename(src, dst)? { return Ok(()); }
    copy_path(src, dst, progress)?;
    remove_path(src)
}

pub fn remove_path(p: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(p)?;
    if meta.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 { v /= 1000.0; u += 1; }
    if u == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// Open a file with the operating system's default application.
pub fn open_with_system(path: &Path) -> io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(path).spawn().map(|_| ())
}

/// Open a file in a plain text editor.
pub fn open_in_editor(path: &Path) -> io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-t");
        c
    } else if cfg!(windows) {
        std::process::Command::new("notepad")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(path).spawn().map(|_| ())
}

/// Terminal apps offered in Settings on macOS (only installed ones are listed).
pub const MAC_TERMINALS: [&str; 7] = ["Terminal", "iTerm", "Warp", "Ghostty", "WezTerm", "Alacritty", "kitty"];

pub fn installed_terminals() -> Vec<&'static str> {
    if !cfg!(target_os = "macos") { return Vec::new(); }
    MAC_TERMINALS.into_iter().filter(|name| {
        *name == "Terminal"
            || Path::new(&format!("/Applications/{name}.app")).exists()
            || home_dir().is_some_and(|h| h.join(format!("Applications/{name}.app")).exists())
    }).collect()
}

/// Open a terminal window in `dir`. `mac_app` names the macOS terminal app.
pub fn open_terminal(dir: &Path, mac_app: &str) -> io::Result<()> {
    use std::process::Command;
    if cfg!(target_os = "macos") {
        // `open -a <app> <folder>` starts a new window in that folder for all common terminals.
        let app = if mac_app.trim().is_empty() { "Terminal" } else { mac_app.trim() };
        return Command::new("open").arg("-a").arg(app).arg(dir).spawn().map(|_| ());
    }
    if cfg!(windows) {
        // Windows Terminal when installed, otherwise a classic command prompt.
        if Command::new("wt").arg("-d").arg(dir).spawn().is_ok() { return Ok(()); }
        return Command::new("cmd").args(["/C", "start", "cmd", "/K"]).current_dir(dir).spawn().map(|_| ());
    }
    // Linux / BSD: the first terminal that starts. They open in the current directory.
    let candidates = ["x-terminal-emulator", "gnome-terminal", "konsole", "xfce4-terminal", "kitty", "alacritty", "wezterm", "xterm"];
    for term in candidates {
        if Command::new(term).current_dir(dir).spawn().is_ok() { return Ok(()); }
    }
    Err(io::Error::other("no terminal program found"))
}

/// Quick Look preview on macOS; system default app elsewhere.
pub fn preview(path: &Path) -> io::Result<()> {
    if cfg!(target_os = "macos") {
        std::process::Command::new("qlmanage")
            .arg("-p").arg(path)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().map(|_| ())
    } else {
        open_with_system(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mc-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn copy_move_delete_tree() {
        let t = tmp("tree");
        fs::create_dir_all(t.join("a/sub")).unwrap();
        fs::write(t.join("a/sub/f.txt"), "hello").unwrap();
        fs::create_dir(t.join("dst")).unwrap();

        let pairs = plan_targets(&[t.join("a")], t.join("dst").to_str().unwrap(), &t);
        assert_eq!(pairs[0].1, t.join("dst/a"));
        copy_path(&pairs[0].0, &pairs[0].1, &mut |_, _| Ok(())).unwrap();
        assert_eq!(fs::read_to_string(t.join("dst/a/sub/f.txt")).unwrap(), "hello");

        // rename via a relative name
        let pairs = plan_targets(&[t.join("a")], "b", &t);
        move_path(&pairs[0].0, &pairs[0].1, &mut |_, _| Ok(())).unwrap();
        assert!(!t.join("a").exists() && t.join("b/sub/f.txt").exists());

        remove_path(&t.join("b")).unwrap();
        assert!(!t.join("b").exists());

        // "<folder>/<own name>" is that exact path even when such a folder exists there;
        // a trailing separator still means "into".
        fs::create_dir_all(t.join("dst/c")).unwrap();
        fs::create_dir_all(t.join("c")).unwrap();
        let exact = plan_targets(&[t.join("c")], t.join("dst/c").to_str().unwrap(), &t);
        assert_eq!(exact[0].1, t.join("dst/c"));
        let into = plan_targets(&[t.join("c")], &format!("{}/", t.join("dst/c").display()), &t);
        assert_eq!(into[0].1, t.join("dst/c/c"));
        let renamed = plan_targets(&[t.join("c")], t.join("dst/c-old").to_str().unwrap(), &t);
        assert_eq!(renamed[0].1, t.join("dst/c-old"));
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn big_file_reports_bytes_and_cancel_cleans_up() {
        let t = tmp("big");
        let size = 3 * BIG_FILE + 123;
        fs::create_dir(t.join("src")).unwrap();
        fs::write(t.join("src/big.bin"), vec![7u8; size as usize]).unwrap();
        fs::write(t.join("src/small.txt"), "abc").unwrap();
        assert_eq!(measure(&t.join("src")), size + 3);

        let mut reported = 0u64;
        copy_path(&t.join("src"), &t.join("dst"), &mut |_, b| { reported += b; Ok(()) }).unwrap();
        assert_eq!(reported, size + 3, "progress adds up to exactly the copied bytes");
        assert_eq!(fs::metadata(t.join("dst/big.bin")).unwrap().len(), size);

        // Cancel as soon as the big file starts: error, and no half-written file left behind.
        let r = copy_file(&t.join("src/big.bin"), &t.join("partial.bin"), size, &mut |_, _| Err(io::Error::other("cancelled")));
        assert!(r.is_err());
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!t.join("partial.bin").exists());
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn clash_helpers() {
        let t = tmp("clash");
        fs::write(t.join("photo.jpg"), "a").unwrap();
        fs::write(t.join("photo (1).jpg"), "b").unwrap();
        fs::create_dir(t.join("Folder")).unwrap();
        let none = std::collections::HashSet::new();
        assert_eq!(unique_target(&t.join("photo.jpg"), &none), t.join("photo (2).jpg"));
        assert_eq!(unique_target(&t.join("Folder"), &none), t.join("Folder (1)"));
        let taken = [t.join("Folder (1)")].into_iter().collect();
        assert_eq!(unique_target(&t.join("Folder"), &taken), t.join("Folder (2)"));

        // Merge by renaming: both sides' files survive, same names are overwritten.
        fs::create_dir_all(t.join("src/sub")).unwrap();
        fs::write(t.join("src/new.txt"), "new").unwrap();
        fs::write(t.join("src/same.txt"), "from src").unwrap();
        fs::write(t.join("src/sub/deep.txt"), "deep").unwrap();
        fs::create_dir_all(t.join("dst/sub")).unwrap();
        fs::write(t.join("dst/old.txt"), "old").unwrap();
        fs::write(t.join("dst/same.txt"), "from dst").unwrap();
        fs::write(t.join("dst/sub/keep.txt"), "keep").unwrap();
        assert!(merge_rename(&t.join("src"), &t.join("dst")).unwrap());
        assert!(!t.join("src").exists(), "source is gone after the move");
        for (f, want) in [("new.txt", "new"), ("old.txt", "old"), ("same.txt", "from src"), ("sub/deep.txt", "deep"), ("sub/keep.txt", "keep")] {
            assert_eq!(fs::read_to_string(t.join("dst").join(f)).unwrap(), want, "{f}");
        }
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn refuses_copy_into_itself() {
        let t = tmp("self");
        fs::create_dir(t.join("a")).unwrap();
        assert!(check_pair(&t.join("a"), &t.join("a/a")).is_err());
        assert!(check_pair(&t.join("a"), &t.join("b")).is_ok());
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn case_only_rename_keeps_file() {
        let t = tmp("case");
        fs::write(t.join("x.txt"), "data").unwrap();
        move_path(&t.join("x.txt"), &t.join("X.txt"), &mut |_, _| Ok(())).unwrap();
        assert_eq!(fs::read_to_string(t.join("X.txt")).unwrap(), "data");
        fs::remove_dir_all(&t).unwrap();
    }
}
