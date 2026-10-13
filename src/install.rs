//! Adding and removing llca hooks in Codex and Claude Code settings.
//!
//! Codex: hooks.json in ~/.codex or <project>/.codex.
//! Claude Code: the "hooks" key of settings.json (~/.claude) or
//! settings.local.json (<project>/.claude, not committed).
//! Every other key and every third-party hook is preserved, a backup is made
//! before a file changes, and running the same command twice changes nothing.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use crate::json::{self, Map, Value};
use crate::obj;
use crate::store::agent_events;
use crate::util;

pub const ADAPTERS: [(&str, &str); 2] = [("codex-v0", "codex"), ("claude-code-v0", "claude-code")];

pub fn adapter_for(agent: &str) -> Option<&'static str> {
    ADAPTERS.iter().find(|(_, a)| *a == agent).map(|(ad, _)| *ad)
}

pub fn agent_for(adapter: &str) -> Option<&'static str> {
    ADAPTERS.iter().find(|(ad, _)| *ad == adapter).map(|(_, a)| *a)
}

fn default_file(agent: &str) -> &'static str {
    if agent == "codex" {
        "hooks.json"
    } else {
        "settings.json"
    }
}

fn timeout(agent: &str) -> i64 {
    if agent == "codex" {
        3
    } else {
        5
    }
}

fn note(agent: &str) -> &'static str {
    if agent == "codex" {
        "Registration does not prove activation. Trust the hooks in Codex /hooks, then start a new local session."
    } else {
        "Claude Code reloads its settings automatically; use /hooks to check. If in doubt, start a new session."
    }
}

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}
impl From<json::ParseError> for Error {
    fn from(e: json::ParseError) -> Self {
        Error(e.to_string())
    }
}

/// The configuration file for an agent: an explicit .json path, or the
/// agent's default file inside a configuration directory.
pub fn config_file(target: &Path, agent: &str) -> PathBuf {
    if target.extension().is_some_and(|e| e == "json") {
        target.to_path_buf()
    } else {
        target.join(default_file(agent))
    }
}

/// Where `init` copies the collector, so that hooks never depend on the
/// location of the installed package (Homebrew upgrades, moved folders…).
pub fn collector_path(data_dir: &Path) -> PathBuf {
    util::resolve(data_dir).join("bin").join("llca")
}

pub fn hook_command(data_dir: &Path, agent: &str) -> String {
    let data_dir = util::resolve(data_dir);
    let binary = collector_path(&data_dir);
    util::shell_join(&[
        &binary.to_string_lossy(),
        "--data-dir",
        &data_dir.to_string_lossy(),
        "hook",
        "--adapter",
        adapter_for(agent).unwrap_or("codex-v0"),
    ])
}

/// Recognises llca handlers, so that upgrading replaces them instead of duplicating.
pub fn is_ours(handler: &Value) -> bool {
    let Some(h) = handler.as_object() else { return false };
    if h.get("type").and_then(Value::as_str) != Some("command") {
        return false;
    }
    let Some(argv) = h.get("command").and_then(Value::as_str).and_then(util::shell_split) else { return false };
    let name = |s: &str| Path::new(s).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let adapter_ok = |s: &str| agent_for(s).is_some();
    // <bin>/llca --data-dir DIR hook --adapter X
    argv.len() == 6
        && name(&argv[0]) == "llca"
        && argv[1] == "--data-dir"
        && argv[3] == "hook"
        && argv[4] == "--adapter"
        && adapter_ok(&argv[5])
}

fn remove_ours(config: &mut Map) -> Result<(), Error> {
    if !config.contains_key("hooks") {
        config.insert("hooks", Value::Object(Map::new()));
    }
    let Some(Value::Object(hooks)) = config.get_mut("hooks") else {
        return Err(Error("hooks must be a JSON object".into()));
    };
    let events: Vec<String> = hooks.iter().map(|(k, _)| k.clone()).collect();
    for event in events {
        let Some(Value::Array(groups)) = hooks.get(&event) else {
            return Err(Error(format!("Invalid hook list: {event}")));
        };
        let mut cleaned = Vec::new();
        for group in groups {
            let handlers = group.get("hooks").and_then(Value::as_array);
            let (Some(g), Some(handlers)) = (group.as_object(), handlers) else {
                return Err(Error(format!("Invalid hook group: {event}")));
            };
            let kept: Vec<Value> = handlers.iter().filter(|h| !is_ours(h)).cloned().collect();
            if kept.len() == handlers.len() {
                cleaned.push(group.clone());
            } else if !kept.is_empty() {
                let mut g = g.clone();
                g.insert("hooks", Value::Array(kept));
                cleaned.push(Value::Object(g));
            }
        }
        if cleaned.is_empty() {
            hooks.remove(&event);
        } else {
            hooks.insert(event, Value::Array(cleaned));
        }
    }
    Ok(())
}

// ---------- Collector version ----------

/// Command that refreshes the collector copy (and changes nothing else).
pub const UPDATE_COMMAND: &str = "llca init --global";

/// `x.y.z` as numbers, for comparing versions.
fn version_parts(v: &str) -> Vec<u64> {
    v.trim().split('.').map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)).collect()
}

/// True when version `a` is older than version `b`.
pub fn older(a: &str, b: &str) -> bool {
    version_parts(a) < version_parts(b)
}

/// The version printed by `<binary> --version`, waiting two seconds at most.
fn ask_version(binary: &Path) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new(binary).arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    let version = out.trim().to_string();
    (!version.is_empty() && version.len() < 40 && version.chars().all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c)))
        .then_some(version)
}

/// Version of the collector copy the hooks run. The answer is kept until the
/// file changes, so the dashboard can ask on every refresh.
pub fn collector_version(data_dir: &Path) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    // The file's path, size and modification time, and the version it gave.
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    type Cache = Mutex<HashMap<PathBuf, (Key, Option<String>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let path = collector_path(data_dir);
    let meta = fs::metadata(&path).ok()?;
    let key: Key = (path.clone(), meta.len(), meta.modified().ok());
    let cache = CACHE.get_or_init(Default::default);
    if let Some((k, v)) = cache.lock().ok()?.get(&path) {
        if *k == key {
            return v.clone();
        }
    }
    let version = ask_version(&path);
    if let Ok(mut c) = cache.lock() {
        c.insert(path, (key, version.clone()));
    }
    version
}

/// What `doctor` and the dashboard say about the collector copy: its
/// version, this binary's version, and whether the hooks run an older one
/// (older redaction rules among other things). A copy that cannot report its
/// version counts as outdated.
pub fn collector_state(data_dir: &Path) -> Value {
    let running = crate::cli::VERSION;
    let path = collector_path(data_dir);
    let exists = path.exists();
    let version = if exists { collector_version(data_dir) } else { None };
    let outdated = exists && version.as_deref().is_none_or(|v| older(v, running));
    obj![
        ("path", path.to_string_lossy().into_owned()),
        ("exists", exists),
        ("version", version),
        ("running", running),
        ("outdated", outdated),
        ("fix", if outdated { Value::str(UPDATE_COMMAND) } else { Value::Null }),
    ]
}

/// Copy the running binary next to the journal (atomic, executable by the user only).
fn install_collector(data_dir: &Path) -> Result<(), Error> {
    let destination = collector_path(data_dir);
    // LLCA_BIN lets tests (and packagers) name the binary to install.
    let current = match std::env::var_os("LLCA_BIN") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe()?,
    };
    if util::resolve(&current) == destination {
        return Ok(());
    }
    let bytes = fs::read(&current)?;
    util::private_dir(destination.parent().expect("bin directory"))?;
    util::atomic_write(&destination, &bytes, 0o700)?;
    Ok(())
}

fn read_config(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn parse_config(bytes: &[u8], name: &str) -> Result<Map, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error(format!("{name} is not valid UTF-8")))?;
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match json::parse(text)? {
        Value::Object(m) => Ok(m),
        _ => Err(Error(format!("{name} must contain a JSON object"))),
    }
}

/// Add (or remove) llca hooks for one agent. Returns a short report.
pub fn configure(target: &Path, data_dir: &Path, uninstall: bool, agent: &str) -> Result<Value, Error> {
    let events = agent_events(agent).ok_or_else(|| Error(format!("Unsupported agent: {agent}")))?;
    let file = config_file(target, agent);
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let data_dir = util::resolve(data_dir);
    let unchanged = |path: &Path| {
        obj![
            ("agent", agent),
            ("path", path.to_string_lossy().into_owned()),
            ("changed", false),
            ("backup", Value::Null),
            ("uninstalled", uninstall),
        ]
    };
    // Removing hooks from a configuration that does not exist: nothing to do,
    // and no folder is created for an agent that is not installed.
    if uninstall && !file.parent().is_some_and(Path::exists) {
        forget_installation(&data_dir, &file)?;
        return Ok(unchanged(&file));
    }
    // Resolve the directory only: resolving the file would follow a symlink.
    let path = util::resolve(file.parent().unwrap_or(Path::new("."))).join(&name);
    fs::create_dir_all(path.parent().expect("parent"))?;
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Error(format!("{name} is a symbolic link; configure it manually")));
    }
    // The lock lives in llca's data folder, so nothing is left next to the
    // agent's settings. Versions before 0.7.7 put it there: remove it.
    util::private_dir(&data_dir)?;
    let lock = File::options().create(true).truncate(false).write(true).open(data_dir.join("install.lock"))?;
    lock.lock()?;
    let _ = fs::remove_file(path.with_file_name(".llca-install.lock"));

    let before = read_config(&path)?;
    let mut config = match &before {
        Some(b) if !String::from_utf8_lossy(b).trim().is_empty() => parse_config(b, &name)?,
        _ => Map::new(),
    };
    let original = config.clone();
    let had_hooks = config.contains_key("hooks");
    remove_ours(&mut config)?;
    if !uninstall {
        install_collector(&data_dir)?;
        let command = hook_command(&data_dir, agent);
        let Some(Value::Object(hooks)) = config.get_mut("hooks") else { unreachable!() };
        for event in events {
            let handler = obj![("type", "command"), ("command", command.clone()), ("timeout", Value::int(timeout(agent)))];
            let group = obj![("hooks", Value::Array(vec![handler]))];
            match hooks.get_mut(event) {
                Some(Value::Array(groups)) => groups.push(group),
                _ => hooks.insert(*event, Value::Array(vec![group])),
            }
        }
    } else if config.get("hooks").and_then(Value::as_object).is_some_and(Map::is_empty) && (agent == "claude-code" || !had_hooks) {
        // settings.json holds other settings: do not leave an empty hooks block behind.
        config.remove("hooks");
    }
    // Compare contents, not text: a file that already says the same thing is
    // left exactly as it is (indentation, spacing and line endings included).
    let changed = original != config || (before.is_none() && !uninstall);
    let mut backup = Value::Null;
    if changed {
        if let Some(b) = before.as_ref().filter(|b| !b.is_empty()) {
            let backup_path = PathBuf::from(format!("{}.llca-backup-{}", path.display(), util::backup_stamp()));
            util::atomic_write(&backup_path, b, 0o600)?;
            backup = Value::str(backup_path.to_string_lossy());
        }
        if before.is_some() || !uninstall {
            // Keep the file's own indentation (2 spaces when it has none yet).
            let width = before.as_deref().and_then(indent_width).unwrap_or(2);
            let after = format!("{}\n", Value::Object(config).to_pretty_indent(width)).into_bytes();
            util::atomic_write(&path, &after, 0o600)?;
        }
    }
    if uninstall {
        forget_installation(&data_dir, &path)?;
    } else {
        record_installation(&data_dir, agent, &path, &hook_command(&data_dir, agent))?;
    }
    Ok(obj![
        ("agent", agent),
        ("path", path.to_string_lossy().into_owned()),
        ("changed", changed),
        ("backup", backup),
        ("uninstalled", uninstall),
    ])
}

/// The indentation of a JSON file: the spaces before its first indented line.
fn indent_width(text: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(text).ok()?;
    text.lines().skip(1).find(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start_matches(' ').len()).filter(|w| (1..=8).contains(w))
}

const INSTALLATIONS: &str = "installation.json";

/// Where this data folder's collector is used: configuration file → agent and
/// hook command. Read by `uninstall`, to remove the collector copy once no
/// configuration uses it any more.
fn installations(data_dir: &Path) -> Map {
    let parsed = fs::read_to_string(data_dir.join(INSTALLATIONS)).ok().and_then(|t| json::parse(&t).ok());
    let Some(Value::Object(map)) = parsed else { return Map::new() };
    let mut out = Map::new();
    for (key, entry) in map.iter() {
        // Before 0.7.7 the key was the agent, with the file in "config_path".
        match entry.get("config_path").and_then(Value::as_str) {
            Some(file) if ADAPTERS.iter().any(|(_, a)| a == key) => {
                out.insert(file, obj![("agent", key.as_str()), ("command", entry.get("command").cloned().unwrap_or(Value::Null))]);
            }
            _ if entry.get("agent").is_some() => out.insert(key.as_str(), entry.clone()),
            _ => {}
        }
    }
    out
}

fn save_installations(data_dir: &Path, map: Map) -> Result<(), Error> {
    let file = data_dir.join(INSTALLATIONS);
    if map.is_empty() {
        match fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => return Ok(()),
        }
    }
    util::atomic_write(&file, Value::Object(map).to_pretty().as_bytes(), 0o600)?;
    Ok(())
}

fn record_installation(data_dir: &Path, agent: &str, path: &Path, command: &str) -> Result<(), Error> {
    let mut map = installations(data_dir);
    map.insert(path.to_string_lossy(), obj![("agent", agent), ("command", command)]);
    save_installations(data_dir, map)
}

fn forget_installation(data_dir: &Path, path: &Path) -> Result<(), Error> {
    if !data_dir.join(INSTALLATIONS).exists() {
        return Ok(());
    }
    let mut map = installations(data_dir);
    map.remove(&path.to_string_lossy());
    save_installations(data_dir, map)
}

/// True when a configuration file still runs this data folder's collector.
fn uses_collector(path: &Path, data_dir: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(config) = read_config(path).ok().flatten().and_then(|b| parse_config(&b, &name).ok()) else { return false };
    let commands: Vec<String> = ADAPTERS.iter().map(|(_, agent)| hook_command(data_dir, agent)).collect();
    let Some(hooks) = config.get("hooks").and_then(Value::as_object) else { return false };
    let handlers =
        hooks.iter().filter_map(|(_, groups)| groups.as_array()).flatten().filter_map(|g| g.get("hooks").and_then(Value::as_array));
    let used =
        handlers.flatten().any(|h| is_ours(h) && h.get("command").and_then(Value::as_str).is_some_and(|c| commands.iter().any(|x| x == c)));
    used
}

/// After `uninstall`: remove the collector copy (bin/llca) when no
/// configuration runs it any more — neither one recorded by `init`, nor one of
/// `also` (the agents' default configurations). The journal is kept.
/// Returns the removed path.
pub fn remove_collector_if_unused(data_dir: &Path, also: &[(PathBuf, &str)]) -> Result<Option<PathBuf>, Error> {
    let data_dir = util::resolve(data_dir);
    let binary = collector_path(&data_dir);
    if !binary.exists() {
        return Ok(None);
    }
    let mut map = installations(&data_dir);
    let recorded: Vec<String> = map.iter().map(|(k, _)| k.clone()).collect();
    let mut used = false;
    for file in recorded {
        if uses_collector(Path::new(&file), &data_dir) {
            used = true;
        } else {
            map.remove(&file); // a hook removed by hand, or a deleted project
        }
    }
    save_installations(&data_dir, map)?;
    used |= also.iter().any(|(target, agent)| uses_collector(&config_file(target, agent), &data_dir));
    if used {
        return Ok(None);
    }
    fs::remove_file(&binary)?;
    let _ = fs::remove_dir(binary.parent().expect("bin directory")); // only if empty
    let _ = fs::remove_file(data_dir.join("install.lock"));
    Ok(Some(binary))
}

/// What `doctor` and the dashboard report about one agent.
pub fn inspect(data_dir: &Path, target: &Path, agent: &str) -> Result<Value, Error> {
    let events = agent_events(agent).ok_or_else(|| Error(format!("Unsupported agent: {agent}")))?;
    let path = config_file(target, agent);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let config = match read_config(&path)? {
        Some(b) => parse_config(&b, &name)?,
        None => Map::new(),
    };
    let expected = hook_command(data_dir, agent);
    let hooks = config.get("hooks").and_then(Value::as_object);
    let registered: Vec<Value> = events
        .iter()
        .filter(|event| {
            hooks.and_then(|h| h.get(event)).and_then(Value::as_array).is_some_and(|groups| {
                groups
                    .iter()
                    .filter_map(|g| g.get("hooks").and_then(Value::as_array))
                    .flatten()
                    .any(|h| is_ours(h) && h.get("command").and_then(Value::as_str) == Some(expected.as_str()))
            })
        })
        .map(|e| Value::str(*e))
        .collect();
    let collector = collector_path(data_dir).exists();
    let installed = registered.len() == events.len() && collector;
    Ok(obj![
        ("agent", agent),
        ("config_path", path.to_string_lossy().into_owned()),
        ("registered", Value::Array(registered)),
        ("expected", Value::Array(events.iter().map(|e| Value::str(*e)).collect())),
        ("installed", installed),
        ("collector_exists", collector),
        ("trust", if agent == "codex" { "unknown" } else { "not_required" }),
        ("note", note(agent)),
    ])
}
