//! Your rules: `watch.rules`, a file of your own next to the journal, that
//! marks the actions you want to notice ("Touched production", "Sent an
//! email"). The dashboard shows them under "Worth a look".
//!
//! These rules only READ the journal, when the dashboard shows it: they never
//! block an agent, never change what is recorded, and apply to the whole
//! history as soon as the file is saved. (Masking secrets is a different
//! job, done before anything is written: see rules/redaction.rules.)
//!
//! One rule per line: `[alert] KIND PATTERN   # Label`
//!   file     a file path the agent read or edited, or named in a command
//!   command  a shell command, as recorded (secrets already masked)
//!   tool     a tool name, such as mcp__server__tool
//!   service  an online service, by its name in the dashboard
//! In a pattern, `*` matches anything and case is ignored; the pattern may
//! match any part of the text. `~/` stands for the home folder.

use std::path::Path;

use crate::json::Value;
use crate::obj;

pub const FILE_NAME: &str = "watch.rules";

/// What a new watch.rules file says, written by `init` when there is none.
pub const TEMPLATE: &str = "\
# llca: your rules. Actions that match are shown under \"Worth a look\".
# They never block an agent and never change the journal; they apply to your
# whole history as soon as you save this file.
#
# One rule per line:   [alert] KIND PATTERN   # Label shown in the dashboard
#   file     a file path the agent read or edited, or named in a command
#   command  a shell command, as recorded (secrets already masked)
#   tool     a tool name, such as mcp__server__tool
#   service  an online service, by its name in the dashboard (Google Cloud…)
# `alert` shows the rule in red. In a pattern, * matches anything and case is
# ignored. Rules with the same label count as one.
#
# Examples (remove the # to use them):
# file      .env                          # Touched a .env file
# file      ~/.ssh/                       # Touched SSH keys
# alert command kubectl * --context prod  # Touched production
# tool      mcp__*gmail*__send*           # Sent an email
# service   Stripe                        # Used Stripe
";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    File,
    Command,
    Tool,
    Service,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Command => "command",
            Kind::Tool => "tool",
            Kind::Service => "service",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub line: usize,
    pub alert: bool,
    pub kind: Kind,
    /// The pattern as written (shown in the details).
    pub pattern: String,
    /// Lower-case pieces between the `*`.
    pieces: Vec<String>,
    pub label: String,
}

#[derive(Debug, Default)]
pub struct Rules {
    pub rules: Vec<Rule>,
    /// Lines that were skipped, with the reason.
    pub errors: Vec<(usize, String)>,
}

/// `~/` at the start of a pattern stands for the home folder.
fn expand_home(pattern: &str) -> String {
    match pattern.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", crate::util::home().to_string_lossy().trim_end_matches('/')),
        None => pattern.to_string(),
    }
}

pub fn parse(source: &str) -> Rules {
    let mut out = Rules::default();
    for (n, raw) in source.lines().enumerate() {
        let line = n + 1;
        let (body, label) = match raw.split_once(" #").or_else(|| raw.split_once("\t#")) {
            Some((b, l)) => (b.trim(), l.trim()),
            None => (raw.trim(), ""),
        };
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let mut words = body.split_whitespace();
        let mut first = words.next().unwrap_or_default();
        let alert = first.eq_ignore_ascii_case("alert");
        if alert {
            first = words.next().unwrap_or_default();
        }
        let kind = match first.to_ascii_lowercase().as_str() {
            "file" => Kind::File,
            "command" => Kind::Command,
            "tool" => Kind::Tool,
            "service" => Kind::Service,
            "" => {
                out.errors.push((line, "a kind is missing (file, command, tool or service)".into()));
                continue;
            }
            other => {
                out.errors.push((line, format!("unknown kind \u{201c}{other}\u{201d}: use file, command, tool or service")));
                continue;
            }
        };
        let pattern = words.collect::<Vec<_>>().join(" ");
        if pattern.trim_matches('*').trim().is_empty() {
            out.errors.push((line, "the pattern is missing or matches everything".into()));
            continue;
        }
        let expanded = if kind == Kind::File { expand_home(&pattern) } else { pattern.clone() };
        let pieces = expanded.to_lowercase().split('*').map(str::to_string).collect();
        let label = if label.is_empty() { format!("{} {pattern}", kind.name()) } else { label.chars().take(80).collect() };
        out.rules.push(Rule { line, alert, kind, pattern, pieces, label });
    }
    out
}

/// The rules in `data_dir/watch.rules`, and whether the file exists.
pub fn load(data_dir: &Path) -> (Rules, bool) {
    match std::fs::read_to_string(data_dir.join(FILE_NAME)) {
        Ok(text) => (parse(&text), true),
        Err(_) => (Rules::default(), false),
    }
}

/// Create watch.rules with its explanations, if there is none. Never overwrites.
pub fn create_if_missing(data_dir: &Path) -> std::io::Result<bool> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    crate::util::private_dir(data_dir)?;
    match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(data_dir.join(FILE_NAME)) {
        Ok(mut f) => f.write_all(TEMPLATE.as_bytes()).map(|_| true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

impl Rule {
    /// True when the pieces appear in this order in `text` (case ignored).
    fn found_in(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        let mut at = 0;
        for piece in &self.pieces {
            match text[at..].find(piece.as_str()) {
                Some(i) => at += i + piece.len(),
                None => return false,
            }
        }
        true
    }
}

/// What one call offers to the rules: its file paths, its command, its tool,
/// its services. Built from the recorded (already redacted) arguments.
pub struct Subject<'a> {
    pub tool: &'a str,
    pub command: String,
    pub files: Vec<String>,
    pub services: Vec<String>,
}

impl<'a> Subject<'a> {
    pub fn of(tool: &'a str, category: &str, args: &Value, services: Vec<String>) -> Subject<'a> {
        let command = if category == "shell" {
            crate::resources::command_text(args.get("command")) + &crate::resources::command_text(args.get("cmd"))
        } else {
            String::new()
        };
        let mut files = Vec::new();
        for key in ["file_path", "path", "notebook_path"] {
            if let Some(p) = args.get(key).and_then(Value::as_str) {
                files.push(p.to_string());
            }
        }
        if let Some(list) = args.get("files").and_then(Value::as_array) {
            files.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
        }
        Subject { tool, command, files, services }
    }
}

/// One rule that matched, and on what.
pub struct Match<'r> {
    pub rule: &'r Rule,
    pub matched: String,
}

pub fn matches<'r>(rules: &'r Rules, s: &Subject) -> Vec<Match<'r>> {
    let mut out: Vec<Match> = Vec::new();
    for rule in &rules.rules {
        let hit = match rule.kind {
            // A path the tool names, or a path written in the command.
            Kind::File => {
                s.files.iter().find(|f| rule.found_in(f)).cloned().or_else(|| rule.found_in(&s.command).then(|| s.command.clone()))
            }
            Kind::Command => rule.found_in(&s.command).then(|| s.command.clone()),
            Kind::Tool => rule.found_in(s.tool).then(|| s.tool.to_string()),
            // A service is named in full ("Stripe"), unless the pattern has a *.
            Kind::Service => s
                .services
                .iter()
                .find(|name| if rule.pattern.contains('*') { rule.found_in(name) } else { name.eq_ignore_ascii_case(rule.pattern.trim()) })
                .cloned(),
        };
        if let Some(matched) = hit {
            // One entry per label: rules sharing a label count as one.
            if !out.iter().any(|m| m.rule.label == rule.label) {
                out.push(Match { rule, matched: matched.chars().take(200).collect() });
            }
        }
    }
    out
}

/// The matches as JSON, for one event.
pub fn to_json(found: &[Match]) -> Value {
    Value::Array(
        found
            .iter()
            .map(|m| {
                obj![
                    ("label", m.rule.label.clone()),
                    ("alert", m.rule.alert),
                    ("line", Value::int(m.rule.line as i64)),
                    ("rule", format!("{} {}", m.rule.kind.name(), m.rule.pattern)),
                    ("matched", m.matched.clone())
                ]
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject<'a>(tool: &'a str, command: &str, files: &[&str], services: &[&str]) -> Subject<'a> {
        Subject {
            tool,
            command: command.into(),
            files: files.iter().map(|f| f.to_string()).collect(),
            services: services.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn parses_rules_labels_and_mistakes() {
        let r = parse("# comment\n\nfile .env  # Touched a .env file\nalert command kubectl * --context prod # Touched production\ncomand x # typo\nfile * # all\ntool mcp__*gmail*__send*\n");
        assert_eq!(r.rules.len(), 3);
        assert_eq!((r.rules[0].line, r.rules[0].label.as_str(), r.rules[0].alert), (3, "Touched a .env file", false));
        assert!(r.rules[1].alert);
        assert_eq!(r.rules[2].label, "tool mcp__*gmail*__send*");
        assert_eq!(r.errors.len(), 2);
        assert_eq!(r.errors[0].0, 5);
        assert!(r.errors[0].1.contains("unknown kind"));
        assert!(r.errors[1].1.contains("matches everything"));
        assert!(parse(TEMPLATE).rules.is_empty() && parse(TEMPLATE).errors.is_empty());
    }

    #[test]
    fn matches_files_commands_tools_and_services() {
        let r = parse(
            "file .env # Env\nalert command kubectl * --context prod # Prod\ncommand gcloud * --project shop-prod # Prod\ntool mcp__*gmail*__send* # Mail\nservice stripe # Stripe\nfile ~/.ssh/ # SSH",
        );
        let labels = |s: &Subject| matches(&r, s).iter().map(|m| m.rule.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&subject("Read", "", &["/p/.env.local"], &[])), ["Env"]);
        assert_eq!(labels(&subject("Bash", "cp .env.example .env", &[], &[])), ["Env"]);
        assert_eq!(labels(&subject("Bash", "KUBECTL apply -f x.yaml --context prod", &[], &[])), ["Prod"]);
        // Two rules with the same label: one entry.
        assert_eq!(
            labels(&subject("Bash", "gcloud run deploy --project shop-prod && kubectl get pods --context prod", &[], &["Google Cloud"])),
            ["Prod"]
        );
        assert!(labels(&subject("Bash", "kubectl get pods --context staging", &[], &[])).is_empty());
        assert_eq!(labels(&subject("mcp__claude_ai_Gmail__send_message", "", &[], &["Gmail"])), ["Mail"]);
        assert!(labels(&subject("mcp__claude_ai_Gmail__search_threads", "", &[], &["Gmail"])).is_empty());
        assert_eq!(labels(&subject("Bash", "stripe listen", &[], &["Stripe"])), ["Stripe"]);
        let home = crate::util::home().to_string_lossy().trim_end_matches('/').to_string();
        assert_eq!(labels(&subject("Read", "", &[&format!("{home}/.ssh/id_ed25519")], &[])), ["SSH"]);
        let m = matches(&r, &subject("Read", "", &["/p/.env.local"], &[]));
        assert_eq!(m[0].matched, "/p/.env.local");
    }
}
