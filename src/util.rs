//! Small helpers: time, randomness, shell quoting, private files, signals.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------- Time ----------

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's algorithm: days since 1970-01-01 → (year, month, day).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn now_micros() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

fn parts(micros: i64) -> (i64, u32, u32, i64, i64, i64, i64) {
    let secs = micros.div_euclid(1_000_000);
    let frac = micros.rem_euclid(1_000_000);
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    (y, mo, d, sod / 3600, (sod % 3600) / 60, sod % 60, frac)
}

/// UTC timestamp like Python's `datetime.now(timezone.utc).isoformat(timespec="milliseconds")`.
pub fn iso_from_micros(micros: i64) -> String {
    let (y, mo, d, h, mi, s, frac) = parts(micros);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{:03}+00:00", frac / 1000)
}

/// Days since 1970-01-01 for a civil date (inverse of `civil_from_days`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Microseconds since 1970 for a stored timestamp (`2026-10-10T12:23:20.123+00:00`).
pub fn micros_from_iso(iso: &str) -> Option<i64> {
    let n = |a: usize, b: usize| iso.get(a..b)?.parse::<i64>().ok();
    if iso.get(4..5)? != "-" || iso.get(10..11)? != "T" {
        return None;
    }
    let (y, mo, d, h, mi, s) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let ms = if iso.get(19..20) == Some(".") { n(20, 23).unwrap_or(0) } else { 0 };
    Some(((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s) * 1000 + ms) * 1000)
}

/// The local day (`2026-10-11`) of a stored timestamp, `tz_minutes` east of UTC.
pub fn local_day(iso: &str, tz_minutes: i64) -> Option<String> {
    let micros = micros_from_iso(iso)? + tz_minutes * 60_000_000;
    let (y, m, d) = civil_from_days(micros.div_euclid(86_400_000_000));
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

pub fn now_iso() -> String {
    iso_from_micros(now_micros())
}

/// Compact UTC stamp for backup file names, e.g. 20261010T143012123456.
pub fn backup_stamp() -> String {
    let (y, mo, d, h, mi, s, frac) = parts(now_micros());
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}{frac:06}")
}

// ---------- Randomness ----------

pub fn random_bytes(n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf)
}

/// URL-safe base64 without padding (like Python's `secrets.token_urlsafe`).
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// Constant-time comparison for secrets.
pub fn same_secret(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------- Shell quoting (same rules as Python's shlex) ----------

pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

pub fn shell_join(args: &[&str]) -> String {
    args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

/// POSIX-style split. Returns None on an unterminated quote or escape.
pub fn shell_split(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => current.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let next = chars.next()?;
                            if !matches!(next, '\\' | '"' | '$' | '`' | '\n') {
                                current.push('\\');
                            }
                            current.push(next);
                        }
                        c => current.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                current.push(chars.next()?);
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        out.push(current);
    }
    Some(out)
}

// ---------- Paths and private files ----------

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => p.to_path_buf(),
    }
}

/// Absolute, symlink-free path, even when the path does not exist yet
/// (like Python's `Path.resolve()`).
pub fn resolve(p: &Path) -> PathBuf {
    let p = expand_tilde(p);
    let absolute = if p.is_absolute() { p } else { std::env::current_dir().unwrap_or_default().join(p) };
    if let Ok(real) = fs::canonicalize(&absolute) {
        return real;
    }
    let mut existing = absolute.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut out = fs::canonicalize(&existing).unwrap_or(existing);
    for name in rest.iter().rev() {
        out.push(name);
    }
    // Normalise any remaining "." / ".." components lexically.
    let mut clean = PathBuf::new();
    for c in out.components() {
        match c {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    clean
}

pub fn private_dir(p: &Path) -> std::io::Result<()> {
    fs::create_dir_all(p)?;
    fs::set_permissions(p, fs::Permissions::from_mode(0o700))
}

/// Write a file atomically (temporary file + rename) with the given mode.
pub fn atomic_write(path: &Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let suffix = base64url(&random_bytes(9)?);
    let tmp = dir.join(format!(".agentledger-{suffix}"));
    let result = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(content)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

// ---------- Process-level settings ----------

#[cfg(target_os = "macos")]
type ModeT = u16;
#[cfg(not(target_os = "macos"))]
type ModeT = u32;

extern "C" {
    fn umask(mask: ModeT) -> ModeT;
    fn signal(signum: i32, handler: usize) -> usize;
}

/// New files and folders are private to the current user.
pub fn private_umask() {
    unsafe { umask(0o077) };
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: i32) {
    STOP.store(true, Ordering::SeqCst);
}

/// Ctrl+C / SIGTERM set a flag instead of killing the process, so the demo
/// can delete its temporary folder before exiting.
pub fn catch_stop_signals() {
    unsafe {
        signal(2, on_signal as extern "C" fn(i32) as usize); // SIGINT
        signal(15, on_signal as extern "C" fn(i32) as usize); // SIGTERM
    }
}

pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamps() {
        assert_eq!(iso_from_micros(0), "1970-01-01T00:00:00.000+00:00");
        assert_eq!(iso_from_micros(1_791_635_000_123_456), "2026-10-10T12:23:20.123+00:00");
        assert_eq!(iso_from_micros(951_782_400_000_000), "2000-02-29T00:00:00.000+00:00");
        for micros in [0, 951_782_400_000_000, 1_791_635_000_123_000] {
            assert_eq!(micros_from_iso(&iso_from_micros(micros)), Some(micros));
        }
        assert_eq!(micros_from_iso("yesterday"), None);
    }

    #[test]
    fn quotes_like_shlex() {
        assert_eq!(shell_quote("/usr/bin/x"), "/usr/bin/x");
        assert_eq!(shell_quote("/Users/y/Library/Application Support/A"), "'/Users/y/Library/Application Support/A'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        let joined = shell_join(&["/a b/c", "--data-dir", "it's here", "hook"]);
        assert_eq!(shell_split(&joined).unwrap(), vec!["/a b/c", "--data-dir", "it's here", "hook"]);
        assert_eq!(shell_split("a \"b\\\"c\" d\\ e").unwrap(), vec!["a", "b\"c", "d e"]);
        assert!(shell_split("'open").is_none());
    }

    #[test]
    fn encodes_base64url() {
        assert_eq!(base64url(b"\xfb\xff"), "-_8");
        assert_eq!(base64url(&[0u8; 32]).len(), 43);
    }
}
