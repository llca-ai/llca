# llca · v0.7

**Local Ledger for Coding Agents.** A **local, private** log of what **Codex** and **Claude Code** do on your machine: every tool call, in plain words, with a dashboard. One self-contained binary, no account, no server, nothing sent anywhere.

Written in Rust (versions 0.1–0.2 were a Python prototype). Runs on **macOS** and **Linux**.

## Why Rust, and why no dependencies

- **One binary, stable path.** `init` copies the binary into the data folder (`bin/llca`, see [Where the data lives](#where-the-data-lives)) and the hooks call that copy. Upgrading Homebrew, Python or moving the source folder can no longer break the hooks.
- **Fast.** The collector runs on every agent action. It handles an event in about 3–5 ms (the Python version took 30–100 ms, mostly interpreter start-up).
- **Nothing to audit but this repository.** There are no third-party crates: JSON, the HTTP server, secret redaction and SQLite bindings are written here (about 2,000 lines), and SQLite itself is the system library (`libsqlite3`, part of macOS and of every Linux distribution). For a tool that sees what your agents run, that matters.

## Build and install

You need Rust (`brew install rust` or [rustup](https://rustup.rs)) and:

- on **macOS**, the Xcode Command Line Tools (`xcode-select --install`);
- on **Linux**, a C toolchain and the SQLite development files, used to link against the system SQLite:
  - Debian, Ubuntu: `sudo apt install build-essential libsqlite3-dev`
  - Fedora: `sudo dnf install gcc sqlite-devel`
  - Arch: `sudo pacman -S base-devel sqlite`

```sh
cargo test                       # 62 tests
cargo build --release            # → target/release/llca
./target/release/llca demo
```

With Homebrew (local tap, nothing published):

```sh
sh scripts/package.sh            # tests, builds dist/llca-0.7.6.tar.gz, writes Formula/, updates the local tap
brew reinstall local/llca/llca    # or: brew install local/llca/llca
llca init --global --agent all
llca doctor
llca open
```

`package.sh` also clears Homebrew's cached archive, so `brew reinstall` always builds what you just packaged.

### Where the data lives

| | macOS | Linux |
|---|---|---|
| Journal, collector copy, your rules | `~/Library/Application Support/llca/` | `$XDG_DATA_HOME/llca/` (by default `~/.local/share/llca/`) |
| Codex hooks | `~/.codex/hooks.json` | same |
| Claude Code hooks | `~/.claude/settings.json` | same |

`LLCA_HOME` or `--data-dir` choose another folder. A folder llca creates is private to you (`700`) and its files are `600`; a folder you choose keeps its own permissions, and `llca doctor` says if other users can open it.

On Linux, the dashboard and the rules file open with `xdg-open`. Application icons, and the applications recognised in commands (`/Applications/…`, `open -a`, AppleScript), are macOS features; the rest is the same.

## Commands

```sh
llca init --global [--agent codex|claude|all]      # default: codex
llca init --project /path/to/project [--agent …]   # hooks for one project only
llca doctor [--project PATH] [--agent …] [--json]   # checkup: what works, what to fix (exit code 1 if anything)
llca retention [DAYS | off]                        # how long events are kept (default: 90 days)
llca logs [--json] [--search TEXT] [--session ID] [--agent codex|claude] [--limit N]
llca open [--port N] [--no-browser]                 # dashboard, until Ctrl+C
llca demo                                           # dashboard with sample data
llca redact                                         # apply today's redaction to events already recorded
llca audit                                          # list kept values that look like secrets, with the rule to add
llca uninstall --global --agent all                 # removes hooks, keeps the journal
```

`--data-dir PATH` (before the command) or `LLCA_HOME` change where the journal lives. `--codex-dir` and `--claude-dir` point `init`, `uninstall` and `doctor` at another configuration folder, for testing. `CODEX_HOME` and `CLAUDE_CONFIG_DIR` are honoured.

## How it works

- **Codex:** seven hooks in `~/.codex/hooks.json` (SessionStart, PreToolUse, PermissionRequest, PostToolUse, Stop, SessionEnd, Interrupt). Codex runs a hook only after you trust it in `/hooks`, and only in sessions started afterwards. With `--project`, they go to `<project>/.codex/hooks.json`; since the hook names a path on your computer, `init` lists that file in `.git/info/exclude` (git's local ignore list: the repository does not change), and warns if the file is already in the repository.
- **Claude Code:** eight hooks under the `hooks` key of `~/.claude/settings.json` (adds PermissionDenied and PostToolUseFailure, no Interrupt). With `--project`, they go to `.claude/settings.local.json`, which is not committed. If `/doctor` in Claude Code reports several installations, make sure the one on your `PATH` is up to date.
- **The collector copy.** The hooks call `bin/llca` in the data folder, a copy made by `init`, so that upgrading or moving llca never breaks them. The flip side: after an upgrade, the hooks keep the old copy (and its older redaction rules) until you run `llca init --global --agent all` again. `doctor` and the dashboard compare the copy's version with their own and tell you when it is behind; `doctor` then exits with code 1.
- **The collector** (`llca hook`) reads one event on stdin, redacts it, appends it to SQLite and exits with code 0. It never allows, denies or changes anything: for Codex it prints `{}`, for Claude Code nothing at all. A failure is logged to `collector-errors.log` without any of the event's content.
- **Cost to the agent.** The agent waits for each hook, two or three per tool call. Measured on Linux with a 150,000-event journal: about 4 ms per hook (half of it starting the program), the same with an empty journal, and about 5 ms while the dashboard reads the whole history (the journal uses SQLite's WAL mode, where reading never blocks writing). If the journal stays busy for more than 0.5 s, the collector gives up, notes it in `collector-errors.log` and lets the agent go on.
- **The dashboard** (`open`) is a small HTTP server on `127.0.0.1` that lives until Ctrl+C. Its API needs a token created for each launch; it checks the Host and Origin headers and sends a strict Content-Security-Policy. The page is embedded in the binary. The page asks for fresh figures every 4 seconds while it is visible; the overview is computed in one reading of the period and kept until the journal or your rules change, so an idle dashboard costs almost nothing (150,000 events: about 1.5 s to compute, 20 ms when nothing changed).

The dashboard has two pages, sharing one row of filters (period: today, 7, 30 or 90 days, all time or custom dates; agent; project):

- **Overview**: key figures (actions, sessions, commands, file changes, failures), actions per day stacked by agent (per week beyond three months, with a table view), and ranked lists of what the agents did, what is worth a look, projects and applications. Every figure and row opens the activity list with the matching filter.
- **Activity**: the detailed list below, with more filters (kind of action, result, service, your rules, search). Filters are applied by the server, so they search the whole period, not only the most recent events: the server sends every tool call that may match, and the page applies the exact rule. When few actions match, the page looks further back on its own; "Load earlier activity" keeps going.

**Where the agents went.** A map on the overview links each agent to the applications, projects, online services and websites it reached, with a line as thick as the number of actions. A line is drawn only when the journal shows it: an application from `/Applications/…`, `open -a` or AppleScript; a project from the session's folder; a service from a connected app (`mcp__cloudflare__…`), a command line tool (`gcloud`, `wrangler`, `gh`…) or a URL's domain; any other domain is a website. Service names are in [`rules/services.rules`](rules/services.rules). Each group shows its three most used resources and a button for the rest; clicking a resource shows who used it, when, with what result and a few example commands; clicking a line opens those actions. A table view lists the same data, and on a phone the map becomes a list. Beyond 200,000 events in the period, the map is drawn from the most recent 200,000 (the figures still count every event). A service reached through Computer Use in a browser shows as Computer Use, since the agent does not report the app.

**Online services.** Every action that reached an online service carries that service's badge: its logo in its brand colour (about 115 services, from [Simple Icons](https://simpleicons.org), CC0, copied into the binary: nothing is loaded from the internet), or two letters in a colour of its own for a service without a logo (AWS, Azure, Slack, OpenAI… whose owners had their logo removed from Simple Icons), the same in the activity list, the session recaps, the details, the map and the "Online services" card of the overview. A service is recognised from a command line tool (`gcloud`, `bq`, `gh`, `stripe`, `wrangler`…), a connected app (`mcp__github__…`, `mcp__claude_ai_Gmail__…`) or a URL's domain, with the rules of [`rules/services.rules`](rules/services.rules). The activity list has a "Service" filter, applied by the server, so it finds older actions too.

**Secrets seen by the agents.** When llca masks a secret, it also records what kind it was ("GitHub token", "URL parameter \"sig\"", "Value of \"API_TOKEN\"", never the value). If a secret was masked, the agent had it in clear, and so did the service that runs its model. The overview then opens with an alert listing the kinds, the agents and the number of actions; the "Secrets" filter shows those actions, and the details explain what happened and suggest rotating long-lived credentials. Events recorded before 0.5 show "kind not recorded".

Once you have dealt with them, **Dismiss** hides the alert (with an Undo right after). The choice is stored in the journal, so it lasts across launches. It covers the actions the alert showed, for the period, agent and project selected: a secret recorded later brings the alert back, counting only the new actions. Dismissed actions still count in "Worth a look" and stay under the "Secrets" filter.

The activity list tells the story in plain words: one line per action ("Ran `npm test`", "Edited `src/app.js`", "Used GitHub to create pull request"), grouped by session, with a final status. A program given by its full path is shown by its name ("Ran `arduino-cli version`"); the full command stays in the details.

Each session starts with a short recap: the files changed (exact paths from file-editing tools) and the **applications** used. An application is recognised when a command runs something inside `/Applications/…/X.app` (also `~/Applications` and `/System/Applications`), uses `open -a X`, or AppleScript's `tell application "X"`. Its icon is read from the app bundle on this Mac (`sips`, or `qlmanage` for apps with an asset catalog), converted to PNG and cached in the data folder; unknown apps get a letter badge. The "Applications" filter lists these actions.

**Computer Use** (the agent operating apps on this Mac, e.g. Codex's `mcp__cua_repl__js`) shows as "Controlled your Mac — <step>", using the short title the agent gives each step. Consecutive steps in the same turn are grouped into one line, with every step listed in the details. The script behind each step is never kept, so the app's name is only known if the agent mentions it in a title.

**Your rules.** To notice what matters to *you* ("an agent touched production", "it read a `.env` file", "it sent an email"), write one line per thing to watch in `watch.rules`, next to your journal (`init` creates it, with examples; the "Open the rules file" button in "Worth a look" opens it in your text editor):

```
file      .env                          # Touched a .env file
alert command kubectl * --context prod  # Touched production
tool      mcp__*gmail*__send*           # Sent an email
service   Stripe                        # Used Stripe
```

`file` matches a path the agent read or edited, or named in a command; `command` a shell command as recorded; `tool` a tool name; `service` an online service by its dashboard name. `*` matches anything, case is ignored, `~/` is your home folder. Matching actions are listed under "Your rules" in "Worth a look" (amber, or red with `alert`), and marked in the activity list, where each rule is also a filter. These rules only *read* the journal when the dashboard shows it: they never block an agent, never change what is recorded, and apply to your whole history as soon as you save the file. A line with a mistake is skipped and named on the overview and by `doctor`. (Masking secrets is a different job, done before anything is written: see below.)

"Worth a look" lists failures, denials, hidden secrets, Computer Use, risky commands (sudo, recursive deletes, git push, piping a download into a shell, publishing, deploying), data sent outside this computer (Claude Code `Artifact`, `ShareOnboardingGuide`) and work scheduled to run later (`CronCreate`, `ScheduleWakeup`, `RemoteTrigger`). These flags are simple heuristics, not a security verdict.

## What is kept, and what is not

Kept: agent, session, tool name, redacted arguments, working folder, permission mode, time, and a structured result (exit code, success flag). Not kept: prompts, model replies, tool output, file contents (patches and Edit/Write keep the file path only), error messages.

Secrets are redacted **before** anything is written, and replaced by `[REDACTED]`. The rules are in [`rules/redaction.rules`](rules/redaction.rules), a plain-text file compiled into the binary, and each one is proven by a case in [`rules/cases.txt`](rules/cases.txt). They cover:

- secret-named fields (`password`, `token`, `api_key`, `passphrase`, `DB_PASS`…) and `KEY=value`, `--token value`, `"token": "…"` in text, including JSON escaped inside a string, and `config set NAME VALUE` (`git config`, `aws configure`);
- labels in plain language ("mot de passe : …", `mdp="…"`, "clé API : …", `pass:` for openssl);
- credential headers, whole value: `Cookie`, `Set-Cookie`, `Authorization` (any scheme), `Proxy-Authorization`;
- signed or authenticated URL parameters (`sig`, `X-Amz-Signature`, `access_token`, `code`…), also inside an encoded redirect URL;
- known token formats (OpenAI/Anthropic, GitHub, Google, AWS, Slack, Stripe, GitLab, npm, Hugging Face, PyPI, SendGrid, DigitalOcean, Shopify, Supabase, xAI, Groq, Replicate, Linear, Notion, Vault, Postman, Atlassian, Twilio, Mailgun, JWT);
- passwords given as a short option, for the programs listed in the rules (`mysql -p`, `docker login -p`, `redis-cli -a`);
- in code: `Authorization: Bearer …`, credentials in URLs, `curl -u`, `sshpass -p`, private key blocks (PGP too, and keys cut into lines or missing their END line).

Contents are not kept and become `[CONTENT NOT KEPT]`: heredoc bodies (`cat <<EOF > file`), file contents, and text typed into a running process (Codex `write_stdin`).

Redaction is best-effort: a command can still embed something unrecognised. Two commands help keep it up to date:

- `llca audit` lists recorded values that look random (letters and digits mixed, high entropy) and that no rule masks, shortened to four characters, with the rule that would mask them. Identifiers such as UUIDs, commit hashes and `*_id` fields are skipped. It changes nothing.
- `llca redact` applies the current rules to events already in the journal, records the kinds of what it masks (as the collector does), and wipes the old text from free space and from the WAL file. If another program keeps the journal open (usually the dashboard), the WAL cannot be emptied: it says so and exits with code 1; close the dashboard and run it again.

To add a rule, see [CONTRIBUTING.md](CONTRIBUTING.md): one line in the rules file, one case in the cases file, `cargo test`.

Files are private to your user (folders `0700`, files `0600`). The database is not encrypted.

**Retention.** Events older than **90 days** are deleted, and their text is overwritten in the file. The collector deletes a few at a time, at most once an hour, so that no agent waits; `llca open` deletes the whole backlog when it starts. `llca retention` shows the setting, `llca retention 30` keeps 30 days (older events are deleted at once), `llca retention off` keeps everything.

## Limits

- Service logos are trademarks of their owners, shown only to identify each service. The Codex logo in the side menu is a trademark of OpenAI, shown only to identify Codex; unlike the service logos, it is not CC0.

- Permission requests are logged, but neither agent reports your final decision.
- Only local sessions run hooks. Cloud-run conversations and the ChatGPT and Claude chat apps are not observed.
- Hooks see a tool call's arguments, not what happens inside a shell command or at a remote provider.
- Observations are not tamper-proof evidence and not a security control.

## Development

```sh
cargo test                 # unit tests (JSON, redaction, utilities) + end-to-end tests
cargo clippy --all-targets -- -D warnings
cargo fmt                  # formatting (rustfmt.toml: lines up to 140 characters)
```

Every push and pull request runs the same three checks on macOS and Linux ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)), plus a build with the oldest supported Rust (1.89).

The end-to-end tests cover: preserving existing configuration (RTK, other settings), repeatable install, backups, uninstall, the packaged collector run through a shell (paths with spaces and apostrophes), redaction before storage, statuses for both agents, the silent Claude Code collector, concurrent writes, pagination, HTTP access control and migration of a v0.1 journal. Redaction was also checked against the Python implementation on about 3,000 generated commands.

Found a secret that slipped through, or another security issue? See [SECURITY.md](SECURITY.md): please report it privately. llca is released under the [MIT licence](LICENSE).

Protocol references: [Codex hooks](https://learn.chatgpt.com/docs/hooks) and [Claude Code hooks](https://code.claude.com/docs/en/hooks), consulted on 10 October 2026.
