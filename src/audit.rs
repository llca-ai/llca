//! `llca audit`: look for values in the journal that resemble secrets
//! but were kept, and suggest the rule that would mask them. Read-only.
//!
//! A candidate is a run of at least 20 token characters that looks random:
//! letters and digits interleaved, high character entropy. Identifiers that
//! are known not to be secrets (UUIDs, commit hashes, fields named *_id,
//! *_sha…) are skipped. Values are never printed in full.

use std::collections::BTreeMap;
use std::path::Path;

use crate::json::Value;
use crate::redact::redact;
use crate::store::{self, Error};

const MIN_LEN: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Context {
    /// `?name=VALUE` or `&name=VALUE` in a URL.
    UrlParam(String),
    /// `NAME=VALUE`, `NAME: VALUE`, `"NAME":"VALUE"` or `--NAME VALUE` in text.
    Key(String),
    /// The whole value of an argument field.
    Field(String),
    /// A value with a recognisable prefix, such as `art_v2_x_…`.
    Prefix(String),
    /// Anything else.
    Bare,
}

impl Context {
    fn title(&self) -> String {
        match self {
            Context::UrlParam(n) => format!("URL parameter \"{n}\""),
            Context::Key(n) => format!("After \"{n}\" in text"),
            Context::Field(n) => format!("Field \"{n}\""),
            Context::Prefix(p) => format!("Values starting \"{p}\""),
            Context::Bare => "Other random-looking values".into(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Finding {
    pub count: usize,
    pub sample: String,
    pub events: Vec<i64>,
    pub tools: Vec<String>,
    pub lengths: (usize, usize),
    pub chars: (bool, bool, bool, Vec<char>), // lower, upper, digit, others
}

#[derive(Debug, Default)]
pub struct Report {
    pub checked: usize,
    pub outdated: Vec<i64>,
    pub findings: BTreeMap<Context, Finding>,
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_-+%~".contains(c)
}

fn entropy(s: &[char]) -> f64 {
    let mut counts: BTreeMap<char, usize> = BTreeMap::new();
    for &c in s {
        *counts.entry(c).or_default() += 1;
    }
    let n = s.len() as f64;
    counts
        .values()
        .map(|&k| {
            let p = k as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Random-looking: long, letters and digits interleaved, high entropy.
pub fn looks_random(s: &[char]) -> bool {
    if s.len() < MIN_LEN {
        return false;
    }
    let letters = s.iter().filter(|c| c.is_ascii_alphabetic()).count();
    let digits = s.iter().filter(|c| c.is_ascii_digit()).count();
    let alnum: Vec<&char> = s.iter().filter(|c| c.is_ascii_alphanumeric()).collect();
    let switches = alnum.windows(2).filter(|w| w[0].is_ascii_digit() != w[1].is_ascii_digit()).count();
    // Hexadecimal has 16 symbols (4 bits at most per character), others more.
    let threshold = if is_hex(s) { 3.0 } else { 3.6 };
    letters >= 5 && digits >= 3 && switches * 8 >= s.len() && entropy(s) >= threshold
}

fn is_uuid(s: &[char]) -> bool {
    let parts: Vec<String> = s.iter().collect::<String>().split('-').map(String::from).collect();
    parts.iter().map(String::len).eq([8, 4, 4, 4, 12]) && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_hex(s: &[char]) -> bool {
    s.iter().all(|c| c.is_ascii_hexdigit())
}

/// Names of fields and parameters that hold identifiers, not secrets.
fn is_identifier_name(name: &str) -> bool {
    let mut words = Vec::new();
    let mut word = String::new();
    for c in name.chars() {
        if c == '_' || c == '-' || c == '.' || (c.is_ascii_uppercase() && !word.is_empty()) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if c == '_' || c == '-' || c == '.' {
                continue;
            }
        }
        word.push(c.to_ascii_lowercase());
    }
    words.push(word);
    const IDS: [&str; 14] =
        ["id", "ids", "uuid", "guid", "sha", "hash", "etag", "digest", "checksum", "commit", "oid", "rev", "revision", "path"];
    words.last().is_some_and(|w| IDS.contains(&w.as_str()))
}

/// The name the value at `start` belongs to: `?name=`, `NAME=`, `NAME: `,
/// `"name":"`, `\"name\":\"` or `--name `.
fn name_before(s: &[char], start: usize) -> Option<Context> {
    let word = |c: char| c.is_ascii_alphanumeric() || "_-.".contains(c);
    let mut i = start;
    while i > 0 && "\"'\\".contains(s[i - 1]) {
        i -= 1;
    }
    let after_spaces = i;
    while i > 0 && s[i - 1] == ' ' {
        i -= 1;
    }
    if i > 0 && matches!(s[i - 1], '=' | ':') {
        let sep = s[i - 1];
        i -= 1;
        while i > 0 && "\"'\\ ".contains(s[i - 1]) {
            i -= 1;
        }
        let end = i;
        while i > 0 && word(s[i - 1]) {
            i -= 1;
        }
        if i == end {
            return None;
        }
        let name: String = s[i..end].iter().collect();
        if sep == '=' && i > 0 && matches!(s[i - 1], '?' | '&') {
            return Some(Context::UrlParam(name.to_lowercase()));
        }
        return Some(Context::Key(name));
    }
    if i < after_spaces {
        // --flag VALUE
        let end = i;
        while i > 0 && word(s[i - 1]) {
            i -= 1;
        }
        let flag: String = s[i..end].iter().collect();
        if flag.len() > 2 && flag.starts_with("--") && (i == 0 || s[i - 1].is_whitespace()) {
            return Some(Context::Key(flag[2..].to_string()));
        }
    }
    None
}

/// `art_v2_x_` in `art_v2_x_a1b2…`: short leading segments ending with _ or -.
fn prefix_of(s: &[char]) -> Option<String> {
    let mut end = 0;
    let mut i = 0;
    while i < s.len() {
        let seg = s[i..].iter().take_while(|c| c.is_ascii_alphanumeric()).count();
        if seg == 0 || seg > 8 || i + seg >= s.len() || !matches!(s[i + seg], '_' | '-') {
            break;
        }
        i += seg + 1;
        end = i;
        if end > 14 {
            break;
        }
    }
    (end >= 3 && s[..end].iter().any(|c| c.is_ascii_alphabetic())).then(|| s[..end].iter().collect())
}

fn shorten(s: &[char]) -> String {
    format!("{}… ({} characters)", s.iter().take(4).collect::<String>(), s.len())
}

impl Report {
    fn add(&mut self, context: Context, value: &[char], id: i64, tool: &str) {
        let context_key = context.clone();
        let f = self.findings.entry(context).or_default();
        if f.count == 0 {
            f.sample = shorten(value);
            f.lengths = (value.len(), value.len());
        }
        f.count += 1;
        f.lengths = (f.lengths.0.min(value.len()), f.lengths.1.max(value.len()));
        // For a prefix, the characters that follow it make the suggested set.
        let skip = if let Context::Prefix(p) = &context_key { p.chars().count() } else { 0 };
        for &c in &value[skip.min(value.len())..] {
            if c.is_ascii_lowercase() {
                f.chars.0 = true;
            } else if c.is_ascii_uppercase() {
                f.chars.1 = true;
            } else if c.is_ascii_digit() {
                f.chars.2 = true;
            } else if !f.chars.3.contains(&c) {
                f.chars.3.push(c);
            }
        }
        if !f.events.contains(&id) {
            f.events.push(id);
        }
        if !tool.is_empty() && !f.tools.iter().any(|t| t == tool) {
            f.tools.push(tool.to_string());
        }
    }

    fn scan_string(&mut self, text: &str, field: Option<&str>, id: i64, tool: &str) {
        let s: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < s.len() {
            if !token_char(s[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < s.len() && token_char(s[i]) {
                i += 1;
            }
            let run = &s[start..i];
            if !looks_random(run) || is_uuid(run) {
                continue;
            }
            let whole = start == 0 && i == s.len();
            let context = if whole && field.is_some() {
                Context::Field(field.unwrap_or_default().to_string())
            } else {
                name_before(&s, start).unwrap_or_else(|| prefix_of(run).map(Context::Prefix).unwrap_or(Context::Bare))
            };
            let named = match &context {
                Context::UrlParam(n) | Context::Key(n) | Context::Field(n) => Some(n.as_str()),
                _ => None,
            };
            if named.is_some_and(is_identifier_name) || (named.is_none() && is_hex(run)) {
                continue; // an identifier (file_id, commit_sha) or a bare hash
            }
            self.add(context, run, id, tool);
        }
    }

    fn scan(&mut self, value: &Value, field: Option<&str>, id: i64, tool: &str) {
        match value {
            Value::Object(map) => {
                for (k, v) in map.iter() {
                    self.scan(v, Some(k), id, tool);
                }
            }
            Value::Array(items) => {
                for v in items {
                    self.scan(v, field, id, tool);
                }
            }
            Value::String(s) => self.scan_string(s, field, id, tool),
            _ => {}
        }
    }
}

/// Character set covering what was seen, for a suggested `token` rule.
fn class_of(f: &Finding) -> String {
    let mut class = String::from("[");
    if f.chars.0 {
        class.push_str("a-z");
    }
    if f.chars.1 {
        class.push_str("A-Z");
    }
    if f.chars.2 {
        class.push_str("0-9");
    }
    for &c in &f.chars.3 {
        if c != '-' {
            class.push(c);
        }
    }
    if f.chars.3.contains(&'-') {
        class.push('-');
    }
    class.push(']');
    class
}

fn suggestion(context: &Context, f: &Finding) -> Option<String> {
    Some(match context {
        Context::UrlParam(n) => format!("url-param {n}"),
        Context::Key(n) => format!("key {}", n.to_lowercase()),
        Context::Field(n) => format!("field {}", n.to_lowercase()),
        Context::Prefix(p) => format!("token {p} {} {}", (f.lengths.0.saturating_sub(p.chars().count())).max(8), class_of(f)),
        Context::Bare => return None,
    })
}

pub fn audit(data_dir: &Path) -> Result<Report, Error> {
    let con = store::connect(data_dir)?;
    let rows = con.query("SELECT id, tool, payload FROM events ORDER BY id", &[])?;
    let mut report = Report { checked: rows.len(), ..Default::default() };
    for row in &rows {
        let (Some(id), Some(text)) = (row[0].int(), row[2].text()) else { continue };
        let tool = row[1].text().unwrap_or_default();
        let Ok(payload) = crate::json::parse(text) else { continue };
        let Some(args) = payload.get("arguments") else { continue };
        // Look at each event as today's rules would store it, so that only
        // values no rule covers yet are reported.
        let current = redact(args);
        if &current != args || text.contains("[SECRET MASQU") || text.contains("[CONTENU NON CONSERV") {
            report.outdated.push(id);
        }
        report.scan(&current, None, id, tool);
    }
    Ok(report)
}

fn event_list(ids: &[i64]) -> String {
    let shown: Vec<String> = ids.iter().take(5).map(|i| format!("#{i}")).collect();
    let more = if ids.len() > 5 { format!(" and {} more", ids.len() - 5) } else { String::new() };
    shown.join(", ") + &more
}

pub fn render(report: &Report) -> String {
    let mut out = format!("llca audit · {} events checked · nothing is changed\n\n", report.checked);
    if report.outdated.is_empty() {
        out.push_str("All events are redacted with today's rules.\n");
    } else {
        out.push_str(&format!(
            "{} event(s) were recorded with older rules and would change ({}).\n  Run: llca redact\n",
            report.outdated.len(),
            event_list(&report.outdated)
        ));
    }
    if report.findings.is_empty() {
        out.push_str("\nNo value that looks like a secret was found.\n");
        return out;
    }
    let mut findings: Vec<_> = report.findings.iter().collect();
    findings.sort_by_key(|(c, f)| (matches!(c, Context::Bare), std::cmp::Reverse(f.count)));
    out.push_str("\nValues that look random and were kept. Check each one: if it is a secret, add the\nsuggested rule to rules/redaction.rules, with a made-up example in rules/cases.txt.\n");
    for (context, f) in findings {
        out.push_str(&format!(
            "\n  {} · {} value(s), e.g. {}\n    events {} · {}\n",
            context.title(),
            f.count,
            f.sample,
            event_list(&f.events),
            f.tools.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        ));
        match suggestion(context, f) {
            Some(rule) => out.push_str(&format!("    rule: {rule}\n")),
            None => out.push_str("    no context to build a rule from: look at these events with llca logs\n"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn what_looks_random() {
        for secret in [
            "3f7a1c9e5b2d8f4a6c0e1b7d9a3f5c8e2b4d6a1f",
            "Qm7xK2pV9tLw4Rz8Nc1Bf6Hy3Dj5Gs0Ak2Ve7Xu9Pq",
            "tR8vLq2ZpX0mWc5YbN7kJ3hG9fD1sA6eQ4uI",
        ] {
            assert!(looks_random(&chars(secret)), "{secret}");
        }
        for ordinary in [
            "llca-dashboard-concepts-deploy",
            "2026-01-01T00%3A00%3A00Z",
            "react-native-reanimated-3",
            "test_dir_2026_10_10_check",
            "SerialTest_SerialTest_ino",
            "short1a2b3c",
        ] {
            assert!(!looks_random(&chars(ordinary)), "{ordinary}");
        }
        assert!(is_uuid(&chars("8981e47a-681f-4a6a-b86c-73d6347614b7")));
    }

    #[test]
    fn contexts_and_suggestions() {
        let mut r = Report::default();
        let args = crate::json::parse(
            r#"{"file_id":"file_000000003868820a92272101151b2de0","command":"curl 'https://x.example/a?skey=Zq81mXw0pL3vB7nR2tY9uH4&x=1' --api-secret Pk92Lmz7Qw3eR8tY5uI1oA0s && echo xyz_k7_a1B2c3D4e5F6g7H8i9J0","commit_sha":"50225c47e1374527b0e963dcf28b8595a8e01608","note":"aB3dE5gH7jK9mN1pQ3sT5vX7"}"#,
        )
        .unwrap();
        r.scan(&args, None, 7, "Bash");
        let rules: Vec<String> = r.findings.iter().filter_map(|(c, f)| suggestion(c, f)).collect();
        assert!(rules.contains(&"url-param skey".to_string()), "{rules:?}");
        assert!(rules.contains(&"key api-secret".to_string()), "{rules:?}");
        assert!(rules.contains(&"token xyz_k7_ 20 [a-zA-Z0-9]".to_string()), "{rules:?}");
        assert!(rules.contains(&"field note".to_string()), "{rules:?}");
        // file_id and commit_sha are identifiers, not secrets.
        assert_eq!(r.findings.len(), 4, "{:?}", r.findings.keys().collect::<Vec<_>>());
        let text = render(&r);
        assert!(!text.contains("Zq81mXw0pL3vB7nR2tY9uH4") && text.contains("Zq81… (23 characters)"), "{text}");
    }
}
