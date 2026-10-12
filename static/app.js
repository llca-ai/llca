// llca dashboard. Turns raw hook events into a readable story:
// one line per action, grouped by session, in plain words.
const $ = id => document.getElementById(id);
const fragment = new URLSearchParams(location.hash.slice(1));
const token = fragment.get('token') || sessionStorage.getItem('llca-token');
if (token) sessionStorage.setItem('llca-token', token);
history.replaceState(null, '', '/');

const AGENTS = {codex: 'Codex', 'claude-code': 'Claude Code'};
const SESSION_EVENTS = new Set(['SessionStart', 'SessionEnd', 'Stop', 'Interrupt']);
const narrow = window.matchMedia('(max-width: 1100px)');
const state = {page: 'overview', rows: [], actions: [], sessions: [], selected: null, kind: '', agent: '', session: '', project: '', result: '', service: '', rule: '', range: '7d',
  generation: 0, status: null, overview: null, dismissed: null, next: null, scanned: 0, loading: false, pages: false,
  mapView: 'map', mapFocus: null, mapExpanded: new Set()};
let searchTimer;

const agentName = id => AGENTS[id] || id || 'Agent';

function node(tag, text, cls) {
  const e = document.createElement(tag);
  if (text !== undefined && text !== null) e.textContent = text;
  if (cls) e.className = cls;
  return e;
}

async function api(path, method = 'GET') {
  const response = await fetch(path, {method, headers: {Authorization: 'Bearer ' + token}});
  if (!response.ok) {
    if (response.status === 401) throw new Error('Access expired. Run llca open again.');
    // The server's own message when it gave one ("Could not open the rules file: …").
    const reason = await response.json().then(b => b.error, () => null);
    throw new Error(reason && reason !== 'Log unavailable' ? reason : 'The log could not be read.');
  }
  return response.json();
}

// ---------- Reading events ----------

function commandText(value) {
  if (Array.isArray(value)) {
    if (value.length === 3 && /^-l?c$/.test(value[1])) return String(value[2]); // ["bash", "-lc", "git status"]
    return value.join(' ');
  }
  return typeof value === 'string' ? value : '';
}

function shortPath(path, cwd) {
  if (!path) return '';
  if (cwd && path.startsWith(cwd + '/')) return path.slice(cwd.length + 1);
  return path.replace(/^\/Users\/[^/]+/, '~');
}

const project = cwd => cwd ? cwd.split('/').filter(Boolean).pop() : 'unknown folder';

function words(name) {
  return name.replace(/[_-]+/g, ' ').replace(/([a-z])([A-Z])/g, '$1 $2').toLowerCase().trim();
}

function title(name) {
  const w = words(name);
  return w.charAt(0).toUpperCase() + w.slice(1);
}

const BRANDS = {github: 'GitHub', gitlab: 'GitLab', cloudflare: 'Cloudflare', gcp: 'Google Cloud', aws: 'AWS', notion: 'Notion', slack: 'Slack', linear: 'Linear', figma: 'Figma', supabase: 'Supabase', vercel: 'Vercel', stripe: 'Stripe'};

// A command whose program is given by its full path reads better with the
// program name only: "'/Applications/Arduino IDE.app/…/arduino-cli' version"
// becomes "arduino-cli version". The full command stays in the details.
function readableCommand(command) {
  const m = command.match(/^\s*(?:'([^']+)'|"([^"]+)"|(\S+))(.*)$/s);
  if (!m) return command;
  const program = m[1] || m[2] || m[3];
  if (!program.includes('/') || /^\.\.?\//.test(program)) return command;
  const name = program.split('/').filter(Boolean).pop();
  return name ? name + m[4] : command;
}

// Applications a command opens or runs: anything inside /Applications/X.app
// (also ~/Applications and /System/Applications), `open -a X`, and
// AppleScript's `tell application "X"`.
function appsIn(command) {
  const names = new Set();
  for (const m of command.matchAll(/\/Applications\/(?:Utilities\/)?([^\/'"\n]+?)\.app(?=[\/'"\s]|$)/g)) names.add(m[1].trim());
  for (const m of command.matchAll(/\bopen\s+(?:-[a-zA-Z]+\s+)*?-[a-zA-Z]*a\s+(?:"([^"]+)"|'([^']+)'|(\S+))/g)) {
    const name = (m[1] || m[2] || m[3]).split('/').filter(Boolean).pop().replace(/\.app$/, '').trim();
    if (name) names.add(name);
  }
  // AppleScript: osascript -e 'tell application "Mail" to activate'
  for (const m of command.matchAll(/\btell\s+app(?:lication)?\s+(?:\\?")([^"\\]+)(?:\\?")/gi)) names.add(m[1].trim());
  return [...names];
}

// Computer Use: the agent drives apps on this Mac through an MCP server
// (Codex: mcp__cua_repl__js; other clients: computer-use servers). Each call
// carries a short title written by the agent; the script itself is not kept.
function isComputerUse(tool) {
  return /^mcp__(cua|cua_repl|computer[-_]?use)(__|$)/i.test(tool || '');
}

// What the agent did, as a short sentence: a verb and its object.
function describe(row) {
  const a = row.arguments || {};
  const tool = row.tool || '';
  const cwd = row.cwd;
  const path = p => shortPath(p, cwd);
  if (row.category === 'shell') {
    const full = commandText(a.command) || commandText(a.cmd) || tool;
    return {kind: 'shell', verb: 'Ran', object: readableCommand(full), full, code: true, apps: appsIn(full)};
  }
  if (tool === 'apply_patch') {
    const files = (a.files || []).map(path);
    return {kind: 'edit', verb: 'Changed', object: files.length === 1 ? files[0] : files.length + ' files', list: files.length > 1 ? files : null, code: files.length === 1};
  }
  if (tool === 'Edit' || tool === 'MultiEdit') return {kind: 'edit', verb: 'Edited', object: path(a.file_path), code: true};
  if (tool === 'Write') return {kind: 'edit', verb: 'Wrote', object: path(a.file_path), code: true};
  if (tool === 'NotebookEdit') return {kind: 'edit', verb: 'Edited notebook', object: path(a.notebook_path), code: true};
  if (tool === 'Read' || tool === 'read_file') return {kind: 'read', verb: 'Read', object: path(a.file_path || a.path), code: true};
  if (tool === 'Grep') return {kind: 'read', verb: 'Searched files for', object: a.pattern, code: true};
  if (tool === 'Glob') return {kind: 'read', verb: 'Looked for files named', object: a.pattern, code: true};
  if (tool === 'LS') return {kind: 'read', verb: 'Listed', object: path(a.path), code: true};
  if (tool === 'WebFetch') {
    let where = a.url || '';
    try { const u = new URL(a.url); where = u.host + (u.pathname.length > 1 ? u.pathname : ''); } catch (e) { /* keep raw */ }
    return {kind: 'web', verb: 'Opened', object: where, code: false};
  }
  if (tool === 'WebSearch') return {kind: 'web', verb: 'Searched the web for', object: a.query, code: false, quote: true};
  if (isComputerUse(tool)) {
    const step = a.title || a.description || a.action || '';
    return {kind: 'computer', verb: 'Controlled your Mac' + (step ? ' —' : ''), object: step, code: false, computer: true};
  }
  if (tool.startsWith('mcp__')) {
    const [, server, ...rest] = tool.split('__');
    const known = (row.services || []).find(sv => sv.via.startsWith('Connected app'));
    const app = known ? known.name : BRANDS[(server || '').toLowerCase()] || title(server || 'an app');
    return {kind: 'mcp', verb: 'Used ' + app + ' to', object: words(rest.join(' ') || 'run a tool'), code: false};
  }
  if (tool === 'Task' || tool === 'Agent' || tool === 'spawn_agent') return {kind: 'other', verb: 'Started a helper agent', object: a.description || '', code: false};
  if (tool === 'TodoWrite' || tool === 'update_plan') return {kind: 'other', verb: 'Updated its plan', object: '', code: false};
  if (tool === 'Artifact') return {kind: 'other', verb: 'Published a page on claude.ai', object: a.title || '', code: false};
  if (tool === 'ShareOnboardingGuide') return {kind: 'other', verb: 'Shared an onboarding guide', object: '', code: false};
  if (tool === 'SendFeedback') return {kind: 'other', verb: 'Drafted feedback (sent only if you choose)', object: '', code: false};
  if (tool === 'CronCreate') return {kind: 'other', verb: 'Scheduled a task in this session', object: a.cron || '', code: true};
  if (tool === 'ScheduleWakeup') return {kind: 'other', verb: 'Scheduled its next run', object: '', code: false};
  if (tool === 'RemoteTrigger') return {kind: 'other', verb: 'Managed a routine on claude.ai', object: a.action || '', code: false};
  return {kind: 'other', verb: 'Used', object: tool, code: true};
}

// Things a person would want to notice, from the command text alone.
const RISKS = [
  [/\bsudo\b/, 'Runs with administrator rights'],
  [/\brm\s+(-[a-zA-Z]*[rR][a-zA-Z]*|--recursive)\b/, 'Deletes files and folders'],
  [/\bgit\s+push\b[^;&|]*\s(--force|-f)\b/, 'Force-pushes code'],
  [/\bgit\s+push\b/, 'Pushes code to a remote'],
  [/\bgit\s+(reset\s+--hard|clean\s+-[a-z]*f)/, 'Discards local changes'],
  [/\b(curl|wget)\b[^|;&]*\|\s*(sudo\s+)?(ba|z)?sh\b/, 'Runs a script downloaded from the internet'],
  [/\bchmod\s+(-R\s+)?0?777\b/, 'Makes files writable by everyone'],
  [/\b(npm|pnpm|yarn)\s+publish\b|\bdocker\s+push\b|\bgh\s+release\s+create\b/, 'Publishes a package or image'],
  [/\b(terraform\s+(apply|destroy)|kubectl\s+(apply|delete))\b|\bdeploy\b/, 'Changes something that is live'],
];

const STATUS_FLAGS = new Set(['It failed', 'It was denied', 'It asked for approval and no result was recorded']);

function flagsFor(action) {
  const flags = [];
  if (action.kind === 'shell') {
    for (const [pattern, label] of RISKS) {
      if (pattern.test(action.full || action.object) && !(label === 'Pushes code to a remote' && flags.includes('Force-pushes code'))) flags.push(label);
    }
  }
  // Known tools only: data leaving this computer, and work that continues later.
  // SendFeedback is not listed: it only drafts a report, sent if you choose.
  const tool = action.main.tool;
  if (['Artifact', 'ShareOnboardingGuide'].includes(tool)) flags.push('Sends data outside this computer');
  if (action.computer) flags.push('Controlled apps on your Mac');
  if (['CronCreate', 'ScheduleWakeup', 'RemoteTrigger'].includes(tool)) flags.push('Schedules something that runs later');
  if (action.status === 'failed') flags.push('It failed');
  if (action.status === 'denied') flags.push('It was denied');
  if (action.status === 'waiting') flags.push('It asked for approval and no result was recorded');
  if (action.secrets.length) flags.push(secretFlag(action.secrets));
  for (const w of action.watch || []) flags.push(watchFlag(w));
  return flags;
}

// Your rules (watch.rules), as worked out by the server for each event.
const WATCH_PREFIX = 'Your rule';
const watchFlag = w => WATCH_PREFIX + (w.alert ? ' (alert)' : '') + ': ' + w.label;
function watchOf(stages) {
  const seen = new Map();
  for (const st of stages) for (const w of st.watch || []) if (!seen.has(w.label)) seen.set(w.label, w);
  return [...seen.values()];
}

// Kinds of secrets masked in an action ("GitHub token"…). Events recorded
// before version 0.5 only show that something was masked.
const UNKNOWN_SECRET = 'A secret of unknown kind';
function secretsOf(stages) {
  const kinds = new Set(stages.flatMap(s => s.secrets || []));
  if (!kinds.size && stages.some(s => /\[REDACTED\]|\[SECRET MASQU/.test(JSON.stringify(s.arguments || {})))) kinds.add(UNKNOWN_SECRET);
  return [...kinds];
}
const secretFlag = kinds => (kinds.length === 1 ? 'A secret went through the agent: ' : 'Secrets went through the agent: ') + joinAnd(kinds.map(k => k === UNKNOWN_SECRET ? 'kind not recorded' : k));

// What each status means, shown on hover.
const STATUS_HELP = {
  ok: 'Success confirmed by the agent (exit code 0 or a success flag).',
  done: 'The tool returned, but the agent reported no success or failure. llca does not read the output, so it does not guess.',
  failed: 'The agent reported a failure (non-zero exit code or an error flag).',
  denied: 'The request was denied, so it did not run.',
  waiting: 'The agent asked for your approval. Your decision and the result were not recorded.',
  running: 'Started moments ago; the result is not in yet.',
  unknown: 'Started, but no result was recorded.',
};

// Final outcome of one action, from all of its steps.
function outcome(stages) {
  const events = stages.map(s => s.event);
  const last = stages[stages.length - 1];
  const asked = events.includes('PermissionRequest');
  if (events.includes('PermissionDenied')) return {status: 'denied', label: 'Denied', asked};
  if (last.status === 'error') return {status: 'failed', label: 'Failed', asked};
  if (last.status === 'success') return {status: 'ok', label: 'Done', asked};
  if (last.event === 'PostToolUse') return {status: 'done', label: 'No status reported', asked};
  if (asked) return {status: 'waiting', label: 'Needed approval', asked};
  const age = Date.now() - new Date(last.received_at).getTime();
  return age < 120000 ? {status: 'running', label: 'Running', asked} : {status: 'unknown', label: 'No result', asked};
}

function exitCode(stages) {
  for (const s of [...stages].reverse()) if (s.outcome && Number.isInteger(s.outcome.exit_code)) return s.outcome.exit_code;
  return null;
}

// Build actions (one per tool call) and sessions from raw rows.
function build(rows) {
  const ordered = [...rows].sort((a, b) => a.id - b.id);
  const byCall = new Map(), lastCall = new Map(), sessions = new Map(), actions = [];
  const sessionOf = row => {
    const key = row.agent + '|' + row.session_id;
    if (!sessions.has(key)) sessions.set(key, {key, agent: row.agent, id: row.session_id, cwd: row.cwd, model: null, start: row.received_at, end: null, last: row.received_at, actions: [], interrupted: 0});
    const s = sessions.get(key);
    s.cwd = s.cwd || row.cwd;
    s.last = row.received_at > s.last ? row.received_at : s.last;
    if (row.received_at < s.start) s.start = row.received_at;
    return s;
  };
  for (const row of ordered) {
    const session = sessionOf(row);
    if (SESSION_EVENTS.has(row.event) || !row.tool) {
      if (row.event === 'SessionStart') { session.model = row.model || session.model; session.started = row.received_at; }
      if (row.event === 'SessionEnd') session.end = row.received_at;
      if (row.event === 'Interrupt') session.interrupted += 1;
      continue;
    }
    const callKey = [row.agent, row.session_id, row.turn_id || '', row.tool].join('|');
    const key = row.tool_use_id ? [row.agent, row.session_id, row.tool_use_id].join('|') : null;
    let action = (key && byCall.get(key)) || (!key && row.event === 'PermissionRequest' ? lastCall.get(callKey) : null);
    if (!action) {
      action = {key: key || 'event-' + row.id, stages: [], session};
      if (key) byCall.set(key, action);
      actions.push(action);
      session.actions.push(action);
    }
    lastCall.set(callKey, action);
    action.stages.push(row);
  }
  for (const action of actions) {
    action.main = action.stages.find(s => s.event === 'PreToolUse') || action.stages[0];
    action.at = action.stages[0].received_at;
    action.id = action.stages[action.stages.length - 1].id;
    Object.assign(action, describe(action.main), outcome(action.stages));
    action.secrets = secretsOf(action.stages);
    action.watch = watchOf(action.stages);
    action.flags = flagsFor(action);
    // Online services, with how they were reached (from the server, same rules as the map).
    const via = new Map();
    for (const st of action.stages) for (const sv of st.services || []) if (!via.has(sv.name)) via.set(sv.name, sv.via);
    action.services = [...via.keys()];
    action.serviceVia = via;
  }
  // One line per Computer Use sequence: consecutive calls in the same turn,
  // with no other action in between, become one action with steps.
  for (const s of sessions.values()) s.actions = mergeComputerUse(s.actions);
  const merged = [...sessions.values()].flatMap(s => s.actions);
  for (const s of sessions.values()) {
    s.failed = s.actions.filter(a => a.status === 'failed' || a.status === 'denied').length;
    s.commands = s.actions.filter(a => a.kind === 'shell').length;
    // Files changed: exact paths from apply_patch, Edit, Write, NotebookEdit.
    s.files = [...new Set(s.actions.filter(a => a.kind === 'edit').flatMap(a => a.list || (a.object ? [a.object] : [])))];
    s.apps = [...new Set(s.actions.flatMap(a => a.apps || []))];
    s.services = new Map();
    for (const a of s.actions) for (const name of a.services) s.services.set(name, (s.services.get(name) || 0) + 1);
    s.flagged = s.actions.filter(a => a.flags.length).length;
  }
  return {actions: merged, sessions: [...sessions.values()].sort((a, b) => (b.last > a.last ? 1 : -1))};
}

function mergeComputerUse(list) {
  const out = [];
  for (const action of [...list].sort((a, b) => (a.at > b.at ? 1 : a.at < b.at ? -1 : a.id - b.id))) {
    const prev = out[out.length - 1];
    const sameRun = prev && prev.computer && action.computer && (prev.main.turn_id || '') === (action.main.turn_id || '');
    if (!sameRun) {
      out.push(action.computer ? Object.assign(action, {steps: [action]}) : action);
      continue;
    }
    // Fold this call into the running sequence.
    const seq = prev.steps.length === 1 ? Object.assign({}, prev, {key: prev.key + '+seq', steps: [...prev.steps]}) : prev;
    seq.steps.push(action);
    seq.stages = seq.steps.flatMap(st => st.stages);
    seq.id = action.id;
    const failed = seq.steps.filter(st => st.status === 'failed' || st.status === 'denied');
    const last = seq.steps[seq.steps.length - 1];
    Object.assign(seq, failed.length ? {status: 'failed', label: failed.length === 1 ? '1 step failed' : failed.length + ' steps failed'}
      : {status: last.status, label: last.label});
    seq.asked = seq.steps.some(st => st.asked);
    seq.secrets = [...new Set(seq.steps.flatMap(st => st.secrets))];
    seq.watch = watchOf(seq.steps.flatMap(st => st.stages));
    seq.services = [...new Set(seq.steps.flatMap(st => st.services))];
    seq.serviceVia = new Map(seq.steps.flatMap(st => [...st.serviceVia]));
    seq.flags = [...new Set(seq.steps.flatMap(st => st.flags))];
    if (seq.flags.includes('It failed') && !failed.length) seq.flags = seq.flags.filter(f => f !== 'It failed');
    out[out.length - 1] = seq;
  }
  return out;
}

// ---------- Formatting ----------

// When something was last used, short: "today 18:20", "yesterday 09:05",
// "3 days ago", then the date.
function lastUse(iso) {
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const day = new Date(iso); day.setHours(0, 0, 0, 0);
  const days = Math.round((today - day) / 86400000);
  if (days <= 0) return 'today ' + time(iso);
  if (days === 1) return 'yesterday ' + time(iso);
  if (days < 7) return days + ' days ago';
  return new Date(iso).toLocaleDateString('en-GB', {day: 'numeric', month: 'short'});
}

const time = iso => new Date(iso).toLocaleTimeString('en-GB', {hour: '2-digit', minute: '2-digit'});
const fullTime = iso => new Date(iso).toLocaleString('en-GB', {weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit', second: '2-digit'});

function dayLabel(iso) {
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const day = new Date(iso); day.setHours(0, 0, 0, 0);
  const diff = Math.round((today - day) / 86400000);
  if (diff === 0) return 'Today';
  if (diff === 1) return 'Yesterday';
  return day.toLocaleDateString('en-GB', {weekday: 'long', day: 'numeric', month: 'long'});
}

const plural = (n, one, many) => n + ' ' + (n === 1 ? one : (many || one + 's'));

function joinAnd(list) {
  if (list.length < 2) return list.join('');
  return list.slice(0, -1).join(', ') + ' and ' + list[list.length - 1];
}

function sentence(action, cls) {
  const p = node('p', undefined, cls);
  p.append(node('span', action.verb + (action.object ? ' ' : ''), 'verb'));
  if (action.object) {
    if (action.code) p.append(node('code', action.object));
    else p.append(node('span', action.quote ? '“' + action.object + '”' : action.object, 'object'));
  }
  return p;
}

// ---------- Rendering ----------

const RESULT_MATCH = {
  problem: a => a.status === 'failed' || a.status === 'denied',
  waiting: a => a.status === 'waiting' || (a.asked && a.status !== 'denied'),
  ok: a => a.status === 'ok',
  done: a => a.status === 'done',
};

function visible(action) {
  if (state.result && !RESULT_MATCH[state.result](action)) return false;
  if (!state.kind) return true;
  if (state.kind === 'attention') return action.flags.length > 0;
  if (state.kind === 'secret') return action.secrets.length > 0;
  if (state.kind === 'app') return (action.apps || []).length > 0 || Boolean(action.computer);
  return action.kind === state.kind;
}

// ---------- Application icons ----------
// Icons are served by the local dashboard (converted from the app bundle) and
// fetched with the API token, then shown from memory. Unknown apps keep a
// letter badge.
const icons = new Map(); // name → object URL, or null when unavailable
let iconRender = null;

function requestIcon(name) {
  if (icons.has(name)) return;
  icons.set(name, null);
  fetch('/api/app-icon?name=' + encodeURIComponent(name), {headers: {Authorization: 'Bearer ' + token}})
    .then(r => (r.status === 200 ? r.blob() : null))
    .then(blob => {
      if (!blob) return;
      icons.set(name, URL.createObjectURL(blob));
      clearTimeout(iconRender);
      iconRender = setTimeout(render, 50);
    })
    .catch(() => {});
}

function appIcon(name) {
  requestIcon(name);
  const url = icons.get(name);
  if (url) {
    const img = node('img', undefined, 'app-icon');
    img.src = url;
    img.alt = '';
    return img;
  }
  return node('span', name.charAt(0).toUpperCase(), 'app-icon letter');
}

function appChip(name) {
  const chip = node('span', undefined, 'app-chip');
  chip.title = 'Application: ' + name;
  chip.append(appIcon(name), node('span', name));
  return chip;
}

// Shown on hover wherever "Computer Use" stands in for an application.
const COMPUTER_USE_HELP = 'Computer Use: the agent controlled apps on this Mac by taking screenshots, clicking and typing. '
  + 'The agent does not report which app it controlled, and llca does not keep the script of each step, only its title.';

function computerChip() {
  const chip = node('span', undefined, 'app-chip computer');
  chip.title = COMPUTER_USE_HELP;
  chip.append(node('span', '↖', 'app-icon letter'), node('span', 'Computer Use'));
  return chip;
}

// ---------- Online services ----------
// Each service gets a badge: two letters and a colour of its own, the same
// everywhere (rows, session recaps, details, the map, the services card).
// The colour comes from the name, so it never changes; it avoids the red,
// amber and agent colours, and the letters always say which service it is.
const SERVICE_COLOURS = 6;
// Six colours validated together (style.css). Common services that often show
// up together get different ones; any other service gets one from its name.
const SERVICE_TONES = {
  'Google Cloud': 0, 'GitHub': 1, 'Gmail': 2, 'Stripe': 3, 'Cloudflare': 4, 'ChatGPT Sites': 5,
  'Vercel': 5, 'AWS': 2, 'Supabase': 4, 'Slack': 3, 'Notion': 0, 'Linear': 3, 'Google Drive': 4, 'Google Calendar': 5,
  'OpenAI': 4, 'Anthropic': 2, 'npm': 1, 'GitLab': 3, 'Firebase': 2, 'Expo': 0, 'Sentry': 1, 'Figma': 5, 'Atlassian': 0,
};
function serviceTone(name) {
  if (name in SERVICE_TONES) return 'svc-' + SERVICE_TONES[name];
  let h = 0;
  for (const c of name) h = (h * 31 + c.codePointAt(0)) >>> 0;
  return 'svc-' + (h % SERVICE_COLOURS);
}

// "Google Cloud" → GC, "GitHub" → GH, "Stripe" → St, "AWS" → AW.
function serviceInitials(name) {
  const words = name.split(/\s+/).filter(Boolean);
  if (words.length > 1) return (words[0][0] + words[1][0]).toUpperCase();
  const word = words[0] || '?';
  const capitals = [...word].filter(c => c >= 'A' && c <= 'Z');
  if (capitals.length >= 2 && capitals.length < word.length) return capitals[0] + capitals[1];
  return word[0].toUpperCase() + (word[1] || '').toLowerCase();
}

// The service's logo in its own colour (static/brands.js, from Simple Icons),
// on white; a logo too pale for white goes on a dark disc. No logo: initials.
const LOGOS = typeof BRAND_LOGOS === 'object' ? BRAND_LOGOS : {};
function paleColour(hex) {
  const lin = c => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const [r, g, b] = [0, 2, 4].map(i => lin(parseInt(hex.slice(i, i + 2), 16) / 255));
  return 1.05 / (0.2126 * r + 0.7152 * g + 0.0722 * b + 0.05) < 2;
}

function serviceBadge(name, size) {
  const logo = LOGOS[name];
  if (!logo) {
    const b = node('span', serviceInitials(name), 'svc-badge ' + serviceTone(name) + (size ? ' ' + size : ''));
    b.setAttribute('aria-hidden', 'true');
    return b;
  }
  const b = node('span', undefined, 'svc-badge logo' + (paleColour(logo[0]) ? ' dark' : '') + (size ? ' ' + size : ''));
  b.setAttribute('aria-hidden', 'true');
  const svg = document.createElementNS(SVG, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  const path = document.createElementNS(SVG, 'path');
  path.setAttribute('d', logo[1]);
  path.setAttribute('fill', '#' + logo[0]);
  svg.append(path);
  b.append(svg);
  return b;
}

// The colour of a service's bars: its brand colour with a logo, else its tone.
const serviceColour = name => (LOGOS[name] ? '#' + LOGOS[name][0] : serviceTone(name));

function serviceChip(name, count) {
  const chip = node('span', undefined, 'svc-chip ' + (LOGOS[name] ? 'logo' : serviceTone(name)));
  chip.title = 'Online service: ' + name;
  chip.append(serviceBadge(name), node('span', count ? name + ' · ' + count : name));
  return chip;
}

// "Command line (gcloud)" → "gcloud", "Connected app (MCP)" → "MCP", "URL (…)" → "URL".
function shortVia(via) {
  const m = via.match(/^Command line \((.+)\)$/);
  if (m) return m[1];
  if (via.startsWith('Connected app')) return 'MCP';
  if (via.startsWith('URL')) return 'URL';
  return via;
}

// Services of the period, from the overview's map, most used first.
const periodServices = () => ((state.overview && state.overview.map && state.overview.map.resources) || []).filter(r => r.category === 'service');

function showService(name) {
  if (state.page === 'activity') {
    state.service = name;
    state.selected = null;
    render();
    refresh();
  } else {
    goToActivity({service: name});
  }
}

function renderSummary() {
  const ov = state.overview;
  $('headline').textContent = state.page === 'overview' ? 'Overview' : 'Activity';
  if (!ov) return;
  const t = ov.totals;
  const period = rangeBounds().label;
  if (!t.events) {
    $('summary').textContent = period + ': nothing recorded.';
    return;
  }
  const agents = [...new Set(ov.by_day.filter(d => d.actions).map(d => agentName(d.agent)))];
  let text = period + ': ' + (agents.length ? joinAnd(agents) : 'your agents') + ' took ' + plural(t.actions, 'action') + ' in ' + plural(t.sessions, 'session') + '.';
  if (t.failed) text += ' ' + plural(t.failed, 'action') + ' failed or ' + (t.failed === 1 ? 'was' : 'were') + ' denied.';
  $('summary').textContent = text;
}

function renderTimeline() {
  const list = $('events');
  list.replaceChildren();
  let shown = 0;
  for (const session of state.sessions) {
    // Most recent first, everywhere: sessions by last activity, actions inside too.
    const acts = session.actions.filter(visible).sort((a, b) => (b.at > a.at ? 1 : b.at < a.at ? -1 : b.id - a.id));
    // Sessions without any action (opened and closed) only add noise.
    if (!acts.length && state.session !== session.id) continue;
    const block = node('section', undefined, 'session ' + (session.agent || ''));
    const head = node('header', undefined, 'session-head');
    const name = node('h3');
    name.append(node('span', '', 'swatch ' + (session.agent || '')), agentName(session.agent) + ' in ', node('strong', project(session.cwd)));
    const meta = node('p', undefined, 'session-meta');
    const range = dayLabel(session.start) + ', ' + time(session.start) + (session.end ? '–' + time(session.end) : '');
    // Filtered by kind, result, service, rule or search, the server only sends
    // the matching actions: the session's totals are not known here.
    const partial = state.kind || state.result || state.service || state.rule || $('search').value;
    const parts = [range, partial ? plural(acts.length, 'matching action') : plural(session.actions.length, 'action')];
    if (session.commands && !partial) parts.push(plural(session.commands, 'command'));
    if (session.failed && !partial) parts.push(plural(session.failed, 'failure'));
    if (!session.end) parts.push(Date.now() - new Date(session.last).getTime() < 600000 ? 'active' : 'no end recorded');
    if (session.interrupted) parts.push('interrupted ' + plural(session.interrupted, 'time'));
    meta.textContent = parts.join(', ');
    head.append(name, meta);
    // What changed, at a glance: exact file paths and the applications used.
    const usedComputer = session.actions.some(a => a.computer);
    if (session.files.length || session.apps.length || usedComputer || session.services.size) {
      const recap = node('div', undefined, 'session-recap');
      if (session.files.length) {
        const files = node('p', undefined, 'recap-line');
        files.append(node('span', plural(session.files.length, 'file') + ' changed', 'recap-label'));
        for (const f of session.files.slice(0, 4)) files.append(node('code', f));
        if (session.files.length > 4) files.append(node('span', 'and ' + (session.files.length - 4) + ' more', 'quiet'));
        recap.append(files);
      }
      if (session.apps.length || usedComputer) {
        const apps = node('p', undefined, 'recap-line');
        apps.append(node('span', session.apps.length + (usedComputer ? 1 : 0) === 1 ? 'Application' : 'Applications', 'recap-label'));
        if (usedComputer) apps.append(computerChip());
        for (const a of session.apps) apps.append(appChip(a));
        recap.append(apps);
      }
      if (session.services.size) {
        const line = node('p', undefined, 'recap-line');
        line.append(node('span', session.services.size === 1 ? 'Service' : 'Services', 'recap-label'));
        for (const [name, n] of [...session.services].sort((a, b) => b[1] - a[1])) line.append(serviceChip(name, n));
        recap.append(line);
      }
      head.append(recap);
    }
    if (!state.session) {  // when filtered, the bar above the list offers the way back
      const only = node('button', 'Only this session', 'link');
      only.onclick = () => setSession(session.id);
      head.append(only);
    }
    block.append(head);
    if (!acts.length) block.append(node('p', 'No actions recorded in this session.', 'quiet'));
    const ol = node('ol', undefined, 'actions');
    for (const action of acts) {
      shown += 1;
      const li = node('li');
      const b = node('button', undefined, 'action ' + action.status + (action.flags.length ? ' flagged' : '') + (action.secrets.length ? ' secret' : '') + (action.watch.some(w => w.alert) ? ' alert' : '') + (state.selected === action.key ? ' selected' : ''));
      b.dataset.key = action.key;
      b.setAttribute('aria-pressed', state.selected === action.key ? 'true' : 'false');
      const status = node('span', undefined, 'state ' + action.status);
      status.title = STATUS_HELP[action.status] || '';
      status.append(node('span', '', 'dot'), node('span', action.label, 'state-label'));
      b.append(node('time', time(action.at)), sentence(action, 'what'), status);
      if ((action.apps || []).length || action.computer || action.services.length) {
        const apps = node('span', undefined, 'row-apps');
        for (const name of action.services) apps.append(serviceChip(name));
        if (action.computer) {
          apps.append(computerChip());
          if (action.steps && action.steps.length > 1) apps.append(node('span', plural(action.steps.length, 'step'), 'quiet'));
        }
        for (const a of action.apps || []) apps.append(appChip(a));
        b.append(apps);
      }
      // Failure and denial already show in the status; list the other reasons.
      const reasons = action.flags.filter(f => !STATUS_FLAGS.has(f));
      if (reasons.length) b.append(node('span', reasons.map((r, i) => (i ? r.charAt(0).toLowerCase() + r.slice(1) : r)).join(', '), 'flag'));
      b.onclick = () => select(action, false);
      li.append(b);
      ol.append(li);
    }
    if (acts.length) block.append(ol);
    list.append(block);
  }
  if (!list.children.length) {
    const empty = node('div', undefined, 'empty');
    const filtered = $('search').value || state.kind || state.agent || state.session || state.service || state.rule || state.result || state.project;
    if (filtered && state.next !== null) {
      empty.append(node('strong', 'Nothing found in the last ' + state.scanned.toLocaleString('en-US') + ' events.'), node('p', 'Use “Load earlier activity” below to keep searching, or try another filter.'));
    } else if (filtered) {
      empty.append(node('strong', 'No matching activity.'), node('p', 'Try another filter or clear the search.'));
    } else if (state.rows.length) {
      empty.append(node('strong', 'No actions yet.'), node('p', 'Sessions were opened, but no agent action was recorded. Ask Codex or Claude Code to do something small, like running pwd.'));
    } else {
      empty.append(node('strong', 'Your log starts here.'),
        node('p', 'Start a new Codex or Claude Code session on this computer and ask it to do something small, like running pwd. It will appear here within seconds.'),
        node('p', 'Not seeing anything? Run llca doctor in a terminal.', 'quiet'));
    }
    list.append(empty);
  }
  return shown;
}

function renderDetail() {
  const action = state.actions.find(a => a.key === state.selected);
  const box = $('detail');
  if (!action) {
    box.replaceChildren(node('div', undefined, 'placeholder'));
    box.firstChild.append(node('p', 'Select an action to see what the agent did, where, and what happened.'));
    return;
  }
  const wrap = node('div', undefined, 'detail');
  const back = node('button', '← Back to the timeline', 'link back');
  back.onclick = () => document.querySelector('.action.selected')?.scrollIntoView({behavior: 'smooth', block: 'center'});
  const status = node('p', undefined, 'state big ' + action.status);
  status.title = STATUS_HELP[action.status] || '';
  status.append(node('span', '', 'dot'), node('span', action.label, 'state-label'));
  wrap.append(back, status, sentence(action, 'headline'));
  if (action.list) {
    const ul = node('ul', undefined, 'files');
    for (const f of action.list) ul.append(node('li', f));
    wrap.append(ul);
  }
  if (action.computer) {
    // Each step of the sequence, as titled by the agent.
    const list = node('ol', undefined, 'cu-steps');
    for (const st of action.steps || [action]) {
      const li = node('li', undefined, st.status);
      const state = node('span', undefined, 'state ' + st.status);
      state.append(node('span', '', 'dot'));
      li.append(node('time', time(st.at)), state, node('span', st.object || 'Step without a title'));
      list.append(li);
    }
    wrap.append(node('h4', (action.steps || [action]).length === 1 ? 'Step' : (action.steps.length + ' steps'), 'cu-title'), list,
      node('p', 'The agent controlled apps on this Mac by taking screenshots, clicking and typing. It does not report which app; llca keeps each step’s title, not the script it ran.', 'quiet'));
  }
  const note = action.main.arguments && action.main.arguments.description;
  if (note && action.kind !== 'other') wrap.append(node('p', 'The agent’s note: ' + note, 'agent-note'));
  if (action.secrets.length) {
    const box = node('div', undefined, 'secret-box');
    box.append(node('h4', action.secrets.length === 1 ? 'A secret went through the agent' : 'Secrets went through the agent'));
    const ul = node('ul');
    for (const k of action.secrets) ul.append(node('li', k === UNKNOWN_SECRET ? 'Kind not recorded (event recorded before version 0.5)' : k));
    box.append(ul,
      node('p', agentName(action.session.agent) + ' sent it in clear to the service that runs its model. This log keeps it masked.'),
      node('p', 'If it does not expire on its own, consider rotating it, and give agents short-lived credentials with limited rights.', 'advice'));
    wrap.append(box);
  }
  if (action.services.length) {
    const box = node('div', undefined, 'svc-box');
    box.append(node('h4', action.services.length === 1 ? 'Online service' : 'Online services'));
    for (const name of action.services) {
      const row = node('div', undefined, 'svc-line');
      const text = node('span', undefined, 'svc-text');
      text.append(node('strong', name), node('span', 'Reached through ' + (action.serviceVia.get(name) || '').replace(/^Command line/, 'the command line').replace(/^Connected app/, 'a connected app').replace(/^URL/, 'a URL'), 'quiet'));
      const all = node('button', 'See every ' + name + ' action', 'link');
      all.onclick = () => showService(name);
      row.append(serviceBadge(name, 'lg'), text);
      box.append(row, all);
    }
    wrap.append(box);
  }
  for (const w of action.watch) {
    const box = node('div', undefined, w.alert ? 'secret-box' : 'flags watch-box');
    box.append(node('h4', 'Matched your rule' + (w.alert ? ' (alert)' : '')), node('p', w.label, 'watch-label'));
    const how = node('p', undefined, 'watch-how');
    how.append('Rule ', node('code', w.rule), ', line ' + w.line + ' of watch.rules.');
    if (w.matched && !w.rule.startsWith('command')) how.append(' It matched ', node('code', w.matched), '.');
    const all = node('button', 'See every action of this rule', 'link');
    all.onclick = () => { state.rule = w.label; state.selected = null; render(); refresh(); };
    box.append(how, all);
    wrap.append(box);
  }
  const otherFlags = action.flags.filter(f => !f.startsWith('A secret went') && !f.startsWith('Secrets went') && !f.startsWith(WATCH_PREFIX));
  if (otherFlags.length) {
    const flags = node('div', undefined, 'flags');
    flags.append(node('h4', 'Worth a look'));
    const ul = node('ul');
    for (const f of otherFlags) ul.append(node('li', f));
    flags.append(ul);
    wrap.append(flags);
  }
  const facts = node('dl', undefined, 'facts');
  const code = exitCode(action.stages);
  const result = {
    ok: 'It completed successfully' + (code === 0 ? ' (exit code 0).' : '.'),
    done: 'The tool returned, but did not say whether it succeeded.',
    failed: 'It failed' + (code !== null ? ' with exit code ' + code + '.' : '.') + ' The error message is not recorded.',
    denied: 'The request was denied, so it did not run.',
    waiting: 'The agent asked for your approval. Your decision and the result were not recorded.',
    running: 'It started moments ago. The result will appear here.',
    unknown: 'It started, but no result was recorded.',
  }[action.status];
  const rowsOut = [
    ['Result', result],
    ['Agent', agentName(action.session.agent) + (action.session.model ? ' (' + action.session.model + ')' : '')],
    ['Project', action.session.cwd ? shortPath(action.session.cwd) : 'Unknown'],
    ['When', fullTime(action.at)],
  ];
  if (action.asked) rowsOut.push(['Approval', action.status === 'denied' ? 'Asked, then denied.' : 'Asked. The final decision is not shared with llca.']);
  for (const [k, v] of rowsOut) facts.append(node('dt', k), node('dd', v));
  if (action.computer) {
    const dd = node('dd', undefined, 'detail-apps');
    dd.append(computerChip());
    facts.append(node('dt', 'Application'), dd);
  }
  if ((action.apps || []).length) {
    const dd = node('dd', undefined, 'detail-apps');
    for (const name of action.apps) dd.append(appChip(name));
    facts.append(node('dt', action.apps.length === 1 ? 'Application' : 'Applications'), dd);
  }
  if (action.full && action.full !== action.object) {
    const dd = node('dd');
    dd.append(node('code', action.full));
    facts.append(node('dt', 'Full command'), dd);
  }
  wrap.append(facts);
  const only = node('button', state.session === action.session.id ? 'Show all sessions' : 'Show only this session', 'ghost');
  only.onclick = () => setSession(state.session === action.session.id ? '' : action.session.id);
  wrap.append(only);

  const steps = node('ol', undefined, 'steps');
  const stepText = {PreToolUse: 'Asked to run', PermissionRequest: 'Asked for your approval', PermissionDenied: 'Denied', PostToolUse: 'Tool returned', PostToolUseFailure: 'Failed'};
  for (const s of action.stages) {
    const li = node('li');
    li.append(node('time', new Date(s.received_at).toLocaleTimeString('en-GB')), node('span', stepText[s.event] || s.event));
    steps.append(li);
  }
  const tech = node('details', undefined, 'technical');
  tech.append(node('summary', 'Technical details'));
  const techFacts = node('dl', undefined, 'facts small');
  for (const [k, v] of [['Tool', action.main.tool], ['Call ID', action.main.tool_use_id || 'Not provided'], ['Session ID', action.session.id],
    ['Permission mode', action.main.permission_mode || 'Not provided'], ['Source', action.main.origin === 'demo' ? 'Demo data' : 'Local hook']]) {
    techFacts.append(node('dt', k), node('dd', v));
  }
  tech.append(node('h4', 'Steps'), steps, techFacts, node('h4', 'Arguments as recorded'), node('pre', JSON.stringify(action.main.arguments, null, 2)));
  wrap.append(tech, node('p', 'llca never records outputs or file contents. Secrets are hidden on a best-effort basis.', 'quiet'));
  box.replaceChildren(wrap);
}

// The hooks run a copy of llca made by `init`. After an upgrade that
// copy is older than this dashboard, with older redaction rules, until
// `init` is run again.
function renderCollector(status) {
  const banner = $('collector-banner');
  const c = status.collector;
  const recording = Object.values(status.installation || {}).some(s => s.installed);
  banner.hidden = !(c && c.outdated && recording);
  if (banner.hidden) return;
  const command = node('code', c.fix);
  const copy = node('button', 'Copy', 'link');
  copy.onclick = () => navigator.clipboard.writeText(c.fix).then(() => { copy.textContent = 'Copied'; }, () => {});
  banner.replaceChildren(
    node('strong', 'Your hooks run an older llca' + (c.version ? ' (' + c.version + ')' : '') + ' than this one (' + c.running + '). '),
    'New redaction rules only apply once they are updated. Run ', command, ' ', copy);
}

function renderRail(status) {
  const stats = (status && status.stats && status.stats.agents) || {};
  for (const id of Object.keys(AGENTS)) {
    const item = $('agent-' + id);
    const count = (stats[id] || {}).events || 0;
    const on = status.demo ? count > 0 : (status.installation[id] || {}).installed;
    item.classList.toggle('off', !on);
    item.querySelector('.count').textContent = on ? '' : 'not set up';
    item.title = (on ? agentName(id) + ' is being recorded' : 'Not set up. Run: llca init --global --agent ' + (id === 'codex' ? 'codex' : 'claude'))
      + (state.agent === id ? '. Click to show every agent.' : '. Click to show only ' + agentName(id) + '.');
  }
  const installed = Object.entries(status.installation || {}).filter(([, s]) => s.installed).map(([id]) => agentName(id));
  $('setup').textContent = status.demo ? 'Showing demo data.'
    : installed.length ? 'Recording ' + joinAnd(installed) + '.' + (installed.includes('Codex') ? ' Codex hooks must be trusted in /hooks.' : '')
    : 'Not recording yet. Run llca init --global --agent all.';
}

// ---------- Period ----------

// Timestamps in the stored format (UTC, "+00:00"), so they compare as text.
const stamp = date => date.toISOString().replace('Z', '+00:00');
const startOfDay = date => { const d = new Date(date); d.setHours(0, 0, 0, 0); return d; };
const addDays = (date, n) => { const d = new Date(date); d.setDate(d.getDate() + n); return d; };
const dayKey = date => date.getFullYear() + '-' + String(date.getMonth() + 1).padStart(2, '0') + '-' + String(date.getDate()).padStart(2, '0');
const shortDate = date => date.toLocaleDateString('en-GB', {day: 'numeric', month: 'short'});

function rangeBounds() {
  const today = startOfDay(new Date());
  const tomorrow = addDays(today, 1);
  const preset = {today: [0, 'Today'], '7d': [6, 'Last 7 days'], '30d': [29, 'Last 30 days'], '90d': [89, 'Last 90 days']}[state.range];
  if (preset) return {start: addDays(today, -preset[0]), end: tomorrow, label: preset[1]};
  if (state.range === 'custom') {
    const from = $('from').value ? startOfDay(new Date($('from').value + 'T00:00')) : null;
    const to = $('to').value ? addDays(startOfDay(new Date($('to').value + 'T00:00')), 1) : tomorrow;
    const label = (from ? shortDate(from) : 'Start') + ' – ' + shortDate(addDays(to, -1));
    return {start: from, end: to, label};
  }
  return {start: null, end: null, label: 'All time'};
}

function scopeParams() {
  const b = rangeBounds();
  const p = new URLSearchParams();
  if (b.start) p.set('from', stamp(b.start));
  if (b.end) p.set('to', stamp(b.end));
  if (state.agent) p.set('agent', state.agent);
  if (state.project) p.set('project', state.project);
  return p;
}

// ---------- Overview ----------

const KIND_LABELS = [
  ['shell', 'Commands'], ['edit', 'File changes'], ['read', 'Reading & search'], ['web', 'Web'],
  ['mcp', 'Connected apps'], ['computer', 'Computer Use'], ['other', 'Other tools'],
];
const kindOfTool = (tool, category) => describe({tool, category, arguments: {}, cwd: ''}).kind;
const compact = n => (n >= 10000 ? Math.round(n / 1000) + 'K' : n >= 1000 ? (n / 1000).toFixed(1).replace('.0', '') + 'K' : String(n));

// Opens the activity list from the overview: the overview's period, agent
// and project, plus what was clicked (`drill`). The overview's own filters
// are left as they were, so coming back shows the same overview.
function goToActivity(drill) {
  setPage('activity', drill || {});
}

function card(title, note) {
  const c = node('section', undefined, 'card');
  const head = node('div', undefined, 'card-head');
  head.append(node('h2', title));
  if (note) head.append(node('p', note, 'card-note'));
  c.append(head);
  return c;
}

// A ranked list with thin bars; each row opens the activity list, filtered.
function barList(rows, color) {
  const max = Math.max(1, ...rows.map(r => r.value));
  const ul = node('ul', undefined, 'bar-list');
  for (const r of rows) {
    const li = node('li');
    const b = node('button', undefined, 'bar-row');
    b.title = r.hint || '';
    const label = node('span', undefined, 'bar-label');
    if (r.icon) label.append(r.icon);
    if (r.sub) {
      const two = node('span', undefined, 'bar-two');
      two.append(node('span', r.label), node('span', r.sub, 'bar-sub'));
      label.append(two);
    } else {
      label.append(node('span', r.label));
    }
    const track = node('span', undefined, 'bar-track');
    const tone = r.color || color;
    const brand = tone && tone.startsWith('#');
    const fill = node('span', undefined, 'bar-fill' + (brand ? ' brand' : tone ? ' ' + tone : ''));
    if (brand) fill.style.background = tone;
    fill.style.width = Math.max(2, (r.value / max) * 100) + '%';
    track.append(fill);
    b.append(label, track, node('span', compact(r.value), 'bar-value'));
    b.onclick = r.onclick;
    li.append(b);
    ul.append(li);
  }
  return ul;
}

// Buckets for the activity chart: one per day, or per week past ~3 months.
function buckets(byDay) {
  const b = rangeBounds();
  const days = new Map();
  for (const d of byDay) {
    if (!d.day) continue;
    const entry = days.get(d.day) || {codex: 0, 'claude-code': 0, failed: 0};
    entry[d.agent] = (entry[d.agent] || 0) + d.actions;
    entry.failed += d.failed;
    days.set(d.day, entry);
  }
  const keys = [...days.keys()].sort();
  let start = b.start || (keys.length ? new Date(keys[0] + 'T00:00') : startOfDay(new Date()));
  const end = b.end ? addDays(b.end, -1) : startOfDay(new Date());
  const span = Math.round((end - start) / 86400000) + 1;
  const weekly = span > 92;
  if (weekly) start = addDays(start, -((start.getDay() + 6) % 7)); // back to Monday
  const out = [];
  for (let d = new Date(start); d <= end; d = addDays(d, weekly ? 7 : 1)) {
    const bucket = {date: new Date(d), weekly, codex: 0, 'claude-code': 0, failed: 0};
    for (let i = 0; i < (weekly ? 7 : 1); i++) {
      const e = days.get(dayKey(addDays(d, i)));
      if (e) { bucket.codex += e.codex || 0; bucket['claude-code'] += e['claude-code'] || 0; bucket.failed += e.failed; }
    }
    out.push(bucket);
  }
  return out;
}

const SVG = 'http://www.w3.org/2000/svg';

// An agent's logo (static/brands.js), drawn in the agent's colour by CSS
// (.agent-logo); null when it has none.
function agentLogo(id) {
  const logo = typeof AGENT_LOGOS === 'object' ? AGENT_LOGOS[id] : null;
  if (!logo) return null;
  const svg = document.createElementNS(SVG, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('class', 'agent-logo ' + id);
  svg.setAttribute('aria-hidden', 'true');
  const path = document.createElementNS(SVG, 'path');
  path.setAttribute('d', logo[1]);
  path.setAttribute('fill', 'currentColor');
  svg.append(path);
  return svg;
}
function svg(tag, attrs) {
  const e = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs || {})) e.setAttribute(k, v);
  return e;
}

function niceMax(n) {
  if (n <= 4) return 4;
  const pow = 10 ** Math.floor(Math.log10(n));
  return [1, 2, 2.5, 5, 10].map(m => m * pow).find(m => m >= n);
}

// Columns per day, stacked by agent (fixed order: Codex, then Claude Code).
function activityChart(data, series) {
  const wrap = node('div', undefined, 'chart');
  const width = Math.max(320, ($('overview').clientWidth || 900) - 40);
  const height = 210, left = 34, right = 8, top = 10, bottom = 26;
  const plotW = width - left - right, plotH = height - top - bottom;
  const total = d => series.reduce((s, k) => s + d[k], 0);
  const max = niceMax(Math.max(0, ...data.map(total)));
  const y = v => top + plotH - (v / max) * plotH;
  const band = plotW / Math.max(1, data.length);
  // Many buckets (30 days and more): narrow bars, at most 24 px. A few (a
  // week): wide bars with a small gap, so the week reads as one block
  // rather than thin sticks far apart.
  const barW = data.length <= 14 ? Math.min(200, band * 0.84) : Math.max(3, Math.min(24, band * 0.62));
  const chart = svg('svg', {viewBox: `0 0 ${width} ${height}`, width: '100%', role: 'img', 'aria-label': 'Actions per ' + (data[0] && data[0].weekly ? 'week' : 'day')});
  for (const v of [0, max / 2, max]) {
    chart.append(svg('line', {x1: left, x2: width - right, y1: y(v), y2: y(v), class: 'grid'}));
    const t = svg('text', {x: left - 6, y: y(v) + 4, class: 'axis', 'text-anchor': 'end'});
    t.textContent = compact(v);
    chart.append(t);
  }
  const every = Math.max(1, Math.ceil(data.length / 8));
  const tip = node('div', undefined, 'tip');
  tip.hidden = true;
  data.forEach((d, i) => {
    const x = left + i * band + (band - barW) / 2;
    let base = y(0);
    const present = series.filter(k => d[k] > 0);
    present.forEach((k, j) => {
      const h = (d[k] / max) * plotH;
      const gap = j > 0 ? 2 : 0; // surface gap between stacked segments
      const top = base - h + gap;
      const isTop = j === present.length - 1;
      const r = isTop ? Math.min(4, (h - gap) / 2, barW / 2) : 0;
      const segH = Math.max(0, h - gap);
      const path = r > 0
        ? `M${x},${top + segH} V${top + r} Q${x},${top} ${x + r},${top} H${x + barW - r} Q${x + barW},${top} ${x + barW},${top + r} V${top + segH} Z`
        : `M${x},${top + segH} V${top} H${x + barW} V${top + segH} Z`;
      chart.append(svg('path', {d: path, class: 'bar ' + k}));
      base -= h;
    });
    if (i % every === 0) {
      const t = svg('text', {x: left + i * band + band / 2, y: height - 8, class: 'axis', 'text-anchor': 'middle'});
      t.textContent = data.length <= 14 && !d.weekly ? d.date.toLocaleDateString('en-GB', {weekday: 'short', day: 'numeric'}) : shortDate(d.date);
      chart.append(t);
    }
    // The hit target is the whole column band, not just the painted bar.
    const hit = svg('rect', {x: left + i * band, y: top, width: band, height: plotH, class: 'hit'});
    if (data.length <= 31) hit.setAttribute('tabindex', '0');
    const show = () => {
      tip.replaceChildren(node('p', (d.weekly ? 'Week of ' : '') + d.date.toLocaleDateString('en-GB', {weekday: 'short', day: 'numeric', month: 'short'}), 'tip-date'));
      for (const k of series) {
        const row = node('p', undefined, 'tip-row');
        row.append(node('span', '', 'key ' + k), node('strong', String(d[k])), node('span', agentName(k), 'tip-name'));
        tip.append(row);
      }
      if (d.failed) tip.append(node('p', plural(d.failed, 'failure'), 'tip-note'));
      tip.hidden = false;
      // Beside the column, never over it: left of it on the right half.
      const px = ((left + i * band + band / 2) / width) * 100;
      const side = px > 50;
      tip.style.left = side ? '' : 'calc(' + px + '% + ' + (barW / 2 + 10) + 'px)';
      tip.style.right = side ? 'calc(' + (100 - px) + '% + ' + (barW / 2 + 10) + 'px)' : '';
    };
    hit.addEventListener('pointerenter', show);
    hit.addEventListener('focus', show);
    hit.addEventListener('pointerleave', () => { tip.hidden = true; });
    hit.addEventListener('blur', () => { tip.hidden = true; });
    chart.append(hit);
  });
  wrap.append(chart, tip);
  return wrap;
}

function chartTable(data, series) {
  const details = node('details', undefined, 'table-view');
  details.append(node('summary', 'Show as table'));
  const table = node('table');
  const head = node('tr');
  head.append(node('th', data[0] && data[0].weekly ? 'Week of' : 'Day'));
  for (const k of series) head.append(node('th', agentName(k)));
  head.append(node('th', 'Failures'));
  table.append(head);
  for (const d of [...data].reverse()) {
    if (!series.some(k => d[k]) && !d.failed) continue;
    const tr = node('tr');
    tr.append(node('td', d.date.toLocaleDateString('en-GB', {weekday: 'short', day: 'numeric', month: 'short'})));
    for (const k of series) tr.append(node('td', String(d[k])));
    tr.append(node('td', String(d.failed)));
    table.append(tr);
  }
  details.append(table);
  return details;
}

function renderOverview() {
  const box = $('overview');
  const ov = state.overview;
  if (!ov) return;
  const t = ov.totals;
  if (!t.events) {
    const empty = node('div', undefined, 'empty');
    empty.append(node('strong', 'Nothing in this period.'),
      node('p', state.status && !state.status.stats.events
        ? 'Start a new Codex or Claude Code session on this computer and ask it to do something small, like running pwd.'
        : 'Pick a longer period or another agent or project.'));
    box.replaceChildren(empty);
    return;
  }
  // Per-kind counts from tool names (one count per tool call).
  const kinds = {};
  const toolCount = names => ov.by_tool.filter(r => names.includes(r.tool)).reduce((s, r) => s + r.actions, 0);
  for (const r of ov.by_tool) {
    const k = kindOfTool(r.tool, r.category);
    kinds[k] = (kinds[k] || 0) + r.actions;
  }
  // Command-text checks, as in the activity list.
  const risks = {};
  const apps = {};
  let risky = 0;
  for (const c of ov.commands) {
    const full = commandText(c.arguments && (c.arguments.command || c.arguments.cmd)) || '';
    const labels = RISKS.filter(([re]) => re.test(full)).map(([, label]) => label);
    if (labels.includes('Force-pushes code')) labels.splice(labels.indexOf('Pushes code to a remote'), 1);
    if (labels.length) risky += 1;
    for (const l of labels) risks[l] = (risks[l] || 0) + 1;
    for (const a of appsIn(full)) apps[a] = (apps[a] || 0) + 1;
  }
  const computerSteps = ov.by_tool.filter(r => isComputerUse(r.tool)).reduce((s, r) => s + r.actions, 0);

  // 1. Key figures, each opening the matching list.
  const figures = node('div', undefined, 'figures');
  // One line of figures on the page itself, no boxes: the number, then
  // what it counts.
  for (const [label, value, filters, hint] of [
    ['actions', t.actions, {}, 'Tool calls made by the agents'],
    ['sessions', t.sessions, {}, 'Conversations with at least one event'],
    ['commands', kinds.shell || 0, {kind: 'shell'}, 'Shell commands run'],
    ['file changes', kinds.edit || 0, {kind: 'edit'}, 'Edits, writes and patches'],
    ['failed or denied', t.failed, {result: 'problem'}, 'Actions that failed or were refused'],
  ]) {
    const f = node('button', undefined, 'figure' + (label === 'failed or denied' && value ? ' alert' : ''));
    f.title = hint + '. Click to see them.';
    f.append(node('strong', compact(value), 'figure-value'), node('span', label, 'figure-label'));
    f.onclick = () => goToActivity(filters);
    figures.append(f);
  }

  // 2. Activity over time.
  const series = state.agent ? [state.agent] : ['codex', 'claude-code'];
  const data = buckets(ov.by_day);
  const timeCard = card(data[0] && data[0].weekly ? 'Actions per week' : 'Actions per day', state.agent ? agentName(state.agent) : null);
  if (series.length > 1) {
    const legend = node('div', undefined, 'legend');
    for (const k of series) {
      const item = node('span', undefined, 'legend-item');
      item.append(node('span', '', 'key ' + k), node('span', agentName(k)));
      legend.append(item);
    }
    timeCard.append(legend);
  }
  timeCard.append(activityChart(data, series), chartTable(data, series));

  // 3. What the agents did.
  const kindRows = KIND_LABELS.filter(([k]) => kinds[k]).map(([k, label]) => ({
    label, value: kinds[k], hint: k === 'computer' ? COMPUTER_USE_HELP : '',
    onclick: () => goToActivity({kind: k === 'computer' ? 'app' : k === 'other' ? '' : k}),
  }));
  const kindCard = card('What the agents did');
  kindCard.append(kindRows.length ? barList(kindRows) : node('p', 'No tool calls in this period.', 'quiet'));

  // 4. Worth a look.
  const reasons = [
    ['Failed or denied', t.failed, {result: 'problem'}],
    ['Risky commands', risky, {kind: 'attention'}],
    ['Secrets seen by the agents', t.secrets, {kind: 'secret'}],
    ['Computer Use steps', computerSteps, {kind: 'app'}],
    ['Data sent outside this computer', toolCount(['Artifact', 'ShareOnboardingGuide']), {kind: 'attention'}],
    ['Scheduled to run later', toolCount(['CronCreate', 'ScheduleWakeup', 'RemoteTrigger']), {kind: 'attention'}],
  ].filter(r => r[1]);
  const lookCard = card('Worth a look', ov.commands_truncated ? 'Command checks cover the latest 5,000 commands.' : null);
  lookCard.classList.add('card-attention'); // tinted so that it stands out among the cards
  if (reasons.length) {
    // Red for problems (failures, secrets), amber for the rest, as in the activity list and the map.
    const problem = ['Failed or denied', 'Secrets seen by the agents'];
    lookCard.append(barList(reasons.map(([label, value, filters]) => ({label, value, color: problem.includes(label) ? 'fail' : 'warn',
      hint: label === 'Computer Use steps' ? COMPUTER_USE_HELP : '', onclick: () => goToActivity(filters)})), 'warn'));
    const detail = Object.entries(risks).sort((a, b) => b[1] - a[1]);
    if (detail.length) {
      const p = node('p', undefined, 'card-foot');
      p.textContent = 'Risky commands: ' + detail.map(([l, n]) => l.charAt(0).toLowerCase() + l.slice(1) + ' (' + n + ')').join(', ') + '.';
      lookCard.append(p);
    }
  } else {
    lookCard.append(node('p', 'Nothing stands out in this period.', 'quiet'));
  }
  lookCard.append(...watchSection(ov.watch));

  // 5. Projects.
  const projectCard = card('Projects');
  const projects = ov.projects.filter(p => p.actions).slice(0, 8);
  projectCard.append(projects.length ? barList(projects.map(p => ({
    label: project(p.cwd), value: p.actions, hint: shortPath(p.cwd) + ', ' + plural(p.sessions, 'session'),
    onclick: () => goToActivity({project: p.cwd}),
  }))) : node('p', 'No project in this period.', 'quiet'));

  // 6. Applications.
  const appRows = Object.entries(apps).sort((a, b) => b[1] - a[1]).slice(0, 8).map(([name, n]) => ({
    label: name, value: n, icon: appIcon(name), onclick: () => goToActivity({kind: 'app', search: name}),
  }));
  if (computerSteps) appRows.unshift({label: 'Computer Use', value: computerSteps, hint: COMPUTER_USE_HELP, icon: node('span', '↖', 'app-icon letter cu'), onclick: () => goToActivity({kind: 'app'})});
  const appCard = card('Applications', 'Opened or run by the agents');
  appCard.append(appRows.length ? barList(appRows) : node('p', 'No application in this period.', 'quiet'));

  // 7. Online services: one row per service, in its colour.
  const serviceCard = card('Online services', 'Reached by the agents');
  const services = periodServices().slice(0, 8);
  serviceCard.append(services.length ? barList(services.map(r => ({
    label: r.name, sub: [...new Set(r.via.map(shortVia))].join(', ') + (r.last ? ' · ' + lastUse(r.last) : ''), value: r.actions, icon: serviceBadge(r.name, 'md'), color: serviceColour(r.name),
    hint: r.name + ': ' + r.via.join(', ') + (r.failed ? ' · ' + r.failed + ' failed' : ''), onclick: () => goToActivity({service: r.name}),
  }))) : node('p', 'No online service in this period. Services are recognised from command line tools (gcloud, gh, stripe…), connected apps (MCP) and URLs.', 'quiet'));

  const grid = node('div', undefined, 'cards');
  grid.append(kindCard, lookCard, serviceCard, appCard, projectCard);
  box.replaceChildren(...[secretAlert(ov), figures, timeCard, mapCard(ov.map), grid].filter(Boolean));
}

// "Your rules" at the bottom of "Worth a look": one row per label, red for
// alert rules, amber otherwise; rules without a match stay listed, greyed.
function watchSection(w) {
  if (!w) return [];
  const out = [];
  const divider = node('p', undefined, 'watch-divider');
  divider.append(node('span', 'Your rules'));
  out.push(divider);
  if (!w.rules.length) {
    const hint = node('div', undefined, 'watch-hint');
    hint.append(node('p', 'Want to see when an agent touches something that matters to you? Write one line per thing to watch in your rules file:'),
      node('pre', 'file .env   # Touched a .env file\nalert command kubectl * --context prod   # Touched production'));
    hint.append(openRulesButton('ghost'), node('p', w.exists ? 'The file has examples to uncomment: remove the # in front of a line, then save.' : 'The file is created with examples the first time you open it.', 'quiet'));
    out.push(hint);
  } else {
    const max = Math.max(1, ...w.rules.map(r => r.actions));
    const ul = node('ul', undefined, 'bar-list');
    for (const r of w.rules) {
      const li = node('li');
      if (!r.actions) {
        const row = node('div', undefined, 'bar-row zero');
        row.append(node('span', r.label, 'bar-label'), node('span', 'Nothing in this period', 'bar-note'), node('span', '0', 'bar-value'));
        li.append(row);
      } else {
        const tag = r.alert ? node('span', 'alert', 'watch-tag') : null;
        const list = barList([{label: r.label, value: r.actions, icon: tag, color: r.alert ? 'fail' : 'warn',
          hint: 'Line ' + r.lines.join(', ') + ' of watch.rules', onclick: () => goToActivity({rule: r.label})}]);
        li.append(list.firstChild.firstChild);
        // keep the scale of the whole list
        li.querySelector('.bar-fill').style.width = Math.max(2, (r.actions / max) * 100) + '%';
      }
      ul.append(li);
    }
    out.push(ul);
  }
  if (w.errors.length) {
    const box = node('div', undefined, 'watch-errors');
    box.setAttribute('role', 'status');
    for (const e of w.errors) box.append(node('p', 'Line ' + e.line + ' of your rules was skipped: ' + e.message + '.'));
    out.push(box);
  }
  const foot = node('p', undefined, 'card-foot');
  foot.append('Your rules: ', node('code', w.path),
    w.rules.length ? ' · ' + plural(w.rules.length, 'rule') + '. They apply to your whole history as soon as you save the file.' : '');
  const actions = node('span', undefined, 'foot-actions');
  if (w.rules.length || w.errors.length) actions.append(openRulesButton('link'));
  actions.append(revealRulesButton(), copyPathButton(w.path));
  foot.append(actions);
  out.push(foot);
  return out;
}

// "Show in Finder" on a Mac; on Linux, the file's folder opens.
const finderLabel = () => (state.status && state.status.os && state.status.os !== 'macos' ? 'Show in folder' : 'Show in Finder');

// Shows watch.rules in Finder: ~/Library is hidden there, so finding the
// file by hand is not obvious.
function revealRulesButton() {
  const label = finderLabel();
  const b = node('button', label, 'link');
  b.onclick = async () => {
    try {
      await api('/api/watch/open?show=finder', 'POST');
    } catch (error) {
      b.textContent = 'Could not show it';
      b.title = error.message;
      setTimeout(() => { b.textContent = label; }, 4000);
    }
  };
  return b;
}

function copyPathButton(path) {
  const b = node('button', 'Copy the path', 'link');
  b.onclick = async () => {
    try {
      await navigator.clipboard.writeText(path);
      b.textContent = 'Copied';
    } catch (e) {
      b.textContent = 'Select the path above to copy it';
    }
    setTimeout(() => { b.textContent = 'Copy the path'; }, 2500);
  };
  return b;
}

// Opens watch.rules in the default text editor (the local server runs it).
function openRulesButton(cls) {
  const b = node('button', 'Open the rules file', cls);
  b.title = 'Opens watch.rules in your text editor. Save it, and the dashboard picks up the change.';
  b.onclick = async () => {
    b.disabled = true;
    try {
      await api('/api/watch/open', 'POST');
      b.textContent = 'Opened in your text editor';
    } catch (error) {
      // Say why, next to the button: the message from the server.
      b.textContent = 'Could not open it';
      b.title = error.message;
      const why = node('span', error.message + '. Use “' + finderLabel() + '” or “Copy the path” below.', 'open-error');
      b.after(why);
      setTimeout(() => why.remove(), 8000);
    }
    setTimeout(() => { b.textContent = 'Open the rules file'; b.disabled = false; refresh(); }, 2500);
  };
  return b;
}

// ---------- Where the agents went ----------
// Agents on the left, the resources they reached on the right (from
// /api/overview's map). A line is drawn only for calls the journal records;
// its width grows with the number of actions, its colour is the agent's.

const MAP_GROUPS = [['app', 'Applications'], ['project', 'Projects'], ['service', 'Online services'], ['web', 'Websites']];
const MAP_VISIBLE = 3;
const AGENT_ORDER = ['codex', 'claude-code'];
const ICON_PATHS = {
  project: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
  service: 'M7 18h10a4 4 0 0 0 .5-7.97A6 6 0 0 0 6 9a4.5 4.5 0 0 0 1 9z',
  web: 'M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18M12 3a9 9 0 1 0 0.01 0',
  computer: 'M5 3l14 7-6 2-2 6z',
};

function lineIcon(kind) {
  const box = node('span', undefined, 'map-icon ' + kind);
  const s = svg('svg', {width: 18, height: 18, viewBox: '0 0 24 24', 'aria-hidden': 'true', fill: 'none', stroke: 'currentColor', 'stroke-width': 1.7, 'stroke-linecap': 'round', 'stroke-linejoin': 'round'});
  s.append(svg('path', {d: ICON_PATHS[kind]}));
  box.append(s);
  return box;
}

const resourceKey = r => r.category + '|' + r.name;

function resourceIcon(r) {
  if (r.category === 'app') return r.name === 'Computer Use' ? lineIcon('computer') : appIcon(r.name);
  if (r.category === 'service') return serviceBadge(r.name, 'xl');
  return lineIcon(r.category);
}

function resourceMeta(r) {
  if (r.name === 'Computer Use' && r.category === 'app') return 'App not reported · ' + plural(r.actions, 'step');
  // Projects: the group title already says what they are.
  if (r.category === 'project') return plural(r.actions, 'action');
  if (r.category === 'service') return [...new Set(r.via.map(shortVia))].join(', ') + ' · ' + plural(r.actions, 'action');
  return r.via[0] + ' · ' + plural(r.actions, 'action');
}

function chips(r) {
  const out = [];
  if (r.secrets) {
    const c = node('span', 'Secret', 'map-chip secret');
    c.title = plural(r.secrets, 'action') + ' sent a secret';
    out.push(c);
  }
  if (r.failed) out.push(node('span', r.failed + ' failed', 'map-chip failed'));
  return out;
}

// Opens the activity list on this resource (and agent, for a line).
function openResource(r, agent) {
  const computer = r.category === 'app' && r.name === 'Computer Use';
  const drill = r.project ? {project: r.project} : computer ? {kind: 'computer'} : r.category === 'service' ? {service: r.name} : {search: r.search || r.name};
  if (agent) drill.agent = agent;
  goToActivity(drill);
}

// Width available for the map; under 640px it becomes a list.
// Room left by the card and the dotted ground's padding.
const mapWidth = () => Math.min(1040, Math.max(320, ($('overview').clientWidth || 1000) - 82));

function mapLayout(map, hostW) {
  const groups = [];
  let y = 0;
  const compact = hostW < 900;
  const leftW = compact ? 180 : 220, rightW = compact ? 290 : 340;
  const rightX = hostW - rightW;
  const nodes = [];
  for (const [cat, label] of MAP_GROUPS) {
    const all = map.resources.filter(r => r.category === cat);
    if (!all.length) continue;
    const expanded = state.mapExpanded.has(cat);
    const shown = expanded ? all : all.slice(0, MAP_VISIBLE);
    const hidden = all.slice(shown.length);
    groups.push({cat, label, count: all.length, top: y});
    y += 24;
    for (const r of shown) { nodes.push({r, top: y, height: 60}); y += 68; }
    if (hidden.length) {
      const agents = {};
      for (const h of hidden) for (const [a, n] of Object.entries(h.agents)) agents[a] = (agents[a] || 0) + n;
      nodes.push({more: cat, label, hidden, agents, actions: hidden.reduce((s, h) => s + h.actions, 0), top: y, height: 44});
      y += 52;
    } else if (expanded && all.length > MAP_VISIBLE) {
      nodes.push({less: cat, label, top: y, height: 36});
      y += 44;
    }
    y += 16;
  }
  const height = Math.max(y, 260);
  const agents = AGENT_ORDER.filter(a => map.agents[a]);
  const span = agents.length * 104 + (agents.length - 1) * 116;
  const agentNodes = agents.map((a, i) => ({agent: a, top: Math.round((height - span) / 2) + i * 220, height: 104}));
  return {groups, nodes, agentNodes, height, leftW, rightW, rightX};
}

function focusMatches(link) {
  const f = state.mapFocus;
  if (!f) return null;
  if (f.type === 'agent') return link.agent === f.id;
  return link.key === f.id;
}

function resourceMap(map) {
  const wrap = node('div', undefined, 'map');
  const hostW = mapWidth();
  if (hostW < 640) {
    wrap.append(resourceList(map));
    return wrap;
  }
  const L = mapLayout(map, hostW);
  const host = node('div', undefined, 'map-host');
  host.style.width = hostW + 'px';
  host.style.height = L.height + 'px';

  // Links: one per (agent, resource), stacked along each node's edge.
  const links = [];
  for (const n of L.nodes) {
    if (n.less) continue;
    const per = n.more ? n.agents : n.r.agents;
    for (const a of AGENT_ORDER) if (per[a]) links.push({agent: a, n, count: per[a], key: n.more ? 'more|' + n.more : resourceKey(n.r)});
  }
  const max = Math.max(1, ...links.map(l => l.count));
  for (const l of links) l.w = Math.round((1.5 + 10.5 * Math.sqrt(l.count / max)) * 10) / 10;
  const agentTop = Object.fromEntries(L.agentNodes.map(a => [a.agent, a]));
  for (const a of L.agentNodes) {
    const mine = links.filter(l => l.agent === a.agent).sort((p, q) => p.n.top - q.n.top);
    const total = mine.reduce((s, l) => s + l.w, 0) + 3 * Math.max(0, mine.length - 1);
    let y = a.top + a.height / 2 - total / 2;
    for (const l of mine) { l.y0 = y + l.w / 2; y += l.w + 3; }
  }
  for (const n of L.nodes) {
    const mine = links.filter(l => l.n === n);
    const total = mine.reduce((s, l) => s + l.w, 0) + 3 * Math.max(0, mine.length - 1);
    let y = n.top + n.height / 2 - total / 2;
    for (const l of mine) { l.y1 = y + l.w / 2; y += l.w + 3; }
  }
  const plot = svg('svg', {width: hostW, height: L.height, viewBox: `0 0 ${hostW} ${L.height}`, class: 'map-lines'});
  const x0 = L.leftW, x1 = L.rightX, mid = (x0 + x1) / 2;
  const tip = node('div', undefined, 'tip map-tip');
  tip.hidden = true;
  for (const l of links) {
    if (!agentTop[l.agent]) continue;
    const d = `M ${x0} ${l.y0} C ${mid} ${l.y0}, ${mid} ${l.y1}, ${x1} ${l.y1}`;
    const on = focusMatches(l);
    const path = svg('path', {d, class: 'map-line ' + l.agent + (on === true ? ' on' : on === false ? ' off' : ''), 'stroke-width': l.w});
    const name = l.n.more ? plural(l.n.hidden.length, 'more ' + l.n.label.toLowerCase().replace(/s$/, ''), 'more ' + l.n.label.toLowerCase()) : l.n.r.name;
    const hit = svg('path', {d, class: 'map-hit', 'stroke-width': Math.max(14, l.w + 8)});
    hit.addEventListener('pointerenter', e => {
      tip.replaceChildren(node('p', agentName(l.agent) + ' → ' + name, 'tip-date'), node('p', plural(l.count, 'action') + (l.n.more ? '' : ' · click to see them'), 'tip-name'));
      tip.hidden = false;
      const box = host.getBoundingClientRect();
      tip.style.left = Math.min(hostW - 220, Math.max(0, e.clientX - box.left + 12)) + 'px';
      tip.style.top = Math.max(0, e.clientY - box.top - 52) + 'px';
      path.classList.add('hover');
    });
    hit.addEventListener('pointerleave', () => { tip.hidden = true; path.classList.remove('hover'); });
    hit.addEventListener('click', () => (l.n.more ? (state.mapExpanded.add(l.n.more), renderOverview()) : openResource(l.n.r, l.agent)));
    plot.append(path, hit);
  }
  host.append(plot);

  if (L.agentNodes.length) {
    const label = node('span', 'Agents', 'map-label');
    label.style.top = (L.agentNodes[0].top - 26) + 'px';
    host.append(label);
  }
  for (const a of L.agentNodes) {
    const info = map.agents[a.agent];
    const b = node('button', undefined, 'map-agent ' + a.agent);
    const f = state.mapFocus;
    const focusedRes = f && f.type === 'resource' ? map.resources.find(r => resourceKey(r) === f.id) : null;
    const dim = f && ((f.type === 'agent' && f.id !== a.agent) || (focusedRes && !focusedRes.agents[a.agent]));
    if (dim) b.classList.add('dim');
    if (f && f.type === 'agent' && f.id === a.agent) b.setAttribute('aria-pressed', 'true');
    Object.assign(b.style, {top: a.top + 'px', width: L.leftW + 'px', height: a.height + 'px'});
    const meta = focusedRes ? (focusedRes.agents[a.agent] ? plural(focusedRes.agents[a.agent], 'action') + ' of ' + info.actions : 'Did not use it')
      : plural(info.sessions, 'session') + ' · ' + plural(info.actions, 'action');
    const name = node('strong', undefined, 'map-agent-name');
    const logo = agentLogo(a.agent);
    if (logo) name.append(logo);
    name.append(agentName(a.agent));
    b.append(name, node('span', meta, 'map-meta'));
    b.title = 'Highlight what ' + agentName(a.agent) + ' reached';
    b.onclick = () => { state.mapFocus = f && f.type === 'agent' && f.id === a.agent ? null : {type: 'agent', id: a.agent}; renderOverview(); };
    host.append(b);
  }
  for (const g of L.groups) {
    const label = node('span', g.label + ' · ' + g.count, 'map-label');
    Object.assign(label.style, {top: g.top + 'px', left: L.rightX + 'px'});
    host.append(label);
  }
  for (const n of L.nodes) {
    const b = node('button', undefined, 'map-node');
    Object.assign(b.style, {top: n.top + 'px', left: L.rightX + 'px', width: L.rightW + 'px', height: n.height + 'px'});
    if (n.more || n.less) {
      b.classList.add('map-more');
      if (n.more) {
        const label = node('span', n.hidden.length + ' more ' + n.label.toLowerCase(), 'map-more-label');
        if (n.more === 'service') {
          const stack = node('span', undefined, 'svc-stack');
          for (const h of n.hidden.slice(0, 4)) stack.append(serviceBadge(h.name, 'sm'));
          label.append(stack);
        }
        b.append(label, node('span', plural(n.actions, 'action'), 'map-meta'));
        b.onclick = () => { state.mapExpanded.add(n.more); renderOverview(); };
      } else {
        b.append(node('span', 'Show fewer'));
        b.onclick = () => { state.mapExpanded.delete(n.less); renderOverview(); };
      }
      if (state.mapFocus) b.classList.add('dim');
      host.append(b);
      continue;
    }
    const r = n.r, key = resourceKey(r);
    const f = state.mapFocus;
    const selected = f && f.type === 'resource' && f.id === key;
    if (selected) b.setAttribute('aria-pressed', 'true');
    if (f && !selected && !(f.type === 'agent' && r.agents[f.id])) b.classList.add('dim');
    const text = node('span', undefined, 'map-text');
    const strong = node('strong', r.name);
    text.append(strong, node('span', resourceMeta(r), 'map-meta'));
    b.append(resourceIcon(r), text, ...chips(r));
    if (r.category === 'app' && r.name === 'Computer Use') {
      b.title = COMPUTER_USE_HELP;
      b.append(node('span', 'i', 'map-info'));
    } else {
      b.title = r.category === 'project' ? r.project : r.name + ': ' + r.via.join(', ');
    }
    b.onclick = () => { state.mapFocus = selected ? null : {type: 'resource', id: key}; renderOverview(); };
    host.append(b);
  }
  host.append(tip);
  const scroller = node('div', undefined, 'map-scroll');
  scroller.append(host);
  wrap.append(scroller);
  const f = state.mapFocus;
  const focused = f && f.type === 'resource' ? map.resources.find(r => resourceKey(r) === f.id) : null;
  if (focused) wrap.append(resourcePanel(focused));
  return wrap;
}

function resourcePanel(r) {
  const panel = node('div', undefined, 'map-panel');
  const body = node('div');
  body.append(node('h3', r.name), node('p', r.category === 'project' ? 'Project folder: ' + shortPath(r.project) : r.via.join(' · '), 'map-meta'));
  const dl = node('dl', undefined, 'facts');
  const users = AGENT_ORDER.filter(a => r.agents[a]).map(a => agentName(a) + ' · ' + plural(r.agents[a], 'action')).join(', ');
  const results = [r.ok && r.ok + ' done', r.failed && r.failed + ' failed', r.asked && plural(r.asked, 'needed approval', 'needed approval'),
    r.secrets && plural(r.secrets, 'sent a secret', 'sent a secret')].filter(Boolean).join(' · ') || 'No result reported';
  for (const [k, v] of [['Used by', users + ' in ' + plural(r.sessions, 'session')], ['Last used', fullTime(r.last)], ['Results', results]]) dl.append(node('dt', k), node('dd', v));
  if (r.examples.length) {
    const dd = node('dd', undefined, 'map-examples');
    for (const e of r.examples.slice(0, 4)) dd.append(node('code', e));
    dl.append(node('dt', r.category === 'app' && r.name === 'Computer Use' ? 'Steps' : 'Examples'), dd);
  }
  body.append(dl);
  if (r.category === 'app' && r.name === 'Computer Use') body.append(node('p', COMPUTER_USE_HELP, 'quiet'));
  const see = node('button', 'See ' + (r.actions === 1 ? 'this action' : 'these ' + r.actions + ' actions'), 'ghost');
  see.onclick = () => openResource(r);
  panel.append(body, see);
  return panel;
}

// Table view (also the accessible version of the map).
function resourceTable(map) {
  const box = node('div', undefined, 'map-table');
  const table = node('table');
  const agents = AGENT_ORDER.filter(a => map.agents[a]);
  const head = node('tr');
  for (const h of ['Resource', 'Kind', 'Reached through', ...agents.map(agentName), 'Failed', 'Secret']) head.append(node('th', h));
  table.append(head);
  for (const [cat, label] of MAP_GROUPS) {
    for (const r of map.resources.filter(x => x.category === cat)) {
      const tr = node('tr');
      const td = node('td');
      const b = node('button', r.name, 'link');
      b.onclick = () => openResource(r);
      if (r.category === 'service') td.append(serviceBadge(r.name, 'sm'), ' ');
      td.append(b);
      tr.append(td, node('td', label.replace(/s$/, '')), node('td', r.category === 'project' ? shortPath(r.project) : r.via.join(', ')));
      for (const a of agents) tr.append(node('td', String(r.agents[a] || 0), 'num'));
      tr.append(node('td', String(r.failed), 'num'), node('td', r.secrets ? 'Yes' : '', 'num'));
      table.append(tr);
    }
  }
  box.append(table);
  return box;
}

// Phone layout: grouped rows with a bar per agent.
function resourceList(map) {
  const box = node('div', undefined, 'map-list');
  const agents = AGENT_ORDER.filter(a => map.agents[a]);
  const only = state.mapFocus && state.mapFocus.type === 'agent' ? state.mapFocus.id : null;
  if (agents.length > 1) {
    const pick = node('div', undefined, 'map-picks');
    pick.setAttribute('role', 'group');
    pick.setAttribute('aria-label', 'Agent');
    for (const a of [null, ...agents]) {
      const b = node('button', a ? agentName(a) + ' · ' + map.agents[a].actions : 'Both', 'kind');
      b.setAttribute('aria-pressed', String(only === a));
      b.onclick = () => { state.mapFocus = a ? {type: 'agent', id: a} : null; renderOverview(); };
      pick.append(b);
    }
    box.append(pick);
  }
  const rows = map.resources.filter(r => !only || r.agents[only]);
  const max = Math.max(1, ...rows.map(r => r.actions));
  for (const [cat, label] of MAP_GROUPS) {
    const all = rows.filter(r => r.category === cat);
    if (!all.length) continue;
    const expanded = state.mapExpanded.has(cat);
    const group = node('div', undefined, 'map-group');
    group.append(node('span', label + ' · ' + all.length, 'map-label static'));
    for (const r of expanded ? all : all.slice(0, MAP_VISIBLE)) {
      const b = node('button', undefined, 'map-row');
      const text = node('span', undefined, 'map-text');
      const note = r.secrets ? node('span', 'Secret went through', 'map-meta secret') : r.failed ? node('span', r.failed + ' failed', 'map-meta failed') : node('span', r.category === 'project' ? 'Project folder' : r.via[0], 'map-meta');
      text.append(node('strong', r.name), note);
      const bar = node('span', undefined, 'map-bar');
      const track = node('span', undefined, 'map-track');
      for (const a of agents) {
        if (!r.agents[a] || (only && a !== only)) continue;
        const seg = node('span', undefined, 'map-seg ' + a);
        seg.style.width = (r.agents[a] / max) * 100 + '%';
        track.append(seg);
      }
      bar.append(track, node('span', String(only ? r.agents[only] : r.actions), 'map-count'));
      b.append(resourceIcon(r), text, bar);
      if (r.name === 'Computer Use' && r.category === 'app') b.title = COMPUTER_USE_HELP;
      b.onclick = () => openResource(r, only);
      group.append(b);
    }
    if (all.length > MAP_VISIBLE) {
      const more = node('button', expanded ? 'Show fewer' : (all.length - MAP_VISIBLE) + ' more ' + label.toLowerCase(), 'map-row map-more');
      more.onclick = () => { if (expanded) state.mapExpanded.delete(cat); else state.mapExpanded.add(cat); renderOverview(); };
      group.append(more);
    }
    box.append(group);
  }
  return box;
}

function mapCard(map) {
  const c = card('Where the agents went');
  if (!map || !map.resources.length) {
    c.append(node('p', 'No application, project or service in this period.', 'quiet'));
    return c;
  }
  const head = c.querySelector('.card-head');
  // A focus that no longer exists (other period or filters) is dropped.
  if (state.mapFocus && state.mapFocus.type === 'resource' && !map.resources.some(r => resourceKey(r) === state.mapFocus.id)) state.mapFocus = null;
  if (state.mapFocus && state.mapFocus.type === 'agent' && !map.agents[state.mapFocus.id]) state.mapFocus = null;
  const f = state.mapFocus;
  const focusedName = f && (f.type === 'agent' ? agentName(f.id) : (map.resources.find(r => resourceKey(r) === f.id) || {}).name);
  const note = node('p', undefined, 'card-note');
  if (focusedName) {
    note.append('Showing ', node('strong', focusedName), ' · ');
    const all = node('button', 'Show everything', 'link');
    all.onclick = () => { state.mapFocus = null; renderOverview(); };
    note.append(all);
  } else {
    note.textContent = 'Applications, projects and services each agent used. A line is drawn only when the log shows it.'
      + (state.overview && state.overview.map_truncated ? ' Based on the latest 200,000 events of this period.' : '');
  }
  head.append(note);
  const views = node('div', undefined, 'map-views');
  views.setAttribute('role', 'group');
  views.setAttribute('aria-label', 'View');
  for (const [v, label] of [['map', 'Map'], ['table', 'Table']]) {
    const b = node('button', label);
    b.setAttribute('aria-pressed', String(state.mapView === v));
    b.onclick = () => { state.mapView = v; renderOverview(); };
    views.append(b);
  }
  head.append(views);
  if (state.mapView === 'table') {
    c.append(resourceTable(map));
    return c;
  }
  if (mapWidth() < 640) {
    c.append(resourceList(map), node('p', 'Tap a row to see those actions. A service reached through Computer Use in a browser shows as Computer Use.', 'card-foot'));
    return c;
  }
  const legend = node('div', undefined, 'legend map-legend');
  for (const a of AGENT_ORDER.filter(x => map.agents[x])) {
    const item = node('span', undefined, 'legend-item');
    item.append(node('span', '', 'line-key ' + a), node('span', agentName(a)));
    legend.append(item);
  }
  legend.append(node('span', 'Thicker line = more actions', 'legend-item'));
  for (const [cls, text, label] of [['secret', 'Secret', 'a secret went through'], ['failed', '1 failed', 'failed or denied']]) {
    const item = node('span', undefined, 'legend-item');
    item.append(node('span', text, 'map-chip ' + cls), node('span', label));
    legend.append(item);
  }
  c.append(legend, resourceMap(map),
    node('p', 'Click an agent or a resource to highlight its lines; click a line to see those actions. Services are recognised from connected apps (MCP), command line tools and URLs. A service reached through Computer Use in a browser shows as Computer Use.', 'card-foot'));
  return c;
}

// Shown at the top of the overview when secrets went through the agents.
// Dismissing it hides the calls shown; a new secret brings it back, counting
// only the new calls. The secrets stay in the figures and the Activity list.
const DISMISS_HELP = 'Hide this alert. It comes back if a new secret goes through an agent. These actions stay listed under Secrets in Activity.';

async function dismissSecrets(through) {
  const p = scopeParams();
  p.set('through', String(through));
  try {
    const r = await api('/api/secret-alert/dismiss?' + p, 'POST');
    state.dismissed = r.batch;
    refresh();
  } catch (error) {
    $('connection').textContent = error.message;
    $('connection').classList.add('error');
  }
}

async function restoreSecrets() {
  const batch = state.dismissed;
  state.dismissed = null;
  try {
    await api('/api/secret-alert/restore?batch=' + batch, 'POST');
  } catch (error) {
    $('connection').textContent = error.message;
    $('connection').classList.add('error');
  }
  refresh();
}

function secretAlert(ov) {
  const total = ov.totals.alert_secrets;
  if (!total) {
    if (!state.dismissed) return null;
    const done = node('p', 'Secret alert dismissed. It comes back if a new secret goes through an agent. ', 'secret-dismissed');
    done.setAttribute('role', 'status');
    const undo = node('button', 'Undo', 'link');
    undo.onclick = restoreSecrets;
    done.append(undo);
    return done;
  }
  const rows = ov.secret_kinds || [];
  const agents = [...new Set(rows.map(r => r.agent))].map(agentName);
  const alert = node('section', undefined, 'secret-alert');
  alert.setAttribute('role', 'status');
  const body = node('div', undefined, 'secret-alert-body');
  // Some calls of this period were dismissed earlier: the rest are new.
  const fresh = ov.totals.secrets > total ? ' new' : '';
  body.append(
    node('h2', (total === 1 ? '1' + fresh + ' action' : total + fresh + ' actions') + ' sent a secret to ' + (agents.length ? joinAnd(agents) : 'an agent')),
    node('p', 'The agent, and the service that runs its model, saw these values in clear. This log keeps them masked.'));
  // One chip per kind, with the agents and the number of actions.
  const kinds = new Map();
  for (const r of rows) {
    const k = r.kind || UNKNOWN_SECRET;
    const e = kinds.get(k) || {calls: 0, agents: new Set(), last: ''};
    e.calls += r.calls;
    e.agents.add(r.agent);
    if (r.last > e.last) e.last = r.last;
    kinds.set(k, e);
  }
  const list = node('ul', undefined, 'secret-kinds');
  for (const [k, e] of [...kinds].sort((a, b) => b[1].calls - a[1].calls).slice(0, 8)) {
    const li = node('li');
    const b = node('button', undefined, 'secret-kind');
    b.title = 'Last seen ' + fullTime(e.last) + '. Click to see these actions.';
    b.append(node('span', k === UNKNOWN_SECRET ? 'Kind not recorded' : k, 'secret-kind-name'),
      node('span', [...e.agents].map(agentName).join(', ') + ' · ' + plural(e.calls, 'action'), 'secret-kind-meta'));
    b.onclick = () => goToActivity({kind: 'secret'});
    li.append(b);
    list.append(li);
  }
  if (kinds.size > 8) list.append(node('li', 'and ' + (kinds.size - 8) + ' more', 'quiet'));
  body.append(list, node('p', 'Rotate any that do not expire on their own. Give agents short-lived credentials with limited rights.', 'advice'));
  const actions = node('div', undefined, 'secret-alert-actions');
  const see = node('button', 'See these actions', 'ghost');
  see.onclick = () => goToActivity({kind: 'secret'});
  const dismiss = node('button', 'Dismiss', 'ghost');
  dismiss.title = DISMISS_HELP;
  dismiss.setAttribute('aria-label', 'Dismiss this alert');
  dismiss.onclick = () => { dismiss.disabled = true; dismissSecrets(ov.totals.alert_through); };
  actions.append(see, dismiss);
  alert.append(node('span', '!', 'secret-alert-mark'), body, actions);
  return alert;
}

// ---------- Page, filters, rendering ----------

// Each page keeps its own filters. The overview has period, agent and
// project; the activity list adds result, kind, search and session.
const FILTER_KEYS = ['range', 'agent', 'project', 'result', 'kind', 'session', 'service', 'rule'];
const NO_FILTERS = {range: '7d', from: '', to: '', agent: '', project: '', result: '', kind: '', session: '', service: '', rule: '', search: ''};
const savedFilters = {};

function currentFilters() {
  const f = {from: $('from').value, to: $('to').value, search: $('search').value};
  for (const k of FILTER_KEYS) f[k] = state[k];
  return f;
}

function applyFilters(f) {
  for (const k of FILTER_KEYS) state[k] = f[k] || NO_FILTERS[k];
  $('from').value = f.from || '';
  $('to').value = f.to || '';
  $('search').value = f.search || '';
}

// The active filters as removable chips, under the filter bar.
function renderActiveFilters() {
  const chips = [];
  const add = (label, clear) => chips.push([label, clear]);
  if (state.range !== '7d') add('Period: ' + rangeBounds().label, () => { state.range = '7d'; });
  if (state.agent) add('Agent: ' + agentName(state.agent), () => { state.agent = ''; });
  if (state.project) add('Project: ' + project(state.project), () => { state.project = ''; });
  if (state.page === 'activity') {
    const kindButton = document.querySelector('.kind[data-kind="' + state.kind + '"]');
    const kindLabel = kindButton ? kindButton.textContent : {computer: 'Computer Use', other: 'Other tools'}[state.kind];
    if (state.kind && kindLabel) add('Showing: ' + kindLabel, () => { state.kind = ''; });
    if (state.result) add('Result: ' + $('status-filter').selectedOptions[0].textContent, () => { state.result = ''; });
    if (state.service) add('Service: ' + state.service, () => { state.service = ''; });
    if (state.rule) add('Your rule: ' + state.rule, () => { state.rule = ''; });
    if ($('search').value) add('Search: “' + $('search').value + '”', () => { $('search').value = ''; });
    if (state.session) {
      const s = state.sessions.find(x => x.id === state.session);
      add('Session: ' + (s ? agentName(s.agent) + ' in ' + project(s.cwd) : 'one session'), () => { state.session = ''; });
    }
  }
  const box = $('active-filters');
  box.hidden = !chips.length;
  if (!chips.length) return box.replaceChildren();
  const list = [node('span', 'Filtered by', 'active-title')];
  for (const [label, clear] of chips) {
    const b = node('button', undefined, 'filter-chip');
    b.setAttribute('aria-label', 'Remove filter ' + label);
    b.append(node('span', label), node('span', '×', 'chip-x'));
    b.onclick = () => { clear(); render(); refresh(); };
    list.push(b);
  }
  const all = node('button', 'Clear all', 'link');
  all.onclick = () => { applyFilters(NO_FILTERS); render(); refresh(); };
  list.push(all);
  box.replaceChildren(...list);
}

function syncFilters() {
  $('range').value = state.range;
  $('custom-range').hidden = state.range !== 'custom';
  $('agent-filter').value = state.agent;
  $('status-filter').value = state.result;
  const select = $('project-filter');
  const known = new Map((state.overview ? state.overview.projects : []).filter(p => p.cwd).map(p => [p.cwd, p]));
  if (state.project && !known.has(state.project)) known.set(state.project, {cwd: state.project});
  const options = [node('option', 'All projects')];
  options[0].value = '';
  for (const cwd of known.keys()) {
    const o = node('option', project(cwd));
    o.value = cwd;
    o.title = shortPath(cwd);
    options.push(o);
  }
  select.replaceChildren(...options);
  select.value = state.project;
  // Services seen in the period (from the overview's map), most used first.
  const services = $('service-filter');
  const seen = periodServices().map(r => [r.name, r.actions]);
  if (state.service && !seen.some(([n]) => n === state.service)) seen.push([state.service, null]);
  const choices = [node('option', 'All services')];
  choices[0].value = '';
  for (const [name, n] of seen) {
    const o = node('option', n === null ? name : name + ' · ' + n);
    o.value = name;
    choices.push(o);
  }
  services.replaceChildren(...choices);
  services.value = state.service;
  renderActiveFilters();
  for (const el of document.querySelectorAll('.activity-only')) el.hidden = state.page !== 'activity';
}

function render() {
  renderSummary();
  syncFilters();
  // The agents in the side menu show which one the page is filtered on.
  for (const b of document.querySelectorAll('.agent[data-agent]')) b.setAttribute('aria-pressed', b.dataset.agent === state.agent ? 'true' : 'false');
  for (const b of document.querySelectorAll('.page-link')) {
    if (b.dataset.page === state.page) b.setAttribute('aria-current', 'page');
    else b.removeAttribute('aria-current');
  }
  $('page-overview').hidden = state.page !== 'overview';
  $('page-activity').hidden = state.page !== 'activity';
  if (state.page === 'overview') {
    renderOverview();
    return;
  }
  renderTimeline();
  renderDetail();
  for (const b of document.querySelectorAll('.kind')) b.setAttribute('aria-pressed', b.dataset.kind === state.kind ? 'true' : 'false');
}

function select(action, scrollList) {
  state.selected = action.key;
  render();
  if (scrollList) document.querySelector('.action.selected')?.scrollIntoView({behavior: 'smooth', block: 'center'});
  if (narrow.matches) $('detail-panel').scrollIntoView({behavior: 'smooth', block: 'start'});
}

function setSession(id) {
  state.session = id;
  refresh();
}

// Switch page. Back to the overview: its filters as it was left. To the
// activity list: the overview's period, agent and project, plus `drill`
// (what was clicked on the overview); from the menu, nothing more.
function setPage(page, drill) {
  if (page === state.page && !drill) return;
  savedFilters[state.page] = currentFilters();
  const overview = savedFilters.overview || NO_FILTERS;
  const next = page === 'overview' ? overview
    : {...NO_FILTERS, range: overview.range, from: overview.from, to: overview.to, agent: overview.agent, project: overview.project, ...(drill || {})};
  applyFilters(next);
  state.page = page;
  state.rows = [];
  state.selected = null;
  state.dismissed = null; // the "Undo" line is offered until the overview is left
  render();
  refresh();
  window.scrollTo({top: 0});
}

// ---------- Data ----------

async function refresh(more = false) {
  const gen = ++state.generation;
  const scope = scopeParams();
  const ovParams = new URLSearchParams(scope);
  ovParams.set('tz', String(-new Date().getTimezoneOffset()));
  if (state.page !== 'overview') ovParams.set('commands', '0');
  const requests = [api('/api/status'), api('/api/overview?' + ovParams)];
  if (state.page === 'activity') {
    const p = new URLSearchParams(scope);
    if ($('search').value) p.set('q', $('search').value);
    if (state.session) p.set('session', state.session);
    if (state.service) p.set('service', state.service);
    if (state.rule) p.set('rule', state.rule);
    if (state.kind) p.set('kind', state.kind);
    if (state.result) p.set('result', state.result);
    if (more && state.next !== null) p.set('before', state.next);
    requests.push(api('/api/events?' + p).then(page => moreMatches(page, p, gen)));
  }
  state.loading = true;
  try {
    const [status, overview, events] = await Promise.all(requests);
    if (gen !== state.generation) return;
    state.status = status;
    $('demo-banner').hidden = !status.demo;
    $('connection').textContent = status.demo ? 'Demo' : 'Live';
    $('connection').classList.remove('error');
    renderRail(status);
    renderCollector(status);
    const overviewChanged = JSON.stringify(overview) !== JSON.stringify(state.overview);
    state.overview = overview;
    let rowsChanged = false;
    if (events) {
      const rows = more ? withoutRepeats(state.rows.concat(events.events)) : events.events;
      rowsChanged = JSON.stringify(rows) !== JSON.stringify(state.rows) || state.next !== events.next;
      state.rows = rows;
      state.next = events.next;
      state.pages = more;
      state.scanned = (more ? state.scanned : 0) + events.scanned;
      $('more').hidden = events.next === null;
      if (rowsChanged) Object.assign(state, build(rows));
    }
    if (overviewChanged || rowsChanged) render();
  } catch (error) {
    $('connection').textContent = error.message;
    $('connection').classList.add('error');
  } finally {
    if (gen === state.generation) state.loading = false;
  }
}

// The server sends whole tool calls; at the edges of a page, some events
// come twice. Keep each event once.
function withoutRepeats(rows) {
  const seen = new Set();
  return rows.filter(r => !seen.has(r.id) && seen.add(r.id));
}

// With a kind or result filter, the server sends every call that MAY match;
// the exact rule is applied here. When few of them do, look further back
// on our own, a few pages at most, so that "nothing found" is rarely wrong.
const ENOUGH = 30;
const AUTO_PAGES = 5;
async function moreMatches(page, params, gen) {
  if (!state.kind && !state.result) return page;
  for (let n = 0; n < AUTO_PAGES && page.next !== null && gen === state.generation; n++) {
    const shown = build(page.events).actions.filter(visible).length;
    if (shown >= ENOUGH) break;
    params.set('before', page.next);
    const older = await api('/api/events?' + params);
    page = {events: withoutRepeats(page.events.concat(older.events)), next: older.next, scanned: page.scanned + older.scanned};
  }
  return page;
}

// ---------- Theme ----------
// Auto follows the system; light or dark is kept in the data folder (the port,
// and so the browser's storage, changes at each launch). /theme.js applies
// the saved choice before the page is drawn.
function showTheme(theme) {
  if (theme === 'auto') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  for (const b of document.querySelectorAll('.theme-btn')) b.setAttribute('aria-pressed', String(b.dataset.theme === theme));
}
showTheme(document.documentElement.dataset.theme || 'auto');
for (const b of document.querySelectorAll('.theme-btn')) {
  b.onclick = () => {
    showTheme(b.dataset.theme);
    api('/api/theme?set=' + b.dataset.theme, 'POST').catch(() => {});
  };
}

// Agents in the side menu: their logo (static/brands.js) in the agent's colour;
// the colour alone when there is none.
for (const id of Object.keys(AGENTS)) {
  const svg = agentLogo(id);
  if (svg) $('agent-' + id).querySelector('.agent-icon').replaceChildren(svg);
}

$('refresh').onclick = () => refresh();
for (const b of document.querySelectorAll('.page-link')) b.onclick = () => setPage(b.dataset.page);
$('range').onchange = () => {
  state.range = $('range').value;
  if (state.range === 'custom' && !$('from').value) {
    $('from').value = dayKey(addDays(new Date(), -6));
    $('to').value = dayKey(new Date());
  }
  render();
  refresh();
};
$('from').onchange = $('to').onchange = () => { render(); refresh(); };
$('agent-filter').onchange = () => { state.agent = $('agent-filter').value; render(); refresh(); };
// An agent in the side menu: only its activity, on this page; a second click
// shows every agent again (the same as the Agent filter).
for (const b of document.querySelectorAll('.agent[data-agent]')) {
  b.onclick = () => { state.agent = state.agent === b.dataset.agent ? '' : b.dataset.agent; state.selected = null; render(); refresh(); };
}
$('project-filter').onchange = () => { state.project = $('project-filter').value; render(); refresh(); };
$('status-filter').onchange = () => { state.result = $('status-filter').value; state.selected = null; render(); refresh(); };
$('service-filter').onchange = () => { state.service = $('service-filter').value; state.selected = null; render(); refresh(); };
$('search').oninput = () => { clearTimeout(searchTimer); searchTimer = setTimeout(() => { renderActiveFilters(); refresh(); }, 250); };
for (const b of document.querySelectorAll('.kind')) b.onclick = () => { state.kind = b.dataset.kind; state.selected = null; render(); refresh(); };
$('more').onclick = () => refresh(true);
document.addEventListener('keydown', e => {
  if (e.key === 'Escape' && state.selected) { state.selected = null; render(); }
});
let resizeTimer;
window.addEventListener('resize', () => { clearTimeout(resizeTimer); resizeTimer = setTimeout(() => { if (state.page === 'overview') renderOverview(); }, 150); });
render();
refresh();
// Live updates: not while a refresh is still running (a large journal can
// take a moment), and not once older pages were loaded.
setInterval(() => { if (document.visibilityState === 'visible' && !state.loading && !state.pages) refresh(); }, 4000);
