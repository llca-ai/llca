// Static demo: answers the dashboard's /api/ calls from JSON files saved from
// `llca demo`, so the real dashboard runs on GitHub Pages with no server.
// Loaded before app.js. Nothing leaves the page: every file is the site's own.
(() => {
  const DATA = 'data/';
  const NOTE = 'This is not available in the demo. Install llca to use it on your own agents.';
  const DAY = 86400000;
  const realFetch = window.fetch.bind(window);
  const cache = new Map();

  // The page does history.replaceState(null, '', '/') to hide the token; on a
  // project site "/" is not this page.
  const replaceState = history.replaceState.bind(history);
  history.replaceState = (state, title, url) => replaceState(state, title, url === '/' ? location.pathname + location.search : url);

  const load = name => {
    if (!cache.has(name)) {
      cache.set(name, realFetch(DATA + name + '.json').then(r => {
        if (!r.ok) throw new Error('Demo data missing: ' + name);
        return r.json();
      }));
    }
    return cache.get(name);
  };

  // The saved journal ends at generated_at. Every date is moved forward by
  // the same amount, so the newest action is "just now" whenever the page is
  // opened and the demo never looks old.
  const meta = load('meta');
  const iso = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?[+-]\d\d:\d\d$/;
  const day = /^\d{4}-\d\d-\d\d$/;
  const utcDay = ms => Math.floor(ms / DAY);
  let shiftMs = 0;
  let shiftDays = 0;
  const ready = meta.then(m => {
    const generated = Date.parse(m.generated_at);
    shiftMs = Date.now() - generated;
    shiftDays = utcDay(Date.now()) - utcDay(generated);
  });

  const shifted = value => {
    if (typeof value === 'string') {
      if (iso.test(value)) return new Date(Date.parse(value) + shiftMs).toISOString().replace('Z', '+00:00');
      if (day.test(value)) return new Date(Date.parse(value + 'T00:00:00Z') + shiftDays * DAY).toISOString().slice(0, 10);
      return value;
    }
    if (Array.isArray(value)) return value.map(shifted);
    if (value && typeof value === 'object') {
      const out = {};
      for (const k of Object.keys(value)) out[k] = shifted(value[k]);
      return out;
    }
    return value;
  };

  const json = (body, status = 200) =>
    new Response(JSON.stringify(body), {status, headers: {'Content-Type': 'application/json'}});
  const refuse = (message, status = 400) => json({error: message}, status);

  // Which saved period a request asks for: by its length in days.
  const period = (from, to) => {
    if (!from || !to) return '30d';
    const days = Math.round((Date.parse(to) - Date.parse(from)) / DAY);
    return days <= 1 ? 'today' : days <= 7 ? '7d' : '30d';
  };

  const UNSUPPORTED = ['project', 'kind', 'result', 'service', 'rule'];
  const unsupported = params => UNSUPPORTED.find(k => params.get(k));

  async function overview(params) {
    if (unsupported(params)) return refuse('Install llca to use this filter.');
    const agent = params.get('agent') || 'all';
    const name = 'overview-' + period(params.get('from'), params.get('to')) + '-' + agent + (params.get('commands') === '0' ? '-nocmd' : '');
    return json(shifted(await load(name)));
  }

  async function events(params) {
    if (unsupported(params)) return refuse('Install llca to use this filter.');
    const all = shifted(await load('events-30d-all')).events;
    const from = params.get('from') ? Date.parse(params.get('from')) : -Infinity;
    const to = params.get('to') ? Date.parse(params.get('to')) : Infinity;
    const agent = params.get('agent');
    const session = params.get('session');
    const q = (params.get('q') || '').toLowerCase();
    const before = params.get('before') ? Number(params.get('before')) : Infinity;
    const rows = all.filter(e => {
      const t = Date.parse(e.received_at);
      return t >= from && t < to && e.id < before && (!agent || e.agent === agent) && (!session || e.session_id === session)
        && (!q || JSON.stringify([e.tool, e.event, e.cwd, e.arguments]).toLowerCase().includes(q));
    });
    const page = rows.slice(0, 200);
    return json({events: page, next: rows.length > 200 ? page[page.length - 1].id : null, scanned: rows.length});
  }

  async function handle(url, method) {
    const path = url.pathname.replace(/^.*\/api\//, '/api/');
    if (path === '/api/status' && method === 'GET') return json(shifted(await load('status')));
    if (path === '/api/overview' && method === 'GET') return overview(url.searchParams);
    if (path === '/api/events' && method === 'GET') return events(url.searchParams);
    if (path === '/api/app-icon') return new Response(null, {status: 204});
    if (path === '/api/theme' && method === 'POST') {
      const set = url.searchParams.get('set');
      try { if (set === 'auto') localStorage.removeItem('llca-theme'); else localStorage.setItem('llca-theme', set); } catch (e) { /* private window */ }
      return json({theme: set});
    }
    if (method === 'POST') return refuse(NOTE, 403);
    return refuse('Not found', 404);
  }

  window.fetch = async (input, init = {}) => {
    const url = new URL(typeof input === 'string' ? input : input.url, location.href);
    if (!/(^|\/)api\//.test(url.pathname)) return realFetch(input, init);
    await ready;
    try {
      return await handle(url, (init.method || 'GET').toUpperCase());
    } catch (error) {
      return refuse(error.message, 500);
    }
  };

  window.addEventListener('DOMContentLoaded', () => {
    const banner = document.getElementById('demo-banner');
    if (!banner) return;
    banner.textContent = 'Live demo with sample data: nothing here really happened. To see your own agents: ';
    const code = document.createElement('code');
    code.textContent = 'brew install llca-ai/tap/llca';
    const link = document.createElement('a');
    link.href = 'https://github.com/llca-ai/llca#install';
    link.textContent = 'other ways to install';
    link.style.color = 'inherit';
    banner.append(code, ' or ', link, '.');
  });
})();
