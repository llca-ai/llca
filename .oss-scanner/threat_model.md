# llca threat model

llca ("Local Ledger for Coding Agents") is a local, private journal of what the coding agents
Codex and Claude Code do on a developer's computer (macOS and Linux). It is a single Rust binary
with no third-party crates. SQLite is the system library, used through FFI (`src/sqlite.rs`).

How it works:

1. `llca init` adds hooks to `~/.claude/settings.json` and `~/.codex/hooks.json` (or per project)
   and copies the binary to the data folder (`bin/llca`). This copy is the "collector".
2. On every tool call, the agent runs `llca hook` and writes a JSON event to its stdin.
3. The collector redacts secrets **before** writing the event to `events.sqlite3`
   (`src/redact.rs`, rules in `rules/redaction.rules`, test cases in `rules/cases.txt`).
4. `llca open` serves a dashboard on `127.0.0.1` that shows the journal in plain language
   (`src/server.rs`, `static/`).

The collector must never block or slow down the agent: on any error it records the error
(without the event content) in `collector-errors.log` and exits normally.

## Adversarial inputs

The agent is not trusted. It can be steered by prompt injection from files, web pages or MCP
servers, so everything it sends to the hook must be treated as attacker-controlled:

- **Hook stdin** (`llca hook`, `src/cli.rs`, `src/store.rs`): arbitrary JSON up to 64 MB, with
  commands, file paths, file contents, MCP tool names and arguments, URLs.
- **Event content rendered in the dashboard** (`static/app.js`): commands, paths, tool names,
  service names. The dashboard must never execute or inject it as HTML (it uses `textContent`).
- **Redaction**: a common secret format stored in clear in `events.sqlite3` is a security bug.
  The same applies to content that should never be kept (heredocs, file contents, `write_stdin`
  text, tool outputs). Redaction is best-effort for unknown formats; well-known formats
  (API keys, tokens, private keys, passwords in options, `Authorization`/`Cookie` headers,
  signed URLs) must be redacted.

The local HTTP server must resist other origins:

- Any web page the user visits (CSRF, DNS rebinding, cross-origin reads). The server binds to
  `127.0.0.1`, uses a per-launch token, checks `Host` and `Origin`, sends a strict CSP, and only
  accepts writes as POST (`/api/secret-alert/dismiss`, `/api/secret-alert/restore`,
  `/api/watch/open`, `/api/theme`).
- `/api/watch/open` runs `open` / `xdg-open` on a fixed path. Request data must never reach a
  command line.

Installation (`src/install.rs`) edits the user's agent configuration files: it must refuse
symlinks, keep backups, keep other hooks intact and never corrupt the JSON.

Other local files (`watch.rules`, `retention`, `dashboard-theme`) are written by the user and
are trusted, but malformed content must not crash the collector or the server.

## Out of scope

- Attackers who already run code as the same user: they can read the journal and the agent
  configuration directly. The database is not encrypted (documented).
- Secrets seen by the model provider: the agent sends them to OpenAI or Anthropic before llca
  sees them. llca can only record that it happened and warn the user.
- The agents themselves (Codex, Claude Code) and the hook protocol.
- The demo site (`scripts/build-demo-site.mjs`, `scripts/demo-site/`), which only serves static
  demo data.
- Missing redaction of rare or ambiguous formats that `llca audit` is meant to surface.

## Severity rubric

- **Critical**: a web page or remote party can read the journal, change it, or run code through
  the dashboard or the collector.
- **High**: a common secret format is stored in clear; the collector blocks, hangs or crashes the
  agent; HTML or script injection in the dashboard from event content; `init` corrupts or
  replaces the user's agent configuration.
- **Medium**: redaction gap for a less common but documented format; a crafted event makes the
  dashboard unusable (crash, unbounded memory or CPU); journal files readable by other local users.
- **Low**: everything else, including information leaks with no secret involved.

## Reports and patches

- Please include a minimal reproducer: the hook JSON (with made-up secret values only), or the
  HTTP request.
- Patches should be minimal. For a redaction bug, add a case to `rules/cases.txt` that fails
  before the fix and passes after. Prefer a new line in `rules/redaction.rules` over code changes.
- `cargo test --release` must pass. Group reports by root cause (one report per missing redaction
  rule, not one per example).
