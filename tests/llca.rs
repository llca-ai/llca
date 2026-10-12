//! End-to-end tests. Ported from the Python test suite (v0.2), plus checks
//! for upgrading from the Python version.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use llca::install::{configure, inspect, is_ours};
use llca::json::{self, Value};
use llca::redact::MASK;
use llca::store::{self, query, record, stats, Filter};
use llca::{obj, server, sqlite, util};

const BIN: &str = env!("CARGO_BIN_EXE_llca");

/// A temporary directory whose path contains a space and an apostrophe.
struct Temp(PathBuf);

impl Temp {
    fn new() -> Temp {
        // Library calls to configure() must install the real binary, not the test harness.
        std::env::set_var("LLCA_BIN", BIN);
        let name = format!("llca test ' {}", util::base64url(&util::random_bytes(6).unwrap()));
        let dir = std::env::temp_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
    fn data(&self) -> PathBuf {
        self.0.join("data")
    }
    fn codex(&self) -> PathBuf {
        self.0.join("codex")
    }
    fn claude(&self) -> PathBuf {
        self.0.join("claude")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn payload(event: &str) -> Value {
    obj![
        ("session_id", "session-test"),
        ("hook_event_name", event),
        ("tool_name", "Bash"),
        ("tool_input", obj![("command", "git status")]),
    ]
}

fn with(mut v: Value, key: &str, value: Value) -> Value {
    v.as_object_mut().unwrap().insert(key, value);
    v
}

fn all(data: &Path) -> Vec<Value> {
    query(data, &Filter { limit: 500, ..Default::default() }).unwrap().events
}

fn read_json(path: &Path) -> Value {
    json::parse(&fs::read_to_string(path).unwrap()).unwrap()
}

fn hook_command(config: &Path, event: &str, index: usize) -> String {
    let v = read_json(config);
    let groups = v.get("hooks").unwrap().get(event).unwrap().as_array().unwrap().clone();
    let group = if index == usize::MAX { groups.last().unwrap().clone() } else { groups[index].clone() };
    group.get("hooks").unwrap().as_array().unwrap()[0].get("command").unwrap().as_str().unwrap().to_string()
}

fn run_shell(command: &str, input: &str) -> (i32, String, String) {
    let mut child = Command::new("sh").arg("-c").arg(command).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

fn cli(args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN).args(args).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into())
}

#[test]
fn merge_idempotency_backup_uninstall_and_executable_collector() {
    let t = Temp::new();
    fs::create_dir_all(t.codex()).unwrap();
    let original = r#"{"custom": "keep", "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "rtk hook codex"}]}]}}"#;
    let path = t.codex().join("hooks.json");
    fs::write(&path, original).unwrap();
    let result = configure(&t.codex(), &t.data(), false, "codex").unwrap();
    assert_eq!(fs::read_to_string(result.get("backup").unwrap().as_str().unwrap()).unwrap(), original);
    let first = fs::read(&path).unwrap();
    assert_eq!(configure(&t.codex(), &t.data(), false, "codex").unwrap().get("changed"), Some(&Value::Bool(false)));
    assert_eq!(first, fs::read(&path).unwrap());
    let config = read_json(&path);
    assert_eq!(config.get("custom").unwrap().as_str(), Some("keep"));
    assert_eq!(
        config.get("hooks").unwrap().get("PreToolUse").unwrap().as_array().unwrap()[0],
        json::parse(original).unwrap().get("hooks").unwrap().get("PreToolUse").unwrap().as_array().unwrap()[0]
    );
    assert_eq!(inspect(&t.data(), &t.codex(), "codex").unwrap().get("installed"), Some(&Value::Bool(true)));
    let command = hook_command(&path, "PreToolUse", usize::MAX);
    let (code, out, err) = run_shell(&command, &payload("PreToolUse").to_compact());
    assert_eq!((code, out.trim(), err.as_str()), (0, "{}", ""));
    assert_eq!(all(&t.data()).len(), 1);
    configure(&t.codex(), &t.data(), true, "codex").unwrap();
    assert_eq!(read_json(&path), json::parse(original).unwrap());
    assert_eq!(inspect(&t.data(), &t.codex(), "codex").unwrap().get("installed"), Some(&Value::Bool(false)));
}

#[test]
fn invalid_configuration_is_not_overwritten() {
    let t = Temp::new();
    fs::create_dir_all(t.codex()).unwrap();
    let path = t.codex().join("hooks.json");
    fs::write(&path, r#"{"broken":"#).unwrap();
    assert!(configure(&t.codex(), &t.data(), false, "codex").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"broken":"#);
}

#[test]
fn symlink_configuration_rejected() {
    let t = Temp::new();
    fs::create_dir_all(t.codex()).unwrap();
    let target = t.0.join("other.json");
    fs::write(&target, "{}").unwrap();
    std::os::unix::fs::symlink(&target, t.codex().join("hooks.json")).unwrap();
    assert!(configure(&t.codex(), &t.data(), false, "codex").is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "{}");
}

#[test]
fn redaction_happens_before_storage_and_response_is_omitted() {
    let t = Temp::new();
    let mut event = with(payload("PostToolUse"), "tool_response", obj![("exit_code", Value::int(0)), ("stdout", "RAW-SENSITIVE-OUTPUT")]);
    event = with(
        event,
        "tool_input",
        obj![
            ("command", r#"API_TOKEN="my-secret-value" curl -H "Authorization: Bearer bearer-secret-value" https://name:password@example.com"#),
            ("nested", obj![("password", "deep-secret"), ("content", "private-file-content")]),
        ],
    );
    event = with(event, "prompt", Value::str("PRIVATE-PROMPT"));
    record(&t.data(), &event, "hook", "codex", None).unwrap();
    let data = Value::Array(all(&t.data())).to_compact();
    let db = String::from_utf8_lossy(&fs::read(t.data().join("events.sqlite3")).unwrap()).into_owned();
    for secret in ["my-secret-value", "bearer-secret-value", "deep-secret", "private-file-content", "RAW-SENSITIVE-OUTPUT", "PRIVATE-PROMPT", "name:password"] {
        assert!(!data.contains(secret), "{secret}");
        assert!(!db.contains(secret), "{secret} in database");
    }
    assert_eq!(all(&t.data())[0].get("status").unwrap().as_str(), Some("success"));
}

#[test]
fn permission_request_is_not_approval_and_unknown_result_is_not_success() {
    let t = Temp::new();
    let status = |t: &Temp| all(&t.data())[0].get("status").unwrap().as_str().unwrap().to_string();
    record(&t.data(), &payload("PermissionRequest"), "hook", "codex", None).unwrap();
    assert_eq!(status(&t), "approval_requested");
    record(&t.data(), &with(payload("PostToolUse"), "tool_response", Value::str("success: false")), "hook", "codex", None).unwrap();
    assert_eq!(status(&t), "returned");
    record(&t.data(), &with(payload("PostToolUse"), "tool_response", obj![("exit_code", Value::int(2))]), "hook", "codex", None).unwrap();
    assert_eq!(status(&t), "error");
}

#[test]
fn patch_records_paths_without_file_contents() {
    let t = Temp::new();
    let event = with(with(payload("PreToolUse"), "tool_name", Value::str("apply_patch")), "tool_input", obj![("command", "*** Begin Patch\n*** Add File: src/demo.py\n+PRIVATE-CODE\n*** End Patch")]);
    record(&t.data(), &event, "hook", "codex", None).unwrap();
    let row = &all(&t.data())[0];
    assert_eq!(row.get("arguments").unwrap().get("files"), Some(&Value::Array(vec![Value::str("src/demo.py")])));
    assert!(!row.to_compact().contains("PRIVATE-CODE"));
}

#[test]
fn invalid_hook_is_passive_and_error_log_has_no_payload() {
    let t = Temp::new();
    let data = t.data();
    let (code, out, err) = run_shell(&format!("{} --data-dir {} hook --adapter codex-v0", util::shell_quote(BIN), util::shell_quote(&data.to_string_lossy())), "BAD-SECRET-PAYLOAD");
    assert_eq!((code, out.trim(), err.as_str()), (0, "{}", ""));
    let log = fs::read_to_string(data.join("collector-errors.log")).unwrap();
    assert!(log.contains("event not recorded") && !log.contains("BAD-SECRET"));
}

#[test]
fn stop_returns_valid_passive_json() {
    let t = Temp::new();
    let (code, out, _) = run_shell(
        &format!("{} --data-dir {} hook --adapter codex-v0", util::shell_quote(BIN), util::shell_quote(&t.data().to_string_lossy())),
        &payload("Stop").to_compact(),
    );
    assert_eq!((code, json::parse(out.trim()).unwrap()), (0, obj![]));
    assert_eq!(all(&t.data())[0].get("status").unwrap().as_str(), Some("turn_ended"));
}

#[test]
fn concurrent_recording_and_pagination() {
    let t = Temp::new();
    record(&t.data(), &payload("PreToolUse"), "hook", "codex", None).unwrap();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let data = t.data();
            std::thread::spawn(move || {
                for _ in 0..4 {
                    record(&data, &payload("PreToolUse"), "hook", "codex", None).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(stats(&t.data()).unwrap().get("events").unwrap().as_integer(), Some(33));
    let first = query(&t.data(), &Filter { limit: 10, ..Default::default() }).unwrap().events;
    let last_id = first.last().unwrap().get("id").unwrap().as_integer().unwrap();
    let second = query(&t.data(), &Filter { limit: 10, before: last_id, ..Default::default() }).unwrap().events;
    let ids = |rows: &[Value]| rows.iter().map(|r| r.get("id").unwrap().as_integer().unwrap()).collect::<Vec<_>>();
    assert!(ids(&first).iter().all(|id| !ids(&second).contains(id)));
    assert!(query(&t.data(), &Filter { search: "' OR 1=1 --", limit: 10, ..Default::default() }).unwrap().events.is_empty());
}

fn http(port: u16, path: &str, headers: &[(&str, String)]) -> (u16, String, String) {
    http_method(port, "GET", path, headers)
}

fn http_method(port: u16, method: &str, path: &str, headers: &[(&str, String)]) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    let code = head.split(' ').nth(1).unwrap().parse().unwrap();
    (code, head.to_string(), body.to_string())
}

#[test]
fn http_access_control_and_assets() {
    let t = Temp::new();
    record(&t.data(), &payload("PreToolUse"), "hook", "codex", None).unwrap();
    let (listener, dashboard, url) = server::bind(&t.data(), vec![("codex".into(), t.codex())], 0, false).unwrap();
    let port = listener.local_addr().unwrap().port();
    let token = url.split("#token=").nth(1).unwrap().to_string();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let handle = std::thread::spawn(move || server::serve_until(listener, dashboard, move || flag.load(std::sync::atomic::Ordering::SeqCst)));
    assert_eq!(http(port, "/api/events", &[]).0, 401);
    let auth = ("Authorization", format!("Bearer {token}"));
    let (code, head, body) = http(port, "/api/events", std::slice::from_ref(&auth));
    assert_eq!(code, 200);
    assert!(head.contains("Cache-Control: no-store"));
    assert_eq!(json::parse(&body).unwrap().get("events").unwrap().as_array().unwrap().len(), 1);
    assert_eq!(http(port, "/api/events?before=abc", std::slice::from_ref(&auth)).0, 400);
    assert_eq!(http(port, "/api/events?kind=everything", std::slice::from_ref(&auth)).0, 400);
    assert_eq!(http(port, "/api/events?result=maybe", std::slice::from_ref(&auth)).0, 400);
    let (code, _, body) = http(port, "/api/events?kind=shell&result=problem", std::slice::from_ref(&auth));
    assert_eq!(code, 200);
    let page = json::parse(&body).unwrap();
    assert_eq!(page.get("next"), Some(&Value::Null));
    assert!(page.get("scanned").and_then(Value::as_integer).is_some());
    let (code, _, body) = http(port, "/api/status", std::slice::from_ref(&auth));
    assert_eq!(code, 200);
    assert_eq!(json::parse(&body).unwrap().get("os").and_then(Value::as_str), Some(std::env::consts::OS));
    // Wrong Host and foreign Origin are refused.
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(format!("GET /api/events HTTP/1.1\r\nHost: evil.example\r\nAuthorization: Bearer {token}\r\n\r\n").as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 403"));
    assert_eq!(http(port, "/api/events", &[auth, ("Origin", "https://evil.example".into())]).0, 403);
    let (code, head, body) = http(port, "/", &[]);
    assert_eq!(code, 200);
    assert!(body.contains("llca") && body.contains("data-kind=\"shell\"") && body.contains("data-kind=\"app\"") && !body.contains(" style="));
    assert!(head.contains("frame-ancestors 'none'") && head.contains("img-src 'self' blob:"));
    // App icons: bare names only, token required, unknown apps are a 404.
    assert_eq!(http(port, "/api/app-icon?name=Finder", &[]).0, 401);
    let auth = ("Authorization", format!("Bearer {token}"));
    assert_eq!(http(port, "/api/app-icon?name=..%2Fetc%2Fpasswd", std::slice::from_ref(&auth)).0, 400);
    assert_eq!(http(port, "/api/app-icon", std::slice::from_ref(&auth)).0, 400);
    // The theme: saved by POST with the token, applied by /theme.js before the first paint.
    assert_eq!(http_method(port, "POST", "/api/theme?set=dark", &[]).0, 401);
    assert_eq!(http(port, "/api/theme?set=dark", std::slice::from_ref(&auth)).0, 405);
    assert_eq!(http_method(port, "POST", "/api/theme?set=purple", std::slice::from_ref(&auth)).0, 400);
    assert!(http(port, "/theme.js", &[]).2.contains("Auto"));
    assert_eq!(http_method(port, "POST", "/api/theme?set=dark", std::slice::from_ref(&auth)).0, 200);
    assert!(http(port, "/theme.js", &[]).2.contains("dataset.theme = \"dark\""));
    assert_eq!(http_method(port, "POST", "/api/theme?set=auto", std::slice::from_ref(&auth)).0, 200);
    assert!(http(port, "/theme.js", &[]).2.contains("Auto"));
    assert_eq!(http(port, "/api/overview?tz=120&from=2026-01-01T00:00:00.000%2B00:00", std::slice::from_ref(&auth)).0, 200);
    assert_eq!(http(port, "/api/overview?from=1%27%20OR%201%3D1", std::slice::from_ref(&auth)).0, 400);
    assert_eq!(http(port, "/api/overview?tz=99999", std::slice::from_ref(&auth)).0, 400);
    // The overview is kept until the journal (or watch.rules) changes.
    let overview = || json::parse(&http(port, "/api/overview?tz=0", std::slice::from_ref(&auth)).2).unwrap().get("totals").unwrap().clone();
    let count = |v: &Value, k: &str| v.get(k).and_then(Value::as_integer).unwrap();
    let first = overview();
    assert_eq!(overview(), first);
    record(&t.data(), &with(payload("PreToolUse"), "tool_input", obj![("command", "export API_TOKEN=FAKEfakeFAKEfake1234")]), "hook", "codex", None).unwrap();
    let second = overview();
    assert_eq!((count(&second, "events"), count(&second, "alert_secrets")), (count(&first, "events") + 1, 1));
    assert_eq!(http_method(port, "POST", "/api/secret-alert/dismiss?through=999999", std::slice::from_ref(&auth)).0, 200);
    assert_eq!(count(&overview(), "alert_secrets"), 0, "a dismissal is seen at once");
    assert_eq!(http_method(port, "POST", "/api/secret-alert/restore?batch=1", std::slice::from_ref(&auth)).0, 200);
    assert_eq!(count(&overview(), "alert_secrets"), 1, "and so is an undo");
    assert_eq!(http(port, "/api/events?to=2020-01-01T00:00:00.000Z", std::slice::from_ref(&auth)).0, 200);
    assert_eq!(http(port, "/api/app-icon?name=No%20Such%20App%20llca", std::slice::from_ref(&auth)).0, 204);
    // Secret alert: POST only, token required, numeric parameters.
    assert_eq!(http_method(port, "POST", "/api/secret-alert/dismiss?through=9", &[]).0, 401);
    assert_eq!(http(port, "/api/secret-alert/dismiss?through=9", std::slice::from_ref(&auth)).0, 405);
    assert_eq!(http_method(port, "POST", "/api/overview", std::slice::from_ref(&auth)).0, 405);
    assert_eq!(http_method(port, "POST", "/api/secret-alert/dismiss?through=abc", std::slice::from_ref(&auth)).0, 400);
    assert_eq!(http_method(port, "POST", "/api/secret-alert/restore", std::slice::from_ref(&auth)).0, 400);
    let (code, _, body) = http_method(port, "POST", "/api/secret-alert/dismiss?through=9", std::slice::from_ref(&auth));
    assert_eq!((code, json::parse(&body).unwrap().get("batch").unwrap().as_integer()), (200, Some(1)));
    assert_eq!(http_method(port, "POST", "/api/secret-alert/restore?batch=1", std::slice::from_ref(&auth)).0, 200);
    // Your rules: POST only, token required; the file is created even where
    // no text editor can be opened (as on a test machine).
    assert_eq!(http_method(port, "POST", "/api/watch/open", &[]).0, 401);
    assert_eq!(http(port, "/api/watch/open", std::slice::from_ref(&auth)).0, 405);
    let code = http_method(port, "POST", "/api/watch/open", std::slice::from_ref(&auth)).0;
    assert!(code == 200 || code == 500);
    assert!(fs::read_to_string(t.data().join("watch.rules")).unwrap().contains("your rules"));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn secret_alert_dismiss_new_secret_and_undo() {
    let t = Temp::new();
    let call = |id: &str, event: &str, agent: &str, command: &str| {
        let p = obj![("session_id", "s1"), ("cwd", "/p/one"), ("tool_use_id", id), ("hook_event_name", event), ("tool_name", "Bash"), ("tool_input", obj![("command", command)])];
        record(&t.data(), &p, "hook", agent, None).unwrap()
    };
    call("a", "PreToolUse", "codex", "export TOKEN=abc && ls");
    call("b", "PreToolUse", "claude-code", "gh auth login --token ghtoken123");
    call("c", "PreToolUse", "codex", "git status");
    let alert = || {
        let ov = store::overview(&t.data(), &Filter::default(), 0, false).unwrap();
        let totals = ov.get("totals").unwrap();
        let n = |k: &str| totals.get(k).unwrap().as_integer();
        (n("secrets").unwrap(), n("alert_secrets").unwrap(), n("alert_through"), ov.get("secret_kinds").unwrap().as_array().unwrap().len())
    };
    let (secrets, shown, through, kinds) = alert();
    assert_eq!((secrets, shown, kinds), (2, 2, 2));
    let through = through.unwrap();

    // Dismissing only covers what is in scope: Codex here.
    let codex = Filter { agent: "codex", ..Default::default() };
    let first = store::dismiss_secrets(&t.data(), &codex, through).unwrap();
    assert_eq!(alert(), (2, 1, Some(through), 1));
    store::dismiss_secrets(&t.data(), &Filter::default(), through).unwrap();
    assert_eq!(alert(), (2, 0, None, 0));
    // The result of a dismissed call does not bring the alert back.
    call("a", "PostToolUse", "codex", "export TOKEN=abc && ls");
    assert_eq!(alert().1, 0);
    // A secret recorded after the dismissed alert was shown is not dismissed.
    let late = call("d", "PreToolUse", "codex", "export API_TOKEN=late");
    store::dismiss_secrets(&t.data(), &Filter::default(), through).unwrap();
    assert_eq!(alert(), (3, 1, Some(late), 1));
    // Undo brings back the calls of that click only.
    store::restore_secrets(&t.data(), first).unwrap();
    assert_eq!(alert().1, 2);
}

#[test]
fn events_name_the_services_they_reached_and_filter_by_service() {
    let t = Temp::new();
    let call = |id: &str, tool: &str, input: Value| {
        let p = obj![("session_id", "s1"), ("tool_use_id", id), ("hook_event_name", "PreToolUse"), ("tool_name", tool), ("tool_input", input)];
        record(&t.data(), &p, "hook", "codex", None).unwrap();
    };
    call("a", "Bash", obj![("command", "gcloud projects list")]);
    call("b", "Bash", obj![("command", "ls -la")]);
    call("c", "mcp__github__create_pull_request", obj![("title", "Fix")]);
    call("d", "mcp__gcp__list_buckets", obj![]);
    // Older than one page of the service filter: still found.
    for i in 0..1200 {
        call(&format!("x{i}"), "Bash", obj![("command", "echo filler")]);
    }
    let services = |row: &Value| -> Vec<String> {
        row.get("services").unwrap().as_array().unwrap().iter().map(|s| s.get("name").unwrap().as_str().unwrap().to_string()).collect()
    };
    let page = query(&t.data(), &Filter { limit: 200, ..Default::default() }).unwrap().events;
    assert_eq!(page.len(), 200);
    let google = query(&t.data(), &Filter { service: "Google Cloud", limit: 200, ..Default::default() }).unwrap().events;
    let ids: Vec<_> = google.iter().map(|r| r.get("tool_use_id").unwrap().as_str().unwrap()).collect();
    assert_eq!(ids, ["d", "a"]);
    assert!(google.iter().all(|r| services(r) == ["Google Cloud"]));
    let via: Vec<_> = google.iter().map(|r| r.get("services").unwrap().as_array().unwrap()[0].get("via").unwrap().as_str().unwrap()).collect();
    assert_eq!(via, ["Connected app (MCP)", "Command line (gcloud)"]);
    assert_eq!(query(&t.data(), &Filter { service: "GitHub", limit: 200, ..Default::default() }).unwrap().events.len(), 1);
    assert!(query(&t.data(), &Filter { service: "Stripe", limit: 200, ..Default::default() }).unwrap().events.is_empty());
    // Paging with `before` keeps working under the filter.
    let older = query(&t.data(), &Filter { service: "Google Cloud", limit: 1, ..Default::default() }).unwrap().events;
    let before = older[0].get("id").unwrap().as_integer().unwrap();
    let next = query(&t.data(), &Filter { service: "Google Cloud", limit: 1, before, ..Default::default() }).unwrap().events;
    assert_eq!(next[0].get("tool_use_id").unwrap().as_str(), Some("a"));
}

#[test]
fn the_map_shows_the_most_recent_events_of_the_period() {
    let t = Temp::new();
    let call = |id: &str, command: &str| {
        let p = obj![("session_id", "s1"), ("tool_use_id", id), ("hook_event_name", "PreToolUse"), ("tool_name", "Bash"), ("tool_input", obj![("command", command)])];
        record(&t.data(), &p, "hook", "codex", None).unwrap();
    };
    for i in 0..5 {
        call(&format!("old{i}"), "gcloud projects list");
    }
    for i in 0..4 {
        call(&format!("new{i}"), "gh pr list");
    }
    let names = |limit: i64| {
        let ov = store::overview_with_map_limit(&t.data(), &Filter::default(), 0, true, limit).unwrap();
        let names: Vec<String> = ov.get("map").unwrap().get("resources").unwrap().as_array().unwrap().iter().map(|r| r.get("name").unwrap().as_str().unwrap().to_string()).collect();
        (names, ov.get("map_truncated").cloned(), ov.get("totals").unwrap().get("actions").and_then(Value::as_integer))
    };
    // Limited to 4 events: the 4 most recent (GitHub), not the oldest; the
    // figures still count every event.
    let (limited, truncated, actions) = names(4);
    assert!(limited.contains(&"GitHub".to_string()) && !limited.contains(&"Google Cloud".to_string()), "{limited:?}");
    assert_eq!((truncated, actions), (Some(Value::Bool(true)), Some(9)));
    let (all, truncated, _) = names(9);
    assert!(all.contains(&"GitHub".to_string()) && all.contains(&"Google Cloud".to_string()));
    assert_eq!(truncated, Some(Value::Bool(false)));
}

#[test]
fn kind_and_result_filters_search_the_whole_history_by_call() {
    let t = Temp::new();
    let event = |id: &str, event: &str, tool: &str, input: Value, response: Option<Value>| {
        let mut p = obj![("session_id", "s1"), ("tool_use_id", id), ("hook_event_name", event), ("tool_name", tool), ("tool_input", input)];
        if let Some(r) = response {
            p = with(p, "tool_response", r);
        }
        record(&t.data(), &p, "hook", "codex", None).unwrap();
    };
    let filler = |from: usize, n: usize| {
        for i in from..from + n {
            event(&format!("x{i}"), "PreToolUse", "Bash", obj![("command", "echo filler")], None);
            event(&format!("x{i}"), "PostToolUse", "Bash", obj![("command", "echo filler")], Some(obj![("exit_code", Value::int(0))]));
        }
    };
    // Old calls, far beyond one page of 200 events.
    event("fail", "PreToolUse", "Bash", obj![("command", "make build")], None);
    event("fail", "PostToolUse", "Bash", obj![("command", "make build")], Some(obj![("exit_code", Value::int(2))]));
    event("ask", "PreToolUse", "Bash", obj![("command", "ls")], None);
    event("ask", "PermissionRequest", "Bash", obj![("command", "ls")], None);
    event("key", "PreToolUse", "Bash", obj![("command", "export OPENAI_API_KEY=sk-proj-FAKEFAKEFAKEFAKEFAKEFAKEFAKE1234")], None);
    event("read", "PreToolUse", "Read", obj![("file_path", "/p/notes.md")], None);
    event("push", "PreToolUse", "Bash", obj![("command", "git push origin main")], None);
    filler(0, 700);
    let ids = |f: Filter| -> Vec<String> {
        let mut v: Vec<String> = query(&t.data(), &Filter { limit: 200, ..f }).unwrap().events.iter().map(|r| r.get("tool_use_id").unwrap().as_str().unwrap().to_string()).collect();
        v.dedup();
        v
    };
    // Without a filter, a page only reaches the recent filler.
    assert!(ids(Filter::default()).iter().all(|id| id.starts_with('x')));
    // A failed call: both of its events, the request (what ran) and the result.
    let page = query(&t.data(), &Filter { result: "problem", limit: 200, ..Default::default() }).unwrap();
    let events: Vec<_> = page.events.iter().map(|r| (r.get("tool_use_id").unwrap().as_str().unwrap(), r.get("event").unwrap().as_str().unwrap())).collect();
    assert_eq!(events, [("fail", "PostToolUse"), ("fail", "PreToolUse")]);
    assert_eq!(page.next, None, "the whole history was searched");
    assert!(page.scanned > 1400);
    assert_eq!(ids(Filter { result: "waiting", ..Default::default() }), ["ask"]);
    assert_eq!(ids(Filter { kind: "secret", ..Default::default() }), ["key"]);
    assert_eq!(ids(Filter { kind: "read", ..Default::default() }), ["read"]);
    // Worth a look: a superset (every risky-looking command, failure, approval,
    // secret); the dashboard applies the exact rules. Never the plain filler.
    let worth = ids(Filter { kind: "attention", ..Default::default() });
    for id in ["push", "key", "ask", "fail"] {
        assert!(worth.contains(&id.to_string()), "{id} missing from {worth:?}");
    }
    assert!(!worth.iter().any(|id| id.starts_with('x') || id == "read"), "{worth:?}");
    // Both filters: the call must match both.
    assert_eq!(ids(Filter { kind: "shell", result: "problem", ..Default::default() }), ["fail"]);
    assert!(ids(Filter { kind: "read", result: "problem", ..Default::default() }).is_empty());

    // Many matching calls: pages follow each other with `next`, and a call cut
    // by the edge of a page still comes with all of its events.
    let t2 = Temp::new();
    for i in 0..150 {
        let id = format!("c{i}");
        record(&t2.data(), &obj![("session_id", "s1"), ("tool_use_id", id.as_str()), ("hook_event_name", "PreToolUse"), ("tool_name", "Bash"), ("tool_input", obj![("command", "false")])], "hook", "codex", None).unwrap();
        record(&t2.data(), &obj![("session_id", "s1"), ("tool_use_id", id.as_str()), ("hook_event_name", "PostToolUse"), ("tool_name", "Bash"), ("tool_input", obj![("command", "false")]), ("tool_response", obj![("exit_code", Value::int(1))])], "hook", "codex", None).unwrap();
    }
    let first = query(&t2.data(), &Filter { result: "problem", limit: 101, ..Default::default() }).unwrap();
    let next = first.next.expect("more to read");
    let second = query(&t2.data(), &Filter { result: "problem", limit: 500, before: next, ..Default::default() }).unwrap();
    assert_eq!(second.next, None);
    let mut calls: std::collections::BTreeMap<String, std::collections::BTreeSet<i64>> = Default::default();
    for r in first.events.iter().chain(&second.events) {
        calls.entry(r.get("tool_use_id").unwrap().as_str().unwrap().into()).or_default().insert(r.get("id").unwrap().as_integer().unwrap());
    }
    assert_eq!(calls.len(), 150);
    assert!(calls.values().all(|ids| ids.len() == 2), "every call has its request and its result");
}

#[test]
fn watch_rules_mark_actions_without_changing_the_journal() {
    let t = Temp::new();
    let call = |id: &str, event: &str, tool: &str, input: Value| {
        let p = obj![("session_id", "s1"), ("tool_use_id", id), ("hook_event_name", event), ("tool_name", tool), ("tool_input", input)];
        record(&t.data(), &p, "hook", "codex", None).unwrap();
    };
    call("a", "PreToolUse", "Read", obj![("file_path", "/p/.env.local")]);
    call("a", "PostToolUse", "Read", obj![("file_path", "/p/.env.local")]);
    call("b", "PreToolUse", "Bash", obj![("command", "kubectl apply -f x.yaml --context prod")]);
    call("c", "PreToolUse", "Bash", obj![("command", "ls")]);
    let before = all(&t.data());
    // No file yet: nothing is marked, and the overview says so.
    let ov = store::overview(&t.data(), &Filter::default(), 0, false).unwrap();
    assert_eq!(ov.get("watch").unwrap().get("exists"), Some(&Value::Bool(false)));
    // init creates the file with its explanations, and never overwrites it.
    assert!(llca::watch::create_if_missing(&t.data()).unwrap());
    fs::write(t.data().join("watch.rules"), "file .env # Touched a .env file\nalert command kubectl * --context prod # Touched production\ncomand oops # typo\nfile ~/.ssh/ # Touched SSH keys\n").unwrap();
    assert!(!llca::watch::create_if_missing(&t.data()).unwrap());

    let labels = |row: &Value| -> Vec<String> { row.get("watch").unwrap().as_array().unwrap().iter().map(|w| w.get("label").unwrap().as_str().unwrap().to_string()).collect() };
    let rows = all(&t.data());
    let by_id = |id: &str| rows.iter().find(|r| r.get("tool_use_id").and_then(Value::as_str) == Some(id)).unwrap();
    assert_eq!(labels(by_id("a")), ["Touched a .env file"]);
    assert_eq!(labels(by_id("b")), ["Touched production"]);
    assert!(labels(by_id("c")).is_empty());
    let w = &by_id("b").get("watch").unwrap().as_array().unwrap()[0];
    assert_eq!((w.get("alert"), w.get("line").unwrap().as_integer()), (Some(&Value::Bool(true)), Some(2)));
    // The journal itself is unchanged.
    let strip = |rows: Vec<Value>| rows.into_iter().map(|mut r| { r.as_object_mut().unwrap().remove("watch"); r }).collect::<Vec<_>>();
    assert_eq!(strip(all(&t.data())), strip(before));

    // Filter by rule, as the activity list does.
    let env = query(&t.data(), &Filter { rule: "Touched a .env file", limit: 200, ..Default::default() }).unwrap().events;
    assert_eq!(env.len(), 2); // its PreToolUse and PostToolUse
    // Overview: one count per call, every label listed, skipped lines named.
    let ov = store::overview(&t.data(), &Filter::default(), 0, false).unwrap();
    let watch = ov.get("watch").unwrap();
    let rules = watch.get("rules").unwrap().as_array().unwrap();
    let count = |label: &str| rules.iter().find(|r| r.get("label").unwrap().as_str() == Some(label)).unwrap().get("actions").unwrap().as_integer().unwrap();
    assert_eq!((count("Touched a .env file"), count("Touched production"), count("Touched SSH keys")), (1, 1, 0));
    let errors = watch.get("errors").unwrap().as_array().unwrap();
    assert_eq!(errors[0].get("line").unwrap().as_integer(), Some(3));
}

#[test]
fn outdated_collector_is_reported() {
    use llca::install::{collector_path, collector_state, older};
    use std::os::unix::fs::PermissionsExt;
    assert!(older("0.6.1", "0.6.2") && older("0.9.9", "0.10.0") && !older("0.6.2", "0.6.2") && !older("1.0.0", "0.9.9"));
    let t = Temp::new();
    let state = |k: &str| collector_state(&t.data()).get(k).cloned().unwrap();
    // Nothing installed: nothing to report.
    assert_eq!(state("outdated"), Value::Bool(false));
    // init copies this very binary: up to date.
    configure(&t.codex(), &t.data(), false, "codex").unwrap();
    assert_eq!(state("version").as_str(), Some(llca::cli::VERSION));
    assert_eq!(state("outdated"), Value::Bool(false));
    let doctor = |args: &[&str]| {
        let mut cmd = Command::new(BIN);
        cmd.arg("--data-dir").arg(t.data()).arg("doctor").args(["--agent", "codex", "--codex-dir"]).arg(t.codex()).args(args);
        cmd.output().unwrap()
    };
    assert_eq!(doctor(&[]).status.code(), Some(0));
    // An older copy (as after `brew upgrade` without `init`): reported, with the fix.
    let copy = collector_path(&t.data());
    let fake = |script: &str| {
        fs::write(&copy, script).unwrap();
        fs::set_permissions(&copy, fs::Permissions::from_mode(0o700)).unwrap();
    };
    fake("#!/bin/sh\necho 0.5.0\n");
    assert_eq!((state("version").as_str(), state("outdated")), (Some("0.5.0"), Value::Bool(true)));
    assert_eq!(state("fix").as_str(), Some("llca init --global --agent all"));
    let out = doctor(&[]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("1 problem to fix") && text.contains("older than this one") && text.contains("Fix: llca init --global --agent all"), "{text}");
    // A copy that cannot say its version counts as outdated.
    fake("#!/bin/sh\nexit 1\n# a different length, so the cached answer is not reused\n");
    assert_eq!((state("version"), state("outdated")), (Value::Null, Value::Bool(true)));
    // Running init again replaces the copy.
    configure(&t.codex(), &t.data(), false, "codex").unwrap();
    assert_eq!(state("outdated"), Value::Bool(false));
}

#[test]
fn codex_hook_command_points_to_private_collector() {
    let t = Temp::new();
    configure(&t.codex(), &t.data(), false, "codex").unwrap();
    let argv = util::shell_split(&hook_command(&t.codex().join("hooks.json"), "Stop", 0)).unwrap();
    let data = util::resolve(&t.data());
    assert_eq!(argv[0], data.join("bin/llca").to_string_lossy());
    assert_eq!(&argv[1..], ["--data-dir", &data.to_string_lossy(), "hook", "--adapter", "codex-v0"]);
}

#[test]
fn claude_settings_are_merged_preserved_and_restored() {
    let t = Temp::new();
    fs::create_dir_all(t.claude()).unwrap();
    let original = r#"{"model": "opus", "permissions": {"allow": ["Bash(git status)"]}, "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "./lint.sh"}]}]}}"#;
    let settings = t.claude().join("settings.json");
    fs::write(&settings, original).unwrap();
    let result = configure(&t.claude(), &t.data(), false, "claude-code").unwrap();
    assert_eq!(result.get("changed"), Some(&Value::Bool(true)));
    assert!(Path::new(result.get("backup").unwrap().as_str().unwrap()).exists());
    let config = read_json(&settings);
    let orig = json::parse(original).unwrap();
    assert_eq!(config.get("model"), orig.get("model"));
    assert_eq!(config.get("permissions"), orig.get("permissions"));
    assert_eq!(config.get("hooks").unwrap().get("PreToolUse").unwrap().as_array().unwrap()[0], orig.get("hooks").unwrap().get("PreToolUse").unwrap().as_array().unwrap()[0]);
    assert!(config.get("hooks").unwrap().get("PostToolUseFailure").is_some());
    assert!(config.get("hooks").unwrap().get("Interrupt").is_none());
    assert_eq!(configure(&t.claude(), &t.data(), false, "claude-code").unwrap().get("changed"), Some(&Value::Bool(false)));
    assert_eq!(inspect(&t.data(), &t.claude(), "claude-code").unwrap().get("installed"), Some(&Value::Bool(true)));
    configure(&t.claude(), &t.data(), true, "claude-code").unwrap();
    assert_eq!(read_json(&settings), orig);
}

#[test]
fn claude_uninstall_leaves_no_empty_hooks_block() {
    let t = Temp::new();
    fs::create_dir_all(t.claude()).unwrap();
    fs::write(t.claude().join("settings.json"), r#"{"theme": "dark"}"#).unwrap();
    configure(&t.claude(), &t.data(), false, "claude-code").unwrap();
    configure(&t.claude(), &t.data(), true, "claude-code").unwrap();
    assert_eq!(read_json(&t.claude().join("settings.json")), obj![("theme", "dark")]);
}

#[test]
fn claude_collector_is_silent_and_tags_events() {
    let t = Temp::new();
    configure(&t.claude(), &t.data(), false, "claude-code").unwrap();
    let command = hook_command(&t.claude().join("settings.json"), "SessionStart", 0);
    let event = obj![("session_id", "c1"), ("hook_event_name", "SessionStart"), ("source", "startup"), ("cwd", "/tmp")];
    assert_eq!(run_shell(&command, &event.to_compact()), (0, String::new(), String::new()));
    let row = &all(&t.data())[0];
    assert_eq!(row.get("agent").unwrap().as_str(), Some("claude-code"));
    assert_eq!(row.get("source").unwrap().as_str(), Some("startup"));
    assert!(query(&t.data(), &Filter { agent: "codex", limit: 10, ..Default::default() }).unwrap().events.is_empty());
}

#[test]
fn claude_events_statuses_and_file_contents() {
    let t = Temp::new();
    let base = |event: &str, tool: &str, input: Value| obj![("session_id", "c1"), ("tool_use_id", "toolu_1"), ("cwd", "/tmp"), ("hook_event_name", event), ("tool_name", tool), ("tool_input", input)];
    record(&t.data(), &base("PreToolUse", "Edit", obj![("file_path", "/tmp/a.py"), ("old_string", "PRIVATE-OLD"), ("new_string", "PRIVATE-NEW")]), "hook", "claude-code", None).unwrap();
    record(&t.data(), &base("PreToolUse", "MultiEdit", obj![("file_path", "/tmp/b.py"), ("edits", Value::Array(vec![obj![("old_string", "PRIVATE-OLD"), ("new_string", "PRIVATE-NEW")]]))]), "hook", "claude-code", None).unwrap();
    let failure = with(with(base("PostToolUseFailure", "Bash", obj![("command", "npm test")]), "error", Value::str("PRIVATE-ERROR-TEXT")), "is_interrupt", Value::Bool(false));
    record(&t.data(), &failure, "hook", "claude-code", None).unwrap();
    record(&t.data(), &base("PermissionDenied", "Bash", obj![("command", "rm -rf build")]), "hook", "claude-code", None).unwrap();
    record(&t.data(), &base("PostToolUse", "Bash", obj![("command", "pwd")]), "hook", "claude-code", None).unwrap();
    let rows = all(&t.data());
    let field = |k: &str| rows.iter().map(|r| r.get(k).unwrap().as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(field("status"), ["success", "denied", "error", "requested", "requested"]);
    assert_eq!(field("category"), ["shell", "shell", "shell", "file", "file"]);
    assert_eq!(rows[2].get("outcome"), Some(&obj![("interrupted", false)]));
    let dump = Value::Array(rows.clone()).to_compact() + &String::from_utf8_lossy(&fs::read(t.data().join("events.sqlite3")).unwrap());
    for secret in ["PRIVATE-OLD", "PRIVATE-NEW", "PRIVATE-ERROR-TEXT"] {
        assert!(!dump.contains(secret), "{secret}");
    }
    assert!(record(&t.data(), &obj![("session_id", "c1"), ("hook_event_name", "Interrupt")], "hook", "claude-code", None).is_err());
    let s = stats(&t.data()).unwrap();
    assert_eq!(s.get("agents").unwrap().get("claude-code").unwrap().get("events").unwrap().as_integer(), Some(5));
}

#[test]
fn codex_post_tool_use_without_structured_result_stays_returned() {
    let t = Temp::new();
    let event = with(payload("PostToolUse"), "tool_response", obj![("stdout", "/tmp")]);
    record(&t.data(), &event, "hook", "codex", None).unwrap();
    assert_eq!(all(&t.data())[0].get("status").unwrap().as_str(), Some("returned"));
}

#[test]
fn journal_from_python_v01_is_migrated() {
    let t = Temp::new();
    util::private_dir(&t.data()).unwrap();
    let con = sqlite::Connection::open(&t.data().join("events.sqlite3"), 500).unwrap();
    con.execute("CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, received_at TEXT NOT NULL, session_id TEXT NOT NULL, event TEXT NOT NULL, tool TEXT NOT NULL, status TEXT NOT NULL, category TEXT NOT NULL, payload TEXT NOT NULL)", &[]).unwrap();
    con.execute("INSERT INTO events(received_at,session_id,event,tool,status,category,payload) VALUES('t','old','Stop','','turn_ended','session','{\"event\": \"Stop\"}')", &[]).unwrap();
    drop(con);
    record(&t.data(), &obj![("session_id", "c1"), ("hook_event_name", "Stop")], "hook", "claude-code", None).unwrap();
    let agents: Vec<_> = all(&t.data()).iter().map(|r| r.get("agent").unwrap().as_str().unwrap().to_string()).collect();
    assert_eq!(agents, ["claude-code", "codex"]);
}

#[test]
fn only_llca_handlers_are_ours() {
    let ours = |command: &str| is_ours(&obj![("type", "command"), ("command", command)]);
    assert!(ours("'/Users/me/Library/Application Support/llca/bin/llca' --data-dir /tmp/d hook --adapter codex-v0"));
    assert!(!ours("rtk hook codex"));
    // No compatibility with the former name: its hooks are not recognised.
    assert!(!ours("/tmp/d/bin/agentledger --data-dir /tmp/d hook --adapter codex-v0"));
}

#[test]
fn cli_installs_both_agents_and_doctor_reports_each() {
    let t = Temp::new();
    let project = t.0.join("projet");
    let data = t.data().to_string_lossy().into_owned();
    let p = project.to_string_lossy().into_owned();
    assert_eq!(cli(&["--data-dir", &data, "init", "--project", &p, "--agent", "all"]).0, 0);
    assert!(project.join(".codex/hooks.json").exists());
    assert!(project.join(".claude/settings.local.json").exists());
    assert!(!project.join(".claude/settings.json").exists());
    let (code, out) = cli(&["--data-dir", &data, "doctor", "--project", &p, "--json"]);
    let report = json::parse(&out).unwrap();
    assert_eq!(code, 0);
    assert_eq!(report.get("agents").unwrap().get("codex").unwrap().get("installed"), Some(&Value::Bool(true)));
    assert_eq!(report.get("agents").unwrap().get("claude-code").unwrap().get("installed"), Some(&Value::Bool(true)));
    cli(&["--data-dir", &data, "uninstall", "--project", &p, "--agent", "claude"]);
    let report = json::parse(&cli(&["--data-dir", &data, "doctor", "--project", &p, "--json"]).1).unwrap();
    assert_eq!(report.get("agents").unwrap().get("codex").unwrap().get("installed"), Some(&Value::Bool(true)));
    assert_eq!(report.get("agents").unwrap().get("claude-code").unwrap().get("installed"), Some(&Value::Bool(false)));
    assert_eq!(cli(&["--data-dir", &data, "init"]).0, 2);
    assert_eq!(cli(&["--data-dir", &data, "logs", "--agent", "nope"]).0, 2);
}

#[test]
fn logs_json_and_text_output() {
    let t = Temp::new();
    record(&t.data(), &payload("PreToolUse"), "hook", "codex", None).unwrap();
    let data = t.data().to_string_lossy().into_owned();
    let (code, out) = cli(&["--data-dir", &data, "logs", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(json::parse(out.trim()).unwrap().get("tool").unwrap().as_str(), Some("Bash"));
    let (_, out) = cli(&["--data-dir", &data, "logs", "--agent", "claude"]);
    assert!(out.starts_with("No events"));
}

#[test]
fn demo_data_is_well_formed() {
    let t = Temp::new();
    llca::demo::fill(&t.data()).unwrap();
    let rows = all(&t.data());
    assert_eq!(rows.len(), 76);
    let dump = Value::Array(rows).to_compact();
    assert!(!dump.contains("sk_live_51Hdemo"));
    // Computer Use: the step title is kept, the script is not.
    assert!(dump.contains("Refermer le détail des prévisions") && !dump.contains("ui.press"));
}

#[test]
fn time_range_project_and_overview() {
    let t = Temp::new();
    let at = |day: &str| format!("2026-10-{day}T10:00:00.000+00:00");
    let call = |sid: &str, cwd: &str, tool: &str, event: &str| {
        obj![("session_id", sid), ("cwd", cwd), ("tool_use_id", "x"), ("hook_event_name", event), ("tool_name", tool), ("tool_input", obj![("command", "export TOKEN=abc && sudo ls")])]
    };
    record(&t.data(), &call("s1", "/p/one", "Bash", "PreToolUse"), "hook", "codex", Some(&at("01"))).unwrap();
    record(&t.data(), &call("s1", "/p/one", "Bash", "PostToolUseFailure"), "hook", "claude-code", Some(&at("01"))).unwrap();
    record(&t.data(), &call("s2", "/p/two", "Edit", "PreToolUse"), "hook", "claude-code", Some(&at("05"))).unwrap();
    record(&t.data(), &call("s3", "/p/two", "Read", "PreToolUse"), "hook", "codex", Some(&at("09"))).unwrap();

    let range = |from: &'static str, to: &'static str| Filter { from, to, limit: 100, ..Default::default() };
    assert_eq!(query(&t.data(), &range("2026-10-02T00:00:00.000+00:00", "")).unwrap().events.len(), 2);
    assert_eq!(query(&t.data(), &range("", "2026-10-05T00:00:00.000+00:00")).unwrap().events.len(), 2);
    assert_eq!(query(&t.data(), &Filter { project: "/p/two", limit: 100, ..Default::default() }).unwrap().events.len(), 2);

    let ov = store::overview(&t.data(), &Filter::default(), 120, true).unwrap();
    let totals = ov.get("totals").unwrap();
    let n = |k: &str| totals.get(k).unwrap().as_integer().unwrap();
    // Secrets are counted per tool call (agent, session, call id): 4 calls here.
    assert_eq!((n("events"), n("actions"), n("sessions"), n("failed"), n("secrets")), (4, 3, 4, 1, 4));
    let kinds = ov.get("secret_kinds").unwrap().as_array().unwrap();
    let codex = kinds.iter().find(|k| k.get("agent").unwrap().as_str() == Some("codex")).unwrap();
    assert_eq!(codex.get("kind").unwrap().as_str(), Some("Value of \"TOKEN\""));
    assert_eq!(codex.get("calls").unwrap().as_integer(), Some(2));
    let days: Vec<_> = ov.get("by_day").unwrap().as_array().unwrap().iter().map(|d| d.get("day").unwrap().as_str().unwrap().to_string()).collect();
    assert_eq!(days.first().map(String::as_str), Some("2026-10-01"));
    assert!(days.contains(&"2026-10-09".to_string()));
    let projects = ov.get("projects").unwrap().as_array().unwrap();
    assert_eq!(projects[0].get("cwd").unwrap().as_str(), Some("/p/two")); // 2 actions, before /p/one (1)
    assert_eq!(ov.get("commands").unwrap().as_array().unwrap().len(), 1);
    let scoped = store::overview(&t.data(), &Filter { project: "/p/two", agent: "codex", ..Default::default() }, 0, false).unwrap();
    assert_eq!(scoped.get("totals").unwrap().get("actions").unwrap().as_integer(), Some(1));
    assert!(scoped.get("commands").unwrap().as_array().unwrap().is_empty()); // skipped on request
    // A late-evening event belongs to the viewer's local day.
    record(&t.data(), &call("s4", "/p/one", "Bash", "PreToolUse"), "hook", "codex", Some("2026-10-09T23:30:00.000+00:00")).unwrap();
    let local = store::overview(&t.data(), &Filter { from: "2026-10-09T22:00:00.000+00:00", ..Default::default() }, 120, true).unwrap();
    assert_eq!(local.get("by_day").unwrap().as_array().unwrap()[0].get("day").unwrap().as_str(), Some("2026-10-10"));
}

#[test]
fn stored_events_keep_the_v02_shape() {
    let t = Temp::new();
    record(&t.data(), &payload("PreToolUse"), "hook", "codex", None).unwrap();
    let row = &all(&t.data())[0];
    let keys: Vec<_> = row.as_object().unwrap().iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        // `id`, `services` and `watch` are added when reading; the stored JSON keeps the v0.2 keys.
        ["session_id", "turn_id", "tool_use_id", "cwd", "model", "permission_mode", "source", "agent_type", "received_at", "agent", "event", "tool", "category", "status", "arguments", "outcome", "origin", "id", "services", "watch"]
    );
    assert!(row.get("arguments").unwrap().to_compact().contains("git status"));
    assert_ne!(MASK, "");
    let _ = store::MAX_INPUT;
}

/// Linux: the journal goes to $XDG_DATA_HOME/llca, or ~/.local/share/llca.
#[cfg(not(target_os = "macos"))]
#[test]
fn linux_keeps_its_data_in_the_xdg_data_folder() {
    let t = Temp::new();
    let run = |envs: &[(&str, &Path)]| {
        let mut cmd = Command::new(BIN);
        cmd.arg("logs").env_remove("LLCA_HOME").env_remove("XDG_DATA_HOME").env("HOME", &t.0);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        assert!(cmd.output().unwrap().status.success());
    };
    run(&[]);
    assert!(t.0.join(".local/share/llca/events.sqlite3").exists());
    let xdg = t.0.join("xdg");
    run(&[("XDG_DATA_HOME", &xdg)]);
    assert!(xdg.join("llca/events.sqlite3").exists());
    // A relative XDG_DATA_HOME is ignored, as the XDG specification says.
    run(&[("XDG_DATA_HOME", Path::new("relative"))]);
    assert!(!t.0.join("relative").exists());
    // LLCA_HOME still wins.
    let chosen = t.0.join("chosen");
    run(&[("XDG_DATA_HOME", &xdg), ("LLCA_HOME", &chosen)]);
    assert!(chosen.join("events.sqlite3").exists());
}

#[test]
fn codex_project_hooks_stay_out_of_git() {
    if Command::new("git").arg("--version").output().is_err() {
        return; // no git on this machine
    }
    let t = Temp::new();
    let repo = t.0.join("project");
    fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| Command::new("git").arg("-C").arg(&repo).args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"]).args(args).output().unwrap();
    assert!(git(&["init", "-q"]).status.success());
    let init = || {
        let out = Command::new(BIN).arg("--data-dir").arg(t.data()).args(["init", "--agent", "codex", "--project"]).arg(&repo).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    // The hooks file names a path on this computer: git leaves it out, locally.
    assert!(init().contains("added .codex/hooks.json to .git/info/exclude"));
    assert!(repo.join(".codex/hooks.json").exists());
    let status = String::from_utf8_lossy(&git(&["status", "--porcelain", "--untracked-files=all"]).stdout).to_string();
    assert!(!status.contains(".codex"), "{status}");
    // A second init does not add the line again.
    assert!(init().contains("stays on this computer"));
    let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.lines().filter(|l| *l == "/.codex/hooks.json").count(), 1);
    // A hooks file already in the repository: llca says not to commit the change.
    let shared = t.0.join("shared");
    fs::create_dir_all(shared.join(".codex")).unwrap();
    fs::write(shared.join(".codex/hooks.json"), "{}").unwrap();
    let git2 = |args: &[&str]| Command::new("git").arg("-C").arg(&shared).args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"]).args(args).output().unwrap();
    git2(&["init", "-q"]);
    git2(&["add", "."]);
    assert!(git2(&["commit", "-qm", "hooks"]).status.success());
    let out = Command::new(BIN).arg("--data-dir").arg(t.data()).args(["init", "--agent", "codex", "--project"]).arg(&shared).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("git rm --cached .codex/hooks.json"));
}

#[test]
fn an_existing_data_folder_keeps_its_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let t = Temp::new();
    // A folder of the user's, given with --data-dir: llca writes its private
    // files there but leaves the folder's own permissions alone.
    let mine = t.0.join("my folder");
    fs::create_dir_all(&mine).unwrap();
    fs::set_permissions(&mine, fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(BIN).arg("--data-dir").arg(&mine).arg("logs").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::metadata(&mine).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::metadata(mine.join("events.sqlite3")).unwrap().permissions().mode() & 0o777, 0o600);
    // doctor says the folder is open to others, and how to close it.
    let doctor = Command::new(BIN).arg("--data-dir").arg(&mine).arg("doctor").arg("--codex-dir").arg(t.codex()).arg("--claude-dir").arg(t.claude()).env("NO_COLOR", "1").output().unwrap();
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(text.contains("Other users of this computer can open the journal") && text.contains("chmod 700"), "{text}");
    // A folder llca creates itself is private.
    let fresh = t.0.join("new data");
    Command::new(BIN).arg("--data-dir").arg(&fresh).arg("logs").output().unwrap();
    assert_eq!(fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o700);
    // A file in place of the folder is refused.
    let file = t.0.join("a file");
    fs::write(&file, "x").unwrap();
    assert!(!Command::new(BIN).arg("--data-dir").arg(&file).arg("logs").output().unwrap().status.success());
}

#[test]
fn large_events_are_recorded_without_their_content() {
    // A PostToolUse carries the tool's whole answer: here 5 MB, as for a
    // large file read. The event is kept; the answer is not.
    let t = Temp::new();
    let big = "x".repeat(5 * 1024 * 1024);
    let input = obj![
        ("session_id", "s1"),
        ("tool_use_id", "big"),
        ("hook_event_name", "PostToolUse"),
        ("tool_name", "Read"),
        ("tool_input", obj![("file_path", "/p/huge.log")]),
        ("tool_response", obj![("content", big.as_str())]),
    ]
    .to_compact();
    let data = t.data().to_string_lossy().to_string();
    let (code, out, _) = run_shell(&format!("{} --data-dir {} hook --adapter claude-code-v0", BIN, util::shell_quote(&data)), &input);
    assert_eq!((code, out.as_str()), (0, ""));
    let rows = all(&t.data());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("status").and_then(Value::as_str), Some("success"));
    let stored = rows[0].to_compact();
    assert!(stored.len() < 10_000 && stored.contains("huge.log"), "{} bytes", stored.len());
    assert!(!t.data().join("collector-errors.log").exists());
}

#[test]
fn redact_command_cleans_events_recorded_by_earlier_versions() {
    let t = Temp::new();
    record(&t.data(), &payload("PreToolUse"), "hook", "codex", None).unwrap();
    // Two leaks stored by v0.3.0 (observed on 2026-10-10): a credential typed
    // into a running process, and a signed download URL.
    let leaked = [
        r#"{"session_id":"s","event":"PermissionRequest","tool":"write_stdin","arguments":{"session_id":20090,"chars":"{\"credential\":{\"token\":\"art_v2_x_LEAKEDTOKEN1\"}}\n"}}"#,
        r#"{"session_id":"s","event":"PostToolUse","tool":"mcp__sites__deploy","arguments":{"archive":{"download_url":"https://f.example/raw?se=2026&sp=r&sig=LEAKEDSIGNATURE2%3D","file_name":"a.tar.gz"}}}"#,
    ];
    {
        let con = store::connect(&t.data()).unwrap();
        for p in leaked {
            con.execute(
                "INSERT INTO events(received_at,session_id,event,tool,status,category,agent,payload) VALUES('2026-10-10T21:36:29.000+00:00','s','PostToolUse','x','returned','tool','codex',?)",
                &[sqlite::Param::Text(p)],
            )
            .unwrap();
        }
    }
    let out = Command::new(BIN).arg("--data-dir").arg(t.data()).arg("redact").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Checked 3 events; 2 updated.");
    let rows = all(&t.data());
    let text: String = rows.iter().map(Value::to_compact).collect();
    assert!(!text.contains("LEAKED"), "{text}");
    assert!(text.contains("sig=[REDACTED]") && text.contains("[CONTENT NOT KEPT]"), "{text}");
    assert!(text.contains("git status")); // untouched
    // The old text is gone from the files on disk, not only from the rows.
    for entry in fs::read_dir(t.data()).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(!bytes.windows(6).any(|w| w == b"LEAKED"));
    }
    // What was masked is named, as the collector does it (not "kind not recorded").
    let signed = rows.iter().find(|r| r.to_compact().contains("sig=[REDACTED]")).unwrap();
    let kinds = signed.get("secrets").and_then(Value::as_array).cloned().unwrap_or_default();
    assert!(kinds.iter().any(|k| k.as_str() == Some("URL parameter \"sig\"")), "{kinds:?}");
    // Running it again changes nothing.
    let again = Command::new(BIN).arg("--data-dir").arg(t.data()).arg("redact").output().unwrap();
    assert!(String::from_utf8_lossy(&again.stdout).contains("0 updated"));
}

#[test]
fn redact_says_when_the_wal_could_not_be_wiped() {
    let t = Temp::new();
    record(&t.data(), &payload("PreToolUse"), "hook", "codex", None).unwrap();
    {
        let con = store::connect(&t.data()).unwrap();
        con.execute(
            "INSERT INTO events(received_at,session_id,event,tool,status,category,agent,payload) VALUES('2026-10-10T21:36:29.000+00:00','s','PreToolUse','Bash','started','shell','codex',?)",
            &[sqlite::Param::Text(r#"{"session_id":"s","event":"PreToolUse","tool":"Bash","arguments":{"command":"curl 'https://x.example/?sig=LEAKEDSIGNATURE3'"}}"#)],
        )
        .unwrap();
    }
    // Another program reads the journal (as the dashboard does) and keeps
    // its snapshot open: the rows are cleaned, the WAL cannot be emptied.
    let reader = store::connect(&t.data()).unwrap();
    reader.execute("BEGIN", &[]).unwrap();
    reader.query("SELECT COUNT(*) FROM events", &[]).unwrap();
    let busy = Command::new(BIN).arg("--data-dir").arg(t.data()).arg("redact").output().unwrap();
    assert_eq!(busy.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&busy.stdout).contains("1 updated"));
    assert!(String::from_utf8_lossy(&busy.stderr).contains("Close the dashboard"));
    reader.execute("COMMIT", &[]).unwrap();
    drop(reader);
    // Once it is closed, a second run finishes the job.
    let done = Command::new(BIN).arg("--data-dir").arg(t.data()).arg("redact").output().unwrap();
    assert_eq!(done.status.code(), Some(0), "{}", String::from_utf8_lossy(&done.stderr));
    for entry in fs::read_dir(t.data()).unwrap() {
        let bytes = fs::read(entry.unwrap().path()).unwrap();
        assert!(!bytes.windows(6).any(|w| w == b"LEAKED"));
    }
}

#[test]
fn audit_reports_uncovered_values_without_printing_them() {
    let t = Temp::new();
    let args = obj![("command", "curl 'https://api.example/v1?session_key=Zq81mXw0pL3vB7nR2tY9uH4k&page=2' -H 'X-Trace: ok'")];
    record(&t.data(), &with(payload("PreToolUse"), "tool_input", args), "hook", "codex", None).unwrap();
    record(&t.data(), &payload("PreToolUse"), "hook", "codex", None).unwrap();
    let out = Command::new(BIN).arg("--data-dir").arg(t.data()).arg("audit").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(text.contains("2 events checked") && text.contains("All events are redacted with today's rules"), "{text}");
    assert!(text.contains("URL parameter \"session_key\"") && text.contains("rule: url-param session_key"), "{text}");
    assert!(!text.contains("Zq81mXw0pL3vB7nR2tY9uH4k"), "{text}");
}

#[test]
fn events_say_which_kind_of_secret_went_through_the_agent() {
    let t = Temp::new();
    let args = obj![("command", "git clone https://bob:pa55word@git.example/r.git && curl 'https://x.example/f?sig=abc123' -H 'Authorization: Bearer xyz'")];
    record(&t.data(), &with(payload("PreToolUse"), "tool_input", args), "hook", "codex", None).unwrap();
    record(&t.data(), &payload("PreToolUse"), "hook", "codex", None).unwrap();
    let rows = all(&t.data());
    let with_secret = rows.iter().find(|r| r.get("secrets").is_some()).unwrap();
    let kinds: Vec<&str> = with_secret.get("secrets").unwrap().as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(kinds, ["Authorization header", "Password in a URL", "URL parameter \"sig\""]);
    assert!(!with_secret.to_compact().contains("pa55word") && !with_secret.to_compact().contains("abc123"));
    assert_eq!(rows.iter().filter(|r| r.get("secrets").is_some()).count(), 1);
}

#[test]
fn overview_map_shows_where_each_agent_went() {
    let t = Temp::new();
    let call = |sid: &str, id: &str, tool: &str, input: Value, event: &str| {
        obj![("session_id", sid), ("cwd", "/Users/me/projects/infra"), ("tool_use_id", id), ("hook_event_name", event), ("tool_name", tool), ("tool_input", input)]
    };
    let gcloud = obj![("command", "gcloud projects describe demo --format=json")];
    record(&t.data(), &call("s1", "a", "Bash", gcloud.clone(), "PreToolUse"), "hook", "codex", None).unwrap();
    let failed = with(call("s1", "a", "Bash", gcloud, "PostToolUse"), "tool_response", obj![("exit_code", Value::int(1))]);
    record(&t.data(), &failed, "hook", "codex", None).unwrap();
    record(&t.data(), &call("s2", "b", "WebFetch", obj![("url", "https://docs.example.dev/x")], "PreToolUse"), "hook", "claude-code", None).unwrap();
    record(&t.data(), &call("s2", "c", "mcp__github__create_issue", obj![("token", "abc")], "PreToolUse"), "hook", "claude-code", None).unwrap();
    let ov = store::overview(&t.data(), &Filter::default(), 0, false).unwrap();
    let map = ov.get("map").unwrap();
    let find = |name: &str| map.get("resources").unwrap().as_array().unwrap().iter().find(|r| r.get("name").unwrap().as_str() == Some(name)).cloned().unwrap();
    let gcp = find("Google Cloud");
    assert_eq!(gcp.get("category").unwrap().as_str(), Some("service"));
    assert_eq!(gcp.get("agents").unwrap().get("codex").unwrap().as_integer(), Some(1)); // one call, two events
    assert_eq!(gcp.get("failed").unwrap().as_integer(), Some(1));
    assert_eq!(find("GitHub").get("secrets").unwrap().as_integer(), Some(1));
    assert_eq!(find("docs.example.dev").get("category").unwrap().as_str(), Some("web"));
    let infra = find("infra");
    assert_eq!(infra.get("actions").unwrap().as_integer(), Some(3));
    assert_eq!(infra.get("project").unwrap().as_str(), Some("/Users/me/projects/infra"));
    assert_eq!(map.get("agents").unwrap().get("claude-code").unwrap().get("actions").unwrap().as_integer(), Some(2));
}
