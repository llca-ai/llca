//! Demo data: a short, realistic story across three sessions over the last
//! three days, after a month of ordinary work. All values are fictitious.

use std::path::Path;

use crate::json::{self, Map, Value};
use crate::obj;
use crate::store::{record, Error};
use crate::util;

#[derive(Clone, Copy)]
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
    history(dir, &mut clock)?;
    let ok = r#"{"stdout": "output not kept"}"#;
    session(
        dir,
        &mut clock,
        "codex",
        "demo-codex-shop",
        "/Users/demo/projects/shop-api",
        3 * 24 * 60 + 115, // three days ago
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
        24 * 60 + 48, // yesterday
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
                r#"{"code": "await app('Weather').open()", "title": "Open the Weather app"}"#,
                Response(r#"{"isError": false}"#),
            ),
            (
                "mcp__cua_repl__js",
                r#"{"code": "await ui.click('Paris')", "title": "Show the forecast for Paris"}"#,
                Response(r#"{"isError": false}"#),
            ),
            (
                "mcp__cua_repl__js",
                r#"{"code": "await ui.click('10-day forecast')", "title": "Open the 10-day forecast"}"#,
                Response(r#"{"isError": false}"#),
            ),
            (
                "mcp__cua_repl__js",
                r#"{"code": "await ui.press('Escape')", "title": "Close the forecast"}"#,
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

/// The projects of the month before the story, with files they change and
/// commands they run.
const PROJECTS: [(&str, &[&str], &[&str]); 6] = [
    (
        "shop-api",
        &["src/cart.py", "src/orders.py", "src/api/routes.py", "tests/test_orders.py"],
        &["python -m pytest -q", "ruff check .", "stripe events list --limit 5", "docker compose up -d db", "supabase db diff"],
    ),
    (
        "landing-page",
        &["src/pages/index.tsx", "src/components/Pricing.tsx", "src/hero.css", "tests/hero.spec.ts"],
        &["npm run build", "npm test", "npx eslint src", "vercel deploy --prebuilt", "open -a Figma"],
    ),
    (
        "infra",
        &["main.tf", "modules/dns/main.tf", "k8s/deploy.yaml", "Makefile"],
        &[
            "terraform plan",
            "terraform fmt -check",
            "gcloud run services list",
            "npx wrangler deploy --dry-run",
            "kubectl get pods -n staging",
        ],
    ),
    (
        "docs-site",
        &["docs/getting-started.md", "docs/api.md", "astro.config.mjs", "src/layouts/Docs.astro"],
        &["npm run build", "npx markdownlint docs", "netlify deploy --dir dist", "npx linkinator dist"],
    ),
    (
        "mobile-app",
        &["app/(tabs)/index.tsx", "app.json", "src/hooks/useCart.ts", "src/api/client.ts"],
        &["npx tsc --noEmit", "npx expo-doctor", "eas build --platform ios --profile preview", "npm test -- --watch=false"],
    ),
    (
        "data-pipeline",
        &["dags/orders.py", "models/daily_orders.sql", "tests/test_transform.py", "pyproject.toml"],
        &[
            "pytest tests -q",
            "dbt run --select daily_orders",
            "bq query --use_legacy_sql=false 'select count(*) from demo.orders'",
            "gsutil ls gs://demo-exports/",
        ],
    ),
];

const SHARED_COMMANDS: [&str; 6] =
    ["git status --short", "git diff --stat", "git log --oneline -5", "rg TODO", "gh pr create --fill", "gh run list --limit 5"];

const PAGES: [&str; 8] = [
    "https://react.dev/reference/react/useEffect",
    "https://nextjs.org/docs/app/building-your-application/routing",
    "https://docs.python.org/3/library/asyncio-task.html",
    "https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API",
    "https://registry.terraform.io/providers/hashicorp/google/latest/docs",
    "https://docs.expo.dev/build/introduction/",
    "https://stackoverflow.com/questions/demo/flaky-playwright-test",
    "https://docs.getdbt.com/docs/build/incremental-models",
];

/// Connected apps (MCP): tool name and arguments, `{project}` replaced.
const APPS: [(&str, &str); 9] = [
    ("mcp__github__list_issues", r#"{"owner": "demo", "repo": "{project}", "state": "open"}"#),
    ("mcp__github__get_pull_request", r#"{"owner": "demo", "repo": "{project}", "pull_number": 42}"#),
    ("mcp__linear__create_issue", r#"{"team": "Core", "title": "Follow up on the flaky test in {project}"}"#),
    ("mcp__sentry__get_issue", r#"{"issue": "DEMO-4821"}"#),
    ("mcp__claude_ai_Notion__search", r#"{"query": "release checklist"}"#),
    ("mcp__supabase__list_tables", r#"{"schemas": ["public"]}"#),
    ("mcp__vercel__list_deployments", r#"{"project": "{project}"}"#),
    ("mcp__figma__get_file", r#"{"file": "Landing v3"}"#),
    ("mcp__claude_ai_Slack__slack_send_message", r##"{"channel": "#releases", "text": "private"}"##),
];

/// A month of ordinary sessions before the story, so that the 30-day view
/// looks like real use. The same every time (fixed pseudo-random sequence).
fn history(dir: &Path, clock: &mut Clock) -> Result<(), Error> {
    let mut seed: u64 = 0x11ca_de30;
    let mut pick = move |n: usize| -> usize {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 33) as usize) % n
    };
    for day in (4..30i64).rev() {
        let sessions = [0, 1, 2, 2, 2, 3, 3][pick(7)];
        for s in 0..sessions {
            let agent = if pick(5) < 3 { "claude-code" } else { "codex" };
            let (project, files, commands) = PROJECTS[pick(PROJECTS.len())];
            let calls: Vec<(String, String, Outcome)> =
                (0..6 + pick(14)).map(|_| ordinary_call(agent, project, files, commands, &mut pick)).collect();
            let calls: Vec<(&str, &str, Outcome)> = calls.iter().map(|(t, i, o)| (t.as_str(), i.as_str(), *o)).collect();
            let start = day * 24 * 60 + (s as i64) * 150 + pick(240) as i64;
            let sid = format!("demo-{agent}-{project}-{day}-{s}");
            session(dir, clock, agent, &sid, &format!("/Users/demo/projects/{project}"), start, &calls, true)?;
        }
    }
    Ok(())
}

fn ordinary_call(
    agent: &str,
    project: &str,
    files: &[&str],
    commands: &[&str],
    pick: &mut impl FnMut(usize) -> usize,
) -> (String, String, Outcome) {
    use Outcome::*;
    let codex = agent == "codex";
    let ok = if codex { r#"{"exit_code": 0}"# } else { r#"{"stdout": "output not kept"}"# };
    let failed = if codex { Response(r#"{"exit_code": 1}"#) } else { Fail };
    let path = |file: &str| format!("/Users/demo/projects/{project}/{file}");
    let shell = |command: &str| {
        let input = if codex {
            obj![("command", Value::Array(vec![Value::str("bash"), Value::str("-lc"), Value::str(command)]))]
        } else {
            obj![("command", command), ("description", "Run a command")]
        };
        ("Bash".to_string(), input.to_compact())
    };
    let roll = pick(100);
    let (tool, input, outcome) = match roll {
        0..=29 => {
            let command = if pick(3) == 0 { SHARED_COMMANDS[pick(SHARED_COMMANDS.len())] } else { commands[pick(commands.len())] };
            let (tool, input) = shell(command);
            (tool, input, if pick(12) == 0 { failed } else { Response(ok) })
        }
        30..=33 => {
            let branch = if pick(4) == 0 { "main" } else { "feature/next" };
            let (tool, input) = shell(&format!("git push origin {branch}"));
            (tool, input, Approve)
        }
        34..=58 => {
            let file = files[pick(files.len())];
            if codex {
                let patch = format!("*** Begin Patch\n*** Update File: {file}\n@@\n-a\n+b\n*** End Patch");
                ("apply_patch".to_string(), obj![("command", patch)].to_compact(), Response(r#"{"success": true}"#))
            } else {
                let input = obj![("file_path", path(file)), ("old_string", "private"), ("new_string", "private")];
                ("Edit".to_string(), input.to_compact(), Response(ok))
            }
        }
        59..=78 => {
            let file = files[pick(files.len())];
            if codex {
                let (tool, input) = shell(&format!("sed -n '1,120p' {file}"));
                (tool, input, Response(ok))
            } else if pick(2) == 0 {
                ("Read".to_string(), obj![("file_path", path(file))].to_compact(), Response(ok))
            } else {
                ("Grep".to_string(), obj![("pattern", "TODO|FIXME"), ("path", "src")].to_compact(), Response(ok))
            }
        }
        79..=87 if !codex => {
            let url = PAGES[pick(PAGES.len())];
            ("WebFetch".to_string(), obj![("url", url), ("prompt", "Summarize")].to_compact(), Response(ok))
        }
        _ => {
            let (tool, input) = APPS[pick(APPS.len())];
            let response = if codex { r#"{"isError": false}"# } else { ok };
            (tool.to_string(), input.replace("{project}", project), if pick(15) == 0 { failed } else { Response(response) })
        }
    };
    (tool, input, outcome)
}
