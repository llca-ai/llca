//! "Where the agents went": the applications, projects, online services and
//! websites each agent reached, from what the journal records. Nothing is
//! guessed: a resource appears only when a tool call shows it (a command, a
//! URL, a connected app's tool name, the session's folder).
//!
//! Service names come from `rules/services.rules`, compiled into the binary.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::OnceLock;

use crate::json::{Map, Value};
use crate::obj;

pub const SOURCE: &str = include_str!("../rules/services.rules");

#[derive(Debug, Default)]
pub struct Services {
    pub cli: Vec<(String, String)>,
    pub mcp: Vec<(String, String)>,
    pub domains: Vec<(String, String)>,
    /// (Simple Icons slug, service name): the logos in static/brands.js.
    pub icons: Vec<(String, String)>,
}

/// Parse a services file. Errors name the line, for contributors.
pub fn parse(source: &str) -> Result<Services, String> {
    let mut s = Services::default();
    for (n, raw) in source.lines().enumerate() {
        let line = raw.split(" #").next().unwrap_or_default().trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fail = |e: &str| format!("line {}: {e}: {raw}", n + 1);
        let mut parts = line.splitn(3, char::is_whitespace);
        let (Some(kind), Some(key), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(fail("expected: KIND KEY NAME"));
        };
        let (key, name) = (key.to_lowercase(), name.trim().to_string());
        if name.is_empty() {
            return Err(fail("missing NAME"));
        }
        let list = match kind {
            "cli" => &mut s.cli,
            "mcp" => &mut s.mcp,
            "domain" => &mut s.domains,
            "icon" => &mut s.icons,
            other => return Err(fail(&format!("unknown rule {other:?}"))),
        };
        if list.iter().any(|(k, _)| *k == key) {
            return Err(fail("duplicate"));
        }
        list.push((key, name));
    }
    // Longest prefixes and domains first, so the most specific rule wins.
    s.mcp.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    s.domains.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    Ok(s)
}

pub fn services() -> &'static Services {
    static S: OnceLock<Services> = OnceLock::new();
    // Checked by the test suite, so this cannot fail in a release.
    S.get_or_init(|| parse(SOURCE).expect("rules/services.rules is invalid"))
}

// ---------- Reading commands ----------

/// The text of a command argument: a string, or argv (`["bash", "-lc", "…"]`).
pub fn command_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let parts: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
            if parts.len() == 3 && (parts[1] == "-c" || parts[1] == "-lc") {
                parts[2].to_string()
            } else {
                parts.join(" ")
            }
        }
        _ => String::new(),
    }
}

/// Applications on this Mac named by a command: `/Applications/X.app`,
/// `open -a X`, AppleScript `tell application "X"`. Same rules as the
/// dashboard's application chips.
pub fn apps_in(command: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut add = |n: &str| {
        let n = n.trim();
        if !n.is_empty() && !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    };
    for (i, _) in command.match_indices("/Applications/") {
        let mut rest = &command[i + "/Applications/".len()..];
        rest = rest.strip_prefix("Utilities/").unwrap_or(rest);
        for (j, c) in rest.char_indices() {
            if "/'\"\n".contains(c) {
                break;
            }
            if rest[j..].starts_with(".app") {
                let after = rest[j + 4..].chars().next();
                if j > 0 && after.is_none_or(|a| "/'\"".contains(a) || a.is_whitespace()) {
                    add(&rest[..j]);
                    break;
                }
            }
        }
    }
    // open [-flags] -a NAME
    let words = shell_words(command);
    for (i, w) in words.iter().enumerate() {
        if w != "open" {
            continue;
        }
        let mut k = i + 1;
        while k < words.len() && words[k].starts_with('-') && words[k].len() > 1 && words[k][1..].chars().all(|c| c.is_ascii_alphabetic()) {
            if words[k].ends_with('a') {
                if let Some(app) = words.get(k + 1) {
                    let last = app.trim_end_matches('/').rsplit('/').next().unwrap_or(app);
                    add(last.strip_suffix(".app").unwrap_or(last));
                }
                break;
            }
            k += 1;
        }
    }
    // tell application "Mail". The search ignores ASCII case on the command
    // itself: positions found in a lowercased copy would not match the
    // original, since some characters change length in lower case (İ).
    let bytes = command.as_bytes();
    let starts = (0..bytes.len().saturating_sub(4)).filter(|&i| bytes[i..i + 5].eq_ignore_ascii_case(b"tell "));
    for i in starts {
        let rest = &command[i + 5..];
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("application").or_else(|| rest.strip_prefix("app")).unwrap_or("");
        let rest = rest.trim_start().trim_start_matches('\\');
        if let Some(body) = rest.strip_prefix('"') {
            if let Some(end) = body.find(['"', '\\']) {
                add(&body[..end]);
            }
        }
    }
    names
}

/// Words of a command, with quotes removed (a light shell tokenizer).
fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() || ";&|()`".contains(c) => {
                if !word.is_empty() || any {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
                if !c.is_whitespace() {
                    words.push(c.to_string());
                }
            }
            None => word.push(c),
        }
    }
    if !word.is_empty() || any {
        words.push(word);
    }
    words
}

/// Programs a command runs: the first word of each part (after `&&`, `;`,
/// `|`, `$(`), past `VAR=value`, `sudo`, `env`, `npx`…, without its path.
pub fn programs_in(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at_start = true;
    for w in shell_words(command) {
        if [";", "&", "|", "(", ")", "`"].contains(&w.as_str()) || w == "$" || w.ends_with("$(") {
            at_start = true;
            continue;
        }
        if !at_start {
            continue;
        }
        let w = w.trim_start_matches("$(").to_string();
        let is_assignment = w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if w.is_empty() || is_assignment || ["sudo", "env", "command", "exec", "time", "nohup", "npx", "bunx", "xargs"].contains(&w.as_str()) {
            continue;
        }
        let program = w.rsplit('/').next().unwrap_or(&w).to_string();
        if !out.contains(&program) {
            out.push(program);
        }
        at_start = false;
    }
    out
}

/// Host names of the URLs in a text (`https://…`, `git@host:`), lower case,
/// without `www.`; local addresses are left out.
pub fn hosts_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |authority: &str| {
        let host = authority.rsplit('@').next().unwrap_or(authority);
        let host = host.split(':').next().unwrap_or(host).trim_end_matches('.').to_lowercase();
        let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
        let local = ["localhost", "127.0.0.1", "0.0.0.0"].contains(&host.as_str()) || host.ends_with(".local");
        let plausible = host.contains('.') && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c));
        if plausible && !local && !out.contains(&host) {
            out.push(host);
        }
    };
    let end = |s: &str| s.find(|c: char| c.is_whitespace() || "/?#'\"\\)>,;`".contains(c)).unwrap_or(s.len());
    for scheme in ["https://", "http://"] {
        for (i, _) in text.match_indices(scheme) {
            let rest = &text[i + scheme.len()..];
            push(&rest[..end(rest)]);
        }
    }
    for (i, _) in text.match_indices("git@") {
        let rest = &text[i + 4..];
        if let Some(colon) = rest.find(':') {
            if colon < end(rest) {
                push(&rest[..colon]);
            }
        }
    }
    out
}

fn title_case(s: &str) -> String {
    let words: Vec<String> = s
        .split(['_', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect();
    words.join(" ")
}

fn is_computer_use(tool: &str) -> bool {
    let t = tool.to_lowercase();
    ["mcp__cua__", "mcp__cua_repl__", "mcp__computer_use__", "mcp__computer-use__", "mcp__computeruse__"].iter().any(|p| t.starts_with(p))
        || ["mcp__cua", "mcp__cua_repl", "mcp__computer_use"].contains(&t.as_str())
}

/// A resource one tool call reached.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub category: &'static str, // app, project, service, web
    pub name: String,
    pub via: String,
    /// How the activity list finds these calls: a search term, or a project.
    pub search: Option<String>,
    pub project: Option<String>,
}

fn found(category: &'static str, name: impl Into<String>, via: impl Into<String>, search: Option<String>) -> Found {
    Found { category, name: name.into(), via: via.into(), search, project: None }
}

/// Everything one call reached. `args` are the recorded (redacted) arguments.
pub fn reached(tool: &str, category: &str, args: &Value, cwd: Option<&str>) -> Vec<Found> {
    let s = services();
    let mut out: Vec<Found> = Vec::new();
    let mut add = |f: Found| {
        if !out.iter().any(|o| o.category == f.category && o.name == f.name) {
            out.push(f);
        }
    };
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        let name = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(cwd);
        add(Found { category: "project", name: name.to_string(), via: cwd.to_string(), search: None, project: Some(cwd.to_string()) });
    }
    let by_domain = |host: &str| s.domains.iter().find(|(d, _)| host == d || host.ends_with(&format!(".{d}"))).map(|(_, n)| n.clone());
    let url_found = |text: &str, add: &mut dyn FnMut(Found)| {
        for host in hosts_in(text) {
            match by_domain(&host) {
                Some(name) => add(found("service", name, format!("URL ({host})"), Some(host.clone()))),
                None => add(found("web", host.clone(), "Website", Some(host.clone()))),
            }
        }
    };
    if is_computer_use(tool) {
        add(found("app", "Computer Use", "App not reported", Some(tool.to_string())));
    } else if let Some(rest) = tool.strip_prefix("mcp__") {
        let lower = rest.to_lowercase();
        let server = rest.split("__").next().unwrap_or(rest);
        // Connectors of the Claude and ChatGPT apps carry a prefix: mcp__claude_ai_Gmail__…
        let bare = ["claude_ai_", "codex_apps__"].iter().find_map(|p| server.strip_prefix(p)).filter(|b| !b.is_empty()).unwrap_or(server);
        // A rule matches the full name, or the name without claude_ai_.
        let unprefixed = lower.strip_prefix("claude_ai_").unwrap_or(&lower).to_string();
        let name = s.mcp.iter().find(|(p, _)| [&lower, &unprefixed].iter().any(|l| **l == *p || l.starts_with(&format!("{p}__")))).map(|(_, n)| n.clone());
        add(found("service", name.unwrap_or_else(|| title_case(bare)), "Connected app (MCP)", Some(format!("mcp__{server}"))));
    } else if category == "shell" {
        let text = command_text(args.get("command")).to_string() + &command_text(args.get("cmd"));
        for app in apps_in(&text) {
            add(found("app", app.clone(), "Application", Some(app)));
        }
        for program in programs_in(&text) {
            if let Some((_, name)) = s.cli.iter().find(|(c, _)| *c == program.to_lowercase()) {
                add(found("service", name.clone(), format!("Command line ({program})"), Some(program.clone())));
            }
        }
        url_found(&text, &mut add);
    } else if tool == "WebFetch" || tool == "web_fetch" {
        if let Some(url) = args.get("url").and_then(Value::as_str) {
            url_found(url, &mut add);
        }
    }
    out
}

/// The online services one call reached, with how (`Command line (gcloud)`,
/// `Connected app (MCP)`, `URL (api.stripe.com)`). Same rules as the map.
pub fn services_of(tool: &str, category: &str, args: &Value) -> Vec<(String, String)> {
    reached(tool, category, args, None).into_iter().filter(|f| f.category == "service").map(|f| (f.name, f.via)).collect()
}

// ---------- The map ----------

#[derive(Default)]
struct Call {
    agent: String,
    session: String,
    tool: String,
    category: String,
    cwd: Option<String>,
    args: Option<Value>,
    at: String,
    asked: bool,
    failed: bool,
    ok: bool,
    secret: bool,
    example: String,
}

#[derive(Default)]
struct Resource {
    category: &'static str,
    name: String,
    vias: Vec<String>,
    search: Option<String>,
    project: Option<String>,
    agents: BTreeMap<String, i64>,
    sessions: BTreeSet<String>,
    failed: i64,
    ok: i64,
    asked: i64,
    secrets: i64,
    last: String,
    examples: Vec<String>,
}

/// One row of the journal, as read for the map.
pub struct Row<'a> {
    pub id: i64,
    pub agent: &'a str,
    pub session: &'a str,
    pub event: &'a str,
    pub tool: &'a str,
    pub category: &'a str,
    pub status: &'a str,
    pub at: &'a str,
    pub payload: &'a str,
}

fn example_of(tool: &str, category: &str, args: &Value) -> String {
    let text = if category == "shell" {
        command_text(args.get("command")) + &command_text(args.get("cmd"))
    } else if let (Some(rest), false) = (tool.strip_prefix("mcp__"), is_computer_use(tool)) {
        // mcp__github__create_pull_request → "create pull request"
        rest.split("__").skip(1).collect::<Vec<_>>().join(" ").replace('_', " ")
    } else if let Some(url) = args.get("url").and_then(Value::as_str) {
        url.to_string()
    } else if let Some(title) = args.get("title").and_then(Value::as_str) {
        title.to_string()
    } else {
        tool.to_string()
    };
    let line = text.lines().next().unwrap_or_default().trim().to_string();
    if line.chars().count() > 140 { line.chars().take(139).collect::<String>() + "…" } else { line }
}

/// Build the map from the rows of the selected period, oldest first.
pub fn map(rows: &[Row]) -> Value {
    let mut b = MapBuilder::default();
    for r in rows {
        if let Ok(payload) = crate::json::parse(r.payload) {
            b.add(r, &payload);
        }
    }
    b.finish()
}

/// The map, built one row at a time (oldest first), from rows already
/// parsed: the overview reads each event once for all of its figures.
#[derive(Default)]
pub struct MapBuilder {
    calls: Vec<Call>,
    by_key: HashMap<String, usize>,
    last_call: HashMap<String, usize>,
}

impl MapBuilder {
    pub fn add(&mut self, r: &Row, payload: &Value) {
        let MapBuilder { calls, by_key, last_call } = self;
        if r.tool.is_empty() {
            return;
        }
        let tool_use_id = payload.get("tool_use_id").and_then(Value::as_str).unwrap_or_default();
        let turn = payload.get("turn_id").and_then(Value::as_str).unwrap_or_default();
        let group = format!("{}|{}|{turn}|{}", r.agent, r.session, r.tool);
        let index = if !tool_use_id.is_empty() {
            let key = format!("{}|{}|{tool_use_id}", r.agent, r.session);
            *by_key.entry(key).or_insert_with(|| {
                calls.push(Call::default());
                calls.len() - 1
            })
        } else if r.event == "PermissionRequest" && last_call.contains_key(&group) {
            last_call[&group]
        } else {
            calls.push(Call::default());
            calls.len() - 1
        };
        last_call.insert(group, index);
        let c = &mut calls[index];
        if c.agent.is_empty() {
            c.agent = r.agent.to_string();
            c.session = r.session.to_string();
            c.tool = r.tool.to_string();
            c.category = r.category.to_string();
            c.at = r.at.to_string();
        }
        if c.cwd.is_none() {
            c.cwd = payload.get("cwd").and_then(Value::as_str).map(String::from);
        }
        if let Some(args) = payload.get("arguments") {
            let empty = args.as_object().is_some_and(|m| m.is_empty());
            if !empty && (c.args.is_none() || r.event == "PreToolUse") {
                c.args = Some(args.clone());
            }
            // The raw text is checked first: serialising every call's
            // arguments again was a large part of the overview's time.
            if !c.secret && (r.payload.contains("[REDACTED]") || r.payload.contains("[SECRET MASQU")) {
                let text = args.to_compact();
                c.secret |= text.contains("[REDACTED]") || text.contains("[SECRET MASQU");
            }
        }
        c.secret |= payload.get("secrets").is_some();
        match r.event {
            "PermissionRequest" => c.asked = true,
            "PermissionDenied" | "PostToolUseFailure" => c.failed = true,
            "PostToolUse" if r.status == "error" => c.failed = true,
            "PostToolUse" if r.status == "success" => c.ok = true,
            _ => {}
        }
        let _ = r.id;
    }

    pub fn finish(self) -> Value {
        let mut calls = self.calls;
        let mut resources: BTreeMap<(String, String), Resource> = BTreeMap::new();
        let mut agents: BTreeMap<String, (i64, BTreeSet<String>)> = BTreeMap::new();
        for c in &mut calls {
            let args = c.args.take().unwrap_or_else(|| Value::Object(Map::new()));
            c.example = example_of(&c.tool, &c.category, &args);
            let a = agents.entry(c.agent.clone()).or_default();
            a.0 += 1;
            a.1.insert(c.session.clone());
            for f in reached(&c.tool, &c.category, &args, c.cwd.as_deref()) {
                let r = resources.entry((f.category.to_string(), f.name.clone())).or_insert_with(|| Resource {
                    category: f.category,
                    name: f.name.clone(),
                    search: f.search.clone(),
                    project: f.project.clone(),
                    ..Default::default()
                });
                if !r.vias.contains(&f.via) && r.vias.len() < 4 {
                    r.vias.push(f.via.clone());
                }
                *r.agents.entry(c.agent.clone()).or_default() += 1;
                r.sessions.insert(format!("{}|{}", c.agent, c.session));
                r.failed += c.failed as i64;
                r.ok += (c.ok && !c.failed) as i64;
                r.asked += c.asked as i64;
                r.secrets += c.secret as i64;
                if c.at > r.last {
                    r.last = c.at.clone();
                }
                // Most recent examples first (calls are in time order).
                if f.category != "project" && !c.example.is_empty() && !r.examples.contains(&c.example) {
                    r.examples.insert(0, c.example.clone());
                    r.examples.truncate(5);
                }
            }
        }

        let mut list: Vec<&Resource> = resources.values().collect();
        list.sort_by(|a, b| {
            let total = |r: &Resource| r.agents.values().sum::<i64>();
            total(b).cmp(&total(a)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let mut per_category: HashMap<&str, usize> = HashMap::new();
        let items: Vec<Value> = list
            .into_iter()
            .filter(|r| {
                let n = per_category.entry(r.category).or_default();
                *n += 1;
                *n <= 60
            })
            .map(|r| {
                let mut by_agent = Map::new();
                for (agent, n) in &r.agents {
                    by_agent.insert(agent.clone(), Value::int(*n));
                }
                obj![
                    ("category", r.category),
                    ("name", r.name.clone()),
                    ("via", Value::Array(r.vias.iter().map(Value::str).collect())),
                    ("search", r.search.clone().map(Value::String).unwrap_or(Value::Null)),
                    ("project", r.project.clone().map(Value::String).unwrap_or(Value::Null)),
                    ("agents", Value::Object(by_agent)),
                    ("actions", Value::int(r.agents.values().sum())),
                    ("sessions", Value::int(r.sessions.len() as i64)),
                    ("failed", Value::int(r.failed)),
                    ("ok", Value::int(r.ok)),
                    ("asked", Value::int(r.asked)),
                    ("secrets", Value::int(r.secrets)),
                    ("last", r.last.clone()),
                    ("examples", Value::Array(r.examples.iter().map(Value::str).collect())),
                ]
            })
            .collect();
        let mut agent_list = Map::new();
        for (agent, (n, sessions)) in agents {
            agent_list.insert(agent, obj![("actions", Value::int(n)), ("sessions", Value::int(sessions.len() as i64))]);
        }
        obj![("agents", Value::Object(agent_list)), ("resources", Value::Array(items))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_logo_belongs_to_a_service_and_is_generated() {
        let s = parse(SOURCE).unwrap();
        let brands = include_str!("../static/brands.js");
        let named: Vec<&String> = s.cli.iter().chain(&s.mcp).chain(&s.domains).map(|(_, n)| n).collect();
        for (slug, name) in &s.icons {
            assert!(named.contains(&name), "icon {slug}: no rule names the service {name:?}");
            assert!(brands.contains(&format!("\n  {}: [\"", crate::json::Value::str(name).to_compact())), "{name:?} is missing from static/brands.js: run scripts/brands.py");
        }
        let generated = brands.lines().filter(|l| l.starts_with("  \"")).count();
        assert_eq!(generated, s.icons.len(), "static/brands.js is out of date: run scripts/brands.py");
        // Claude's connectors match without their claude_ai_ prefix.
        let empty = Value::Object(Map::new());
        assert_eq!(services_of("mcp__claude_ai_Discord__send_message", "mcp", &empty)[0].0, "Discord");
    }

    #[test]
    fn the_shipped_services_parse() {
        let s = parse(SOURCE).unwrap();
        assert!(s.cli.iter().any(|(c, n)| c == "gcloud" && n == "Google Cloud"));
        assert!(parse("cli gcloud Google Cloud\ncli gcloud Other").unwrap_err().contains("line 2: duplicate"));
        assert!(parse("tool x Y").unwrap_err().contains("unknown rule"));
    }

    #[test]
    fn applications() {
        assert_eq!(apps_in("'/Applications/Arduino IDE.app/Contents/Resources/arduino-cli' version"), ["Arduino IDE"]);
        assert_eq!(apps_in("open -a \"Visual Studio Code\" ."), ["Visual Studio Code"]);
        assert_eq!(apps_in("open -na Safari https://x.example"), ["Safari"]);
        assert_eq!(apps_in("osascript -e 'tell application \"Mail\" to activate'"), ["Mail"]);
        assert_eq!(apps_in("ls /System/Applications/Utilities/Terminal.app"), ["Terminal"]);
        assert!(apps_in("open README.md && cat app.py").is_empty());
        // Characters that change length in lower case used to shift positions and panic.
        assert_eq!(apps_in("echo İİİ; osascript -e 'TELL application \"Mail\"'"), ["Mail"]);
        assert!(apps_in("İ tell é").is_empty());
        assert!(apps_in("\u{212A}\u{212A} tell application").is_empty());
    }

    #[test]
    fn programs_and_hosts() {
        assert_eq!(programs_in("cd infra && CLOUDSDK_CONFIG=/tmp/x gcloud projects list | head"), ["cd", "gcloud", "head"]);
        assert_eq!(programs_in("sudo /usr/local/bin/wrangler deploy; npx vercel --prod"), ["wrangler", "vercel"]);
        assert_eq!(programs_in("echo \"gcloud is great\""), ["echo"]);
        assert_eq!(hosts_in("git clone https://[REDACTED]@github.com/a/b.git && curl -s 'https://www.example.org/x?y=1' http://localhost:3000"), ["github.com", "example.org"]);
        assert_eq!(hosts_in("git clone git@gitlab.com:team/repo.git"), ["gitlab.com"]);
    }

    #[test]
    fn what_a_call_reached() {
        let cmd = crate::json::parse(r#"{"command": ["bash", "-lc", "gcloud auth list && curl https://api.github.com/repos && open -a Safari https://docs.example.dev/page"]}"#).unwrap();
        let found = reached("Bash", "shell", &cmd, Some("/Users/me/projects/infra"));
        let names: Vec<(&str, &str)> = found.iter().map(|f| (f.category, f.name.as_str())).collect();
        assert_eq!(names, [("project", "infra"), ("app", "Safari"), ("service", "Google Cloud"), ("service", "GitHub"), ("web", "docs.example.dev")]);
        let empty = Value::Object(Map::new());
        assert_eq!(reached("mcp__codex_apps__sites__save_version", "mcp", &empty, None)[0].name, "ChatGPT Sites");
        assert_eq!(reached("mcp__acme_crm__find", "mcp", &empty, None)[0].name, "Acme Crm");
        assert_eq!(reached("mcp__claude_ai_Gmail__search_threads", "mcp", &empty, None)[0].name, "Gmail");
        assert_eq!(reached("mcp__claude_ai_Acme_Notes__list", "mcp", &empty, None)[0].name, "Acme Notes");
        assert_eq!(services_of("Bash", "shell", &cmd), [("Google Cloud".to_string(), "Command line (gcloud)".to_string()), ("GitHub".to_string(), "URL (api.github.com)".to_string())]);
        assert_eq!(reached("mcp__cua_repl__js", "mcp", &empty, None)[0].name, "Computer Use");
        let fetch = crate::json::parse(r#"{"url": "https://developer.mozilla.org/en-US/docs"}"#).unwrap();
        assert_eq!(reached("WebFetch", "tool", &fetch, None)[0].name, "developer.mozilla.org");
    }
}
