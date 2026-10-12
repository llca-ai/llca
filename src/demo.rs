//! Demo data: a short, realistic story across three sessions. All values are fictitious.

use std::path::Path;

use crate::json::{self, Map, Value};
use crate::store::{record, Error};
use crate::util;

enum Outcome {
    Response(&'static str),
    Approve,
    ApprovePending,
    Fail,
}

struct Clock(i64);

impl Clock {
    fn minutes_ago(&mut self, minutes: i64) -> String {
        self.0 = util::now_micros() - minutes * 60_000_000;
        util::iso_from_micros(self.0)
    }
    fn step(&mut self, seconds: i64) -> String {
        self.0 += seconds * 1_000_000;
        util::iso_from_micros(self.0)
    }
}

fn with(base: &Map, extra: &[(&str, Value)]) -> Value {
    let mut m = base.clone();
    for (k, v) in extra {
        m.insert(*k, v.clone());
    }
    Value::Object(m)
}

#[allow(clippy::too_many_arguments)]
fn session(
    dir: &Path,
    clock: &mut Clock,
    agent: &str,
    sid: &str,
    cwd: &str,
    start: i64,
    calls: &[(&str, &str, Outcome)],
    end: bool,
) -> Result<(), Error> {
    let mut base = Map::new();
    base.insert("session_id", Value::str(sid));
    base.insert("cwd", Value::str(cwd));
    base.insert("permission_mode", Value::str("default"));
    let model = if agent == "codex" { "gpt-demo" } else { "claude-demo" };
    let at = clock.minutes_ago(start);
    record(
        dir,
        &with(&base, &[("hook_event_name", "SessionStart".into()), ("source", "startup".into()), ("model", model.into())]),
        "demo",
        agent,
        Some(&at),
    )?;
    for (i, (tool, input, outcome)) in calls.iter().enumerate() {
        let input = json::parse(input).expect("demo input is valid JSON");
        let call = with(
            &base,
            &[
                ("turn_id", "turn-1".into()),
                ("tool_use_id", format!("{sid}-call-{i}").into()),
                ("tool_name", (*tool).into()),
                ("tool_input", input),
            ],
        );
        let call = call.as_object().expect("object").clone();
        let at = clock.step(7);
        record(dir, &with(&call, &[("hook_event_name", "PreToolUse".into())]), "demo", agent, Some(&at))?;
        let mut outcome = outcome;
        let approved;
        if matches!(outcome, Outcome::Approve | Outcome::ApprovePending) {
            let at = clock.step(20);
            record(dir, &with(&call, &[("hook_event_name", "PermissionRequest".into())]), "demo", agent, Some(&at))?;
            if matches!(outcome, Outcome::ApprovePending) {
                continue;
            }
            approved = Outcome::Response(if agent == "codex" { r#"{"exit_code": 0}"# } else { r#"{"stdout": "output not kept"}"# });
            outcome = &approved;
        }
        let at = clock.step(4);
        match outcome {
            Outcome::Fail => record(
                dir,
                &with(
                    &call,
                    &[("hook_event_name", "PostToolUseFailure".into()), ("error", "sample".into()), ("is_interrupt", false.into())],
                ),
                "demo",
                agent,
                Some(&at),
            )?,
            Outcome::Response(response) => {
                let response = json::parse(response).expect("demo response is valid JSON");
                record(
                    dir,
                    &with(&call, &[("hook_event_name", "PostToolUse".into()), ("tool_response", response)]),
                    "demo",
                    agent,
                    Some(&at),
                )?
            }
            _ => unreachable!(),
        };
    }
    let at = clock.step(5);
    record(dir, &with(&base, &[("hook_event_name", "Stop".into())]), "demo", agent, Some(&at))?;
    if end {
        let at = clock.step(30);
        record(dir, &with(&base, &[("hook_event_name", "SessionEnd".into())]), "demo", agent, Some(&at))?;
    }
    Ok(())
}

/// Your rules, for the demo (shown under "Worth a look").
const DEMO_RULES: &str = "\
alert command git push * main   # Pushed to main
file src/cart.py                # Touched the cart code
service Cloudflare              # Used Cloudflare
file ~/.ssh/                    # Touched SSH keys
";

pub fn fill(dir: &Path) -> Result<(), Error> {
    use Outcome::*;
    let mut clock = Clock(0);
    let ok = r#"{"stdout": "output not kept"}"#;
    session(
        dir,
        &mut clock,
        "codex",
        "demo-codex-shop",
        "/Users/demo/projects/shop-api",
        115,
        &[
            ("Bash", r#"{"command": ["bash", "-lc", "git status --short"]}"#, Response(r#"{"exit_code": 0}"#)),
            (
                "apply_patch",
                r#"{"command": "*** Begin Patch\n*** Update File: src/cart.py\n@@\n-a\n+b\n*** Add File: tests/test_cart.py\n+x\n*** End Patch"}"#,
                Response(r#"{"success": true}"#),
            ),
            ("Bash", r#"{"command": ["bash", "-lc", "python -m pytest -q"]}"#, Response(r#"{"exit_code": 1}"#)),
            (
                "apply_patch",
                r#"{"command": "*** Begin Patch\n*** Update File: src/cart.py\n@@\n-b\n+c\n*** End Patch"}"#,
                Response(r#"{"success": true}"#),
            ),
            ("Bash", r#"{"command": ["bash", "-lc", "python -m pytest -q"]}"#, Response(r#"{"exit_code": 0}"#)),
            ("Bash", r#"{"command": ["bash", "-lc", "stripe trigger checkout.session.completed"]}"#, Response(r#"{"exit_code": 0}"#)),
            (
                "Bash",
                r#"{"command": ["bash", "-lc", "curl -o build.zip 'https://storage.example/build.zip?X-Amz-Expires=600&X-Amz-Signature=demo0sig0123456789'"]}"#,
                Response(r#"{"exit_code": 0}"#),
            ),
            ("Bash", r#"{"command": ["bash", "-lc", "git push origin main"]}"#, Approve),
            (
                "Bash",
                r#"{"command": ["bash", "-lc", "'/Applications/Arduino IDE.app/Contents/Resources/app/lib/backend/resources/arduino-cli' version"]}"#,
                Response(r#"{"exit_code": 0}"#),
            ),
            ("Bash", r#"{"command": ["bash", "-lc", "open -a \"Visual Studio Code\" ."]}"#, Response(r#"{"exit_code": 0}"#)),
        ],
        true,
    )?;
    session(
        dir,
        &mut clock,
        "claude-code",
        "demo-claude-landing",
        "/Users/demo/projects/landing-page",
        48,
        &[
            ("Read", r#"{"file_path": "/Users/demo/projects/landing-page/README.md"}"#, Response(ok)),
            ("Grep", r#"{"pattern": "TODO", "path": "src"}"#, Response(ok)),
            (
                "Edit",
                r#"{"file_path": "/Users/demo/projects/landing-page/src/hero.css", "old_string": "private", "new_string": "private"}"#,
                Response(ok),
            ),
            ("WebSearch", r#"{"query": "css container queries browser support"}"#, Response(ok)),
            (
                "WebFetch",
                r#"{"url": "https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_containment", "prompt": "Summarize"}"#,
                Response(ok),
            ),
            ("WebFetch", r#"{"url": "https://caniuse.com/css-container-queries", "prompt": "Support table"}"#, Response(ok)),
            ("WebFetch", r#"{"url": "https://web.dev/articles/container-queries", "prompt": "Summarize"}"#, Response(ok)),
            ("WebFetch", r#"{"url": "https://css-tricks.com/a-primer-on-css-container-queries/", "prompt": "Examples"}"#, Response(ok)),
            ("Bash", r#"{"command": "npm test", "description": "Run the test suite"}"#, Fail),
            (
                "Bash",
                r#"{"command": "export STRIPE_KEY=sk_live_51Hdemo0000000000000 && npm run deploy:preview", "description": "Deploy a preview"}"#,
                Approve,
            ),
            ("mcp__github__create_pull_request", r#"{"owner": "demo", "repo": "landing-page", "title": "Responsive hero"}"#, Response(ok)),
            ("mcp__claude_ai_Gmail__search_threads", r#"{"query": "from:design subject:hero feedback"}"#, Response(ok)),
            ("Task", r#"{"description": "Review the diff for accessibility issues", "prompt": "private"}"#, Response(ok)),
            (
                "Artifact",
                r#"{"title": "Landing page preview", "file_path": "/Users/demo/projects/landing-page/preview.html"}"#,
                Response(ok),
            ),
            ("CronCreate", r#"{"cron": "0 9 * * 1", "prompt": "private"}"#, Response(ok)),
        ],
        true,
    )?;
    session(
        dir,
        &mut clock,
        "codex",
        "demo-codex-infra",
        "/Users/demo/projects/infra",
        9,
        &[
            (
                "mcp__cua_repl__js",
                r#"{"code": "await app('Weather').open()", "title": "Ouvrir l'app Météo"}"#,
                Response(r#"{"isError": false}"#),
            ),
            (
                "mcp__cua_repl__js",
                r#"{"code": "await ui.click('Paris')", "title": "Afficher les prévisions de Paris"}"#,
                Response(r#"{"isError": false}"#),
            ),
            (
                "mcp__cua_repl__js",
                r#"{"code": "await ui.click('10-day forecast')", "title": "Ouvrir le détail des prévisions"}"#,
                Response(r#"{"isError": false}"#),
            ),
            (
                "mcp__cua_repl__js",
                r#"{"code": "await ui.press('Escape')", "title": "Refermer le détail des prévisions"}"#,
                Response(r#"{"isError": false}"#),
            ),
            (
                "Bash",
                r#"{"command": ["bash", "-lc", "gcloud auth list --format='table(account,status)'"]}"#,
                Response(r#"{"exit_code": 0}"#),
            ),
            ("Bash", r#"{"command": ["bash", "-lc", "npx wrangler deploy --dry-run"]}"#, Response(r#"{"exit_code": 1}"#)),
            (
                "mcp__cloudflare__list_zones",
                r#"{"account_id": "demo-account", "api_token": "demo-token-123"}"#,
                Response(r#"{"isError": false}"#),
            ),
            ("Bash", r#"{"command": ["bash", "-lc", "sudo rm -rf /usr/local/lib/old-tool"]}"#, ApprovePending),
        ],
        false,
    )?;
    std::fs::write(dir.join(crate::watch::FILE_NAME), DEMO_RULES)?;
    Ok(())
}
