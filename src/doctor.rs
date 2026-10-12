//! `llca doctor`: the checkup, as a short report a person can read.
//! One line per part (each agent, the collector, the journal), then the
//! problems with the command that fixes each one. `--json` prints the same
//! facts for scripts.

use std::path::Path;

use crate::json::Value;
use crate::store::agent_name;
use crate::util;

/// Something to fix: what is wrong, and the command that fixes it.
pub struct Problem {
    pub what: String,
    pub fix: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Ok,
    Problem,
    Warning,
    Off,
}

struct Line {
    mark: Mark,
    name: String,
    text: String,
    notes: Vec<String>,
}

fn int(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_integer).unwrap_or(0)
}

fn text(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or_default()
}

/// 1234567 → "1,234,567".
fn thousands(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// A path under the home folder, written with `~`.
pub fn tilde(path: &str) -> String {
    let home = util::home();
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some(rest) if home.len() > 1 && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// "just now", "5 minutes ago", "3 hours ago", "2 days ago", or the date.
pub fn ago(iso: &str, now_micros: i64) -> String {
    let Some(then) = util::micros_from_iso(iso) else { return iso.to_string() };
    let seconds = (now_micros - then).max(0) / 1_000_000;
    match seconds {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} ago", plural(seconds / 60, "minute", "minutes")),
        3600..=86_399 => format!("{} ago", plural(seconds / 3600, "hour", "hours")),
        86_400..=2_591_999 => format!("{} ago", plural(seconds / 86_400, "day", "days")),
        _ => format!("on {}", &iso[..iso.len().min(10)]),
    }
}

/// Lines and problems from the report built by `cli` (the `--json` output).
/// `target` is how the user chose the configuration (`--global` or
/// `--project PATH`), reused in the suggested commands.
fn assess(report: &Value, target: &str, now: i64) -> (Vec<Line>, Vec<Problem>) {
    let mut lines = Vec::new();
    let mut problems = Vec::new();
    let journal = report.get("journal");
    let per_agent = journal.and_then(|j| j.get("agents"));
    let collector = report.get("collector");
    let any_installed = report.get("installed").and_then(Value::as_bool).unwrap_or(false);
    // Hooks of ours somewhere, even incomplete: they run the collector copy.
    let any_hooks = report.get("agents").and_then(Value::as_object).is_some_and(|a| {
        a.iter().any(|(_, s)| s.get("registered").and_then(Value::as_array).is_some_and(|r| !r.is_empty()))
    });

    // Agents.
    if let Some(agents) = report.get("agents").and_then(Value::as_object) {
        for (id, state) in agents.iter() {
            let name = agent_name(id).to_string();
            let flag = if id == "codex" { "codex" } else { "claude" };
            let fix = format!("llca init {target} --agent {flag}");
            let registered = state.get("registered").and_then(Value::as_array).map_or(0, Vec::len);
            let expected = state.get("expected").and_then(Value::as_array).map_or(0, Vec::len);
            let config = tilde(text(state.get("config_path")));
            let installed = state.get("installed").and_then(Value::as_bool).unwrap_or(false);
            let last = per_agent.and_then(|a| a.get(id)).map(|a| text(a.get("last_event")).to_string()).filter(|t| !t.is_empty());
            if installed {
                let mut notes = Vec::new();
                let text = match &last {
                    Some(t) => format!("Recording · last event {}", ago(t, now)),
                    None => {
                        notes.push(if id == "codex" {
                            "No event received yet. Codex runs hooks only once trusted: open /hooks in Codex, then start a new session.".to_string()
                        } else {
                            "No event received yet. Start a new Claude Code session and ask it to run pwd.".to_string()
                        });
                        "Hooks installed, nothing received yet".to_string()
                    }
                };
                notes.push(format!("{expected} hooks in {config}"));
                let mark = if last.is_some() { Mark::Ok } else { Mark::Warning };
                lines.push(Line { mark, name, text, notes });
            } else if registered > 0 {
                let (line, what) = if registered < expected {
                    (format!("Only {registered} of {expected} hooks installed"), format!("{name}: only {registered} of {expected} hooks are installed."))
                } else {
                    ("Hooks installed, collector missing".to_string(), format!("{name}: the hooks call a collector that is missing."))
                };
                lines.push(Line { mark: Mark::Problem, name, text: line, notes: vec![format!("in {config}")] });
                problems.push(Problem { what, fix: Some(fix) });
            } else {
                let notes = if any_installed { vec![format!("To record it too: {fix}")] } else { Vec::new() };
                lines.push(Line { mark: Mark::Off, name, text: "Not set up".into(), notes });
            }
        }
    }
    if !any_installed && !lines.iter().any(|l| l.mark == Mark::Problem) {
        problems.push(Problem { what: "No agent is being recorded.".into(), fix: Some(format!("llca init {target} --agent all")) });
    }

    // Collector copy.
    if let Some(c) = collector.filter(|c| c.get("exists").and_then(Value::as_bool).unwrap_or(false)) {
        let running = text(c.get("running"));
        let version = text(c.get("version"));
        if c.get("outdated").and_then(Value::as_bool).unwrap_or(false) {
            let shown = if version.is_empty() { "an unknown version".to_string() } else { version.to_string() };
            lines.push(Line {
                mark: Mark::Problem,
                name: "Collector".into(),
                text: format!("{shown}, older than this version ({running})"),
                notes: vec!["The hooks run this copy, with its older redaction rules.".into()],
            });
            if any_hooks {
                problems.push(Problem {
                    what: format!("The hooks run llca {shown}, older than this one ({running}): newer redaction rules do not apply yet."),
                    fix: Some(format!("llca init {target} --agent all")),
                });
            }
        } else {
            lines.push(Line { mark: Mark::Ok, name: "Collector".into(), text: format!("{version}, up to date"), notes: Vec::new() });
        }
    }

    // Journal.
    if let Some(j) = journal {
        let events = int(j.get("events"));
        let path = tilde(text(report.get("journal_path")));
        if events == 0 {
            let mark = if any_installed { Mark::Warning } else { Mark::Off };
            lines.push(Line { mark, name: "Journal".into(), text: "Empty".into(), notes: vec![path] });
        } else {
            let last = text(j.get("last_event"));
            lines.push(Line {
                mark: Mark::Ok,
                name: "Journal".into(),
                text: format!("{} in {} · last one {}", plural(events, "event", "events"), plural(int(j.get("sessions")), "session", "sessions"), ago(last, now)),
                notes: vec![path],
            });
        }
    }

    // Your rules (watch.rules).
    if let Some(w) = report.get("watch").filter(|w| w.get("exists").and_then(Value::as_bool).unwrap_or(false)) {
        let count = int(w.get("rules"));
        let path = tilde(text(w.get("path")));
        let errors = w.get("errors").and_then(Value::as_array).cloned().unwrap_or_default();
        if !errors.is_empty() {
            let mut notes: Vec<String> = errors.iter().map(|e| format!("Line {} skipped: {}", int(e.get("line")), text(e.get("message")))).collect();
            notes.push(path);
            lines.push(Line { mark: Mark::Warning, name: "Your rules".into(), text: format!("{}, {}", plural(count, "rule", "rules"), plural(errors.len() as i64, "line skipped", "lines skipped")), notes });
        } else if count > 0 {
            lines.push(Line { mark: Mark::Ok, name: "Your rules".into(), text: plural(count, "rule", "rules"), notes: vec![path] });
        } else {
            lines.push(Line { mark: Mark::Off, name: "Your rules".into(), text: "None yet".into(), notes: vec![format!("Actions to watch for go in {path}")] });
        }
    }

    // Events the collector could not record.
    if let Some(errors) = report.get("collector_errors").filter(|e| e.is_truthy()) {
        let count = int(errors.get("count"));
        let last = text(errors.get("last"));
        let mut text_line = format!("{} not recorded", plural(count, "event", "events"));
        if !last.is_empty() {
            text_line.push_str(&format!(" · last one {}", ago(last, now)));
        }
        lines.push(Line { mark: Mark::Warning, name: "Errors".into(), text: text_line, notes: vec![format!("Log (no event content kept): {}", tilde(text(errors.get("path"))))] });
    }
    (lines, problems)
}

/// The problems, for the `--json` output.
pub fn problems(report: &Value, target: &str) -> Vec<Problem> {
    assess(report, target, util::now_micros()).1
}

/// The readable report. Returns the text and the number of problems.
pub fn render(report: &Value, target: &str, color: bool) -> (String, usize) {
    let (lines, problems) = assess(report, target, util::now_micros());
    let paint = |code: &str, s: &str| if color { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() };
    let symbol = |m: Mark| match m {
        Mark::Ok => paint("32", "✓"),
        Mark::Problem => paint("31", "✗"),
        Mark::Warning => paint("33", "!"),
        Mark::Off => paint("2", "○"),
    };
    let width = lines.iter().map(|l| l.name.chars().count()).max().unwrap_or(0) + 2;
    let mut out = format!("{}\n\n", paint("1", &format!("llca {} · checkup", crate::cli::VERSION)));
    for l in &lines {
        let name = format!("{:width$}", l.name);
        let body = if l.mark == Mark::Problem { paint("31", &l.text) } else { l.text.clone() };
        out.push_str(&format!("  {} {}{}\n", symbol(l.mark), paint("1", &name), body));
        for n in &l.notes {
            out.push_str(&format!("    {:width$}{}\n", "", paint("2", n)));
        }
    }
    out.push('\n');
    if problems.is_empty() {
        let warnings = lines.iter().any(|l| l.mark == Mark::Warning);
        out.push_str(&if warnings { paint("33", "Nothing to fix, but check the lines marked !") } else { paint("32", "All good.") });
        out.push('\n');
    } else {
        let title = if problems.len() == 1 { "1 problem to fix:".to_string() } else { format!("{} problems to fix:", problems.len()) };
        out.push_str(&paint("1;31", &title));
        out.push('\n');
        for p in &problems {
            out.push_str(&format!("  {} {}\n", paint("31", "✗"), p.what));
            if let Some(fix) = &p.fix {
                out.push_str(&format!("    Fix: {}\n", paint("1", fix)));
            }
        }
    }
    out.push_str(&paint("2", "\nAll details: llca doctor --json\n"));
    (out, problems.len())
}

/// Count and last time of the events the collector could not record, from
/// its error log (one line per failure: time, kind; never the content).
pub fn collector_errors(data_dir: &Path) -> Value {
    let path = data_dir.join("collector-errors.log");
    let Ok(log) = std::fs::read_to_string(&path) else { return Value::Bool(false) };
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Value::Bool(false);
    }
    let last = lines.last().and_then(|l| l.split_whitespace().next()).unwrap_or_default();
    crate::obj![("count", Value::int(lines.len() as i64)), ("last", last), ("path", path.to_string_lossy().into_owned())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_numbers_and_times() {
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(12), "12");
        let now = util::micros_from_iso("2026-10-11T12:00:00.000+00:00").unwrap();
        assert_eq!(ago("2026-10-11T11:59:30.000+00:00", now), "just now");
        assert_eq!(ago("2026-10-11T11:55:00.000+00:00", now), "5 minutes ago");
        assert_eq!(ago("2026-10-11T09:00:00.000+00:00", now), "3 hours ago");
        assert_eq!(ago("2026-10-10T12:00:00.000+00:00", now), "1 day ago");
        assert_eq!(ago("2026-08-01T12:00:00.000+00:00", now), "on 2026-08-01");
    }
}
