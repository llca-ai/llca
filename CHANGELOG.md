# Changelog

Versions before the first public release are listed for the record. After an
upgrade, run `llca init --global --agent all` so that the hooks use the new
version: `llca doctor` tells you when they are behind.

## [0.7.6] - 2026-10-13

First public release.

- **Retention.** Events older than 90 days are deleted by default, and their
  text is overwritten in the file. `llca retention [DAYS | off]` shows or
  changes the setting. The collector deletes at most 2,000 old events per hour,
  and `llca open` deletes the rest when it starts. `llca doctor` shows the
  setting.
- `llca demo` shows a month of sample activity across six projects, so that
  the 30-day view looks like real use. The demo dashboard opens on the last
  30 days; `llca open` still opens on the last 7 days.
- README: screenshots of the dashboard and a diagram of how llca works.

## [0.7.5] - 2026-10-12

- Renamed from AgentLedger to **llca** (Local Ledger for Coding Agents). Data
  now lives in an `llca` folder.
- **Linux support.** Data goes in `$XDG_DATA_HOME/llca` (by default
  `~/.local/share/llca`), and files open with `xdg-open`.
- Events up to 64 MB are recorded; before, events over 2 MB were dropped. A
  larger event is read to the end and logged as an error, so the agent is
  never disturbed.
- A folder chosen with `--data-dir` or `LLCA_HOME` keeps its permissions. Only
  a folder llca creates is made private. `llca doctor` warns when other users
  can open the journal's folder.
- `llca init --project` keeps Codex's project hooks out of git, through
  `.git/info/exclude`.
- Dashboard theme switch (Auto, Light, Dark).
- Code formatted with rustfmt. CI runs on macOS and Linux.

## [0.7.4] - 2026-10-11

- Services show their real logo in its brand colour: about 115 logos from
  Simple Icons, all CC0, stored in the binary.
- The Overview's key figures are shown on one line. Each service shows the
  date it was last used.
- `redact` records the kind of each secret it masks. It now checks that the
  old text has left the journal's write-ahead log, and says so when it could
  not.

## [0.7.3] - 2026-10-11

- The Overview is computed in a single reading and cached until something
  changes. An index on time was added.
- On very large journals, the map keeps the most recent events.

## [0.7.2] - 2026-10-11

- Every Activity filter now runs on the server, over the whole period, not
  just the latest 200 events.
- New redaction rule kind: `password-option`.

## [0.7.1] - 2026-10-11

- "Open the rules file" button in the dashboard.

## [0.7.0] - 2026-10-11

- **Your rules** (`watch.rules`): flag the actions that matter to you, by
  file, command, tool or service. They are shown in "Worth a look", in
  Activity and in `llca doctor`.

## [0.6.x] - 2026-10-10 to 2026-10-11

- Online services: a badge on every action, a Service filter, and an Online
  services card.
- `doctor` gives a readable checkup and shows the command that fixes each
  problem. `--json` gives the same report for scripts.
- Warns when the hooks run an older collector than the installed version.
- "Dismiss" on the secret alert.
- Many redaction fixes:
  - cookies, other `Authorization` schemes, passphrases, PGP keys, split
    private keys;
  - 17 more token formats;
  - a `header` rule kind.

## [0.6.0] and earlier

- 0.6: the "Where the agents went" map, and `rules/services.rules`.
- 0.5: the kind of each masked secret is recorded, with an alert when a
  secret went through an agent.
- 0.4: redaction rules in `rules/redaction.rules`, with test cases, and
  `audit`.
- 0.3: rewritten in Rust as one binary with no dependencies.
- 0.1 to 0.2: Python prototype.
