//! Finder sidebar favourites (macOS).
//!
//! Finder keeps them in `~/Library/Application Support/com.apple.sharedfilelist/
//! com.apple.LSSharedFileList.FavoriteItems.sfl4` (sfl3/sfl2 on older systems): an
//! NSKeyedArchiver plist whose items hold "bookmark" blobs with the folder path.
//! That folder is privacy-protected: it can only be read when the app has Full Disk
//! Access. Without it the sidebar shows the built-in list instead. (Asking `sfltool` for
//! a copy is not an option: macOS shows an administrator password prompt for it.)

use std::io::Cursor;
use std::path::{Path, PathBuf};

use plist::Value;

/// Folder paths from Finder's sidebar "Favorites", in Finder's order.
/// `None` when they can't be read (not macOS, or no Full Disk Access).
pub fn favorite_paths() -> Option<Vec<PathBuf>> {
    if !cfg!(target_os = "macos") { return None; }
    read_from(&crate::ops::home_dir()?.join("Library/Application Support"))
}

fn read_from(app_support: &Path) -> Option<Vec<PathBuf>> {
    let dir = app_support.join("com.apple.sharedfilelist");
    ["sfl4", "sfl3", "sfl2"].iter().find_map(|ext| {
        let bytes = std::fs::read(dir.join(format!("com.apple.LSSharedFileList.FavoriteItems.{ext}"))).ok()?;
        parse_sfl(&bytes)
    })
}

/// Decode the keyed archive and return the paths of all visible items.
pub fn parse_sfl(bytes: &[u8]) -> Option<Vec<PathBuf>> {
    let root = Value::from_reader(Cursor::new(bytes)).ok()?;
    let objects = root.as_dictionary()?.get("$objects")?.as_array()?;
    let deref = |v: &Value| -> Option<Value> {
        match v {
            Value::Uid(uid) => objects.get(uid.get() as usize).cloned(),
            other => Some(other.clone()),
        }
    };
    // An archived NSDictionary: parallel NS.keys / NS.objects arrays of references.
    let entries = |v: &Value| -> Vec<(String, Value)> {
        let Some(d) = v.as_dictionary() else { return Vec::new() };
        let (Some(keys), Some(vals)) = (d.get("NS.keys").and_then(Value::as_array), d.get("NS.objects").and_then(Value::as_array)) else { return Vec::new() };
        keys.iter().zip(vals).filter_map(|(k, v)| Some((deref(k)?.as_string()?.to_string(), deref(v)?))).collect()
    };
    let top = objects.get(1)?;
    let items = entries(top).into_iter().find(|(k, _)| k == "items")?.1;
    let items = items.as_dictionary()?.get("NS.objects")?.as_array()?.clone();

    let mut paths = Vec::new();
    for item in &items {
        let fields = entries(&deref(item)?);
        let get = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        if get("visibility").and_then(|v| v.as_signed_integer()).unwrap_or(0) != 0 { continue; }
        if let Some(path) = get("Bookmark").and_then(|b| b.as_data().and_then(bookmark_path)) {
            paths.push(path);
        }
    }
    Some(paths)
}

/// Extract the path from macOS bookmark data ("book" format). The path is stored under
/// TOC key 0x1004 as an array of UTF-8 path components. Returns `None` for items without
/// a file path (AirDrop, Recents, network locations).
pub fn bookmark_path(b: &[u8]) -> Option<PathBuf> {
    let u32_at = |off: usize| -> Option<usize> { Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?) as usize) };
    if b.get(..4)? != b"book" { return None; }
    let base = u32_at(12)?; // header size: offsets below are relative to it
    let mut toc = base + u32_at(base)?;
    for _ in 0..16 {
        let count = u32_at(toc + 16)?;
        for i in 0..count {
            let entry = toc + 20 + i * 12;
            if u32_at(entry)? != 0x1004 { continue; }
            let rec = base + u32_at(entry + 4)?;
            let n = u32_at(rec)? / 4;
            let mut path = PathBuf::from("/");
            for j in 0..n {
                let s = base + u32_at(rec + 8 + j * 4)?;
                let len = u32_at(s)?;
                path.push(std::str::from_utf8(b.get(s + 8..s + 8 + len)?).ok()?);
            }
            return Some(path);
        }
        let next = u32_at(toc + 12)?;
        if next == 0 { return None; }
        toc = base + next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal bookmark blob holding `parts` under key 0x1004.
    fn bookmark(parts: &[&str]) -> Vec<u8> {
        let base = 48usize;
        let mut data: Vec<u8> = vec![0; 4]; // first u32: offset of the TOC (patched below)
        let mut string_offsets = Vec::new();
        for p in parts {
            string_offsets.push(data.len() as u32);
            data.extend((p.len() as u32).to_le_bytes());
            data.extend(0x0101u32.to_le_bytes());
            data.extend(p.as_bytes());
            while data.len() % 4 != 0 { data.push(0); }
        }
        let array_off = data.len() as u32;
        data.extend(((parts.len() * 4) as u32).to_le_bytes());
        data.extend(0x0601u32.to_le_bytes());
        for o in &string_offsets { data.extend(o.to_le_bytes()); }
        let toc_off = data.len() as u32;
        data[..4].copy_from_slice(&toc_off.to_le_bytes());
        for v in [12u32, 0xffff_fffe, 1, 0, 1, 0x1004, array_off, 0] { data.extend(v.to_le_bytes()); }
        let mut b = b"book".to_vec();
        b.extend(((base + data.len()) as u32).to_le_bytes());
        b.extend(0x1004_0000u32.to_le_bytes());
        b.extend((base as u32).to_le_bytes());
        b.resize(base, 0);
        b.extend(data);
        b
    }

    #[test]
    fn reads_path_from_bookmark() {
        assert_eq!(bookmark_path(&bookmark(&["Users", "me", "Projects"])), Some(PathBuf::from("/Users/me/Projects")));
        assert_eq!(bookmark_path(b"not a bookmark"), None);
        assert_eq!(bookmark_path(&bookmark(&["Users"])[..60]), None, "truncated data must not panic");
    }
}
