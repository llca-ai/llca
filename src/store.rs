//! Normalising hook events and storing them in the local SQLite journal.
//! The schema and the stored JSON are the same as version 0.2 (Python),
//! so existing journals keep working.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::json::{Map, Value};
use crate::obj;
use crate::redact::{redact, redact_noting};
use crate::sqlite::{Cell, Connection, Param};
use crate::util;
use std::collections::{BTreeMap, HashMap, HashSet};

pub const CODEX_EVENTS: [&str; 7] = ["SessionStart", "PreToolUse", "PermissionRequest", "PostToolUse", "Stop", "SessionEnd", "Interrupt"];
// Claude Code has no Interrupt event, but reports failed tools and denied permissions.
pub const CLAUDE_EVENTS: [&str; 8] =
    ["SessionStart", "PreToolUse", "PermissionRequest", "PermissionDenied", "PostToolUse", "PostToolUseFailure", "Stop", "SessionEnd"];
/// The largest event the collector reads. Agents send the tool's whole answer
/// with PostToolUse (the content of a file read, a command's output): it is
/// never kept, but it has to be read. 2 MB lost real events (a large Read or
/// Write); 64 MB covers what agents send, and bounds the work.
pub const MAX_INPUT: usize = 64 * 1024 * 1024;

pub fn agent_events(agent: &str) -> Option<&'static [&'static str]> {
    match agent {
        "codex" => Some(&CODEX_EVENTS),
        "claude-code" => Some(&CLAUDE_EVENTS),
        _ => None,
    }
}

pub fn agent_name(agent: &str) -> &str {
    match agent {
        "codex" => "Codex",
        "claude-code" => "Claude Code",
        other => other,
    }
}

fn status_for(event: &str) -> &'static str {
    match event {
        "PreToolUse" => "requested",
        "PermissionRequest" => "approval_requested",
        "PermissionDenied" => "denied",
        "PostToolUse" => "returned",
        "PostToolUseFailure" => "error",
        "SessionStart" => "started",
        "SessionEnd" => "ended",
        "Stop" => "turn_ended",
        _ => "interrupted",
    }
}

const SHELL_TOOLS: [&str; 6] = ["Bash", "exec_command", "shell", "shell_command", "BashOutput", "KillShell"];
const FILE_TOOLS: [&str; 10] = ["apply_patch", "Read", "Edit", "MultiEdit", "Write", "NotebookEdit", "Glob", "Grep", "LS", "read_file"];

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl From<crate::sqlite::Error> for Error {
    fn from(e: crate::sqlite::Error) -> Self {
        Error(e.to_string())
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

/// File paths touched by an apply_patch envelope (contents are not kept).
fn patch_files(patch: &str) -> Vec<Value> {
    const MARKERS: [&str; 4] = ["*** Add File: ", "*** Update File: ", "*** Delete File: ", "*** Move to: "];
    patch
        .split('\n')
        .filter_map(|line| MARKERS.iter().find_map(|m| line.strip_prefix(m)))
        .filter(|path| !path.is_empty())
        .map(Value::str)
        .collect()
}

/// Turn a raw hook payload into the stored event. Arguments are redacted
/// here, before anything reaches the disk.
pub fn normalize(payload: &Value, origin: &str, agent: &str) -> Result<Value, Error> {
    let events = agent_events(agent).ok_or_else(|| Error("Unsupported agent".into()))?;
    let p = payload.as_object().ok_or_else(|| Error("Missing or unsupported event".into()))?;
    let event = p.get("hook_event_name").and_then(Value::as_str).filter(|e| events.contains(e));
    let event = event.ok_or_else(|| Error("Missing or unsupported event".into()))?;
    match p.get("session_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => {}
        _ => return Err(Error("Missing session_id".into())),
    }
    let tool = match p.get("tool_name") {
        Some(Value::String(s)) => s.clone(),
        Some(v) if v.is_truthy() => v.to_compact(),
        _ => String::new(),
    };
    let mut args = p.get("tool_input").cloned().unwrap_or_else(|| Value::Object(Map::new()));
    if matches!(tool.as_str(), "apply_patch" | "Edit" | "Write") {
        if let Value::Object(map) = &mut args {
            if let Some(Value::String(patch)) = map.get("command") {
                if patch.contains("*** Begin Patch") {
                    let files = patch_files(patch);
                    map.insert("files", Value::Array(files));
                    map.insert("command", Value::str("[PATCH NOT KEPT]"));
                }
            }
        }
    }
    // `secrets`: what kinds of secrets were masked ("GitHub token"), so the
    // dashboard can say a secret went through the agent. Never the values.
    let (args, secrets) = redact_noting(&args);

    // A returned tool is not necessarily a successful operation. Only explicit
    // structured fields establish success or failure; free text is not read.
    let mut status = status_for(event).to_string();
    let mut outcome = Map::new();
    if event == "PostToolUse" {
        match p.get("tool_response") {
            Some(Value::Object(response)) => {
                if let Some(code) = response.get("exit_code").and_then(Value::as_integer) {
                    outcome.insert("exit_code", Value::int(code));
                    status = if code == 0 { "success" } else { "error" }.into();
                } else if let Some(is_error) = response.get("isError").and_then(Value::as_bool) {
                    status = if is_error { "error" } else { "success" }.into();
                } else if let Some(success) = response.get("success").and_then(Value::as_bool) {
                    status = if success { "success" } else { "error" }.into();
                }
                outcome.insert("response_type", Value::str("object"));
            }
            other => {
                outcome.insert("response_type", Value::str(other.unwrap_or(&Value::Null).python_type()));
            }
        }
    }
    if agent == "claude-code" && event == "PostToolUse" && status == "returned" {
        // Claude Code sends PostToolUseFailure when a tool fails (observed with a
        // non-zero exit code), so a plain PostToolUse means the tool succeeded.
        status = "success".into();
    }
    if event == "PostToolUseFailure" {
        // The error text itself is not kept: it may quote secrets or file contents.
        if let Some(interrupted) = p.get("is_interrupt").and_then(Value::as_bool) {
            outcome.insert("interrupted", Value::Bool(interrupted));
        }
    }
    let category = if SHELL_TOOLS.contains(&tool.as_str()) {
        "shell"
    } else if FILE_TOOLS.contains(&tool.as_str()) {
        "file"
    } else if tool.starts_with("mcp__") {
        "mcp"
    } else if !tool.is_empty() {
        "tool"
    } else {
        "session"
    };
    let mut safe = Map::new();
    for key in ["session_id", "turn_id", "tool_use_id", "cwd", "model", "permission_mode", "source", "agent_type"] {
        safe.insert(key, p.get(key).cloned().unwrap_or(Value::Null));
    }
    let Value::Object(mut row) = redact(&Value::Object(safe)) else { unreachable!() };
    row.insert("received_at", Value::str(util::now_iso()));
    row.insert("agent", Value::str(agent));
    row.insert("event", Value::str(event));
    row.insert("tool", redact(&Value::str(tool)));
    row.insert("category", Value::str(category));
    row.insert("status", Value::String(status));
    row.insert("arguments", args);
    if !secrets.is_empty() {
        row.insert("secrets", Value::Array(secrets.into_iter().map(Value::String).collect()));
    }
    row.insert("outcome", Value::Object(outcome));
    row.insert("origin", Value::str(origin));
    Ok(Value::Object(row))
}

/// Open the journal, creating it (private to the user) if needed.
pub fn connect(data_dir: &Path) -> Result<Connection, Error> {
    util::private_dir(data_dir)?;
    let path = data_dir.join("events.sqlite3");
    OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let con = Connection::open(&path, 500)?;
    con.execute("PRAGMA journal_mode=WAL", &[])?;
    con.execute(
        "CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, received_at TEXT NOT NULL, session_id TEXT NOT NULL, event TEXT NOT NULL, tool TEXT NOT NULL, status TEXT NOT NULL, category TEXT NOT NULL, payload TEXT NOT NULL)",
        &[],
    )?;
    con.execute("CREATE INDEX IF NOT EXISTS events_session ON events(session_id, id)", &[])?;
    // Periods (7 days, 30 days…) read only their own events.
    con.execute("CREATE INDEX IF NOT EXISTS events_time ON events(received_at)", &[])?;
    // Tool calls whose secret alert was dismissed on the dashboard. `batch`
    // groups one click, so that it can be undone.
    con.execute("CREATE TABLE IF NOT EXISTS dismissed_secrets (call TEXT PRIMARY KEY, batch INTEGER NOT NULL)", &[])?;
    let columns = con.query("PRAGMA table_info(events)", &[])?;
    if !columns.iter().any(|row| row.get(1).and_then(Cell::text) == Some("agent")) {
        // Journals created before multi-agent support only contain Codex events.
        if let Err(e) = con.execute("ALTER TABLE events ADD COLUMN agent TEXT NOT NULL DEFAULT 'codex'", &[]) {
            if !e.0.contains("duplicate column") {
                return Err(e.into()); // otherwise another collector migrated first
            }
        }
    }
    Ok(con)
}

// ---------- Retention ----------

/// Events older than this many days are deleted, unless the user changes it
/// with `llca retention` (kept in the file RETENTION_FILE of the data folder).
pub const DEFAULT_RETENTION_DAYS: i64 = 90;
const RETENTION_FILE: &str = "retention";
/// When the collector last deleted old events (its modification time).
const PRUNED_FILE: &str = "last-prune";

/// Days of events kept, or None when nothing is ever deleted ("off").
pub fn retention_days(data_dir: &Path) -> Option<i64> {
    match fs::read_to_string(data_dir.join(RETENTION_FILE)) {
        Ok(text) => match text.trim() {
            "off" => None,
            n => Some(n.parse::<i64>().ok().filter(|d| (1..=36_500).contains(d)).unwrap_or(DEFAULT_RETENTION_DAYS)),
        },
        Err(_) => Some(DEFAULT_RETENTION_DAYS),
    }
}

pub fn set_retention(data_dir: &Path, days: Option<i64>) -> std::io::Result<()> {
    util::private_dir(data_dir)?;
    let text = days.map_or("off".to_string(), |d| d.to_string());
    util::atomic_write(&data_dir.join(RETENTION_FILE), format!("{text}\n").as_bytes(), 0o600)
}

/// Delete the events older than the retention period, at most `limit` of
/// them (all when None). Deleted text is overwritten (secure_delete), as the
/// events may hold commands. Returns how many were deleted.
pub fn prune(data_dir: &Path, limit: Option<i64>) -> Result<usize, Error> {
    let Some(days) = retention_days(data_dir) else { return Ok(0) };
    let cutoff = util::iso_from_micros(util::now_micros() - days * 86_400_000_000);
    let con = connect(data_dir)?;
    con.query("PRAGMA secure_delete=ON", &[])?;
    match limit {
        Some(n) => con.execute(
            "DELETE FROM events WHERE id IN (SELECT id FROM events WHERE received_at < ? ORDER BY id LIMIT ?)",
            &[Param::Text(&cutoff), Param::Int(n)],
        )?,
        None => con.execute("DELETE FROM events WHERE received_at < ?", &[Param::Text(&cutoff)])?,
    };
    Ok(con.changes().max(0) as usize)
}

/// Called by the collector: at most once an hour, delete up to 2,000 old
/// events, so that the journal keeps its size without slowing any agent
/// (a large backlog goes in steps, or at once when the dashboard starts).
pub fn prune_if_due(data_dir: &Path) {
    let marker = data_dir.join(PRUNED_FILE);
    let due = fs::metadata(&marker).and_then(|m| m.modified()).map_or(true, |t| t.elapsed().map_or(true, |e| e.as_secs() >= 3600));
    if due && retention_days(data_dir).is_some() {
        let _ = util::atomic_write(&marker, b"", 0o600);
        let _ = prune(data_dir, Some(2000));
    }
}

/// Normalise and store one event. `at` overrides the timestamp (demo data only).
pub fn record(data_dir: &Path, payload: &Value, origin: &str, agent: &str, at: Option<&str>) -> Result<i64, Error> {
    let mut row = normalize(payload, origin, agent)?;
    let map = row.as_object_mut().expect("normalize returns an object");
    if let Some(at) = at {
        map.insert("received_at", Value::str(at));
    }
    let field = |k: &str| map.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let values: Vec<String> =
        ["received_at", "session_id", "event", "tool", "status", "category", "agent"].iter().map(|k| field(k)).collect();
    let json = row.to_compact();
    let con = connect(data_dir)?;
    let mut params: Vec<Param> = values.iter().map(|v| Param::Text(v)).collect();
    params.push(Param::Text(&json));
    Ok(con
        .execute("INSERT INTO events(received_at,session_id,event,tool,status,category,agent,payload) VALUES(?,?,?,?,?,?,?,?)", &params)?)
}

/// Apply the current redaction rules to events already stored, for secrets
/// that earlier versions missed. Freed space is overwritten and the WAL file
/// emptied, so the old text does not linger on disk. Returns (checked, updated).
/// What `redact` did: events checked and updated, and whether the old text
/// could also be wiped from the WAL file (not while another program, such as
/// the dashboard, is reading the journal).
pub struct Redacted {
    pub checked: usize,
    pub changed: usize,
    pub wiped: bool,
}

pub fn redact_journal(data_dir: &Path) -> Result<Redacted, Error> {
    let con = connect(data_dir)?;
    con.query("PRAGMA secure_delete=ON", &[])?;
    con.execute("BEGIN IMMEDIATE", &[])?;
    let result = (|| -> Result<(usize, usize), Error> {
        let rows = con.query("SELECT id, payload FROM events", &[])?;
        let mut changed = 0;
        for row in &rows {
            let (Some(id), Some(text)) = (row[0].int(), row[1].text()) else { continue };
            // Markers written by the French versions (0.1) become today's markers.
            let current = text.replace("[SECRET MASQUÉ]", crate::redact::MASK).replace("[CONTENU NON CONSERVÉ]", crate::redact::NOT_KEPT);
            let Ok(Value::Object(mut payload)) = crate::json::parse(&current) else { continue };
            let Some(args) = payload.get("arguments") else { continue };
            let (cleaned, found) = crate::redact::redact_noting(args);
            if &cleaned == args && current == text {
                continue;
            }
            payload.insert("arguments", cleaned);
            // Kinds of what is masked now, as the collector records them, so
            // that the dashboard names them ("GitHub token", not "kind not
            // recorded"). Kinds already listed are kept.
            let mut kinds: Vec<String> = payload
                .get("secrets")
                .and_then(Value::as_array)
                .map(|l| l.iter().filter_map(Value::as_str).map(String::from).collect())
                .unwrap_or_default();
            for kind in found {
                if !kinds.contains(&kind) {
                    kinds.push(kind);
                }
            }
            if !kinds.is_empty() {
                payload.insert("secrets", Value::Array(kinds.into_iter().map(Value::String).collect()));
            }
            let json = Value::Object(payload).to_compact();
            con.execute("UPDATE events SET payload=? WHERE id=?", &[Param::Text(&json), Param::Int(id)])?;
            changed += 1;
        }
        Ok((rows.len(), changed))
    })();
    match result {
        Ok((checked, changed)) => {
            con.execute("COMMIT", &[])?;
            if changed > 0 {
                con.execute("VACUUM", &[])?;
            }
            // Copy everything into the journal file and empty the WAL, where
            // the old text may still be. A reader (the dashboard, a hook)
            // can hold it for a moment: each try waits up to 0.5 s (the busy
            // timeout), then the next one starts 0.1 s later; after 4, say so.
            let mut wiped = false;
            for attempt in 0..4 {
                let row = con.query("PRAGMA wal_checkpoint(TRUNCATE)", &[])?;
                if row.first().and_then(|r| r.first()).and_then(Cell::int) == Some(0) {
                    wiped = true;
                    break;
                }
                if attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            Ok(Redacted { checked, changed, wiped })
        }
        Err(e) => {
            let _ = con.execute("ROLLBACK", &[]);
            Err(e)
        }
    }
}

#[derive(Default)]
pub struct Filter<'a> {
    pub search: &'a str,
    pub category: &'a str,
    pub session: &'a str,
    pub agent: &'a str,
    pub before: i64,
    pub limit: i64,
    /// Time range, as UTC timestamps in the stored format (inclusive, exclusive).
    pub from: &'a str,
    pub to: &'a str,
    /// Working folder of the session (exact match).
    pub project: &'a str,
    /// An online service the call reached (name from rules/services.rules).
    pub service: &'a str,
    /// The label of one of your watch rules (watch.rules).
    pub rule: &'a str,
    /// What the action is, as the Activity buttons say: shell, edit, read,
    /// web, mcp, computer, app, other, secret or attention (see KINDS).
    pub kind: &'a str,
    /// How the action ended: problem, waiting, ok or done (see RESULTS).
    pub result: &'a str,
}

/// The values the `kind` and `result` filters accept.
pub const KINDS: [&str; 10] = ["shell", "edit", "read", "web", "mcp", "computer", "app", "other", "secret", "attention"];
pub const RESULTS: [&str; 4] = ["problem", "waiting", "ok", "done"];

/// One page of the event list. `next` is where the following page starts
/// (pass it as `before`), or None when there is nothing older to look at.
pub struct Page {
    pub events: Vec<Value>,
    pub next: Option<i64>,
    /// How many events were looked at to fill this page.
    pub scanned: usize,
}

/// WHERE clauses shared by the event list and the overview: agent, time range, project.
fn scope<'a>(f: &Filter<'a>, clauses: &mut Vec<&'static str>, params: &mut Vec<Param<'a>>) {
    if !f.agent.is_empty() {
        clauses.push("agent=?");
        params.push(Param::Text(f.agent));
    }
    if !f.from.is_empty() {
        clauses.push("received_at>=?");
        params.push(Param::Text(f.from));
    }
    if !f.to.is_empty() {
        clauses.push("received_at<?");
        params.push(Param::Text(f.to));
    }
    if !f.project.is_empty() {
        clauses.push("json_extract(payload,'$.cwd')=?");
        params.push(Param::Text(f.project));
    }
}

fn where_sql(clauses: &[&str]) -> String {
    if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    }
}

/// One event read for the list, with what the filters need to know about it.
struct Listed {
    id: i64,
    /// The tool call it belongs to (several events per call).
    call: String,
    /// The event as sent to the dashboard (with id, agent, services, watch).
    value: Value,
    /// It passed the service and rule filters.
    kept: bool,
    /// It shows its call has the kind, or the result, asked for.
    kind: bool,
    result: bool,
}

/// Reads events for the list: parses them and works out their services and
/// your rules (same code as the map and the overview).
struct Reader<'a> {
    f: &'a Filter<'a>,
    watch: crate::watch::Rules,
}

impl Reader<'_> {
    fn read(&self, row: &[Cell]) -> Listed {
        let id = row[0].int().unwrap_or(0);
        let agent = row[1].text().unwrap_or("codex").to_string();
        let raw = row[2].text().unwrap_or("{}");
        let mut v = crate::json::parse(raw).unwrap_or_else(|_| Value::Object(Map::new()));
        let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let (tool, category, event, status) = (text("tool"), text("category"), text("event"), text("status"));
        let call = match v.get("tool_use_id").and_then(Value::as_str) {
            Some(use_id) if !use_id.is_empty() => format!("{agent}|{}|{use_id}", text("session_id")),
            // Without an id, a call is its turn and tool (as the dashboard groups them).
            _ => format!("{agent}|{}|{}|{tool}", text("session_id"), text("turn_id")),
        };
        let args = v.get("arguments").cloned().unwrap_or(Value::Null);
        let services = crate::resources::services_of(&tool, &category, &args);
        let mut kept = self.f.service.is_empty() || services.iter().any(|(name, _)| name == self.f.service);
        let names = services.iter().map(|(name, _)| name.clone()).collect();
        let found = if tool.is_empty() {
            Vec::new()
        } else {
            crate::watch::matches(&self.watch, &crate::watch::Subject::of(&tool, &category, &args, names))
        };
        kept &= self.f.rule.is_empty() || found.iter().any(|m| m.rule.label == self.f.rule);
        let e = Seen {
            tool: &tool,
            category: &category,
            event: &event,
            status: &status,
            args: &args,
            secrets: v.get("secrets").is_some() || raw.contains("[REDACTED]") || raw.contains("[SECRET MASQU"),
            watched: !found.is_empty(),
        };
        let (kind, result) = (tool.is_empty() || e.kind(self.f.kind), tool.is_empty() || e.result(self.f.result));
        let found = crate::watch::to_json(&found);
        if let Some(map) = v.as_object_mut() {
            map.insert("id", Value::int(id));
            map.insert("agent", Value::str(agent));
            map.insert("services", Value::Array(services.into_iter().map(|(name, via)| obj![("name", name), ("via", via)]).collect()));
            map.insert("watch", found);
        }
        Listed { id, call, value: v, kept, kind, result }
    }
}

/// What one event shows about its call, for the kind and result filters.
///
/// The dashboard decides what an action is from all of its events (the
/// request, the approval, the result) and from rules written in JavaScript
/// (what a risky command looks like…). Here, each event only says whether
/// its call MAY match; the dashboard then applies the exact rule. So these
/// tests must never miss a call the dashboard would show: when unsure, yes.
struct Seen<'a> {
    tool: &'a str,
    category: &'a str,
    event: &'a str,
    status: &'a str,
    args: &'a Value,
    secrets: bool,
    watched: bool,
}

/// Words found in every risky command the dashboard flags (RISKS in app.js).
const RISK_WORDS: [&str; 12] = ["sudo", "rm", "git", "curl", "wget", "chmod", "publish", "docker", "gh", "terraform", "kubectl", "deploy"];
const EDIT_TOOLS: [&str; 5] = ["apply_patch", "Edit", "MultiEdit", "Write", "NotebookEdit"];
const READ_TOOLS: [&str; 5] = ["Read", "read_file", "Grep", "Glob", "LS"];
const WEB_TOOLS: [&str; 2] = ["WebFetch", "WebSearch"];
/// Tools the dashboard flags by name: data leaving the computer, work scheduled for later.
const FLAGGED_TOOLS: [&str; 5] = ["Artifact", "ShareOnboardingGuide", "CronCreate", "ScheduleWakeup", "RemoteTrigger"];

impl Seen<'_> {
    fn command(&self) -> String {
        crate::resources::command_text(self.args.get("command")) + &crate::resources::command_text(self.args.get("cmd"))
    }

    fn shell(&self) -> bool {
        self.category == "shell"
    }

    /// Computer Use (isComputerUse in app.js), more broadly.
    fn computer(&self) -> bool {
        let tool = self.tool.to_ascii_lowercase();
        tool.starts_with("mcp__cua") || tool.starts_with("mcp__computer")
    }

    fn secret(&self) -> bool {
        self.secrets
    }

    fn kind(&self, kind: &str) -> bool {
        let tool = self.tool;
        let other = !self.shell()
            && !EDIT_TOOLS.contains(&tool)
            && !READ_TOOLS.contains(&tool)
            && !WEB_TOOLS.contains(&tool)
            && !tool.starts_with("mcp__");
        match kind {
            "" => true,
            "shell" => self.shell(),
            "edit" => !self.shell() && EDIT_TOOLS.contains(&tool),
            "read" => !self.shell() && READ_TOOLS.contains(&tool),
            "web" => !self.shell() && WEB_TOOLS.contains(&tool),
            "mcp" => !self.shell() && tool.starts_with("mcp__"),
            "computer" => self.computer(),
            // An application opened by a command (/Applications/X.app, open -a,
            // tell application), or Computer Use.
            "app" => {
                self.computer()
                    || (self.shell() && {
                        let c = self.command().to_ascii_lowercase();
                        c.contains(".app") || c.contains("open") || c.contains("tell")
                    })
            }
            "other" => other,
            "secret" => self.secret(),
            "attention" => {
                self.secret()
                    || self.watched
                    || self.computer()
                    || FLAGGED_TOOLS.contains(&tool)
                    || self.result("problem")
                    || self.result("waiting")
                    || (self.shell() && {
                        let c = self.command();
                        RISK_WORDS.iter().any(|w| c.contains(w))
                    })
            }
            _ => false,
        }
    }

    fn result(&self, result: &str) -> bool {
        match result {
            "" => true,
            "problem" => self.status == "error" || self.event == "PermissionDenied",
            "waiting" => self.event == "PermissionRequest",
            "ok" => self.status == "success",
            "done" => self.event == "PostToolUse",
            _ => false,
        }
    }
}

pub fn query(data_dir: &Path, f: &Filter) -> Result<Page, Error> {
    let search: String = f.search.chars().take(500).collect();
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    scope(f, &mut clauses, &mut params);
    if !search.is_empty() {
        clauses.push("instr(lower(payload),lower(?)) > 0");
        params.push(Param::Text(&search));
    }
    if !f.category.is_empty() {
        clauses.push("category=?");
        params.push(Param::Text(f.category));
    }
    if !f.session.is_empty() {
        clauses.push("session_id=?");
        params.push(Param::Text(f.session));
    }
    let limit = f.limit.clamp(1, 500) as usize;
    let con = connect(data_dir)?;
    let reader = Reader { f, watch: crate::watch::load(data_dir).0 };
    // Without a filter worked out here, one page is one query. With one, rows
    // are read in batches until the page is full, at most SCAN rows back.
    const BATCH: i64 = 1000;
    const SCAN: usize = 100_000;
    let by_call = !f.kind.is_empty() || !f.result.is_empty();
    let filtered = by_call || !f.service.is_empty() || !f.rule.is_empty();
    let batch = if filtered { BATCH } else { limit as i64 };
    let sql =
        format!("SELECT id,agent,payload FROM events{} ORDER BY id DESC LIMIT ?", where_sql(&[clauses.as_slice(), &["id<?"]].concat()));
    let start = if f.before != 0 { f.before } else { i64::MAX };
    let mut cursor = start;
    let mut out: Vec<Listed> = Vec::new();
    let mut scanned = 0;

    // The kind and result filters choose whole calls: a call matches when one
    // of its events shows the kind and one shows the result. All its events
    // are then sent, so the dashboard can tell the full story: the ones read
    // before it matched (kept in `recent`) and, at the edges of the page, the
    // ones just before and after it (read again below).
    use std::collections::{HashMap, HashSet, VecDeque};
    const NEAR: usize = 2000;
    let mut calls: HashSet<String> = HashSet::new();
    let mut marks: HashMap<String, (bool, bool)> = HashMap::new();
    let mut recent: VecDeque<Listed> = VecDeque::new();

    let next = loop {
        let mut page = params_copy(&params);
        page.push(Param::Int(cursor));
        page.push(Param::Int(batch));
        let rows = con.query(&sql, &page)?;
        let count = rows.len();
        scanned += count;
        for row in rows {
            let r = reader.read(&row);
            cursor = r.id;
            if !r.kept {
                continue;
            }
            if !by_call {
                out.push(r);
            } else if r.value.get("tool").and_then(Value::as_str).unwrap_or_default().is_empty() {
                continue; // session events are not actions
            } else if calls.contains(&r.call) {
                out.push(r);
            } else {
                let mark = marks.entry(r.call.clone()).or_default();
                mark.0 |= r.kind;
                mark.1 |= r.result;
                if *mark == (true, true) {
                    marks.remove(&r.call);
                    calls.insert(r.call.clone());
                    out.extend(recent.iter().filter(|x| x.call == r.call).map(|x| Listed {
                        id: x.id,
                        call: x.call.clone(),
                        value: x.value.clone(),
                        kept: true,
                        kind: true,
                        result: true,
                    }));
                    recent.retain(|x| x.call != r.call);
                    out.push(r);
                } else {
                    recent.push_back(r);
                    if recent.len() > NEAR {
                        recent.pop_front();
                    }
                }
            }
            if out.len() >= limit {
                break;
            }
        }
        if out.len() >= limit || (filtered && scanned >= SCAN) {
            break Some(cursor);
        }
        if (count as i64) < batch {
            break None;
        }
        if !filtered {
            break Some(cursor);
        }
    };

    if by_call && !calls.is_empty() {
        // The events of these calls just outside the page: after it (they
        // were on the previous page, before the call was known to match) and
        // before it (on the next page). The dashboard drops duplicates.
        let mut edges = Vec::new();
        if start != i64::MAX {
            edges.push(format!(
                "SELECT id,agent,payload FROM events{} ORDER BY id LIMIT {NEAR}",
                where_sql(&[clauses.as_slice(), &["id>=?"]].concat())
            ));
        }
        if next.is_some() {
            edges.push(format!(
                "SELECT id,agent,payload FROM events{} ORDER BY id DESC LIMIT {NEAR}",
                where_sql(&[clauses.as_slice(), &["id<?"]].concat())
            ));
        }
        let mut seen: HashSet<i64> = out.iter().map(|r| r.id).collect();
        let bounds = [start, cursor];
        for (sql, bound) in edges.iter().zip(if start != i64::MAX { &bounds[..] } else { &bounds[1..] }) {
            let mut page = params_copy(&params);
            page.push(Param::Int(*bound));
            for row in con.query(sql, &page)? {
                let r = reader.read(&row);
                if r.kept && calls.contains(&r.call) && seen.insert(r.id) {
                    out.push(r);
                }
            }
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.id));
    }
    Ok(Page { events: out.into_iter().map(|r| r.value).collect(), next, scanned })
}

fn cell_value(c: &Cell) -> Value {
    match c {
        Cell::Null => Value::Null,
        Cell::Int(n) => Value::int(*n),
        Cell::Text(s) => Value::str(s.clone()),
    }
}

pub fn stats(data_dir: &Path) -> Result<Value, Error> {
    let con = connect(data_dir)?;
    let row = con
        .query(
            "SELECT COUNT(*), COUNT(DISTINCT session_id), SUM(event='PreToolUse'), SUM(event='PermissionRequest'), MAX(received_at) FROM events",
            &[],
        )?
        .remove(0);
    let mut agents = Map::new();
    for r in con.query("SELECT agent, COUNT(*), MAX(received_at) FROM events GROUP BY agent", &[])? {
        agents.insert(r[0].text().unwrap_or_default(), obj![("events", cell_value(&r[1])), ("last_event", cell_value(&r[2]))]);
    }
    Ok(obj![
        ("events", cell_value(&row[0])),
        ("sessions", cell_value(&row[1])),
        ("calls", cell_value(&row[2])),
        ("approvals", cell_value(&row[3])),
        ("last_event", cell_value(&row[4])),
        ("agents", Value::Object(agents)),
    ])
}

// One tool call can produce several events (PreToolUse, PermissionRequest,
// PostToolUse): secrets are counted, and dismissed, per call.
macro_rules! call_sql {
    () => {
        "agent||'|'||session_id||'|'||COALESCE(json_extract(payload,'$.tool_use_id'), 'event-'||events.id)"
    };
}
const CALL: &str = call_sql!();
/// The text test first, so that only the few events that may hold a mask
/// are parsed (json_extract on every event was most of the overview's time).
const SECRET: &str = "((instr(payload,'\"secrets\"')>0 OR instr(payload,'[REDACTED]')>0 OR instr(payload,'[SECRET MASQU')>0) \
                      AND (json_extract(payload,'$.secrets') IS NOT NULL OR instr(json_extract(payload,'$.arguments'),'[REDACTED]')>0 OR instr(payload,'[SECRET MASQU')>0))";

/// Copies of the scope parameters, for queries that add their own.
fn params_copy<'a>(params: &[Param<'a>]) -> Vec<Param<'a>> {
    params
        .iter()
        .map(|p| match p {
            Param::Text(s) => Param::Text(s),
            Param::Int(n) => Param::Int(*n),
        })
        .collect()
}

/// Dismiss the secret alert for the calls in scope recorded up to event
/// `through` (the last one the dashboard showed, so that a secret arriving
/// meanwhile still raises the alert). Returns the batch number, for undo.
pub fn dismiss_secrets(data_dir: &Path, f: &Filter, through: i64) -> Result<i64, Error> {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    scope(f, &mut clauses, &mut params);
    clauses.push(SECRET);
    clauses.push("id<=?");
    let con = connect(data_dir)?;
    let batch = con.query("SELECT COALESCE(MAX(batch),0)+1 FROM dismissed_secrets", &[])?.remove(0)[0].int().unwrap_or(1);
    let mut all = vec![Param::Int(batch)];
    all.extend(params_copy(&params));
    all.push(Param::Int(through));
    con.execute(
        &format!("INSERT OR IGNORE INTO dismissed_secrets(call,batch) SELECT DISTINCT {CALL}, ? FROM events{}", where_sql(&clauses)),
        &all,
    )?;
    Ok(batch)
}

/// Undo one dismissal.
pub fn restore_secrets(data_dir: &Path, batch: i64) -> Result<(), Error> {
    connect(data_dir)?.execute("DELETE FROM dismissed_secrets WHERE batch=?", &[Param::Int(batch)])?;
    Ok(())
}

/// Aggregates for the overview page, computed by SQLite over the selected
/// scope (agent, time range, project). `tz_minutes` is the viewer's offset
/// from UTC, so that days are the viewer's local days.
/// The map keeps the most recent MAP_EVENTS tool events of the period.
const MAP_EVENTS: i64 = 200_000;

pub fn overview(data_dir: &Path, f: &Filter, tz_minutes: i64, with_commands: bool) -> Result<Value, Error> {
    overview_with_map_limit(data_dir, f, tz_minutes, with_commands, MAP_EVENTS)
}

/// `overview`, with the map's limit as a parameter (for the tests).
#[doc(hidden)]
pub fn overview_with_map_limit(data_dir: &Path, f: &Filter, tz_minutes: i64, with_commands: bool, map_events: i64) -> Result<Value, Error> {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    scope(f, &mut clauses, &mut params);
    let filter = where_sql(&clauses);
    let and = |extra: &str| if clauses.is_empty() { format!(" WHERE {extra}") } else { format!("{filter} AND {extra}") };
    let con = connect(data_dir)?;

    // Every figure but the shell commands comes from one reading of the
    // period, in Rust: each event is read and parsed once (one SQL query per
    // figure read the whole journal again each time, 3 s for 150,000 events).
    let mut sum = Summary {
        dismissed: con.query("SELECT call FROM dismissed_secrets", &[])?.iter().filter_map(|r| r[0].text().map(String::from)).collect(),
        tz_minutes,
        ..Default::default()
    };
    let rules = crate::watch::load(data_dir);
    let mut watch = WatchCount::new(&rules.0);
    let mut map = crate::resources::MapBuilder::default();
    // The map: the most recent `map_events` tool events of the period.
    let mut cut_params = params_copy(&params);
    cut_params.push(Param::Int(map_events));
    let map_from = con
        .query(&format!("SELECT id FROM events{} ORDER BY id DESC LIMIT 1 OFFSET ?", and("tool!=''")), &cut_params)?
        .first()
        .and_then(|r| r[0].int());
    let map_truncated = map_from.is_some();
    let map_from = map_from.map_or(0, |id| id + 1);

    const BATCH: i64 = 5000;
    let sql = format!(
        "SELECT id, agent, session_id, event, tool, category, status, received_at, payload FROM events{} ORDER BY id LIMIT {BATCH}",
        and("id>?")
    );
    let mut after = 0;
    loop {
        let mut page = params_copy(&params);
        page.push(Param::Int(after));
        let rows = con.query(&sql, &page)?;
        for r in &rows {
            fn text(c: &Cell) -> &str {
                c.text().unwrap_or_default()
            }
            let row = crate::resources::Row {
                id: r[0].int().unwrap_or(0),
                agent: text(&r[1]),
                session: text(&r[2]),
                event: text(&r[3]),
                tool: text(&r[4]),
                category: text(&r[5]),
                status: text(&r[6]),
                at: text(&r[7]),
                payload: text(&r[8]),
            };
            after = row.id;
            let payload = crate::json::parse(row.payload).unwrap_or(Value::Null);
            sum.add(&row, &payload);
            if !row.tool.is_empty() && row.id >= map_from {
                map.add(&row, &payload);
                watch.add(&row, &payload);
            }
        }
        if (rows.len() as i64) < BATCH {
            break;
        }
    }

    // Shell commands of the period, for the checks that read command text
    // (risky commands, applications). Arguments are already redacted.
    // The activity page does not need them (`with_commands` = false).
    const MAX_COMMANDS: usize = 5000;
    let limit = if with_commands { MAX_COMMANDS } else { 0 };
    let commands: Vec<Value> = con
        .query(
            &format!(
                "SELECT received_at, agent, json_extract(payload,'$.arguments') FROM events{} ORDER BY id DESC LIMIT {limit}",
                and("event='PreToolUse' AND category='shell'")
            ),
            &params,
        )?
        .iter()
        .map(|r| {
            let args = r[2].text().and_then(|t| crate::json::parse(t).ok()).unwrap_or(Value::Null);
            obj![("at", cell_value(&r[0])), ("agent", cell_value(&r[1])), ("arguments", args)]
        })
        .collect();
    let truncated = with_commands && commands.len() >= MAX_COMMANDS;
    let map = map.finish();
    let watch = watch.finish(data_dir, rules.1);
    let (totals, by_day, by_tool, projects, secret_kinds) = sum.finish();

    Ok(obj![
        ("totals", totals),
        ("map", map),
        ("map_truncated", map_truncated),
        ("by_day", by_day),
        ("by_tool", by_tool),
        ("projects", projects),
        ("secret_kinds", secret_kinds),
        ("commands", Value::Array(commands)),
        ("commands_truncated", truncated),
        ("watch", watch),
    ])
}

/// Your rules for the overview: each label with the number of tool calls
/// that matched it in the period (calls, not events: a call's PreToolUse and
/// PostToolUse count once), and the lines of the file that were skipped.
struct WatchCount<'r> {
    rules: &'r crate::watch::Rules,
    labels: Vec<(String, bool, Vec<i64>, HashSet<String>)>,
}

impl<'r> WatchCount<'r> {
    fn new(rules: &'r crate::watch::Rules) -> WatchCount<'r> {
        let mut labels: Vec<(String, bool, Vec<i64>, HashSet<String>)> = Vec::new();
        for r in &rules.rules {
            match labels.iter_mut().find(|l| l.0 == r.label) {
                Some(l) => {
                    l.1 |= r.alert;
                    l.2.push(r.line as i64);
                }
                None => labels.push((r.label.clone(), r.alert, vec![r.line as i64], HashSet::new())),
            }
        }
        WatchCount { rules, labels }
    }

    fn add(&mut self, row: &crate::resources::Row, payload: &Value) {
        if self.labels.is_empty() {
            return;
        }
        let args = payload.get("arguments").cloned().unwrap_or(Value::Null);
        let names = crate::resources::services_of(row.tool, row.category, &args).into_iter().map(|(n, _)| n).collect();
        let found = crate::watch::matches(self.rules, &crate::watch::Subject::of(row.tool, row.category, &args, names));
        if found.is_empty() {
            return;
        }
        let call = match payload.get("tool_use_id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => format!("{}|{}|{id}", row.agent, row.session),
            _ => format!("event-{}", row.id),
        };
        for m in found {
            if let Some(l) = self.labels.iter_mut().find(|l| l.0 == m.rule.label) {
                l.3.insert(call.clone());
            }
        }
    }

    fn finish(self, data_dir: &Path, exists: bool) -> Value {
        obj![
            ("path", data_dir.join(crate::watch::FILE_NAME).to_string_lossy().into_owned()),
            ("exists", exists),
            (
                "rules",
                Value::Array(
                    self.labels
                        .into_iter()
                        .map(|(label, alert, lines, calls)| obj![
                            ("label", label),
                            ("alert", alert),
                            ("lines", Value::Array(lines.into_iter().map(Value::int).collect())),
                            ("actions", Value::int(calls.len() as i64))
                        ])
                        .collect()
                )
            ),
            (
                "errors",
                Value::Array(
                    self.rules
                        .errors
                        .iter()
                        .map(|(line, message)| obj![("line", Value::int(*line as i64)), ("message", message.clone())])
                        .collect()
                )
            ),
        ]
    }
}

/// The overview's figures, added up one event at a time. Each figure keeps
/// the meaning it had as an SQL query (see FAILED, SECRET, CALL).
#[derive(Default)]
struct Summary {
    dismissed: HashSet<String>,
    tz_minutes: i64,
    events: i64,
    sessions: HashSet<String>,
    actions: i64,
    failed: i64,
    approvals: i64,
    /// Calls that carried a secret, and those whose alert was not dismissed.
    secret_calls: HashSet<String>,
    alert_calls: HashSet<String>,
    alert_through: Option<i64>,
    first: Option<String>,
    last: Option<String>,
    /// (local day, agent) → (actions, failed), in day order.
    days: BTreeMap<(Option<String>, String), (i64, i64)>,
    /// (tool, category) → (actions, failed).
    tools: HashMap<(String, String), (i64, i64)>,
    /// (kind of secret, agent) → (calls, last time), alert calls only.
    kinds: HashMap<(String, String), (HashSet<String>, String)>,
    /// Secrets masked before kinds were recorded (0.5), per agent.
    unknown: BTreeMap<String, (HashSet<String>, String)>,
    /// Working folder → (actions, sessions).
    projects: HashMap<String, (i64, HashSet<String>)>,
}

impl Summary {
    fn add(&mut self, r: &crate::resources::Row, payload: &Value) {
        let session = format!("{}|{}", r.agent, r.session);
        let action = r.event == "PreToolUse";
        let failed = r.event == "PostToolUseFailure" || (r.event == "PostToolUse" && r.status == "error") || r.event == "PermissionDenied";
        self.events += 1;
        self.actions += action as i64;
        self.failed += failed as i64;
        self.approvals += (r.event == "PermissionRequest") as i64;
        if self.first.as_deref().is_none_or(|f| r.at < f) {
            self.first = Some(r.at.to_string());
        }
        if self.last.as_deref().is_none_or(|l| r.at > l) {
            self.last = Some(r.at.to_string());
        }
        let day = self.days.entry((crate::util::local_day(r.at, self.tz_minutes), r.agent.to_string())).or_default();
        day.0 += action as i64;
        day.1 += failed as i64;
        if !r.tool.is_empty() {
            let t = self.tools.entry((r.tool.to_string(), r.category.to_string())).or_default();
            t.0 += action as i64;
            t.1 += failed as i64;
        }
        if let Some(Value::String(cwd)) = payload.get("cwd") {
            let p = self.projects.entry(cwd.clone()).or_default();
            p.0 += action as i64;
            p.1.insert(session.clone());
        }
        self.sessions.insert(session);

        // SECRET: a list of kinds, a mask in the arguments, or the old French mask.
        let raw = r.payload;
        if !(raw.contains("\"secrets\"") || raw.contains("[REDACTED]") || raw.contains("[SECRET MASQU")) {
            return;
        }
        let kinds = payload.get("secrets").filter(|v| !matches!(v, Value::Null));
        let secret = kinds.is_some()
            || raw.contains("[SECRET MASQU")
            || payload.get("arguments").is_some_and(|a| a.to_compact().contains("[REDACTED]"));
        let call = match payload.get("tool_use_id") {
            Some(Value::String(id)) => format!("{}|{}|{id}", r.agent, r.session),
            Some(Value::Null) | None => format!("{}|{}|event-{}", r.agent, r.session, r.id),
            Some(other) => format!("{}|{}|{}", r.agent, r.session, other.to_compact()),
        };
        let open = !self.dismissed.contains(&call);
        if secret {
            self.secret_calls.insert(call.clone());
            if open {
                self.alert_calls.insert(call.clone());
                self.alert_through = self.alert_through.max(Some(r.id));
            }
        }
        if !open {
            return;
        }
        let note = |entry: &mut (HashSet<String>, String)| {
            entry.0.insert(call.clone());
            if r.at > entry.1.as_str() {
                entry.1 = r.at.to_string();
            }
        };
        match kinds {
            Some(Value::Array(list)) => {
                for kind in list.iter().filter_map(Value::as_str) {
                    note(self.kinds.entry((kind.to_string(), r.agent.to_string())).or_default());
                }
            }
            Some(_) => {}
            None if secret => note(self.unknown.entry(r.agent.to_string()).or_default()),
            None => {}
        }
    }

    /// (totals, by_day, by_tool, projects, secret_kinds), as the dashboard reads them.
    fn finish(self) -> (Value, Value, Value, Value, Value) {
        let int = |n: i64| Value::int(n);
        let text = |s: Option<String>| s.map(Value::String).unwrap_or(Value::Null);
        let totals = obj![
            ("events", int(self.events)),
            ("sessions", int(self.sessions.len() as i64)),
            ("actions", int(self.actions)),
            ("failed", int(self.failed)),
            ("approvals", int(self.approvals)),
            ("secrets", int(self.secret_calls.len() as i64)),
            // Secret alert: calls not dismissed, and the last event behind them.
            ("alert_secrets", int(self.alert_calls.len() as i64)),
            ("alert_through", self.alert_through.map_or(Value::Null, Value::int)),
            ("first", text(self.first)),
            ("last", text(self.last)),
        ];
        let by_day = self
            .days
            .into_iter()
            .map(|((day, agent), (n, failed))| obj![("day", text(day)), ("agent", agent), ("actions", int(n)), ("failed", int(failed))])
            .collect();
        let mut tools: Vec<_> = self.tools.into_iter().collect();
        tools.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
        let by_tool = tools
            .into_iter()
            .take(200)
            .map(|((tool, category), (n, failed))| {
                obj![("tool", tool), ("category", category), ("actions", int(n)), ("failed", int(failed))]
            })
            .collect();
        let mut projects: Vec<_> = self.projects.into_iter().collect();
        projects.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
        let projects = projects
            .into_iter()
            .take(20)
            .map(|(cwd, (n, sessions))| obj![("cwd", cwd), ("actions", int(n)), ("sessions", int(sessions.len() as i64))])
            .collect();
        let mut kinds: Vec<_> = self.kinds.into_iter().collect();
        kinds.sort_by(|a, b| b.1 .0.len().cmp(&a.1 .0.len()).then_with(|| a.0.cmp(&b.0)));
        let mut secret_kinds: Vec<Value> = kinds
            .into_iter()
            .take(50)
            .map(|((kind, agent), (calls, last))| {
                obj![("kind", kind), ("agent", agent), ("calls", int(calls.len() as i64)), ("last", last)]
            })
            .collect();
        for (agent, (calls, last)) in self.unknown {
            secret_kinds.push(obj![("kind", Value::Null), ("agent", agent), ("calls", int(calls.len() as i64)), ("last", last)]);
        }
        (totals, Value::Array(by_day), Value::Array(by_tool), Value::Array(projects), Value::Array(secret_kinds))
    }
}
