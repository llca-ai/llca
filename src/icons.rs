//! Application icons for the dashboard (macOS only).
//!
//! The dashboard shows which applications an agent opened or ran. Their icon
//! is taken from the app bundle and converted to PNG with tools that ship
//! with macOS (`plutil`, `sips`, and `qlmanage` for apps that only have an
//! asset catalog). Results, including misses, are cached in the data folder.
//! Only an application *name* is accepted, looked up in the standard
//! Applications folders; programs run with arguments, never through a shell.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::util;

/// A bare application name: no path, no hidden file, reasonable length.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 120
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0', '\n', '\r'])
}

fn bundle(name: &str) -> Option<PathBuf> {
    let file = format!("{name}.app");
    let home = util::home();
    [
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
        home.join("Applications"),
    ]
    .into_iter()
    .map(|dir| dir.join(&file))
    .find(|p| p.is_dir())
}

fn quiet(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// FNV-1a, enough to name cache files without putting app names on disk paths.
fn hash(name: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn convert(app: &Path, out: &Path, work: &Path) -> bool {
    // 1. The .icns named in Info.plist (CFBundleIconFile), resized by sips.
    let plist = app.join("Contents/Info.plist");
    if let Ok(o) = Command::new("plutil").args(["-extract", "CFBundleIconFile", "raw", "-o", "-"]).arg(&plist).stderr(Stdio::null()).output() {
        let file = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && !file.is_empty() && !file.contains('/') {
            let file = if file.ends_with(".icns") { file } else { format!("{file}.icns") };
            let icns = app.join("Contents/Resources").join(file);
            if icns.is_file()
                && quiet(Command::new("sips").args(["-s", "format", "png", "-Z", "64"]).arg(&icns).arg("--out").arg(out))
                && out.is_file()
            {
                return true;
            }
        }
    }
    // 2. Quick Look thumbnail of the bundle, which renders asset-catalog icons too.
    let _ = fs::create_dir_all(work);
    if quiet(Command::new("qlmanage").args(["-t", "-s", "64", "-o"]).arg(work).arg(app)) {
        if let Some(name) = app.file_name() {
            let thumb = work.join(format!("{}.png", name.to_string_lossy()));
            if thumb.is_file() && fs::rename(&thumb, out).is_ok() {
                return true;
            }
        }
    }
    false
}

/// PNG bytes for an application's icon, or None (not macOS, not installed,
/// or no usable icon).
pub fn png(data_dir: &Path, name: &str) -> Option<Vec<u8>> {
    if !cfg!(target_os = "macos") || !valid_name(name) {
        return None;
    }
    let cache = data_dir.join("icons");
    let key = hash(name);
    let file = cache.join(format!("{key}.png"));
    let miss = cache.join(format!("{key}.none"));
    if let Ok(bytes) = fs::read(&file) {
        return Some(bytes);
    }
    // A miss is retried after a day, in case the app is installed later.
    let recent_miss = fs::metadata(&miss)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().map(|e| e.as_secs() < 86_400).unwrap_or(true));
    if recent_miss {
        return None;
    }
    util::private_dir(&cache).ok()?;
    let app = bundle(name);
    let work = cache.join(format!("{key}.tmp"));
    let ok = app.as_deref().is_some_and(|app| convert(app, &file, &work));
    let _ = fs::remove_dir_all(&work);
    if ok {
        fs::read(&file).ok()
    } else {
        let _ = fs::write(&miss, b"");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_bare_names() {
        assert!(valid_name("Arduino IDE"));
        assert!(valid_name("Visual Studio Code"));
        for bad in ["", "../etc", "/Applications/X", ".hidden", "a\0b", "x\ny"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }
}
