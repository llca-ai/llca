//! Command-line interface.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::demo;
use crate::install;
use crate::json::{self, Value};
use crate::obj;
use crate::server;
use crate::store::{self, Filter};
use crate::util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
llca: Local Ledger for Coding Agents. A local log of what Codex and
Claude Code do on this machine.

Usage: llca [--data-dir PATH] COMMAND [OPTIONS]

Commands:
  init       Add llca hooks       (--global | --project PATH) [--agent codex|claude|all]
  uninstall  Remove llca hooks    (--global | --project PATH) [--agent codex|claude|all]
  doctor     Check the installation      [--project PATH] [--agent codex|claude|all] [--json]
  logs       Print recorded events       [--json] [--search TEXT] [--session ID] [--agent codex|claude] [--limit N]
  open       Open the dashboard          [--port N] [--no-browser]
  demo       Dashboard with sample data  [--port N] [--no-browser]
  redact     Apply the current secret redaction to events already recorded
  audit      List recorded values that look like secrets, with the rule to add

Options:
  --data-dir PATH     Private location of the log (default: $LLCA_HOME, else
                      ~/Library/Application Support/llca on macOS and
                      $XDG_DATA_HOME/llca or ~/.local/share/llca on Linux)
  --codex-dir PATH    Codex configuration directory (init, uninstall, doctor)
  --claude-dir PATH   Claude Code settings directory or JSON file (init, uninstall, doctor)
  -h, --help          Show this help
  --version           Show the version
";

const COVERAGE: [&str; 5] = [
    "Local calls only",
    "Permission requests: final decision not exposed (Claude Code reports some denials only)",
    "No exhaustive record of reads, clicks or cloud actions",
    "Tool output and file contents are not kept",
    "Secrets redacted on a best-effort basis; commands may still be sensitive",
];

struct UsageError(String);

#[derive(Default)]
struct Args {
    data_dir: Option<PathBuf>,
    command: String,
    adapter: Option<String>,
    agent: Option<String>,
    codex_dir: Option<PathBuf>,
    claude_dir: Option<PathBuf>,
    global: bool,
    project: Option<PathBuf>,
    json: bool,
    search: String,
    session: String,
    limit: i64,
    port: u16,
    no_browser: bool,
}

fn parse_args(raw: Vec<String>) -> Result<Option<Args>, UsageError> {
    let mut args = Args { limit: 100, ..Default::default() };
    let mut it = raw.into_iter().peekable();
    let take =
        |name: &str, inline: Option<String>, it: &mut std::iter::Peekable<std::vec::IntoIter<String>>| -> Result<String, UsageError> {
            inline.or_else(|| it.next()).ok_or_else(|| UsageError(format!("argument {name}: expected one argument")))
        };
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let cmd = args.command.as_str();
        let allowed = |names: &[&str]| names.contains(&cmd);
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--version" if cmd.is_empty() => {
                println!("{VERSION}");
                std::process::exit(0);
            }
            "--data-dir" if cmd.is_empty() => args.data_dir = Some(PathBuf::from(take(&flag, inline, &mut it)?)),
            "--adapter" if allowed(&["hook"]) => args.adapter = Some(take(&flag, inline, &mut it)?),
            "--agent" if allowed(&["init", "uninstall", "doctor", "logs"]) => {
                let value = take(&flag, inline, &mut it)?;
                let valid: &[&str] = if cmd == "logs" { &["codex", "claude"] } else { &["codex", "claude", "all"] };
                if !valid.contains(&value.as_str()) {
                    return Err(UsageError(format!("argument --agent: invalid choice: '{value}' (choose from {})", valid.join(", "))));
                }
                args.agent = Some(value);
            }
            "--codex-dir" if allowed(&["init", "uninstall", "doctor"]) => {
                args.codex_dir = Some(PathBuf::from(take(&flag, inline, &mut it)?))
            }
            "--claude-dir" if allowed(&["init", "uninstall", "doctor"]) => {
                args.claude_dir = Some(PathBuf::from(take(&flag, inline, &mut it)?))
            }
            "--global" if allowed(&["init", "uninstall", "doctor"]) => args.global = true,
            "--project" if allowed(&["init", "uninstall", "doctor"]) => args.project = Some(PathBuf::from(take(&flag, inline, &mut it)?)),
            "--json" if allowed(&["logs", "doctor"]) => args.json = true,
            "--search" if allowed(&["logs"]) => args.search = take(&flag, inline, &mut it)?,
            "--session" if allowed(&["logs"]) => args.session = take(&flag, inline, &mut it)?,
            "--limit" if allowed(&["logs"]) => {
                let v = take(&flag, inline, &mut it)?;
                args.limit = v.parse().map_err(|_| UsageError(format!("argument --limit: invalid int value: '{v}'")))?;
            }
            "--port" if allowed(&["open", "demo"]) => {
                let v = take(&flag, inline, &mut it)?;
                args.port = v.parse().map_err(|_| UsageError(format!("argument --port: invalid int value: '{v}'")))?;
            }
            "--no-browser" if allowed(&["open", "demo"]) => args.no_browser = true,
            name if cmd.is_empty() && !name.starts_with('-') => {
                if !["hook", "init", "uninstall", "doctor", "logs", "open", "demo", "redact", "audit"].contains(&name) {
                    return Err(UsageError(format!("invalid command: '{name}'")));
                }
                args.command = name.to_string();
            }
            other => return Err(UsageError(format!("unrecognized argument: {other}"))),
        }
    }
    if args.command.is_empty() {
        return Err(UsageError("a command is required".into()));
    }
    if args.global && args.project.is_some() {
        return Err(UsageError("argument --project: not allowed with argument --global".into()));
    }
    Ok(Some(args))
}

fn default_data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("LLCA_HOME") {
        return PathBuf::from(p);
    }
    if cfg!(target_os = "macos") {
        return util::home().join("Library/Application Support/llca");
    }
    // Linux and others: the XDG base directory (only an absolute path counts).
    match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(p) => p.join("llca"),
        None => util::home().join(".local/share/llca"),
    }
}

/// Codex reads a project's hooks from `.codex/hooks.json`, a file that may be
/// committed with the project, and llca's hook names a path on this computer
/// (with the user name). Claude Code has settings.local.json for that; for
/// Codex, the file is listed in `.git/info/exclude`, git's own list of files to
/// leave out on this computer only (nothing in the repository changes).
fn keep_codex_hooks_local(project: &Path) -> String {
    // The hooks file, and the backup and lock files llca writes next to it.
    const ENTRIES: [&str; 3] = ["/.codex/hooks.json", "/.codex/hooks.json.llca-backup-*", "/.codex/.llca-install.lock"];
    let git = project.join(".git");
    if !git.is_dir() {
        return "Note: .codex/hooks.json now names a path on this computer. Do not commit it with the project.".into();
    }
    let tracked = std::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["ls-files", "--error-unmatch", ".codex/hooks.json"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if tracked {
        return "Warning: .codex/hooks.json is in this project's git repository, and it now names a path on this computer. \
                Do not commit this change; to keep the file out of git: git rm --cached .codex/hooks.json"
            .into();
    }
    let exclude = git.join("info").join("exclude");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    let missing: Vec<&str> = ENTRIES.iter().copied().filter(|e| !current.lines().any(|l| l.trim() == *e)).collect();
    if missing.is_empty() {
        return "Git: .codex/hooks.json stays on this computer (.git/info/exclude).".into();
    }
    let mut text = current;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("# llca: Codex hooks with a path on this computer\n");
    for e in missing {
        text.push_str(e);
        text.push('\n');
    }
    match std::fs::create_dir_all(git.join("info")).and_then(|_| std::fs::write(&exclude, text)) {
        Ok(()) => {
            "Git: added .codex/hooks.json to .git/info/exclude, so it stays on this computer (the repository does not change).".into()
        }
        Err(_) => "Note: .codex/hooks.json now names a path on this computer. Do not commit it with the project.".into(),
    }
}

fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| util::home().join(".codex"))
}

fn claude_home() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| util::home().join(".claude"))
}

fn agents_for(choice: Option<&str>, default: &str) -> Vec<&'static str> {
    match choice.unwrap_or(default) {
        "codex" => vec!["codex"],
        "claude" => vec!["claude-code"],
        _ => vec!["codex", "claude-code"],
    }
}

/// Configuration file or directory for one agent, from --global/--project/--*-dir.
fn target_for(agent: &str, args: &Args) -> PathBuf {
    if agent == "codex" {
        return args.codex_dir.clone().unwrap_or_else(|| match &args.project {
            Some(p) => p.join(".codex"),
            None => codex_home(),
        });
    }
    // Claude Code project hooks go to settings.local.json: the command holds an
    // absolute path on this machine and must not be committed with the repository.
    args.claude_dir.clone().unwrap_or_else(|| match &args.project {
        Some(p) => p.join(".claude").join("settings.local.json"),
        None => claude_home(),
    })
}

/// The hook entry point. Never blocks or changes the agent: always exits 0,
/// prints `{}` for Codex (required by its Stop contract) and nothing for
/// Claude Code (plain output on SessionStart would reach the model).
fn collect(data_dir: &Path, adapter: &str) -> i32 {
    let agent = install::agent_for(adapter).unwrap_or("codex");
    std::panic::set_hook(Box::new(|_| {})); // a panic must not write to the agent's terminal
    let result = std::panic::catch_unwind(|| -> Result<(), String> {
        let mut raw = Vec::new();
        let mut stdin = std::io::stdin();
        (&mut stdin).take(store::MAX_INPUT as u64 + 1).read_to_end(&mut raw).map_err(|_| "OSError".to_string())?;
        if raw.len() > store::MAX_INPUT {
            // Read the rest anyway, so the agent never writes into a closed pipe.
            let _ = std::io::copy(&mut stdin, &mut std::io::sink());
            return Err("TooLarge".into());
        }
        let text = std::str::from_utf8(&raw).map_err(|_| "UnicodeDecodeError".to_string())?;
        let payload = json::parse(text).map_err(|_| "JSONDecodeError".to_string())?;
        store::record(data_dir, &payload, "hook", agent, None).map_err(|_| "RecordError".to_string())?;
        Ok(())
    });
    let failure = match result {
        Ok(Ok(())) => None,
        Ok(Err(kind)) => Some(kind),
        Err(_) => Some("Panic".to_string()),
    };
    if let Some(kind) = failure {
        // No raw input or error text: either may contain credentials.
        let _ = (|| -> std::io::Result<()> {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            util::private_dir(data_dir)?;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(data_dir.join("collector-errors.log"))?;
            writeln!(f, "{} {kind} event not recorded", util::now_iso())
        })();
    }
    if agent == "codex" {
        println!("{{}}");
    }
    0
}

fn print_logs(data_dir: &Path, args: &Args) -> Result<(), String> {
    let agent = match args.agent.as_deref() {
        Some("claude") => "claude-code",
        Some(a) => a,
        None => "",
    };
    let filter = Filter { search: &args.search, session: &args.session, agent, limit: args.limit, ..Default::default() };
    let rows = store::query(data_dir, &filter).map_err(|e| e.to_string())?.events;
    let field = |row: &Value, k: &str| row.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    for row in &rows {
        if args.json {
            println!("{}", row.to_compact());
        } else {
            let tool = field(row, "tool");
            println!(
                "{}  {:11} {:18} {:30} {}",
                field(row, "received_at"),
                store::agent_name(&field(row, "agent")),
                field(row, "event"),
                if tool.is_empty() { "session".to_string() } else { tool },
                field(row, "status")
            );
        }
    }
    if rows.is_empty() && !args.json {
        println!("No events. Run llca doctor (and, for Codex, check trust in /hooks).");
    }
    Ok(())
}

fn serve(data_dir: &Path, args: &Args, demo: bool) -> Result<(), String> {
    let targets = vec![("codex".to_string(), codex_home()), ("claude-code".to_string(), claude_home())];
    let (listener, dashboard, url) = server::bind(data_dir, targets, args.port, demo).map_err(|e| e.to_string())?;
    println!("{url}");
    println!("Local dashboard · Ctrl+C to stop");
    if !args.no_browser {
        let url = url.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            server::open_browser(&url);
        });
    }
    util::catch_stop_signals();
    server::serve_until(listener, dashboard, util::stop_requested).map_err(|e| e.to_string())
}

fn run(args: Args) -> Result<i32, String> {
    let data_dir = util::resolve(&args.data_dir.clone().unwrap_or_else(default_data_dir));
    match args.command.as_str() {
        "hook" => Ok(collect(&data_dir, args.adapter.as_deref().unwrap_or("codex-v0"))),
        cmd @ ("init" | "uninstall") => {
            if !(args.global || args.project.is_some() || args.codex_dir.is_some() || args.claude_dir.is_some()) {
                eprintln!("llca: error: Specify --global, --project PATH, --codex-dir PATH or --claude-dir PATH");
                return Ok(2);
            }
            let agents = agents_for(args.agent.as_deref(), "codex");
            let mut results = Vec::new();
            for agent in &agents {
                results
                    .push(install::configure(&target_for(agent, &args), &data_dir, cmd == "uninstall", agent).map_err(|e| e.to_string())?);
            }
            let report = if results.len() == 1 { results.remove(0) } else { Value::Array(results) };
            println!("{}", report.to_pretty());
            if cmd == "init" {
                if crate::watch::create_if_missing(&data_dir).unwrap_or(false) {
                    println!("Your rules: {} (see the examples inside).", data_dir.join(crate::watch::FILE_NAME).display());
                }
                if agents.contains(&"codex") {
                    if let (Some(project), None) = (&args.project, &args.codex_dir) {
                        println!("{}", keep_codex_hooks_local(project));
                    }
                    println!("Codex: open /hooks to review and trust llca, then start a new session.");
                }
                if agents.contains(&"claude-code") {
                    println!("Claude Code: hooks are picked up automatically; check them with /hooks or start a new session.");
                }
                println!("Hooks observe calls without changing permission decisions.");
            }
            Ok(0)
        }
        "doctor" => {
            let mut agents = json::Map::new();
            let mut any = false;
            for agent in agents_for(args.agent.as_deref(), "all") {
                let state = install::inspect(&data_dir, &target_for(agent, &args), agent).map_err(|e| e.to_string())?;
                any |= state.get("installed").and_then(Value::as_bool).unwrap_or(false);
                agents.insert(agent, state);
            }
            let mut report = obj![
                ("agents", Value::Object(agents)),
                ("installed", any),
                ("collector", install::collector_state(&data_dir)),
                ("journal", store::stats(&data_dir).map_err(|e| e.to_string())?),
                ("journal_path", data_dir.join("events.sqlite3").to_string_lossy().into_owned()),
                ("data_dir_shared", util::open_to_others(&data_dir)),
                ("collector_errors", crate::doctor::collector_errors(&data_dir)),
                ("watch", {
                    let (rules, exists) = crate::watch::load(&data_dir);
                    obj![
                        ("path", data_dir.join(crate::watch::FILE_NAME).to_string_lossy().into_owned()),
                        ("exists", exists),
                        ("rules", Value::int(rules.rules.len() as i64)),
                        (
                            "errors",
                            Value::Array(
                                rules
                                    .errors
                                    .into_iter()
                                    .map(|(line, message)| obj![("line", Value::int(line as i64)), ("message", message)])
                                    .collect()
                            )
                        ),
                    ]
                }),
                ("coverage", Value::Array(COVERAGE.iter().map(|c| Value::str(*c)).collect())),
                ("version", VERSION),
            ];
            // How the user chose the configuration, reused in the suggested commands.
            let target = match &args.project {
                Some(p) => format!("--project {}", util::shell_quote(&p.to_string_lossy())),
                None => "--global".to_string(),
            };
            if args.json {
                let problems = crate::doctor::problems(&report, &target);
                let list = problems.iter().map(|p| obj![("problem", p.what.clone()), ("fix", p.fix.clone())]).collect();
                if let Some(map) = report.as_object_mut() {
                    map.insert("problems", Value::Array(list));
                }
                println!("{}", report.to_pretty());
                return Ok(if problems.is_empty() { 0 } else { 1 });
            }
            use std::io::IsTerminal;
            let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
            let (text, problems) = crate::doctor::render(&report, &target, color);
            print!("{text}");
            Ok(if problems == 0 { 0 } else { 1 })
        }
        "logs" => print_logs(&data_dir, &args).map(|_| 0),
        "open" => serve(&data_dir, &args, false).map(|_| 0),
        "audit" => {
            let report = crate::audit::audit(&data_dir).map_err(|e| e.to_string())?;
            print!("{}", crate::audit::render(&report));
            Ok(0)
        }
        "redact" => {
            let r = store::redact_journal(&data_dir).map_err(|e| e.to_string())?;
            println!("Checked {} events; {} updated.", r.checked, r.changed);
            if !r.wiped {
                // The rows are clean; earlier copies may remain in the WAL file.
                eprintln!("The journal was in use (the dashboard is probably open), so older copies of the text may remain in its WAL file until the next cleanup.");
                eprintln!("Close the dashboard (Ctrl+C in its terminal), then run llca redact again.");
                return Ok(1);
            }
            Ok(0)
        }
        "demo" => {
            let name = format!("llca-demo-{}", util::base64url(&util::random_bytes(6).map_err(|e| e.to_string())?));
            let dir = std::env::temp_dir().join(name);
            util::private_dir(&dir).map_err(|e| e.to_string())?;
            let result = demo::fill(&dir).map_err(|e| e.to_string()).and_then(|_| serve(&dir, &args, true));
            let _ = std::fs::remove_dir_all(&dir);
            result.map(|_| 0)
        }
        _ => unreachable!(),
    }
}

pub fn main() -> i32 {
    util::private_umask();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(raw) {
        Ok(None) => {
            print!("{USAGE}");
            0
        }
        Ok(Some(args)) => match run(args) {
            Ok(code) => code,
            Err(message) => {
                eprintln!("llca: {message}");
                1
            }
        },
        Err(UsageError(message)) => {
            eprint!("{USAGE}");
            eprintln!("llca: error: {message}");
            2
        }
    }
}
